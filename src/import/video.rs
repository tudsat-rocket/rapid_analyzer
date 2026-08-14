//! Video import: metadata via `ffprobe`, frame extraction via `ffmpeg`.
//! Both are external processes -- there is no mature pure-Rust decoder for
//! arbitrary mp4/H.264 content, and shelling out keeps the build light.

use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use anyhow::{Context, Result};

use crate::import::start_time;
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

    let (start_utc, start_utc_source) = start_time::resolve(path, creation_time);

    Ok(VideoSource {
        duration,
        fps,
        width,
        height,
        start_utc,
        start_utc_source,
    })
}

/// Frames are downscaled to at most this wide before they reach us: a video
/// pane is a few hundred pixels across, and shipping 4K frames through a pipe
/// and up to the GPU 25 times a second is the difference between smooth
/// playback and a slideshow.
const MAX_DECODE_WIDTH: u32 = 1280;

/// A running `ffmpeg` that hands out consecutive frames.
///
/// One process per *playback position*, not per frame: seeking and spinning
/// up ffmpeg costs tens of milliseconds, which is fine once but hopeless at
/// 25 frames a second. As long as the playhead keeps moving forward the same
/// process just keeps producing the next frame.
pub struct FrameStream {
    child: Child,
    stdout: ChildStdout,
    width: u32,
    height: u32,
    frame_dt: f64,
    /// Position, in seconds into the file, of the frame that `next()` will
    /// return.
    next_time: f64,
    /// Set once ffmpeg has run out of frames, so we stop reading a dead pipe.
    exhausted: bool,
    buf: Vec<u8>,
}

impl FrameStream {
    /// Starts decoding at `start`, emitting frames at a constant `fps`.
    ///
    /// The constant rate is what lets us tell time by counting frames: with
    /// `-ss` applied before the input, ffmpeg restarts output timestamps at
    /// the seek point, so frame `n` is exactly `start + n / fps`.
    pub fn start(path: &Path, start: f64, fps: f64, source_width: u32, source_height: u32) -> Result<Self> {
        let fps = if fps.is_finite() && fps > 0.0 { fps.min(120.0) } else { 30.0 };
        let (width, height) = decode_size(source_width, source_height);

        let mut child = Command::new("ffmpeg")
            .args(["-v", "error"])
            .args(["-ss", &format!("{:.3}", start.max(0.0))])
            .arg("-i")
            .arg(path)
            // No audio or subtitle streams: we only want pixels.
            .args(["-an", "-sn"])
            .args(["-vf", &format!("scale={width}:{height}")])
            .args(["-r", &format!("{fps}")])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("running ffmpeg to decode video (is it installed?)")?;

        let stdout = child.stdout.take().context("capturing ffmpeg stdout")?;
        Ok(Self {
            child,
            stdout,
            width,
            height,
            frame_dt: 1.0 / fps,
            next_time: start.max(0.0),
            exhausted: false,
            buf: vec![0; width as usize * height as usize * 4],
        })
    }

    pub fn frame_dt(&self) -> f64 {
        self.frame_dt
    }

    /// Position of the next frame this stream will produce.
    pub fn next_time(&self) -> f64 {
        self.next_time
    }

    pub fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// Decodes the next frame, or `None` at the end of the video.
    pub fn next_frame(&mut self) -> Result<Option<(f64, image::RgbaImage)>> {
        if self.exhausted {
            return Ok(None);
        }
        match self.stdout.read_exact(&mut self.buf) {
            Ok(()) => {}
            // A short read is the end of the stream, not a failure: ffmpeg
            // closes the pipe when the file runs out.
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                self.exhausted = true;
                return Ok(None);
            }
            Err(e) => {
                self.exhausted = true;
                return Err(e).context("reading a decoded frame from ffmpeg");
            }
        }
        let time = self.next_time;
        self.next_time += self.frame_dt;
        let image = image::RgbaImage::from_raw(self.width, self.height, self.buf.clone())
            .context("decoded frame had an unexpected size")?;
        Ok(Some((time, image)))
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        // Seeking elsewhere abandons a stream mid-file; without this, ffmpeg
        // would sit blocked on a full pipe forever.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn decode_size(width: u32, height: u32) -> (u32, u32) {
    let (width, height) = (width.max(2), height.max(2));
    if width <= MAX_DECODE_WIDTH {
        return (even(width), even(height));
    }
    let scaled_height = (height as f64 * MAX_DECODE_WIDTH as f64 / width as f64).round() as u32;
    (even(MAX_DECODE_WIDTH), even(scaled_height.max(2)))
}

/// Odd dimensions break several of ffmpeg's scalers.
fn even(v: u32) -> u32 {
    v & !1
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
