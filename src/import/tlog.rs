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

use crate::can::{CanFrame, CanFrames, iocan};
use crate::dialect::rapid::MavMessage;
use crate::mavlink_meta::{self, FieldMeta};
use crate::model::{LogFormat, LogSource};
use crate::series::TimeSeries;

pub fn import(path: &Path) -> Result<LogSource> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    import_reader(BufReader::new(file), &path.display().to_string())
}

/// [`import`] from anything readable -- a file, or the bytes a browser hands
/// over, which is all the web build ever has. `label` names it in messages.
pub fn import_reader<R: Read>(reader: R, label: &str) -> Result<LogSource> {
    let mut reader = PeekReader::new(reader);

    let mut collector = Collector::default();
    let mut ts_buf = [0u8; 8];
    let mut n_messages = 0u64;
    let mut n_skipped = 0u64;
    let mut n_bad_time = 0u64;
    let mut last_good_t: Option<f64> = None;

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
                // Whenever mavlink-core had to resync -- a bad CRC, or a
                // false-positive frame that swallowed the next record's
                // timestamp -- the 8 bytes read above were not this frame's
                // timestamp but whatever preceded the scan. The message is
                // fine; its time is noise, and one sample in 1970 stretches
                // the timeline across 56 years.
                if !plausible_time(t_utc, last_good_t) {
                    n_bad_time += 1;
                    continue;
                }
                last_good_t = Some(t_utc);
                n_messages += 1;
                collector.push_message(&msg, header.system_id, header.component_id, t_utc);
            }
            Err(MessageReadError::Io(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(MessageReadError::Parse(_)) => {
                // A frame whose CRC checked out but whose payload doesn't
                // decode: an id outside our dialect that happens to pass the
                // CRC, or an enum value the XML doesn't list. mavlink-core
                // has already consumed the frame, so skip just this one --
                // stopping here once silently dropped the last 3.6 h of a
                // 6.7 h log over a single bad frame.
                n_skipped += 1;
            }
            Err(e) => {
                // A read error the resync can't get past; keep whatever was
                // parsed so far.
                log::warn!("{label}: stopped after {n_messages} messages: {e}");
                break;
            }
        }
    }

    anyhow::ensure!(n_messages > 0, "no valid MAVLink messages found in {label}");

    let (series, can) = collector.finish();
    log::info!(
        "{label}: {n_messages} messages -> {} series ({} CAN frames; skipped {n_skipped} undecodable frames, {n_bad_time} with a corrupt timestamp)",
        series.len(),
        can.len(),
    );

    Ok(LogSource {
        series,
        format: LogFormat::Tlog,
        can,
    })
}

/// Whether `t_utc` can be a real record's time: inside this century, and
/// within a day of the last accepted record. Bytes read out of alignment
/// essentially never land in that window, and a real log never jumps a day
/// between two records.
fn plausible_time(t_utc: f64, last_good: Option<f64>) -> bool {
    const EARLIEST: f64 = 946_684_800.0; // 2000-01-01
    const LATEST: f64 = 4_102_444_800.0; // 2100-01-01
    const MAX_JUMP: f64 = 24.0 * 3600.0;
    (EARLIEST..LATEST).contains(&t_utc) && last_good.is_none_or(|last| (t_utc - last).abs() <= MAX_JUMP)
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
    can: CanFrames,
}

impl Collector {
    fn push_message(&mut self, msg: &MavMessage, system: u8, component: u8, t_utc: f64) {
        // A forwarded CAN frame is a container, not a measurement: its `id`
        // says which node sent which signal, so expanding it field-by-field
        // like every other message would fold every node's traffic into one
        // `CAN_FRAME.data[0]` line. It goes to `crate::can` instead.
        if let MavMessage::CAN_FRAME(frame) = msg {
            self.can.push(CanFrame {
                t_utc,
                id: frame.id,
                bus: frame.bus,
                len: frame.len,
                data: frame.data,
            });
            return;
        }

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

    fn finish(self) -> (Vec<TimeSeries>, CanFrames) {
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
        out.extend(iocan::decode(self.can.frames()));
        out.sort_by(|a, b| a.name.cmp(&b.name));
        (out, self.can)
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
        collector.finish().0.into_iter().map(|s| s.name).collect()
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
        let (series, _) = collector.finish();
        let temp = series
            .iter()
            .find(|s| s.name == "PRESSURE_VESSEL[1].temperature1")
            .expect("temperature series");
        assert_eq!(temp.unit.as_deref(), Some("°C"));
        assert_eq!(temp.value_at(0.0, 0.0), Some(21.44));
    }

    /// One tlog record: big-endian µs timestamp, then a MAVLink v2 frame.
    fn record(t_us: u64, msg: &MavMessage) -> Vec<u8> {
        let mut raw = mavlink_core::MAVLinkV2MessageRaw::new();
        raw.serialize_message(mavlink_core::MavHeader::default(), msg);
        let mut out = t_us.to_be_bytes().to_vec();
        out.extend_from_slice(raw.raw_bytes());
        out
    }

    /// 2025-10-09, in tlog microseconds.
    const T0: u64 = 1_760_000_000_000_000;

    fn heartbeat_times(bytes: &[u8]) -> Option<(f64, f64)> {
        let log = import_reader(bytes, "test").unwrap();
        let series = log.series.iter().find(|s| s.name == "HEARTBEAT.custom_mode").unwrap();
        series.time_bounds().map(|(lo, hi)| (lo - T0 as f64 / 1e6, hi - T0 as f64 / 1e6))
    }

    #[test]
    fn an_undecodable_frame_does_not_end_the_import() {
        let heartbeat = MavMessage::HEARTBEAT(Default::default());
        // A frame with an id outside the dialect whose CRC still checks out
        // (unknown ids get a CRC_EXTRA of 0), so mavlink-core hands it back
        // as a parse error rather than resyncing past it.
        let mut bad = record(T0 + 2_000_000, &heartbeat);
        let frame = &mut bad[8..];
        frame[7..10].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
        let len = 10 + frame[1] as usize;
        let crc = mavlink_core::calculate_crc(&frame[1..len], 0);
        frame[len..len + 2].copy_from_slice(&crc.to_le_bytes());

        let mut bytes = record(T0 + 1_000_000, &heartbeat);
        bytes.extend(bad);
        bytes.extend(record(T0 + 3_000_000, &heartbeat));
        assert_eq!(heartbeat_times(&bytes), Some((1.0, 3.0)));
    }

    #[test]
    fn a_corrupt_timestamp_does_not_stretch_the_timeline() {
        let heartbeat = MavMessage::HEARTBEAT(Default::default());
        let mut bytes = record(T0 + 1_000_000, &heartbeat);
        // Bytes read out of alignment after a resync: 1970, and 2^56 µs.
        bytes.extend(record(12_345, &heartbeat));
        bytes.extend(record(1 << 56, &heartbeat));
        bytes.extend(record(T0 + 3_000_000, &heartbeat));
        assert_eq!(heartbeat_times(&bytes), Some((1.0, 3.0)));
    }

    #[test]
    fn plausible_times() {
        let t = T0 as f64 / 1e6;
        assert!(plausible_time(t, None));
        assert!(plausible_time(t + 3600.0, Some(t)));
        assert!(!plausible_time(0.0, None));
        assert!(!plausible_time(t + 2.0 * 86_400.0, Some(t)));
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
