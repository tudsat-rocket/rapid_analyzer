use egui::{Color32, Rect, Sense, Stroke, Vec2};

/// Narrowest visible window, in seconds. Zooming past a millisecond says
/// nothing about data logged at a few hundred hertz, and a window that
/// reaches zero width can't be zoomed back out.
const MIN_VIEW_SPAN: f64 = 0.001;

/// Playback speeds offered in the toolbar, and stepped through with ↑/↓.
pub const SPEEDS: &[f32] = &[0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0];

/// Master synchronization state shared by every panel: log graphs, video and
/// audio all read from this to know "when" they are.
pub struct Timeline {
    /// Currently visible time window in the linked graphs (master UTC seconds).
    /// Panning/zooming any graph updates this, and every other graph/video/
    /// audio panel picks it up on the next frame.
    pub view_start: f64,
    pub view_end: f64,
    /// The playhead: what instant videos/audio show and graphs mark with a line.
    pub cursor: f64,
    pub playing: bool,
    pub speed: f32,
    /// Drag-to-zoom mode: while on, dragging inside any graph selects a
    /// rectangle to zoom into instead of panning. It lives here rather than in
    /// the app because every pane has to agree on it, and the panes already
    /// share this struct.
    pub box_zoom: bool,
}

impl Timeline {
    pub fn new(bounds: (f64, f64)) -> Self {
        Self {
            view_start: bounds.0,
            view_end: bounds.1,
            cursor: bounds.0,
            playing: false,
            speed: 1.0,
            box_zoom: false,
        }
    }

    pub fn tick(&mut self, dt_seconds: f64, bounds: Option<(f64, f64)>) {
        if !self.playing {
            return;
        }
        self.cursor += dt_seconds * self.speed as f64;
        if let Some((_, hi)) = bounds
            && self.cursor >= hi {
                self.cursor = hi;
                self.playing = false;
            }
    }

    pub fn seek(&mut self, t: f64, bounds: Option<(f64, f64)>) {
        self.cursor = if let Some((lo, hi)) = bounds { t.clamp(lo, hi) } else { t };
    }

    pub fn step(&mut self, delta_seconds: f64, bounds: Option<(f64, f64)>) {
        let t = self.cursor + delta_seconds;
        self.seek(t, bounds);
    }

    /// Frames the timeline on `bounds` -- the first import, or the last one
    /// being removed -- while leaving playback settings (speed, box zoom
    /// mode) as the user set them.
    pub fn reset_to(&mut self, bounds: (f64, f64)) {
        self.set_view(bounds.0, bounds.1);
        self.cursor = bounds.0;
        self.playing = false;
    }

    pub fn view_span(&self) -> f64 {
        self.view_end - self.view_start
    }

    /// Sets the visible window, refusing a degenerate or inverted one.
    pub fn set_view(&mut self, start: f64, end: f64) {
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
        if end - start < MIN_VIEW_SPAN {
            let center = (start + end) * 0.5;
            self.view_start = center - MIN_VIEW_SPAN * 0.5;
            self.view_end = center + MIN_VIEW_SPAN * 0.5;
        } else {
            self.view_start = start;
            self.view_end = end;
        }
    }

    /// Scales the visible window about `center`. `factor` below 1 zooms in.
    pub fn zoom_view(&mut self, factor: f64, center: f64) {
        let start = center + (self.view_start - center) * factor;
        let end = center + (self.view_end - center) * factor;
        self.set_view(start, end);
    }

    /// Back to showing everything that is loaded.
    pub fn reset_view(&mut self, bounds: Option<(f64, f64)>) {
        if let Some((lo, hi)) = bounds {
            self.set_view(lo, hi);
        }
    }

    /// Adopts the x range a plot pane came back with -- a pan, a scroll zoom
    /// or a box selection -- and a click on it as a seek. Every pane on the
    /// time axis goes through this, which is what keeps them in step.
    pub fn follow_plot(&mut self, x0: f64, x1: f64, clicked: Option<f64>, bounds: Option<(f64, f64)>) {
        if (x0 - self.view_start).abs() > 1e-6 || (x1 - self.view_end).abs() > 1e-6 {
            self.set_view(x0, x1);
        }
        if let Some(t) = clicked {
            self.seek(t, bounds);
            self.playing = false;
        }
    }

