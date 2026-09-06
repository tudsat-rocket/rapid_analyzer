//! Exporting a graph as a picture, for putting in a written report.
//!
//! What comes out is deliberately *not* a screenshot. A screenshot is the
//! wrong resolution, the wrong colours for a white page, and carries the
//! sidebar and the tab bar with it. Instead the same series the panes draw are
//! laid out again ([`figure`]) into a figure the user gives a physical size, a
//! resolution and a time window, and written as either vector text
//! ([`svg`], the one to prefer -- it prints at the printer's resolution) or a
//! bitmap ([`raster`], for tools that will not take an SVG).
//!
//! The axes come from [`crate::panes::value_ranges`], the same function the
//! pane on screen uses, so an exported graph is the graph the user was looking
//! at rather than a second opinion about it.

pub mod dialog;
pub mod figure;
pub mod raster;
pub mod svg;
pub mod text;

use std::path::{Path, PathBuf};

use egui::{Color32, Vec2};

use crate::model::{Project, SourceKind};
use crate::panes::{AxisMap, PlotAxis, PlotId, PlotSpec, Plots};
use crate::tank::{TankId, TankSpec, Tanks};
use crate::timeline::Timeline;

pub use figure::LegendPos;

/// Figure units are CSS pixels: 96 to the inch, which is what an SVG
/// `viewBox` is measured in.
pub const UNITS_PER_INCH: f32 = 96.0;
pub const MM_PER_INCH: f32 = 25.4;

pub fn mm_to_units(mm: f32) -> f32 {
    mm / MM_PER_INCH * UNITS_PER_INCH
}

pub fn points_to_units(pt: f32) -> f32 {
    pt / 72.0 * UNITS_PER_INCH
}

pub fn mm_to_pixels(mm: f32, dpi: f32) -> f32 {
    mm / MM_PER_INCH * dpi
}

/// A pane the exporter can put in a figure.
///
/// Graphs and tank panes both draw against the master timeline, which is what
/// makes them stackable in one figure; the video and phase panes do not, and
/// are not offered.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ExportItem {
    Plot(PlotId),
    Tank(TankId),
}

impl ExportItem {
    /// What to call it in the picker and on the panel.
    pub fn title(self, plots: &Plots, tanks: &Tanks) -> String {
        match self {
            Self::Plot(id) => plots.get(id).map_or_else(|| "plot".to_string(), PlotSpec::title),
            Self::Tank(id) => tanks.get(id).map_or_else(|| "tank".to_string(), TankSpec::title),
        }
    }
}

/// Whether a time (or value) window is one anything can be drawn in.
///
/// Written as a predicate rather than a `>` at each call site because the
/// interesting case is the one a comparison gets wrong: a range that is NaN
/// -- an empty log, a bad offset -- is *not* drawable, and `!(a > b)` and
/// `a <= b` disagree about that.
pub fn is_drawable_range((lo, hi): (f64, f64)) -> bool {
    lo.is_finite() && hi.is_finite() && hi > lo
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportFormat {
    Svg,
    Png,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Png => "png",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Svg => "SVG (vector)",
            Self::Png => "PNG (bitmap)",
        }
    }
}

/// Which stretch of the timeline the figure covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RangeMode {
    /// Whatever the graphs are showing right now.
    View,
    /// Everything that is loaded.
    All,
    /// A window typed in by hand, as seconds from the start of the data.
    Custom,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportTheme {
    Light,
    Dark,
}

/// Everything the export dialog asks about.
#[derive(Clone, PartialEq, Debug)]
pub struct ExportSettings {
    pub format: ExportFormat,
    /// Figure width on the page.
    pub width_mm: f32,
    /// Height of *one* graph; a figure with three stacked is three times this.
    pub height_mm: f32,
    /// Pixels per inch. Sets the size of a PNG, and is written into it so a
    /// word processor places it at `width_mm` rather than at its own guess.
    pub dpi: f32,
    pub padding_mm: f32,
    pub font_pt: f32,
    pub line_width_pt: f32,
    pub theme: ExportTheme,
    pub transparent: bool,
    pub grid: bool,
    pub legend: LegendPos,
    pub titles: bool,
    pub cursor: bool,
    /// Ignore the value range a box zoom pinned, and fit the axes to what the
    /// exported window actually holds.
    pub auto_fit_y: bool,
    /// One file per graph instead of one figure with the graphs stacked.
    pub separate_files: bool,
    pub range: RangeMode,
    /// Seconds from the start of the data, for [`RangeMode::Custom`].
    pub custom_from: f64,
    pub custom_to: f64,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            // Vector by default: it is the one that survives being printed.
            format: ExportFormat::Svg,
            // A figure the width of a text column on A4 with 25 mm margins.
            width_mm: 160.0,
            height_mm: 90.0,
            dpi: 300.0,
            padding_mm: 3.0,
            font_pt: 9.0,
            line_width_pt: 1.0,
            // A report is printed on white paper.
            theme: ExportTheme::Light,
            transparent: false,
            grid: true,
            legend: LegendPos::TopLeft,
            titles: true,
            cursor: false,
            auto_fit_y: false,
            separate_files: false,
            range: RangeMode::View,
            custom_from: 0.0,
            custom_to: 0.0,
        }
    }
}

