//! Off-UI-thread frame extraction for a video source. `ffmpeg` seeking +
//! decoding a frame can take tens of milliseconds, which would otherwise
//! stall the UI thread on every scrub.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;

use crate::import::video;

type FrameResult = Result<image::RgbaImage, String>;

pub struct VideoWorker {
    request_tx: Sender<f64>,
    response_rx: Receiver<(f64, FrameResult)>,
    pub texture: Option<egui::TextureHandle>,
    pub texture_for_time: Option<f64>,
    last_requested: Option<f64>,
    pub error: Option<String>,
}

impl VideoWorker {
    pub fn new(path: PathBuf) -> Self {
        let (request_tx, request_rx): (Sender<f64>, Receiver<f64>) = channel();
        let (response_tx, response_rx) = channel();

        thread::spawn(move || {
            let mut last_served: Option<f64> = None;
            while let Ok(mut t) = request_rx.recv() {
                // Coalesce: only serve the most recent scrub position.
                loop {
                    match request_rx.try_recv() {
                        Ok(newer) => t = newer,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                if last_served.is_some_and(|s| (s - t).abs() < 1e-6) {
                    continue;
                }
                let result = video::extract_frame(&path, t).map_err(|e| e.to_string());
                last_served = Some(t);
                if response_tx.send((t, result)).is_err() {
                    return;
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
        while let Ok((t, result)) = self.response_rx.try_recv() {
            latest = Some((t, result));
        }
        if let Some((t, result)) = latest {
            match result {
                Ok(img) => {
                    let size = [img.width() as usize, img.height() as usize];
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    // Update the existing texture in place rather than
                    // allocating a new GPU texture per frame -- with 4K
                    // video that's tens of MB uploaded on every scrub tick.
                    match &mut self.texture {
                        Some(handle) => handle.set(color_image, egui::TextureOptions::LINEAR),
                        None => {
                            self.texture = Some(ctx.load_texture("video-frame", color_image, egui::TextureOptions::LINEAR));
                        }
                    }
                    self.texture_for_time = Some(t);
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
    }
}
