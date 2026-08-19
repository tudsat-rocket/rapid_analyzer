//! The nitrous oxide phase pane: where a measured tank sits relative to the
//! saturation curve, over time and on the curve itself.
//!
//! A nitrous tank is only understandable as a point relative to its own
//! vapour pressure. The same 50 bar is a full, cold, self-pressurising tank at
//! 20 °C and a nearly empty warm one at 25 °C, and neither the pressure trace
//! nor the temperature trace says which -- only the two together, against
//! [`crate::n2o`], do. So this pane draws exactly one thing: the distance from
//! the curve, with the three zones it separates shaded behind it.

use egui::{Color32, Vec2b};
use egui_plot::{Corner, FilledArea, HLine, Legend, Line, Plot, PlotBounds, Points, VLine};

use crate::model::{Project, SourceId, SourceKind};
use crate::n2o::{self, Phase};
use crate::series::TimeSeries;
use crate::timeline::Timeline;

pub type VaporId = u64;

/// Standard atmosphere, added to a gauge reading to get an absolute one.
const AMBIENT_KPA: f64 = 101.325;

/// Same order of magnitude as a graph pane's budget; the state trace is one
/// line plus its zones.
const TARGET_POINTS: usize = 1500;

/// At most this many series in a picker list at once. Rendering a few hundred
/// buttons a frame is slow, and a list that long is unusable anyway -- the
/// filter box above it is the way through.
const PICKER_LIMIT: usize = 150;

const CURSOR_COLOR: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x3D);

pub fn phase_color(phase: Phase) -> Color32 {
    match phase {
        Phase::Liquid => Color32::from_rgb(0x4C, 0x9A, 0xFF),
        Phase::Saturated => Color32::from_rgb(0x4C, 0xD9, 0x7B),
        Phase::Vapor => Color32::from_rgb(0xFF, 0x8A, 0x3D),
        Phase::Supercritical => Color32::from_rgb(0xB0, 0x7B, 0xFF),
        Phase::BelowTriple => Color32::from_gray(0x99),
    }
}

/// The same colour as a background wash. Zones have to be readable behind a
/// line, not compete with it.
fn zone_fill(phase: Phase) -> Color32 {
    phase_color(phase).gamma_multiply(0.16)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VaporMode {
    /// Distance from the curve, against time -- shares the master timeline.
    State,
    /// Pressure against temperature: the curve itself, with the run drawn on
    /// it. With nothing selected this is just the curve.
    Curve,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TempUnit {
    Celsius,
    Kelvin,
    Fahrenheit,
}

impl TempUnit {
    pub const ALL: &'static [Self] = &[Self::Celsius, Self::Kelvin, Self::Fahrenheit];

    pub fn label(self) -> &'static str {
        match self {
            Self::Celsius => "°C",
            Self::Kelvin => "K",
            Self::Fahrenheit => "°F",
        }
    }

    pub fn to_kelvin(self, value: f64) -> f64 {
        match self {
            Self::Celsius => value + 273.15,
            Self::Kelvin => value,
            Self::Fahrenheit => (value - 32.0) / 1.8 + 273.15,
        }
    }

    pub fn from_kelvin(self, kelvin: f64) -> f64 {
        match self {
            Self::Celsius => kelvin - 273.15,
            Self::Kelvin => kelvin,
            Self::Fahrenheit => (kelvin - 273.15) * 1.8 + 32.0,
        }
    }

    /// A *difference* in kelvin expressed in this unit -- the 273.15 does not
    /// apply to a span, only to a reading.
    pub fn delta_from_kelvin(self, kelvin: f64) -> f64 {
        match self {
            Self::Celsius | Self::Kelvin => kelvin,
            Self::Fahrenheit => kelvin * 1.8,
        }
    }

    /// The unit a series declares, when it is one we know.
    pub fn from_series_unit(unit: Option<&str>) -> Option<Self> {
        match unit?.trim().to_lowercase().as_str() {
            "°c" | "degc" | "c" | "celsius" => Some(Self::Celsius),
            "k" | "kelvin" => Some(Self::Kelvin),
            "°f" | "degf" | "f" => Some(Self::Fahrenheit),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PressureUnit {
    Bar,
    Kpa,
    Mpa,
    Psi,
    Pa,
}

impl PressureUnit {
    pub const ALL: &'static [Self] = &[Self::Bar, Self::Kpa, Self::Mpa, Self::Psi, Self::Pa];

    pub fn label(self) -> &'static str {
        match self {
            Self::Bar => "bar",
            Self::Kpa => "kPa",
            Self::Mpa => "MPa",
            Self::Psi => "psi",
            Self::Pa => "Pa",
        }
    }

    fn kpa_per_unit(self) -> f64 {
        match self {
            Self::Bar => 100.0,
            Self::Kpa => 1.0,
            Self::Mpa => 1000.0,
            Self::Psi => 6.894_757_293_168_361,
            Self::Pa => 0.001,
        }
    }

    pub fn to_kpa(self, value: f64) -> f64 {
        value * self.kpa_per_unit()
    }

    pub fn from_kpa(self, kpa: f64) -> f64 {
        kpa / self.kpa_per_unit()
    }

    pub fn from_series_unit(unit: Option<&str>) -> Option<Self> {
        match unit?.trim().to_lowercase().as_str() {
            "bar" => Some(Self::Bar),
            "kpa" => Some(Self::Kpa),
            "mpa" => Some(Self::Mpa),
            "psi" | "psia" | "psig" => Some(Self::Psi),
            "pa" => Some(Self::Pa),
            _ => None,
        }
    }
}

/// One series of one source, by name -- the same way a graph pane refers to
/// one, so a reimport or a rename shows up as "no longer loaded" rather than
/// as silently wrong data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SeriesRef {
    pub source: SourceId,
    pub series: String,
}

pub struct VaporSpec {
    pub id: VaporId,
    pub temperature: Option<SeriesRef>,
    pub pressure: Option<SeriesRef>,
    pub t_unit: TempUnit,
    pub p_unit: PressureUnit,
    /// The pressure series reads against ambient, so ambient has to be added
    /// back before it can be compared with a vapour pressure. Getting this
    /// wrong moves every point by a bar, which at tank pressures is about the
    /// width of the saturated band -- so it is a switch, not an assumption.
    pub gauge: bool,
    /// Half-width of the "call it saturated" band, as a percentage of the
    /// vapour pressure. See [`n2o::state`] for why it exists.
    pub band_percent: f64,
    pub mode: VaporMode,
    /// Bounds of the curve view, once the user has panned or zoomed it. The
    /// curve's axes are temperature and pressure, so unlike every other pane
    /// here it does not follow the master timeline and has to keep its own.
    curve_view: Option<[[f64; 2]; 2]>,
    t_filter: String,
    p_filter: String,
}

impl VaporSpec {
    pub fn title(&self) -> String {
        match (&self.temperature, &self.pressure) {
            (Some(t), Some(p)) => match shared_group(&t.series, &p.series) {
                Some(group) => format!("N₂O · {group}"),
                None => "N₂O phase".to_string(),
            },
            _ => "N₂O vapour pressure".to_string(),
        }
    }

    /// Whether this pane draws anything belonging to `source`.
    pub fn uses(&self, source: SourceId) -> bool {
        [&self.temperature, &self.pressure]
            .into_iter()
            .flatten()
            .any(|r| r.source == source)
    }

    /// Forgets series from a source that is being unloaded. The pane stays --
    /// the curve is still worth looking at, and the user can point it at
    /// another log.
    pub fn forget_source(&mut self, source: SourceId) {
        self.temperature.take_if(|r| r.source == source);
        self.pressure.take_if(|r| r.source == source);
    }
}

/// `("CAN_SENSOR[5].slot0", "CAN_SENSOR[5].slot1")` -> `"CAN_SENSOR[5]"`.
fn shared_group<'a>(a: &'a str, b: &str) -> Option<&'a str> {
    let group = a.rfind('.').map(|dot| &a[..dot])?;
    b.starts_with(group).then_some(group)
}