impl ExportSettings {
    /// The figure's size in figure units, for `panels` stacked graphs.
    pub fn size_units(&self, panels: usize) -> Vec2 {
        Vec2::new(
            mm_to_units(self.width_mm),
            mm_to_units(self.height_mm) * panels.max(1) as f32,
        )
    }

    pub fn size_mm(&self, panels: usize) -> Vec2 {
        Vec2::new(self.width_mm, self.height_mm * panels.max(1) as f32)
    }

    /// The PNG's pixel dimensions.
    pub fn size_pixels(&self, panels: usize) -> (u32, u32) {
        let size = self.size_mm(panels);
        (
            mm_to_pixels(size.x, self.dpi).round().max(1.0) as u32,
            mm_to_pixels(size.y, self.dpi).round().max(1.0) as u32,
        )
    }

    /// Device pixels per figure unit.
    pub fn scale(&self) -> f32 {
        self.dpi / UNITS_PER_INCH
    }

    pub fn style(&self) -> figure::Style {
        let palette = match self.theme {
            ExportTheme::Light => figure::Palette::light(),
            ExportTheme::Dark => figure::Palette::dark(),
        };
        figure::Style {
            palette: if self.transparent { palette.transparent() } else { palette },
            font: points_to_units(self.font_pt),
            line_width: points_to_units(self.line_width_pt),
            padding: mm_to_units(self.padding_mm),
            grid: self.grid,
            legend: self.legend,
            titles: self.titles,
            cursor: self.cursor,
        }
    }

    /// The window to export, resolved against the timeline and the data.
    pub fn resolve_range(&self, timeline: &Timeline, bounds: Option<(f64, f64)>) -> (f64, f64) {
        match self.range {
            RangeMode::View => (timeline.view_start, timeline.view_end),
            RangeMode::All => bounds.unwrap_or((timeline.view_start, timeline.view_end)),
            RangeMode::Custom => {
                let start = bounds.map_or(0.0, |(lo, _)| lo);
                (start + self.custom_from, start + self.custom_to)
            }
        }
    }
}

/// Everything an exported figure is built from, gathered in one place so the
/// dialog, the preview and the file all draw exactly the same picture.
#[derive(Clone, Copy)]
pub struct FigureRequest<'a> {
    pub project: &'a Project,
    pub plots: &'a Plots,
    pub tanks: &'a Tanks,
    pub ids: &'a [ExportItem],
    pub range: (f64, f64),
    pub cursor: Option<f64>,
}

/// Turns the app's plots into a figure.
///
/// `width_units` is only used to decide how many points to ask each series
/// for: about two per figure unit, which is one per pixel at 192 dpi and more
/// than any renderer can show at the sizes a page allows.
pub fn build_figure(request: &FigureRequest<'_>, settings: &ExportSettings, width_units: f32) -> figure::Figure {
    let target_points = ((width_units * 2.0) as usize).clamp(500, 20_000);
    // The same one-column-per-two-units the pane samples the tank at, so the
    // exported strip is the strip on screen rather than a coarser or finer
    // picture of the same data.
    let columns = ((width_units * 0.5) as usize).clamp(2, 400);
    let panels = request
        .ids
        .iter()
        .filter_map(|item| match item {
            ExportItem::Plot(id) => request.plots.get(*id).map(|plot| {
                build_graph_panel(request.project, plot, request.range, target_points, settings.auto_fit_y)
            }),
            ExportItem::Tank(id) => request
                .tanks
                .get(*id)
                .map(|tank| build_tank_panel(request.project, tank, request.range, columns)),
        })
        .collect();
    figure::Figure {
        panels,
        range: request.range,
        cursor: request.cursor,
    }
}

