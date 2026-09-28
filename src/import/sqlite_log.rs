//! Importer for the custom SQLite telemetry log format:
//! `sensor_data(timestamp REAL unix_seconds, sensor_name TEXT, value REAL)`
//! in long/tidy form, pivoted here into one [`TimeSeries`] per sensor.
//!
//! Two readers feed the same pivot: [`import_bytes`], through the plain-Rust
//! [`sqlite_file`](super::sqlite_file) reader, which is what the web build
//! has; and, with the `sqlite` feature, [`import`] through `rusqlite`.

use std::collections::HashMap;
#[cfg(feature = "sqlite")]
use std::path::Path;

use anyhow::{Context, Result};

use super::sqlite_file::{Database, Value};
use crate::model::{LogFormat, LogSource};
use crate::series::TimeSeries;

const TABLE: &str = "sensor_data";

/// Every row's `(timestamp, sensor_name, value)`, grouped by sensor.
#[derive(Default)]
struct Pivot {
    series: HashMap<String, Vec<[f64; 2]>>,
    rows: u64,
}

impl Pivot {
    fn push(&mut self, t: f64, name: &str, v: f64) {
        match self.series.get_mut(name) {
            Some(points) => points.push([t, v]),
            None => {
                self.series.insert(name.to_string(), vec![[t, v]]);
            }
        }
        self.rows += 1;
    }

    fn finish(self, label: &str) -> Result<LogSource> {
        anyhow::ensure!(self.rows > 0, "{TABLE} table in {label} is empty");
        // `from_points` sorts by time, so the order rows arrived in -- rowid
        // order, or whatever a query returned -- does not matter.
        let mut out: Vec<TimeSeries> = self
            .series
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
}

/// Imports a sensor log from the database file's bytes. `label` names it in
/// messages.
pub fn import_bytes(bytes: &[u8], label: &str) -> Result<LogSource> {
    let db = Database::open(bytes).with_context(|| format!("opening {label}"))?;
    let table = db
        .table(TABLE)
        .context("sensor_data table not found (expected timestamp/sensor_name/value columns)")?;
    let column = |name: &str| {
        table
            .column(name)
            .with_context(|| format!("{TABLE} has no {name} column (expected timestamp/sensor_name/value)"))
    };
    let (t_col, name_col, v_col) = (column("timestamp")?, column("sensor_name")?, column("value")?);

    let mut pivot = Pivot::default();
    db.for_each_row(&table, |row| {
        // A row that is not a number, a name and a number fails the import,
        // as it does through rusqlite, rather than being dropped unnoticed.
        let number = |i: usize, what: &str| {
            row[i]
                .as_f64()
                .with_context(|| format!("row {}: {what} is {:?}, not a number", pivot.rows + 1, row[i]))
        };
        let t = number(t_col, "timestamp")?;
        let v = number(v_col, "value")?;
        let Value::Text(name) = row[name_col] else {
            anyhow::bail!("row {}: sensor_name is {:?}, not text", pivot.rows + 1, row[name_col]);
        };
        pivot.push(t, name, v);
        Ok(())
    })
    .with_context(|| format!("reading {label}"))?;
    pivot.finish(label)
}

/// Imports a sensor log through SQLite itself.
#[cfg(feature = "sqlite")]
pub fn import(path: &Path) -> Result<LogSource> {
    use rusqlite::Connection;

    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;

    let mut stmt = conn
        .prepare("SELECT timestamp, sensor_name, value FROM sensor_data ORDER BY sensor_name, timestamp")
        .context("sensor_data table not found (expected timestamp/sensor_name/value columns)")?;

    let rows = stmt.query_map([], |row| {
        let t: f64 = row.get(0)?;
        let name: String = row.get(1)?;
        let v: f64 = row.get(2)?;
        Ok((t, name, v))
    })?;

    let mut pivot = Pivot::default();
    for row in rows {
        let (t, name, v) = row?;
        pivot.push(t, &name, v);
    }
    pivot.finish(&path.display().to_string())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;

    use super::*;

    /// Both readers, one database: the series must come out the same --
    /// names, lengths, and every point.
    #[test]
    fn both_readers_agree() {
        let dir = std::env::temp_dir().join(format!("rapid-analyzer-sqlite-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("telemetry");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE sensor_data (timestamp REAL, sensor_name TEXT, value REAL);
                 CREATE INDEX idx_ts ON sensor_data (timestamp);",
            )
            .unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            let mut insert = tx.prepare("INSERT INTO sensor_data VALUES (?1, ?2, ?3)").unwrap();
            // Written out of time order, and with whole-number values.
            for i in (0..5000).rev() {
                let t = 1.78e9 + f64::from(i) * 0.02;
                insert.execute((t, ["pressure", "temp", "thrust"][i as usize % 3], f64::from(i % 50))).unwrap();
            }
            drop(insert);
            tx.commit().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let via_sqlite = import(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let via_reader = import_bytes(&bytes, "telemetry").unwrap();

        let points = |log: &LogSource| -> Vec<(String, Vec<[f64; 2]>)> {
            log.series
                .iter()
                // Unbounded, and more points than there are: the raw samples.
                .map(|s| (s.name.clone(), s.slice_for_range(f64::NEG_INFINITY, f64::INFINITY, 0.0, usize::MAX)))
                .collect()
        };
        assert_eq!(via_reader.series.len(), 3);
        assert_eq!(points(&via_reader), points(&via_sqlite));
    }

    #[test]
    fn a_row_that_is_not_a_reading_fails_the_import() {
        let dir = std::env::temp_dir().join(format!("rapid-analyzer-sqlite-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("telemetry");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sensor_data (timestamp REAL, sensor_name TEXT, value REAL);
                 INSERT INTO sensor_data VALUES (1.0, 'p', 2.0), (2.0, 'p', NULL);",
            )
            .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let Err(e) = import_bytes(&bytes, "telemetry") else {
            panic!("a NULL value should have failed the import");
        };
        let err = format!("{e:#}");
        assert!(err.contains("row 2") && err.contains("value"), "{err}");
    }
}
