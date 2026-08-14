use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use egui_tiles::{Tile, TileId};

use crate::colors::color_for_index;
use crate::import;
use crate::model::{Project, Source, SourceId, SourceKind};
use crate::panes::{AudioPlayerSlot, Pane, PlotId, Plots, TreeBehavior};
use crate::timeline::{self, Timeline};
use crate::video_worker::VideoWorker;

struct ImportOutcome {
    path: PathBuf,
    result: Result<(String, SourceKind), String>,
}

/// Sidebar interactions can't touch the pane tree while it is being iterated
/// over, so they're queued up and applied afterwards.
enum PendingAction {
    AddPane(Pane),
    RemovePane(Pane),
    NewPlot(SourceId, String),
    AddToPlot(PlotId, SourceId, String),
    HideSeries(SourceId, String),
}

pub struct App {
    project: Project,
    plots: Plots,
    tree: egui_tiles::Tree<Pane>,
    pane_tiles: HashMap<Pane, TileId>,
    timeline: Timeline,
    video_workers: HashMap<SourceId, VideoWorker>,
    audio_players: HashMap<SourceId, AudioPlayerSlot>,
    import_tx: Sender<ImportOutcome>,
    import_rx: Receiver<ImportOutcome>,
    importing: usize,
    status: Option<String>,
    series_filter: String,
    last_update: Instant,
    ffmpeg_available: bool,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_files: Vec<PathBuf>) -> Self {
        let (import_tx, import_rx) = channel();
        let mut app = Self {
            project: Project::new(),
            plots: Plots::default(),
            tree: egui_tiles::Tree::empty("root"),
            pane_tiles: HashMap::new(),
            timeline: Timeline::new((0.0, 1.0)),
            video_workers: HashMap::new(),
            audio_players: HashMap::new(),
            import_tx,
            import_rx,
            importing: 0,
            status: None,
            series_filter: String::new(),
            last_update: Instant::now(),
            ffmpeg_available: import::video::ffmpeg_available(),
        };
        for path in initial_files {
            app.start_import(path);
        }
        app
    }

    fn start_import(&mut self, path: PathBuf) {
        self.importing += 1;
        let tx = self.import_tx.clone();
        std::thread::spawn(move || {
            let result = import::import_path(&path).map_err(|e| format!("{e:#}"));
            let _ = tx.send(ImportOutcome { path, result });
        });
    }

    fn poll_imports(&mut self) {
        while let Ok(outcome) = self.import_rx.try_recv() {
            self.importing = self.importing.saturating_sub(1);
            match outcome.result {
                Ok((name, kind)) => self.add_source(outcome.path, name, kind),
                Err(e) => self.status = Some(format!("Import failed for {}: {e}", outcome.path.display())),
            }
        }
    }

    fn add_source(&mut self, path: PathBuf, name: String, kind: SourceKind) {
        let id = self.project.alloc_id();
        let idx = self.project.sources.len();
        let color = color_for_index(idx);

        // Media panes are immediately useful, so show them right away. Log
        // series start hidden -- a single tlog can carry hundreds of fields,
        // so the user opts in via the source browser checkboxes.
        let pane_to_add = match &kind {
            SourceKind::Video(_) => Some(Pane::Video(id)),
            SourceKind::Audio(_) => Some(Pane::Audio(id)),
            SourceKind::Log(_) => None,
        };

        self.project.sources.push(Source {
            id,
            name: name.clone(),
            path,
            offset_seconds: 0.0,
            color,
            enabled: true,
            kind,
        });
        if let Some(pane) = pane_to_add {
            self.add_pane(pane);
        }

        if let Some(bounds) = self.project.time_bounds() {
            if self.project.sources.len() == 1 {
                self.timeline = Timeline::new(bounds);
            } else {
                self.timeline.view_start = self.timeline.view_start.min(bounds.0);
                self.timeline.view_end = self.timeline.view_end.max(bounds.1);
            }
        }
        self.status = Some(format!("Imported {name}"));
    }

    fn add_pane(&mut self, pane: Pane) {
        if self.pane_tiles.contains_key(&pane) {
            return;
        }
        let tile_id = self.tree.tiles.insert_pane(pane.clone());
        match self.tree.root {
            Some(root) => match self.tree.tiles.get_mut(root) {
                Some(Tile::Container(container)) => container.add_child(tile_id),
                _ => {
                    let grid = self.tree.tiles.insert_grid_tile(vec![root, tile_id]);
                    self.tree.root = Some(grid);
                }
            },
            None => self.tree.root = Some(tile_id),
        }
        self.pane_tiles.insert(pane, tile_id);
    }

    fn remove_pane(&mut self, pane: &Pane) {
        if let Some(tile_id) = self.pane_tiles.remove(pane) {
            match self.tree.tiles.parent_of(tile_id) {
                Some(parent_id) => {
                    if let Some(Tile::Container(container)) = self.tree.tiles.get_mut(parent_id) {
                        container.remove_child(tile_id);
                    }
                }
                None => {
                    // `tile_id` had no parent, i.e. it *was* the tree root
                    // (the lone pane in an otherwise-empty tree).
                    self.tree.root = None;
                }
            }
            self.tree.tiles.remove(tile_id);
        }
        if let Pane::Plot(id) = pane {
            self.plots.close(*id);
        }
    }

    fn apply(&mut self, action: PendingAction) {
        match action {
            PendingAction::AddPane(pane) => self.add_pane(pane),
            PendingAction::RemovePane(pane) => self.remove_pane(&pane),
            PendingAction::NewPlot(source, series) => {
                let id = self.plots.create(source, series);
                self.add_pane(Pane::Plot(id));
            }
            PendingAction::AddToPlot(plot, source, series) => {
                self.plots.add(plot, source, series);
                // The plot may exist without a pane if its tab was closed.
                self.add_pane(Pane::Plot(plot));
            }
            PendingAction::HideSeries(source, series) => {
                for id in self.plots.remove_series(source, &series) {
                    self.remove_pane(&Pane::Plot(id));
                }
            }
        }
    }

    fn source_browser(&mut self, ui: &mut egui::Ui) {
        ui.heading("rapid-analyzer");
        ui.horizontal(|ui| {
            if ui.button("+ Import file...").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_file() {
                    self.start_import(path);
                }
            if self.importing > 0 {
                ui.spinner();
            }
        });
        if !self.ffmpeg_available {
            ui.colored_label(egui::Color32::YELLOW, "⚠ ffmpeg not found -- video/audio import will fail");
        }
        if let Some(status) = self.status.clone() {
            ui.small(status);
        }
        ui.separator();

        if self.project.sources.is_empty() {
            ui.weak("No sources yet. Import a .tlog, a sensor SQLite log, or a video/audio file.");
            return;
        }

        ui.horizontal(|ui| {
            ui.label("Filter series");
            ui.text_edit_singleline(&mut self.series_filter);
            if !self.series_filter.is_empty() && ui.small_button("✖").clicked() {
                self.series_filter.clear();
            }
        });
        ui.separator();

        let mut pending: Vec<PendingAction> = Vec::new();
        let filter = self.series_filter.to_lowercase();

        egui::ScrollArea::vertical().show(ui, |ui| {
            let sources = &mut self.project.sources;
            let pane_tiles = &self.pane_tiles;
            let plots = &self.plots;
            for source in sources.iter_mut() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut source.enabled, "");
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, source.color);
                    ui.strong(&source.name);
                    let kind_tag = match &source.kind {
                        SourceKind::Log(log) => match log.format {
                            crate::model::LogFormat::Tlog => "tlog",
                            crate::model::LogFormat::SqliteLog => "sqlite",
                        },
                        SourceKind::Video(_) => "video",
                        SourceKind::Audio(_) => "audio",
                    };
                    ui.weak(format!("[{kind_tag}]"));
                });
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.label("offset");
                    ui.add(
                        egui::DragValue::new(&mut source.offset_seconds)
                            .speed(0.01)
                            .suffix(" s")
                            .max_decimals(3),
                    );
                });

                if !source.enabled {
                    ui.separator();
                    continue;
                }

                match &source.kind {
                    SourceKind::Log(log) => {
                        egui::CollapsingHeader::new(format!("series ({})", log.series.len()))
                            .id_salt(source.id)
                            .default_open(log.series.len() <= 12)
                            .show(ui, |ui| {
                                series_browser(ui, source.id, &log.series, plots, &filter, &mut pending);
                            });
                    }
                    SourceKind::Video(v) => {
                        let pane = Pane::Video(source.id);
                        let mut shown = pane_tiles.contains_key(&pane);
                        if ui
                            .checkbox(&mut shown, format!("video panel ({:.1}s, {}x{} @{:.0}fps)", v.duration, v.width, v.height, v.fps))
                            .changed()
                        {
                            pending.push(if shown { PendingAction::AddPane(pane) } else { PendingAction::RemovePane(pane) });
                        }
                    }
                    SourceKind::Audio(a) => {
                        let pane = Pane::Audio(source.id);
                        let mut shown = pane_tiles.contains_key(&pane);
                        if ui.checkbox(&mut shown, format!("audio panel ({:.1}s)", a.duration)).changed() {
                            pending.push(if shown { PendingAction::AddPane(pane) } else { PendingAction::RemovePane(pane) });
                        }
                    }
                }
                ui.separator();
            }
        });

        for action in pending {
            self.apply(action);
        }
    }
}

