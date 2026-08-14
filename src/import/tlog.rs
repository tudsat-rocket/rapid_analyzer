//! Importer for MAVLink `.tlog` files: a stream of
//! `[8-byte big-endian microsecond UTC timestamp][MAVLink v1 or v2 frame]`
//! records, as written by QGroundControl / MAVProxy / Mission Planner.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read};
use std::path::Path;

use anyhow::{Context, Result};
use mavlink_core::peek_reader::PeekReader;
use mavlink_core::{Message, ReadVersion, error::MessageReadError};
use serde_json::Value;

use crate::dialect::rapid::MavMessage;
use crate::mavlink_meta::{self, FieldMeta};
use crate::model::{LogFormat, LogSource};
use crate::series::TimeSeries;

pub fn import(path: &Path) -> Result<LogSource> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = PeekReader::new(BufReader::new(file));

    let mut collector = Collector::default();
    let mut ts_buf = [0u8; 8];
    let mut n_messages = 0u64;

    loop {
        match read_exact_from_peek(&mut reader, &mut ts_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e).context("reading tlog timestamp"),
        }
        let t_us = u64::from_be_bytes(ts_buf);
        let t_utc = t_us as f64 / 1_000_000.0;

        match mavlink_core::read_versioned_msg::<MavMessage, _>(&mut reader, ReadVersion::Any) {
            Ok((header, msg)) => {
                n_messages += 1;
                collector.push_message(&msg, header.system_id, header.component_id, t_utc);
            }
            Err(MessageReadError::Io(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(_) => {
                // mavlink-core already resyncs on bad CRC by rescanning for
                // the next STX byte-by-byte, so messages outside our dialect
                // (or genuine corruption) are silently skipped there, not
                // surfaced here. What reaches this arm is something the
                // resync can't recover from (e.g. the file is truncated
                // mid-frame); stop and keep whatever was parsed so far.
                break;
            }
        }
    }

    anyhow::ensure!(n_messages > 0, "no valid MAVLink messages found in {}", path.display());

    let series = collector.finish();
    log::info!(
        "{}: {n_messages} messages -> {} series",
        path.display(),
        series.len()
    );

    Ok(LogSource {
        series,
        format: LogFormat::Tlog,
    })
}

fn read_exact_from_peek<R: Read>(reader: &mut PeekReader<R>, buf: &mut [u8; 8]) -> std::io::Result<()> {
    let bytes = reader
        .read_exact(8)
        .map_err(|_| std::io::Error::from(ErrorKind::UnexpectedEof))?;
    buf.copy_from_slice(bytes);
    Ok(())
}

/// One time series' worth of samples, plus the schema needed to name and
/// scale it once the whole file has been read.
#[derive(Default)]
struct Group {
    /// Field path (`pressure1`, `voltages[3]`) -> samples, raw units.
    fields: HashMap<String, Vec<[f64; 2]>>,
}

/// What distinguishes one series of a message from another: messages carrying
/// an `instance="true"` field (`PRESSURE_VESSEL.id`, `VALVE.id`) describe a
/// different tank or valve on every send, and several systems can emit the
/// same message onto one link. Folding those together -- as a plain
/// `MSG.field` key does -- interleaves unrelated measurements into one
/// jagged, meaningless line.
#[derive(PartialEq, Eq, Hash)]
struct GroupKey {
    message: &'static str,
    system: u8,
    component: u8,
    instance: Option<String>,
}

#[derive(Default)]
struct Collector {
    groups: HashMap<GroupKey, Group>,
}

impl Collector {
    fn push_message(&mut self, msg: &MavMessage, system: u8, component: u8, t_utc: f64) {
        let message = msg.message_name();
        let Ok(Value::Object(map)) = serde_json::to_value(msg) else {
            return;
        };
        let meta = mavlink_meta::message(message);

        let instance_field = meta.and_then(|m| m.instance_field());
        let instance = instance_field.and_then(|f| map.get(f.name).map(|v| instance_label(f, v)));
        // Once the instance is part of the series name, a series of the
        // instance id itself is a constant line saying what the name already
        // says.
        let redundant_field = instance.as_ref().and(instance_field).map(|f| f.name);

        let group = self
            .groups
            .entry(GroupKey {
                message,
                system,
                component,
                instance,
            })
            .or_default();

        for (field, value) in &map {
            // serde tags the message enum variant with "type"; a MAVLink
            // field actually named `type` is emitted as `mavtype`.
            if field == "type" || Some(field.as_str()) == redundant_field {
                continue;
            }
            let field_meta = meta.and_then(|m| m.field(field));
            push_value(&mut group.fields, field, value, field_meta, t_utc);
        }
    }

