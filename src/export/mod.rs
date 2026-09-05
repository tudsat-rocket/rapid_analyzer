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

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::{Color32, Vec2};

use crate::model::{Project, SourceKind};
use crate::panes::{AxisMap, PlotAxis, PlotId, PlotSpec, Plots};
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
    pub ids: &'a [PlotId],
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
    let panels = request
        .ids
        .iter()
        .filter_map(|id| request.plots.get(*id))
        .map(|plot| build_panel(request.project, plot, request.range, target_points, settings.auto_fit_y))
        .collect();
    figure::Figure {
        panels,
        range: request.range,
        cursor: request.cursor,
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

fn build_panel(
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
        let label = if multi_source {
            format!("{} [{}]", entry.series, source.name)
        } else {
            entry.series.clone()
        };
        prepared.push(Prepared {
            label,
            color: entry.color,
            axis: entry.axis,
            unit: series.unit.clone(),
            points: series.slice_for_range(t0, t1, offset, target_points),
            bounds: series.value_bounds_in_range(t0, t1, offset),
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
        series,
        left,
        right: right.filter(|_| has_right),
        left_label,
        right_label,
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

/// Which graphs the export dialog can offer: the ones with a pane open.
pub fn visible_plots(pane_tiles: impl Iterator<Item = PlotId>, plots: &Plots) -> Vec<PlotId> {
    let open: HashSet<PlotId> = pane_tiles.collect();
    plots.iter().map(|p| p.id).filter(|id| open.contains(id)).collect()
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

    fn two_plots(project: &Project) -> (Plots, Vec<PlotId>) {
        let source = project.sources[0].id;
        let mut plots = Plots::default();
        let a = plots.create(source, "PRESSURE_VESSEL[1].pressure1".to_string());
        plots.add(a, source, "THRUST.force".to_string(), PlotAxis::Right);
        let b = plots.create(source, "THRUST.force".to_string());
        (plots, vec![a, b])
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
            ids: &ids[..1],
            range: (0.0, 50.0),
            cursor: None,
        };
        let figure = build_figure(&request, &ExportSettings::default(), 600.0);
        let panel = &figure.panels[0];
        assert_eq!(panel.left_label, "bar");
        assert_eq!(panel.right_label, "N");
        let right = panel.right.expect("a right axis");
        // Each axis covers its own series, in its own units -- the export
        // draws two axes rather than squeezing one into the other.
        assert!(right.0 < -7500.0 && right.1 > 7500.0, "{right:?}");
        assert!(panel.left.0 > 30.0 && panel.left.1 < 50.0, "{:?}", panel.left);
        assert!(panel.series.iter().any(|s| s.right_axis && s.label.ends_with("(R)")));
    }

    #[test]
    fn normalizing_puts_every_series_on_one_axis() {
        let project = project();
        let (mut plots, ids) = two_plots(&project);
        plots.get_mut(ids[0]).unwrap().normalize = true;
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            ids: &ids[..1],
            range: (0.0, 50.0),
            cursor: None,
        };
        let figure = build_figure(&request, &ExportSettings::default(), 600.0);
        let panel = &figure.panels[0];
        assert_eq!(panel.left_label, "normalized");
        assert!(panel.right.is_none(), "nothing has its own axis when normalized");
        assert!(panel.series.iter().all(|s| !s.right_axis));
        for series in &panel.series {
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
        plots.get_mut(ids[0]).unwrap().y_manual = Some((39.0, 41.0));
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            ids: &ids[..1],
            range: (0.0, 50.0),
            cursor: None,
        };
        let kept = build_figure(&request, &ExportSettings::default(), 600.0);
        assert_eq!(kept.panels[0].left, (39.0, 41.0));
        let right = kept.panels[0].right.expect("a right axis");
        assert!(right.1 - right.0 > 0.0);

        let fitted = build_figure(
            &request,
            &ExportSettings {
                auto_fit_y: true,
                ..Default::default()
            },
            600.0,
        );
        assert_ne!(fitted.panels[0].left, (39.0, 41.0));
        assert!(fitted.panels[0].left.0 < 36.0, "{:?}", fitted.panels[0].left);
    }

    #[test]
    fn both_formats_are_written_and_are_what_they_claim_to_be() {
        let project = project();
        let (plots, ids) = two_plots(&project);
        let dir = temp_dir("formats");
        let request = FigureRequest {
            project: &project,
            plots: &plots,
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

    /// A graph whose source was unloaded, and one whose series is gone: the
    /// exporter drops what it cannot find rather than inventing it.
    #[test]
    fn a_graph_naming_a_series_that_is_gone_exports_an_empty_panel() {
        let project = project();
        let mut plots = Plots::default();
        let id = plots.create(99, "NOT_IMPORTED.field".to_string());
        let request = FigureRequest {
            project: &project,
            plots: &plots,
            ids: &[id],
            range: (0.0, 50.0),
            cursor: None,
        };
        let figure = build_figure(&request, &ExportSettings::default(), 600.0);
        assert_eq!(figure.panels.len(), 1);
        assert!(figure.panels[0].series.is_empty());
        assert!(figure.panels[0].left.1 > figure.panels[0].left.0, "still a usable axis");
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
