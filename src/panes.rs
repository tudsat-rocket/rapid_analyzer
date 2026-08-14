use std::collections::HashMap;

use egui::{Color32, Widget as _};
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, VLine};

use crate::audio_playback::AudioPlayback;
use crate::model::{Project, SourceId, SourceKind};
use crate::timeline::Timeline;
use crate::video_worker::VideoWorker;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Pane {
    Plot { source: SourceId, series: String },
    Video(SourceId),
    Audio(SourceId),
}

const CURSOR_COLOR: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x3D);
const TARGET_PLOT_POINTS: usize = 2000;

/// `None` means we already tried to open an audio device for this source and
/// failed (e.g. headless machine, no sound card) -- don't retry every frame.
pub type AudioPlayerSlot = Option<AudioPlayback>;

pub struct TreeBehavior<'a> {
    pub project: &'a mut Project,
    pub timeline: &'a mut Timeline,
    pub video_workers: &'a mut HashMap<SourceId, VideoWorker>,
    pub audio_players: &'a mut HashMap<SourceId, AudioPlayerSlot>,
}

impl<'a> TreeBehavior<'a> {
    fn source_name(&self, id: SourceId) -> String {
        self.project.source(id).map(|s| s.name.clone()).unwrap_or_else(|| "?".to_string())
    }

