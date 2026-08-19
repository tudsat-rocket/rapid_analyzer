use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use egui_tiles::{Tile, TileId};

use crate::can_builder::{BuilderAction, CanBuilder};
use crate::colors::color_for_index;
use crate::import;
use crate::model::{Project, Source, SourceId, SourceKind};
use crate::panes::{AudioPlayerSlot, Pane, PlotAxis, PlotId, Plots, TreeBehavior};
use crate::timeline::{self, Timeline};
use crate::vapor::Vapors;
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
    AddToPlot(PlotId, SourceId, String, PlotAxis),
    HideSeries(SourceId, String),
    RemoveSource(SourceId),
    OpenCanBuilder(SourceId),
    NewVaporPane,
}

pub struct App {
    project: Project,
    plots: Plots,
    vapors: Vapors,
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
    /// The CAN signal picker, while it is open. At most one at a time -- it
    /// is a modal-ish tool, not a per-source panel.
    can_builder: Option<CanBuilder>,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_files: Vec<PathBuf>) -> Self {
        let (import_tx, import_rx) = channel();
        let mut app = Self {
            project: Project::new(),
            plots: Plots::default(),
            vapors: Vapors::default(),
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
            can_builder: None,
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
                self.timeline.reset_to(bounds);
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
        match pane {
            Pane::Plot(id) => self.plots.close(*id),
            Pane::Vapor(id) => self.vapors.close(*id),
            _ => {}
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
            PendingAction::AddToPlot(plot, source, series, axis) => {
                self.plots.add(plot, source, series, axis);
                // The plot may exist without a pane if its tab was closed.
                self.add_pane(Pane::Plot(plot));
            }
            PendingAction::HideSeries(source, series) => {
                for id in self.plots.remove_series(source, &series) {
                    self.remove_pane(&Pane::Plot(id));
                }
            }
            PendingAction::RemoveSource(source) => self.remove_source(source),
            PendingAction::NewVaporPane => {
                let id = self.vapors.create(&self.project);
                self.add_pane(Pane::Vapor(id));
            }
            PendingAction::OpenCanBuilder(source) => {
                if let Some(SourceKind::Log(log)) = self.project.source(source).map(|s| &s.kind) {
                    self.can_builder = Some(CanBuilder::new(source, &log.can));
                }
            }
        }
    }

    /// The CAN signal picker, and what it hands back: a series the log didn't
    /// name itself, which joins that source's series list as if it had.
    fn can_signal_builder(&mut self, ctx: &egui::Context) {
        // Taken out for the duration so the window can read the source it
        // belongs to while it draws.
        let Some(mut builder) = self.can_builder.take() else {
            return;
        };
        let source_id = builder.source();
        let Some(source) = self.project.source(source_id) else {
            return;
        };
        let SourceKind::Log(log) = &source.kind else {
            return;
        };

        let action = builder.show(ctx, &source.name, &log.can);
        match action {
            BuilderAction::None => self.can_builder = Some(builder),
            BuilderAction::Close => {}
            BuilderAction::Plot(series) => {
                let name = series.name.clone();
                self.add_series(source_id, series);
                let id = self.plots.create(source_id, name.clone());
                self.add_pane(Pane::Plot(id));
                self.status = Some(format!("Added {name}"));
                self.can_builder = Some(builder);
            }
        }
    }

    /// Adds (or replaces) a series on a log source, keeping the list sorted
    /// by name so the sidebar's grouping by message prefix still holds.
    fn add_series(&mut self, source_id: SourceId, series: crate::series::TimeSeries) {
        let Some(source) = self.project.sources.iter_mut().find(|s| s.id == source_id) else {
            return;
        };
        let SourceKind::Log(log) = &mut source.kind else {
            return;
        };
        match log.series.iter_mut().find(|s| s.name == series.name) {
            // Re-adding under the same name is the user refining a signal
            // (a scale factor, a different byte); replace it in place so the
            // graph already showing it picks the change up.
            Some(existing) => *existing = series,
            None => {
                log.series.push(series);
                log.series.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }
    }

    /// Unloads a source and everything that was showing it.
    ///
    /// Forgetting the workers is what actually stops the work: dropping a
    /// `VideoWorker` closes its request channel, which ends the decode thread
    /// and with it the `ffmpeg` it was running, and dropping an
    /// `AudioPlayback` drops the rodio sink that was making the sound.
    fn remove_source(&mut self, id: SourceId) {
        self.remove_pane(&Pane::Video(id));
        self.remove_pane(&Pane::Audio(id));
        for plot in self.plots.remove_source(id) {
            self.remove_pane(&Pane::Plot(plot));
        }
        // A phase pane outlives the log it was pointed at: the curve is still
        // worth looking at, and it can be pointed at another one.
        self.vapors.forget_source(id);
        self.video_workers.remove(&id);
        self.audio_players.remove(&id);

        let name = self.project.source(id).map(|s| s.name.clone());
        self.project.sources.retain(|s| s.id != id);

        // The timeline was sized to include this source; without it, the
        // window and playhead can be left pointing at nothing.
        match self.project.time_bounds() {
            Some(bounds) => self.timeline.clamp_to(Some(bounds)),
            None => self.timeline.reset_to((0.0, 1.0)),
        }
        if let Some(name) = name {
            self.status = Some(format!("Removed {name}"));
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
        // Applied after the borrow of `self.project` below is over.
        let mut new_vapor_pane = false;
        // Offered even with nothing imported: the vapour pressure curve is
        // worth looking at on its own.
        if ui
            .button("＋ N₂O phase plot")
            .on_hover_text(
                "The nitrous oxide vapour pressure curve, and where a temperature/pressure pair sits against it",
            )
            .clicked()
        {
            new_vapor_pane = true;
        }
        if !self.ffmpeg_available {
            ui.colored_label(egui::Color32::YELLOW, "⚠ ffmpeg not found -- video/audio import will fail");
        }
        if let Some(status) = self.status.clone() {
            ui.small(status);
        }
        ui.separator();

        if new_vapor_pane {
            self.apply(PendingAction::NewVaporPane);
        }
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
            // Nothing in this panel may size itself by its text: egui gives a
            // panel the width its contents ask for, so one long series name
            // (or file name) would hand the sidebar half the window and keep
            // it there. Everything truncates; the source name, which is a
            // thing the user picked and wants to read, wraps instead.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);

            let sources = &mut self.project.sources;
            let pane_tiles = &self.pane_tiles;
            let plots = &self.plots;
            for source in sources.iter_mut() {
                source_header(ui, source, &mut pending);
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
                        // The protocol-decoded signals are already in the
                        // list above; this is for everything else on the bus.
                        if !log.can.is_empty() {
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.weak(format!("{} CAN frames", log.can.len()));
                                if ui
                                    .small_button("＋ signal…")
                                    .on_hover_text("Plot any byte range of any CAN identifier in this log")
                                    .clicked()
                                {
                                    pending.push(PendingAction::OpenCanBuilder(source.id));
                                }
                            });
                        }
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

    /// Transport controls on the keyboard, so reviewing a run doesn't mean
    /// hunting for a toolbar button between every step.
    ///
    /// Ignored while a text field has focus -- typing "b" into the series
    /// filter must not toggle box zoom.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let bounds = self.project.time_bounds();
        ctx.input(|i| {
            // Shift for a coarse step, Alt for a fine one: the same gesture
            // at three scales rather than three keys.
            let step = if i.modifiers.shift {
                10.0
            } else if i.modifiers.alt {
                0.1
            } else {
                1.0
            };
            for event in &i.events {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    repeat,
                    modifiers,
                    ..
                } = event
                else {
                    continue;
                };
                // Held arrows should scrub; a held toggle should not flicker.
                let toggle = matches!(key, egui::Key::Space | egui::Key::B | egui::Key::R);
                if *repeat && toggle {
                    continue;
                }
                match key {
                    egui::Key::Space => self.timeline.playing = !self.timeline.playing,
                    egui::Key::ArrowLeft => {
                        self.timeline.step(-step, bounds);
                        self.timeline.playing = false;
                    }
                    egui::Key::ArrowRight => {
                        self.timeline.step(step, bounds);
                        self.timeline.playing = false;
                    }
                    egui::Key::Home => self.timeline.seek(bounds.map_or(0.0, |(lo, _)| lo), bounds),
                    egui::Key::End => self.timeline.seek(bounds.map_or(0.0, |(_, hi)| hi), bounds),
                    egui::Key::ArrowUp => self.timeline.speed = next_speed(self.timeline.speed, 1),
                    egui::Key::ArrowDown => self.timeline.speed = next_speed(self.timeline.speed, -1),
                    // Zoom about the playhead: it is what the user is
                    // looking at, and it keeps the moment of interest on
                    // screen however far in they go.
                    egui::Key::Plus | egui::Key::Equals => self.timeline.zoom_view(0.5, self.timeline.cursor),
                    egui::Key::Minus => self.timeline.zoom_view(2.0, self.timeline.cursor),
                    egui::Key::R if modifiers.is_none() => {
                        self.timeline.reset_view(bounds);
                        self.plots.clear_manual_ranges();
                    }
                    egui::Key::B if modifiers.is_none() => self.timeline.box_zoom = !self.timeline.box_zoom,
                    _ => {}
                }
            }
        });
    }
}

