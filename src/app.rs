use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui_tiles::{Tile, TileId};
use web_time::Instant;

use crate::can_builder::{BuilderAction, CanBuilder};
use crate::canopen_inspector::{CanOpenInspector, InspectorAction};
use crate::colors::color_for_index;
use crate::export::ExportItem;
use crate::export::dialog::{DialogContext, ExportDialog};
use crate::import;
use crate::model::{Project, Source, SourceId, SourceKind};
use crate::panes::{AudioPlayerSlot, Pane, PlotAxis, PlotId, Plots, TreeBehavior};
use crate::session::{self, Session};
use crate::tank::Tanks;
use crate::timeline::{self, Timeline};
use crate::vapor::Vapors;
use crate::video_worker::VideoWorker;

struct ImportOutcome {
    path: PathBuf,
    result: Result<(String, SourceKind), String>,
}

/// A session being reopened while the files it needs are still importing.
/// Its graphs and markers go in once the last of them has come back (or
/// failed to), so a graph never opens half-filled.
struct PendingSession {
    session: Session,
    /// What each of `session.sources` turned out to be.
    resolved: Vec<Option<SourceId>>,
    /// Imports still out, by the index of the saved source they are for.
    waiting: Vec<(usize, PathBuf)>,
    /// Saved sources that couldn't be found or imported.
    missing: Vec<String>,
    label: String,
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
    OpenCanOpen(SourceId),
    NewVaporPane,
    NewTankPane,
}

