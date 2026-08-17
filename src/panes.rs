use std::collections::HashMap;

use egui::{Color32, Vec2b, Widget as _};
use egui_plot::{AxisHints, Corner, HPlacement, Legend, Line, Plot, PlotBounds, PlotPoints, VLine};

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

/// Which of a graph's two value axes a series is drawn against.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PlotAxis {
    #[default]
    Left,
    Right,
}

impl PlotAxis {
    fn label(self) -> &'static str {
        match self {
            Self::Left => "L",
            Self::Right => "R",
        }
    }

    fn flipped(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// One series drawn in a graph.
#[derive(Clone)]
pub struct PlotEntry {
    pub source: SourceId,
    pub series: String,
    pub color: Color32,
    pub axis: PlotAxis,
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
    /// where a shared axis would flatten one into a straight line -- and
    /// unlike [`PlotAxis::Right`], it scales any number of series at once at
    /// the cost of every axis number becoming meaningless.
    pub normalize: bool,
    /// Value range fixed by a box zoom, in left-axis units. `None` means the
    /// range follows whatever is visible.
    pub y_manual: Option<(f64, f64)>,
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

    pub fn get_mut(&mut self, id: PlotId) -> Option<&mut PlotSpec> {
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
            y_manual: None,
            color_cursor: 0,
        });
        self.add(id, source, series, PlotAxis::Left);
        id
    }

    pub fn add(&mut self, id: PlotId, source: SourceId, series: String, axis: PlotAxis) {
        let Some(plot) = self.get_mut(id) else {
            return;
        };
        if plot.contains(source, &series) {
            return;
        }
        let color = color_for_index(plot.color_cursor);
        plot.color_cursor += 1;
        plot.entries.push(PlotEntry {
            source,
            series,
            color,
            axis,
        });
        // A range picked for the old contents would just clip the new series.
        plot.y_manual = None;
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

    /// Puts every graph's value axis back on autoscale, the other half of
    /// "reset the zoom".
    pub fn clear_manual_ranges(&mut self) {
        for plot in &mut self.list {
            plot.y_manual = None;
        }
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
    axis: PlotAxis,
    points: Vec<[f64; 2]>,
    /// Value range over the visible window, before any normalization.
    bounds: Option<(f64, f64)>,
    at_cursor: Option<f64>,
}

/// The affine map from a right-axis series' own values into the plot's
/// (left-axis) coordinates.
///
/// egui_plot draws one coordinate system, so a second axis is a matter of
/// squeezing the right-hand series into the left-hand range and relabelling
/// the ticks on the way out -- which is all a twin axis ever is. The map is
/// built from the *auto* ranges of both sides and then held fixed, so zooming
/// moves both sets of curves together instead of re-fitting one of them under
/// the user's gesture.
#[derive(Clone, Copy)]
struct AxisMap {
    from: (f64, f64),
    to: (f64, f64),
}

impl AxisMap {
    fn plot_y(&self, value: f64) -> f64 {
        let span = self.from.1 - self.from.0;
        if span.abs() < f64::EPSILON {
            return (self.to.0 + self.to.1) * 0.5;
        }
        self.to.0 + (value - self.from.0) / span * (self.to.1 - self.to.0)
    }

    fn axis_value(&self, y: f64) -> f64 {
        let span = self.to.1 - self.to.0;
        if span.abs() < f64::EPSILON {
            return self.from.0;
        }
        self.from.0 + (y - self.to.0) / span * (self.from.1 - self.from.0)
    }

    /// What one plot-coordinate step is worth on the right axis, for picking
    /// how many decimals its tick labels need.
    fn step_scale(&self) -> f64 {
        let span = self.to.1 - self.to.0;
        if span.abs() < f64::EPSILON {
            1.0
        } else {
            (self.from.1 - self.from.0) / span
        }
    }
}

/// Value range for an axis: the union of what its series cover in the visible
/// window, with a little headroom, or a usable default when it has no data.
fn axis_range(series: impl Iterator<Item = (f64, f64)>) -> Option<(f64, f64)> {
    let bounds = series.fold(None, |acc, (lo, hi)| {
        Some(match acc {
            Some((a, b)) => (f64::min(a, lo), f64::max(b, hi)),
            None => (lo, hi),
        })
    });
    Some(match bounds? {
        (lo, hi) if hi > lo => {
            let pad = (hi - lo) * 0.1;
            (lo - pad, hi + pad)
        }
        // A flat line still needs an axis to sit in the middle of.
        (v, _) => (v - 1.0, v + 1.0),
    })
}

/// Tick label for a value axis whose numbers we relabel ourselves, matching
/// the precision to the spacing between ticks.
fn format_axis_value(value: f64, step: f64) -> String {
    let decimals = if step > 0.0 {
        (-step.log10().floor()).clamp(0.0, 6.0) as usize
    } else {
        3
    };
    format!("{value:.decimals$}")
}

/// The distinct units among a set of series, in the order they appear.
fn unit_label<'s>(series: impl Iterator<Item = &'s PreparedSeries>) -> String {
    let mut units: Vec<&str> = Vec::new();
    for unit in series.filter_map(|s| s.unit.as_deref()) {
        if !units.contains(&unit) {
            units.push(unit);
        }
    }
    units.join(" / ")
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
            let mut label = if multi_source {
                format!("{} [{}]", entry.series, source.name)
            } else {
                entry.series.clone()
            };
            // Which axis a line is read against has to be visible on the line
            // itself; the numbers on the two sides are otherwise unattributable.
            if entry.axis == PlotAxis::Right {
                label.push_str(" (R)");
            }
            prepared.push(PreparedSeries {
                label,
                color: entry.color,
                unit: series.unit.clone(),
                axis: entry.axis,
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
                    if plot.y_manual.is_some() && ui.button("⟲ Auto value range").clicked() {
                        plot.y_manual = None;
                    }
                    ui.separator();
                    ui.label("Series");
                    let mut drop = None;
                    let mut flip = None;
                    for (i, entry) in plot.entries.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, entry.color);
                            ui.label(&entry.series);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("✖").on_hover_text("Remove from this plot").clicked() {
                                    drop = Some(i);
                                }
                                // The whole point of the right axis is a
                                // series whose scale swamps the others, so
                                // the switch belongs next to that series.
                                if ui
                                    .small_button(entry.axis.label())
                                    .on_hover_text("Draw this against the left / right value axis")
                                    .clicked()
                                {
                                    flip = Some(i);
                                }
                            });
                        });
                    }
                    if let Some(i) = flip {
                        plot.entries[i].axis = plot.entries[i].axis.flipped();
                        plot.y_manual = None;
                    }
                    if let Some(i) = drop {
                        plot.entries.remove(i);
                        plot.y_manual = None;
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

        // --- value axes ---
        //
        // Each axis covers the union of its own series' visible range. A
        // series on the right is then mapped into the left axis' range for
        // drawing, so a thrust curve in kN and a pressure in bar can share a
        // graph without either being flattened into a straight line.
        let has_right = prepared.iter().any(|s| s.axis == PlotAxis::Right) && !normalize;
        let range_of = |axis: PlotAxis| {
            axis_range(
                prepared
                    .iter()
                    .filter(|s| s.axis == axis)
                    .filter_map(|s| s.bounds),
            )
        };
        let left_range = range_of(PlotAxis::Left);
        let right_range = range_of(PlotAxis::Right);

        // With nothing on the left, right-axis series keep their own numbers
        // rather than being mapped into a range that isn't there.
        let auto_y = if normalize {
            (-0.05, 1.05)
        } else {
            left_range.or(right_range).unwrap_or((0.0, 1.0))
        };
        let right_map = AxisMap {
            from: right_range.unwrap_or(auto_y),
            to: auto_y,
        };
        let (y_lo, y_hi) = match plot.y_manual {
            Some(range) if !normalize => range,
            _ => auto_y,
        };

        let left_label = if normalize {
            "normalized".to_string()
        } else {
            unit_label(prepared.iter().filter(|s| s.axis == PlotAxis::Left))
        };
        let right_label = unit_label(prepared.iter().filter(|s| s.axis == PlotAxis::Right));

        // The hover readout has to undo the mapping again, so it needs to
        // know which line it is over -- which is what `plot_name` is for.
        let hover: Vec<(String, PlotAxis, Option<String>)> = prepared
            .iter()
            .map(|s| (s.label.clone(), s.axis, s.unit.clone()))
            .collect();
        let box_zoom = self.timeline.box_zoom;

        let mut plot_widget = Plot::new(("plot", plot_id))
            .height(ui.available_height().max(80.0))
            // Scrolling and dragging move the shared time window; the value
            // axis stays fitted to the data unless a box zoom pins it.
            .allow_zoom(Vec2b::new(true, false))
            .allow_drag(Vec2b::new(!box_zoom, false))
            .allow_boxed_zoom(true)
            .boxed_zoom_pointer_button(if box_zoom {
                egui::PointerButton::Primary
            } else {
                egui::PointerButton::Secondary
            })
            .legend(Legend::default().position(Corner::LeftTop))
            .x_axis_formatter(|mark, _| crate::timeline::format_axis_time(mark.value, mark.step_size))
            .x_grid_spacer(egui_plot::uniform_grid_spacer(crate::timeline::time_grid_steps))
            .label_formatter(move |pos| {
                let (name, p) = match pos {
                    egui_plot::HoverPosition::NearDataPoint { plot_name, position, .. } => (Some(*plot_name), *position),
                    egui_plot::HoverPosition::Elsewhere { position } => (None, *position),
                };
                let hovered = name.and_then(|n| hover.iter().find(|(label, _, _)| label == n));
                let (value, unit) = match hovered {
                    Some((_, PlotAxis::Right, unit)) => (right_map.axis_value(p.y), unit.as_deref().unwrap_or("")),
                    Some((_, PlotAxis::Left, unit)) => (p.y, unit.as_deref().unwrap_or("")),
                    None => (p.y, ""),
                };
                Some(format!("{value:.4} {unit}\n{}", crate::timeline::format_utc(p.x)))
            });

        if has_right {
            let step_scale = right_map.step_scale();
            plot_widget = plot_widget.custom_y_axes(vec![
                AxisHints::new_y().label(left_label),
                AxisHints::new_y()
                    .label(right_label)
                    .placement(HPlacement::Right)
                    .formatter(move |mark, _| {
                        format_axis_value(right_map.axis_value(mark.value), mark.step_size * step_scale)
                    }),
            ]);
        } else {
            plot_widget = plot_widget.y_axis_label(left_label);
        }

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
                    (false, _) if series.axis == PlotAxis::Right => series
                        .points
                        .iter()
                        .map(|p| [p[0], right_map.plot_y(p[1])])
                        .collect::<Vec<_>>()
                        .into(),
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

        let zoomed_y = self.apply_view_change(&response.transform, (y_lo, y_hi), clicked_time);
        if let Some(range) = zoomed_y
            && !normalize
            && let Some(plot) = self.plots.get_mut(plot_id) {
                plot.y_manual = Some(range);
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
            // The overwhelmingly common one, and not obvious from ffmpeg's
            // wording: distributions ship builds with whole codecs removed.
            if err.contains("no decoder found") {
                ui.small("This ffmpeg build has no decoder for that codec -- see the README for a build that does.");
            }
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
        let box_zoom = self.timeline.box_zoom;
        let mut clicked_time = None;
        // A waveform's amplitude axis has no range worth zooming into, so a
        // box here only ever selects a stretch of time.
        let (y_lo, y_hi) = (-1.05, 1.05);
        let plot = Plot::new(("audio", source_id))
            .height(ui.available_height().max(60.0))
            .allow_zoom(Vec2b::new(true, false))
            .allow_drag(Vec2b::new(!box_zoom, false))
            .allow_boxed_zoom(true)
            .boxed_zoom_pointer_button(if box_zoom {
                egui::PointerButton::Primary
            } else {
                egui::PointerButton::Secondary
            })
            .show_y(false)
            .x_axis_formatter(|mark, _| crate::timeline::format_axis_time(mark.value, mark.step_size))
            .x_grid_spacer(egui_plot::uniform_grid_spacer(crate::timeline::time_grid_steps));
        let response = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max([view_start, y_lo], [view_end, y_hi]));
            let pts: PlotPoints = env.clone().into();
            plot_ui.line(Line::new("waveform", pts).color(color));
            plot_ui.vline(VLine::new("cursor", cursor).color(CURSOR_COLOR));
            if plot_ui.response().clicked()
                && let Some(coord) = plot_ui.pointer_coordinate() {
                    clicked_time = Some(coord.x);
                }
        });
        self.apply_view_change(&response.transform, (y_lo, y_hi), clicked_time);

        if matches!(self.audio_players.get(&source_id), Some(None)) {
            ui.colored_label(Color32::YELLOW, "audio playback unavailable (no output device) -- waveform still shown");
        }
    }

    /// Panning or zooming any plot moves every other pane with it, and a
    /// click on one moves the playhead.
    ///
    /// Returns the value range the user's gesture ended up with, when that is
    /// not the `expected_y` the pane asked for -- a box zoom is the only thing
    /// that can do that, and the pane holds on to it as its manual range.
    fn apply_view_change(
        &mut self,
        transform: &egui_plot::PlotTransform,
        expected_y: (f64, f64),
        clicked_time: Option<f64>,
    ) -> Option<(f64, f64)> {
        let bounds = transform.bounds();
        let (nx0, nx1) = (bounds.min()[0], bounds.max()[0]);
        if (nx0 - self.timeline.view_start).abs() > 1e-6 || (nx1 - self.timeline.view_end).abs() > 1e-6 {
            self.timeline.set_view(nx0, nx1);
        }
        if let Some(t) = clicked_time {
            self.timeline.seek(t, self.project.time_bounds());
            self.timeline.playing = false;
        }

        let (ny0, ny1) = (bounds.min()[1], bounds.max()[1]);
        let tolerance = (expected_y.1 - expected_y.0).abs() * 1e-6;
        let moved = (ny0 - expected_y.0).abs() > tolerance || (ny1 - expected_y.1).abs() > tolerance;
        (moved && ny1 > ny0).then_some((ny0, ny1))
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
                axis: PlotAxis::Left,
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
        plots.add(id, 0, "b".to_string(), PlotAxis::Left);
        assert!(plots.remove_series(0, "a").is_empty());
        assert_eq!(plots.remove_series(0, "b"), vec![id]);
    }

    #[test]
    fn a_series_is_only_added_once_per_plot() {
        let mut plots = Plots::default();
        let id = plots.create(0, "a".to_string());
        plots.add(id, 0, "a".to_string(), PlotAxis::Left);
        assert_eq!(plots.get(id).unwrap().entries.len(), 1);
        // ... but the same series in a second plot is fine.
        let other = plots.create(0, "a".to_string());
        assert!(plots.get(other).unwrap().contains(0, "a"));
    }

    #[test]
    fn dropping_a_source_leaves_the_other_sources_series_alone() {
        let mut plots = Plots::default();
        let id = plots.create(0, "a".to_string());
        plots.add(id, 1, "a".to_string(), PlotAxis::Right);
        assert!(plots.remove_source(0).is_empty(), "the plot still has source 1's series");
        assert_eq!(plots.get(id).unwrap().entries.len(), 1);
        assert_eq!(plots.remove_source(1), vec![id]);
    }

    #[test]
    fn the_right_axis_maps_its_range_onto_the_left_one() {
        // A thrust curve of 0..8000 N drawn against a 0..60 bar pressure axis.
        let map = AxisMap {
            from: (0.0, 8000.0),
            to: (0.0, 60.0),
        };
        assert_eq!(map.plot_y(0.0), 0.0);
        assert_eq!(map.plot_y(8000.0), 60.0);
        assert_eq!(map.plot_y(4000.0), 30.0);
        // Tick labels on the right axis undo it exactly.
        assert_eq!(map.axis_value(30.0), 4000.0);
        assert!((map.axis_value(map.plot_y(1234.0)) - 1234.0).abs() < 1e-9);
    }

    #[test]
    fn a_flat_right_axis_series_does_not_divide_by_zero() {
        let map = AxisMap {
            from: (5.0, 5.0),
            to: (0.0, 10.0),
        };
        assert!(map.plot_y(5.0).is_finite());
        assert!(map.axis_value(0.0).is_finite());
        assert!(map.step_scale().is_finite());
    }

    #[test]
    fn an_axis_range_has_headroom_and_survives_a_flat_line() {
        assert_eq!(axis_range([(0.0, 10.0)].into_iter()), Some((-1.0, 11.0)));
        assert_eq!(axis_range([(0.0, 10.0), (-5.0, 1.0)].into_iter()), Some((-6.5, 11.5)));
        assert_eq!(axis_range([(3.0, 3.0)].into_iter()), Some((2.0, 4.0)));
        assert_eq!(axis_range(std::iter::empty()), None);
    }

    #[test]
    fn axis_tick_labels_follow_the_tick_spacing() {
        assert_eq!(format_axis_value(1234.5678, 100.0), "1235");
        assert_eq!(format_axis_value(1.2345678, 0.05), "1.23");
        assert_eq!(format_axis_value(1.2345678, 0.001), "1.235");
    }
}
