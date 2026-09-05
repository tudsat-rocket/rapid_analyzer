//! The export window: which graphs, over what stretch of time, at what size.
//!
//! It is a window rather than a panel because exporting is a detour from
//! reviewing a run -- the graphs behind it stay where they were, and stay
//! usable -- and because the preview needs the room. The preview is not a
//! mock-up: it is the figure, rendered by the same code and at the same
//! layout as the file, just at screen resolution. What is in the window is
//! what lands in the report.

use std::collections::HashSet;
use std::hash::{Hash as _, Hasher as _};
use std::path::PathBuf;

use egui::Vec2;

use super::{
    ExportFormat, ExportSettings, ExportTheme, FigureRequest, LegendPos, RangeMode, build_figure, export, figure,
    raster, sanitize_file_name,
};
use crate::model::{Project, SourceKind};
use crate::panes::{PlotId, Plots};
use crate::timeline::{Timeline, format_duration, format_utc};

/// Widest the preview is rendered, in screen points. Big enough to read the
/// tick labels, small enough to re-render on every settings change.
const PREVIEW_WIDTH: f32 = 420.0;
/// ... and a ceiling on the height, for a stack of many graphs.
const PREVIEW_MAX_HEIGHT: f32 = 500.0;
/// Height of the window's body -- settings on the left, preview on the right.
///
/// Fixed, and both columns scroll inside it, because a vertical separator in
/// egui grows to the height available to it: left to itself in a window, it
/// makes the window as tall as the screen.
const BODY_HEIGHT: f32 = 560.0;
/// Width of the settings column.
const SETTINGS_WIDTH: f32 = 400.0;

/// What the dialog needs to know about the rest of the app.
pub struct DialogContext<'a> {
    pub project: &'a Project,
    pub plots: &'a Plots,
    pub timeline: &'a Timeline,
    /// The graphs with a pane open, in the order they were created.
    pub visible: Vec<PlotId>,
}

#[derive(Default)]
pub struct ExportDialog {
    open: bool,
    settings: ExportSettings,
    /// Graphs ticked for export. Kept across openings, and filtered against
    /// what is actually on screen every time it is shown.
    selected: HashSet<PlotId>,
    preview: Option<egui::TextureHandle>,
    /// What the preview was rendered from, so it is redrawn when -- and only
    /// when -- something it depends on changes.
    preview_key: Option<u64>,
    preview_error: Option<String>,
    error: Option<String>,
    /// What the last export wrote, shown until the next one.
    notice: Option<String>,
    last_dir: Option<PathBuf>,
}

