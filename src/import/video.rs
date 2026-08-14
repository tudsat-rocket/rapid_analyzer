//! Video import: metadata via `ffprobe`, frame extraction via `ffmpeg`.
//! Both are external processes -- there is no mature pure-Rust decoder for
//! arbitrary mp4/H.264 content, and shelling out keeps the build light.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::model::VideoSource;

pub fn ffmpeg_available() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

pub fn probe(path: &Path) -> Result<VideoSource> {
    let output = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-print_format", "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .context("running ffprobe (is it installed?)")?;
    anyhow::ensure!(output.status.success(), "ffprobe failed for {}", path.display());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).context("parsing ffprobe JSON")?;

    let video_stream = json["streams"]
        .as_array()
        .and_then(|streams| streams.iter().find(|s| s["codec_type"] == "video"))
        .context("no video stream found")?;

    let width = video_stream["width"].as_u64().unwrap_or(0) as u32;
    let height = video_stream["height"].as_u64().unwrap_or(0) as u32;
    let fps = parse_frame_rate(video_stream["r_frame_rate"].as_str().unwrap_or("30/1"));

    let duration = json["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| video_stream["duration"].as_str().and_then(|s| s.parse::<f64>().ok()))
        .unwrap_or(0.0);

    let creation_time = json["format"]["tags"]["creation_time"]
        .as_str()
        .or_else(|| video_stream["tags"]["creation_time"].as_str());

    let (start_utc, start_utc_is_guess) = match creation_time.and_then(parse_iso8601) {
        Some(t) => (t, false),
        None => (file_mtime_utc(path).unwrap_or(0.0), true),
    };

    Ok(VideoSource {
        duration,
        fps,
        width,
        height,
        start_utc,
        start_utc_is_guess,
    })
}

/// Extract a single frame nearest `t_seconds` into the video as RGBA pixels.
pub fn extract_frame(path: &Path, t_seconds: f64) -> Result<image::RgbaImage> {
    let t = t_seconds.max(0.0);
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-noaccurate_seek"])
        .args(["-ss", &format!("{t:.3}")])
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .stdin(Stdio::null())
        .output()
        .context("running ffmpeg to extract a frame (is it installed?)")?;

    if !output.status.success() || output.stdout.is_empty() {
        bail!(
            "ffmpeg produced no frame at t={t:.3}s: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let img = image::load_from_memory(&output.stdout).context("decoding extracted frame")?;
    Ok(img.to_rgba8())
}

fn parse_frame_rate(s: &str) -> f64 {
    if let Some((num, den)) = s.split_once('/') {
        let num: f64 = num.parse().unwrap_or(30.0);
        let den: f64 = den.parse().unwrap_or(1.0);
        if den > 0.0 {
            return num / den;
        }
    }
    s.parse().unwrap_or(30.0)
}

pub(crate) fn parse_iso8601(s: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp() as f64 + dt.timestamp_subsec_micros() as f64 / 1_000_000.0)
}

pub(crate) fn file_mtime_utc(path: &Path) -> Option<f64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let dur: Duration = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(dur.as_secs_f64())
}