/// The tank pane as a panel: the pane's own sampling, ramp and modes, at the
/// figure's size.
fn build_tank_panel(project: &Project, spec: &TankSpec, range: (f64, f64), columns: usize) -> figure::Panel {
    figure::Panel {
        title: spec.title(),
        content: figure::Content::Tank(figure::Tank {
            field: spec.sample(project, range, columns),
            lo_c: spec.lo_c,
            hi_c: spec.hi_c,
            blocks: spec.blocks,
            grid: spec.grid,
        }),
    }
}

/// One series, pulled out of the project before any of it is drawn.
struct Prepared {
    label: String,
    color: Color32,
    axis: PlotAxis,
    unit: Option<String>,
    points: Vec<[f64; 2]>,
    bounds: Option<(f64, f64)>,
}

fn build_graph_panel(
    project: &Project,
    plot: &PlotSpec,
    range: (f64, f64),
    target_points: usize,
    auto_fit_y: bool,
) -> figure::Panel {
    let (t0, t1) = range;
    let multi_source = plot.entries.windows(2).any(|w| w[0].source != w[1].source);
    let mut prepared: Vec<Prepared> = Vec::with_capacity(plot.entries.len());
    for entry in &plot.entries {
        let Some(source) = project.source(entry.source) else {
            continue;
        };
        let SourceKind::Log(log) = &source.kind else {
            continue;
        };
        let Some(series) = log.series.iter().find(|s| s.name == entry.series) else {
            continue;
        };
        let offset = source.offset_seconds;
        let base = if multi_source {
            format!("{} [{}]", entry.series, source.name)
        } else {
            entry.series.clone()
        };
        // A corrected line has to carry its correction into the report, where
        // nobody can open the ⚙ menu to find out about it.
        let label = entry.label_with_offset(&base, series.unit.as_deref());
        let mut points = series.slice_for_range(t0, t1, offset, target_points);
        entry.correct_points(&mut points);
        prepared.push(Prepared {
            label,
            color: entry.color,
            axis: entry.axis,
            unit: series.unit.clone(),
            points,
            bounds: entry.correct_bounds(series.value_bounds_in_range(t0, t1, offset)),
        });
    }

    // The axes the pane on screen would have, over the exported window.
    let (left_auto, right_auto) =
        crate::panes::value_ranges(prepared.iter().map(|s| (s.axis, s.bounds)), plot.zero_aligned);
    let auto_y = if plot.normalize {
        (-0.05, 1.05)
    } else {
        left_auto.or(right_auto).unwrap_or((0.0, 1.0))
    };
    // A range pinned by a box zoom is in left-axis units; the right axis
    // followed it on screen through this map, and has to here too.
    let right_map = AxisMap {
        from: right_auto.unwrap_or(auto_y),
        to: auto_y,
    };
    let manual = plot.y_manual.filter(|_| !auto_fit_y && !plot.normalize);
    let (left, right) = match manual {
        Some(range) => (
            range,
            right_auto.map(|_| (right_map.axis_value(range.0), right_map.axis_value(range.1))),
        ),
        None => (auto_y, right_auto),
    };
    // Everything below divides by these, and a value axis is one place a NaN
    // in the data reaches unchecked -- a series of them has no range at all.
    let left = if is_drawable_range(left) { left } else { (0.0, 1.0) };
    let right = right.filter(|r| !plot.normalize && is_drawable_range(*r));
    let has_right = right.is_some() && prepared.iter().any(|s| s.axis == PlotAxis::Right);

    let left_label = if plot.normalize {
        "normalized".to_string()
    } else {
        unit_label(prepared.iter().filter(|s| s.axis == PlotAxis::Left))
    };
    let right_label = unit_label(prepared.iter().filter(|s| s.axis == PlotAxis::Right));

    let series = prepared
        .into_iter()
        .map(|mut s| {
            let on_right = has_right && s.axis == PlotAxis::Right;
            if plot.normalize {
                normalize(&mut s.points, s.bounds);
            }
            let mut label = s.label;
            // Which axis a line is read against has to be on the line itself;
            // the numbers on the two sides are otherwise unattributable.
            if on_right {
                label.push_str(" (R)");
            }
            figure::Series {
                label,
                color: s.color,
                right_axis: on_right,
                points: s.points,
            }
        })
        .collect();

    figure::Panel {
        title: plot.title(),
        content: figure::Content::Graph(figure::Graph {
            series,
            left,
            right: right.filter(|_| has_right),
            left_label,
            right_label,
        }),
    }
}

