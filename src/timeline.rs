use egui::{Color32, Rect, Sense, Stroke, Vec2};

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
}

impl Timeline {
    pub fn new(bounds: (f64, f64)) -> Self {
        Self {
            view_start: bounds.0,
            view_end: bounds.1,
            cursor: bounds.0,
            playing: false,
            speed: 1.0,
        }
    }

    pub fn tick(&mut self, dt_seconds: f64, bounds: Option<(f64, f64)>) {
        if !self.playing {
            return;
        }
        self.cursor += dt_seconds * self.speed as f64;
        if let Some((_, hi)) = bounds {
            if self.cursor >= hi {
                self.cursor = hi;
                self.playing = false;
            }
        }
    }

    pub fn seek(&mut self, t: f64, bounds: Option<(f64, f64)>) {
        self.cursor = if let Some((lo, hi)) = bounds { t.clamp(lo, hi) } else { t };
    }

    pub fn step(&mut self, delta_seconds: f64, bounds: Option<(f64, f64)>) {
        let t = self.cursor + delta_seconds;
        self.seek(t, bounds);
    }
}

pub fn format_utc(t: f64) -> String {
    match chrono::DateTime::from_timestamp(t.floor() as i64, 0) {
        Some(dt) => format!("{}.{:03}Z", dt.format("%Y-%m-%d %H:%M:%S"), ((t.fract()) * 1000.0) as u32),
        None => format!("{t:.3}"),
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
        if ui.button("⏮").on_hover_text("Jump to start").clicked() {
            if let Some((lo, _)) = project_bounds {
                timeline.seek(lo, project_bounds);
                changed = true;
            }
        }
        if ui.button("◀").on_hover_text("Step back 1s").clicked() {
            timeline.step(-1.0, project_bounds);
            changed = true;
        }
        if ui.button(play_label).on_hover_text("Play/Pause").clicked() {
            timeline.playing = !timeline.playing;
        }
        if ui.button("▶").on_hover_text("Step forward 1s").clicked() {
            timeline.step(1.0, project_bounds);
            changed = true;
        }
        if ui.button("⏭").on_hover_text("Jump to end").clicked() {
            if let Some((_, hi)) = project_bounds {
                timeline.seek(hi, project_bounds);
                changed = true;
            }
        }

        ui.separator();
        ui.label("Speed");
        egui::ComboBox::from_id_salt("playback_speed")
            .selected_text(format!("{:.2}x", timeline.speed))
            .width(70.0)
            .show_ui(ui, |ui| {
                for s in [0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0] {
                    ui.selectable_value(&mut timeline.speed, s as f32, format!("{s:.2}x"));
                }
            });

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

    if response.clicked() || response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let frac = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            timeline.seek(lo + frac as f64 * (hi - lo), project_bounds);
            timeline.playing = false;
            changed = true;
        }
    }

    changed
}
