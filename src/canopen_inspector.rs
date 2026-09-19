//! The CANopen inspector: the IO boards' object dictionaries as the log saw
//! them.
//!
//! Where the sidebar's `CAN_*` series answer "how did this signal move?",
//! this window answers the questions a series can't: which nodes were on the
//! bus and whether they dropped off, what the master read and wrote and which
//! of those a node refused, and what every dictionary object of a node held
//! at the playhead -- configuration included, which never changes and so
//! would make a very dull graph. Everything here is [`CanOpenLog`], built once
//! when the window opens; see [`crate::can::canopen`] for how.

use crate::can::CanFrames;
use crate::can::canopen::{
    self, CanOpenLog, DICTIONARY, NodeInfo, NodeKey, ObjectKey, ObjectTrack, SdoKind, SdoTransfer,
};
use crate::model::{Source, SourceId};
use crate::series::TimeSeries;
use crate::timeline::{format_duration, format_utc};

pub enum InspectorAction {
    None,
    Close,
    /// Move the playhead here (master time).
    Seek(f64),
    /// Add this series to the source and open it in a graph.
    Plot(TimeSeries),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Nodes,
    Dictionary,
    Sdo,
}

/// What the SDO list was filtered with, so it is refiltered only on change.
#[derive(Clone, PartialEq)]
struct SdoFilter {
    node: Option<NodeKey>,
    text: String,
    aborts_only: bool,
}

pub struct CanOpenInspector {
    source: SourceId,
    log: CanOpenLog,
    tab: Tab,
    node: Option<NodeKey>,
    /// Hide dictionary objects the log says nothing about.
    only_seen: bool,
    sdo_filter: SdoFilter,
    /// Indices into `log.transfers` passing `sdo_filter`, and the filter they
    /// were computed for.
    sdo_rows: Option<(SdoFilter, Vec<usize>)>,
    /// Scroll the SDO list to the playhead on the next frame.
    scroll_to_cursor: bool,
}

const ROW_HEIGHT: f32 = 18.0;

impl CanOpenInspector {
    pub fn new(source: SourceId, frames: &CanFrames) -> Self {
        let log = CanOpenLog::build(frames.frames());
        // Open on the busiest SDO node: the one the master talks to is
        // usually the one worth looking at first.
        let node = log
            .nodes
            .iter()
            .max_by_key(|n| (n.sdo_responses + n.sdo_requests, n.heartbeats))
            .map(|n| n.key);
        Self {
            source,
            log,
            tab: Tab::Nodes,
            node,
            only_seen: true,
            sdo_filter: SdoFilter {
                node: None,
                text: String::new(),
                aborts_only: false,
            },
            sdo_rows: None,
            scroll_to_cursor: false,
        }
    }

    pub fn source(&self) -> SourceId {
        self.source
    }

