//! The tank level pane: ten temperatures lining a cylinder, as a picture.
//!
//! Ported from `rapid-scope`'s live tank window, with one substitution that
//! changes everything about it: there is no "now". A live scope draws a strip
//! whose right edge is the present and which scrolls; here the horizontal axis
//! is the master timeline, so the pane shows exactly the window every other
//! pane is showing and moves with them. The playhead is a line through it
//! rather than the right-hand edge.
//!
//! # Why a picture and not ten graphs
//!
//! A tank with sensors up its wall is not really ten signals. It is one signal
//! with a height axis, and the thing worth seeing is *stratification* -- a warm
//! layer sitting on cold liquid, or a boil-off front travelling up the wall.
//! Ten strip charts stacked on top of each other cannot show that, because the
//! reader has to do the comparison between the boxes. So this draws the column
//! itself: height up the screen, temperature as colour, time to the right. A
//! front moving up the tank is a diagonal, a uniform warm-up is a horizontal
//! wash, and a single sensor drifting away from its neighbours is a stripe.
//!
//! # Why these temperatures map to these colours
//!
//! The fluid is nitrous oxide, and for N₂O temperature *is* pressure: the tank
//! is self-pressurising, so the number that matters operationally is the
//! saturation pressure the warmest liquid is holding the vessel at. The ramp is
//! anchored on that:
//!
//! - **−20 °C**, the default cold end: a chilled or freshly filled tank, about
//!   19 bar. Colder than this is off the bottom of any normal fill.
//! - **~10 °C**, the neutral middle of the ramp: about 41 bar.
//! - **+20 °C**, ambient: 50.6 bar, and already two thirds of the way up the
//!   ramp -- which is the honest picture, because ambient N₂O is not a relaxed
//!   state.
//! - **+36.37 °C**, the critical point ([`crate::n2o::CRITICAL_T_K`]): 72.45
//!   bar, and there is no liquid above it. Past here the contents are
//!   supercritical, the pressure stops being set by a phase equilibrium and
//!   starts climbing with whatever the vessel is given. It sits just inside the
//!   default hot end, marked on the colour bar, and any sensor reading past it
//!   is called out in the readout.
//!
//! Both ends are adjustable, because a cold-flow test and a soak test want
//! different contrast, but the default range is chosen so that "red" means
//! "approaching the critical point" rather than merely "warm".
//!
//! # Units
//!
//! Everything below the [`TankSpec::unit`] setting is in °C: the field, the
//! ramp ends, the readouts and the legend. That setting says what unit the
//! *series* are in, so a log recording kelvin is read correctly rather than
//! painted 273 K too cold; it is not a display unit. The ramp is defined by
//! N₂O's own landmarks, which are °C numbers, so there is nothing to gain by
//! restating them in three units and a conversion to get wrong.

use std::collections::HashMap;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke};

use crate::model::{Project, SourceId, SourceKind};
use crate::n2o;
use crate::series::TimeSeries;
use crate::timeline::Timeline;
use crate::vapor::{SeriesRef, TempUnit};

/// How to read a number off a picked series.
///
/// Three of these are the units a series can declare. The fourth is the case
/// that made rapid-scope grow an "assume °C" switch and that the example logs
/// are full of: an IO board only announces what its sensor slots measure every
/// five seconds, so a slot heard before that first announcement comes through
/// the importer as bare counts -- which for those boards are hundredths of a
/// degree. Reading them as °C would put an ordinary tank at 2000 °C.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SensorUnit {
    Celsius,
    /// Raw counts, a hundredth of a degree each.
    CentiCelsius,
    Kelvin,
    Fahrenheit,
}

impl SensorUnit {
    pub const ALL: &'static [Self] = &[Self::Celsius, Self::CentiCelsius, Self::Kelvin, Self::Fahrenheit];

    pub fn label(self) -> &'static str {
        match self {
            Self::Celsius => "°C",
            Self::CentiCelsius => "c°C",
            Self::Kelvin => "K",
            Self::Fahrenheit => "°F",
        }
    }

    /// A reading off the series, in °C -- which is what everything downstream
    /// of here is in.
    pub fn to_celsius(self, value: f64) -> f64 {
        match self {
            Self::Celsius => value,
            Self::CentiCelsius => value / 100.0,
            Self::Kelvin => value - 273.15,
            Self::Fahrenheit => (value - 32.0) / 1.8,
        }
    }

    /// What a series declaring `unit` is in: the declared one where it is a
    /// temperature, and centi-°C where it declares nothing, since that is the
    /// unannounced sensor slot rather than a genuinely unitless number.
    pub fn from_series_unit(unit: Option<&str>) -> Option<Self> {
        if is_unitless(unit) {
            return Some(Self::CentiCelsius);
        }
        match TempUnit::from_series_unit(unit)? {
            TempUnit::Celsius => Some(Self::Celsius),
            TempUnit::Kelvin => Some(Self::Kelvin),
            TempUnit::Fahrenheit => Some(Self::Fahrenheit),
        }
    }
}

/// Whether a series declares no unit at all. `counts` is what
/// [`crate::can::iocan`] calls a slot whose node has not said what it is
/// measuring; an empty or absent unit is the same situation elsewhere.
fn is_unitless(unit: Option<&str>) -> bool {
    match unit.map(str::trim) {
        None | Some("") => true,
        Some(u) => {
            let u = u.to_lowercase();
            u == "counts" || u == "raw"
        }
    }
}

/// Units that are certainly not a temperature. A slot declaring pressure is
/// not a wall sensor at any price: painting a bar reading as a temperature is
/// the mistake this pane can make that still looks like a plausible tank.
const NOT_A_TEMPERATURE: &[&str] = &["bar", "mbar", "kpa", "hpa", "pa", "psi", "n", "kg", "v", "a", "%"];

/// How much a series looks like one of the temperatures up a tank wall.
///
/// A declared temperature unit is evidence, a name containing "temp" is a
/// guess, and a series declaring nothing is the weak candidate above -- worth
/// offering, never worth preferring over a series that says what it is.
fn sensor_score(series: &TimeSeries) -> u8 {
    let unit = series.unit.as_deref();
    let lowered = unit.map(|u| u.trim().to_lowercase());
    if lowered.as_deref().is_some_and(|u| NOT_A_TEMPERATURE.contains(&u)) {
        return 0;
    }
    let by_unit = TempUnit::from_series_unit(unit).is_some();
    let by_name = series.name.to_lowercase().contains("temp");
    match (by_unit, by_name) {
        (true, true) => 4,
        (true, false) => 3,
        (false, true) => 2,
        (false, false) => u8::from(is_unitless(unit)),
    }
}

pub type TankId = u64;

/// Sensors lining the tank wall, bottom first. Sensor 0 is the bottom of the
/// tank and sensor 9 the top.
pub const TANK_SENSORS: usize = 10;

/// The critical point in °C -- above it there is no liquid N₂O, so it is the
/// end of the pressure-follows-temperature regime rather than just a hot
/// reading.
pub const CRITICAL_C: f64 = n2o::CRITICAL_T_K - 273.15;

/// How long one sample stands for, by default. The IO boards broadcast their
/// sensor slots at 50 ms, so a column with nothing inside two seconds of it is
/// a real gap in the log and is drawn as one rather than smeared over.
const DEFAULT_HOLD_S: f64 = 2.0;

/// Same colour the graph and phase panes mark the playhead with.
const CURSOR_COLOR: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x3D);

/// Fewer sensors than this in a group and it is not a picture of a fluid
/// column, so the pane opens blank and asks rather than guessing wrong.
const MIN_GUESSED_SENSORS: usize = 5;

/// Room for the sensor names down the left of the cylinder.
const GUTTER_W: f32 = 26.0;
/// Room for the ten numbers beside it.
const READOUT_W: f32 = 116.0;
/// Room for the colour bar and its labels.
const LEGEND_W: f32 = 64.0;
/// Room for the time axis under it.
const AXIS_H: f32 = 20.0;

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// Blue through a neutral middle to red. Diverging rather than a single hue
/// ramp on purpose: the eye reads the crossing point, so a layer boundary in
/// the tank shows up as an edge instead of as a slow shade change.
///
/// The stops are chosen so that *warmth* -- red minus blue -- climbs all the
/// way along the ramp. Lightness cannot: a diverging map is bright in the
/// middle by construction. So warmth is the only thing carrying the ordering,
/// and a stop that broke it (a deep red darker in red than the orange before
/// it, say) would leave two different temperatures looking equally hot.
/// `the_ramp_runs_cold_to_hot_and_clamps` pins that.
const RAMP: [(f32, Color32); 5] = [
    (0.00, Color32::from_rgb(0x0A, 0x2C, 0xB4)),
    (0.25, Color32::from_rgb(0x4E, 0x92, 0xD8)),
    (0.50, Color32::from_rgb(0xE6, 0xE4, 0xDC)),
    (0.75, Color32::from_rgb(0xEC, 0x8C, 0x38)),
    (1.00, Color32::from_rgb(0xD8, 0x14, 0x18)),
];

