//! Off-UI-thread frame decoding for a video source.
//!
//! The expensive part of showing a video frame is not decoding it, it is
//! *getting to* it: spawning `ffmpeg` and seeking costs tens of milliseconds,
//! so doing that per frame caps playback at a handful of frames a second no
//! matter how fast the codec is. Instead the worker keeps one decoder running
//! (`video::FrameStream`) and simply pulls the next frame while the playhead
//! moves forward, restarting only when the user jumps somewhere else.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::Instant;

use crate::import::video::FrameStream;

/// Never decode forward across more than this, however cheap frames look --
/// a stale estimate shouldn't be able to wedge the worker for minutes.
const MAX_FORWARD_DECODE: f64 = 30.0;

enum Response {
    Frame(f64, image::RgbaImage),
    Error(String),
}

pub struct VideoWorker {
    request_tx: Sender<f64>,
    response_rx: Receiver<Response>,
    pub texture: Option<egui::TextureHandle>,
    /// Position in the video (local seconds) the current texture shows.
    pub texture_for_time: Option<f64>,
    last_requested: Option<f64>,
    pub error: Option<String>,
}

impl VideoWorker {
    pub fn new(path: PathBuf, fps: f64, width: u32, height: u32) -> Self {
        let (request_tx, request_rx) = channel::<f64>();
        let (response_tx, response_rx) = channel();

        thread::spawn(move || {
            let mut decoder: Option<FrameStream> = None;
            let mut shown: Option<f64> = None;
            let mut pacing = Pacing::default();
            // Earliest position known to decode to nothing. Without this, a
            // playhead sitting past the last frame would respawn ffmpeg on
            // every UI frame.
            let mut eof_after: Option<f64> = None;
            let mut seek_started: Option<Instant> = None;

            while let Ok(mut request) = request_rx.recv() {
                // Only the most recent scrub position matters; anything the
                // UI queued while we were busy is already stale.
                loop {
                    match request_rx.try_recv() {
                        Ok(newer) => request = newer,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                let t = request.max(0.0);

                if !can_reach(decoder.as_ref(), shown, t, &pacing) {
                    if eof_after.is_some_and(|end| t >= end) {
                        continue;
                    }
                    seek_started = Some(Instant::now());
                    decoder = match FrameStream::start(&path, t, fps, width, height) {
                        Ok(d) => Some(d),
                        Err(e) => {
                            seek_started = None;
                            if response_tx.send(Response::Error(format!("{e:#}"))).is_err() {
                                return;
                            }
                            continue;
                        }
                    };
                    shown = None;
                }
                let Some(stream) = decoder.as_mut() else {
                    continue;
                };

                // Walk forward to the last frame at or before `t`. Frames in
                // between are decoded and dropped -- that is what makes
                // playback continuous rather than a series of seeks.
                let mut latest = None;
                let mut decoded = 0u32;
                let started = Instant::now();
                while stream.next_time() <= t && !stream.exhausted() {
                    match stream.next_frame() {
                        Ok(Some(frame)) => {
                            decoded += 1;
                            latest = Some(frame);
                        }
                        Ok(None) => break,
                        Err(e) => {
                            if response_tx.send(Response::Error(format!("{e:#}"))).is_err() {
                                return;
                            }
                            break;
                        }
                    }
                }

                match seek_started.take() {
                    // The seek's cost lands on the first frame read, since
                    // spawning ffmpeg returns before it has decoded anything.
                    Some(at) if decoded > 0 => pacing.record_seek(at.elapsed().as_secs_f64()),
                    _ => pacing.record_decode(decoded, started.elapsed().as_secs_f64()),
                }

                match latest {
                    Some((time, image)) => {
                        shown = Some(time);
                        if response_tx.send(Response::Frame(time, image)).is_err() {
                            return;
                        }
                    }
                    // A freshly started stream that yields nothing is past
                    // the end of the file, whatever the container's declared
                    // duration says.
                    None if shown.is_none() && stream.exhausted() => {
                        eof_after = Some(eof_after.map_or(t, |end| end.min(t)));
                    }
                    None => {}
                }
            }
        });

        Self {
            request_tx,
            response_rx,
            texture: None,
            texture_for_time: None,
            last_requested: None,
            error: None,
        }
    }

    pub fn request_frame(&mut self, t: f64) {
        if self.last_requested.is_some_and(|r| (r - t).abs() < 1e-6) {
            return;
        }
        self.last_requested = Some(t);
        let _ = self.request_tx.send(t);
    }

    /// Pull any newly-decoded frame into a GPU texture. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut latest = None;
        while let Ok(response) = self.response_rx.try_recv() {
            match response {
                Response::Frame(t, image) => latest = Some((t, image)),
                Response::Error(e) => self.error = Some(e),
            }
        }
        let Some((t, img)) = latest else {
            return;
        };
        let size = [img.width() as usize, img.height() as usize];
        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
        // Update the existing texture in place rather than allocating a new
        // GPU texture per frame -- at 25 fps that is a lot of churn.
        match &mut self.texture {
            Some(handle) => handle.set(color_image, egui::TextureOptions::LINEAR),
            None => {
                self.texture = Some(ctx.load_texture("video-frame", color_image, egui::TextureOptions::LINEAR));
            }
        }
        self.texture_for_time = Some(t);
        self.error = None;
    }
}

/// Running estimate of what the two ways of reaching a frame cost.
///
/// Which one wins depends entirely on the file: seeking a VP9 webm costs
/// about a third of a second, while decoding the same file runs at nearly
/// twenty times real time, so it is worth decoding *seconds* of video rather
/// than seeking. A 4K stream inverts that. Measuring beats guessing, and both
/// numbers are had for free while doing the work anyway.
struct Pacing {
    /// Frames decoded per second of wall time.
    decode_rate: f64,
    /// Wall seconds to start a decoder and get its first frame.
    seek_cost: f64,
}

impl Default for Pacing {
    fn default() -> Self {
        // Rough figures for a downscaled SD stream; both are replaced by
        // measurements within the first couple of interactions.
        Self {
            decode_rate: 200.0,
            seek_cost: 0.3,
        }
    }
}

impl Pacing {
    fn record_seek(&mut self, seconds: f64) {
        self.seek_cost = blend(self.seek_cost, seconds.clamp(0.01, 5.0));
    }