/// The series list for one log source, grouped by message so a few hundred
/// tlog fields stay navigable.
fn series_browser(
    ui: &mut egui::Ui,
    source: SourceId,
    series: &[crate::series::TimeSeries],
    plots: &Plots,
    filter: &str,
    pending: &mut Vec<PendingAction>,
) {
    let matching: Vec<&crate::series::TimeSeries> = series
        .iter()
        .filter(|s| filter.is_empty() || s.name.to_lowercase().contains(filter))
        .collect();
    if matching.is_empty() {
        ui.weak("no matching series");
        return;
    }

    // The list arrives sorted by name, so each message's fields are already
    // a contiguous run -- no grouping pass needed, just a look at where the
    // prefix changes.
    let mut i = 0;
    while i < matching.len() {
        let group = group_of(&matching[i].name);
        let end = matching[i..]
            .iter()
            .position(|s| group_of(&s.name) != group)
            .map_or(matching.len(), |n| i + n);

        match group {
            // Series without a message prefix (a sqlite sensor log) have
            // nothing to group by; list them directly.
            None => {
                for s in &matching[i..end] {
                    series_row(ui, source, s, &s.name, plots, pending);
                }
            }
            Some(name) => {
                egui::CollapsingHeader::new(format!("{name}  ({})", end - i))
                    .id_salt((source, name))
                    // Searching means the user is already looking at a short
                    // list and wants to see it.
                    .default_open(!filter.is_empty())
                    .show(ui, |ui| {
                        for s in &matching[i..end] {
                            let label = s.name.strip_prefix(name).and_then(|r| r.strip_prefix('.')).unwrap_or(&s.name);
                            series_row(ui, source, s, label, plots, pending);
                        }
                    });
            }
        }
        i = end;
    }
}

