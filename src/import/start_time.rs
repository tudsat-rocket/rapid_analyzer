//! Working out when a video or audio recording *started* in absolute UTC,
//! which is what places it on the master timeline next to the log.
//!
//! Container metadata is the only authoritative answer, and plenty of
//! recordings don't carry it: the `.webm`/`.ogx` examples in `examples/` have
//! no `creation_time` at all. Falling straight through to the file's mtime is
//! usually wrong by however long the file sat around before being copied --
//! days, for those examples -- which drops the media somewhere far off the
//! end of the log. The recording time is very often right there in the file
//! name instead (`video_abc_2026-08-08T17:41:58.webm`), so try that first.

use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

/// Where a source's `start_utc` came from, so the UI can say how much to
/// trust it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartTimeSource {
    /// The container's `creation_time` tag: authoritative.
    Metadata,
    /// A timestamp parsed out of the file name, assumed to be UTC.
    FileName,
    /// Last resort: when the file was last written.
    FileMtime,
}

impl StartTimeSource {
    pub fn is_guess(self) -> bool {
        self != StartTimeSource::Metadata
    }

    pub fn describe(self) -> &'static str {
        match self {
            StartTimeSource::Metadata => "container metadata",
            StartTimeSource::FileName => "the file name, read as UTC",
            StartTimeSource::FileMtime => "the file's modification time",
        }
    }
}

/// Best available start time for `path`, given the container's
/// `creation_time` tag if it had one.
pub fn resolve(path: &Path, creation_time: Option<&str>) -> (f64, StartTimeSource) {
    if let Some(t) = creation_time.and_then(parse_iso8601) {
        return (t, StartTimeSource::Metadata);
    }
    if let Some(t) = path.file_name().and_then(|n| n.to_str()).and_then(parse_file_name) {
        return (t, StartTimeSource::FileName);
    }
    (file_mtime_utc(path).unwrap_or(0.0), StartTimeSource::FileMtime)
}

pub fn parse_iso8601(s: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp() as f64 + dt.timestamp_subsec_micros() as f64 / 1_000_000.0)
}

fn file_mtime_utc(path: &Path) -> Option<f64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let dur: Duration = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(dur.as_secs_f64())
}

/// Finds the first date-and-time stamp anywhere in a file name.
///
/// Deliberately loose about separators, because every recorder writes them
/// differently (`2026-08-08T17:41:58`, `2026-08-08_17-41-58`,
/// `20260808_174158`). Without an explicit `Z` the stamp is read as UTC --
/// there is nothing in a file name to say otherwise, and the caller marks the
/// result as a guess so a wrong timezone is a slider nudge away from fixed.
pub fn parse_file_name(name: &str) -> Option<f64> {
    let chars: Vec<char> = name.chars().collect();
    (0..chars.len()).find_map(|i| parse_stamp_at(&chars[i..]))
}

fn parse_stamp_at(s: &[char]) -> Option<f64> {
    const DATE_SEPS: &[char] = &['-', '_', '.'];
    const TIME_SEPS: &[char] = &[':', '-', '_', '.'];
    // Between date and time: `T` (ISO), or any of the usual fillers.
    const DATE_TIME_SEPS: &[char] = &['T', 't', '_', '-', ' ', '.'];

    let mut p = 0usize;
    let year = digits(s, &mut p, 4)?;
    if !(1970..=2200).contains(&year) {
        return None;
    }
    skip_one_of(s, &mut p, DATE_SEPS);
    let month = digits(s, &mut p, 2)?;
    skip_one_of(s, &mut p, DATE_SEPS);
    let day = digits(s, &mut p, 2)?;

    if !skip_one_of(s, &mut p, DATE_TIME_SEPS) {
        return None;
    }
    let hour = digits(s, &mut p, 2)?;
    skip_one_of(s, &mut p, TIME_SEPS);
    let minute = digits(s, &mut p, 2)?;
    skip_one_of(s, &mut p, TIME_SEPS);
    let second = digits(s, &mut p, 2)?;

    // A digit immediately after the seconds means we mis-split some longer
    // number rather than reading a real timestamp.
    if s.get(p).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }

    let date = chrono::NaiveDate::from_ymd_opt(year, month as u32, day as u32)?;
    let time = date.and_hms_opt(hour as u32, minute as u32, second as u32)?;
    Some(time.and_utc().timestamp() as f64)
}

fn digits(s: &[char], p: &mut usize, n: usize) -> Option<i32> {
    let end = p.checked_add(n)?;
    let slice = s.get(*p..end)?;
    if !slice.iter().all(char::is_ascii_digit) {
        return None;
    }
    *p = end;
    slice.iter().collect::<String>().parse().ok()
}

fn skip_one_of(s: &[char], p: &mut usize, allowed: &[char]) -> bool {
    match s.get(*p) {
        Some(c) if allowed.contains(c) => {
            *p += 1;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> f64 {
        parse_iso8601(s).unwrap()
    }

    #[test]
    fn reads_the_example_media_names() {
        // These have to land on the tlog's own start time, or the video and
        // the graphs never line up out of the box.
        let expected = utc("2026-08-08T17:41:58Z");
        assert_eq!(parse_file_name("video_abc_2026-08-08T17:41:58.webm"), Some(expected));
        assert_eq!(parse_file_name("audio_xyz_2026-08-08T17:41:58.ogx"), Some(expected));
        assert_eq!(parse_file_name("2026-08-08T17:41:58Z-04.tlog"), Some(expected));
    }

    #[test]
    fn accepts_the_usual_separator_styles() {
        let expected = utc("2026-07-27T14:08:36Z");
        for name in [
            "telemetry_2026-07-27_14-08-36",
            "20260727_140836.mp4",
            "VID 2026.07.27 14.08.36.mov",
        ] {
            assert_eq!(parse_file_name(name), Some(expected), "{name}");
        }
    }

    #[test]
    fn ignores_names_without_a_timestamp() {
        assert_eq!(parse_file_name("hotfire.mp4"), None);
        assert_eq!(parse_file_name("clip_00012345678.mp4"), None);
        // A date on its own says nothing about when recording started.
        assert_eq!(parse_file_name("2026-08-08.mp4"), None);
        // Impossible date: keep looking rather than inventing a time.
        assert_eq!(parse_file_name("2026-13-40T99:99:99.mp4"), None);
    }
}
