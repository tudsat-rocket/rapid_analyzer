use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use egui_tiles::{Tile, TileId};

use crate::colors::color_for_index;
use crate::import;
use crate::model::{Project, Source, SourceId, SourceKind};
use crate::panes::{AudioPlayerSlot, Pane, TreeBehavior};
use crate::timeline::{self, Timeline};
use crate::video_worker::VideoWorker;

struct ImportOutcome {
    path: PathBuf,
    result: Result<(String, SourceKind), String>,
}

enum PendingAction {
    Add(Pane),
    Remove(Pane),
}

pub struct App {
    project: Project,
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
    }

    fn source_browser(&mut self, ui: &mut egui::Ui) {
        ui.heading("rapid-analyzer");
        ui.horizontal(|ui| {
            if ui.button("+ Import file...").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    self.start_import(path);
                }
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
        });
        ui.separator();

        let mut pending: Vec<PendingAction> = Vec::new();
        let filter = self.series_filter.to_lowercase();

        egui::ScrollArea::vertical().show(ui, |ui| {
            let sources = &mut self.project.sources;
            let pane_tiles = &self.pane_tiles;
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
                                for series in &log.series {
                                    if !filter.is_empty() && !series.name.to_lowercase().contains(&filter) {
                                        continue;
                                    }
                                    let pane = Pane::Plot {
                                        source: source.id,
                                        series: series.name.clone(),
                                    };
                                    let mut shown = pane_tiles.contains_key(&pane);
                                    if ui.checkbox(&mut shown, &series.name).changed() {
                                        pending.push(if shown {
                                            PendingAction::Add(pane)
                                        } else {
                                            PendingAction::Remove(pane)
                                        });
                                    }
                                }
                            });
                    }
                    SourceKind::Video(v) => {
                        let pane = Pane::Video(source.id);
                        let mut shown = pane_tiles.contains_key(&pane);
                        if ui
                            .checkbox(&mut shown, format!("video panel ({:.1}s, {}x{} @{:.0}fps)", v.duration, v.width, v.height, v.fps))
                            .changed()
                        {
                            pending.push(if shown { PendingAction::Add(pane) } else { PendingAction::Remove(pane) });
                        }
                    }
                    SourceKind::Audio(a) => {
                        let pane = Pane::Audio(source.id);
                        let mut shown = pane_tiles.contains_key(&pane);
                        if ui.checkbox(&mut shown, format!("audio panel ({:.1}s)", a.duration)).changed() {
                            pending.push(if shown { PendingAction::Add(pane) } else { PendingAction::Remove(pane) });
                        }
                    }
                }
                ui.separator();
            }
        });

        for action in pending {
            match action {
                PendingAction::Add(p) => self.add_pane(p),
                PendingAction::Remove(p) => self.remove_pane(&p),
            }
        }
    }
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

        egui::CentralPanel::default().show(ui, |ui| {
            if self.tree.tiles.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("Import data and check series/panels in the left sidebar to display them here.\nDrag tabs to rearrange; drop on an edge to split.");
                });
                return;
            }
            let mut behavior = TreeBehavior {
                project: &mut self.project,
                timeline: &mut self.timeline,
                video_workers: &mut self.video_workers,
                audio_players: &mut self.audio_players,
            };
            self.tree.ui(&mut behavior, ui);
        });
    }
}