/// Where `c` lands on the ramp, clamped at both ends. Out-of-range readings
/// keep the end colour rather than wrapping, so a disconnected probe reading
/// −327 °C is simply the coldest blue and not a red herring.
pub fn heat_colour(c: f64, lo: f64, hi: f64) -> Color32 {
    let span = (hi - lo).max(f64::EPSILON);
    let t = (((c - lo) / span) as f32).clamp(0.0, 1.0);
    for pair in RAMP.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if t <= t1 {
            let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
            return c0.lerp_to_gamma(c1, f);
        }
    }
    RAMP[RAMP.len() - 1].1
}

// ---------------------------------------------------------------------------
// Picking the ten series
// ---------------------------------------------------------------------------

/// A run of series that differ only by a trailing number -- which is what a
/// row of sensors looks like in every log this reads: `CAN_SENSOR[6].slot0`
/// through `slot9` from the IO boards, `tank_temp_1..10` from a sensor SQLite
/// log.
pub struct SensorGroup {
    pub source: SourceId,
    /// Everything the members share, up to and including the last non-digit:
    /// `"CAN_SENSOR[6].slot"`.
    pub stem: String,
    /// `(trailing number, series name)`, in ascending numeric order.
    pub members: Vec<(u32, String)>,
    /// Sum of how much each member looks like a temperature.
    score: u32,
}

impl SensorGroup {
    /// The ten heights this group fills, bottom first.
    ///
    /// The *number* in the name is the height, offset so the lowest-numbered
    /// member is the bottom of the tank -- `slot0..slot9` and `temp_1..temp_10`
    /// both line the wall. A number the row skips is a height left blank: a
    /// disconnected probe is a hole in the picture, and packing the ones that
    /// did report down into it would move every sensor above it to a height it
    /// was never at. That is the failure mode that still looks like a plausible
    /// tank, so it is the one pinned by a test.
    fn assign(&self) -> [Option<SeriesRef>; TANK_SENSORS] {
        let mut out: [Option<SeriesRef>; TANK_SENSORS] = Default::default();
        let Some((base, _)) = self.members.first() else {
            return out;
        };
        for (index, name) in &self.members {
            let Some(slot) = out.get_mut((index - base) as usize) else {
                continue;
            };
            *slot = Some(SeriesRef {
                source: self.source,
                series: name.clone(),
            });
        }
        out
    }

    /// How many of the ten heights this group would fill -- which is what
    /// makes it a better or worse candidate than another row, since members
    /// past the tenth height are dropped either way.
    fn fill(&self) -> usize {
        let Some((base, _)) = self.members.first() else {
            return 0;
        };
        self.members
            .iter()
            .filter(|(index, _)| (index - base) < TANK_SENSORS as u32)
            .count()
    }

    pub fn label(&self) -> String {
        format!("{}…  ({} of {TANK_SENSORS} heights)", self.stem, self.fill())
    }
}

/// Splits a series name into what it shares with its siblings and the number
/// that orders them: `"CAN_SENSOR[6].slot3"` -> `("CAN_SENSOR[6].slot", 3)`.
///
/// `None` when the name does not end in a number, which is every series that
/// is not one of a row.
fn indexed(name: &str) -> Option<(&str, u32)> {
    let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.is_empty() || stem.len() == name.len() {
        return None;
    }
    name[stem.len()..].parse().ok().map(|n| (stem, n))
}

/// Every run of numbered temperature-ish series in the project, best first.
///
/// "Best" is the number of sensors it would fill, then how strongly they look
/// like temperatures, then the name -- so the list is stable from one frame to
/// the next and the first entry is what the pane opens on.
pub fn sensor_groups(project: &Project) -> Vec<SensorGroup> {
    let mut by_stem: HashMap<(SourceId, &str), SensorGroup> = HashMap::new();
    for source in &project.sources {
        let SourceKind::Log(log) = &source.kind else {
            continue;
        };
        for series in &log.series {
            let score = sensor_score(series);
            if score == 0 {
                continue;
            }
            let Some((stem, index)) = indexed(&series.name) else {
                continue;
            };
            let group = by_stem
                .entry((source.id, stem))
                .or_insert_with(|| SensorGroup {
                    source: source.id,
                    stem: stem.to_string(),
                    members: Vec::new(),
                    score: 0,
                });
            group.members.push((index, series.name.clone()));
            group.score += u32::from(score);
        }
    }

    let mut groups: Vec<SensorGroup> = by_stem.into_values().collect();
    for group in &mut groups {
        group.members.sort_by_key(|(index, _)| *index);
    }
    groups.sort_by(|a, b| {
        b.fill()
            .cmp(&a.fill())
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.stem.cmp(&b.stem))
    });
    groups
}

fn series_of<'p>(project: &'p Project, r: &SeriesRef) -> Option<(&'p TimeSeries, f64)> {
    let source = project.source(r.source)?;
    let SourceKind::Log(log) = &source.kind else {
        return None;
    };
    let series = log.series.iter().find(|s| s.name == r.series)?;
    Some((series, source.offset_seconds))
}

// ---------------------------------------------------------------------------
// The pane's state
// ---------------------------------------------------------------------------

pub struct TankSpec {
    pub id: TankId,
    /// The wall, bottom sensor first.
    pub sensors: [Option<SeriesRef>; TANK_SENSORS],
    /// The unit the series report in -- not a display unit; see the module
    /// docs.
    pub unit: SensorUnit,
    /// Ends of the colour ramp, in °C.
    pub lo_c: f64,
    pub hi_c: f64,
    /// A line at each sensor's height, or at each region boundary in blocks
    /// mode.
    pub grid: bool,
    /// Draw each sensor's region as one flat rectangle instead of shading
    /// between neighbours.
    pub blocks: bool,
    /// How long one sample stands for before the strip shows a hole.
    pub hold_s: f64,
    /// Shared by the ten pickers, since only one of their menus is open at a
    /// time.
    filter: String,
    sensors_open: bool,
}

impl Default for TankSpec {
    fn default() -> Self {
        Self {
            id: 0,
            sensors: Default::default(),
            unit: SensorUnit::Celsius,
            // Cold enough for a chilled fill, hot enough to put the critical
            // point just inside the ramp. See the module docs for why these.
            lo_c: -20.0,
            hi_c: 40.0,
            grid: true,
            blocks: false,
            hold_s: DEFAULT_HOLD_S,
            filter: String::new(),
            sensors_open: false,
        }
    }
}

impl TankSpec {
    pub fn title(&self) -> String {
        match self.stem() {
            Some(stem) => format!("Tank · {}", stem.trim_end_matches(['.', '_', '-'])),
            None => "Tank level".to_string(),
        }
    }

    /// What the picked series share, when they are one row of sensors.
    fn stem(&self) -> Option<&str> {
        let mut stem: Option<&str> = None;
        for r in self.sensors.iter().flatten() {
            let (s, _) = indexed(&r.series)?;
            match stem {
                Some(previous) if previous != s => return None,
                _ => stem = Some(s),
            }
        }
        stem
    }

    /// Whether this pane draws anything belonging to `source`.
    pub fn uses(&self, source: SourceId) -> bool {
        self.sensors.iter().flatten().any(|r| r.source == source)
    }

    /// Forgets series from a source that is being unloaded. The pane stays --
    /// it can be pointed at another log.
    pub fn forget_source(&mut self, source: SourceId) {
        for slot in &mut self.sensors {
            slot.take_if(|r| r.source == source);
        }
    }

    pub fn picked(&self) -> usize {
        self.sensors.iter().flatten().count()
    }

    /// Opens the by-hand height pickers, which is what the pane does for
    /// itself when it could not guess a row.
    pub fn open_sensor_pickers(&mut self) {
        self.sensors_open = true;
    }

    /// A reading off one of these series, in °C.
    fn celsius(&self, value: f64) -> f64 {
        self.unit.to_celsius(value)
    }
}

/// Every tank pane the user has opened, and the only place their ids are
/// minted.
#[derive(Default)]
pub struct Tanks {
    list: Vec<TankSpec>,
    next_id: TankId,
}

impl Tanks {
    /// Every open pane, in the order they were opened.
    pub fn iter(&self) -> impl Iterator<Item = &TankSpec> {
        self.list.iter()
    }

    pub fn get(&self, id: TankId) -> Option<&TankSpec> {
        self.list.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TankId) -> Option<&mut TankSpec> {
        self.list.iter_mut().find(|t| t.id == id)
    }