impl ExportDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the window, starting from every graph currently on screen.
    pub fn open(&mut self, visible: &[PlotId]) {
        self.open = true;
        self.error = None;
        self.notice = None;
        if self.selected.iter().all(|id| !visible.contains(id)) {
            self.selected = visible.iter().copied().collect();
        }
    }

    /// Draws the window. Returns a line for the app's status bar when
    /// something was written.
    pub fn show(&mut self, ctx: &egui::Context, cx: &DialogContext<'_>) -> Option<String> {
        if !self.open {
            return None;
        }
        // A graph whose pane was closed while the window was open must not
        // stay in the export.
        self.selected.retain(|id| cx.visible.contains(id));

        let mut open = true;
        let mut status = None;
        let centre = ctx.content_rect().center();
        egui::Window::new("Export graph")
            .open(&mut open)
            .default_pos(centre - egui::vec2(450.0, 330.0))
            .default_width(880.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                status = self.contents(ui, cx);
            });
        if !open {
            self.open = false;
        }
        status
    }

    fn contents(&mut self, ui: &mut egui::Ui, cx: &DialogContext<'_>) -> Option<String> {
        if cx.visible.is_empty() {
            ui.label("No graphs are open.");
            ui.weak("Tick a series in the sidebar to open one, then export it from here.");
            return None;
        }

        let bounds = cx.project.time_bounds();
        let range = self.settings.resolve_range(cx.timeline, bounds);
        let mut status = None;

        ui.horizontal_top(|ui| {
            ui.set_height(BODY_HEIGHT);
            ui.vertical(|ui| {
                ui.set_width(SETTINGS_WIDTH);
                egui::ScrollArea::vertical()
                    .id_salt("export_settings")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.graph_picker(ui, cx);
                        ui.add_space(6.0);
                        self.range_picker(ui, cx, bounds, range);
                        ui.add_space(6.0);
                        self.page_settings(ui);
                        ui.add_space(6.0);
                        self.style_settings(ui);
                    });
            });
            ui.separator();
            ui.vertical(|ui| {
                egui::ScrollArea::vertical()
                    .id_salt("export_preview")
                    .auto_shrink([true, false])
                    .show(ui, |ui| {
                        self.preview(ui, cx, range);
                    });
            });
        });

        ui.separator();
        if let Some(error) = &self.error {
            let color = ui.visuals().error_fg_color;
            ui.colored_label(color, error);
        }
        // The window stays open after an export -- a report is usually
        // several figures -- so it has to say for itself that it wrote one.
        if let Some(notice) = &self.notice {
            ui.weak(notice);
        }
        ui.horizontal(|ui| {
            let ready = !self.selected.is_empty() && super::is_drawable_range(range);
            let button = egui::Button::new("💾  Export…");
            if ui.add_enabled(ready, button).on_hover_text("Choose a file and write the figure").clicked() {
                status = self.save(cx, range);
            }
            if ui.button("Close").clicked() {
                self.open = false;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Clear of the window's resize corner.
                ui.add_space(12.0);
                let panels = self.panels_per_file();
                let (w, h) = self.settings.size_pixels(panels);
                let size = self.settings.size_mm(panels);
                let detail = match self.settings.format {
                    ExportFormat::Svg => format!("{:.0} × {:.0} mm, vector", size.x, size.y),
                    ExportFormat::Png => format!("{:.0} × {:.0} mm · {w} × {h} px", size.x, size.y),
                };
                ui.weak(detail);
            });
        });
        status
    }

    /// How many graphs end up in one file.
    fn panels_per_file(&self) -> usize {
        if self.settings.separate_files {
            1
        } else {
            self.selected.len().max(1)
        }
    }

    /// The graphs to export, in the order they are shown in.
    fn ordered_selection(&self, cx: &DialogContext<'_>) -> Vec<PlotId> {
        cx.visible.iter().copied().filter(|id| self.selected.contains(id)).collect()
    }

    fn graph_picker(&mut self, ui: &mut egui::Ui, cx: &DialogContext<'_>) {
        ui.strong("Graphs");
        ui.horizontal(|ui| {
            if ui.small_button("All").clicked() {
                self.selected = cx.visible.iter().copied().collect();
            }
            if ui.small_button("None").clicked() {
                self.selected.clear();
            }
            ui.weak(format!("{} of {} selected", self.selected.len(), cx.visible.len()));
        });
        // The names are series names, which are long; the list truncates
        // rather than pushing the window wider than the screen.
        ui.scope(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for id in &cx.visible {
                let title = cx.plots.get(*id).map_or_else(|| "plot".to_string(), |p| p.title());
                let mut on = self.selected.contains(id);
                if ui.checkbox(&mut on, &title).on_hover_text(title.clone()).changed() {
                    if on {
                        self.selected.insert(*id);
                    } else {
                        self.selected.remove(id);
                    }
                }
            }
        });
        if self.selected.len() > 1 {
            ui.checkbox(&mut self.settings.separate_files, "One file per graph")
                .on_hover_text("Off: the graphs are stacked in one figure on a shared time axis");
        }
    }

    fn range_picker(
        &mut self,
        ui: &mut egui::Ui,
        cx: &DialogContext<'_>,
        bounds: Option<(f64, f64)>,
        range: (f64, f64),
    ) {
        ui.strong("Time range");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.settings.range, RangeMode::View, "Current view")
                .on_hover_text("What the graphs are showing right now");
            ui.selectable_value(&mut self.settings.range, RangeMode::All, "All data");
            // An empty window is a poor place to start typing from, so the
            // first time round it is filled in from what is on screen.
            if ui.selectable_value(&mut self.settings.range, RangeMode::Custom, "Custom").clicked()
                && self.settings.custom_to <= self.settings.custom_from
            {
                let start = bounds.map_or(0.0, |(lo, _)| lo);
                self.settings.custom_from = cx.timeline.view_start - start;
                self.settings.custom_to = cx.timeline.view_end - start;
            }
        });
        if self.settings.range == RangeMode::Custom {
            let start = bounds.map_or(0.0, |(lo, _)| lo);
            let end = bounds.map_or(0.0, |(_, hi)| hi) - start;
            ui.horizontal(|ui| {
                ui.label("from");
                ui.add(
                    egui::DragValue::new(&mut self.settings.custom_from)
                        .speed(0.1)
                        .range(0.0..=end.max(0.0))
                        .suffix(" s")
                        .max_decimals(3),
                );
                ui.label("to");
                ui.add(
                    egui::DragValue::new(&mut self.settings.custom_to)
                        .speed(0.1)
                        .range(0.0..=end.max(0.0))
                        .suffix(" s")
                        .max_decimals(3),
                );
                if ui
                    .small_button("use view")
                    .on_hover_text("Fill these in from the current view")
                    .clicked()
                {
                    self.settings.custom_from = cx.timeline.view_start - start;
                    self.settings.custom_to = cx.timeline.view_end - start;
                }
            });
            ui.weak("seconds from the start of the data");
        }
        if super::is_drawable_range(range) {
            ui.weak(format!(
                "{}  to  {}   ({})",
                format_utc(range.0),
                format_utc(range.1),
                format_duration(range.1 - range.0)
            ));
        } else {
            let color = ui.visuals().warn_fg_color;
            ui.colored_label(color, "⚠ that range is empty");
        }
    }

    fn page_settings(&mut self, ui: &mut egui::Ui) {
        ui.strong("Page");
        egui::Grid::new("export_page").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Format");
            ui.horizontal(|ui| {
                for format in [ExportFormat::Svg, ExportFormat::Png] {
                    ui.selectable_value(&mut self.settings.format, format, format.label());
                }
            });
            ui.end_row();

            ui.label("Width");
            ui.add(
                egui::DragValue::new(&mut self.settings.width_mm)
                    .speed(1.0)
                    .range(20.0..=1000.0)
                    .suffix(" mm"),
            )
            .on_hover_text("How wide the figure is on the page");
            ui.end_row();

            ui.label("Height per graph");
            ui.add(
                egui::DragValue::new(&mut self.settings.height_mm)
                    .speed(1.0)
                    .range(20.0..=1000.0)
                    .suffix(" mm"),
            );
            ui.end_row();

            ui.label("Resolution");
            ui.add(
                egui::DragValue::new(&mut self.settings.dpi)
                    .speed(10.0)
                    .range(36.0..=1200.0)
                    .suffix(" dpi"),
            )
            .on_hover_text(match self.settings.format {
                ExportFormat::Png => "Pixels per inch: how big the image is, and the size it is placed at",
                ExportFormat::Svg => "Only used for the preview -- an SVG is drawn at the printer's own resolution",
            });
            ui.end_row();

            ui.label("Padding");
            ui.add(
                egui::DragValue::new(&mut self.settings.padding_mm)
                    .speed(0.25)
                    .range(0.0..=40.0)
                    .suffix(" mm"),
            )
            .on_hover_text("Blank margin around the whole figure");
            ui.end_row();
        });
        if self.settings.format == ExportFormat::Png {
            let (w, h) = self.settings.size_pixels(self.panels_per_file());
            if (w as usize).saturating_mul(h as usize) > raster::MAX_PIXELS {
                let color = ui.visuals().warn_fg_color;
                ui.colored_label(color, format!("⚠ {w} × {h} px is too large to render"));
            }
        }
    }

    fn style_settings(&mut self, ui: &mut egui::Ui) {
        ui.strong("Style");
        egui::Grid::new("export_style").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Colours");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.settings.theme, ExportTheme::Light, "Light")
                    .on_hover_text("Black on white, for a printed page");
                ui.selectable_value(&mut self.settings.theme, ExportTheme::Dark, "Dark");
                ui.checkbox(&mut self.settings.transparent, "no background");
            });
            ui.end_row();

            ui.label("Text size");
            ui.add(
                egui::DragValue::new(&mut self.settings.font_pt)
                    .speed(0.25)
                    .range(4.0..=32.0)
                    .suffix(" pt"),
            );
            ui.end_row();

            ui.label("Line width");
            ui.add(
                egui::DragValue::new(&mut self.settings.line_width_pt)
                    .speed(0.05)
                    .range(0.1..=8.0)
                    .suffix(" pt"),
            );
            ui.end_row();

            ui.label("Legend");
            egui::ComboBox::from_id_salt("export_legend")
                .selected_text(self.settings.legend.label())
                .show_ui(ui, |ui| {
                    for pos in [LegendPos::TopLeft, LegendPos::TopRight, LegendPos::Below, LegendPos::Off] {
                        ui.selectable_value(&mut self.settings.legend, pos, pos.label());
                    }
                });
            ui.end_row();
        });
        ui.checkbox(&mut self.settings.grid, "Grid lines");
        ui.checkbox(&mut self.settings.titles, "Graph titles");
        ui.checkbox(&mut self.settings.cursor, "Mark the playhead")
            .on_hover_text("Draw the line the playhead is on, if it falls inside the exported window");
        ui.checkbox(&mut self.settings.auto_fit_y, "Fit the value axes to this window")
            .on_hover_text("Off: keep the value range the graph is showing, including one pinned by a box zoom");
    }

    fn preview(&mut self, ui: &mut egui::Ui, cx: &DialogContext<'_>, range: (f64, f64)) {
        ui.strong("Preview");
        self.update_preview(ui.ctx(), cx, range);
        if let Some(error) = &self.preview_error {
            let color = ui.visuals().warn_fg_color;
            ui.colored_label(color, error);
            return;
        }
        let Some(texture) = &self.preview else {
            ui.weak("nothing to show");
            return;
        };
        let source = egui::load::SizedTexture::from_handle(texture);
        // The preview is rendered at screen resolution, so it is shown at its
        // own size rather than stretched to the panel.
        ui.add(egui::Image::new(source).fit_to_exact_size(source.size));
        ui.weak(if self.settings.separate_files && self.selected.len() > 1 {
            format!("the first of {} files", self.selected.len())
        } else {
            "the figure, at screen resolution".to_string()
        });
    }

    /// Re-renders the preview if -- and only if -- something it depends on
    /// has changed. Rendering it every frame would be a figure a frame.
    fn update_preview(&mut self, ctx: &egui::Context, cx: &DialogContext<'_>, range: (f64, f64)) {
        let key = self.preview_key(cx, range);
        if self.preview_key == Some(key) && (self.preview.is_some() || self.preview_error.is_some()) {
            return;
        }
        self.preview_key = Some(key);
        self.preview = None;
        self.preview_error = None;

        let ids = self.ordered_selection(cx);
        if ids.is_empty() {
            self.preview_error = Some("no graphs selected".to_string());
            return;
        }
        if !super::is_drawable_range(range) {
            self.preview_error = Some("nothing to draw over an empty time range".to_string());
            return;
        }
        let ids = if self.settings.separate_files { &ids[..1] } else { &ids[..] };

        let request = FigureRequest {
            project: cx.project,
            plots: cx.plots,
            ids,
            range,
            cursor: Some(cx.timeline.cursor),
        };
        let size = self.settings.size_units(ids.len());
        let figure = build_figure(&request, &self.settings, size.x);
        // Same figure, same layout, fewer pixels.
        let scale = (PREVIEW_WIDTH / size.x.max(1.0)).min(PREVIEW_MAX_HEIGHT / size.y.max(1.0));
        let mut canvas = match raster::RasterCanvas::new(size, scale) {
            Ok(canvas) => canvas,
            Err(e) => {
                self.preview_error = Some(format!("{e}"));
                return;
            }
        };
        figure::draw(&mut canvas, &figure, &self.settings.style());
        let image = canvas.into_color_image();
        self.preview = Some(ctx.load_texture("export_preview", image, egui::TextureOptions::LINEAR));
    }

    /// Everything the preview is a picture of, boiled down to one number.
    fn preview_key(&self, cx: &DialogContext<'_>, range: (f64, f64)) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", self.settings).hash(&mut hasher);
        range.0.to_bits().hash(&mut hasher);
        range.1.to_bits().hash(&mut hasher);
        cx.timeline.cursor.to_bits().hash(&mut hasher);
        for id in self.ordered_selection(cx) {
            id.hash(&mut hasher);
            let Some(plot) = cx.plots.get(id) else { continue };
            plot.title().hash(&mut hasher);
            for entry in &plot.entries {
                entry.source.hash(&mut hasher);
                entry.series.hash(&mut hasher);
                entry.color.to_array().hash(&mut hasher);
                // A series can grow (a CAN signal re-extracted, a re-import),
                // which changes the picture without changing any setting.
                if let Some(source) = cx.project.source(entry.source)
                    && let SourceKind::Log(log) = &source.kind
                {
                    source.offset_seconds.to_bits().hash(&mut hasher);
                    if let Some(series) = log.series.iter().find(|s| s.name == entry.series) {
                        series.len().hash(&mut hasher);
                    }
                }
            }
        }
        hasher.finish()
    }

    /// Asks for a file and writes it.
    fn save(&mut self, cx: &DialogContext<'_>, range: (f64, f64)) -> Option<String> {
        let ids = self.ordered_selection(cx);
        let title = ids
            .first()
            .and_then(|id| cx.plots.get(*id))
            .map(|p| p.title())
            .unwrap_or_else(|| "figure".to_string());
        let extension = self.settings.format.extension();
        let mut dialog = rfd::FileDialog::new()
            .set_file_name(format!("{}.{extension}", sanitize_file_name(&title)))
            .add_filter(self.settings.format.label(), &[extension]);
        if let Some(dir) = &self.last_dir {
            dialog = dialog.set_directory(dir);
        }
        let path = dialog.save_file()?;
        // A file dialog that filters on an extension does not always add it.
        let path = if path.extension().is_some() {
            path
        } else {
            path.with_extension(extension)
        };
        self.last_dir = path.parent().map(PathBuf::from);

        let request = FigureRequest {
            project: cx.project,
            plots: cx.plots,
            ids: &ids,
            range,
            cursor: Some(cx.timeline.cursor),
        };
        match export(&request, &self.settings, &path) {
            Ok(written) => {
                self.error = None;
                let status = match written.as_slice() {
                    [one] => format!("Exported {}", one.display()),
                    many => format!("Exported {} files to {}", many.len(), path.parent().unwrap_or(&path).display()),
                };
                self.notice = Some(format!("✔ {status}"));
                Some(status)
            }
            Err(e) => {
                self.notice = None;
                self.error = Some(format!("Export failed: {e:#}"));
                None
            }
        }
    }

    /// The figure the dialog would write, for tests.
    #[cfg(test)]
    fn figure_for(&self, cx: &DialogContext<'_>, range: (f64, f64)) -> figure::Figure {
        let ids = self.ordered_selection(cx);
        let request = FigureRequest {
            project: cx.project,
            plots: cx.plots,
            ids: &ids,
            range,
            cursor: None,
        };
        build_figure(&request, &self.settings, super::mm_to_units(self.settings.width_mm))
    }
}

