//! Markers and the measuring tool: the two things a reviewer leaves *on* the
//! time axis rather than reads off it.
//!
//! Both live on [`Timeline`] for the reason box zoom does -- every pane has to
//! agree on them, and the panes already share that struct. Both are also drawn
//! by one painter ([`paint`]) on top of whatever the pane drew, rather than as
//! plot items: the tank pane is not an `egui_plot`, and a marker that looked
//! different there would not read as the same marker.

use egui::{Align2, Color32, FontId, Pos2, Rect, Shape, Stroke};

use crate::colors::marker_color;
use crate::timeline::{Timeline, format_duration};

/// One named instant on the master timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub id: u64,
    /// Master (UTC) seconds.
    pub time: f64,
    pub color: Color32,
    pub name: String,
}

/// The markers the user has placed, kept in time order.
#[derive(Default)]
pub struct Markers {
    list: Vec<Marker>,
    /// How many have ever been placed. It numbers and colours the next one,
    /// so deleting a marker never hands its colour to a new one while its
    /// neighbours are still on screen.
    placed: u64,
}

impl Markers {
    /// Places a marker at `time`, unless there already is one exactly there
    /// -- a held key or a second click must not stack them invisibly.
    pub fn add(&mut self, time: f64) -> Option<u64> {
        if !time.is_finite() || self.list.iter().any(|m| m.time == time) {
            return None;
        }
        let id = self.placed;
        self.placed += 1;
        let at = self.list.partition_point(|m| m.time <= time);
        self.list.insert(
            at,
            Marker {
                id,
                time,
                color: marker_color(id as usize),
                name: format!("M{}", id + 1),
            },
        );
        Some(id)
    }

    pub fn remove(&mut self, id: u64) {
        self.list.retain(|m| m.id != id);
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn as_slice(&self) -> &[Marker] {
        &self.list
    }

    pub fn iter(&self) -> impl Iterator<Item = &Marker> {
        self.list.iter()
    }

    /// For renaming. The times are not handed out mutably: the list's order
    /// depends on them.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Marker> {
        self.list.iter_mut()
    }
}

/// The measuring tool: two instants, and what lies between them.
///
/// A measurement is a pair of *times*, shared by every pane. Each graph then
/// reads its own series at the two ends, which is what makes one gesture
/// answer all three questions -- how long, how much, and how much per
/// `span` -- without the user having to hit a sample with the mouse.
pub struct Measure {
    /// While on, a click in a pane places an end instead of moving the
    /// playhead.
    pub active: bool,
    /// One click measures a window of exactly `span` seconds, instead of two
    /// clicks measuring whatever lies between them.
    pub fixed: bool,
    /// The fixed window's length, in seconds.
    pub span: f64,
    a: Option<f64>,
    b: Option<f64>,
    /// Where the pointer is while the second end is still to be placed, so
    /// the band can be seen before it is committed. Written by whichever
    /// pane is hovered and read by all of them a frame later -- see
    /// [`Timeline::begin_frame`].
    hover: Option<f64>,
    hover_next: Option<f64>,
}

impl Default for Measure {
    fn default() -> Self {
        Self {
            active: false,
            fixed: false,
            span: 60.0,
            a: None,
            b: None,
            hover: None,
            hover_next: None,
        }
    }
}

impl Measure {
    /// A click at `t`. With a fixed span every click starts the window
    /// there; otherwise the first places one end, the second the other, and
    /// a third starts over.
    pub fn click(&mut self, t: f64) {
        if self.fixed || self.a.is_none() || self.b.is_some() {
            self.a = Some(t);
            self.b = None;
        } else {
            self.b = Some(t);
        }
    }

    /// The pointer is at `t`. Only matters between the two clicks.
    pub fn hover(&mut self, t: f64) {
        if self.active && !self.fixed && self.a.is_some() && self.b.is_none() {
            self.hover_next = Some(t);
        }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.hover = self.hover_next.take();
    }

    /// Forgets the measurement, leaving the tool as it is.
    pub fn clear(&mut self) {
        self.a = None;
        self.b = None;
        self.hover = None;
        self.hover_next = None;
    }

    /// Turns the tool on or off. Off takes the measurement with it: a band
    /// that outlived its tool could not be moved or removed by clicking.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.clear();
        }
    }

    /// The measured window, earlier end first, once it has two ends -- the
    /// second of which may still be following the pointer.
    pub fn ends(&self) -> Option<(f64, f64)> {
        let a = self.a?;
        let b = if self.fixed {
            a + self.span
        } else {
            self.b.or(self.hover)?
        };
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The first end on its own, while it is waiting for the second.
    fn pending(&self) -> Option<f64> {
        if self.ends().is_some() { None } else { self.a }
    }
}