    fn finish(self) -> Vec<TimeSeries> {
        // Only spell out the sending system when the log actually carries
        // more than one, so the common single-vehicle case stays readable.
        let mut senders: HashMap<&'static str, Vec<(u8, u8)>> = HashMap::new();
        for key in self.groups.keys() {
            let entry = senders.entry(key.message).or_default();
            if !entry.contains(&(key.system, key.component)) {
                entry.push((key.system, key.component));
            }
        }

        let mut out = Vec::new();
        for (key, group) in self.groups {
            let ambiguous = senders.get(key.message).is_some_and(|s| s.len() > 1);
            let mut prefix = key.message.to_string();
            if let Some(instance) = &key.instance {
                prefix.push_str(&format!("[{instance}]"));
            }
            if ambiguous {
                prefix.push_str(&format!("@{}:{}", key.system, key.component));
            }

            let meta = mavlink_meta::message(key.message);
            for (field, points) in group.fields {
                if points.is_empty() {
                    continue;
                }
                let unit = meta
                    .and_then(|m| m.field(base_field(&field)))
                    .and_then(|f| f.unit_scale().0)
                    .map(str::to_string);
                out.push(TimeSeries::from_points(format!("{prefix}.{field}"), points).with_unit(unit));
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Flattens one JSON field into `fields`, expanding arrays into `name[i]`
/// series. Enum and bitmask fields arrive as `{"type": "ENTRY"}` objects and
/// `"A | B"` strings respectively; both are resolved back to the numbers they
/// stand for so they can share an axis with everything else.
fn push_value(
    fields: &mut HashMap<String, Vec<[f64; 2]>>,
    path: &str,
    value: &Value,
    meta: Option<&'static FieldMeta>,
    t_utc: f64,
) {
    if let Value::Array(items) = value {
        // Only expand arrays of plain numbers (skip byte/char arrays used for
        // fixed-size strings, which show up as arrays of small ints).
        if items.len() <= 64 && items.iter().all(Value::is_number) {
            for (i, item) in items.iter().enumerate() {
                push_value(fields, &format!("{path}[{i}]"), item, meta, t_utc);
            }
        }
        return;
    }

    let Some(raw) = numeric_value(value, meta) else {
        return;
    };
    if meta.and_then(FieldMeta::invalid_value).is_some_and(|inv| inv == raw || (inv.is_nan() && raw.is_nan())) {
        return;
    }
    let scaled = raw * meta.map_or(1.0, |m| m.unit_scale().1);

    // Look up before inserting so the hot path doesn't allocate a key string
    // for every sample of every field of every message.
    match fields.get_mut(path) {
        Some(points) => points.push([t_utc, scaled]),
        None => {
            fields.insert(path.to_string(), vec![[t_utc, scaled]]);
        }
    }
}

fn numeric_value(value: &Value, meta: Option<&FieldMeta>) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        // An enum-typed field: `{"type": "VALVE_ID_MAIN"}`.
        Value::Object(map) => match map.get("type") {
            Some(Value::String(entry)) if map.len() == 1 => {
                mavlink_meta::enum_value(meta.and_then(|m| m.enum_name), entry).map(|v| v as f64)
            }
            _ => None,
        },
        // A bitmask field: `""`, or `"FLAG_A | FLAG_B"`. Plain strings
        // (`param_id`, `mode_name`) have no enum and are left out.
        Value::String(s) => {
            let enum_name = meta?.enum_name?;
            let mut bits = 0i64;
            for entry in s.split('|').map(str::trim).filter(|e| !e.is_empty()) {
                bits |= mavlink_meta::enum_value(Some(enum_name), entry)?;
            }
            Some(bits as f64)
        }
        _ => None,
    }
}

/// `"voltages[3]"` -> `"voltages"`, so an expanded array element still finds
/// its field's schema.
fn base_field(path: &str) -> &str {
    path.split('[').next().unwrap_or(path)
}

/// How an instance shows up in a series name: a plain number for
/// `PRESSURE_VESSEL.id`, the enum entry for `VALVE.id`.
fn instance_label(field: &FieldMeta, value: &Value) -> String {
    match value {
        Value::Number(n) => n.to_string(),
        Value::Object(map) => match map.get("type") {
            Some(Value::String(entry)) => mavlink_meta::short_enum_entry(field.enum_name, entry),
            _ => "?".to_string(),
        },
        Value::String(s) => s.clone(),
        _ => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::rapid::{FluidType, PRESSURE_VESSEL_DATA, VALVE_DATA, ValveId};

    fn collect(messages: &[(MavMessage, u8)]) -> Vec<String> {
        let mut collector = Collector::default();
        for (i, (msg, system)) in messages.iter().enumerate() {
            collector.push_message(msg, *system, 1, i as f64);
        }
        collector.finish().into_iter().map(|s| s.name).collect()
    }

    fn pressure_vessel(id: u8, pressure1: u16, temperature1: i16) -> MavMessage {
        MavMessage::PRESSURE_VESSEL(PRESSURE_VESSEL_DATA {
            id,
            pressure1,
            temperature1,
            // "no reading" everywhere else.
            pressure2: u16::MAX,
            temperature2: i16::MAX,
            level: u16::MAX,
            rated_pressure: 5500,
            volume: 8000,
            flags: Default::default(),
            fluid: FluidType::NITROGEN,
        })
    }

    #[test]
    fn instances_become_separate_series() {
        let names = collect(&[
            (pressure_vessel(0, 4540, 2000), 4),
            (pressure_vessel(1, 110, 2144), 4),
        ]);
        assert!(names.contains(&"PRESSURE_VESSEL[0].pressure1".to_string()), "{names:?}");
        assert!(names.contains(&"PRESSURE_VESSEL[1].pressure1".to_string()), "{names:?}");
        // The id is in the name now; a series of it would be a constant line.
        assert!(!names.iter().any(|n| n.ends_with(".id")), "{names:?}");
        // Every sample of these was the "no reading" sentinel, so they should
        // not show up as flat lines at 65535.
        assert!(!names.iter().any(|n| n.ends_with(".pressure2")), "{names:?}");
        assert!(!names.iter().any(|n| n.ends_with(".level")), "{names:?}");
    }

    #[test]
    fn enum_instance_fields_are_named_and_split() {
        let valve = |id, state: f32| {
            MavMessage::VALVE(VALVE_DATA {
                id,
                state,
                commanded: state,
            })
        };
        let names = collect(&[
            (valve(ValveId::VALVE_ID_MAIN, 1.0), 4),
            (valve(ValveId::VALVE_ID_OXIDIZER_VENT, 0.0), 4),
        ]);
        assert!(names.contains(&"VALVE[MAIN].state".to_string()), "{names:?}");
        assert!(names.contains(&"VALVE[OXIDIZER_VENT].state".to_string()), "{names:?}");
    }

    #[test]
    fn centi_units_are_scaled_to_their_display_unit() {
        let mut collector = Collector::default();
        collector.push_message(&pressure_vessel(1, 110, 2144), 4, 1, 0.0);
        let series = collector.finish();
        let temp = series
            .iter()
            .find(|s| s.name == "PRESSURE_VESSEL[1].temperature1")
            .expect("temperature series");
        assert_eq!(temp.unit.as_deref(), Some("°C"));
        assert_eq!(temp.value_at(0.0, 0.0), Some(21.44));
    }

    #[test]
    fn system_ids_are_spelled_out_only_when_ambiguous() {
        let heartbeat = || MavMessage::HEARTBEAT(Default::default());
        let single = collect(&[(heartbeat(), 4)]);
        assert!(single.iter().all(|n| !n.contains('@')), "{single:?}");

        let mixed = collect(&[(heartbeat(), 4), (heartbeat(), 254)]);
        assert!(mixed.contains(&"HEARTBEAT@4:1.custom_mode".to_string()), "{mixed:?}");
        assert!(mixed.contains(&"HEARTBEAT@254:1.custom_mode".to_string()), "{mixed:?}");
    }
}
