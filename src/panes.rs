use std::collections::HashMap;

use egui::{Color32, Widget as _};
use egui_plot::{Corner, Legend, Line, Plot, PlotBounds, PlotPoints, VLine};

use crate::audio_playback::AudioPlayback;
use crate::colors::color_for_index;
use crate::model::{Project, SourceId, SourceKind};
use crate::timeline::Timeline;
use crate::video_worker::VideoWorker;

pub type PlotId = u64;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Pane {
    Plot(PlotId),
    Video(SourceId),
    Audio(SourceId),
}

const CURSOR_COLOR: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x3D);
const TARGET_PLOT_POINTS: usize = 2000;

/// `None` means we already tried to open an audio device for this source and
/// failed (e.g. headless machine, no sound card) -- don't retry every frame.
pub type AudioPlayerSlot = Option<AudioPlayback>;

/// One series drawn in a graph.
#[derive(Clone)]
pub struct PlotEntry {
    pub source: SourceId,
    pub series: String,
    pub color: Color32,
}

/// A graph pane: any number of series sharing one time axis.
///
/// Several series per graph is the point -- comparing a tank's pressure
/// against its temperature only means something when the two are drawn over
/// each other -- so a plot is a list rather than a single series, and the
/// title is what identifies it once the contents stop being one obvious name.
pub struct PlotSpec {
    pub id: PlotId,
    pub entries: Vec<PlotEntry>,
    /// Only used once the user renames the plot; until then the title follows
    /// whatever is in it.
    custom_title: String,
    /// Rescale every series to 0..1 over the visible window. The escape hatch
    /// for comparing quantities whose ranges are orders of magnitude apart,
    /// where a shared axis would flatten one into a straight line.
    pub normalize: bool,
    /// Kept separate from `entries.len()` so removing a series doesn't
    /// recolour the ones that stay.
    color_cursor: usize,
}

impl PlotSpec {
    pub fn title(&self) -> String {
        if !self.custom_title.is_empty() {
            return self.custom_title.clone();
        }
        auto_title(&self.entries)
    }

    pub fn contains(&self, source: SourceId, series: &str) -> bool {
        self.entries.iter().any(|e| e.source == source && e.series == series)
    }
}

/// Every graph the user has opened, and the only place plot ids are minted.
#[derive(Default)]
pub struct Plots {
    list: Vec<PlotSpec>,
    next_id: PlotId,
}

impl Plots {
    pub fn iter(&self) -> impl Iterator<Item = &PlotSpec> {
        self.list.iter()
    }

    pub fn get(&self, id: PlotId) -> Option<&PlotSpec> {
        self.list.iter().find(|p| p.id == id)
    }

    fn get_mut(&mut self, id: PlotId) -> Option<&mut PlotSpec> {
        self.list.iter_mut().find(|p| p.id == id)
    }

    /// Opens a new graph showing `series`.
    pub fn create(&mut self, source: SourceId, series: String) -> PlotId {
        let id = self.next_id;
        self.next_id += 1;
        self.list.push(PlotSpec {
            id,
            entries: Vec::new(),
            custom_title: String::new(),
            normalize: false,
            color_cursor: 0,
        });
        self.add(id, source, series);
        id
    }

    pub fn add(&mut self, id: PlotId, source: SourceId, series: String) {
        let Some(plot) = self.get_mut(id) else {
            return;
        };
        if plot.contains(source, &series) {
            return;
        }
        let color = color_for_index(plot.color_cursor);
        plot.color_cursor += 1;
        plot.entries.push(PlotEntry { source, series, color });
    }

    pub fn shows(&self, source: SourceId, series: &str) -> bool {
        self.list.iter().any(|p| p.contains(source, series))
    }

    /// Drops a series from every graph. Returns the graphs left empty, which
    /// the caller closes.
    pub fn remove_series(&mut self, source: SourceId, series: &str) -> Vec<PlotId> {
        for plot in &mut self.list {
            plot.entries.retain(|e| e.source != source || e.series != series);
        }
        self.empty_plots()
    }

    /// Drops everything belonging to a source that is going away.
    pub fn remove_source(&mut self, source: SourceId) -> Vec<PlotId> {
        for plot in &mut self.list {
            plot.entries.retain(|e| e.source != source);
        }
        self.empty_plots()
    }