    fn record_decode(&mut self, frames: u32, seconds: f64) {
        // One or two frames is mostly measurement noise; wait for a batch.
        if frames >= 4 && seconds > 0.0 {
            self.decode_rate = blend(self.decode_rate, (frames as f64 / seconds).clamp(1.0, 10_000.0));
        }
    }

    /// How far forward it is still worth decoding rather than seeking.
    fn forward_budget(&self, fps: f64) -> f64 {
        let fps = if fps > 0.0 { fps } else { 30.0 };
        (self.seek_cost * self.decode_rate / fps).min(MAX_FORWARD_DECODE)
    }
}

fn blend(current: f64, sample: f64) -> f64 {
    current * 0.7 + sample * 0.3
}

/// Whether `stream` can serve time `t` by decoding forward, or whether it has
/// to be restarted: going backwards is impossible, and a jump far enough
/// forward is cheaper as a fresh seek than as the frames in between.
fn can_reach(stream: Option<&FrameStream>, shown: Option<f64>, t: f64, pacing: &Pacing) -> bool {
    let Some(stream) = stream else {
        return false;
    };
    if stream.exhausted() {
        // Nothing left to decode, but the last frame stays valid until the
        // playhead moves off it.
        return shown.is_some_and(|s| t >= s);
    }
    // Half a frame of slack, so the tiny backwards drift between the
    // playhead and a frame's own timestamp doesn't trigger a re-seek.
    let earliest = shown.unwrap_or(stream.next_time()) - stream.frame_dt() * 0.5;
    let budget = pacing.forward_budget(1.0 / stream.frame_dt());
    t >= earliest && t <= stream.next_time() + budget
}