/// Every phase pane the user has opened, and the only place their ids are minted.
#[derive(Default)]
pub struct Vapors {
    list: Vec<VaporSpec>,
    next_id: VaporId,
}

impl Vapors {
    pub fn get(&self, id: VaporId) -> Option<&VaporSpec> {
        self.list.iter().find(|v| v.id == id)
    }

    pub fn get_mut(&mut self, id: VaporId) -> Option<&mut VaporSpec> {
        self.list.iter_mut().find(|v| v.id == id)
    }

    /// Opens a pane, pointed at the most likely pair of series in the project.
    pub fn create(&mut self, project: &Project) -> VaporId {
        let id = self.next_id;
        self.next_id += 1;
        let (temperature, pressure) = guess_series(project);
        let t_unit = temperature
            .as_ref()
            .and_then(|r| declared_unit(project, r))
            .and_then(|u| TempUnit::from_series_unit(Some(&u)))
            .unwrap_or(TempUnit::Celsius);
        let p_unit = pressure
            .as_ref()
            .and_then(|r| declared_unit(project, r))
            .and_then(|u| PressureUnit::from_series_unit(Some(&u)))
            .unwrap_or(PressureUnit::Bar);
        // With nothing to compare, the curve on its own is the whole point of
        // opening the pane -- so that is what it opens on.
        let mode = if temperature.is_some() && pressure.is_some() {
            VaporMode::State
        } else {
            VaporMode::Curve
        };
        self.list.push(VaporSpec {
            id,
            temperature,
            pressure,
            t_unit,
            p_unit,
            gauge: false,
            band_percent: 2.0,
            mode,
            curve_view: None,
            t_filter: String::new(),
            p_filter: String::new(),
        });
        id
    }

    pub fn close(&mut self, id: VaporId) {
        self.list.retain(|v| v.id != id);
    }

    pub fn forget_source(&mut self, source: SourceId) {
        for spec in &mut self.list {
            spec.forget_source(source);
        }
    }
}

fn declared_unit(project: &Project, r: &SeriesRef) -> Option<String> {
    series_of(project, r).and_then(|(s, _)| s.unit.clone())
}

/// The series and its source's time offset.
fn series_of<'p>(project: &'p Project, r: &SeriesRef) -> Option<(&'p TimeSeries, f64)> {
    let source = project.source(r.source)?;
    let SourceKind::Log(log) = &source.kind else {
        return None;
    };
    let series = log.series.iter().find(|s| s.name == r.series)?;
    Some((series, source.offset_seconds))
}

/// How well a series looks like the quantity we want: its declared unit
/// first, then its name. A declared unit is evidence; a name containing
/// "temp" is a guess.
fn score(series: &TimeSeries, temperature: bool) -> u8 {
    let unit = series.unit.as_deref();
    let by_unit = if temperature {
        TempUnit::from_series_unit(unit).is_some()
    } else {
        PressureUnit::from_series_unit(unit).is_some()
    };
    let name = series.name.to_lowercase();
    let by_name = if temperature {
        name.contains("temp")
    } else {
        name.contains("press")
    };
    match (by_unit, by_name) {
        (true, true) => 3,
        (true, false) => 2,
        (false, true) => 1,
        (false, false) => 0,
    }
}

/// Picks the most likely temperature/pressure pair in the project, preferring
/// two that describe the same thing -- the same vessel, or the same board --
/// over the two best individual matches.
fn guess_series(project: &Project) -> (Option<SeriesRef>, Option<SeriesRef>) {
    let mut best: Option<(u16, SeriesRef, SeriesRef)> = None;
    let mut best_temperature: Option<(u8, SeriesRef)> = None;
    let mut best_pressure: Option<(u8, SeriesRef)> = None;

    for source in &project.sources {
        let SourceKind::Log(log) = &source.kind else {
            continue;
        };
        let reference = |s: &TimeSeries| SeriesRef {
            source: source.id,
            series: s.name.clone(),
        };
        for series in &log.series {
            for (temperature, slot) in [(true, &mut best_temperature), (false, &mut best_pressure)] {
                let s = score(series, temperature);
                if s > 0 && slot.as_ref().is_none_or(|(best, _)| s > *best) {
                    *slot = Some((s, reference(series)));
                }
            }
        }
        // A pair from one message is worth more than two better-scoring
        // series that measure unrelated things.
        for t in log.series.iter().filter(|s| score(s, true) > 0) {
            for p in log.series.iter().filter(|s| score(s, false) > 0) {
                if shared_group(&t.name, &p.name).is_none() {
                    continue;
                }
                let combined = score(t, true) as u16 + score(p, false) as u16;
                if best.as_ref().is_none_or(|(b, _, _)| combined > *b) {
                    best = Some((combined, reference(t), reference(p)));
                }
            }
        }
    }

    match best {
        Some((_, t, p)) => (Some(t), Some(p)),
        None => (best_temperature.map(|(_, r)| r), best_pressure.map(|(_, r)| r)),
    }
}