/// The next playback speed up (`direction` 1) or down the ladder.
fn next_speed(current: f32, direction: i32) -> f32 {
    let speeds = timeline::SPEEDS;
    let index = speeds
        .iter()
        .position(|s| (s - current).abs() < 1e-6)
        .unwrap_or(speeds.len() / 2) as i32;
    speeds[(index + direction).clamp(0, speeds.len() as i32 - 1) as usize]
}

/// The header row for one source: enable, colour, name, kind, unload.
///
/// The name is the one thing in the sidebar that is arbitrarily long, so the
/// trailing controls are laid out first (right to left) and the name wraps
/// into whatever width is left over. Laying it out the other way round would
/// let a long file name push the panel wider than the window.
fn source_header(ui: &mut egui::Ui, source: &mut Source, pending: &mut Vec<PendingAction>) {
    let kind_tag = match &source.kind {
        SourceKind::Log(log) => match log.format {
            crate::model::LogFormat::Tlog => "tlog",
            crate::model::LogFormat::SqliteLog => "sqlite",
        },
        SourceKind::Video(_) => "video",
        SourceKind::Audio(_) => "audio",
    };

    ui.horizontal(|ui| {
        ui.checkbox(&mut source.enabled, "")
            .on_hover_text("Show this source in the graphs and panels");
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, source.color);

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("✖")
                .on_hover_text("Unload this source, and close every graph and panel showing it")
                .clicked()
            {
                pending.push(PendingAction::RemoveSource(source.id));
            }
            ui.weak(format!("[{kind_tag}]"));
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&source.name).strong()).wrap())
                    .on_hover_text(source.path.display().to_string());
            });
        });
    });
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
                        ui.horizontal(|ui| {
                            if ui.button(plot.title()).clicked() {
                                pending.push(PendingAction::AddToPlot(
                                    plot.id,
                                    source,
                                    series.name.clone(),
                                    PlotAxis::Left,
                                ));
                                ui.close();
                            }
                            // Its own axis is what makes a series with a
                            // wildly different scale -- thrust next to
                            // pressure -- readable in the same graph.
                            if ui
                                .small_button("→R")
                                .on_hover_text("Add on that plot's right-hand value axis")
                                .clicked()
                            {
                                pending.push(PendingAction::AddToPlot(
                                    plot.id,
                                    source,
                                    series.name.clone(),
                                    PlotAxis::Right,
                                ));
                                ui.close();
                            }
                        });
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
        self.handle_shortcuts(&ctx);

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

        self.can_signal_builder(&ctx);

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
                vapors: &mut self.vapors,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LogFormat, LogSource};

    fn log_source(name: &str) -> Source {
        Source {
            id: 0,
            name: name.to_string(),
            path: name.into(),
            offset_seconds: 0.0,
            color: egui::Color32::WHITE,
            enabled: true,
            kind: SourceKind::Log(LogSource {
                series: Vec::new(),
                format: LogFormat::Tlog,
                can: Default::default(),
            }),
        }
    }

    /// Lays out one source header inside a panel `width` wide, and reports the
    /// size it asked for.
    fn header_size(name: &str, width: f32) -> egui::Vec2 {
        let ctx = egui::Context::default();
        let mut source = log_source(name);
        let mut pending = Vec::new();
        let mut size = egui::Vec2::ZERO;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0))),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            let rect = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(width, 800.0));
            let scope = ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                source_header(ui, &mut source, &mut pending);
            });
            size = scope.response.rect.size();
        })
        .drop_without_applying_deltas();
        size
    }

    /// egui hands a panel the width its contents ask for, so a source name
    /// that refuses to wrap makes the sidebar as wide as the longest file name
    /// anyone ever imported -- and it stays that way, because the size is
    /// persisted.
    #[test]
    fn a_long_source_name_wraps_instead_of_widening_the_sidebar() {
        const WIDTH: f32 = 320.0;
        let short = header_size("run.tlog", WIDTH);
        let long = header_size(
            "2026-08-08T17-41-58Z_static-fire-04_oxidizer-tank-and-chamber-instrumentation_recovered-from-the-backup-logger.tlog",
            WIDTH,
        );
        assert!(long.x <= WIDTH + 1.0, "the long name asked for {} px of a {WIDTH} px panel", long.x);
        assert!(
            long.y > short.y,
            "the long name should have wrapped onto more lines ({} vs {})",
            long.y,
            short.y
        );
    }
}