    /// Opens a pane on the most likely row of sensors in the project.
    pub fn create(&mut self, project: &Project) -> TankId {
        let id = self.next_id;
        self.next_id += 1;

        let best = sensor_groups(project)
            .into_iter()
            .find(|g| g.fill() >= MIN_GUESSED_SENSORS);
        let sensors = best.as_ref().map(SensorGroup::assign).unwrap_or_default();
        // The row brings its own declared unit with it, which is nearly always
        // the right one -- including the "declares nothing, so it is counts"
        // case that the example logs' tank node is in.
        let unit = sensors
            .iter()
            .flatten()
            .find_map(|r| series_of(project, r).and_then(|(s, _)| SensorUnit::from_series_unit(s.unit.as_deref())))
            .unwrap_or(SensorUnit::Celsius);

        self.list.push(TankSpec {
            id,
            sensors,
            unit,
            sensors_open: best.is_none(),
            ..TankSpec::default()
        });
        id
    }

    pub fn close(&mut self, id: TankId) {
        self.list.retain(|t| t.id != id);
    }

    pub fn forget_source(&mut self, source: SourceId) {
        for spec in &mut self.list {
            spec.forget_source(source);
        }
    }
}

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

/// The picture as numbers: `cols` moments across the visible window, each
/// holding one temperature per sensor in °C, bottom sensor first. `NaN` where
/// that sensor had said nothing recently enough to stand for that moment.
///
/// Column 0 is the left edge (`view_start`) and column `cols - 1` the right.
pub struct Field {
    pub cols: usize,
    cells: Vec<f64>,
}

impl Field {
    pub fn at(&self, col: usize, sensor: usize) -> f64 {
        self.cells[col * TANK_SENSORS + sensor]
    }

    /// True when nothing at all landed in the window -- series that are gone,
    /// or a window the log does not cover.
    pub fn is_empty(&self) -> bool {
        self.cells.iter().all(|v| v.is_nan())
    }
}

impl TankSpec {
    /// Walks each sensor's samples in the window into the column grid.
    ///
    /// A column takes the last sample at or before its moment, which is what
    /// makes the picture a record of what was known at that time rather than
    /// an interpolation across a dropout.
    pub fn sample(&self, project: &Project, window: (f64, f64), cols: usize) -> Field {
        let cols = cols.max(2);
        let mut cells = vec![f64::NAN; cols * TANK_SENSORS];
        let step = (window.1 - window.0) / (cols - 1) as f64;
        // A column cannot resolve a gap narrower than itself, and zoomed out
        // the LOD slice is coarser than the log is -- so a fixed hold would
        // hollow the strip out at exactly the zoom where the whole run is on
        // screen. It grows with the column instead.
        let hold = self.hold_s.max(step);

        for (sensor, slot) in self.sensors.iter().enumerate() {
            let Some(r) = slot else { continue };
            let Some((series, offset)) = series_of(project, r) else {
                continue;
            };
            // Reaching back one hold means the first column can still be
            // filled by a sample from just before the window.
            let points = series.slice_for_range(window.0 - hold, window.1, offset, cols * 4);

            // Both sequences are sorted, so one pass covers all of them.
            let mut i = 0usize;
            let mut held: Option<[f64; 2]> = None;
            for col in 0..cols {
                let t = window.0 + step * col as f64;
                while i < points.len() && points[i][0] <= t {
                    held = Some(points[i]);
                    i += 1;
                }
                if let Some(point) = held
                    && t - point[0] <= hold
                    && !point[1].is_nan()
                {
                    cells[col * TANK_SENSORS + sensor] = self.celsius(point[1]);
                }
            }
        }

        Field { cols, cells }
    }

    /// What each sensor read at the playhead, in °C, for the numbers beside
    /// the tank. `None` where the series has nothing recent enough to answer
    /// with -- holding a value across a dropout would report a temperature
    /// nobody measured.
    pub fn readings_at(&self, project: &Project, t: f64) -> [Option<f64>; TANK_SENSORS] {
        std::array::from_fn(|sensor| {
            let r = self.sensors[sensor].as_ref()?;
            let (series, offset) = series_of(project, r)?;
            let point = series.last_at_or_before(t, offset)?;
            (t - point[0] <= self.hold_s).then(|| self.celsius(point[1]))
        })
    }
}

// ---------------------------------------------------------------------------
// The pane
// ---------------------------------------------------------------------------

impl TankSpec {
    /// Draws the pane. Returns `true` if the user asked to close it.
    pub fn ui(&mut self, ui: &mut egui::Ui, project: &Project, timeline: &mut Timeline) -> bool {
        let close = self.header(ui, project);
        let readings = self.readings_at(project, timeline.cursor);
        self.summary(ui, &readings);

        let rect = ui.available_rect_before_wrap();
        let Some(layout) = layout(rect) else {
            ui.weak("not enough room to draw the tank");
            return close;
        };

        // Roughly one column every two pixels, which is finer than anything
        // the eye takes off a strip and cheap enough to redo every frame -- so
        // no state has to be kept in step with the window.
        let cols = ((layout.body.width() / 2.0) as usize).clamp(2, 480);
        let window = (timeline.view_start, timeline.view_end);
        let field = self.sample(project, window, cols);

        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        self.draw(ui, layout, &field, &readings, window, timeline.cursor);
        self.interact(&response, layout, window, &field, timeline, project.time_bounds());
        close
    }

    /// Returns `true` if the user hit the close button.
    ///
    /// Like the phase pane and unlike a graph, this one is not made of series
    /// the sidebar can untick, so closing it has to be possible from here.
    fn header(&mut self, ui: &mut egui::Ui, project: &Project) -> bool {
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("sensors");
            let label = match self.stem() {
                Some(stem) => format!("{stem}…"),
                None if self.picked() == 0 => "pick a row…".to_string(),
                None => format!("{} picked", self.picked()),
            };
            ui.menu_button(label, |ui| {
                ui.set_min_width(280.0);
                let groups = sensor_groups(project);
                if groups.is_empty() {
                    ui.weak("no numbered temperature series in this project");
                }
                for group in &groups {
                    let name = project
                        .source(group.source)
                        .map(|s| s.name.as_str())
                        .unwrap_or("");
                    if ui.button(format!("{}  —  {name}", group.label())).clicked() {
                        self.sensors = group.assign();
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text(
                "Fill all ten heights from one row of numbered series, lowest number at the bottom \
                 of the tank. Individual heights can be overridden below.",
            );
            ui.toggle_value(&mut self.sensors_open, "⚙")
                .on_hover_text("Pick the series for each height by hand");

            ui.separator();
            ui.label("series unit");
            egui::ComboBox::from_id_salt(("tank_unit", self.id))
                .selected_text(self.unit.label())
                .width(60.0)
                .show_ui(ui, |ui| {
                    for unit in SensorUnit::ALL {
                        ui.selectable_value(&mut self.unit, *unit, unit.label());
                    }
                })
                .response
                .on_hover_text(
                    "What the picked series report in. The picture itself is always °C.\n\
                     c°C is raw counts at a hundredth of a degree each -- what an IO board's \
                     sensor slots carry before the node has announced what they measure.",
                );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.small_button("✖").on_hover_text("Close this pane").clicked();
            });
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("scale");
            ui.add(
                egui::DragValue::new(&mut self.lo_c)
                    .range(-100.0..=95.0)
                    .speed(0.5)
                    .suffix(" °C"),
            )
            .on_hover_text("Cold end of the colour ramp.");
            ui.add(
                egui::DragValue::new(&mut self.hi_c)
                    .range(self.lo_c + 5.0..=200.0)
                    .speed(0.5)
                    .suffix(" °C"),
            )
            .on_hover_text("Hot end of the colour ramp.");
            if ui
                .small_button("N₂O")
                .on_hover_text(
                    "Back to the default range: −20 °C (a chilled fill, ~19 bar) to +40 °C, which \
                     puts the 36.4 °C critical point just inside the hot end.",
                )
                .clicked()
            {
                let defaults = TankSpec::default();
                self.lo_c = defaults.lo_c;
                self.hi_c = defaults.hi_c;
            }
            // Dragging the low end past the high one is easier than it looks,
            // and a ramp that runs backwards paints cold as red.
            self.hi_c = self.hi_c.max(self.lo_c + 5.0);

            ui.separator();
            ui.toggle_value(&mut self.blocks, "Blocks").on_hover_text(
                "Drop the interpolation: draw each sensor's region as one flat rectangle of \
                 exactly what that sensor measured, with nothing blended between neighbours. The \
                 regions meet half way between sensors and do not overlap.",
            );
            ui.checkbox(&mut self.grid, "Grid").on_hover_text(
                "A line at each sensor's height, or at each region boundary in blocks mode.",
            );

            ui.separator();
            ui.label("hold");
            ui.add(
                egui::DragValue::new(&mut self.hold_s)
                    .range(0.05..=600.0)
                    .speed(0.1)
                    .suffix(" s"),
            )
            .on_hover_text(
                "How long one sample stands for. A sensor with nothing inside this much of a \
                 moment leaves a hole there rather than having its last value smeared across the \
                 gap. Zoomed out, one column's width wins when it is longer.",
            );
        });

        if self.sensors_open {
            self.sensor_pickers(ui, project);
        }
        close
    }

