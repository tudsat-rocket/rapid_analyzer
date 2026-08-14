//! The parts of the MAVLink XML schema that don't survive code generation:
//! which field identifies a message *instance*, what unit a field is in,
//! which enum an enum-typed field draws from, and which value means "no
//! reading". `build.rs` re-parses `mavlink_dialects/*.xml` to emit the tables
//! below; everything here is lookup and interpretation on top of them.

use std::collections::HashMap;
use std::sync::OnceLock;

pub struct MessageMeta {
    pub name: &'static str,
    pub fields: &'static [FieldMeta],
}

pub struct FieldMeta {
    /// Field name as serde emits it (`type` is renamed to `mavtype`).
    pub name: &'static str,
    /// MAVLink type, e.g. `uint16_t` or `float[4]`.
    pub ty: &'static str,
    pub units: Option<&'static str>,
    pub enum_name: Option<&'static str>,
    /// Explicit `invalid="..."` sentinel, when it is an unambiguous one.
    pub invalid: Option<f64>,
    /// `instance="true"`: this field says *which* sensor/tank/valve the rest
    /// of the message is about, so each of its values is its own time series.
    pub is_instance: bool,
}

include!(concat!(env!("OUT_DIR"), "/mavlink_meta.rs"));

impl MessageMeta {
    pub fn field(&self, name: &str) -> Option<&'static FieldMeta> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn instance_field(&self) -> Option<&'static FieldMeta> {
        self.fields.iter().find(|f| f.is_instance)
    }
}

impl FieldMeta {
    /// The value that means "this sensor reported nothing", if any.
    ///
    /// Beyond the explicit `invalid="..."` attribute, MAVLink's convention is
    /// that an integer measurement is unset when it sits at its type's
    /// maximum -- `common.xml` documents that on some fields and leaves it
    /// implicit on others, and `Rapid.xml` leaves it implicit everywhere
    /// (`PRESSURE_VESSEL` sends `pressure=65535` / `temperature=32767` for
    /// every unpopulated sensor). Requiring a `units` attribute keeps the
    /// rule to physical measurements, so counters, ids and bitmasks that
    /// legitimately reach their type max are left alone.
    pub fn invalid_value(&self) -> Option<f64> {
        if let Some(v) = self.invalid {
            return Some(v);
        }
        if self.units.is_none() || self.is_instance || self.enum_name.is_some() {
            return None;
        }
        int_type_max(self.ty)
    }

    /// Display unit and the factor raw values are multiplied by to reach it.
    /// MAVLink transports several quantities pre-scaled to keep them integral
    /// (centidegrees, 1e-7 degrees); plotting those raw is just misleading.
    pub fn unit_scale(&self) -> (Option<&'static str>, f64) {
        match self.units {
            Some("cdegC") => (Some("°C"), 0.01),
            Some("cdeg") => (Some("deg"), 0.01),
            Some("cA") => (Some("A"), 0.01),
            Some("c%") => (Some("%"), 0.01),
            Some("degE7") => (Some("deg"), 1e-7),
            other => (other, 1.0),
        }
    }
}

pub fn message(name: &str) -> Option<&'static MessageMeta> {
    static BY_NAME: OnceLock<HashMap<&'static str, &'static MessageMeta>> = OnceLock::new();
    BY_NAME
        .get_or_init(|| MESSAGES.iter().map(|m| (m.name, m)).collect())
        .get(name)
        .copied()
}

/// Numeric value of an enum entry, e.g. `("VALVE_ID", "VALVE_ID_MAIN") -> 4`.
/// Falls back to matching the entry name in any enum, since a bitmask field
/// occasionally names entries from an enum it doesn't declare.
pub fn enum_value(enum_name: Option<&str>, entry: &str) -> Option<i64> {
    static BY_ENUM: OnceLock<HashMap<(&'static str, &'static str), i64>> = OnceLock::new();
    static BY_ENTRY: OnceLock<HashMap<&'static str, i64>> = OnceLock::new();

    if let Some(en) = enum_name {
        let map = BY_ENUM.get_or_init(|| ENUM_ENTRIES.iter().map(|(e, n, v)| ((*e, *n), *v)).collect());
        if let Some(v) = map.get(&(en, entry)) {
            return Some(*v);
        }
    }
    BY_ENTRY
        .get_or_init(|| ENUM_ENTRIES.iter().map(|(_, n, v)| (*n, *v)).collect())
        .get(entry)
        .copied()
}

/// Strips the enum's own prefix off an entry name, so an instance shows up as
/// `VALVE[MAIN]` rather than `VALVE[VALVE_ID_MAIN]`.
pub fn short_enum_entry(enum_name: Option<&str>, entry: &str) -> String {
    match enum_name.and_then(|en| entry.strip_prefix(en).and_then(|rest| rest.strip_prefix('_'))) {
        Some(short) if !short.is_empty() => short.to_string(),
        _ => entry.to_string(),
    }
}

fn int_type_max(ty: &str) -> Option<f64> {
    let base = ty.split('[').next().unwrap_or(ty);
    Some(match base {
        "uint8_t" => 255.0,
        "int8_t" => 127.0,
        "uint16_t" => 65535.0,
        "int16_t" => 32767.0,
        "uint32_t" => 4294967295.0,
        "int32_t" => 2147483647.0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_dialect_messages_are_present() {
        let pv = message("PRESSURE_VESSEL").expect("PRESSURE_VESSEL in the generated schema");
        assert_eq!(pv.instance_field().map(|f| f.name), Some("id"));
        assert_eq!(pv.field("pressure1").unwrap().units, Some("kPa"));
        // Rapid.xml declares no `invalid`, so the implicit type-max rule has
        // to be what removes the 65535 "no reading" samples.
        assert_eq!(pv.field("pressure1").unwrap().invalid_value(), Some(65535.0));
        assert_eq!(pv.field("temperature1").unwrap().invalid_value(), Some(32767.0));
        assert_eq!(pv.field("temperature1").unwrap().unit_scale(), (Some("°C"), 0.01));
        // The instance field itself must survive; dropping id 255 would be
        // dropping a whole vessel.
        assert_eq!(pv.field("id").unwrap().invalid_value(), None);
    }

    #[test]
    fn enum_fields_resolve_to_numbers() {
        let valve = message("VALVE").expect("VALVE in the generated schema");
        let id = valve.instance_field().expect("VALVE.id is the instance field");
        assert_eq!(id.enum_name, Some("VALVE_ID"));
        assert_eq!(enum_value(id.enum_name, "VALVE_ID_MAIN"), Some(4));
        assert_eq!(short_enum_entry(id.enum_name, "VALVE_ID_MAIN"), "MAIN");
    }

    #[test]
    fn common_dialect_is_merged_in() {
        let bat = message("BATTERY_STATUS").expect("BATTERY_STATUS from common.xml");
        assert_eq!(bat.instance_field().map(|f| f.name), Some("id"));
        // Declared as invalid="[UINT16_MAX]" -- an array form we ignore, so
        // the implicit rule has to cover the per-cell series.
        assert_eq!(bat.field("voltages").unwrap().invalid_value(), Some(65535.0));
        // `type` is renamed by the code generator; the schema must follow.
        assert!(bat.field("mavtype").is_some());
    }
}