/// One measurement pair, on the master timeline.
#[derive(Clone, Copy)]
struct Sample {
    t: f64,
    state: n2o::State,
}

#[derive(Default)]
struct Prepared {
    samples: Vec<Sample>,
    /// Why there is nothing to draw, when there isn't.
    problem: Option<String>,
}

impl Prepared {
    fn problem(message: impl Into<String>) -> Self {
        Self {
            samples: Vec::new(),
            problem: Some(message.into()),
        }
    }
}

impl VaporSpec {
    /// Pairs the two series up over `window`.
    ///
    /// The two are logged independently, at their own rates, so one of them
    /// has to be interpolated onto the other's timestamps. The denser one in
    /// this window wins, since interpolating the sparse one loses less. Where
    /// the other series has not started (or has ended) there is no pair to be
    /// made and the sample is dropped -- holding its first or last value would
    /// invent a phase out of nothing.
    fn prepare(&self, project: &Project, window: (f64, f64)) -> Prepared {
        let (Some(t_ref), Some(p_ref)) = (&self.temperature, &self.pressure) else {
            return Prepared::problem("Pick a temperature and a pressure series above.");
        };
        let (Some((t_series, t_offset)), Some((p_series, p_offset))) =
            (series_of(project, t_ref), series_of(project, p_ref))
        else {
            return Prepared::problem("One of the series is no longer loaded.");
        };

        let t_points = t_series.slice_for_range(window.0, window.1, t_offset, TARGET_POINTS);
        let p_points = p_series.slice_for_range(window.0, window.1, p_offset, TARGET_POINTS);
        if t_points.is_empty() || p_points.is_empty() {
            return Prepared::problem("Neither series has samples in this time window.");
        }

        let temperature_is_base = t_points.len() >= p_points.len();
        let (base, other, other_offset) = if temperature_is_base {
            (&t_points, p_series, p_offset)
        } else {
            (&p_points, t_series, t_offset)
        };
        let Some((other_lo, other_hi)) = other.time_bounds().map(|(lo, hi)| (lo + other_offset, hi + other_offset))
        else {
            return Prepared::problem("One of the series is empty.");
        };

        let band = self.band_percent / 100.0;
        let mut samples = Vec::with_capacity(base.len());
        for point in base {
            let (t, value) = (point[0], point[1]);
            if t < other_lo || t > other_hi {
                continue;
            }
            let Some(paired) = other.value_at(t, other_offset) else {
                continue;
            };
            let (temperature, pressure) = if temperature_is_base { (value, paired) } else { (paired, value) };
            samples.push(Sample {
                t,
                state: n2o::state(self.t_unit.to_kelvin(temperature), self.absolute_kpa(pressure), band),
            });
        }

        if samples.is_empty() {
            return Prepared::problem("The two series do not overlap in this time window.");
        }
        Prepared {
            samples,
            problem: None,
        }
    }

    /// A reading in the pane's pressure unit, as absolute kPa.
    fn absolute_kpa(&self, reading: f64) -> f64 {
        self.p_unit.to_kpa(reading) + if self.gauge { AMBIENT_KPA } else { 0.0 }
    }

    /// The state at one instant, for the readout that follows the playhead.
    fn state_at(&self, project: &Project, t: f64) -> Option<n2o::State> {
        let (t_ref, p_ref) = (self.temperature.as_ref()?, self.pressure.as_ref()?);
        let (t_series, t_offset) = series_of(project, t_ref)?;
        let (p_series, p_offset) = series_of(project, p_ref)?;
        let inside = |s: &TimeSeries, offset: f64| {
            s.time_bounds()
                .is_some_and(|(lo, hi)| (lo + offset..=hi + offset).contains(&t))
        };
        if !inside(t_series, t_offset) || !inside(p_series, p_offset) {
            return None;
        }
        let temperature = t_series.value_at(t, t_offset)?;
        let pressure = p_series.value_at(t, p_offset)?;
        Some(n2o::state(
            self.t_unit.to_kelvin(temperature),
            self.absolute_kpa(pressure),
            self.band_percent / 100.0,
        ))
    }

    /// Draws the pane. Returns `true` if the user asked to close it.
    pub fn ui(&mut self, ui: &mut egui::Ui, project: &Project, timeline: &mut Timeline) -> bool {
        let close = self.header(ui, project);

        let prepared = if self.temperature.is_some() && self.pressure.is_some() {
            self.prepare(project, (timeline.view_start, timeline.view_end))
        } else if self.mode == VaporMode::Curve {
            // The curve alone is a perfectly good thing to look at.
            Prepared::default()
        } else {
            Prepared::problem("Pick a temperature and a pressure series above.")
        };

        self.readout(ui, project, timeline.cursor);

        match self.mode {
            VaporMode::State => self.state_plot(ui, &prepared, timeline, project.time_bounds()),
            VaporMode::Curve => self.curve_plot(ui, &prepared, timeline),
        }
        close
    }