    /// The ten heights, top of the tank first -- the order they are drawn in,
    /// so the list reads like the picture.
    fn sensor_pickers(&mut self, ui: &mut egui::Ui, project: &Project) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .id_salt(("tank_sensors", self.id))
                .show(ui, |ui| {
                    for sensor in (0..TANK_SENSORS).rev() {
                        ui.horizontal(|ui| {
                            let name = match sensor {
                                0 => "s0 bottom".to_string(),
                                s if s == TANK_SENSORS - 1 => format!("s{s} top"),
                                s => format!("s{s}"),
                            };
                            ui.label(egui::RichText::new(name).monospace());
                            let mut picked = None;
                            crate::vapor::series_picker(
                                ui,
                                &format!("tank_{}_{sensor}", self.id),
                                project,
                                &self.sensors[sensor],
                                &mut self.filter,
                                true,
                                &mut picked,
                            );
                            if let Some(r) = picked {
                                self.sensors[sensor] = Some(r);
                            }
                            if self.sensors[sensor].is_some()
                                && ui.small_button("✖").on_hover_text("Leave this height blank").clicked()
                            {
                                self.sensors[sensor] = None;
                            }
                        });
                    }
                });
        });
    }

    /// The numbers the picture is a picture of, at the playhead.
    fn summary(&self, ui: &mut egui::Ui, readings: &[Option<f64>; TANK_SENSORS]) {
        let values: Vec<f64> = readings.iter().flatten().copied().collect();
        ui.horizontal_wrapped(|ui| {
            if values.is_empty() {
                if self.picked() == 0 {
                    ui.weak("Pick a row of temperature sensors above.");
                } else {
                    ui.weak("no reading at the playhead");
                }
                return;
            }
            let min = values.iter().copied().fold(f64::INFINITY, f64::min);
            let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            ui.label(
                egui::RichText::new(format!("min {min:.2} °C   mean {mean:.2} °C   max {max:.2} °C")).monospace(),
            );

            // Top minus bottom, which is the number a lined tank exists to
            // report: a warm layer sitting on cold liquid is the
            // stratification everything else here is a picture of.
            if let (Some(bottom), Some(top)) = (readings[0], readings[TANK_SENSORS - 1]) {
                ui.separator();
                ui.label(egui::RichText::new(format!("ΔT top−bottom {:+.2} K", top - bottom)).monospace())
                    .on_hover_text("Sensor 9 minus sensor 0.");
            }

            ui.separator();
            match n2o::psat_kpa(max + 273.15) {
                Some(kpa) => {
                    ui.label(egui::RichText::new(format!("P_sat {:.1} bar", kpa / 100.0)).monospace())
                        .on_hover_text(
                            "Saturation pressure of N₂O at the warmest sensor -- the pressure the \
                             vessel is being held at, if there is still liquid in it.",
                        );
                    ui.separator();
                    ui.weak(format!("{:.1} K below critical", CRITICAL_C - max));
                }
                None if max > CRITICAL_C => {
                    ui.colored_label(
                        Color32::from_rgb(0xE0, 0x6C, 0x6C),
                        egui::RichText::new("SUPERCRITICAL").strong(),
                    )
                    .on_hover_text(format!(
                        "A sensor reads above the {CRITICAL_C:.2} °C critical point. There is no \
                         liquid N₂O above it and no saturation pressure to quote: pressure is no \
                         longer set by the phase equilibrium."
                    ));
                }
                None => {
                    ui.weak("below the triple point");
                }
            }

            let blank = TANK_SENSORS - values.len();
            if blank > 0 {
                ui.separator();
                ui.weak(format!("{blank} of {TANK_SENSORS} heights have no reading here"));
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Layout, drawing and interaction
// ---------------------------------------------------------------------------

/// Where the cylinder sits inside the pane. Every rect below is arrived at by
/// subtracting fixed gutters from whatever the pane is, so it is worked out
/// once and refused when what is left is too small to draw in -- the
/// alternative is a negative width and a divide by zero somewhere further
/// down.
#[derive(Clone, Copy)]
struct Layout {
    /// The whole pane, which the painter is clipped to.
    rect: Rect,
    /// The data area: height against time, undistorted.
    body: Rect,
    /// Height of the dished ends, which live outside `body` so the vessel
    /// shape is drawn around the picture rather than through it.
    cap_h: f32,
}

fn layout(rect: Rect) -> Option<Layout> {
    let body_left = rect.left() + GUTTER_W;
    let body_right = rect.right() - READOUT_W - LEGEND_W;
    if body_right - body_left < 40.0 {
        return None;
    }
    let cap_h = ((body_right - body_left) * 0.06).clamp(8.0, 22.0);
    let body = Rect::from_min_max(
        Pos2::new(body_left, rect.top() + cap_h),
        Pos2::new(body_right, rect.bottom() - AXIS_H - cap_h),
    );
    (body.height() >= 40.0).then_some(Layout { rect, body, cap_h })
}

impl Layout {
    fn time_at(&self, x: f32, window: (f64, f64)) -> f64 {
        let f = ((x - self.body.left()) / self.body.width().max(1.0)) as f64;
        window.0 + f * (window.1 - window.0)
    }

    fn x_at(&self, t: f64, window: (f64, f64)) -> f32 {
        let span = window.1 - window.0;
        let f = if span.abs() < f64::EPSILON { 0.0 } else { (t - window.0) / span };
        self.body.left() + self.body.width() * f as f32
    }

    /// Which sensor's height `y` is nearest, for the hover readout.
    fn sensor_at(&self, y: f32) -> usize {
        let f = (self.body.bottom() - y) / self.body.height().max(1.0);
        ((f * (TANK_SENSORS - 1) as f32).round() as i32).clamp(0, TANK_SENSORS as i32 - 1) as usize
    }
}

/// The colours the painter reads off the theme, gathered once so the drawing
/// functions do not each need a `Ui`.
#[derive(Clone, Copy)]
struct Paint {
    dim: Color32,
    strong: Color32,
    outline: Stroke,
    grid: Stroke,
    hole: Color32,
    cap_fill: Color32,
}

impl TankSpec {
    fn draw(
        &self,
        ui: &egui::Ui,
        layout: Layout,
        field: &Field,
        readings: &[Option<f64>; TANK_SENSORS],
        window: (f64, f64),
        cursor: f64,
    ) {
        let visuals = ui.visuals();
        let paint = Paint {
            dim: visuals.weak_text_color(),
            strong: visuals.strong_text_color(),
            outline: Stroke::new(1.5, visuals.widgets.noninteractive.fg_stroke.color),
            grid: Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.35)),
            hole: visuals.extreme_bg_color,
            cap_fill: visuals.faint_bg_color,
        };
        let painter = ui.painter_at(layout.rect);
        let body = layout.body;

        // Anything with no data keeps this, so a gap reads as a hole rather
        // than as a temperature.
        painter.rect_filled(body, 0.0, paint.hole);
        let heat = if self.blocks {
            self.heat_blocks(body, field)
        } else {
            self.heat_mesh(body, field)
        };
        if !heat.is_empty() {
            painter.add(Shape::mesh(heat));
        }
        caps(&painter, body, layout.cap_h, paint);

        if self.grid {
            // In blocks mode the line belongs on the seam between two sensors'
            // regions, not through the middle of one: the point of that mode
            // is that a region is all one measurement, and a line down the
            // middle of it would suggest a division that is not there.
            let lines: Vec<f32> = if self.blocks {
                (1..TANK_SENSORS)
                    .map(|sensor| sensor_band(sensor, body.top(), body.bottom()).1)
                    .collect()
            } else {
                (0..TANK_SENSORS).map(|s| sensor_y(s, body)).collect()
            };
            for y in lines {
                painter.line_segment([Pos2::new(body.left(), y), Pos2::new(body.right(), y)], paint.grid);
            }
        }
        painter.rect_stroke(body, 0.0, paint.outline, egui::StrokeKind::Inside);

        // The playhead, in the colour every other pane marks it with. Unlike
        // the live scope this is what the numbers to the right are read at.
        if (window.0..=window.1).contains(&cursor) {
            let x = layout.x_at(cursor, window);
            painter.line_segment(
                [Pos2::new(x, body.top()), Pos2::new(x, body.bottom())],
                Stroke::new(1.5, CURSOR_COLOR),
            );
        }

        gutter(&painter, body, paint);
        time_axis(&painter, layout, window, paint);
        self.readouts(&painter, layout, readings, paint);
        self.legend(&painter, body, paint);
    }

    /// The temperature field as one mesh with a colour at every
    /// sensor-by-moment vertex, so the shading between sensors is
    /// interpolation the GPU does rather than a stack of banded rectangles.
    fn heat_mesh(&self, body: Rect, field: &Field) -> egui::Mesh {
        let x_of = |col: usize| body.left() + body.width() * col as f32 / (field.cols - 1) as f32;
        let mut mesh = egui::Mesh::default();
        for col in 0..field.cols {
            for sensor in 0..TANK_SENSORS {
                let c = field.at(col, sensor);
                let colour = if c.is_nan() {
                    Color32::TRANSPARENT
                } else {
                    heat_colour(c, self.lo_c, self.hi_c)
                };
                mesh.colored_vertex(Pos2::new(x_of(col), sensor_y(sensor, body)), colour);
            }
        }

        let index = |col: usize, sensor: usize| (col * TANK_SENSORS + sensor) as u32;
        for col in 0..field.cols.saturating_sub(1) {
            for sensor in 0..TANK_SENSORS - 1 {
                // A cell is drawn only when all four corners are real. Half a
                // cell would be a shape the data does not support.
                let corners = [
                    field.at(col, sensor),
                    field.at(col + 1, sensor),
                    field.at(col, sensor + 1),
                    field.at(col + 1, sensor + 1),
                ];
                if corners.iter().any(|v| v.is_nan()) {
                    continue;
                }
                let (a, b) = (index(col, sensor), index(col + 1, sensor));
                let (c, d) = (index(col + 1, sensor + 1), index(col, sensor + 1));
                mesh.add_triangle(a, b, c);
                mesh.add_triangle(a, c, d);
            }
        }
        mesh
    }

    /// The field with no interpolation at all: one flat rectangle per sensor
    /// per moment.
    ///
    /// The shaded picture is easier to read as a fluid, but every pixel
    /// between two sensors is a guess about a tank that may well be stratified
    /// in steps rather than smoothly. This mode gives up the smoothness to
    /// make each region exactly one number, which is what you want when
    /// reading a value off the screen.
    fn heat_blocks(&self, body: Rect, field: &Field) -> egui::Mesh {
        let mut mesh = egui::Mesh::default();
        for col in 0..field.cols {
            let (x0, x1) = column_cell(col, field.cols, body.left(), body.right());
            for sensor in 0..TANK_SENSORS {
                let c = field.at(col, sensor);
                if c.is_nan() {
                    continue;
                }
                let (top, bottom) = sensor_band(sensor, body.top(), body.bottom());
                mesh.add_colored_rect(
                    Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x1, bottom)),
                    heat_colour(c, self.lo_c, self.hi_c),
                );
            }
        }
        mesh
    }

    /// The ten numbers, each level with the height it was measured at -- which
    /// is the point of putting them here rather than in a table.
    fn readouts(
        &self,
        painter: &egui::Painter,
        layout: Layout,
        readings: &[Option<f64>; TANK_SENSORS],
        paint: Paint,
    ) {
        let body = layout.body;
        let x = body.right() + 12.0;
        painter.text(
            Pos2::new(x, body.top() - layout.cap_h),
            Align2::LEFT_TOP,
            "TOP",
            FontId::proportional(9.0),
            paint.dim,
        );
        painter.text(
            Pos2::new(x, body.bottom() + layout.cap_h),
            Align2::LEFT_BOTTOM,
            "BOTTOM",
            FontId::proportional(9.0),
            paint.dim,
        );

        for (sensor, reading) in readings.iter().enumerate() {
            let y = sensor_y(sensor, body);
            let swatch = Rect::from_min_size(Pos2::new(x, y - 5.0), egui::vec2(10.0, 10.0));
            match *reading {
                Some(c) => {
                    painter.rect_filled(swatch, 2.0, heat_colour(c, self.lo_c, self.hi_c));
                    // Past the critical point the reading stops meaning "warm"
                    // and starts meaning "there is no liquid left", so it is
                    // not left as one number among ten.
                    let colour = if c > CRITICAL_C {
                        Color32::from_rgb(0xE0, 0x6C, 0x6C)
                    } else {
                        paint.strong
                    };
                    painter.text(
                        Pos2::new(x + 16.0, y),
                        Align2::LEFT_CENTER,
                        format!("{c:>7.2} °C"),
                        FontId::monospace(11.0),
                        colour,
                    );
                }
                None => {
                    painter.rect_stroke(swatch, 2.0, Stroke::new(1.0, paint.dim), egui::StrokeKind::Inside);
                    painter.text(
                        Pos2::new(x + 16.0, y),
                        Align2::LEFT_CENTER,
                        "      — °C",
                        FontId::monospace(11.0),
                        paint.dim,
                    );
                }
            }
        }
    }

    /// The colour bar, with the critical point marked on it -- the one
    /// temperature on this scale that is a physical boundary rather than a
    /// preference.
    fn legend(&self, painter: &egui::Painter, body: Rect, paint: Paint) {
        let bar = Rect::from_min_max(
            Pos2::new(body.right() + READOUT_W + 8.0, body.top()),
            Pos2::new(body.right() + READOUT_W + 24.0, body.bottom()),
        );

        let mut mesh = egui::Mesh::default();
        const STEPS: usize = 32;
        for i in 0..=STEPS {
            let f = i as f64 / STEPS as f64;
            let c = self.lo_c + (self.hi_c - self.lo_c) * f;
            let y = bar.bottom() - bar.height() * f as f32;
            let colour = heat_colour(c, self.lo_c, self.hi_c);
            mesh.colored_vertex(Pos2::new(bar.left(), y), colour);
            mesh.colored_vertex(Pos2::new(bar.right(), y), colour);
            if i > 0 {
                let base = (i as u32 - 1) * 2;
                mesh.add_triangle(base, base + 1, base + 3);
                mesh.add_triangle(base, base + 3, base + 2);
            }
        }
        painter.add(Shape::mesh(mesh));
        painter.rect_stroke(bar, 0.0, paint.outline, egui::StrokeKind::Outside);

        for (label_c, at_top) in [(self.hi_c, true), (self.lo_c, false)] {
            painter.text(
                Pos2::new(bar.right() + 4.0, if at_top { bar.top() } else { bar.bottom() }),
                if at_top { Align2::LEFT_TOP } else { Align2::LEFT_BOTTOM },
                format!("{label_c:.0}"),
                FontId::monospace(10.0),
                paint.dim,
            );
        }

        if (self.lo_c..=self.hi_c).contains(&CRITICAL_C) {
            let f = (CRITICAL_C - self.lo_c) / (self.hi_c - self.lo_c);
            let y = bar.bottom() - bar.height() * f as f32;
            painter.line_segment(
                [Pos2::new(bar.left() - 3.0, y), Pos2::new(bar.right() + 3.0, y)],
                Stroke::new(1.5, paint.strong),
            );
            painter.text(
                Pos2::new(bar.right() + 4.0, y),
                Align2::LEFT_CENTER,
                "Tc",
                FontId::monospace(10.0),
                paint.strong,
            );
        }
    }

    /// Pan, zoom and seek, written back into the master timeline so the graphs
    /// and the video follow -- the same gestures a plot pane answers to, since
    /// this one is not an `egui_plot` and gets none of them for free.
    fn interact(
        &self,
        response: &egui::Response,
        layout: Layout,
        window: (f64, f64),
        field: &Field,
        timeline: &mut Timeline,
        bounds: Option<(f64, f64)>,
    ) {
        let seconds_per_px = (window.1 - window.0) / layout.body.width().max(1.0) as f64;

        if response.dragged() {
            let dx = response.drag_delta().x as f64 * seconds_per_px;
            timeline.set_view(window.0 - dx, window.1 - dx);
        }
        if let Some(pos) = response.hover_pos() {
            let scroll = response.ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                timeline.zoom_view((-scroll as f64 * 0.005).exp(), layout.time_at(pos.x, window));
            }
        }
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            timeline.seek(layout.time_at(pos.x, window), bounds);
            timeline.playing = false;
        }

        let Some(pos) = response.hover_pos() else {
            return;
        };
        let hint = if layout.body.contains(pos) {
            let sensor = layout.sensor_at(pos.y);
            let col = (((pos.x - layout.body.left()) / layout.body.width().max(1.0)
                * (field.cols - 1) as f32)
                .round() as i32)
                .clamp(0, field.cols as i32 - 1) as usize;
            let value = field.at(col, sensor);
            let reading = if value.is_nan() {
                "no reading".to_string()
            } else {
                format!("{value:.2} °C")
            };
            format!(
                "s{sensor}  {reading}\n{}",
                crate::timeline::format_utc(layout.time_at(pos.x, window))
            )
        } else {
            "Height up the screen, time to the right. Drag to pan, scroll to zoom, click to seek."
                .to_string()
        };
        response.clone().on_hover_text(hint);
    }
}