/// Rescales a series onto 0..1 over the window, as the pane's normalize
/// toggle does. A flat series has no range to divide by and sits in the
/// middle rather than at infinity.
fn normalize(points: &mut [[f64; 2]], bounds: Option<(f64, f64)>) {
    match bounds {
        Some((lo, hi)) if hi > lo => {
            for p in points {
                p[1] = (p[1] - lo) / (hi - lo);
            }
        }
        _ => {
            for p in points {
                p[1] = 0.5;
            }
        }
    }
}

/// The distinct units among a set of series, in the order they appear.
fn unit_label<'s>(series: impl Iterator<Item = &'s Prepared>) -> String {
    let mut units: Vec<&str> = Vec::new();
    for unit in series.filter_map(|s| s.unit.as_deref()) {
        if !units.contains(&unit) {
            units.push(unit);
        }
    }
    units.join(" / ")
}

/// Renders `figure` and writes it to `path`.
pub fn write_figure(figure: &figure::Figure, settings: &ExportSettings, path: &Path) -> anyhow::Result<()> {
    let panels = figure.panels.len().max(1);
    let size = settings.size_units(panels);
    let style = settings.style();
    match settings.format {
        ExportFormat::Svg => {
            let mut canvas = svg::SvgCanvas::new(size, settings.size_mm(panels));
            figure::draw(&mut canvas, figure, &style);
            std::fs::write(path, canvas.finish())?;
        }
        ExportFormat::Png => {
            let mut canvas = raster::RasterCanvas::new(size, settings.scale())?;
            figure::draw(&mut canvas, figure, &style);
            std::fs::write(path, canvas.into_png(settings.dpi)?)?;
        }
    }
    Ok(())
}

/// Writes every selected graph, either as one stacked figure or as a file
/// each. Returns what was written, for the status line.
pub fn export(request: &FigureRequest<'_>, settings: &ExportSettings, path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    anyhow::ensure!(!request.ids.is_empty(), "no graphs selected");
    anyhow::ensure!(is_drawable_range(request.range), "the exported time range is empty");

    let width_units = mm_to_units(settings.width_mm);
    if !settings.separate_files || request.ids.len() == 1 {
        let figure = build_figure(request, settings, width_units);
        write_figure(&figure, settings, path)?;
        return Ok(vec![path.to_path_buf()]);
    }

    let mut written = Vec::new();
    for (i, id) in request.ids.iter().enumerate() {
        let one = FigureRequest {
            ids: std::slice::from_ref(id),
            ..*request
        };
        let figure = build_figure(&one, settings, width_units);
        let path = numbered(path, i + 1, settings.format);
        write_figure(&figure, settings, &path)?;
        written.push(path);
    }
    Ok(written)
}

/// `plots.svg` -> `plots-2.svg`, for the one-file-per-graph case.
fn numbered(path: &Path, n: usize, format: ExportFormat) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "figure".to_string());
    path.with_file_name(format!("{stem}-{n}.{}", format.extension()))
}