/// A length of time as a reader would say it: seconds while it is short, a
/// clock once it is not.
pub fn format_span(seconds: f64) -> String {
    let s = seconds.abs();
    if s < 1.0 {
        format!("{:.1} ms", s * 1000.0)
    } else if s < 60.0 {
        format!("{s:.3} s")
    } else {
        format_duration(s)
    }
}

/// One series across a measured window: how far it moved, between which two
/// readings, and how fast.
pub fn describe_change(from: f64, to: f64, seconds: f64, unit: &str) -> String {
    let delta = to - from;
    let mut text = format!("Δ {delta:+.4} {unit}").trim_end().to_string();
    text.push_str(&format!("  ({from:.4} → {to:.4}"));
    if seconds > 0.0 {
        text.push_str(&format!(", {:+.4} {unit}/s", delta / seconds));
    }
    text.push(')');
    text
}

/// Draws the markers and the measured window over a pane whose horizontal
/// axis is the master timeline: `frame` is where its data is drawn, `window`
/// the times at `frame`'s left and right edges.
pub fn paint(ui: &egui::Ui, frame: Rect, window: (f64, f64), timeline: &Timeline) {
    let (t0, t1) = window;
    if t1 <= t0 || frame.width() <= 0.0 {
        return;
    }
    let x_of = |t: f64| frame.left() + ((t - t0) / (t1 - t0)) as f32 * frame.width();
    let visible = |t: f64| (t0..=t1).contains(&t);
    let painter = ui.painter_at(frame);
    let visuals = ui.visuals();
    let font = FontId::proportional(11.0);

    // --- the measured window ---
    let edge = Stroke::new(1.0, visuals.selection.stroke.color);
    let edge_line = |t: f64| {
        if visible(t) {
            painter.line_segment([Pos2::new(x_of(t), frame.top()), Pos2::new(x_of(t), frame.bottom())], edge);
        }
    };
    if let Some((a, b)) = timeline.measure.ends() {
        // Clamped in time rather than in pixels: far enough out, a window's
        // edge is an x that an `f32` cannot hold.
        let (left, right) = (x_of(a.clamp(t0, t1)), x_of(b.clamp(t0, t1)));
        if b >= t0 && a <= t1 {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(left, frame.top()), Pos2::new(right, frame.bottom())),
                0.0,
                visuals.selection.bg_fill.gamma_multiply(0.25),
            );
            edge_line(a);
            edge_line(b);
            let text = format!("Δt {}", format_span(b - a));
            let galley = painter.layout_no_wrap(text, font.clone(), visuals.strong_text_color());
            // Centred on the band, but kept inside the pane: a window wider
            // than the view still has to say how wide it is.
            let half = galley.size().x * 0.5 + 3.0;
            let centre = ((left + right) * 0.5).clamp(frame.left() + half, (frame.right() - half).max(frame.left() + half));
            let rect = Align2::CENTER_BOTTOM
                .anchor_size(Pos2::new(centre, frame.bottom() - 4.0), galley.size())
                .expand2(egui::vec2(3.0, 1.0));
            painter.rect_filled(rect, 2.0, visuals.extreme_bg_color.gamma_multiply(0.85));
            painter.galley(rect.shrink2(egui::vec2(3.0, 1.0)).min, galley, visuals.strong_text_color());
        }
    } else if let Some(a) = timeline.measure.pending() {
        edge_line(a);
    }

    // --- markers ---
    for marker in timeline.markers.iter().filter(|m| visible(m.time)) {
        let x = x_of(marker.time);
        // Dashed, so that a marker is never mistaken for the playhead --
        // which is a solid line, and whose colour a marker may come close to.
        painter.extend(Shape::dashed_line(
            &[Pos2::new(x, frame.top()), Pos2::new(x, frame.bottom())],
            Stroke::new(1.5, marker.color),
            6.0,
            3.0,
        ));
        if marker.name.is_empty() {
            continue;
        }
        let galley = painter.layout_no_wrap(marker.name.clone(), font.clone(), Color32::WHITE);
        let tag = Rect::from_min_size(Pos2::new(x, frame.top()), galley.size() + egui::vec2(6.0, 2.0));
        painter.rect_filled(tag, 2.0, marker.color);
        // White on the marker's own colour: every marker colour is a
        // mid-tone, so this holds in both themes.
        painter.galley(tag.min + egui::vec2(3.0, 1.0), galley, Color32::WHITE);
    }
}