/// The screen height sensor `sensor` is measured at.
/// The height a sensor sits at, bottom first: sensor 0 is `body.bottom()` and
/// the last one `body.top()`.
///
/// Shared with the figure exporter (`crate::export`), which draws the same
/// picture at a different size -- the heights have to be the same division of
/// the vessel there or the exported tank would be a different tank.
pub(crate) fn sensor_y(sensor: usize, body: Rect) -> f32 {
    body.bottom() - body.height() * sensor as f32 / (TANK_SENSORS - 1) as f32
}

/// The height one sensor owns when the picture is drawn as blocks: half way to
/// each neighbour, and out to the wall at the two ends. Returned as
/// `(top, bottom)` in screen coordinates, so `top` is the smaller number.
///
/// Half way is the only division that needs no assumption about the fluid.
/// Together the bands tile the vessel exactly -- no overlap, no seam -- which
/// is the property `the_bands_tile_the_tank_exactly` holds them to and the
/// whole reason this mode exists.
pub(crate) fn sensor_band(sensor: usize, top: f32, bottom: f32) -> (f32, f32) {
    let y = |s: usize| bottom - (bottom - top) * s as f32 / (TANK_SENSORS - 1) as f32;
    let upper = if sensor + 1 >= TANK_SENSORS {
        top
    } else {
        (y(sensor) + y(sensor + 1)) / 2.0
    };
    let lower = if sensor == 0 {
        bottom
    } else {
        (y(sensor) + y(sensor - 1)) / 2.0
    };
    (upper, lower)
}