pub struct App {
    project: Project,
    plots: Plots,
    vapors: Vapors,
    tanks: Tanks,
    tree: egui_tiles::Tree<Pane>,
    pane_tiles: HashMap<Pane, TileId>,
    timeline: Timeline,
    video_workers: HashMap<SourceId, VideoWorker>,
    audio_players: HashMap<SourceId, AudioPlayerSlot>,
    import_tx: Sender<ImportOutcome>,
    import_rx: Receiver<ImportOutcome>,
    importing: usize,
    /// Files the browser has read -- picked or dropped -- waiting to be
    /// parsed. They are parsed on the UI thread (there is no other), one
    /// frame after they arrive, so the spinner is already on screen while it
    /// happens.
    #[cfg(target_arch = "wasm32")]
    queued: Vec<(String, Vec<u8>)>,
    /// Files as the browser finishes reading them. Reading is asynchronous,
    /// so they come back over a channel, like a native import.
    #[cfg(target_arch = "wasm32")]
    picked_tx: Sender<(String, Result<Vec<u8>, String>)>,
    #[cfg(target_arch = "wasm32")]
    picked_rx: Receiver<(String, Result<Vec<u8>, String>)>,
    status: Option<String>,
    series_filter: String,
    last_update: Instant,
    #[cfg(feature = "media")]
    ffmpeg_available: bool,
    /// The CAN signal picker, while it is open. At most one at a time -- it
    /// is a modal-ish tool, not a per-source panel.
    can_builder: Option<CanBuilder>,
    /// The CANopen inspector, while it is open. One at a time, like the
    /// signal picker; opening it for another log replaces it.
    canopen: Option<CanOpenInspector>,
    /// The figure exporter's window. It keeps its settings between openings,
    /// so a report's second figure is one click away from its first.
    export: ExportDialog,
    /// Light, dark, or whatever the desktop is set to. Applied to the context
    /// every frame, since egui holds it in its own options rather than here.
    theme: egui::ThemePreference,
    pending_session: Option<PendingSession>,
    /// Where the session was last saved to or opened from, so Ctrl+S saves
    /// back to it without asking.
    #[cfg(not(target_arch = "wasm32"))]
    session_path: Option<PathBuf>,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_files: Vec<PathBuf>) -> Self {
        Self::with_files(initial_files)
    }

    /// Everything [`Self::new`] does that doesn't need a window.
    fn with_files(initial_files: Vec<PathBuf>) -> Self {
        let (import_tx, import_rx) = channel();
        #[cfg(target_arch = "wasm32")]
        let (picked_tx, picked_rx) = channel();
        let mut app = Self {
            project: Project::new(),
            plots: Plots::default(),
            vapors: Vapors::default(),
            tanks: Tanks::default(),
            tree: egui_tiles::Tree::empty("root"),
            pane_tiles: HashMap::new(),
            timeline: Timeline::new((0.0, 1.0)),
            video_workers: HashMap::new(),
            audio_players: HashMap::new(),
            import_tx,
            import_rx,
            importing: 0,
            #[cfg(target_arch = "wasm32")]
            queued: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            picked_tx,
            #[cfg(target_arch = "wasm32")]
            picked_rx,
            status: None,
            series_filter: String::new(),
            last_update: Instant::now(),
            #[cfg(feature = "media")]
            ffmpeg_available: import::video::ffmpeg_available(),
            can_builder: None,
            canopen: None,
            export: ExportDialog::default(),
            theme: egui::ThemePreference::System,
            pending_session: None,
            #[cfg(not(target_arch = "wasm32"))]
            session_path: None,
        };
        for path in initial_files {
            app.start_import(path);
        }
        app
    }

    fn start_import(&mut self, path: PathBuf) {
        if session::is_session_file(&path.to_string_lossy()) {
            self.open_session_file(path);
            return;
        }
        self.importing += 1;
        let tx = self.import_tx.clone();
        let import = move || {
            let result = import::import_path(&path).map_err(|e| format!("{e:#}"));
            let _ = tx.send(ImportOutcome { path, result });
        };
        // There is no file system to read a path from in the browser either,
        // so this only fails there -- but it fails with a message rather
        // than the panic `thread::spawn` would be.
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(import);
        #[cfg(target_arch = "wasm32")]
        import();
    }

    /// Imports a file that arrived as its contents rather than a path.
    #[cfg(target_arch = "wasm32")]
    fn queue_bytes(&mut self, name: String, bytes: Vec<u8>) {
        if session::is_session_file(&name) {
            match Session::from_json(&bytes) {
                Ok(session) => self.open_session(session, None, name),
                Err(e) => self.status = Some(format!("Couldn't open {name}: {e:#}")),
            }
            return;
        }
        self.importing += 1;
        self.status = Some(format!("Reading {name}…"));
        self.queued.push((name, bytes));
    }

    /// The desktop's file dialog: blocks, then imports in the background.
    #[cfg(not(target_arch = "wasm32"))]
    fn pick_file(&mut self, _ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.start_import(path);
        }
    }

    /// The browser's file picker, which only ever answers asynchronously --
    /// and may not answer at all if it is cancelled, which is why nothing
    /// counts as importing until a file has actually been read.
    #[cfg(target_arch = "wasm32")]
    fn pick_file(&mut self, ctx: &egui::Context) {
        let tx = self.picked_tx.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(file) = rfd::AsyncFileDialog::new().pick_file().await {
                let bytes = file.read().await;
                let _ = tx.send((file.file_name(), Ok(bytes)));
                ctx.request_repaint();
            }
        });
    }

    /// Files dropped onto the window. On the desktop they are paths; in the
    /// browser, a handle whose contents can only be read asynchronously.
    fn take_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        for file in dropped {
            #[cfg(not(target_arch = "wasm32"))]
            self.start_import(file.path().to_path_buf());
            #[cfg(target_arch = "wasm32")]
            {
                let tx = self.picked_tx.clone();
                let ctx = ctx.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let name = file.path().to_string_lossy().to_string();
                    let _ = tx.send((name, file.bytes_async().await));
                    ctx.request_repaint();
                });
            }
        }
    }

    fn poll_imports(&mut self) {
        // Queued on an earlier frame, which has drawn the spinner since.
        #[cfg(target_arch = "wasm32")]
        for (name, bytes) in std::mem::take(&mut self.queued) {
            let result = import::import_bytes(&name, &bytes).map_err(|e| format!("{e:#}"));
            let _ = self.import_tx.send(ImportOutcome {
                path: PathBuf::from(name),
                result,
            });
        }
        #[cfg(target_arch = "wasm32")]
        while let Ok((name, read)) = self.picked_rx.try_recv() {
            match read {
                Ok(bytes) => self.queue_bytes(name, bytes),
                Err(e) => self.status = Some(format!("Couldn't read {name}: {e}")),
            }
        }
        while let Ok(outcome) = self.import_rx.try_recv() {
            self.importing = self.importing.saturating_sub(1);
            // An import a reopened session asked for.
            let for_session = self
                .pending_session
                .as_ref()
                .and_then(|p| p.waiting.iter().position(|(_, path)| *path == outcome.path));
            let added = match outcome.result {
                Ok((name, kind)) => Some(self.add_source(outcome.path, name, kind)),
                Err(e) => {
                    self.status = Some(format!("Import failed for {}: {e}", outcome.path.display()));
                    None
                }
            };
            if let (Some(at), Some(pending)) = (for_session, self.pending_session.as_mut()) {
                let (index, _) = pending.waiting.remove(at);
                let saved = &pending.session.sources[index];
                match added {
                    Some(id) => {
                        pending.resolved[index] = Some(id);
                        if let Some(source) = self.project.sources.iter_mut().find(|s| s.id == id) {
                            source.offset_seconds = saved.offset_seconds;
                            source.enabled = saved.enabled;
                        }
                    }
                    None => pending.missing.push(saved.name.clone()),
                }
                if pending.waiting.is_empty() {
                    self.finish_session();
                }
            }
        }
    }

    fn add_source(&mut self, path: PathBuf, name: String, kind: SourceKind) -> SourceId {
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
        id
    }

    /// The state worth keeping, as a session file's contents.
    fn capture_session(&self) -> Session {
        let open = self
            .plots
            .iter()
            .filter(|plot| self.pane_tiles.contains_key(&Pane::Plot(plot.id)));
        Session::capture(&self.project, open, &self.timeline)
    }

    /// Saves to the file the session came from, or asks where.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_session(&mut self, ask: bool) {
        let path = match (&self.session_path, ask) {
            (Some(path), false) => path.clone(),
            _ => {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("rapid-analyzer session", &[session::EXTENSION])
                    .set_file_name(session::default_file_name(&self.project));
                if let Some(dir) = self.session_path.as_ref().and_then(|p| p.parent()) {
                    dialog = dialog.set_directory(dir);
                } else if let Some(dir) = self.project.sources.first().and_then(|s| s.path.parent()) {
                    dialog = dialog.set_directory(dir);
                }
                let Some(path) = dialog.save_file() else {
                    return;
                };
                // A dialog filtering on an extension does not always add it.
                if session::is_session_file(&path.to_string_lossy()) {
                    path
                } else {
                    path.with_extension(session::EXTENSION)
                }
            }
        };
        let result = self
            .capture_session()
            .to_json()
            .and_then(|bytes| Ok(std::fs::write(&path, bytes)?));
        self.status = Some(match result {
            Ok(()) => {
                let status = format!("Saved session to {}", path.display());
                self.session_path = Some(path);
                status
            }
            Err(e) => format!("Couldn't save the session: {e:#}"),
        });
    }

    /// There is nowhere to write to in the browser: the session is a download.
    #[cfg(target_arch = "wasm32")]
    fn save_session(&mut self, _ask: bool) {
        let name = session::default_file_name(&self.project);
        let result = self
            .capture_session()
            .to_json()
            .and_then(|bytes| crate::web::download(&name, &bytes, "application/json"));
        self.status = Some(match result {
            Ok(()) => format!("Downloaded {name}"),
            Err(e) => format!("Couldn't save the session: {e:#}"),
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pick_session(&mut self, _ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().add_filter("rapid-analyzer session", &[session::EXTENSION]);
        if let Some(dir) = self.session_path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open_session_file(path);
        }
    }

    /// Picked like any other file; `queue_bytes` sees the extension.
    #[cfg(target_arch = "wasm32")]
    fn pick_session(&mut self, ctx: &egui::Context) {
        self.pick_file(ctx);
    }

    fn open_session_file(&mut self, path: PathBuf) {
        let read = std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| Session::from_json(&b));
        match read {
            Ok(session) => {
                let label = path.display().to_string();
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.session_path = Some(path.clone());
                }
                self.open_session(session, Some(&path), label);
            }
            Err(e) => self.status = Some(format!("Couldn't open {}: {e:#}", path.display())),
        }
    }

    /// Reopens a saved session: finds or imports its files, then -- once
    /// they are all in -- replaces the graphs and markers with the saved ones.
    /// Sources already loaded stay loaded.
    fn open_session(&mut self, session: Session, session_path: Option<&std::path::Path>, label: String) {
        let mut resolved = vec![None; session.sources.len()];
        // Only ever pushed to where there are paths to import from.
        #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
        let mut waiting = Vec::new();
        let mut missing = Vec::new();
        for (i, saved) in session.sources.iter().enumerate() {
            if let Some(id) = session::find_loaded(saved, &self.project.sources) {
                resolved[i] = Some(id);
                if let Some(source) = self.project.sources.iter_mut().find(|s| s.id == id) {
                    source.offset_seconds = saved.offset_seconds;
                    source.enabled = saved.enabled;
                }
                continue;
            }
            // A browser can't open a path; the file has to be imported first.
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(path) = session::candidate_paths(saved, session_path)
                .into_iter()
                .find(|p| p.is_file())
            {
                // Two saved sources can't be one file, but a file can be
                // listed twice in a hand-edited session.
                if !waiting.iter().any(|(_, p)| *p == path) {
                    waiting.push((i, path.clone()));
                    self.start_import(path);
                }
                continue;
            }
            let _ = session_path;
            missing.push(saved.name.clone());
        }
        let done = waiting.is_empty();
        self.pending_session = Some(PendingSession {
            session,
            resolved,
            waiting,
            missing,
            label,
        });
        if done {
            self.finish_session();
        } else {
            self.status = Some("Opening session: importing its files…".to_string());
        }
    }

    fn finish_session(&mut self) {
        let Some(pending) = self.pending_session.take() else {
            return;
        };
        let session = &pending.session;

        let open: Vec<PlotId> = self.plots.iter().map(|p| p.id).collect();
        for id in open {
            self.remove_pane(&Pane::Plot(id));
        }
        let mut missing_entries = 0;
        let mut opened = 0;
        for plot in &session.plots {
            let (entries, missing) = session.restored_entries(plot, &self.project, &pending.resolved);
            missing_entries += missing;
            if entries.is_empty() {
                continue;
            }
            let id = self.plots.restore(
                entries,
                plot.title.clone(),
                plot.normalize,
                plot.zero_aligned,
                plot.y_manual,
            );
            self.add_pane(Pane::Plot(id));
            opened += 1;
        }

        self.timeline
            .markers
            .restore(session.restored_markers(), session.markers_placed);
        if let Some(view) = session.view {
            self.timeline.set_view(view.start, view.end);
            self.timeline.seek(view.cursor, self.project.time_bounds());
        }

        let mut status = format!(
            "Opened {}: {opened} graph{}, {} marker{}",
            pending.label,
            if opened == 1 { "" } else { "s" },
            self.timeline.markers.len(),
            if self.timeline.markers.len() == 1 { "" } else { "s" },
        );
        if !pending.missing.is_empty() {
            status += &format!(" -- not found: {}", pending.missing.join(", "));
        }
        if missing_entries > 0 {
            status += &format!(" -- {missing_entries} series could not be placed");
        }
        self.status = Some(status);
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
            Pane::Tank(id) => self.tanks.close(*id),
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
            PendingAction::NewTankPane => {
                let id = self.tanks.create(&self.project);
                self.add_pane(Pane::Tank(id));
            }
            PendingAction::OpenCanBuilder(source) => {
                if let Some(SourceKind::Log(log)) = self.project.source(source).map(|s| &s.kind) {
                    self.can_builder = Some(CanBuilder::new(source, &log.can));
                }
            }
            PendingAction::OpenCanOpen(source) => {
                if let Some(SourceKind::Log(log)) = self.project.source(source).map(|s| &s.kind) {
                    self.canopen = Some(CanOpenInspector::new(source, &log.can));
                }
            }
        }
    }

    /// The panes the exporter can offer: the graphs and tanks with a pane
    /// open, in the order they were created.
    fn visible_exports(&self) -> Vec<ExportItem> {
        let plots = self
            .plots
            .iter()
            .map(|plot| plot.id)
            .filter(|id| self.pane_tiles.contains_key(&Pane::Plot(*id)))
            .map(ExportItem::Plot);
        let tanks = self
            .tanks
            .iter()
            .map(|tank| tank.id)
            .filter(|id| self.pane_tiles.contains_key(&Pane::Tank(*id)))
            .map(ExportItem::Tank);
        plots.chain(tanks).collect()
    }

    fn open_export(&mut self) {
        let visible = self.visible_exports();
        self.export.open(&visible);
    }

    /// The export window, and the one thing it hands back: a line saying what
    /// it wrote.
    fn export_window(&mut self, ctx: &egui::Context) {
        if !self.export.is_open() {
            return;
        }
        let visible = self.visible_exports();
        let cx = DialogContext {
            project: &self.project,
            plots: &self.plots,
            tanks: &self.tanks,
            timeline: &self.timeline,
            visible,
        };
        if let Some(status) = self.export.show(ctx, &cx) {
            self.status = Some(status);
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

    /// The CANopen inspector. Like the signal picker, what it hands back is a
    /// series -- one dictionary object's history -- or a place to put the
    /// playhead.
    fn canopen_inspector(&mut self, ctx: &egui::Context) {
        let Some(mut inspector) = self.canopen.take() else {
            return;
        };
        let source_id = inspector.source();
        let Some(source) = self.project.source(source_id) else {
            return;
        };
        match inspector.show(ctx, source, self.timeline.cursor) {
            InspectorAction::None => {}
            InspectorAction::Close => return,
            InspectorAction::Seek(t) => {
                let bounds = self.project.time_bounds();
                self.timeline.jump_to(t, bounds);
            }
            InspectorAction::Plot(series) => {
                let name = series.name.clone();
                self.add_series(source_id, series);
                let id = self.plots.create(source_id, name.clone());
                self.add_pane(Pane::Plot(id));
                self.status = Some(format!("Added {name}"));
            }
        }
        self.canopen = Some(inspector);
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
        // Same for the tank pane: the wall it was drawing is gone, the pane
        // and its scale are not.
        self.tanks.forget_source(id);
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
        ui.horizontal(|ui| {
            ui.heading("rapid-analyzer");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                theme_switch(ui, &mut self.theme);
            });
        });
        ui.horizontal(|ui| {
            if ui.button("+ Import file...").on_hover_text("Or drop files onto the window").clicked() {
                self.pick_file(ui.ctx());
            }
            if self.importing > 0 {
                ui.spinner();
            }
        });
        // Applied after the borrow of `self.project` below is over.
        let mut new_vapor_pane = false;
        let mut new_tank_pane = false;
        let mut open_export = false;
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
        if ui
            .button("＋ Tank level")
            .on_hover_text(
                "The tank wall as a picture: ten temperatures up the cylinder, colour against height and time",
            )
            .clicked()
        {
            new_tank_pane = true;
        }
        let mut save_session = false;
        let mut open_session = false;
        ui.horizontal(|ui| {
            save_session = ui
                .button("💾 Save session…")
                .on_hover_text("Save the open graphs, the markers and the files behind them  (Ctrl+S)")
                .clicked();
            open_session = ui
                .button("📂 Open session…")
                .on_hover_text("Reopen a saved session, importing any of its files that aren't loaded  (Ctrl+O)")
                .clicked();
        });
        if ui
            .button("🖼 Export graph…")
            .on_hover_text("Save the graphs you have open as an SVG or PNG, for a report  (Ctrl+E)")
            .clicked()
        {
            open_export = true;
        }
        #[cfg(feature = "media")]
        if !self.ffmpeg_available {
            let warn = ui.visuals().warn_fg_color;
            ui.colored_label(warn, "⚠ ffmpeg not found -- video/audio import will fail");
        }
        if let Some(status) = self.status.clone() {
            ui.small(status);
        }
        ui.separator();

        if new_vapor_pane {
            self.apply(PendingAction::NewVaporPane);
        }
        if new_tank_pane {
            self.apply(PendingAction::NewTankPane);
        }
        if open_export {
            self.open_export();
        }
        if save_session {
            self.save_session(true);
        }
        if open_session {
            self.pick_session(ui.ctx());
        }
        if self.project.sources.is_empty() {
            ui.weak(IMPORT_HINT);
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
                                if ui
                                    .small_button("CANopen…")
                                    .on_hover_text(
                                        "Inspect the IO boards: nodes and heartbeats, SDO reads, writes and \
                                         aborts, and every object dictionary value at the playhead",
                                    )
                                    .clicked()
                                {
                                    pending.push(PendingAction::OpenCanOpen(source.id));
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
        let mut open_export = false;
        let mut save_session = false;
        let mut open_session = false;
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
                let toggle = matches!(
                    key,
                    egui::Key::Space
                        | egui::Key::B
                        | egui::Key::R
                        | egui::Key::M
                        | egui::Key::D
                        | egui::Key::Escape
                        | egui::Key::E
                        | egui::Key::S
                        | egui::Key::O
                );
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
                    egui::Key::M if modifiers.is_none() => self.timeline.add_marker(),
                    egui::Key::D if modifiers.is_none() => {
                        let active = !self.timeline.measure.active;
                        self.timeline.measure.set_active(active);
                    }
                    egui::Key::Escape => self.timeline.measure.set_active(false),
                    egui::Key::E if modifiers.command => open_export = true,
                    egui::Key::S if modifiers.command => save_session = true,
                    egui::Key::O if modifiers.command => open_session = true,
                    _ => {}
                }
            }
        });
        if open_export {
            self.open_export();
        }
        if save_session {
            // Ctrl+Shift+S always asks, as "save as" does everywhere else.
            let ask = ctx.input(|i| i.modifiers.shift);
            self.save_session(ask);
        }
        if open_session {
            self.pick_session(ctx);
        }
    }
}

