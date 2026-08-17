use std::path::PathBuf;

use egui::Color32;

use crate::can::CanFrames;
use crate::import::start_time::StartTimeSource;
use crate::series::TimeSeries;

pub type SourceId = u64;

/// A whole imported experiment: every data source the user has loaded.
#[derive(Default)]
pub struct Project {
    pub sources: Vec<Source>,
    next_id: SourceId,
}

impl Project {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alloc_id(&mut self) -> SourceId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn source(&self, id: SourceId) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// Master timeline bounds across every loaded source (regardless of the
    /// `enabled` toggle, so hiding a source doesn't reshuffle the scrubber
    /// range), with each source's `offset_seconds` applied.
    pub fn time_bounds(&self) -> Option<(f64, f64)> {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for s in &self.sources {
            if let Some((a, b)) = s.time_bounds() {
                lo = lo.min(a + s.offset_seconds);
                hi = hi.max(b + s.offset_seconds);
            }
        }
        if lo.is_finite() && hi.is_finite() {
            Some((lo, hi))
        } else {
            None
        }
    }
}

pub struct Source {
    pub id: SourceId,
    pub name: String,
    pub path: PathBuf,
    /// User-adjustable correction added to this source's raw UTC timestamps
    /// so it lines up with the other sources on the master timeline.
    pub offset_seconds: f64,
    pub color: Color32,
    pub enabled: bool,
    pub kind: SourceKind,
}

impl Source {
    /// Bounds in absolute UTC seconds (before `offset_seconds` is applied),
    /// consistent across source kinds: log timestamps are already absolute,
    /// while video/audio run on a 0-based local clock and need their
    /// `start_utc` added in to land on the same timeline.
    pub fn time_bounds(&self) -> Option<(f64, f64)> {
        match &self.kind {
            SourceKind::Log(log) => log.series.iter().filter_map(|s| s.time_bounds()).fold(None, |acc, (a, b)| {
                Some(match acc {
                    Some((lo, hi)) => (lo.min(a), hi.max(b)),
                    None => (a, b),
                })
            }),
            SourceKind::Video(v) => Some((v.start_utc, v.start_utc + v.duration)),
            SourceKind::Audio(a) => Some((a.start_utc, a.start_utc + a.duration)),
        }
    }

    /// Converts a master-timeline (absolute UTC + `offset_seconds`) instant
    /// into this source's own local clock: raw UTC seconds for logs, or
    /// 0-based seconds-from-start for video/audio.
    pub fn to_local_time(&self, master_time: f64) -> f64 {
        let base = match &self.kind {
            SourceKind::Log(_) => 0.0,
            SourceKind::Video(v) => v.start_utc,
            SourceKind::Audio(a) => a.start_utc,
        };
        master_time - self.offset_seconds - base
    }

    /// Inverse of [`Self::to_local_time`].
    pub fn to_master_time(&self, local_time: f64) -> f64 {
        let base = match &self.kind {
            SourceKind::Log(_) => 0.0,
            SourceKind::Video(v) => v.start_utc,
            SourceKind::Audio(a) => a.start_utc,
        };
        local_time + self.offset_seconds + base
    }
}

pub enum SourceKind {
    Log(LogSource),
    Video(VideoSource),
    Audio(AudioSource),
}

pub struct LogSource {
    pub series: Vec<TimeSeries>,
    pub format: LogFormat,
    /// Raw CAN traffic the log carried, kept alongside the series decoded
    /// from it so further signals can be pulled out without re-importing.
    pub can: CanFrames,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogFormat {
    Tlog,
    SqliteLog,
}

pub struct VideoSource {
    pub duration: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    /// Best-guess UTC time of the first frame.
    pub start_utc: f64,
    pub start_utc_source: StartTimeSource,
}

pub struct AudioSource {
    pub duration: f64,
    pub start_utc: f64,
    pub start_utc_source: StartTimeSource,
    /// Min/max envelope for waveform display, evenly spaced across `duration`.
    pub waveform_peaks: Vec<[f32; 2]>,
}