/// The same division along the time axis: a sample stands for the strip from
/// half way back to the one before it to half way on to the one after. The
/// first and last columns therefore reach the edges of the window they are the
/// edges of.
pub(crate) fn column_cell(col: usize, cols: usize, left: f32, right: f32) -> (f32, f32) {
    let x = |c: usize| left + (right - left) * c as f32 / (cols - 1).max(1) as f32;
    let start = if col == 0 { left } else { (x(col) + x(col - 1)) / 2.0 };
    let end = if col + 1 >= cols { right } else { (x(col) + x(col + 1)) / 2.0 };
    (start, end)
}

/// Dished ends, which is what makes the strip read as a pressure vessel seen
/// from the side rather than as a bare heat map.
fn caps(painter: &egui::Painter, body: Rect, cap_h: f32, paint: Paint) {
    const STEPS: usize = 40;
    let cx = body.center().x;
    let rx = body.width() / 2.0;

    for (edge, dir) in [(body.top(), -1.0_f32), (body.bottom(), 1.0_f32)] {
        let points: Vec<Pos2> = (0..=STEPS)
            .map(|i| {
                let theta = std::f32::consts::PI * i as f32 / STEPS as f32;
                Pos2::new(cx - rx * theta.cos(), edge + dir * cap_h * theta.sin())
            })
            .collect();
        painter.add(Shape::convex_polygon(points, paint.cap_fill, paint.outline));
    }
}

/// `s9` at the top down to `s0` at the bottom, against the heights they stand
/// for.
fn gutter(painter: &egui::Painter, body: Rect, paint: Paint) {
    for sensor in 0..TANK_SENSORS {
        painter.text(
            Pos2::new(body.left() - 4.0, sensor_y(sensor, body)),
            Align2::RIGHT_CENTER,
            format!("s{sensor}"),
            FontId::monospace(10.0),
            paint.dim,
        );
    }
}