    pub fn open_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }

    /// Lists every object the firmware defines, not only the ones the log
    /// has something about.
    pub fn show_every_object(&mut self, every: bool) {
        self.only_seen = !every;
    }

    pub fn log(&self) -> &CanOpenLog {
        &self.log
    }

    /// `cursor` is the master playhead; `source` converts it into the log's
    /// own clock, which is what every timestamp in here is on.
    pub fn show(&mut self, ctx: &egui::Context, source: &Source, cursor: f64) -> InspectorAction {
        let mut open = true;
        let mut action = InspectorAction::None;
        egui::Window::new(format!("CANopen -- {}", source.name))
            .id(egui::Id::new(("canopen_inspector", self.source)))
            .open(&mut open)
            .default_size([760.0, 520.0])
            .resizable(true)
            .show(ctx, |ui| {
                action = self.contents(ui, source, cursor);
            });
        if !open {
            return InspectorAction::Close;
        }
        action
    }

    pub fn contents(&mut self, ui: &mut egui::Ui, source: &Source, cursor: f64) -> InspectorAction {
        let t = source.to_local_time(cursor);
        let mut action = InspectorAction::None;

        if self.log.nodes.is_empty() {
            ui.weak("No IO board traffic (TPDO, SDO or heartbeat) in this log's CAN frames.");
            return action;
        }

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Nodes, "Nodes");
            ui.selectable_value(&mut self.tab, Tab::Dictionary, "Object dictionary");
            ui.selectable_value(&mut self.tab, Tab::Sdo, format!("SDO log ({})", self.log.transfers.len()));
            ui.separator();
            ui.weak(format!("playhead {}", format_utc(cursor)));
        });
        ui.separator();

        let seek = |t_local: f64| InspectorAction::Seek(source.to_master_time(t_local));
        match self.tab {
            Tab::Nodes => {
                if let Some(t_local) = self.nodes_tab(ui, t) {
                    action = seek(t_local);
                }
            }
            Tab::Dictionary => {
                if let Some(a) = self.dictionary_tab(ui, t) {
                    action = match a {
                        DictionaryAction::Seek(t_local) => seek(t_local),
                        DictionaryAction::Plot(key) => match self.log.series(key) {
                            Some(series) => InspectorAction::Plot(series),
                            None => InspectorAction::None,
                        },
                    };
                }
            }
            Tab::Sdo => {
                if let Some(t_local) = self.sdo_tab(ui, t) {
                    action = seek(t_local);
                }
            }
        }
        action
    }

    // --- nodes ---------------------------------------------------------

    fn nodes_tab(&mut self, ui: &mut egui::Ui, t: f64) -> Option<f64> {
        let mut seek = None;
        ui.weak(
            "Every IO board that sent anything. Link state and raw debug are the node's own \
             view (0x2032 / 0x2031, from its status TPDO) at the playhead.",
        );
        ui.add_space(4.0);
        egui::ScrollArea::both().auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("canopen_nodes").striped(true).num_columns(9).show(ui, |ui| {
                for header in ["node", "NMT", "heartbeat", "longest gap", "link state", "raw debug", "TPDO frames", "SDO", ""] {
                    ui.strong(header);
                }
                ui.end_row();

                let nodes: Vec<&NodeInfo> = self.log.nodes.iter().collect();
                for info in nodes {
                    let key = info.key;
                    ui.label(node_label(key, self.log.multi_bus));
                    ui.label(match info.nmt_state {
                        Some(state) => format!("{state:#04x} {}", canopen::nmt_label(state)),
                        None => "no heartbeat".to_string(),
                    });
                    ui.label(match info.heartbeat_period() {
                        Some(period) => format!("{} × {:.0} ms", info.heartbeats, period * 1000.0),
                        None => format!("{}", info.heartbeats),
                    });
                    match info.longest_heartbeat_gap {
                        Some((gap, at)) => {
                            // Worth a look only when it is well past the
                            // period; a clean node's longest gap is jitter.
                            let dropout = info.heartbeat_period().is_some_and(|p| gap > p * 2.5);
                            let text = format_duration(gap);
                            let response = if dropout {
                                ui.add(egui::Button::new(
                                    egui::RichText::new(format!("⚠ {text}")).color(ui.visuals().warn_fg_color),
                                ))
                                .on_hover_text("The node went quiet here. Click to move the playhead to where it came back.")
                            } else {
                                ui.add(egui::Button::new(text).frame(false))
                                    .on_hover_text("Click to move the playhead here")
                            };
                            if response.clicked() {
                                seek = Some(at);
                            }
                        }
                        None => {
                            ui.weak("--");
                        }
                    }
                    let at = |index: u16| self.log.value_at(object_key(key, index, 0), t);
                    match (at(0x2032), canopen::object(0x2032)) {
                        (Some(v), Some(def)) => {
                            let text = def.format_value(v.raw);
                            if v.raw == 1 {
                                ui.label(text);
                            } else {
                                ui.colored_label(ui.visuals().warn_fg_color, text);
                            }
                        }
                        _ => {
                            ui.weak("--");
                        }
                    }
                    match at(0x2031) {
                        Some(v) if v.raw != 0 => {
                            ui.colored_label(ui.visuals().warn_fg_color, "ON");
                        }
                        Some(_) => {
                            ui.label("off");
                        }
                        None => {
                            ui.weak("--");
                        }
                    }
                    let tpdo: usize = info.tpdo_frames.values().sum();
                    let kinds: Vec<&str> = info
                        .tpdo_frames
                        .keys()
                        .filter_map(|&k| crate::can::iocan::KINDS.get(k).map(|k| k.name()))
                        .collect();
                    ui.label(tpdo.to_string()).on_hover_text(if kinds.is_empty() {
                        "no process data".to_string()
                    } else {
                        kinds.join(", ")
                    });
                    let sdo = format!("{} req / {} resp", info.sdo_requests, info.sdo_responses);
                    if info.aborts > 0 {
                        ui.colored_label(ui.visuals().error_fg_color, format!("{sdo}, {} aborted", info.aborts));
                    } else {
                        ui.label(sdo);
                    }
                    if ui.small_button("dictionary ▸").clicked() {
                        self.node = Some(key);
                        self.tab = Tab::Dictionary;
                    }
                    ui.end_row();
                }
            });
        });
        seek
    }

    // --- dictionary ----------------------------------------------------

    fn dictionary_tab(&mut self, ui: &mut egui::Ui, t: f64) -> Option<DictionaryAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label("node");
            let selected = self.node.map_or("pick…".to_string(), |k| node_label(k, self.log.multi_bus));
            egui::ComboBox::from_id_salt(("canopen_node", self.source))
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for info in &self.log.nodes {
                        ui.selectable_value(&mut self.node, Some(info.key), node_label(info.key, self.log.multi_bus));
                    }
                });
            ui.checkbox(&mut self.only_seen, "only objects in the log")
                .on_hover_text("Otherwise every object the firmware defines is listed, with -- for the unknown ones");
        });
        if !self.log.has_requests() && !self.log.transfers.is_empty() {
            ui.weak(
                "This log holds only the nodes' SDO answers, not the master's requests: a write shows \
                 as acknowledged but not what was written. Values come from SDO reads and TPDOs.",
            );
        }
        let node = self.node?;

        let rows = self.dictionary_rows(node);
        ui.add_space(4.0);
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new(("canopen_dictionary", node.bus, node.node))
                .striped(true)
                .num_columns(8)
                .show(ui, |ui| {
                    for header in ["object", "", "entry", "value at playhead", "since", "from", "traffic", ""] {
                        ui.strong(header);
                    }
                    ui.end_row();

                    let mut previous_index = None;
                    for (index, sub) in rows {
                        let key = object_key(node, index, sub);
                        let def = canopen::object(index);
                        let track = self.log.objects.get(&key);

                        // The object's name only on its first entry, so an
                        // array reads as one block.
                        if previous_index != Some(index) {
                            let name = def.map_or("unknown object", |d| d.name);
                            ui.label(egui::RichText::new(format!("0x{index:04X}")).monospace())
                                .on_hover_text(def.map_or(String::new(), |d| {
                                    format!("{} ({}, {})", d.short, d.ty.name(), d.access.label())
                                }));
                            ui.label(name);
                        } else {
                            ui.label("");
                            ui.label("");
                        }
                        previous_index = Some(index);

                        let entry = def.and_then(|d| d.sub_label(sub));
                        ui.label(match entry {
                            Some(entry) => format!(".{sub} {entry}"),
                            None => format!(".{sub}"),
                        });

                        let value = track.and_then(|tr| tr.value_at(t));
                        match value {
                            Some(v) => {
                                let text = self.log.format_value_at(key, v.raw, t);
                                ui.label(egui::RichText::new(text).strong());
                                if ui
                                    .add(egui::Button::new(format_utc(v.t_utc)).frame(false))
                                    .on_hover_text("When it last changed. Click to move the playhead there.")
                                    .clicked()
                                {
                                    action = Some(DictionaryAction::Seek(v.t_utc));
                                }
                                ui.label(v.origin.label());
                            }
                            None => {
                                let why = match track {
                                    Some(tr) if tr.changes.first().is_some_and(|c| c.t_utc > t) => "not yet",
                                    _ => "--",
                                };
                                ui.weak(why);
                                ui.label("");
                                ui.label("");
                            }
                        }

                        ui.label(traffic(track)).on_hover_text(track.map_or(String::new(), traffic_detail));

                        let plottable = track.is_some_and(|tr| !tr.changes.is_empty());
                        if ui
                            .add_enabled(plottable, egui::Button::new("📈").small())
                            .on_hover_text("Plot this object's history")
                            .clicked()
                        {
                            action = Some(DictionaryAction::Plot(key));
                        }
                        ui.end_row();
                    }
                });
        });
        action
    }

    /// The `(index, sub)` rows for one node: what the log has, and -- unless
    /// only those are wanted -- everything else the dictionary defines.
    fn dictionary_rows(&self, node: NodeKey) -> Vec<(u16, u8)> {
        let mut rows: Vec<(u16, u8)> = self.log.objects_of(node).map(|(k, _)| (k.index, k.sub)).collect();
        if !self.only_seen {
            for def in DICTIONARY {
                for sub in def.subs() {
                    rows.push((def.index, sub));
                }
            }
            rows.sort_unstable();
            rows.dedup();
        }
        rows
    }

    // --- SDO log -------------------------------------------------------

    fn sdo_tab(&mut self, ui: &mut egui::Ui, t: f64) -> Option<f64> {
        let mut seek = None;
        if self.log.transfers.is_empty() {
            ui.weak("No SDO traffic in this log: nothing read or wrote a node's dictionary.");
            return None;
        }
        ui.horizontal(|ui| {
            ui.label("node");
            let selected = self
                .sdo_filter
                .node
                .map_or("all".to_string(), |k| node_label(k, self.log.multi_bus));
            egui::ComboBox::from_id_salt(("canopen_sdo_node", self.source))
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sdo_filter.node, None, "all");
                    for info in self.log.nodes.iter().filter(|n| n.sdo_requests + n.sdo_responses > 0) {
                        ui.selectable_value(
                            &mut self.sdo_filter.node,
                            Some(info.key),
                            node_label(info.key, self.log.multi_bus),
                        );
                    }
                });
            ui.label("filter");
            ui.add(
                egui::TextEdit::singleline(&mut self.sdo_filter.text)
                    .hint_text("0x2010, valve, abort…")
                    .desired_width(160.0),
            );
            ui.checkbox(&mut self.sdo_filter.aborts_only, "aborts only");
            if ui
                .button("⏩ to playhead")
                .on_hover_text("Scroll to the transfer at the playhead")
                .clicked()
            {
                self.scroll_to_cursor = true;
            }
        });

        if self.sdo_rows.as_ref().is_none_or(|(f, _)| *f != self.sdo_filter) {
            let rows = filter_transfers(&self.log.transfers, &self.sdo_filter);
            self.sdo_rows = Some((self.sdo_filter.clone(), rows));
        }
        let rows = &self.sdo_rows.as_ref().map(|(_, r)| r.as_slice()).unwrap_or(&[]);
        ui.weak(format!("{} of {} transfers", rows.len(), self.log.transfers.len()));

        // The row at or just before the playhead, highlighted and scrolled to.
        let at_cursor = rows
            .partition_point(|&i| self.log.transfers[i].t_utc <= t)
            .checked_sub(1);

        ui.add_space(4.0);
        let header = |ui: &mut egui::Ui| {
            ui.horizontal(|ui| {
                for (text, width) in COLUMNS {
                    ui.add_sized([*width, ROW_HEIGHT], egui::Label::new(egui::RichText::new(*text).strong()));
                }
            });
        };
        header(ui);

        let mut area = egui::ScrollArea::both().auto_shrink([false, false]);
        if std::mem::take(&mut self.scroll_to_cursor)
            && let Some(row) = at_cursor
        {
            let spacing = ui.spacing().item_spacing.y;
            area = area.vertical_scroll_offset(((row as f32) - 5.0).max(0.0) * (ROW_HEIGHT + spacing));
        }
        let multi_bus = self.log.multi_bus;
        area.show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
            for row in range {
                let transfer = &self.log.transfers[rows[row]];
                let highlight = Some(row) == at_cursor;
                if let Some(t_local) = sdo_row(ui, transfer, highlight, multi_bus) {
                    seek = Some(t_local);
                }
            }
        });
        seek
    }
}