    /// Returns `true` if the user hit the close button.
    ///
    /// Unlike a graph pane, this one is not made of series the sidebar can
    /// untick, so closing it has to be possible from the pane itself.
    fn header(&mut self, ui: &mut egui::Ui, project: &Project) -> bool {
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.mode, VaporMode::State, "State over time")
                .on_hover_text("How far the tank is from its own vapour pressure, along the master timeline");
            ui.selectable_value(&mut self.mode, VaporMode::Curve, "Vapour pressure curve")
                .on_hover_text("Pressure against temperature, with the run drawn on the curve");
            if self.mode == VaporMode::Curve
                && ui.button("⟲").on_hover_text("Fit the curve back into view").clicked()
            {
                self.curve_view = None;
            }
            ui.separator();
            ui.label("band ±");
            ui.add(
                egui::DragValue::new(&mut self.band_percent)
                    .range(0.0..=25.0)
                    .speed(0.1)
                    .suffix(" %"),
            )
            .on_hover_text(
                "How far off the curve a point may sit and still count as saturated.\n\
                 A tank at equilibrium never reads exactly on it: this is the sensors' error, not the fluid's.",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.small_button("✖").on_hover_text("Close this plot").clicked();
            });
        });

        ui.horizontal_wrapped(|ui| {
            let mut t_pick = None;
            let mut p_pick = None;
            ui.label("T");
            series_picker(ui, "vapor_t", project, &self.temperature, &mut self.t_filter, true, &mut t_pick);
            unit_combo(ui, "vapor_t_unit", &mut self.t_unit, TempUnit::ALL, TempUnit::label);
            ui.separator();
            ui.label("P");
            series_picker(ui, "vapor_p", project, &self.pressure, &mut self.p_filter, false, &mut p_pick);
            unit_combo(ui, "vapor_p_unit", &mut self.p_unit, PressureUnit::ALL, PressureUnit::label);
            ui.selectable_value(&mut self.gauge, false, "abs")
                .on_hover_text("The pressure series is absolute");
            ui.selectable_value(&mut self.gauge, true, "gauge")
                .on_hover_text("The pressure series reads against ambient; one atmosphere is added before comparing");

            // A newly picked series brings its own declared unit with it,
            // which is nearly always the right one.
            if let Some(r) = t_pick {
                if let Some(unit) = TempUnit::from_series_unit(declared_unit(project, &r).as_deref()) {
                    self.t_unit = unit;
                }
                self.temperature = Some(r);
            }
            if let Some(r) = p_pick {
                if let Some(unit) = PressureUnit::from_series_unit(declared_unit(project, &r).as_deref()) {
                    self.p_unit = unit;
                }
                self.pressure = Some(r);
            }
        });
        close
    }

    /// The state at the playhead, in words, so the answer to "what was it
    /// doing here" does not depend on reading a line's height.
    fn readout(&self, ui: &mut egui::Ui, project: &Project, cursor: f64) {
        let Some(state) = self.state_at(project, cursor) else {
            ui.horizontal(|ui| ui.weak("no reading at the playhead"));
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(phase_color(state.phase), egui::RichText::new(state.phase.label()).strong());
            ui.label(format!(
                "T {:.2} {}   P {:.2} {}",
                self.t_unit.from_kelvin(state.t_k),
                self.t_unit.label(),
                self.p_unit.from_kpa(state.p_kpa),
                self.p_unit.label(),
            ));
            if let (Some(psat), Some(margin)) = (state.psat_kpa, state.margin_kpa) {
                ui.weak(format!(
                    "Psat {:.2} {}  ({:+.2} {})",
                    self.p_unit.from_kpa(psat),
                    self.p_unit.label(),
                    self.p_unit.from_kpa(margin),
                    self.p_unit.label(),
                ));
            }
            if let Some(superheat) = state.superheat_k {
                ui.weak(format!("superheat {superheat:+.2} K"));
            }
        })
        .response
        .on_hover_text(describe(self.t_unit, self.p_unit, &state));
    }
}

/// Contiguous stretch of samples that has a vapour pressure to be measured
/// against. Above the critical temperature there is none, and the zones have
/// to stop rather than be drawn through a gap that means something else.
struct Run {
    xs: Vec<f64>,
    margin: Vec<f64>,
    band_lo: Vec<f64>,
    band_hi: Vec<f64>,
}

impl VaporSpec {
    /// Splits the samples into stretches with a saturation curve, converting
    /// into the pane's pressure unit on the way.
    fn runs(&self, samples: &[Sample]) -> Vec<Run> {
        let mut runs: Vec<Run> = Vec::new();
        let mut open = false;
        for sample in samples {
            let (Some(psat), Some(margin)) = (sample.state.psat_kpa, sample.state.margin_kpa) else {
                open = false;
                continue;
            };
            if !open {
                runs.push(Run {
                    xs: Vec::new(),
                    margin: Vec::new(),
                    band_lo: Vec::new(),
                    band_hi: Vec::new(),
                });
                open = true;
            }
            let run = runs.last_mut().expect("just pushed");
            let band = self.p_unit.from_kpa(psat * self.band_percent / 100.0);
            run.xs.push(sample.t);
            run.margin.push(self.p_unit.from_kpa(margin));
            run.band_lo.push(-band);
            run.band_hi.push(band);
        }
        runs.retain(|r| !r.xs.is_empty());
        runs
    }