    /// Pulls the window and the playhead back inside `bounds`, for when a
    /// source is removed and the timeline shrinks out from under them.
    pub fn clamp_to(&mut self, bounds: Option<(f64, f64)>) {
        let Some((lo, hi)) = bounds else {
            return;
        };
        self.cursor = self.cursor.clamp(lo, hi);
        // A window that no longer overlaps the remaining data would leave
        // every graph blank with no hint of where the data went.
        if self.view_end <= lo || self.view_start >= hi || self.view_span() > (hi - lo) {
            self.set_view(lo, hi);
        } else {
            self.set_view(self.view_start.max(lo), self.view_end.min(hi));
        }
    }
}

pub fn format_utc(t: f64) -> String {
    match chrono::DateTime::from_timestamp(t.floor() as i64, 0) {
        Some(dt) => format!("{}.{:03}Z", dt.format("%Y-%m-%d %H:%M:%S"), ((t.fract()) * 1000.0) as u32),
        None => format!("{t:.3}"),
    }
}

/// Grid steps a clock actually uses. egui_plot's default spacer subdivides by
/// powers of ten, which on a time axis puts lines at 100- and 1000-second
/// intervals -- numbers nobody reads a clock in, and coarse enough that a
/// ten-minute window ends up with a single labelled tick.
const TIME_STEPS: &[f64] = &[
    0.001, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, // sub-second
    1.0, 2.0, 5.0, 10.0, 15.0, 30.0, // seconds
    60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, // minutes
    3600.0, 7200.0, 10800.0, 21600.0, 43200.0, 86400.0, // hours and up
];

/// Grid line spacing for a time axis: the three step sizes egui_plot draws at
/// different thicknesses, chosen off the clock ladder above.
pub fn time_grid_steps(input: egui_plot::GridInput) -> [f64; 3] {
    let last = TIME_STEPS.len() - 1;
    let finest = TIME_STEPS
        .iter()
        .position(|s| *s >= input.base_step_size)
        .unwrap_or(last);
    [
        TIME_STEPS[finest],
        TIME_STEPS[(finest + 2).min(last)],
        TIME_STEPS[(finest + 4).min(last)],
    ]
}

/// Tick label for a plot's time axis. The axis carries absolute UTC seconds,
/// which as a raw number (1.786e9) says nothing; what a reader wants is the
/// clock time, at whatever precision the current zoom level resolves.
pub fn format_axis_time(t: f64, step_size: f64) -> String {
    let Some(dt) = chrono::DateTime::from_timestamp(t.floor() as i64, 0) else {
        return format!("{t:.3}");
    };
    if step_size >= 3600.0 {
        dt.format("%H:%M").to_string()
    } else if step_size >= 1.0 {
        dt.format("%H:%M:%S").to_string()
    } else {
        let millis = (t.rem_euclid(1.0) * 1000.0).round() as u32;
        format!("{}.{:03}", dt.format("%H:%M:%S"), millis)
    }
}

pub fn format_duration(seconds: f64) -> String {
    let s = seconds.max(0.0);
    let h = (s / 3600.0) as u64;
    let m = ((s % 3600.0) / 60.0) as u64;
    let sec = s % 60.0;
    if h > 0 {
        format!("{h:02}:{m:02}:{sec:06.3}")
    } else {
        format!("{m:02}:{sec:06.3}")
    }
}