    pub fn close(&mut self, id: PlotId) {
        self.list.retain(|p| p.id != id);
    }

    fn empty_plots(&self) -> Vec<PlotId> {
        self.list.iter().filter(|p| p.entries.is_empty()).map(|p| p.id).collect()
    }
}

/// A name for a graph built from what it contains: the shared part of the
/// series names, then what differs. `PRESSURE_VESSEL[1].pressure1` next to
/// `PRESSURE_VESSEL[1].temperature1` reads as
/// `PRESSURE_VESSEL[1]: pressure1 / temperature1`.
fn auto_title(entries: &[PlotEntry]) -> String {
    match entries {
        [] => "empty plot".to_string(),
        [one] => one.series.clone(),
        _ => {
            let names: Vec<&str> = entries.iter().map(|e| e.series.as_str()).collect();
            let prefix = shared_prefix(&names);
            let joined = names
                .iter()
                .map(|n| &n[prefix.len()..])
                .collect::<Vec<_>>()
                .join(" / ");
            let body = if joined.len() > 60 {
                format!("{} +{} more", names[0], names.len() - 1)
            } else {
                joined
            };
            if prefix.is_empty() {
                body
            } else {
                format!("{}: {body}", prefix.trim_end_matches('.'))
            }
        }
    }
}

/// Longest common prefix of `names`, cut back to a `.` so it ends on a whole
/// message/instance rather than mid-word.
fn shared_prefix<'a>(names: &[&'a str]) -> &'a str {
    let Some(first) = names.first() else {
        return "";
    };
    let mut len = first.len();
    for name in &names[1..] {
        len = first
            .bytes()
            .zip(name.bytes())
            .take(len)
            .take_while(|(a, b)| a == b)
            .count();
    }
    match first[..len].rfind('.') {
        Some(dot) => &first[..dot + 1],
        None => "",
    }
}

pub struct TreeBehavior<'a> {
    pub project: &'a mut Project,
    pub plots: &'a mut Plots,
    pub timeline: &'a mut Timeline,
    pub video_workers: &'a mut HashMap<SourceId, VideoWorker>,
    pub audio_players: &'a mut HashMap<SourceId, AudioPlayerSlot>,
    /// Panes the user closed from their tab or emptied from the ⚙ menu, for
    /// `App` to unregister once the tree is done drawing.
    pub closed: Vec<Pane>,
}

/// Everything a plot pane needs about one series, copied out of the project
/// so the drawing closure doesn't hold a borrow on it.
struct PreparedSeries {
    label: String,
    color: Color32,
    unit: Option<String>,
    points: Vec<[f64; 2]>,
    /// Value range over the visible window, before any normalization.
    bounds: Option<(f64, f64)>,
    at_cursor: Option<f64>,
}

impl<'a> TreeBehavior<'a> {
    fn source_name(&self, id: SourceId) -> String {
        self.project.source(id).map(|s| s.name.clone()).unwrap_or_else(|| "?".to_string())
    }

