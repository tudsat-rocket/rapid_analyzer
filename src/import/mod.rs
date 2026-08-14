pub mod audio;
pub mod sqlite_log;
pub mod start_time;
pub mod tlog;
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

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if VIDEO_EXTS.contains(&ext.as_str()) {
        return Ok((name, SourceKind::Video(video::probe(path)?)));
    }
    if AUDIO_EXTS.contains(&ext.as_str()) {
        return Ok((name, SourceKind::Audio(audio::probe(path)?)));
    }
    if ext == "tlog" {
        return Ok((name, SourceKind::Log(tlog::import(path)?)));
    }

    // No (or unrecognized) extension: sniff the content.
    match sniff(path)? {
        Sniffed::Sqlite => Ok((name, SourceKind::Log(sqlite_log::import(path)?))),
        Sniffed::Tlog => Ok((name, SourceKind::Log(tlog::import(path)?))),
        Sniffed::Unknown => bail!(
            "couldn't recognize the format of {} (expected .tlog, a sensor_data SQLite log, or a video/audio file)",
            path.display()
        ),
    }
}

enum Sniffed {
    Sqlite,
    Tlog,
    Unknown,
}

fn sniff(path: &Path) -> Result<Sniffed> {
    let mut buf = [0u8; 32];
    let mut f = File::open(path)?;
    let n = f.read(&mut buf)?;
    let buf = &buf[..n];

    if buf.starts_with(b"SQLite format 3\0") {
        return Ok(Sniffed::Sqlite);
    }
    // A tlog record starts with an 8-byte big-endian microsecond timestamp
    // followed by a MAVLink v1 (0xFE) or v2 (0xFD) magic byte.
    if buf.len() > 8 && (buf[8] == 0xFD || buf[8] == 0xFE) {
        return Ok(Sniffed::Tlog);
    }
    Ok(Sniffed::Unknown)
}
