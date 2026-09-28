#[cfg(feature = "media")]
pub mod audio;
pub mod sqlite_file;
pub mod sqlite_log;
pub mod start_time;
pub mod tlog;
#[cfg(feature = "media")]
pub mod video;

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Result, bail};

use crate::model::SourceKind;

const VIDEO_EXTS: &[&str] = &["mp4", "mov", "avi", "mkv", "webm", "m4v"];
const AUDIO_EXTS: &[&str] = &["m4a", "mp3", "wav", "aac", "flac", "ogg", "oga", "ogx", "opus", "wma"];

/// Detect the format of `path` (by extension, falling back to content
/// sniffing for extensionless files like the SQLite example log) and import
/// it into a [`SourceKind`], along with a human-friendly default name.
pub fn import_path(path: &Path) -> Result<(String, SourceKind)> {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    let ext = extension(&name);
    if VIDEO_EXTS.contains(&ext.as_str()) {
        #[cfg(feature = "media")]
        return Ok((name, SourceKind::Video(video::probe(path)?)));
        #[cfg(not(feature = "media"))]
        bail!("{name}: this build has no video support (it was built without the `media` feature)");
    }
    if AUDIO_EXTS.contains(&ext.as_str()) {
        #[cfg(feature = "media")]
        return Ok((name, SourceKind::Audio(audio::probe(path)?)));
        #[cfg(not(feature = "media"))]
        bail!("{name}: this build has no audio support (it was built without the `media` feature)");
    }

    let mut head = [0u8; 32];
    let n = File::open(path)?.read(&mut head)?;
    match detect(&ext, &head[..n]) {
        Some(Format::Tlog) => Ok((name, SourceKind::Log(tlog::import(path)?))),
        #[cfg(feature = "sqlite")]
        Some(Format::Sqlite) => Ok((name, SourceKind::Log(sqlite_log::import(path)?))),
        #[cfg(not(feature = "sqlite"))]
        Some(Format::Sqlite) => {
            let bytes = std::fs::read(path)?;
            Ok((name, SourceKind::Log(sqlite_log::import_bytes(&bytes, &path.display().to_string())?)))
        }
        None => bail!(
            "couldn't recognize the format of {} (expected .tlog, a sensor_data SQLite log, or a video/audio file)",
            path.display()
        ),
    }
}

/// [`import_path`] for a file that arrives as its contents: what a browser
/// hands over when a file is picked or dropped, since a web page never sees a
/// path. Logs only -- video and audio are decoded by `ffmpeg`, which needs a
/// file on disk.
pub fn import_bytes(name: &str, bytes: &[u8]) -> Result<(String, SourceKind)> {
    let ext = extension(name);
    if VIDEO_EXTS.contains(&ext.as_str()) || AUDIO_EXTS.contains(&ext.as_str()) {
        bail!("{name}: video and audio can only be opened in the desktop app");
    }
    match detect(&ext, bytes) {
        Some(Format::Tlog) => Ok((name.to_string(), SourceKind::Log(tlog::import_reader(bytes, name)?))),
        Some(Format::Sqlite) => Ok((name.to_string(), SourceKind::Log(sqlite_log::import_bytes(bytes, name)?))),
        None => bail!("couldn't recognize the format of {name} (expected a .tlog or a sensor_data SQLite log)"),
    }
}

fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

enum Format {
    Sqlite,
    Tlog,
}

/// A log's format, from its extension or, failing that, its first bytes.
fn detect(ext: &str, head: &[u8]) -> Option<Format> {
    if ext == "tlog" {
        return Some(Format::Tlog);
    }
    // No (or unrecognized) extension: sniff the content.
    if head.starts_with(b"SQLite format 3\0") {
        return Some(Format::Sqlite);
    }
    // A tlog record starts with an 8-byte big-endian microsecond timestamp
    // followed by a MAVLink v1 (0xFE) or v2 (0xFD) magic byte.
    if head.len() > 8 && (head[8] == 0xFD || head[8] == 0xFE) {
        return Some(Format::Tlog);
    }
    None
}

#[cfg(test)]
mod tests {
    use mavlink_core::{MavHeader, MavlinkVersion};

    use super::*;
    use crate::dialect::rapid::{FluidType, MavMessage, PRESSURE_VESSEL_DATA};

    /// A tlog of `n` `PRESSURE_VESSEL` messages, a second apart.
    fn tlog(n: u16) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..n {
            let t_us = 1_780_000_000_000_000u64 + u64::from(i) * 1_000_000;
            out.extend_from_slice(&t_us.to_be_bytes());
            let msg = MavMessage::PRESSURE_VESSEL(PRESSURE_VESSEL_DATA {
                id: 1,
                pressure1: 5000 + i,
                temperature1: 2000,
                pressure2: u16::MAX,
                temperature2: i16::MAX,
                level: u16::MAX,
                rated_pressure: 5500,
                volume: 8000,
                flags: Default::default(),
                fluid: FluidType::NITROGEN,
            });
            let header = MavHeader {
                system_id: 1,
                component_id: 1,
                sequence: i as u8,
            };
            mavlink_core::write_versioned_msg(&mut out, MavlinkVersion::V2, header, &msg).unwrap();
        }
        out
    }

    fn series(kind: SourceKind) -> Vec<(String, usize)> {
        let SourceKind::Log(log) = kind else {
            panic!("a tlog should import as a log");
        };
        log.series.iter().map(|s| (s.name.clone(), s.len())).collect()
    }

    /// The web build only ever has a file's bytes; what it imports from them
    /// must be what the desktop imports from the file.
    #[test]
    fn bytes_import_the_same_as_the_file() {
        let bytes = tlog(20);
        let dir = std::env::temp_dir().join(format!("rapid-analyzer-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.tlog");
        std::fs::write(&path, &bytes).unwrap();

        let (file_name, from_file) = import_path(&path).unwrap();
        let (bytes_name, from_bytes) = import_bytes("run.tlog", &bytes).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(file_name, bytes_name);
        let from_file = series(from_file);
        assert!(from_file.iter().any(|(name, len)| name.ends_with(".pressure1") && *len == 20));
        assert_eq!(from_file, series(from_bytes));
    }

    #[test]
    fn bytes_without_an_extension_are_sniffed() {
        let (_, kind) = import_bytes("run", &tlog(3)).unwrap();
        assert!(!series(kind).is_empty());
        assert!(import_bytes("notes", b"not a log at all, just some text").is_err());
    }

    #[test]
    fn bytes_refuse_what_only_the_desktop_can_open() {
        let Err(e) = import_bytes("clip.mp4", &[0; 64]) else {
            panic!("a video should have been refused");
        };
        assert!(e.to_string().contains("desktop"), "{e}");
    }

    /// Recognised as SQLite by its magic, and then refused as the broken
    /// database it is -- not mistaken for anything else.
    #[test]
    fn bytes_that_look_like_sqlite_go_to_the_sqlite_reader() {
        let Err(e) = import_bytes("sensors", b"SQLite format 3\0 and then some") else {
            panic!("a truncated database should have been refused");
        };
        assert!(format!("{e:#}").contains("not an SQLite database"), "{e:#}");
    }
}