/// Not every caller wants the whole dialog; the size line under the buttons
/// is the same arithmetic the file uses.
pub fn preview_scale(size: Vec2) -> f32 {
    (PREVIEW_WIDTH / size.x.max(1.0)).min(PREVIEW_MAX_HEIGHT / size.y.max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LogFormat, LogSource, Source, SourceKind};
    use crate::panes::PlotAxis;
    use crate::series::TimeSeries;

    fn project() -> Project {
        let mut project = Project::new();
        let id = project.alloc_id();
        let points: Vec<[f64; 2]> = (0..200).map(|i| [i as f64 * 0.1, (i as f64 * 0.05).sin() * 10.0 + 40.0]).collect();
        project.sources.push(Source {
            id,
            name: "run.tlog".into(),
            path: "run.tlog".into(),
            offset_seconds: 0.0,
            color: egui::Color32::WHITE,
            enabled: true,
            kind: SourceKind::Log(LogSource {
                series: vec![TimeSeries::from_points("PV[1].pressure", points).with_unit(Some("bar".into()))],
                format: LogFormat::Tlog,
                can: Default::default(),
            }),
        });
        project
    }

    #[test]
    fn opening_the_dialog_starts_from_what_is_on_screen() {
        let mut plots = Plots::default();
        let a = plots.create(0, "PV[1].pressure".to_string());
        let b = plots.create(0, "PV[1].pressure".to_string());
        let mut dialog = ExportDialog::default();
        dialog.open(&[a, b]);
        assert!(dialog.is_open());
        assert_eq!(dialog.selected.len(), 2);

        // A graph whose pane is closed drops out of the selection rather than
        // being exported from memory.
        let project = project();
        let timeline = Timeline::new((0.0, 20.0));
        let cx = DialogContext {
            project: &project,
            plots: &plots,
            timeline: &timeline,
            visible: vec![a],
        };
        dialog.selected.retain(|id| cx.visible.contains(id));
        assert_eq!(dialog.ordered_selection(&cx), vec![a]);
    }

    #[test]
    fn the_preview_is_only_redrawn_when_something_changed() {
        let project = project();
        let mut plots = Plots::default();
        let id = plots.create(project.sources[0].id, "PV[1].pressure".to_string());
        let timeline = Timeline::new((0.0, 20.0));
        let cx = DialogContext {
            project: &project,
            plots: &plots,
            timeline: &timeline,
            visible: vec![id],
        };
        let mut dialog = ExportDialog::default();
        dialog.open(&[id]);

        let key = dialog.preview_key(&cx, (0.0, 20.0));
        assert_eq!(key, dialog.preview_key(&cx, (0.0, 20.0)), "nothing changed");
        assert_ne!(key, dialog.preview_key(&cx, (0.0, 10.0)), "a different window");
        dialog.settings.font_pt += 1.0;
        assert_ne!(key, dialog.preview_key(&cx, (0.0, 20.0)), "a different setting");
    }

    #[test]
    fn the_figure_carries_the_series_and_its_unit() {
        let project = project();
        let mut plots = Plots::default();
        let source = project.sources[0].id;
        let id = plots.create(source, "PV[1].pressure".to_string());
        plots.add(id, source, "PV[1].pressure".to_string(), PlotAxis::Left);
        let timeline = Timeline::new((0.0, 20.0));
        let cx = DialogContext {
            project: &project,
            plots: &plots,
            timeline: &timeline,
            visible: vec![id],
        };
        let mut dialog = ExportDialog::default();
        dialog.open(&[id]);

        let figure = dialog.figure_for(&cx, (0.0, 20.0));
        assert_eq!(figure.panels.len(), 1);
        let panel = &figure.panels[0];
        assert_eq!(panel.series.len(), 1);
        assert_eq!(panel.left_label, "bar");
        assert!(panel.right.is_none());
        assert!(!panel.series[0].points.is_empty());
        // The axis covers the data, with the headroom the pane gives it.
        assert!(panel.left.0 < 30.0 && panel.left.1 > 50.0, "{:?}", panel.left);
    }
}
