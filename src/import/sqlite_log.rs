//! Importer for the custom SQLite telemetry log format:
//! `sensor_data(timestamp REAL unix_seconds, sensor_name TEXT, value REAL)`
//! in long/tidy form, pivoted here into one [`TimeSeries`] per sensor.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::model::{LogFormat, LogSource};
use crate::series::TimeSeries;

pub fn import(path: &Path) -> Result<LogSource> {
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;

    let mut stmt = conn
        .prepare("SELECT timestamp, sensor_name, value FROM sensor_data ORDER BY sensor_name, timestamp")
        .context("sensor_data table not found (expected timestamp/sensor_name/value columns)")?;

    let mut series: HashMap<String, Vec<[f64; 2]>> = HashMap::new();
    let rows = stmt.query_map([], |row| {
        let t: f64 = row.get(0)?;
        let name: String = row.get(1)?;
        let v: f64 = row.get(2)?;
        Ok((t, name, v))
    })?;

    let mut n_rows = 0u64;
    for row in rows {
        let (t, name, v) = row?;
        series.entry(name).or_default().push([t, v]);
        n_rows += 1;
    }
    anyhow::ensure!(n_rows > 0, "sensor_data table in {} is empty", path.display());

    let mut out: Vec<TimeSeries> = series
        .into_iter()
        .map(|(name, points)| TimeSeries::from_points(name, points))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(LogSource {
        series: out,
        format: LogFormat::SqliteLog,
        can: Default::default(),
    })
}