/// [`paint`] for an `egui_plot` pane, which also reports where the pointer is
/// over it so a half-placed measurement can follow the mouse.
pub fn paint_over_plot<R>(ui: &egui::Ui, plot: &egui_plot::PlotResponse<R>, timeline: &mut Timeline) {
    let transform = &plot.transform;
    let bounds = transform.bounds();
    paint(ui, *transform.frame(), (bounds.min()[0], bounds.max()[0]), timeline);
    if let Some(pos) = plot.response.hover_pos()
        && transform.frame().contains(pos)
    {
        timeline.measure.hover(transform.value_from_position(pos).x);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_stay_in_time_order_and_each_gets_its_own_colour() {
        let mut markers = Markers::default();
        for t in [30.0, 10.0, 20.0, 5.0] {
            markers.add(t);
        }
        let times: Vec<f64> = markers.iter().map(|m| m.time).collect();
        assert_eq!(times, [5.0, 10.0, 20.0, 30.0]);
        let mut colors: Vec<_> = markers.iter().map(|m| m.color).collect();
        colors.dedup();
        colors.sort_by_key(|c| c.to_array());
        colors.dedup();
        assert_eq!(colors.len(), 4);
    }

    #[test]
    fn a_second_marker_on_the_same_instant_is_refused() {
        let mut markers = Markers::default();
        assert!(markers.add(10.0).is_some());
        assert!(markers.add(10.0).is_none());
        assert!(markers.add(f64::NAN).is_none());
        assert_eq!(markers.len(), 1);
    }

    #[test]
    fn a_deleted_markers_colour_is_not_handed_to_the_next_one() {
        let mut markers = Markers::default();
        markers.add(1.0);
        let second = markers.add(2.0).unwrap();
        let taken: Vec<_> = markers.iter().map(|m| m.color).collect();
        markers.remove(second);
        markers.add(3.0);
        let newest = markers.iter().last().unwrap();
        assert!(!taken.contains(&newest.color));
        assert_eq!(newest.name, "M3");
    }

    #[test]
    fn two_clicks_measure_and_a_third_starts_over() {
        let mut m = Measure::default();
        m.set_active(true);
        assert_eq!(m.ends(), None);
        m.click(40.0);
        assert_eq!(m.ends(), None, "one end is not a measurement");
        // Clicked right to left: the ends still come back in time order.
        m.click(10.0);
        assert_eq!(m.ends(), Some((10.0, 40.0)));
        m.click(70.0);
        assert_eq!(m.ends(), None);
        assert_eq!(m.pending(), Some(70.0));
    }

    #[test]
    fn a_fixed_span_measures_from_a_single_click() {
        let mut m = Measure::default();
        m.set_active(true);
        m.fixed = true;
        m.click(100.0);
        assert_eq!(m.ends(), Some((100.0, 160.0)));
        // The window follows the span as it is typed in...
        m.span = 5.0;
        assert_eq!(m.ends(), Some((100.0, 105.0)));
        // ... and the next click moves it rather than ending it.
        m.click(200.0);
        assert_eq!(m.ends(), Some((200.0, 205.0)));
    }

    #[test]
    fn the_second_end_follows_the_pointer_a_frame_later() {
        let mut m = Measure::default();
        m.hover(5.0);
        m.begin_frame();
        assert_eq!(m.ends(), None, "the tool is off");

        m.set_active(true);
        m.click(10.0);
        m.hover(25.0);
        assert_eq!(m.ends(), None);
        m.begin_frame();
        assert_eq!(m.ends(), Some((10.0, 25.0)));
        // The pointer left every pane: nothing to follow.
        m.begin_frame();
        assert_eq!(m.ends(), None);
        // A committed end is not dragged around by the pointer.
        m.click(30.0);
        m.hover(99.0);
        m.begin_frame();
        assert_eq!(m.ends(), Some((10.0, 30.0)));
    }

    #[test]
    fn turning_the_tool_off_takes_the_measurement_with_it() {
        let mut m = Measure::default();
        m.set_active(true);
        m.click(1.0);
        m.click(2.0);
        m.set_active(false);
        assert_eq!(m.ends(), None);
    }

    #[test]
    fn a_change_reads_as_a_difference_its_ends_and_a_rate() {
        assert_eq!(
            describe_change(40.0, 43.0, 60.0, "bar"),
            "Δ +3.0000 bar  (40.0000 → 43.0000, +0.0500 bar/s)"
        );
        // No unit, and no time to divide by.
        assert_eq!(describe_change(2.0, 1.5, 0.0, ""), "Δ -0.5000  (2.0000 → 1.5000)");
    }

    #[test]
    fn a_span_is_written_at_the_scale_it_is_on() {
        assert_eq!(format_span(0.0125), "12.5 ms");
        assert_eq!(format_span(12.5), "12.500 s");
        assert_eq!(format_span(60.0), "01:00.000");
    }
}