    fn state_plot(&mut self, ui: &mut egui::Ui, prepared: &Prepared, timeline: &mut Timeline, bounds: Option<(f64, f64)>) {
        if let Some(problem) = &prepared.problem {
            ui.weak(problem);
            return;
        }
        let runs = self.runs(&prepared.samples);
        let off_curve = prepared.samples.iter().filter(|s| s.state.margin_kpa.is_none()).count();
        if off_curve > 0 {
            ui.small(format!(
                "{off_curve} of {} samples are outside the two-phase range (above the critical temperature, or below the triple point)",
                prepared.samples.len()
            ));
        }
        if runs.is_empty() {
            ui.weak("Nothing in this window can be compared with the vapour pressure curve.");
            return;
        }

        // The zone bands are part of the picture, so the axis has to hold
        // them as well as the trace.
        let (mut y_lo, mut y_hi) = (0.0f64, 0.0f64);
        for run in &runs {
            for value in run.margin.iter().chain(&run.band_lo).chain(&run.band_hi) {
                y_lo = y_lo.min(*value);
                y_hi = y_hi.max(*value);
            }
        }
        let pad = ((y_hi - y_lo) * 0.1).max(f64::MIN_POSITIVE);
        let (y_lo, y_hi) = (y_lo - pad, y_hi + pad);

        let (view_start, view_end) = (timeline.view_start, timeline.view_end);
        let cursor = timeline.cursor;
        let box_zoom = timeline.box_zoom;
        let line_color = ui.visuals().strong_text_color();
        let (t_unit, p_unit) = (self.t_unit, self.p_unit);
        let hover: Vec<Sample> = prepared.samples.clone();

        let plot = Plot::new(("vapor", self.id))
            .height(ui.available_height().max(80.0))
            .allow_zoom(Vec2b::new(true, false))
            .allow_drag(Vec2b::new(!box_zoom, false))
            .allow_boxed_zoom(true)
            .boxed_zoom_pointer_button(if box_zoom {
                egui::PointerButton::Primary
            } else {
                egui::PointerButton::Secondary
            })
            .legend(Legend::default().position(Corner::LeftTop))
            .y_axis_label(format!("P − Psat(T)  [{}]", p_unit.label()))
            .x_axis_formatter(|mark, _| crate::timeline::format_axis_time(mark.value, mark.step_size))
            .x_grid_spacer(egui_plot::uniform_grid_spacer(crate::timeline::time_grid_steps))
            .label_formatter(move |pos| {
                let position = match pos {
                    egui_plot::HoverPosition::NearDataPoint { position, .. } => *position,
                    egui_plot::HoverPosition::Elsewhere { position } => *position,
                };
                let sample = nearest(&hover, position.x)?;
                Some(format!(
                    "{}\n{}",
                    crate::timeline::format_utc(sample.t),
                    describe(t_unit, p_unit, &sample.state)
                ))
            });

        let mut clicked_time = None;
        let response = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max([view_start, y_lo], [view_end, y_hi]));
            for run in &runs {
                // A zone needs two columns to be an area; a lone sample still
                // gets its trace point, its readout and its hover.
                if run.xs.len() < 2 {
                    continue;
                }
                let top = vec![y_hi; run.xs.len()];
                let bottom = vec![y_lo; run.xs.len()];
                plot_ui.add(
                    FilledArea::new(Phase::Liquid.label(), &run.xs, &run.band_hi, &top)
                        .fill_color(zone_fill(Phase::Liquid)),
                );
                plot_ui.add(
                    FilledArea::new(Phase::Vapor.label(), &run.xs, &bottom, &run.band_lo)
                        .fill_color(zone_fill(Phase::Vapor)),
                );
                plot_ui.add(
                    FilledArea::new(Phase::Saturated.label(), &run.xs, &run.band_lo, &run.band_hi)
                        .fill_color(zone_fill(Phase::Saturated)),
                );
            }
            for run in &runs {
                let trace: Vec<[f64; 2]> = run.xs.iter().zip(&run.margin).map(|(x, y)| [*x, *y]).collect();
                plot_ui.line(Line::new("P − Psat", trace).color(line_color).width(1.5));
            }
            plot_ui.hline(
                HLine::new("on the curve", 0.0)
                    .color(phase_color(Phase::Saturated))
                    .width(1.0),
            );
            plot_ui.vline(VLine::new("cursor", cursor).color(CURSOR_COLOR));
            if plot_ui.response().clicked()
                && let Some(coord) = plot_ui.pointer_coordinate()
            {
                clicked_time = Some(coord.x);
            }
        });

        let x = response.transform.bounds();
        timeline.follow_plot(x.min()[0], x.max()[0], clicked_time, bounds);
    }

    fn curve_plot(&mut self, ui: &mut egui::Ui, prepared: &Prepared, timeline: &Timeline) {
        let curve = n2o::saturation_curve();
        let xs: Vec<f64> = curve.iter().map(|(t, _)| self.t_unit.from_kelvin(*t)).collect();
        let ys: Vec<f64> = curve.iter().map(|(_, p)| self.p_unit.from_kpa(*p)).collect();
        let (x_triple, x_critical) = (xs[0], xs[xs.len() - 1]);
        let (y_triple, y_critical) = (ys[0], ys[ys.len() - 1]);
        let span = x_critical - x_triple;

        // The zones have to keep covering the plot when it is zoomed out, so
        // they are filled far past the default view rather than up to it.
        let far_x = x_critical + span * 3.0;
        let far_y = y_critical * 10.0;
        let default_bounds = [
            [x_triple - span * 0.05, 0.0],
            [x_critical + span * 0.08, y_critical * 1.15],
        ];
        let bounds = self.curve_view.unwrap_or(default_bounds);

        let (t_unit, p_unit) = (self.t_unit, self.p_unit);
        let band = self.band_percent / 100.0;
        let samples = prepared.samples.clone();
        let cursor_state = prepared
            .samples
            .iter()
            .min_by(|a, b| (a.t - timeline.cursor).abs().total_cmp(&(b.t - timeline.cursor).abs()))
            .map(|s| s.state);
        let line_color = ui.visuals().strong_text_color();

        let box_zoom = timeline.box_zoom;
        let plot = Plot::new(("vapor_curve", self.id))
            .height(ui.available_height().max(80.0))
            .legend(Legend::default().position(Corner::RightBottom))
            .allow_drag(Vec2b::new(!box_zoom, !box_zoom))
            .allow_boxed_zoom(true)
            .boxed_zoom_pointer_button(if box_zoom {
                egui::PointerButton::Primary
            } else {
                egui::PointerButton::Secondary
            })
            .x_axis_label(format!("temperature [{}]", t_unit.label()))
            .y_axis_label(format!("pressure [{}]", p_unit.label()))
            // Anywhere in this plane is a state worth describing, whether a
            // measurement landed on it or not.
            .label_formatter(move |pos| {
                let position = match pos {
                    egui_plot::HoverPosition::NearDataPoint { position, .. } => *position,
                    egui_plot::HoverPosition::Elsewhere { position } => *position,
                };
                let state = n2o::state(t_unit.to_kelvin(position.x), p_unit.to_kpa(position.y), band);
                Some(describe(t_unit, p_unit, &state))
            });

        let response = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max(bounds[0], bounds[1]));

            let zero = vec![0.0; xs.len()];
            let ceiling = vec![far_y; xs.len()];
            plot_ui.add(
                FilledArea::new(Phase::Vapor.label(), &xs, &zero, &ys).fill_color(zone_fill(Phase::Vapor)),
            );
            plot_ui.add(
                FilledArea::new(Phase::Liquid.label(), &xs, &ys, &ceiling).fill_color(zone_fill(Phase::Liquid)),
            );
            plot_ui.add(
                FilledArea::new(
                    Phase::Supercritical.label(),
                    &[x_critical, far_x],
                    &[0.0, 0.0],
                    &[far_y, far_y],
                )
                .fill_color(zone_fill(Phase::Supercritical)),
            );

            let curve_points: Vec<[f64; 2]> = xs.iter().zip(&ys).map(|(x, y)| [*x, *y]).collect();
            plot_ui.line(Line::new("N₂O vapour pressure", curve_points).color(line_color).width(1.5));
            plot_ui.points(
                Points::new("critical point", vec![[x_critical, y_critical]])
                    .shape(egui_plot::MarkerShape::Diamond)
                    .radius(5.0)
                    .color(phase_color(Phase::Supercritical)),
            );
            plot_ui.points(
                Points::new("triple point", vec![[x_triple, y_triple]])
                    .shape(egui_plot::MarkerShape::Diamond)
                    .radius(4.0)
                    .color(phase_color(Phase::BelowTriple)),
            );

            // The run itself, one series per phase so the legend doubles as a
            // key and each can be hidden.
            for phase in [Phase::Liquid, Phase::Saturated, Phase::Vapor, Phase::Supercritical] {
                let points: Vec<[f64; 2]> = samples
                    .iter()
                    .filter(|s| s.state.phase == phase)
                    .map(|s| [t_unit.from_kelvin(s.state.t_k), p_unit.from_kpa(s.state.p_kpa)])
                    .collect();
                if !points.is_empty() {
                    plot_ui.points(Points::new(phase.short(), points).radius(1.8).color(phase_color(phase)));
                }
            }
            if let Some(state) = cursor_state {
                plot_ui.points(
                    Points::new(
                        "playhead",
                        vec![[t_unit.from_kelvin(state.t_k), p_unit.from_kpa(state.p_kpa)]],
                    )
                    .radius(5.0)
                    .color(CURSOR_COLOR),
                );
            }
        });

        // Temperature is not time: this plot keeps its own view instead of
        // driving the master timeline.
        let bounds = response.transform.bounds();
        self.curve_view = Some([bounds.min(), bounds.max()]);
    }
}