/// Absolute times, formatted the same way the plot panes' x axis is -- this
/// window is the master one, so the two have to read alike.
fn time_axis(painter: &egui::Painter, layout: Layout, window: (f64, f64), paint: Paint) {
    let body = layout.body;
    let y = body.bottom() + layout.cap_h + 4.0;
    const TICKS: usize = 4;
    let step = (window.1 - window.0) / TICKS as f64;
    for i in 0..=TICKS {
        let f = i as f32 / TICKS as f32;
        let x = body.left() + body.width() * f;
        let t = window.0 + step * i as f64;
        let align = match i {
            0 => Align2::LEFT_TOP,
            TICKS => Align2::RIGHT_TOP,
            _ => Align2::CENTER_TOP,
        };
        painter.text(
            Pos2::new(x, y),
            align,
            crate::timeline::format_axis_time(t, step),
            FontId::proportional(10.0),
            paint.dim,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::can::CanFrames;
    use crate::model::{LogFormat, LogSource, Source};

    /// A log whose series are `name`d and sampled every 0.1 s from `t0`.
    fn log(project: &mut Project, name: &str, series: Vec<TimeSeries>) -> SourceId {
        let id = project.alloc_id();
        project.sources.push(Source {
            id,
            name: name.to_string(),
            path: name.into(),
            offset_seconds: 0.0,
            color: Color32::WHITE,
            enabled: true,
            kind: SourceKind::Log(LogSource {
                series,
                format: LogFormat::Tlog,
                can: CanFrames::default(),
            }),
        });
        id
    }

    fn flat(name: &str, value: f64, unit: &str) -> TimeSeries {
        let points = (0..=100).map(|i| [i as f64 * 0.1, value]).collect();
        TimeSeries::from_points(name, points).with_unit(Some(unit.to_string()))
    }

    /// Ten sensor slots off one IO board, plus the kind of company they keep in
    /// a real log: a pressure row that must not be mistaken for the wall, and
    /// a lone temperature that is not one of a row at all.
    fn project_with_a_lined_tank() -> Project {
        let mut project = Project::new();
        let mut series: Vec<TimeSeries> = (0..TANK_SENSORS)
            .map(|s| flat(&format!("CAN_SENSOR[6].slot{s}"), s as f64, "°C"))
            .collect();
        for s in 0..4 {
            series.push(flat(&format!("CAN_SENSOR[7].slot{s}"), 40.0, "bar"));
        }
        series.push(flat("PRESSURE_VESSEL[1].temperature1", 20.0, "°C"));
        series.sort_by(|a, b| a.name.cmp(&b.name));
        log(&mut project, "run.tlog", series);
        project
    }

    /// The mapping the whole pane rests on: a row of numbered series becomes
    /// ten heights in numeric order, lowest at the bottom. Get this wrong and
    /// the picture is upside down or shuffled, which is exactly the sort of
    /// wrong that still looks like a plausible tank.
    #[test]
    fn a_numbered_row_becomes_the_wall_bottom_first() {
        let project = project_with_a_lined_tank();
        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        let spec = tanks.get(id).expect("the pane exists");

        for sensor in 0..TANK_SENSORS {
            assert_eq!(
                spec.sensors[sensor].as_ref().map(|r| r.series.as_str()),
                Some(format!("CAN_SENSOR[6].slot{sensor}").as_str()),
                "height {sensor} is not reading its own series"
            );
        }
        // `slot10` would sort before `slot2` as text, so the order has to come
        // off the number and not the name.
        assert_eq!(indexed("CAN_SENSOR[6].slot10"), Some(("CAN_SENSOR[6].slot", 10)));
        assert_eq!(indexed("tank_temp_3"), Some(("tank_temp_", 3)));
        assert_eq!(indexed("PRESSURE_VESSEL[1]"), None);
        assert_eq!(indexed("42"), None);
    }

    /// A probe that never reported leaves its height blank. Packing the ones
    /// that did report down into the hole would put every sensor above it at a
    /// height it was never at -- the sort of wrong that still looks like a
    /// plausible tank.
    #[test]
    fn a_missing_sensor_leaves_its_height_blank() {
        let mut project = Project::new();
        let series = (0..TANK_SENSORS)
            .filter(|s| *s != 4)
            .map(|s| flat(&format!("CAN_SENSOR[6].slot{s}"), s as f64, "°C"))
            .collect();
        log(&mut project, "run.tlog", series);

        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        let spec = tanks.get(id).unwrap();
        assert_eq!(spec.sensors[4], None, "the dead probe's height stays blank");
        for sensor in (0..TANK_SENSORS).filter(|s| *s != 4) {
            assert_eq!(
                spec.sensors[sensor].as_ref().map(|r| r.series.as_str()),
                Some(format!("CAN_SENSOR[6].slot{sensor}").as_str()),
            );
        }
    }

    /// The number is a height, not an index into the list: a row numbered from
    /// one lines the same wall as a row numbered from zero.
    #[test]
    fn a_row_numbered_from_one_still_starts_at_the_bottom() {
        let mut project = Project::new();
        let series = (1..=TANK_SENSORS)
            .map(|s| flat(&format!("tank_temp_{s}"), s as f64, "°C"))
            .collect();
        log(&mut project, "log.sqlite", series);

        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        let spec = tanks.get(id).unwrap();
        assert_eq!(
            spec.sensors[0].as_ref().map(|r| r.series.as_str()),
            Some("tank_temp_1")
        );
        assert_eq!(
            spec.sensors[TANK_SENSORS - 1].as_ref().map(|r| r.series.as_str()),
            Some("tank_temp_10")
        );
    }

    /// The bar row is longer than nothing and would win on member count alone;
    /// it must not, because it is not a temperature.
    #[test]
    fn a_row_that_is_not_temperatures_is_not_offered() {
        let project = project_with_a_lined_tank();
        let groups = sensor_groups(&project);
        assert!(
            groups.iter().all(|g| g.stem != "CAN_SENSOR[7].slot"),
            "a row declaring bar is not a tank wall"
        );
        assert_eq!(groups.first().map(|g| g.stem.as_str()), Some("CAN_SENSOR[6].slot"));
    }

    /// Ten unrelated temperatures are not a wall. Guessing one out of them
    /// would draw a picture of nothing, so the pane opens blank instead.
    #[test]
    fn a_project_with_no_row_of_sensors_opens_blank() {
        let mut project = Project::new();
        log(
            &mut project,
            "run.tlog",
            vec![flat("PRESSURE_VESSEL[1].temperature1", 20.0, "°C")],
        );
        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        assert_eq!(tanks.get(id).unwrap().picked(), 0);
        assert_eq!(tanks.get(id).unwrap().title(), "Tank level");
    }

    /// A source being unloaded takes its series with it and leaves the pane.
    #[test]
    fn unloading_a_source_leaves_the_pane_pointing_at_nothing() {
        let project = project_with_a_lined_tank();
        let source = project.sources[0].id;
        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        assert!(tanks.get(id).unwrap().uses(source));
        tanks.forget_source(source);
        assert_eq!(tanks.get(id).unwrap().picked(), 0);
        assert!(!tanks.get(id).unwrap().uses(source));
    }

    /// Each height reads its own series, and the field is indexed the way the
    /// picture is drawn.
    #[test]
    fn every_height_reads_its_own_series() {
        let project = project_with_a_lined_tank();
        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        let spec = tanks.get(id).unwrap();

        let field = spec.sample(&project, (0.0, 10.0), 8);
        for sensor in 0..TANK_SENSORS {
            assert_eq!(field.at(field.cols - 1, sensor), sensor as f64);
            assert_eq!(field.at(0, sensor), sensor as f64);
        }
        assert!(!field.is_empty());
    }

    /// A gap in the log is a hole in the picture. Holding the last value
    /// across a dropout would paint a tank that was never measured -- and a
    /// window that starts before the log does must stay empty rather than be
    /// back-filled with the first sample.
    #[test]
    fn a_dropout_is_a_hole_not_a_held_value() {
        let mut project = Project::new();
        let mut points: Vec<[f64; 2]> = (0..=100).map(|i| [i as f64 * 0.1, 10.0]).collect();
        points.extend((0..=100).map(|i| [20.0 + i as f64 * 0.1, 30.0]));
        let source = log(
            &mut project,
            "run.tlog",
            vec![TimeSeries::from_points("t_0", points).with_unit(Some("°C".into()))],
        );

        let mut spec = TankSpec {
            id: 0,
            ..TankSpec::default()
        };
        spec.sensors[0] = Some(SeriesRef {
            source,
            series: "t_0".to_string(),
        });

        // 31 columns over 30 s: one a second, so the 2 s hold is what decides.
        let field = spec.sample(&project, (0.0, 30.0), 31);
        assert_eq!(field.at(10, 0), 10.0, "the last sample stands briefly");
        assert!(field.at(15, 0).is_nan(), "a gap of more than the hold is a gap");
        assert_eq!(field.at(30, 0), 30.0, "and it comes back cleanly");

        // A window reaching back before the log starts: those columns are
        // empty rather than back-filled with the first sample, which would
        // invent a flat history.
        let before = spec.sample(&project, (-30.0, 10.0), 41);
        assert!(before.at(0, 0).is_nan(), "the empty past must stay empty");
        assert!(before.at(20, 0).is_nan());
        assert_eq!(before.at(40, 0), 10.0, "and the columns the log covers are filled");

        // The readout at the playhead answers the same way.
        assert_eq!(spec.readings_at(&project, 10.5)[0], Some(10.0));
        assert_eq!(spec.readings_at(&project, 15.0)[0], None);
        assert_eq!(spec.readings_at(&project, -1.0)[0], None);
    }

    /// Zoomed out, a column is wider than the hold and the LOD slice is
    /// coarser than the log -- so a fixed hold would hollow the strip out at
    /// exactly the zoom that shows the whole run.
    #[test]
    fn the_hold_grows_with_the_column_so_a_wide_view_is_not_hollow() {
        let mut project = Project::new();
        let points: Vec<[f64; 2]> = (0..500_000).map(|i| [i as f64 * 0.01, 20.0]).collect();
        let source = log(
            &mut project,
            "big.tlog",
            vec![TimeSeries::from_points("t_0", points).with_unit(Some("°C".into()))],
        );
        let mut spec = TankSpec::default();
        spec.sensors[0] = Some(SeriesRef {
            source,
            series: "t_0".to_string(),
        });

        // 5000 s of log across 200 columns: 25 s a column, against a 2 s hold.
        let field = spec.sample(&project, (0.0, 5000.0), 200);
        for col in 1..200 {
            assert_eq!(field.at(col, 0), 20.0, "column {col} went hollow");
        }
    }

    /// A series in kelvin is read as kelvin. Painting 293 K as 293 °C would
    /// put a perfectly ordinary tank off the top of the ramp.
    #[test]
    fn the_series_unit_is_what_the_series_is_read_as() {
        let mut project = Project::new();
        let source = log(&mut project, "k.tlog", vec![flat("t_0", 293.15, "K")]);
        let mut spec = TankSpec {
            unit: SensorUnit::Kelvin,
            ..TankSpec::default()
        };
        spec.sensors[0] = Some(SeriesRef {
            source,
            series: "t_0".to_string(),
        });
        let reading = spec.readings_at(&project, 5.0)[0].expect("a reading");
        assert!((reading - 20.0).abs() < 1e-9, "293.15 K is 20 °C, got {reading}");

        // And the pane opens on the unit the series declares.
        let mut tanks = Tanks::default();
        let mut kelvin_wall = Project::new();
        let series = (0..TANK_SENSORS)
            .map(|s| flat(&format!("wall{s}"), 293.15, "K"))
            .collect();
        log(&mut kelvin_wall, "k.tlog", series);
        let id = tanks.create(&kelvin_wall);
        assert_eq!(tanks.get(id).unwrap().unit, SensorUnit::Kelvin);
    }

    /// The case the example logs are actually in: the tank node's twelve slots
    /// come through declaring nothing, because the node had not announced what
    /// they measure yet. They must still be found -- and read as hundredths of
    /// a degree, not as degrees.
    #[test]
    fn a_row_that_declares_nothing_is_read_as_counts() {
        let mut project = Project::new();
        let mut series: Vec<TimeSeries> = (0..12)
            .map(|s| flat(&format!("CAN_SENSOR[10].slot{s}"), 2000.0, "counts"))
            .collect();
        // The other boards on the bus, which do announce: one temperature
        // among a row of pressures. Neither is a wall.
        series.push(flat("CAN_SENSOR[5].slot0", 16.0, "°C"));
        for s in 1..5 {
            series.push(flat(&format!("CAN_SENSOR[5].slot{s}"), 40.0, "bar"));
        }
        series.sort_by(|a, b| a.name.cmp(&b.name));
        log(&mut project, "run.tlog", series);

        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        let spec = tanks.get(id).unwrap();
        assert_eq!(spec.unit, SensorUnit::CentiCelsius);
        assert_eq!(spec.picked(), TANK_SENSORS);
        assert_eq!(
            spec.sensors[0].as_ref().map(|r| r.series.as_str()),
            Some("CAN_SENSOR[10].slot0")
        );
        // Slots 10 and 11 are past the top of the tank and are dropped.
        assert_eq!(
            spec.sensors[TANK_SENSORS - 1].as_ref().map(|r| r.series.as_str()),
            Some("CAN_SENSOR[10].slot9")
        );
        assert_eq!(spec.readings_at(&project, 5.0)[0], Some(20.0), "2000 counts is 20 °C");

        // And a row of pressures is not offered as a wall at any price.
        let groups = sensor_groups(&project);
        assert_eq!(groups.first().map(|g| g.stem.as_str()), Some("CAN_SENSOR[10].slot"));
        assert!(groups.iter().all(|g| g.members.iter().all(|(_, n)| n != "CAN_SENSOR[5].slot1")));
    }

    /// A declared temperature outranks a row that merely declares nothing, so
    /// a log carrying both opens on the one that says what it is.
    #[test]
    fn a_declared_temperature_row_outranks_an_unannounced_one() {
        let mut project = Project::new();
        let mut series: Vec<TimeSeries> = (0..TANK_SENSORS)
            .map(|s| flat(&format!("CAN_SENSOR[10].slot{s}"), 2000.0, "counts"))
            .collect();
        series.extend((0..TANK_SENSORS).map(|s| flat(&format!("CAN_SENSOR[6].slot{s}"), 20.0, "°C")));
        series.sort_by(|a, b| a.name.cmp(&b.name));
        log(&mut project, "run.tlog", series);

        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        assert_eq!(tanks.get(id).unwrap().unit, SensorUnit::Celsius);
        assert_eq!(
            tanks.get(id).unwrap().sensors[0].as_ref().map(|r| r.series.as_str()),
            Some("CAN_SENSOR[6].slot0")
        );
    }

    /// The promise blocks mode makes: every point in the tank belongs to
    /// exactly one sensor. No overlap between regions, no seam between them,
    /// and the two end sensors reach the walls -- otherwise a reader measuring
    /// a colour off the picture cannot say which sensor it came from, which is
    /// the entire reason for the mode.
    #[test]
    fn the_bands_tile_the_tank_exactly() {
        let (top, bottom) = (10.0_f32, 410.0_f32);
        let bands: Vec<(f32, f32)> = (0..TANK_SENSORS).map(|s| sensor_band(s, top, bottom)).collect();

        assert_eq!(bands[0].1, bottom, "sensor 0 reaches the floor");
        assert_eq!(bands[TANK_SENSORS - 1].0, top, "the top sensor reaches the roof");
        for (sensor, band) in bands.iter().enumerate() {
            assert!(band.0 < band.1, "sensor {sensor} has no height");
        }
        for sensor in 1..TANK_SENSORS {
            assert_eq!(
                bands[sensor].1,
                bands[sensor - 1].0,
                "sensors {} and {sensor} do not meet cleanly",
                sensor - 1
            );
        }
        let body = Rect::from_min_max(Pos2::new(0.0, top), Pos2::new(100.0, bottom));
        for (sensor, band) in bands.iter().enumerate() {
            let y = sensor_y(sensor, body);
            assert!(band.0 <= y && y <= band.1, "sensor {sensor} is not in its band");
        }
    }

    /// The same for the time axis: the columns tile the window, and the two
    /// end columns reach the edges the window promises they are.
    #[test]
    fn the_columns_tile_the_window_exactly() {
        let (left, right) = (0.0_f32, 300.0_f32);
        let cols = 16;
        let cells: Vec<(f32, f32)> = (0..cols).map(|c| column_cell(c, cols, left, right)).collect();

        assert_eq!(cells[0].0, left);
        assert_eq!(cells[cols - 1].1, right);
        for col in 1..cols {
            assert_eq!(cells[col].0, cells[col - 1].1, "column {col} leaves a seam");
        }
    }

    /// Cold is blue, hot is red, and out-of-range clamps instead of wrapping --
    /// a probe reading nonsense must not come out the same colour as a warm
    /// tank.
    #[test]
    fn the_ramp_runs_cold_to_hot_and_clamps() {
        let (lo, hi) = (-20.0, 40.0);
        let cold = heat_colour(lo, lo, hi);
        let hot = heat_colour(hi, lo, hi);
        assert!(cold.b() > cold.r(), "the cold end is blue");
        assert!(hot.r() > hot.b(), "the hot end is red");
        assert_eq!(heat_colour(-300.0, lo, hi), cold);
        assert_eq!(heat_colour(300.0, lo, hi), hot);

        // Warmth climbs the whole way. A diverging ramp is brightest in the
        // middle, so lightness says nothing about ordering and this is the
        // only property that keeps two different temperatures from looking
        // equally hot.
        let mut last = i32::MIN;
        for step in 0..=60 {
            let c = lo + (hi - lo) * f64::from(step) / 60.0;
            let colour = heat_colour(c, lo, hi);
            let warmth = i32::from(colour.r()) - i32::from(colour.b());
            assert!(warmth >= last, "the ramp cooled off again at {c} °C");
            last = warmth;
        }
    }

    /// The picture itself: the mesh carries each sensor's own colour at its own
    /// height, cold at the bottom and warm at the top, and a moment a sensor
    /// said nothing in is a hole in the mesh rather than a triangle of some
    /// interpolated colour.
    #[test]
    fn the_mesh_paints_each_height_its_own_temperature() {
        let project = project_with_a_lined_tank();
        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        let spec = tanks.get(id).unwrap();
        let body = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 200.0));

        // The wall reads 0 °C at the bottom through 9 °C at the top.
        let field = spec.sample(&project, (0.0, 10.0), 4);
        let mesh = spec.heat_mesh(body, &field);
        assert_eq!(mesh.vertices.len(), 4 * TANK_SENSORS);
        // Every cell of a fully-reported wall is drawn: two triangles each.
        assert_eq!(mesh.indices.len(), (4 - 1) * (TANK_SENSORS - 1) * 6);

        let vertex = |col: usize, sensor: usize| mesh.vertices[col * TANK_SENSORS + sensor];
        assert_eq!(vertex(0, 0).pos.y, body.bottom(), "sensor 0 is the bottom of the tank");
        assert_eq!(vertex(0, TANK_SENSORS - 1).pos.y, body.top());
        assert_eq!(vertex(0, 0).color, heat_colour(0.0, spec.lo_c, spec.hi_c));
        assert_eq!(vertex(3, 9).color, heat_colour(9.0, spec.lo_c, spec.hi_c));
        let (bottom, top) = (vertex(0, 0).color, vertex(0, TANK_SENSORS - 1).color);
        assert!(
            i32::from(top.r()) - i32::from(top.b()) > i32::from(bottom.r()) - i32::from(bottom.b()),
            "the warm top must read warmer than the cold bottom"
        );

        // A window the log does not reach is a hole, not a colour.
        let empty = spec.sample(&project, (1e6, 1e6 + 10.0), 4);
        assert!(empty.is_empty());
        assert!(spec.heat_mesh(body, &empty).indices.is_empty());
        assert!(spec.heat_blocks(body, &empty).indices.is_empty());

        // Blocks mode paints the same wall as flat rectangles: two triangles
        // per sensor per column, and the bottom band reaches the floor.
        let blocks = spec.heat_blocks(body, &field);
        assert_eq!(blocks.indices.len(), 4 * TANK_SENSORS * 6);
        assert_eq!(blocks.vertices[0].color, heat_colour(0.0, spec.lo_c, spec.hi_c));
        assert!(blocks.vertices.iter().any(|v| v.pos.y == body.bottom()));
    }

    /// The critical point the ramp and the readout are anchored on is the one
    /// in [`crate::n2o`], not a second copy of it that could drift.
    #[test]
    fn the_critical_point_matches_the_saturation_table() {
        assert!((CRITICAL_C - 36.37).abs() < 0.01, "got {CRITICAL_C}");
        assert!(n2o::psat_kpa(CRITICAL_C + 273.15).is_some());
        assert!(n2o::psat_kpa(CRITICAL_C + 273.15 + 0.5).is_none());
        // The two numbers the default ramp is chosen around.
        let at_zero = n2o::psat_kpa(273.15).unwrap() / 100.0;
        let at_twenty = n2o::psat_kpa(293.15).unwrap() / 100.0;
        assert!((at_zero - 31.3).abs() < 0.3, "0 °C should be ~31.3 bar, got {at_zero}");
        assert!((at_twenty - 50.6).abs() < 0.3, "20 °C should be ~50.6 bar, got {at_twenty}");
    }

    /// Two columns is the narrowest grid there is, and the one the column
    /// spacing divides by.
    #[test]
    fn a_two_column_field_is_still_a_field() {
        let project = project_with_a_lined_tank();
        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        let field = tanks.get(id).unwrap().sample(&project, (0.0, 10.0), 1);
        assert_eq!(field.cols, 2);
        assert_eq!(field.at(1, 3), 3.0);
    }

    /// Every rect the painter uses is arrived at by subtracting fixed gutters
    /// from whatever the pane is, so a narrow pane is the case where a width
    /// goes negative and a `cols - 1` divides by zero. It has to refuse rather
    /// than draw.
    #[test]
    fn the_layout_refuses_a_pane_it_cannot_draw_in() {
        let big = layout(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 640.0))).expect("room");
        assert!(big.body.width() > 0.0 && big.body.height() > 0.0);
        assert!(big.body.right() + READOUT_W + LEGEND_W <= 900.0);
        assert!(layout(Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 640.0))).is_none());
        assert!(layout(Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 50.0))).is_none());
    }
}