/// A file name from a graph's title -- which is a series name, and so can
/// hold anything a MAVLink field can.
pub fn sanitize_file_name(title: &str) -> String {
    let mut out: String = title
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches(['_', '.']).to_string();
    if out.is_empty() {
        "figure".to_string()
    } else {
        out.chars().take(60).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LogFormat, LogSource, Source};
    use crate::panes::PlotAxis;
    use crate::series::TimeSeries;

    /// A pressure in bar and a thrust in newtons: two series that only share
    /// a graph because they are on separate axes.
    fn project() -> Project {
        let mut project = Project::new();
        let id = project.alloc_id();
        let pressure: Vec<[f64; 2]> =
            (0..500).map(|i| [i as f64 * 0.1, 40.0 + (i as f64 * 0.01).sin() * 5.0]).collect();
        let thrust: Vec<[f64; 2]> = (0..500).map(|i| [i as f64 * 0.1, (i as f64 * 0.02).cos() * 7500.0]).collect();
        project.sources.push(Source {
            id,
            name: "run.tlog".into(),
            path: "run.tlog".into(),
            offset_seconds: 0.0,
            color: Color32::WHITE,
            enabled: true,
            kind: SourceKind::Log(LogSource {
                series: vec![
                    TimeSeries::from_points("PRESSURE_VESSEL[1].pressure1", pressure).with_unit(Some("bar".into())),
                    TimeSeries::from_points("THRUST.force", thrust).with_unit(Some("N".into())),
                ],
                format: LogFormat::Tlog,
                can: Default::default(),
            }),
        });
        project
    }

    /// The plot id behind an item, for reaching into `Plots` in a test.
    fn plot_id(item: ExportItem) -> PlotId {
        match item {
            ExportItem::Plot(id) => id,
            ExportItem::Tank(_) => panic!("not a graph"),
        }
    }

    fn two_plots(project: &Project) -> (Plots, Vec<ExportItem>) {
        let source = project.sources[0].id;
        let mut plots = Plots::default();
        let a = plots.create(source, "PRESSURE_VESSEL[1].pressure1".to_string());
        plots.add(a, source, "THRUST.force".to_string(), PlotAxis::Right);
        let b = plots.create(source, "THRUST.force".to_string());
        (plots, vec![ExportItem::Plot(a), ExportItem::Plot(b)])
    }

    /// A tank whose ten heights are one row of sensor slots, warming over the
    /// run so the strip has something in it.
    fn project_with_a_tank() -> Project {
        let mut project = Project::new();
        let id = project.alloc_id();
        let series: Vec<TimeSeries> = (0..crate::tank::TANK_SENSORS)
            .map(|sensor| {
                let points: Vec<[f64; 2]> = (0..200)
                    .map(|i| [i as f64 * 0.25, -10.0 + sensor as f64 * 4.0 + i as f64 * 0.05])
                    .collect();
                TimeSeries::from_points(format!("CAN_SENSOR[6].slot{sensor}"), points).with_unit(Some("°C".into()))
            })
            .collect();
        project.sources.push(Source {
            id,
            name: "tank.tlog".into(),
            path: "tank.tlog".into(),
            offset_seconds: 0.0,
            color: Color32::WHITE,
            enabled: true,
            kind: SourceKind::Log(LogSource {
                series,
                format: LogFormat::Tlog,
                can: Default::default(),
            }),
        });
        project
    }

    /// Writes one figure, for a test that has to re-borrow the panes between
    /// exports.
    fn export_at(
        project: &Project,
        plots: &Plots,
        tanks: &Tanks,
        ids: &[ExportItem],
        settings: &ExportSettings,
        path: &Path,
    ) {
        let request = FigureRequest {
            project,
            plots,
            tanks,
            ids,
            range: (0.0, 50.0),
            cursor: Some(10.0),
        };
        export(&request, settings, path).expect("the figure was written");
    }

    fn graph_of(figure: &figure::Figure, panel: usize) -> &figure::Graph {
        match &figure.panels[panel].content {
            figure::Content::Graph(graph) => graph,
            figure::Content::Tank(_) => panic!("panel {panel} is a tank, not a graph"),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rapid-analyzer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        dir
    }

    #[test]
    fn a_graph_with_two_axes_becomes_a_panel_with_two_axes() {
        let project = project();
        let (plots, ids) = two_plots(&project);
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            tanks: &Tanks::default(),
            ids: &ids[..1],
            range: (0.0, 50.0),
            cursor: None,
        };
        let figure = build_figure(&request, &ExportSettings::default(), 600.0);
        let graph = graph_of(&figure, 0);
        assert_eq!(graph.left_label, "bar");
        assert_eq!(graph.right_label, "N");
        let right = graph.right.expect("a right axis");
        // Each axis covers its own series, in its own units -- the export
        // draws two axes rather than squeezing one into the other.
        assert!(right.0 < -7500.0 && right.1 > 7500.0, "{right:?}");
        assert!(graph.left.0 > 30.0 && graph.left.1 < 50.0, "{:?}", graph.left);
        assert!(graph.series.iter().any(|s| s.right_axis && s.label.ends_with("(R)")));
    }

    #[test]
    fn normalizing_puts_every_series_on_one_axis() {
        let project = project();
        let (mut plots, ids) = two_plots(&project);
        plots.get_mut(plot_id(ids[0])).unwrap().normalize = true;
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            tanks: &Tanks::default(),
            ids: &ids[..1],
            range: (0.0, 50.0),
            cursor: None,
        };
        let figure = build_figure(&request, &ExportSettings::default(), 600.0);
        let graph = graph_of(&figure, 0);
        assert_eq!(graph.left_label, "normalized");
        assert!(graph.right.is_none(), "nothing has its own axis when normalized");
        assert!(graph.series.iter().all(|s| !s.right_axis));
        for series in &graph.series {
            assert!(
                series.points.iter().all(|p| (-0.01..=1.01).contains(&p[1])),
                "{} left the 0..1 range",
                series.label
            );
        }
    }

    /// A box zoom pins the value range; the exported figure is meant to be
    /// the graph on screen, so it keeps that range -- and carries the right
    /// axis along with it, as the pane does.
    #[test]
    fn a_pinned_value_range_is_kept_unless_the_user_asks_for_a_fit() {
        let project = project();
        let (mut plots, ids) = two_plots(&project);
        plots.get_mut(plot_id(ids[0])).unwrap().y_manual = Some((39.0, 41.0));
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            tanks: &Tanks::default(),
            ids: &ids[..1],
            range: (0.0, 50.0),
            cursor: None,
        };
        let kept = build_figure(&request, &ExportSettings::default(), 600.0);
        assert_eq!(graph_of(&kept, 0).left, (39.0, 41.0));
        let right = graph_of(&kept, 0).right.expect("a right axis");
        assert!(right.1 - right.0 > 0.0);

        let fitted = build_figure(
            &request,
            &ExportSettings {
                auto_fit_y: true,
                ..Default::default()
            },
            600.0,
        );
        assert_ne!(graph_of(&fitted, 0).left, (39.0, 41.0));
        assert!(graph_of(&fitted, 0).left.0 < 36.0, "{:?}", graph_of(&fitted, 0).left);
    }

    #[test]
    fn both_formats_are_written_and_are_what_they_claim_to_be() {
        let project = project();
        let (plots, ids) = two_plots(&project);
        let dir = temp_dir("formats");
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            tanks: &Tanks::default(),
            ids: &ids,
            range: (0.0, 50.0),
            cursor: Some(10.0),
        };

        let svg_path = dir.join("figure.svg");
        let settings = ExportSettings::default();
        assert_eq!(export(&request, &settings, &svg_path).unwrap(), vec![svg_path.clone()]);
        let svg = std::fs::read_to_string(&svg_path).unwrap();
        assert!(svg.starts_with("<?xml"), "{}", &svg[..40]);
        assert!(svg.contains("width=\"160mm\""), "the physical size it was asked for");
        assert!(svg.contains("PRESSURE_VESSEL[1].pressure1"), "the series is named in it");
        assert!(svg.matches("<polyline").count() > 2, "the lines themselves");
        assert!(svg.trim_end().ends_with("</svg>"));

        let png_path = dir.join("figure.png");
        let settings = ExportSettings {
            format: ExportFormat::Png,
            dpi: 150.0,
            ..Default::default()
        };
        export(&request, &settings, &png_path).unwrap();
        let decoded = image::open(&png_path).unwrap();
        // 160 x 180 mm at 150 dpi: two stacked graphs.
        assert_eq!((decoded.width(), decoded.height()), settings.size_pixels(2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_file_per_graph_writes_one_file_per_graph() {
        let project = project();
        let (plots, ids) = two_plots(&project);
        let dir = temp_dir("separate");
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            tanks: &Tanks::default(),
            ids: &ids,
            range: (0.0, 50.0),
            cursor: None,
        };
        let settings = ExportSettings {
            separate_files: true,
            ..Default::default()
        };
        let written = export(&request, &settings, &dir.join("run.svg")).unwrap();
        assert_eq!(written.len(), 2);
        for path in &written {
            assert!(path.exists(), "{} was not written", path.display());
        }
        assert_eq!(written[1].file_name().unwrap(), "run-2.svg");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every one of these is a division by an empty range somewhere below.
    #[test]
    fn nothing_to_draw_is_refused_or_drawn_empty_rather_than_panicking() {
        let empty_project = Project::new();
        let no_plots = Plots::default();
        let dir = temp_dir("empty");
        let request = FigureRequest {
            project: &empty_project,
            plots: &no_plots,
            tanks: &Tanks::default(),
            ids: &[],
            range: (0.0, 1.0),
            cursor: None,
        };
        assert!(export(&request, &ExportSettings::default(), &dir.join("x.svg")).is_err());

        let project = project();
        let (plots, ids) = two_plots(&project);
        let backwards = FigureRequest {
            project: &project,
            plots: &plots,
            tanks: &Tanks::default(),
            ids: &ids,
            range: (50.0, 0.0),
            cursor: None,
        };
        assert!(export(&backwards, &ExportSettings::default(), &dir.join("x.svg")).is_err());

        // A window with no samples in it still draws: empty axes, no lines.
        let elsewhere = FigureRequest {
            range: (1e9, 1e9 + 10.0),
            ..backwards
        };
        export(&elsewhere, &ExportSettings::default(), &dir.join("empty.svg")).unwrap();
        assert!(dir.join("empty.svg").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A sensor corrected in a graph is corrected in the figure of it -- and
    /// the figure says so, because whoever reads the report was not there when
    /// the correction was typed in.
    #[test]
    fn a_calibration_offset_moves_the_axis_and_is_named_in_the_legend() {
        let project = project();
        let (mut plots, ids) = two_plots(&project);
        let figure_of = |plots: &Plots| {
            let request = FigureRequest {
                project: &project,
                plots,
                tanks: &Tanks::default(),
                ids: &ids[..1],
                range: (0.0, 50.0),
                cursor: None,
            };
            build_figure(&request, &ExportSettings::default(), 600.0)
        };
        let raw = figure_of(&plots);
        let raw_axis = graph_of(&raw, 0).left;
        let raw_right = graph_of(&raw, 0).right;

        plots.get_mut(plot_id(ids[0])).unwrap().entries[0].value_offset = 10.0;
        let corrected = figure_of(&plots);
        let graph = graph_of(&corrected, 0);
        // The left axis is the pressure's; it moves with the correction, and
        // the thrust on the right axis does not.
        assert!((graph.left.0 - raw_axis.0 - 10.0).abs() < 1e-9, "{:?} vs {raw_axis:?}", graph.left);
        assert!((graph.left.1 - raw_axis.1 - 10.0).abs() < 1e-9, "{:?} vs {raw_axis:?}", graph.left);
        assert_eq!(graph.right, raw_right);

        let pressure = &graph.series[0];
        assert!(pressure.label.ends_with("(+10 bar)"), "{}", pressure.label);
        assert!(pressure.points.iter().all(|p| p[1] > 34.0), "every sample moved up");
    }

    /// The tank pane's whole point is the picture, so the test is that the
    /// picture comes out: a field with the stratification in it, drawn as
    /// gradients (shaded) or flat rectangles (blocks), and ink on the page
    /// where the strip is.
    #[test]
    fn a_tank_pane_exports_the_strip_it_draws() {
        let project = project_with_a_tank();
        let plots = Plots::default();
        let mut tanks = Tanks::default();
        let id = tanks.create(&project);
        let ids = [ExportItem::Tank(id)];
        let dir = temp_dir("tank");
        let settings = ExportSettings::default();

        let figure = {
            let request = FigureRequest {
                project: &project,
                plots: &plots,
                tanks: &tanks,
                ids: &ids,
                range: (0.0, 50.0),
                cursor: Some(10.0),
            };
            build_figure(&request, &settings, 600.0)
        };
        let figure::Content::Tank(tank) = &figure.panels[0].content else {
            panic!("a tank pane exports a tank");
        };
        assert!(figure.panels[0].title.starts_with("Tank"));
        assert!(!tank.field.is_empty(), "nothing landed in the window");
        assert!(tank.field.cols > 2);
        // Bottom cold, top warm -- the wall as the log has it, and the thing
        // the picture exists to show.
        let top = tank.field.at(tank.field.cols / 2, crate::tank::TANK_SENSORS - 1);
        let bottom = tank.field.at(tank.field.cols / 2, 0);
        assert!(top > bottom + 20.0, "{bottom} .. {top}");

        // Shaded: one gradient per column, which is what keeps the SVG an
        // interpolation rather than a stack of bands.
        let path = dir.join("tank.svg");
        export_at(&project, &plots, &tanks, &ids, &settings, &path);
        let svg = std::fs::read_to_string(&path).unwrap();
        // One gradient per column of the strip, plus the colour bar.
        assert!(svg.matches("<linearGradient").count() > 10, "the shading");
        assert!(svg.contains(">s0<") && svg.contains(">s9<"), "the heights are named");
        assert!(svg.contains("°C"), "the colour bar says what it is in");

        // Blocks: flat rectangles, no gradients at all.
        tanks.get_mut(id).unwrap().blocks = true;
        let path = dir.join("blocks.svg");
        export_at(&project, &plots, &tanks, &ids, &settings, &path);
        let blocks = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            blocks.matches("<linearGradient").count(),
            1,
            "blocks mode interpolates nothing -- the only gradient left is the colour bar"
        );
        assert!(blocks.matches("<rect").count() > 10, "one rectangle per region");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// ... and the same picture in pixels: the strip is painted, and the
    /// vessel is not left as an empty box.
    #[test]
    fn a_tank_figure_puts_ink_where_the_strip_is() {
        let project = project_with_a_tank();
        let plots = Plots::default();
        let tanks = {
            let mut tanks = Tanks::default();
            tanks.create(&project);
            tanks
        };
        let ids = [ExportItem::Tank(0)];
        let settings = ExportSettings::default();
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            tanks: &tanks,
            ids: &ids,
            range: (0.0, 50.0),
            cursor: None,
        };
        let size = settings.size_units(1);
        let figure = build_figure(&request, &settings, size.x);
        let mut canvas = raster::RasterCanvas::new(size, 1.0).unwrap();
        figure::draw(&mut canvas, &figure, &settings.style());
        let (width, _, rgba) = canvas.into_rgba();

        // The middle of the figure is inside the strip, and the strip is
        // coloured -- not the white page, and not the grey of a hole.
        let index = |x: usize, y: usize| (y * width + x) * 4;
        let middle = index(width / 2, (size.y * 0.5) as usize);
        let (r, g, b) = (rgba[middle], rgba[middle + 1], rgba[middle + 2]);
        assert!(
            r.abs_diff(b) > 20,
            "the middle of the strip should carry the ramp, got #{r:02x}{g:02x}{b:02x}"
        );
    }

    /// A graph whose source was unloaded, and one whose series is gone: the
    /// exporter drops what it cannot find rather than inventing it.
    #[test]
    fn a_graph_naming_a_series_that_is_gone_exports_an_empty_panel() {
        let project = project();
        let mut plots = Plots::default();
        let id = ExportItem::Plot(plots.create(99, "NOT_IMPORTED.field".to_string()));
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            tanks: &Tanks::default(),
            ids: &[id],
            range: (0.0, 50.0),
            cursor: None,
        };
        let figure = build_figure(&request, &ExportSettings::default(), 600.0);
        assert_eq!(figure.panels.len(), 1);
        let graph = graph_of(&figure, 0);
        assert!(graph.series.is_empty());
        assert!(graph.left.1 > graph.left.0, "still a usable axis");
    }

    #[test]
    fn a_figure_is_as_big_as_it_was_asked_to_be() {
        let settings = ExportSettings {
            width_mm: 160.0,
            height_mm: 90.0,
            dpi: 300.0,
            ..Default::default()
        };
        // 160 mm at 300 dpi.
        assert_eq!(settings.size_pixels(1), (1890, 1063));
        // Two stacked graphs are twice as tall, and no wider.
        assert_eq!(settings.size_pixels(2), (1890, 2126));
        // ... and the same figure in units, which is what the SVG viewBox is.
        let units = settings.size_units(1);
        assert!((units.x - 604.72).abs() < 0.01, "{units:?}");
    }

    #[test]
    fn a_file_name_survives_a_series_name() {
        assert_eq!(sanitize_file_name("PRESSURE_VESSEL[1]: pressure1 / temp"), "PRESSURE_VESSEL_1_pressure1_temp");
        assert_eq!(sanitize_file_name("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_file_name(""), "figure");
        assert_eq!(sanitize_file_name("///"), "figure");
        assert!(sanitize_file_name(&"x".repeat(200)).len() <= 60);
    }

    #[test]
    fn one_file_per_graph_numbers_them() {
        let path = Path::new("/tmp/run.svg");
        assert_eq!(numbered(path, 2, ExportFormat::Svg), Path::new("/tmp/run-2.svg"));
        assert_eq!(numbered(path, 1, ExportFormat::Png), Path::new("/tmp/run-1.png"));
    }

    #[test]
    fn the_custom_range_is_measured_from_the_start_of_the_data() {
        let settings = ExportSettings {
            range: RangeMode::Custom,
            custom_from: 10.0,
            custom_to: 20.0,
            ..Default::default()
        };
        let mut timeline = Timeline::new((1000.0, 2000.0));
        timeline.set_view(1100.0, 1200.0);
        assert_eq!(settings.resolve_range(&timeline, Some((1000.0, 2000.0))), (1010.0, 1020.0));

        let view = ExportSettings {
            range: RangeMode::View,
            ..settings.clone()
        };
        assert_eq!(view.resolve_range(&timeline, Some((1000.0, 2000.0))), (1100.0, 1200.0));

        let all = ExportSettings {
            range: RangeMode::All,
            ..settings
        };
        assert_eq!(all.resolve_range(&timeline, Some((1000.0, 2000.0))), (1000.0, 2000.0));
        // With nothing loaded there is no "everything"; fall back to the view.
        assert_eq!(all.resolve_range(&timeline, None), (1100.0, 1200.0));
    }
}