enum DictionaryAction {
    Seek(f64),
    Plot(ObjectKey),
}

const COLUMNS: &[(&str, f32)] = &[
    ("time (UTC)", 110.0),
    ("node", 60.0),
    ("", 18.0),
    ("access", 64.0),
    ("object", 300.0),
    ("value / reason", 220.0),
    ("latency", 60.0),
];

/// One transfer as a row. Returns the time to seek to if it was clicked.
fn sdo_row(ui: &mut egui::Ui, transfer: &SdoTransfer, highlight: bool, multi_bus: bool) -> Option<f64> {
    let mut seek = None;
    let row = ui.horizontal(|ui| {
        let cells: [String; 7] = [
            format_utc(transfer.t_utc),
            node_label(
                NodeKey {
                    bus: transfer.bus,
                    node: transfer.node,
                },
                multi_bus,
            ),
            if transfer.request { "→" } else { "←" }.to_string(),
            transfer.kind.label().to_string(),
            canopen::describe_object(transfer.index, transfer.sub),
            transfer.detail(),
            transfer
                .latency
                .map_or(String::new(), |l| format!("{:.1} ms", l * 1000.0)),
        ];
        for (i, (text, (_, width))) in cells.into_iter().zip(COLUMNS).enumerate() {
            let mut rich = egui::RichText::new(text);
            if matches!(transfer.kind, SdoKind::Abort(_)) && i >= 3 {
                rich = rich.color(ui.visuals().error_fg_color);
            }
            if i == 0 {
                rich = rich.monospace();
                let response = ui
                    .add_sized([*width, ROW_HEIGHT], egui::Button::new(rich).frame(false).truncate())
                    .on_hover_text("Move the playhead here");
                if response.clicked() {
                    seek = Some(transfer.t_utc);
                }
            } else {
                ui.add_sized([*width, ROW_HEIGHT], egui::Label::new(rich).truncate());
            }
        }
    });
    if highlight {
        let rect = row.response.rect.expand(1.0);
        ui.painter()
            .rect_stroke(rect, 2.0, ui.visuals().selection.stroke, egui::StrokeKind::Outside);
    }
    seek
}