/// The sample nearest `t`, for a hover that has to answer for the whole trace.
fn nearest(samples: &[Sample], t: f64) -> Option<&Sample> {
    let index = samples.partition_point(|s| s.t < t);
    let before = index.checked_sub(1).and_then(|i| samples.get(i));
    let after = samples.get(index);
    match (before, after) {
        (Some(a), Some(b)) => Some(if (t - a.t).abs() <= (b.t - t).abs() { a } else { b }),
        (Some(a), None) => Some(a),
        (None, after) => after,
    }
}

/// Everything worth saying about one measurement, in the pane's units.
fn describe(t_unit: TempUnit, p_unit: PressureUnit, state: &n2o::State) -> String {
    let (t_label, p_label) = (t_unit.label(), p_unit.label());
    let mut out = format!(
        "{}\nT {:.2} {t_label}   P {:.2} {p_label}",
        state.phase.label(),
        t_unit.from_kelvin(state.t_k),
        p_unit.from_kpa(state.p_kpa),
    );
    match (state.psat_kpa, state.margin_kpa) {
        (Some(psat), Some(margin)) => out.push_str(&format!(
            "\nvapour pressure at T: {:.2} {p_label}   ({:+.2} {p_label})",
            p_unit.from_kpa(psat),
            p_unit.from_kpa(margin),
        )),
        _ => out.push_str("\nno vapour pressure at this temperature"),
    }
    match (state.tsat_k, state.superheat_k) {
        (Some(tsat), Some(superheat)) => out.push_str(&format!(
            "\nboiling point at P: {:.2} {t_label}   ({:+.2} K superheat)",
            t_unit.from_kelvin(tsat),
            superheat,
        )),
        _ => out.push_str("\nno boiling point at this pressure"),
    }
    let (to_t, to_p) = state.to_critical();
    out.push_str(&format!(
        "\ncritical point: {:.1} K and {:.1} {p_label} away",
        to_t,
        p_unit.from_kpa(to_p),
    ));
    out
}

fn unit_combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    current: &mut T,
    all: &[T],
    label: fn(T) -> &'static str,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(*current))
        .width(60.0)
        .show_ui(ui, |ui| {
            for unit in all {
                ui.selectable_value(current, *unit, label(*unit));
            }
        });
}

/// A menu that picks one series out of every log in the project.
///
/// A combo box would be hopeless here -- a tlog carries hundreds of series --
/// so it is a filter box over a list, with the series that look like the
/// quantity being asked for floated to the top.
fn series_picker(
    ui: &mut egui::Ui,
    id: &str,
    project: &Project,
    current: &Option<SeriesRef>,
    filter: &mut String,
    temperature: bool,
    picked: &mut Option<SeriesRef>,
) {
    let label = match current {
        Some(r) => short_name(&r.series),
        None => "pick a series…".to_string(),
    };
    let hover = match current {
        Some(r) => r.series.clone(),
        None => String::new(),
    };

    // Scoring every series in the project means walking a few hundred names,
    // so it happens when the menu is open and not on every frame behind it.
    ui.push_id(id, |ui| ui.menu_button(label, |ui| {
        let mut candidates: Vec<(u8, SourceId, &TimeSeries, &str)> = Vec::new();
        for source in &project.sources {
            let SourceKind::Log(log) = &source.kind else {
                continue;
            };
            for series in &log.series {
                candidates.push((score(series, temperature), source.id, series, source.name.as_str()));
            }
        }
        // Best guesses first, then alphabetically, so an unfiltered list opens
        // on the handful of series that could plausibly be the answer.
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.name.cmp(&b.2.name)));
        let multi_source = project.sources.iter().filter(|s| matches!(s.kind, SourceKind::Log(_))).count() > 1;

        ui.set_min_width(360.0);
        ui.horizontal(|ui| {
            ui.label("filter");
            ui.text_edit_singleline(filter);
        });
        let needle = filter.to_lowercase();
        let matching: Vec<_> = candidates
            .iter()
            .filter(|(_, _, s, _)| needle.is_empty() || s.name.to_lowercase().contains(&needle))
            .collect();
        if matching.is_empty() {
            ui.weak("no matching series");
            return;
        }
        if matching.len() > PICKER_LIMIT {
            ui.weak(format!(
                "{} series match; showing the first {PICKER_LIMIT}",
                matching.len()
            ));
        }
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for (_, source_id, series, source_name) in matching.into_iter().take(PICKER_LIMIT) {
                let unit = series.unit.as_deref().unwrap_or("");
                let text = if multi_source {
                    format!("{}  [{unit}]  —  {source_name}", series.name)
                } else {
                    format!("{}  [{unit}]", series.name)
                };
                if ui.button(text).clicked() {
                    *picked = Some(SeriesRef {
                        source: *source_id,
                        series: series.name.clone(),
                    });
                    ui.close();
                }
            }
        });
    }))
    .inner
    .response
    .on_hover_text(hover);
}