fn series_row(
    ui: &mut egui::Ui,
    source: SourceId,
    series: &crate::series::TimeSeries,
    label: &str,
    plots: &Plots,
    pending: &mut Vec<PendingAction>,
) {
    ui.horizontal(|ui| {
        let mut shown = plots.shows(source, &series.name);
        let text = match &series.unit {
            Some(unit) => format!("{label}  [{unit}]"),
            None => label.to_string(),
        };
        if ui
            .checkbox(&mut shown, text)
            .on_hover_text(format!("{} ({} samples)", series.name, series.len()))
            .changed()
        {
            pending.push(if shown {
                PendingAction::NewPlot(source, series.name.clone())
            } else {
                PendingAction::HideSeries(source, series.name.clone())
            });
        }

        // Adding to an *existing* graph is what makes two series comparable,
        // so it lives in the list next to the checkbox rather than buried in
        // the graph itself.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button("➕", |ui| {
                ui.set_min_width(220.0);
                if ui.button("New plot").clicked() {
                    pending.push(PendingAction::NewPlot(source, series.name.clone()));
                    ui.close();
                }
                let targets: Vec<_> = plots.iter().filter(|p| !p.contains(source, &series.name)).collect();
                if !targets.is_empty() {
                    ui.separator();
                    ui.weak("Add to");
                    for plot in targets {
                        if ui.button(plot.title()).clicked() {
                            pending.push(PendingAction::AddToPlot(plot.id, source, series.name.clone()));
                            ui.close();
                        }
                    }
                }
            })
            .response
            .on_hover_text("Plot this together with another series");
        });
    });
}

/// `"PRESSURE_VESSEL[1].pressure1"` -> `Some("PRESSURE_VESSEL[1]")`.
fn group_of(name: &str) -> Option<&str> {
    name.rfind('.').map(|dot| &name[..dot])
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_imports();

        let now = Instant::now();
        let dt = (now - self.last_update).as_secs_f64().min(0.25);
        self.last_update = now;
        self.timeline.tick(dt, self.project.time_bounds());
        if self.timeline.playing || self.importing > 0 {
            ctx.request_repaint();
        }

        egui::Panel::left("source_browser").min_size(280.0).default_size(320.0).show(ui, |ui| {
            self.source_browser(ui);
        });

        egui::Panel::top("timeline_bar").show(ui, |ui| {
            timeline::show(ui, &mut self.timeline, self.project.time_bounds());
        });

        let mut closed = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            if self.tree.tiles.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak(
                        "Import data and tick series/panels in the left sidebar to display them here.\n\
                         Use ➕ next to a series to draw it in an existing plot.\n\
                         Drag tabs to rearrange; drop on an edge to split.",
                    );
                });
                return;
            }
            let mut behavior = TreeBehavior {
                project: &mut self.project,
                plots: &mut self.plots,
                timeline: &mut self.timeline,
                video_workers: &mut self.video_workers,
                audio_players: &mut self.audio_players,
                closed: Vec::new(),
            };
            self.tree.ui(&mut behavior, ui);
            closed = behavior.closed;
        });
        for pane in closed {
            self.remove_pane(&pane);
        }
    }
}
