//! Generates Rust bindings for the `rapid` MAVLink dialect (the common
//! dialect plus our project-specific messages, e.g. `PRESSURE_VESSEL`) from
//! the XML definitions in `mavlink_dialects/`, via `mavlink-bindgen` -- the
//! same code generator the `mavlink` crate itself uses for its bundled
//! dialects. We generate our own because a custom dialect can't be selected
//! through that crate's `dialect-*` Cargo features.
//!
//! `mavlink-bindgen` only emits the *decoder*: the message structs. The
//! importer also needs the schema around each field -- which field identifies
//! the instance (`instance="true"`), what unit a value is in, which enum an
//! enum-typed field belongs to, and what sentinel means "no reading". None of
//! that survives into the generated Rust, so we make a second pass over the
//! same XML here and emit it as static tables (see `src/mavlink_meta.rs`).

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::path::Path;

use mavlink_bindgen::XmlDefinitions;
use quick_xml::events::Event;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let dialects_dir = Path::new(&manifest_dir).join("mavlink_dialects");
    let out_dir = env::var("OUT_DIR").unwrap();

    let result = mavlink_bindgen::generate(XmlDefinitions::Directory(dialects_dir.clone()), &out_dir)
        .expect("failed to generate MAVLink dialect bindings");

    mavlink_bindgen::format_generated_code(&result);
    mavlink_bindgen::emit_cargo_build_messages(&result);

    let schema = Schema::parse_dir(&dialects_dir);
    std::fs::write(Path::new(&out_dir).join("mavlink_meta.rs"), schema.to_rust())
        .expect("writing mavlink_meta.rs");
}

#[derive(Default)]
struct Schema {
    /// Message name -> its fields, in XML order.
    messages: BTreeMap<String, Vec<Field>>,
    /// Enum name -> (entry name, value). Enums are extended across files
    /// (e.g. `Rapid.xml` adds entries to `MAV_CMD`), so entries accumulate.
    enums: BTreeMap<String, Vec<(String, i64)>>,
}

struct Field {
    name: String,
    ty: String,
    units: Option<String>,
    enum_name: Option<String>,
    invalid: Option<String>,
    is_instance: bool,
}