/// Series names are long and their tail is the part that differs.
fn short_name(name: &str) -> String {
    const MAX: usize = 28;
    if name.chars().count() <= MAX {
        return name.to_string();
    }
    let tail: String = name.chars().skip(name.chars().count() - MAX + 1).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LogFormat, LogSource, Source};

    fn tank(temperature_c: impl Fn(f64) -> f64, pressure_bar: impl Fn(f64) -> f64) -> Project {
        let temperature: Vec<[f64; 2]> = (0..500).map(|i| [i as f64, temperature_c(i as f64)]).collect();
        let pressure: Vec<[f64; 2]> = (0..500).map(|i| [i as f64, pressure_bar(i as f64)]).collect();
        let mut project = Project::new();
        let id = project.alloc_id();
        project.sources.push(Source {
            id,
            name: "tank.tlog".to_string(),
            path: "tank.tlog".into(),
            offset_seconds: 0.0,
            color: egui::Color32::WHITE,
            enabled: true,
            kind: SourceKind::Log(LogSource {
                series: vec![
                    TimeSeries::from_points("PRESSURE_VESSEL[1].pressure1", pressure).with_unit(Some("bar".into())),
                    TimeSeries::from_points("PRESSURE_VESSEL[1].temperature1", temperature)
                        .with_unit(Some("°C".into())),
                ],
                format: LogFormat::Tlog,
                can: Default::default(),
            }),
        });
        project
    }

    fn spec_for(project: &Project) -> VaporSpec {
        let mut vapors = Vapors::default();
        let id = vapors.create(project);
        vapors.list.remove(vapors.list.iter().position(|v| v.id == id).unwrap())
    }

    #[test]
    fn temperature_units_round_trip_and_know_a_span_from_a_reading() {
        for unit in TempUnit::ALL {
            let back = unit.to_kelvin(unit.from_kelvin(293.15));
            assert!((back - 293.15).abs() < 1e-9, "{unit:?}");
        }
        assert_eq!(TempUnit::Celsius.to_kelvin(20.0), 293.15);
        assert!((TempUnit::Fahrenheit.to_kelvin(68.0) - 293.15).abs() < 1e-9);
        // 10 K of superheat is 10 °C but 18 °F -- the offset applies to a
        // reading, never to a difference.
        assert_eq!(TempUnit::Celsius.delta_from_kelvin(10.0), 10.0);
        assert_eq!(TempUnit::Fahrenheit.delta_from_kelvin(10.0), 18.0);
    }

    #[test]
    fn pressure_units_round_trip() {
        for unit in PressureUnit::ALL {
            let back = unit.to_kpa(unit.from_kpa(5052.6));
            assert!((back - 5052.6).abs() < 1e-9, "{unit:?}");
        }
        assert_eq!(PressureUnit::Bar.to_kpa(50.0), 5000.0);
        assert!((PressureUnit::Psi.to_kpa(1.0) - 6.894_757_293_168_361).abs() < 1e-12);
    }

    #[test]
    fn units_are_taken_from_what_the_series_declares() {
        assert_eq!(TempUnit::from_series_unit(Some("°C")), Some(TempUnit::Celsius));
        assert_eq!(TempUnit::from_series_unit(Some("K")), Some(TempUnit::Kelvin));
        assert_eq!(TempUnit::from_series_unit(Some("m/s")), None);
        assert_eq!(TempUnit::from_series_unit(None), None);
        assert_eq!(PressureUnit::from_series_unit(Some("kPa")), Some(PressureUnit::Kpa));
        assert_eq!(PressureUnit::from_series_unit(Some("bar")), Some(PressureUnit::Bar));
        assert_eq!(PressureUnit::from_series_unit(Some("psig")), Some(PressureUnit::Psi));
        assert_eq!(PressureUnit::from_series_unit(Some("rad")), None);
    }

    /// A gauge reading is a bar low, which at tank pressures is about the
    /// width of the saturated band -- so the switch has to actually move the
    /// point, and by exactly one atmosphere.
    #[test]
    fn a_gauge_reading_gets_an_atmosphere_added_to_it() {
        let mut spec = spec_for(&tank(|_| 20.0, |_| 50.0));
        assert_eq!(spec.p_unit, PressureUnit::Bar);
        assert_eq!(spec.absolute_kpa(50.0), 5000.0);
        spec.gauge = true;
        assert!((spec.absolute_kpa(50.0) - 5101.325).abs() < 1e-9);
    }

    #[test]
    fn the_pane_opens_on_the_pair_that_describe_the_same_vessel() {
        let project = tank(|_| 20.0, |_| 50.0);
        let spec = spec_for(&project);
        assert_eq!(
            spec.temperature.map(|r| r.series),
            Some("PRESSURE_VESSEL[1].temperature1".to_string())
        );
        assert_eq!(
            spec.pressure.map(|r| r.series),
            Some("PRESSURE_VESSEL[1].pressure1".to_string())
        );
        // ... and with its declared units, not the defaults.
        assert_eq!(spec.t_unit, TempUnit::Celsius);
        assert_eq!(spec.p_unit, PressureUnit::Bar);
    }

    /// Walking the pressure down through the vapour pressure at a fixed
    /// temperature has to read as liquid, then saturated, then vapour, in that
    /// order -- this is the whole point of the pane.
    #[test]
    fn a_tank_bled_down_passes_through_all_three_zones_in_order() {
        // Psat(20 C) is about 50.5 bar; sweep 60 -> 40 bar over 500 s.
        let project = tank(|_| 20.0, |t| 60.0 - t * 0.04);
        let spec = spec_for(&project);
        let prepared = spec.prepare(&project, (0.0, 500.0));
        assert!(prepared.problem.is_none(), "{:?}", prepared.problem);

        let phases: Vec<Phase> = prepared.samples.iter().map(|s| s.state.phase).collect();
        assert_eq!(phases.first(), Some(&Phase::Liquid));
        assert_eq!(phases.last(), Some(&Phase::Vapor));
        assert!(phases.contains(&Phase::Saturated));
        // No going back: each zone is a contiguous stretch.
        let order = |p: &Phase| match p {
            Phase::Liquid => 0,
            Phase::Saturated => 1,
            Phase::Vapor => 2,
            other => panic!("unexpected {other:?}"),
        };
        assert!(phases.windows(2).all(|w| order(&w[0]) <= order(&w[1])), "{phases:?}");

        // The margin crosses zero exactly where the phase turns over.
        let crossing = prepared
            .samples
            .iter()
            .position(|s| s.state.margin_kpa.is_some_and(|m| m < 0.0))
            .expect("the trace crosses the curve");
        assert_eq!(prepared.samples[crossing].state.phase, Phase::Saturated);
    }

    #[test]
    fn warming_past_the_critical_temperature_breaks_the_trace_into_runs() {
        // Two stretches under the critical temperature (36.4 C) with a
        // supercritical one in between.
        let project = tank(
            |t| if (200.0..300.0).contains(&t) { 40.0 } else { 20.0 },
            |_| 50.0,
        );
        let spec = spec_for(&project);
        let prepared = spec.prepare(&project, (0.0, 500.0));
        assert!(prepared.samples.iter().any(|s| s.state.phase == Phase::Supercritical));
        assert_eq!(spec.runs(&prepared.samples).len(), 2, "the supercritical stretch splits the zones");
    }

    #[test]
    fn a_window_past_the_end_of_the_data_keeps_only_the_edge_sample() {
        let project = tank(|_| 20.0, |_| 50.0);
        let spec = spec_for(&project);
        let prepared = spec.prepare(&project, (1e6, 1e6 + 10.0));
        // A range query deliberately hands back the point either side of the
        // window, so a trace still reaches the edge of the viewport. Out here
        // there is nothing else, and nothing is invented to fill the gap.
        assert!(prepared.samples.len() <= 1, "{} samples", prepared.samples.len());
        assert!(prepared.samples.iter().all(|s| s.t <= 499.0));
    }

    #[test]
    fn a_pane_with_nothing_picked_says_so_rather_than_drawing_an_empty_plot() {
        let project = tank(|_| 20.0, |_| 50.0);
        let mut spec = spec_for(&project);
        spec.temperature = None;
        let prepared = spec.prepare(&project, (0.0, 500.0));
        assert!(prepared.samples.is_empty());
        assert!(prepared.problem.is_some());

        // ... and the same when the series it names has been unloaded.
        let mut spec = spec_for(&project);
        spec.pressure = Some(SeriesRef {
            source: 99,
            series: "gone".to_string(),
        });
        assert!(spec.prepare(&project, (0.0, 500.0)).problem.is_some());
    }

    /// One series ends early; the pane must not carry its last value forward
    /// and report a phase for a time only the other series covers.
    #[test]
    fn samples_are_not_paired_beyond_where_the_other_series_ends() {
        let mut project = tank(|_| 20.0, |_| 50.0);
        let SourceKind::Log(log) = &mut project.sources[0].kind else {
            panic!("log source");
        };
        let short: Vec<[f64; 2]> = (0..100).map(|i| [i as f64, 20.0]).collect();
        log.series.retain(|s| !s.name.ends_with("temperature1"));
        log.series.push(TimeSeries::from_points("PRESSURE_VESSEL[1].temperature1", short).with_unit(Some("°C".into())));
        log.series.sort_by(|a, b| a.name.cmp(&b.name));

        let spec = spec_for(&project);
        let prepared = spec.prepare(&project, (0.0, 500.0));
        assert!(!prepared.samples.is_empty());
        let last = prepared.samples.last().unwrap().t;
        assert!(last <= 99.0, "paired out to {last}s, past the end of the temperature series");
    }

    #[test]
    fn the_hover_finds_the_sample_nearest_the_pointer() {
        let samples: Vec<Sample> = [0.0, 1.0, 2.0]
            .into_iter()
            .map(|t| Sample {
                t,
                state: n2o::state(293.15, 5000.0, 0.02),
            })
            .collect();
        assert_eq!(nearest(&samples, -5.0).map(|s| s.t), Some(0.0));
        assert_eq!(nearest(&samples, 0.4).map(|s| s.t), Some(0.0));
        assert_eq!(nearest(&samples, 0.6).map(|s| s.t), Some(1.0));
        assert_eq!(nearest(&samples, 99.0).map(|s| s.t), Some(2.0));
        assert!(nearest(&[], 0.0).is_none());
    }

    /// The pane's close button reports through `ui`'s return value, which
    /// `TreeBehavior` turns into a closed pane and `App` into this call. A
    /// pane that stayed in `Vapors` after its tile was dropped would be a
    /// leak with no way to reach it again.
    #[test]
    fn closing_a_pane_forgets_it() {
        let project = tank(|_| 20.0, |_| 50.0);
        let mut vapors = Vapors::default();
        let first = vapors.create(&project);
        let second = vapors.create(&project);
        vapors.close(first);
        assert!(vapors.get(first).is_none());
        assert!(vapors.get_mut(second).is_some(), "closing one pane must not touch the others");
        vapors.close(second);
        assert!(vapors.get(second).is_none());
        // Closing something already gone is what a double click on the button
        // amounts to, and must not panic.
        vapors.close(first);
    }

    #[test]
    fn a_pane_is_named_after_what_the_two_series_have_in_common() {
        assert_eq!(shared_group("CAN_SENSOR[5].slot0", "CAN_SENSOR[5].slot1"), Some("CAN_SENSOR[5]"));
        assert_eq!(shared_group("A.temp", "B.press"), None);
        assert_eq!(shared_group("bare", "other"), None);

        let project = tank(|_| 20.0, |_| 50.0);
        assert_eq!(spec_for(&project).title(), "N₂O · PRESSURE_VESSEL[1]");
    }
}
