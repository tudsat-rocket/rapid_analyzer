//! Audio import: metadata + waveform peaks via `ffmpeg`/`ffprobe`.
//! Actual playback happens separately through `rodio` (see `audio_playback.rs`).

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::model::AudioSource;

/// Number of (min,max) buckets to generate for the waveform overview,
/// independent of the audio's actual length.
const WAVEFORM_BUCKETS: usize = 4000;
const PROBE_SAMPLE_RATE: u32 = 4000;

pub fn probe(path: &Path) -> Result<AudioSource> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .context("running ffprobe (is it installed?)")?;
    anyhow::ensure!(output.status.success(), "ffprobe failed for {}", path.display());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).context("parsing ffprobe JSON")?;

    let duration = json["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let creation_time = json["format"]["tags"]["creation_time"].as_str();
    let (start_utc, start_utc_is_guess) = match creation_time.and_then(super::video::parse_iso8601) {
        Some(t) => (t, false),
        None => (super::video::file_mtime_utc(path).unwrap_or(0.0), true),
    };

    let waveform_peaks = build_waveform(path, duration).unwrap_or_default();

    Ok(AudioSource {
        duration,
        start_utc,
        start_utc_is_guess,
        waveform_peaks,
    })
}

fn build_waveform(path: &Path, duration: f64) -> Result<Vec<[f32; 2]>> {
    if duration <= 0.0 {
        return Ok(Vec::new());
    }
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-ac", "1",
            "-ar", &PROBE_SAMPLE_RATE.to_string(),
            "-f", "f32le",
            "-",
        ])
        .stdin(Stdio::null())
        .output()
        .context("running ffmpeg to decode audio for waveform")?;
    anyhow::ensure!(output.status.success(), "ffmpeg PCM decode failed: {}", String::from_utf8_lossy(&output.stderr));

    let samples: Vec<f32> = output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let bucket_size = (samples.len() / WAVEFORM_BUCKETS).max(1);
    let mut peaks = Vec::with_capacity(samples.len() / bucket_size + 1);
    for chunk in samples.chunks(bucket_size) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &s in chunk {
            min = min.min(s);
            max = max.max(s);
        }
        peaks.push([min, max]);
    }
    Ok(peaks)
}