/// What can be imported, which is less in a build without video and audio.
const IMPORT_HINT: &str = if cfg!(feature = "media") {
    "No sources yet. Import a .tlog, a sensor SQLite log, or a video/audio file."
} else {
    "No sources yet. Import a .tlog or a sensor SQLite log, or drop one onto the window."
};

/// Light, dark, or the desktop's own setting.
///
/// Icons rather than words: it lives in the sidebar's heading row, which is
/// the one place in the panel that must not be allowed to grow.
fn theme_switch(ui: &mut egui::Ui, theme: &mut egui::ThemePreference) {
    use egui::ThemePreference;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        // Laid out right to left, so these read 💻 ☀ 🌙 on screen.
        ui.selectable_value(theme, ThemePreference::Dark, "🌙")
            .on_hover_text("Dark mode");
        ui.selectable_value(theme, ThemePreference::Light, "☀")
            .on_hover_text("Light mode");
        ui.selectable_value(theme, ThemePreference::System, "💻")
            .on_hover_text("Follow the system theme");
    });
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
        // egui keeps the preference, not us, so it is set every frame. The
        // `Ui` we were handed was built with the *previous* frame's style, so
        // it is restyled here too -- otherwise a theme switch would leave the
        // panels one frame behind the window.
        ctx.set_theme(self.theme);
        *ui.style_mut() = (*ctx.global_style()).clone();
        self.take_dropped_files(&ctx);
        self.poll_imports();
        self.handle_shortcuts(&ctx);
        self.timeline.begin_frame();

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
        self.canopen_inspector(&ctx);
        self.export_window(&ctx);

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
                tanks: &mut self.tanks,
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

    fn app_with_log(name: &str, series: &[&str]) -> (App, SourceId) {
        let mut app = App::with_files(Vec::new());
        let kind = SourceKind::Log(LogSource {
            series: series
                .iter()
                .map(|n| crate::series::TimeSeries::from_points(n.to_string(), vec![[100.0, 1.0], [200.0, 2.0]]))
                .collect(),
            format: LogFormat::Tlog,
            can: Default::default(),
        });
        let id = app.add_source(PathBuf::from("/runs").join(name), name.to_string(), kind);
        (app, id)
    }

    fn wait_for_imports(app: &mut App) {
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while app.importing > 0 || app.pending_session.is_some() {
            assert!(Instant::now() < deadline, "imports never finished");
            app.poll_imports();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Save, then reopen in a fresh app that already has the file loaded:
    /// the graphs come back as panes, with their contents, and so do the
    /// markers and the window.
    #[test]
    fn a_saved_session_reopens_its_graphs_and_markers() {
        let dir = std::env::temp_dir().join(format!("rapid-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("run.session.json");

        let (mut app, id) = app_with_log("a.tlog", &["P.p", "P.t"]);
        app.apply(PendingAction::NewPlot(id, "P.p".into()));
        let plot = app.plots.iter().next().unwrap().id;
        app.apply(PendingAction::AddToPlot(plot, id, "P.t".into(), PlotAxis::Right));
        app.apply(PendingAction::NewPlot(id, "P.t".into()));
        app.project.sources[0].offset_seconds = 1.5;
        app.timeline.cursor = 150.0;
        app.timeline.add_marker();
        app.timeline.markers.iter_mut().next().unwrap().name = "ignition".into();
        app.timeline.set_view(120.0, 180.0);
        app.session_path = Some(file.clone());
        app.save_session(false);
        assert!(app.status.as_deref().unwrap().starts_with("Saved"), "{:?}", app.status);

        let (mut later, _) = app_with_log("a.tlog", &["P.p", "P.t"]);
        later.apply(PendingAction::NewPlot(0, "P.p".into()));
        later.start_import(file.clone());
        wait_for_imports(&mut later);
        std::fs::remove_dir_all(&dir).ok();

        // The graph that was open before the session is replaced, not kept.
        let plots: Vec<_> = later.plots.iter().collect();
        assert_eq!(plots.len(), 2);
        assert_eq!(plots[0].entries.len(), 2);
        assert_eq!(plots[0].entries[1].axis, PlotAxis::Right);
        assert!(plots.iter().all(|p| later.pane_tiles.contains_key(&Pane::Plot(p.id))));
        assert_eq!(later.pane_tiles.len(), 2);
        assert_eq!(later.project.sources[0].offset_seconds, 1.5);
        let markers = later.timeline.markers.as_slice();
        assert_eq!((markers.len(), markers[0].time, markers[0].name.as_str()), (1, 150.0, "ignition"));
        assert_eq!((later.timeline.view_start, later.timeline.view_end), (120.0, 180.0));
        assert!(later.status.as_deref().unwrap().contains("2 graphs, 1 marker"), "{:?}", later.status);
    }

    /// A file the session needs that isn't loaded is imported; one that then
    /// fails to import is reported rather than leaving the session hanging.
    #[test]
    fn a_session_whose_file_fails_to_import_still_opens() {
        let dir = std::env::temp_dir().join(format!("rapid-session-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.tlog");
        std::fs::write(&broken, b"not a log").unwrap();
        let session = Session {
            version: session::VERSION,
            sources: vec![
                session::SavedSource {
                    name: "broken.tlog".into(),
                    path: broken.clone(),
                    offset_seconds: 0.0,
                    enabled: true,
                },
                session::SavedSource {
                    name: "gone.tlog".into(),
                    path: dir.join("gone.tlog"),
                    offset_seconds: 0.0,
                    enabled: true,
                },
            ],
            plots: vec![session::SavedPlot {
                title: String::new(),
                normalize: false,
                zero_aligned: false,
                y_manual: None,
                entries: vec![session::SavedEntry {
                    source: 0,
                    series: "P.p".into(),
                    color: [255, 0, 0, 255],
                    axis: session::SavedAxis::Left,
                    value_offset: 0.0,
                }],
            }],
            markers: vec![session::SavedMarker {
                time: 5.0,
                name: "M1".into(),
                color: [255, 0, 0, 255],
            }],
            markers_placed: 1,
            view: None,
        };
        let file = dir.join("s.json");
        std::fs::write(&file, session.to_json().unwrap()).unwrap();

        let mut app = App::with_files(Vec::new());
        app.start_import(file);
        wait_for_imports(&mut app);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(app.plots.iter().count(), 0);
        assert_eq!(app.timeline.markers.len(), 1);
        let status = app.status.unwrap();
        assert!(status.contains("broken.tlog") && status.contains("gone.tlog"), "{status}");
        assert!(status.contains("1 series could not be placed"), "{status}");
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