impl Schema {
    fn parse_dir(dir: &Path) -> Self {
        let mut schema = Schema::default();
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .expect("reading mavlink_dialects/")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "xml"))
            .collect();
        paths.sort();
        for path in paths {
            println!("cargo:rerun-if-changed={}", path.display());
            let text = std::fs::read_to_string(&path).expect("reading dialect XML");
            schema.parse_str(&text, &path);
        }
        schema
    }

    fn parse_str(&mut self, text: &str, path: &Path) {
        let mut reader = quick_xml::Reader::from_str(text);
        reader.config_mut().trim_text(true);

        // Where we currently are: inside `<message name=...>` or
        // `<enum name=...>`. The two never nest, so one slot each is enough.
        let mut message: Option<String> = None;
        let mut enum_name: Option<String> = None;

        loop {
            match reader.read_event() {
                Ok(Event::Eof) => break,
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let tag = e.name().as_ref().to_vec();
                    let attr = |key: &str| -> Option<String> {
                        e.attributes().flatten().find(|a| a.key.as_ref() == key.as_bytes()).map(|a| {
                            String::from_utf8_lossy(a.value.as_ref()).into_owned()
                        })
                    };
                    match tag.as_slice() {
                        b"message" => {
                            if let Some(name) = attr("name") {
                                self.messages.entry(name.clone()).or_default();
                                message = Some(name);
                            }
                        }
                        b"enum" => enum_name = attr("name"),
                        b"field" => {
                            let (Some(msg), Some(name), Some(ty)) = (message.as_ref(), attr("name"), attr("type"))
                            else {
                                continue;
                            };
                            self.messages.entry(msg.clone()).or_default().push(Field {
                                // `mavlink-bindgen` renames fields that collide
                                // with Rust keywords, and that renamed form is
                                // what serde emits -- match it so lookups by
                                // JSON key hit.
                                name: if name == "type" { "mavtype".to_string() } else { name },
                                ty,
                                units: attr("units"),
                                enum_name: attr("enum"),
                                invalid: attr("invalid"),
                                is_instance: attr("instance").as_deref() == Some("true"),
                            });
                        }
                        b"entry" => {
                            let (Some(en), Some(name)) = (enum_name.as_ref(), attr("name")) else {
                                continue;
                            };
                            // Entries without an explicit value auto-increment
                            // from the previous one, same as the C generator.
                            let entries = self.enums.entry(en.clone()).or_default();
                            let value = match attr("value").as_deref().map(parse_enum_value) {
                                Some(Some(v)) => v,
                                _ => entries.last().map(|(_, v)| v + 1).unwrap_or(0),
                            };
                            entries.push((name, value));
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(e)) => match e.name().as_ref() {
                    b"message" => message = None,
                    b"enum" => enum_name = None,
                    _ => {}
                },
                Ok(_) => {}
                Err(e) => panic!("parsing {}: {e}", path.display()),
            }
        }
    }

    fn to_rust(&self) -> String {
        let mut out = String::from("// @generated by build.rs from mavlink_dialects/*.xml -- do not edit.\n");

        out.push_str("pub static MESSAGES: &[MessageMeta] = &[\n");
        for (name, fields) in &self.messages {
            writeln!(out, "    MessageMeta {{ name: {name:?}, fields: &[").unwrap();
            for f in fields {
                writeln!(
                    out,
                    "        FieldMeta {{ name: {:?}, ty: {:?}, units: {}, enum_name: {}, invalid: {}, is_instance: {} }},",
                    f.name,
                    f.ty,
                    opt_str(&f.units),
                    opt_str(&f.enum_name),
                    invalid_literal(f.invalid.as_deref()),
                    f.is_instance,
                )
                .unwrap();
            }
            out.push_str("    ] },\n");
        }
        out.push_str("];\n\n");

        out.push_str("pub static ENUM_ENTRIES: &[(&str, &str, i64)] = &[\n");
        for (en, entries) in &self.enums {
            for (entry, value) in entries {
                writeln!(out, "    ({en:?}, {entry:?}, {value}),").unwrap();
            }
        }
        out.push_str("];\n");
        out
    }
}

fn opt_str(value: &Option<String>) -> String {
    match value {
        Some(v) => format!("Some({v:?})"),
        None => "None".to_string(),
    }
}

fn parse_enum_value(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16).ok(),
        None => raw.parse().ok(),
    }
}

/// Renders the `invalid="..."` attribute as an `Option<f64>` literal.
///
/// Only the unambiguous sentinels are kept: NaN and the type-max/min
/// constants. `invalid="0"` and `invalid="-1"` also appear in `common.xml`,
/// but those are values a sensor can genuinely report, and silently dropping
/// them would put holes in real data. Array forms (`[NaN]`, `[0]`) describe a
/// whole array being unset and don't map onto per-element series.
fn invalid_literal(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return "None".to_string();
    };
    match raw.trim() {
        "NaN" | "NAN" => "Some(f64::NAN)".to_string(),
        other => match int_limit(other) {
            Some(v) => format!("Some({v:?})"),
            None => "None".to_string(),
        },
    }
}

/// `"UINT16_MAX"` -> `65535.0`, `"INT8_MIN"` -> `-128.0`.
fn int_limit(name: &str) -> Option<f64> {
    let (name, is_max) = match name.strip_suffix("_MAX") {
        Some(rest) => (rest, true),
        None => (name.strip_suffix("_MIN")?, false),
    };
    let (signed, bits) = match name.strip_prefix('U') {
        Some(rest) => (false, rest.strip_prefix("INT")?),
        None => (true, name.strip_prefix("INT")?),
    };
    let bits: u32 = bits.parse().ok()?;
    if !matches!(bits, 8 | 16 | 32 | 64) {
        return None;
    }
    Some(match (signed, is_max) {
        (false, true) => (2f64).powi(bits as i32) - 1.0,
        (false, false) => 0.0,
        (true, true) => (2f64).powi(bits as i32 - 1) - 1.0,
        (true, false) => -(2f64).powi(bits as i32 - 1),
    })
}