fn filter_transfers(transfers: &[SdoTransfer], filter: &SdoFilter) -> Vec<usize> {
    let needle = filter.text.trim().to_lowercase();
    transfers
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            filter
                .node
                .is_none_or(|k| (k.bus, k.node) == (t.bus, t.node))
        })
        .filter(|(_, t)| !filter.aborts_only || matches!(t.kind, SdoKind::Abort(_)))
        .filter(|(_, t)| {
            needle.is_empty() || {
                let text = format!(
                    "{} {} {}",
                    canopen::describe_object(t.index, t.sub),
                    t.kind.label(),
                    t.detail()
                )
                .to_lowercase();
                text.contains(&needle)
            }
        })
        .map(|(i, _)| i)
        .collect()
}

fn object_key(node: NodeKey, index: u16, sub: u8) -> ObjectKey {
    ObjectKey {
        bus: node.bus,
        node: node.node,
        index,
        sub,
    }
}

fn node_label(key: NodeKey, multi_bus: bool) -> String {
    if multi_bus {
        format!("bus {} node {}", key.bus, key.node)
    } else {
        format!("node {}", key.node)
    }
}

/// `"1566 w, 2 ✖"` -- how the object was accessed, compactly.
fn traffic(track: Option<&ObjectTrack>) -> String {
    let Some(track) = track else {
        return String::new();
    };
    let mut parts = Vec::new();
    if track.reads > 0 {
        parts.push(format!("{} r", track.reads));
    }
    if track.writes > 0 {
        parts.push(format!("{} w", track.writes));
    }
    if track.aborts > 0 {
        parts.push(format!("{} ✖", track.aborts));
    }
    if parts.is_empty() && track.updates > 0 {
        parts.push(format!("{} upd", track.updates));
    }
    parts.join(", ")
}

fn traffic_detail(track: &ObjectTrack) -> String {
    let mut lines = vec![
        format!("{} value updates, {} distinct changes", track.updates, track.changes.len()),
        format!("SDO: {} reads, {} writes acknowledged, {} aborted", track.reads, track.writes, track.aborts),
    ];
    if let Some(code) = track.last_abort {
        lines.push(format!("last abort: 0x{code:08X} {}", canopen::abort_reason(code)));
    }
    lines.join("\n")
}