    fn plot_pane(&mut self, ui: &mut egui::Ui, plot_id: PlotId) {
        let (view_start, view_end) = (self.timeline.view_start, self.timeline.view_end);
        let cursor = self.timeline.cursor;

        let Some(plot) = self.plots.list.iter_mut().find(|p| p.id == plot_id) else {
            ui.colored_label(Color32::RED, "plot no longer exists");
            return;
        };

        // Series from different sources need saying which source they came
        // from; within one source that would just be noise on every line.
        let multi_source = plot.entries.windows(2).any(|w| w[0].source != w[1].source);
        let mut prepared: Vec<PreparedSeries> = Vec::with_capacity(plot.entries.len());
        for entry in &plot.entries {
            let Some(source) = self.project.source(entry.source) else {
                continue;
            };
            let SourceKind::Log(log) = &source.kind else {
                continue;
            };
            let Some(series) = log.series.iter().find(|s| s.name == entry.series) else {
                continue;
            };
            let offset = source.offset_seconds;
            prepared.push(PreparedSeries {
                label: if multi_source {
                    format!("{} [{}]", entry.series, source.name)
                } else {
                    entry.series.clone()
                },
                color: entry.color,
                unit: series.unit.clone(),
                points: series.slice_for_range(view_start, view_end, offset, TARGET_PLOT_POINTS),
                bounds: series.value_bounds_in_range(view_start, view_end, offset),
                at_cursor: series.value_at(cursor, offset),
            });
        }

        let title = plot.title();
        let normalize = plot.normalize;

        // --- header: title, readout at the playhead, per-plot settings ---
        ui.horizontal(|ui| {
            ui.strong(&title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("⚙", |ui| {
                    ui.set_min_width(260.0);
                    ui.label("Title");
                    let hint = auto_title(&plot.entries);
                    ui.add(
                        egui::TextEdit::singleline(&mut plot.custom_title)
                            .hint_text(hint)
                            .desired_width(240.0),
                    );
                    ui.checkbox(&mut plot.normalize, "Normalize each series to 0..1")
                        .on_hover_text("Compare shapes when the series don't share a scale");
                    ui.separator();
                    ui.label("Series");
                    let mut drop = None;
                    for entry in &plot.entries {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, entry.color);
                            ui.label(&entry.series);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("✖").on_hover_text("Remove from this plot").clicked() {
                                    drop = Some(entry.series.clone());
                                }
                            });
                        });
                    }
                    if let Some(series) = drop {
                        plot.entries.retain(|e| e.series != series);
                        if plot.entries.is_empty() {
                            self.closed.push(Pane::Plot(plot_id));
                        }
                    }
                });
            });
        });

        ui.horizontal_wrapped(|ui| {
            for series in &prepared {
                let Some(v) = series.at_cursor else { continue };
                let unit = series.unit.as_deref().unwrap_or("");
                ui.colored_label(series.color, format!("{}: {v:.4} {unit}", short_label(&series.label)));
            }
        });

        // --- y axis: the union of every series' visible range, or 0..1 when
        // each series is rescaled onto its own range ---
        let (y_lo, y_hi) = if normalize {
            (-0.05, 1.05)
        } else {
            let bounds = prepared.iter().filter_map(|s| s.bounds).fold(None, |acc, (lo, hi)| {
                Some(match acc {
                    Some((a, b)) => (f64::min(a, lo), f64::max(b, hi)),
                    None => (lo, hi),
                })
            });
            match bounds {
                Some((lo, hi)) if hi > lo => {
                    let pad = (hi - lo) * 0.1;
                    (lo - pad, hi + pad)
                }
                Some((v, _)) => (v - 1.0, v + 1.0),
                None => (0.0, 1.0),
            }
        };

        let units: Vec<&str> = prepared
            .iter()
            .filter_map(|s| s.unit.as_deref())
            .fold(Vec::new(), |mut acc, u| {
                if !acc.contains(&u) {
                    acc.push(u);
                }
                acc
            });
        let y_label = if normalize {
            "normalized".to_string()
        } else {
            units.join(" / ")
        };

        let hover_units = units.clone();
        let plot_widget = Plot::new(("plot", plot_id))
            .height(ui.available_height().max(80.0))
            .allow_boxed_zoom(false)
            .legend(Legend::default().position(Corner::LeftTop))
            .y_axis_label(y_label)
            .x_axis_formatter(|mark, _| crate::timeline::format_axis_time(mark.value, mark.step_size))
            .x_grid_spacer(egui_plot::uniform_grid_spacer(crate::timeline::time_grid_steps))
            .label_formatter(move |pos| {
                let p = match pos {
                    egui_plot::HoverPosition::NearDataPoint { position, .. } => *position,
                    egui_plot::HoverPosition::Elsewhere { position } => *position,
                };
                let unit = if hover_units.len() == 1 { hover_units[0] } else { "" };
                Some(format!("{:.4} {unit}\n{}", p.y, crate::timeline::format_utc(p.x)))
            });

        let mut clicked_time = None;
        let response = plot_widget.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max([view_start, y_lo], [view_end, y_hi]));
            for series in &prepared {
                let points: PlotPoints = match (normalize, series.bounds) {
                    (true, Some((lo, hi))) if hi > lo => series
                        .points
                        .iter()
                        .map(|p| [p[0], (p[1] - lo) / (hi - lo)])
                        .collect::<Vec<_>>()
                        .into(),
                    // A flat series has no range to normalize against; park
                    // it in the middle rather than dividing by zero.
                    (true, _) => series.points.iter().map(|p| [p[0], 0.5]).collect::<Vec<_>>().into(),
                    (false, _) => series.points.clone().into(),
                };
                plot_ui.line(Line::new(series.label.clone(), points).color(series.color));
            }
            plot_ui.vline(VLine::new("cursor", cursor).color(CURSOR_COLOR));
            if plot_ui.response().clicked()
                && let Some(coord) = plot_ui.pointer_coordinate() {
                    clicked_time = Some(coord.x);
                }
        });

        self.apply_view_change(&response.transform, clicked_time);
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
        let local_t = source.to_local_time(self.timeline.cursor);
        // A short clip inside a long experiment is normal, and holding the
        // last frame on screen while the graphs run on reads as if the video
        // were still playing.
        let in_range = local_t >= 0.0 && local_t <= video.duration;
        let path = source.path.clone();
        let (fps, width, height) = (video.fps, video.width, video.height);
        let start_note = (source.offset_seconds == 0.0 && video.start_utc_source.is_guess()).then(|| {
            format!(
                "⚠ start time guessed from {} ({}) -- adjust the offset in the source list if needed",
                video.start_utc_source.describe(),
                crate::timeline::format_utc(video.start_utc)
            )
        });

        let worker = self
            .video_workers
            .entry(source_id)
            .or_insert_with(|| VideoWorker::new(path, fps, width, height));
        if in_range {
            worker.request_frame(local_t);
        }
        worker.poll(ui.ctx());

        if let Some(note) = start_note {
            ui.small(note);
        }

        let avail = ui.available_size();
        let aspect = width.max(1) as f32 / height.max(1) as f32;
        let mut size = avail;
        if size.x / size.y > aspect {
            size.x = size.y * aspect;
        } else {
            size.y = size.x / aspect;
        }

        if !in_range {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
            let label = if local_t < 0.0 { "before this recording" } else { "after this recording" };
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(13.0),
                Color32::from_gray(90),
            );
        } else if let Some(texture) = &worker.texture {
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
        if source.offset_seconds == 0.0 && audio.start_utc_source.is_guess() {
            ui.small(format!(
                "⚠ start time guessed from {} ({}) -- adjust the offset in the source list if needed",
                audio.start_utc_source.describe(),
                crate::timeline::format_utc(audio.start_utc)
            ));
        }
        // Clone out of the borrow (a few tens of KB) so we're free to call
        // `&mut self` methods like `ensure_audio_player` below.
        let peaks = audio.waveform_peaks.clone();

        self.ensure_audio_player(source_id, &path);
        if let Some(Some(player)) = self.audio_players.get_mut(&source_id) {
            player.sync(local_t, self.timeline.playing && (0.0..=duration).contains(&local_t));
        }

        let (view_start, view_end) = (self.timeline.view_start, self.timeline.view_end);
        let n = peaks.len().max(1);
        let dt = duration / n as f64;

        let lo_idx = (((view_start - master_base) / dt).floor().max(0.0) as usize).min(n);
        let hi_idx = (((view_end - master_base) / dt).ceil().max(0.0) as usize + 1).min(n);

        let mut env = Vec::with_capacity((hi_idx - lo_idx) * 2);
        for (i, peak) in peaks[lo_idx..hi_idx].iter().enumerate() {
            let t = master_base + (lo_idx + i) as f64 * dt;
            env.push([t, peak[0] as f64]);
            env.push([t, peak[1] as f64]);
        }

        let cursor = self.timeline.cursor;
        let mut clicked_time = None;
        let plot = Plot::new(("audio", source_id))
            .height(ui.available_height().max(60.0))
            .allow_boxed_zoom(false)
            .show_y(false)
            .x_axis_formatter(|mark, _| crate::timeline::format_axis_time(mark.value, mark.step_size))
            .x_grid_spacer(egui_plot::uniform_grid_spacer(crate::timeline::time_grid_steps));
        let response = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max([view_start, -1.05], [view_end, 1.05]));
            let pts: PlotPoints = env.clone().into();
            plot_ui.line(Line::new("waveform", pts).color(color));
            plot_ui.vline(VLine::new("cursor", cursor).color(CURSOR_COLOR));
            if plot_ui.response().clicked()
                && let Some(coord) = plot_ui.pointer_coordinate() {
                    clicked_time = Some(coord.x);
                }
        });
        self.apply_view_change(&response.transform, clicked_time);

        if matches!(self.audio_players.get(&source_id), Some(None)) {
            ui.colored_label(Color32::YELLOW, "audio playback unavailable (no output device) -- waveform still shown");
        }
    }

    /// Panning or zooming any plot moves every other pane with it, and a
    /// click on one moves the playhead.
    fn apply_view_change(&mut self, transform: &egui_plot::PlotTransform, clicked_time: Option<f64>) {
        let bounds = transform.bounds();
        let (nx0, nx1) = (bounds.min()[0], bounds.max()[0]);
        if (nx0 - self.timeline.view_start).abs() > 1e-6 || (nx1 - self.timeline.view_end).abs() > 1e-6 {
            self.timeline.view_start = nx0;
            self.timeline.view_end = nx1;
        }
        if let Some(t) = clicked_time {
            self.timeline.seek(t, self.project.time_bounds());
            self.timeline.playing = false;
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

/// Legend labels repeat the message prefix on every line; the playhead
/// readout is tighter with just the field.
fn short_label(label: &str) -> &str {
    label.rsplit('.').next().unwrap_or(label)
}

impl<'a> egui_tiles::Behavior<Pane> for TreeBehavior<'a> {
    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        match pane {
            Pane::Plot(id) => match self.plots.get(*id) {
                Some(plot) => plot.title().into(),
                None => "plot".into(),
            },
            Pane::Video(id) => format!("🎬 {}", self.source_name(*id)).into(),
            Pane::Audio(id) => format!("🔊 {}", self.source_name(*id)).into(),
        }
    }

    fn is_tab_closable(&self, _tiles: &egui_tiles::Tiles<Pane>, _tile_id: egui_tiles::TileId) -> bool {
        true
    }

    fn on_tab_close(&mut self, tiles: &mut egui_tiles::Tiles<Pane>, tile_id: egui_tiles::TileId) -> bool {
        // The tree drops the tile itself; `App` still has to forget the pane
        // (and the plot behind it) or the sidebar checkbox stays ticked.
        if let Some(egui_tiles::Tile::Pane(pane)) = tiles.get(tile_id) {
            self.closed.push(pane.clone());
        }
        true
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile_id: egui_tiles::TileId, pane: &mut Pane) -> egui_tiles::UiResponse {
        match pane.clone() {
            Pane::Plot(id) => self.plot_pane(ui, id),
            Pane::Video(id) => self.video_pane(ui, id),
            Pane::Audio(id) => self.audio_pane(ui, id),
        }
        egui_tiles::UiResponse::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(names: &[&str]) -> Vec<PlotEntry> {
        names
            .iter()
            .map(|n| PlotEntry {
                source: 0,
                series: n.to_string(),
                color: Color32::WHITE,
            })
            .collect()
    }

    #[test]
    fn titles_factor_out_what_the_series_share() {
        assert_eq!(
            auto_title(&entries(&["PRESSURE_VESSEL[1].pressure1", "PRESSURE_VESSEL[1].temperature1"])),
            "PRESSURE_VESSEL[1]: pressure1 / temperature1"
        );
        assert_eq!(auto_title(&entries(&["VALVE[MAIN].state"])), "VALVE[MAIN].state");
    }

    #[test]
    fn titles_fall_back_to_the_full_names_when_nothing_is_shared() {
        assert_eq!(
            auto_title(&entries(&["VALVE[MAIN].state", "ATTITUDE.roll"])),
            "VALVE[MAIN].state / ATTITUDE.roll"
        );
    }

    #[test]
    fn removing_the_last_series_reports_the_plot_as_empty() {
        let mut plots = Plots::default();
        let id = plots.create(0, "a".to_string());
        plots.add(id, 0, "b".to_string());
        assert!(plots.remove_series(0, "a").is_empty());
        assert_eq!(plots.remove_series(0, "b"), vec![id]);
    }

    #[test]
    fn a_series_is_only_added_once_per_plot() {
        let mut plots = Plots::default();
        let id = plots.create(0, "a".to_string());
        plots.add(id, 0, "a".to_string());
        assert_eq!(plots.get(id).unwrap().entries.len(), 1);
        // ... but the same series in a second plot is fine.
        let other = plots.create(0, "a".to_string());
        assert!(plots.get(other).unwrap().contains(0, "a"));
    }
}
