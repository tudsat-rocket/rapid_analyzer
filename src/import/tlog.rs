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

use crate::dialect::rapid::MavMessage;
use crate::model::{LogFormat, LogSource};
use crate::series::TimeSeries;

pub fn import(path: &Path) -> Result<LogSource> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = PeekReader::new(BufReader::new(file));

    let mut series: HashMap<String, Vec<[f64; 2]>> = HashMap::new();
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
            Ok((_header, msg)) => {
                n_messages += 1;
                extract_fields(&msg, t_utc, &mut series);
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

    let mut out: Vec<TimeSeries> = series
        .into_iter()
        .map(|(name, points)| TimeSeries::from_points(name, points))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(LogSource {
        series: out,
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

/// Flatten every numeric field of a MAVLink message into `"MSG_NAME.field"`
/// series, generically, so we don't need hand-written glue for each message
/// type in the `rapid` dialect (the common set plus our own, see
/// `crate::dialect` and `mavlink_dialects/Rapid.xml`).
fn extract_fields(msg: &MavMessage, t_utc: f64, series: &mut HashMap<String, Vec<[f64; 2]>>) {
    let name = msg.message_name();
    let value = match serde_json::to_value(msg) {
        Ok(v) => v,
        Err(_) => return,
    };
    let serde_json::Value::Object(map) = value else {
        return;
    };
    for (field, field_value) in map {
        if field == "type" {
            continue;
        }
        push_numeric(&format!("{name}.{field}"), &field_value, t_utc, series);
    }
}

fn push_numeric(key: &str, value: &serde_json::Value, t_utc: f64, series: &mut HashMap<String, Vec<[f64; 2]>>) {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(v) = n.as_f64() {
                series.entry(key.to_string()).or_default().push([t_utc, v]);
            }
        }
        serde_json::Value::Bool(b) => {
            series
                .entry(key.to_string())
                .or_default()
                .push([t_utc, if *b { 1.0 } else { 0.0 }]);
        }
        serde_json::Value::Array(items) => {
            // Only expand arrays of plain numbers (skip byte/char arrays used
            // for fixed-size strings, which show up as arrays of small ints).
            if items.len() <= 64 && items.iter().all(|i| i.is_number()) {
                for (i, item) in items.iter().enumerate() {
                    push_numeric(&format!("{key}[{i}]"), item, t_utc, series);
                }
            }
        }
        _ => {}
    }
}