    fn plot_pane(&mut self, ui: &mut egui::Ui, source_id: SourceId, series_name: &str) {
        let Some(source) = self.project.source(source_id) else {
            ui.colored_label(Color32::RED, "source no longer loaded");
            return;
        };
        let offset = source.offset_seconds;
        let color = source.color;
        let SourceKind::Log(log) = &source.kind else {
            ui.colored_label(Color32::RED, "not a log source");
            return;
        };
        let Some(series) = log.series.iter().find(|s| s.name == series_name) else {
            ui.colored_label(Color32::RED, "series no longer present");
            return;
        };

        let (view_start, view_end) = (self.timeline.view_start, self.timeline.view_end);
        let (y_lo, y_hi) = match series.value_bounds_in_range(view_start, view_end, offset) {
            Some((lo, hi)) if hi > lo => {
                let pad = (hi - lo) * 0.1;
                (lo - pad, hi + pad)
            }
            Some((v, _)) => (v - 1.0, v + 1.0),
            None => (0.0, 1.0),
        };
        let points = series.slice_for_range(view_start, view_end, offset, TARGET_PLOT_POINTS);
        let cursor = self.timeline.cursor;
        let unit = series.unit.clone();

        if let Some(v) = series.value_at(cursor, offset) {
            let suffix = unit.as_deref().unwrap_or("");
            ui.small(format!("at playhead: {v:.4} {suffix}"));
        }

        let plot = Plot::new(("plot", source_id, series_name))
            .height(220.0)
            .allow_boxed_zoom(false)
            .label_formatter(move |pos| {
                let p = match pos {
                    egui_plot::HoverPosition::NearDataPoint { position, .. } => *position,
                    egui_plot::HoverPosition::Elsewhere { position } => *position,
                };
                Some(match &unit {
                    Some(u) => format!("{:.4} {u}\n{}", p.y, crate::timeline::format_utc(p.x)),
                    None => format!("{:.4}\n{}", p.y, crate::timeline::format_utc(p.x)),
                })
            });

        let mut clicked_time = None;
        let response = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max([view_start, y_lo], [view_end, y_hi]));
            let line_points: PlotPoints = points.clone().into();
            plot_ui.line(Line::new(series_name, line_points).color(color));
            plot_ui.vline(VLine::new("cursor", cursor).color(CURSOR_COLOR));
            if plot_ui.response().clicked() {
                if let Some(coord) = plot_ui.pointer_coordinate() {
                    clicked_time = Some(coord.x);
                }
            }
        });

        let new_bounds = response.transform.bounds();
        let (nx0, nx1) = (new_bounds.min()[0], new_bounds.max()[0]);
        if (nx0 - view_start).abs() > 1e-6 || (nx1 - view_end).abs() > 1e-6 {
            self.timeline.view_start = nx0;
            self.timeline.view_end = nx1;
        }
        if let Some(t) = clicked_time {
            self.timeline.seek(t, self.project.time_bounds());
            self.timeline.playing = false;
        }
    }

    fn video_pane(&mut self, ui: &mut egui::Ui, source_id: SourceId) {
        let Some(source) = self.project.source(source_id) else {
            ui.colored_label(Color32::RED, "source no longer loaded");
            return;
        };
        let SourceKind::Video(video) = &source.kind else {
            ui.colored_label(Color32::RED, "not a video source");
            return;
        };
        let local_t = source.to_local_time(self.timeline.cursor).clamp(0.0, video.duration.max(0.0));
        let path = source.path.clone();
        let guess_note = source.offset_seconds == 0.0 && video.start_utc_is_guess;

        let worker = self
            .video_workers
            .entry(source_id)
            .or_insert_with(|| VideoWorker::new(path));
        worker.request_frame(local_t);
        worker.poll(ui.ctx());

        if guess_note {
            ui.small(format!(
                "⚠ start time guessed from file mtime ({}) -- adjust the offset in the source list if needed",
                crate::timeline::format_utc(video.start_utc)
            ));
        }

        if let Some(texture) = &worker.texture {
            let avail = ui.available_size();
            let aspect = video.width.max(1) as f32 / video.height.max(1) as f32;
            let mut size = avail;
            if size.x / size.y > aspect {
                size.x = size.y * aspect;
            } else {
                size.y = size.x / aspect;
            }
            egui::Image::new(texture).fit_to_exact_size(size).ui(ui);
        } else if let Some(err) = &worker.error {
            ui.colored_label(Color32::RED, err);
        } else {
            ui.spinner();
        }
    }

    fn audio_pane(&mut self, ui: &mut egui::Ui, source_id: SourceId) {
        let Some(source) = self.project.source(source_id) else {
            ui.colored_label(Color32::RED, "source no longer loaded");
            return;
        };
        let SourceKind::Audio(audio) = &source.kind else {
            ui.colored_label(Color32::RED, "not an audio source");
            return;
        };
        let color = source.color;
        let path = source.path.clone();
        let duration = audio.duration;
        // `to_master_time(0.0)` / local-time base: absolute UTC start of this
        // recording, offset applied. Master time = master_base + local_time.
        let master_base = source.to_master_time(0.0);
        let local_t = source.to_local_time(self.timeline.cursor);
        if source.offset_seconds == 0.0 && audio.start_utc_is_guess {
            ui.small(format!(
                "⚠ start time guessed from file mtime ({}) -- adjust the offset in the source list if needed",
                crate::timeline::format_utc(audio.start_utc)
            ));
        }
        // Clone out of the borrow (a few tens of KB) so we're free to call
        // `&mut self` methods like `ensure_audio_player` below.
        let peaks = audio.waveform_peaks.clone();

        self.ensure_audio_player(source_id, &path);
        if let Some(Some(player)) = self.audio_players.get_mut(&source_id) {
            player.sync(local_t, self.timeline.playing);
        }

        let (view_start, view_end) = (self.timeline.view_start, self.timeline.view_end);
        let n = peaks.len().max(1);
        let dt = duration / n as f64;

        let lo_idx = (((view_start - master_base) / dt).floor().max(0.0) as usize).min(n);
        let hi_idx = (((view_end - master_base) / dt).ceil().max(0.0) as usize + 1).min(n);

        let mut env = Vec::with_capacity((hi_idx - lo_idx) * 2);
        for i in lo_idx..hi_idx {
            let t = master_base + i as f64 * dt;
            env.push([t, peaks[i][0] as f64]);
            env.push([t, peaks[i][1] as f64]);
        }

        let cursor = self.timeline.cursor;
        let mut clicked_time = None;
        let plot = Plot::new(("audio", source_id)).height(160.0).allow_boxed_zoom(false).show_y(false);
        let response = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max([view_start, -1.05], [view_end, 1.05]));
            let pts: PlotPoints = env.clone().into();
            plot_ui.line(Line::new("waveform", pts).color(color));
            plot_ui.vline(VLine::new("cursor", cursor).color(CURSOR_COLOR));
            if plot_ui.response().clicked() {
                if let Some(coord) = plot_ui.pointer_coordinate() {
                    clicked_time = Some(coord.x);
                }
            }
        });
        let new_bounds = response.transform.bounds();
        let (nx0, nx1) = (new_bounds.min()[0], new_bounds.max()[0]);
        if (nx0 - view_start).abs() > 1e-6 || (nx1 - view_end).abs() > 1e-6 {
            self.timeline.view_start = nx0;
            self.timeline.view_end = nx1;
        }
        if let Some(t) = clicked_time {
            self.timeline.seek(t, self.project.time_bounds());
            self.timeline.playing = false;
        }

        if matches!(self.audio_players.get(&source_id), Some(None)) {
            ui.colored_label(Color32::YELLOW, "audio playback unavailable (no output device) -- waveform still shown");
        }
    }

    fn ensure_audio_player(&mut self, source_id: SourceId, path: &std::path::Path) {
        if self.audio_players.contains_key(&source_id) {
            return;
        }
        match AudioPlayback::new(path) {
            Ok(p) => {
                self.audio_players.insert(source_id, Some(p));
            }
            Err(e) => {
                log::warn!("audio playback init failed for {}: {e:#}", path.display());
                self.audio_players.insert(source_id, None);
            }
        }
    }
}

impl<'a> egui_tiles::Behavior<Pane> for TreeBehavior<'a> {
    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        match pane {
            Pane::Plot { source, series } => format!("{series}  [{}]", self.source_name(*source)).into(),
            Pane::Video(id) => format!("🎬 {}", self.source_name(*id)).into(),
            Pane::Audio(id) => format!("🔊 {}", self.source_name(*id)).into(),
        }
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile_id: egui_tiles::TileId, pane: &mut Pane) -> egui_tiles::UiResponse {
        match pane.clone() {
            Pane::Plot { source, series } => self.plot_pane(ui, source, &series),
            Pane::Video(id) => self.video_pane(ui, id),
            Pane::Audio(id) => self.audio_pane(ui, id),
        }
        egui_tiles::UiResponse::None
    }
}