/// Top playback bar: play/pause/step controls plus a full-range scrubber the
/// user can click or drag to jump the playhead anywhere in the experiment.
/// Returns `true` if the user changed the cursor or the view range.
pub fn show(ui: &mut egui::Ui, timeline: &mut Timeline, project_bounds: Option<(f64, f64)>) -> bool {
    let mut changed = false;

    ui.horizontal(|ui| {
        let play_label = if timeline.playing { "⏸" } else { "▶" };
        if ui.button("⏮").on_hover_text("Jump to start  (Home)").clicked()
            && let Some((lo, _)) = project_bounds {
                timeline.seek(lo, project_bounds);
                changed = true;
            }
        if ui.button("◀").on_hover_text("Step back 1s  (←, Shift for 10s, Alt for 0.1s)").clicked() {
            timeline.step(-1.0, project_bounds);
            changed = true;
        }
        if ui.button(play_label).on_hover_text("Play/Pause  (Space)").clicked() {
            timeline.playing = !timeline.playing;
        }
        if ui.button("▶").on_hover_text("Step forward 1s  (→, Shift for 10s, Alt for 0.1s)").clicked() {
            timeline.step(1.0, project_bounds);
            changed = true;
        }
        if ui.button("⏭").on_hover_text("Jump to end  (End)").clicked()
            && let Some((_, hi)) = project_bounds {
                timeline.seek(hi, project_bounds);
                changed = true;
            }

        ui.separator();
        ui.toggle_value(&mut timeline.box_zoom, "▣")
            .on_hover_text("Box zoom  (B)\nDrag a rectangle in any graph to zoom into it.\nWhen off, the same works with the right mouse button and dragging pans.");
        if ui.button("⟲").on_hover_text("Zoom to fit everything  (R)").clicked() {
            timeline.reset_view(project_bounds);
            changed = true;
        }

        ui.separator();
        ui.label("Speed");
        egui::ComboBox::from_id_salt("playback_speed")
            .selected_text(format!("{:.2}x", timeline.speed))
            .width(70.0)
            .show_ui(ui, |ui| {
                for s in SPEEDS {
                    ui.selectable_value(&mut timeline.speed, *s, format!("{s:.2}x"));
                }
            })
            .response
            .on_hover_text("Playback speed  (↑ / ↓)");

        ui.separator();
        ui.monospace(format_utc(timeline.cursor));
        if let Some((lo, hi)) = project_bounds {
            ui.label(format!("(t+{})", format_duration(timeline.cursor - lo)));
            ui.label(format!("/ {}", format_duration(hi - lo)));
        }
    });

    let Some((lo, hi)) = project_bounds else {
        return changed;
    };
    if hi <= lo {
        return changed;
    }

    let desired_size = Vec2::new(ui.available_width(), 28.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());

    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);

    let t_to_x = |t: f64| -> f32 { rect.left() + ((t - lo) / (hi - lo)) as f32 * rect.width() };

    // Shaded region showing the currently zoomed graph view range.
    let view_lo = t_to_x(timeline.view_start.clamp(lo, hi));
    let view_hi = t_to_x(timeline.view_end.clamp(lo, hi));
    let view_rect = Rect::from_min_max(
        egui::pos2(view_lo, rect.top()),
        egui::pos2(view_hi.max(view_lo + 1.0), rect.bottom()),
    );
    painter.rect_filled(view_rect, 2.0, ui.visuals().selection.bg_fill.gamma_multiply(0.35));

    // Playhead.
    let cursor_x = t_to_x(timeline.cursor.clamp(lo, hi));
    painter.line_segment(
        [egui::pos2(cursor_x, rect.top()), egui::pos2(cursor_x, rect.bottom())],
        Stroke::new(2.0, Color32::from_rgb(0xFF, 0x5C, 0x3D)),
    );

    if (response.clicked() || response.dragged())
        && let Some(pos) = response.interact_pointer_pos() {
            let frac = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            timeline.seek(lo + frac as f64 * (hi - lo), project_bounds);
            timeline.playing = false;
            changed = true;
        }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_keeps_the_centre_where_it_was() {
        let mut t = Timeline::new((0.0, 100.0));
        t.zoom_view(0.5, 50.0);
        assert_eq!((t.view_start, t.view_end), (25.0, 75.0));
        t.zoom_view(2.0, 50.0);
        assert_eq!((t.view_start, t.view_end), (0.0, 100.0));
    }

    #[test]
    fn the_window_never_collapses_to_nothing() {
        let mut t = Timeline::new((0.0, 100.0));
        for _ in 0..100 {
            t.zoom_view(0.1, 50.0);
        }
        assert!(t.view_span() >= MIN_VIEW_SPAN, "span collapsed to {}", t.view_span());
        // ... and is still zoomable back out.
        t.zoom_view(2.0, 50.0);
        assert!(t.view_span() > MIN_VIEW_SPAN);
    }

    #[test]
    fn a_box_selection_dragged_right_to_left_still_zooms() {
        let mut t = Timeline::new((0.0, 100.0));
        t.set_view(70.0, 30.0);
        assert_eq!((t.view_start, t.view_end), (30.0, 70.0));
    }

    #[test]
    fn losing_a_source_pulls_the_view_and_playhead_back_in_range() {
        let mut t = Timeline::new((0.0, 100.0));
        t.set_view(80.0, 100.0);
        t.cursor = 90.0;
        // The source covering everything past t=50 is gone.
        t.clamp_to(Some((0.0, 50.0)));
        assert_eq!(t.cursor, 50.0);
        assert!(t.view_start >= 0.0 && t.view_end <= 50.0, "{}..{}", t.view_start, t.view_end);
        assert!(t.view_span() > 0.0);
    }

    #[test]
    fn a_view_inside_the_remaining_range_is_left_alone() {
        let mut t = Timeline::new((0.0, 100.0));
        t.set_view(10.0, 20.0);
        t.clamp_to(Some((0.0, 50.0)));
        assert_eq!((t.view_start, t.view_end), (10.0, 20.0));
    }
}
