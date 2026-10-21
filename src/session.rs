//! Saving a review and picking it up again: the graphs that were open, the
//! markers on the time axis, and enough about the files behind them to find
//! those files again.
//!
//! A session file is JSON and holds no data, only references to it. A graph
//! names its series by *source index into the file's own list* plus series
//! name, because a [`SourceId`] is minted per run and means nothing in the
//! next one. The sources are recorded with their offsets, since a marker is a
//! master-timeline instant and only lands on the same moment of the data if
//! the data is shifted the way it was when the marker was placed.
//!
//! Reopening matches each recorded source against what is already loaded
//! (same path, else same file name -- a browser only ever has the name) and
//! imports the rest; [`candidate_paths`] also looks next to the session file,
//! so a run folder can be moved or copied with its session in it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use egui::Color32;
use serde::{Deserialize, Serialize};

use crate::markers::Marker;
use crate::model::{Project, Source, SourceId, SourceKind};
use crate::panes::{PlotAxis, PlotEntry, PlotSpec};
use crate::timeline::Timeline;

/// Bumped when a change would make an older build misread a newer file.
/// Added fields with a `#[serde(default)]` don't need it.
pub const VERSION: u32 = 1;

/// What a session file's name ends in; also what the file dialogs filter on.
pub const EXTENSION: &str = "json";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Session {
    pub version: u32,
    pub sources: Vec<SavedSource>,
    #[serde(default)]
    pub plots: Vec<SavedPlot>,
    #[serde(default)]
    pub markers: Vec<SavedMarker>,
    /// How many markers had ever been placed, so a marker added after
    /// reopening gets a colour none of the saved ones has.
    #[serde(default)]
    pub markers_placed: u64,
    #[serde(default)]
    pub view: Option<SavedView>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SavedSource {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub offset_seconds: f64,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SavedPlot {
    /// Empty while the title follows the contents.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub normalize: bool,
    #[serde(default)]
    pub zero_aligned: bool,
    #[serde(default)]
    pub y_manual: Option<(f64, f64)>,
    pub entries: Vec<SavedEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SavedEntry {
    /// Index into [`Session::sources`].
    pub source: usize,
    pub series: String,
    /// RGBA, unmultiplied.
    pub color: [u8; 4],
    #[serde(default)]
    pub axis: SavedAxis,
    #[serde(default)]
    pub value_offset: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SavedAxis {
    #[default]
    Left,
    Right,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SavedMarker {
    /// Master (UTC) seconds.
    pub time: f64,
    pub name: String,
    pub color: [u8; 4],
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct SavedView {
    pub start: f64,
    pub end: f64,
    pub cursor: f64,
}

fn yes() -> bool {
    true
}

impl Session {
    /// The state worth keeping: every loaded source, the graphs `plots`
    /// yields (the caller passes the ones with a pane open), the markers and
    /// the window.
    pub fn capture<'a>(project: &Project, plots: impl Iterator<Item = &'a PlotSpec>, timeline: &Timeline) -> Self {
        let sources = &project.sources;
        let index_of = |id: SourceId| sources.iter().position(|s| s.id == id);
        let plots = plots
            .filter_map(|plot| {
                let entries: Vec<SavedEntry> = plot
                    .entries
                    .iter()
                    .filter_map(|e| {
                        Some(SavedEntry {
                            source: index_of(e.source)?,
                            series: e.series.clone(),
                            color: e.color.to_srgba_unmultiplied(),
                            axis: match e.axis {
                                PlotAxis::Left => SavedAxis::Left,
                                PlotAxis::Right => SavedAxis::Right,
                            },
                            value_offset: e.value_offset,
                        })
                    })
                    .collect();
                (!entries.is_empty()).then(|| SavedPlot {
                    title: plot.custom_title().to_string(),
                    normalize: plot.normalize,
                    zero_aligned: plot.zero_aligned,
                    y_manual: plot.y_manual,
                    entries,
                })
            })
            .collect();
        Self {
            version: VERSION,
            sources: sources
                .iter()
                .map(|s| SavedSource {
                    name: s.name.clone(),
                    path: s.path.clone(),
                    offset_seconds: s.offset_seconds,
                    enabled: s.enabled,
                })
                .collect(),
            plots,
            markers: timeline
                .markers
                .iter()
                .map(|m| SavedMarker {
                    time: m.time,
                    name: m.name.clone(),
                    color: m.color.to_srgba_unmultiplied(),
                })
                .collect(),
            markers_placed: timeline.markers.placed(),
            view: Some(SavedView {
                start: timeline.view_start,
                end: timeline.view_end,
                cursor: timeline.cursor,
            }),
        }
    }

    pub fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(self)?)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let session: Self = serde_json::from_slice(bytes).context("not a rapid-analyzer session file")?;
        if session.version > VERSION {
            bail!(
                "this session was saved by a newer rapid-analyzer (format {}, this build reads up to {VERSION})",
                session.version
            );
        }
        Ok(session)
    }

    /// The markers as the timeline holds them, in time order.
    pub fn restored_markers(&self) -> Vec<Marker> {
        let mut markers: Vec<Marker> = self
            .markers
            .iter()
            .filter(|m| m.time.is_finite())
            .enumerate()
            .map(|(i, m)| Marker {
                id: i as u64,
                time: m.time,
                color: rgba(m.color),
                name: m.name.clone(),
            })
            .collect();
        markers.sort_by(|a, b| a.time.total_cmp(&b.time));
        markers.dedup_by(|a, b| a.time == b.time);
        markers
    }

    /// One saved graph's entries, against the sources as they resolved.
    /// Returns the entries that could be placed and how many could not --
    /// a source that didn't come back, or a series it no longer has (one
    /// made with the CAN signal picker, say).
    pub fn restored_entries(&self, plot: &SavedPlot, project: &Project, resolved: &[Option<SourceId>]) -> (Vec<PlotEntry>, usize) {
        let mut missing = 0;
        let entries = plot
            .entries
            .iter()
            .filter_map(|e| {
                let found = resolved
                    .get(e.source)
                    .copied()
                    .flatten()
                    .filter(|id| has_series(project, *id, &e.series));
                if found.is_none() {
                    missing += 1;
                }
                Some(PlotEntry {
                    source: found?,
                    series: e.series.clone(),
                    color: rgba(e.color),
                    axis: match e.axis {
                        SavedAxis::Left => PlotAxis::Left,
                        SavedAxis::Right => PlotAxis::Right,
                    },
                    value_offset: e.value_offset,
                })
            })
            .collect();
        (entries, missing)
    }
}

fn rgba([r, g, b, a]: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn has_series(project: &Project, id: SourceId, series: &str) -> bool {
    match project.source(id).map(|s| &s.kind) {
        Some(SourceKind::Log(log)) => log.series.iter().any(|s| s.name == series),
        // A video or audio source has no series; nothing names one in a graph.
        _ => false,
    }
}

/// The loaded source a saved one refers to: the same file, or failing that a
/// file of the same name. The name is what a browser has, and what is left
/// to go on after a run folder has been moved.
pub fn find_loaded(saved: &SavedSource, sources: &[Source]) -> Option<SourceId> {
    sources
        .iter()
        .find(|s| s.path == saved.path)
        .or_else(|| sources.iter().find(|s| s.name == saved.name))
        .map(|s| s.id)
}

/// Where to look for a saved source that isn't loaded: where it was, then
/// beside the session file.
pub fn candidate_paths(saved: &SavedSource, session_path: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![saved.path.clone()];
    if let Some(dir) = session_path.and_then(Path::parent) {
        let beside = dir.join(&saved.name);
        if beside != saved.path {
            paths.push(beside);
        }
    }
    paths
}

/// Whether a file is a session rather than data to import.
pub fn is_session_file(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

/// A file name to offer when saving: after the first log, or the first
/// source of any kind.
pub fn default_file_name(project: &Project) -> String {
    let first = project
        .sources
        .iter()
        .find(|s| matches!(s.kind, SourceKind::Log(_)))
        .or(project.sources.first());
    let stem = first
        .and_then(|s| Path::new(&s.name).file_stem())
        .map_or_else(|| "session".to_string(), |s| s.to_string_lossy().to_string());
    format!("{stem}.session.{EXTENSION}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LogFormat, LogSource};
    use crate::panes::Plots;
    use crate::series::TimeSeries;

    fn log(project: &mut Project, name: &str, series: &[&str]) -> SourceId {
        let id = project.alloc_id();
        project.sources.push(Source {
            id,
            name: name.to_string(),
            path: PathBuf::from("/runs").join(name),
            offset_seconds: 0.0,
            color: Color32::WHITE,
            enabled: true,
            kind: SourceKind::Log(LogSource {
                series: series
                    .iter()
                    .map(|n| TimeSeries::from_points(n.to_string(), vec![[0.0, 1.0], [10.0, 2.0]]))
                    .collect(),
                format: LogFormat::Tlog,
                can: Default::default(),
            }),
        });
        id
    }

    fn sample() -> (Project, Plots, Timeline) {
        let mut project = Project::new();
        let a = log(&mut project, "a.tlog", &["P.p", "P.t"]);
        let b = log(&mut project, "b.tlog", &["X.y"]);
        project.sources[1].offset_seconds = -2.5;
        let mut plots = Plots::default();
        let one = plots.create(a, "P.p".into());
        plots.add(one, b, "X.y".into(), PlotAxis::Right);
        let spec = plots.get_mut(one).unwrap();
        spec.entries[1].value_offset = 10.0;
        spec.y_manual = Some((0.0, 5.0));
        spec.zero_aligned = true;
        plots.create(a, "P.t".into());
        let mut timeline = Timeline::new((0.0, 10.0));
        timeline.cursor = 4.0;
        timeline.markers.add(3.0);
        timeline.markers.add(1.0);
        timeline.markers.add(7.0);
        let second = timeline.markers.iter().find(|m| m.time == 3.0).unwrap().id;
        timeline.markers.remove(second);
        (project, plots, timeline)
    }

    #[test]
    fn a_session_survives_a_round_trip_through_json() {
        let (project, plots, timeline) = sample();
        let session = Session::capture(&project, plots.iter(), &timeline);
        let back = Session::from_json(&session.to_json().unwrap()).unwrap();
        assert_eq!(back, session);
        assert_eq!(back.plots.len(), 2);
        assert_eq!(back.plots[0].entries[1].source, 1);
        assert_eq!(back.plots[0].entries[1].axis, SavedAxis::Right);
        assert_eq!(back.sources[1].offset_seconds, -2.5);
        // Two left of three placed: the next colour must not repeat one.
        assert_eq!(back.markers.len(), 2);
        assert_eq!(back.markers_placed, 3);
        assert_eq!(back.view.unwrap().cursor, 4.0);
    }

    /// Ids are per run, so a reopened session is matched up by file -- here
    /// against the same files loaded in a different order.
    #[test]
    fn entries_follow_their_files_not_their_old_ids() {
        let (project, plots, timeline) = sample();
        let session = Session::capture(&project, plots.iter(), &timeline);

        let mut later = Project::new();
        log(&mut later, "unrelated.tlog", &[]);
        let b = log(&mut later, "b.tlog", &["X.y"]);
        let a = log(&mut later, "a.tlog", &["P.p", "P.t"]);
        let resolved: Vec<_> = session.sources.iter().map(|s| find_loaded(s, &later.sources)).collect();
        assert_eq!(resolved, [Some(a), Some(b)]);

        let (entries, missing) = session.restored_entries(&session.plots[0], &later, &resolved);
        assert_eq!(missing, 0);
        assert_eq!((entries[0].source, entries[1].source), (a, b));
        assert_eq!(entries[1].value_offset, 10.0);
        assert_eq!(entries[1].color, plots.iter().next().unwrap().entries[1].color);
    }

    #[test]
    fn a_series_or_source_that_did_not_come_back_is_counted_not_invented() {
        let (project, plots, timeline) = sample();
        let session = Session::capture(&project, plots.iter(), &timeline);
        let mut later = Project::new();
        let a = log(&mut later, "a.tlog", &["P.t"]);
        let resolved = [Some(a), None];
        let (entries, missing) = session.restored_entries(&session.plots[0], &later, &resolved);
        assert!(entries.is_empty());
        assert_eq!(missing, 2);
    }

    #[test]
    fn a_moved_file_is_found_by_name_and_beside_the_session() {
        let saved = SavedSource {
            name: "a.tlog".into(),
            path: "/old/place/a.tlog".into(),
            offset_seconds: 0.0,
            enabled: true,
        };
        let mut project = Project::new();
        let id = log(&mut project, "a.tlog", &[]);
        assert_eq!(find_loaded(&saved, &project.sources), Some(id));
        assert_eq!(
            candidate_paths(&saved, Some(Path::new("/new/place/run.session.json"))),
            [PathBuf::from("/old/place/a.tlog"), PathBuf::from("/new/place/a.tlog")]
        );
    }

    #[test]
    fn markers_come_back_in_time_order_with_their_names_and_colours() {
        let (project, plots, mut timeline) = sample();
        timeline.markers.iter_mut().next().unwrap().name = "ignition".into();
        let session = Session::capture(&project, plots.iter(), &timeline);
        let markers = session.restored_markers();
        assert_eq!(markers.iter().map(|m| m.time).collect::<Vec<_>>(), [1.0, 7.0]);
        assert_eq!(markers[0].name, "ignition");
        assert_eq!(markers[1].color, timeline.markers.as_slice()[1].color);
    }

    #[test]
    fn a_file_from_a_newer_build_is_refused_and_unknown_fields_are_not() {
        let newer = br#"{"version": 99, "sources": []}"#;
        assert!(Session::from_json(newer).is_err());
        let minimal = br#"{"version": 1, "sources": [{"name": "a.tlog", "path": "a.tlog"}], "later": 1}"#;
        let session = Session::from_json(minimal).unwrap();
        assert!(session.sources[0].enabled);
        assert!(session.plots.is_empty());
        assert!(Session::from_json(b"not json").is_err());
    }

    #[test]
    fn session_files_are_told_apart_from_data() {
        assert!(is_session_file("run.session.json"));
        assert!(is_session_file("RUN.JSON"));
        assert!(!is_session_file("run.tlog"));
        assert!(!is_session_file("telemetry_2026-07-27_14-08-36"));
    }
}
