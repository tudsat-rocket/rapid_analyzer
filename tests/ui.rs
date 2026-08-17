//! Headless UI tests.
//!
//! `egui::Context::run` needs no window, GPU or event loop: it lays out and
//! paints into a shape list. That is enough to run the real pane code against
//! real data, which is where the interesting mistakes live -- a panic in a
//! layout closure, an id clash between two panes, an axis whose range is
//! computed from an empty set. None of that shows up in a unit test of the
//! functions underneath.

use std::collections::HashMap;

use egui::RawInput;
use egui_tiles::{Behavior as _, TileId};

use rapid_analyzer::can::{CanFrame, CanFrames, FieldKind, SignalSpec};
use rapid_analyzer::can_builder::CanBuilder;
use rapid_analyzer::model::{LogFormat, LogSource, Project, Source, SourceKind};
use rapid_analyzer::panes::{Pane, PlotAxis, Plots, TreeBehavior};
use rapid_analyzer::series::TimeSeries;
use rapid_analyzer::timeline::Timeline;

/// A log source with two series on wildly different scales: a pressure in
/// bar, and a thrust in newtons that would flatten it on a shared axis.
fn project_with_two_scales() -> Project {
    let pressure: Vec<[f64; 2]> = (0..500).map(|i| [i as f64 * 0.1, 40.0 + (i as f64 * 0.01).sin() * 5.0]).collect();
    let thrust: Vec<[f64; 2]> = (0..500).map(|i| [i as f64 * 0.1, (i as f64 * 0.02).cos() * 7500.0]).collect();

    let mut project = Project::new();
    let id = project.alloc_id();
    project.sources.push(Source {
        id,
        name: "run.tlog".to_string(),
        path: "run.tlog".into(),
        offset_seconds: 0.0,
        color: egui::Color32::WHITE,
        enabled: true,
        kind: SourceKind::Log(LogSource {
            series: vec![
                TimeSeries::from_points("PRESSURE_VESSEL[1].pressure1", pressure).with_unit(Some("bar".into())),
                TimeSeries::from_points("THRUST.force", thrust).with_unit(Some("N".into())),
            ],
            format: LogFormat::Tlog,
            can: can_frames(),
        }),
    });
    project
}

fn can_frames() -> CanFrames {
    let mut frames = CanFrames::default();
    for i in 0..200u32 {
        // node 5 HcoState, and a node this protocol knows nothing about.
        frames.push(CanFrame {
            t_utc: i as f64 * 0.1,
            id: 0x245,
            bus: 1,
            len: 8,
            data: [0x00, 0x80, 0x98, 0x08, 0x00, 0x00, 0xD0, 0x07],
        });
        frames.push(CanFrame {
            t_utc: i as f64 * 0.1,
            id: 0x18A,
            bus: 1,
            len: 8,
            data: [i as u8, (i >> 8) as u8, 0, 0, 0, 0, 0, 0],
        });
    }
    frames
}

/// A window's worth of screen, so panes get a realistic rect to lay out in.
fn input() -> RawInput {
    RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0))),
        ..Default::default()
    }
}

/// Lays out and paints one frame of `contents`, on a context of its own.
fn draw(contents: impl FnMut(&mut egui::Ui)) {
    let ctx = egui::Context::default();
    draw_on(&ctx, contents);
}

fn draw_on(ctx: &egui::Context, contents: impl FnMut(&mut egui::Ui)) {
    // The deltas would otherwise be handed to a renderer that isn't there.
    ctx.run_ui(input(), contents).drop_without_applying_deltas();
}

/// Draws `pane` once; the assertion is that it did not panic.
fn draw_pane(project: &mut Project, plots: &mut Plots, timeline: &mut Timeline, pane: Pane) {
    let mut video_workers = HashMap::new();
    let mut audio_players = HashMap::new();
    draw(|ui| {
        let mut behavior = TreeBehavior {
            project,
            plots,
            timeline,
            video_workers: &mut video_workers,
            audio_players: &mut audio_players,
            closed: Vec::new(),
        };
        let _ = behavior.pane_ui(ui, TileId::from_u64(1), &mut pane.clone());
    });
}

#[test]
fn a_graph_with_a_series_on_each_axis_draws() {
    let mut project = project_with_two_scales();
    let source = project.sources[0].id;
    let mut plots = Plots::default();
    let id = plots.create(source, "PRESSURE_VESSEL[1].pressure1".to_string());
    plots.add(id, source, "THRUST.force".to_string(), PlotAxis::Right);
    let mut timeline = Timeline::new(project.time_bounds().unwrap());

    draw_pane(&mut project, &mut plots, &mut timeline, Pane::Plot(id));

    // The gesture-driven range is what a box zoom would leave behind; the
    // pane has to survive being handed one.
    plots.get_mut(id).unwrap().y_manual = Some((39.0, 41.0));
    draw_pane(&mut project, &mut plots, &mut timeline, Pane::Plot(id));
}

#[test]
fn a_graph_draws_with_everything_on_the_right_axis() {
    let mut project = project_with_two_scales();
    let source = project.sources[0].id;
    let mut plots = Plots::default();
    let id = plots.create(source, "THRUST.force".to_string());
    plots.get_mut(id).unwrap().entries[0].axis = PlotAxis::Right;
    let mut timeline = Timeline::new(project.time_bounds().unwrap());
    draw_pane(&mut project, &mut plots, &mut timeline, Pane::Plot(id));
}

#[test]
fn a_graph_whose_window_holds_no_samples_still_draws() {
    let mut project = project_with_two_scales();
    let source = project.sources[0].id;
    let mut plots = Plots::default();
    let id = plots.create(source, "PRESSURE_VESSEL[1].pressure1".to_string());
    plots.add(id, source, "THRUST.force".to_string(), PlotAxis::Right);

    let mut timeline = Timeline::new((0.0, 50.0));
    // Scrolled a long way past the end of the data: every series has bounds
    // of `None`, so both axes are empty.
    timeline.set_view(1e6, 1e6 + 10.0);
    draw_pane(&mut project, &mut plots, &mut timeline, Pane::Plot(id));

    // ... and normalized, where the per-series rescale has nothing to divide by.
    plots.get_mut(id).unwrap().normalize = true;
    draw_pane(&mut project, &mut plots, &mut timeline, Pane::Plot(id));
}

#[test]
fn a_graph_naming_a_series_that_is_gone_draws_a_message_rather_than_panicking() {
    let mut project = project_with_two_scales();
    let source = project.sources[0].id;
    let mut plots = Plots::default();
    let id = plots.create(source, "NOT_IMPORTED.field".to_string());
    let mut timeline = Timeline::new((0.0, 50.0));
    draw_pane(&mut project, &mut plots, &mut timeline, Pane::Plot(id));
}

#[test]
fn the_timeline_bar_draws_in_both_zoom_modes() {
    let project = project_with_two_scales();
    let bounds = project.time_bounds();
    let mut timeline = Timeline::new(bounds.unwrap());
    for box_zoom in [false, true] {
        timeline.box_zoom = box_zoom;
        draw(|ui| {
            rapid_analyzer::timeline::show(ui, &mut timeline, bounds);
        });
    }
    // ... and with nothing loaded, where there is no range to draw.
    draw(|ui| {
        rapid_analyzer::timeline::show(ui, &mut timeline, None);
    });
}

#[test]
fn the_can_signal_picker_draws_and_previews_a_signal() {
    let project = project_with_two_scales();
    let source = project.sources[0].id;
    let SourceKind::Log(log) = &project.sources[0].kind else {
        panic!("log source");
    };
    let mut builder = CanBuilder::new(source, &log.can);
    let ctx = egui::Context::default();
    // Twice on the same context: the first pass computes the preview, the
    // second takes the cached path.
    for _ in 0..2 {
        draw_on(&ctx, |ui| {
            builder.show(ui.ctx(), "run.tlog", &log.can);
        });
    }
}

#[test]
fn a_hand_specified_signal_becomes_a_plottable_series() {
    let project = project_with_two_scales();
    let SourceKind::Log(log) = &project.sources[0].kind else {
        panic!("log source");
    };
    // The unknown-to-the-protocol identifier, read as a little-endian u16
    // counter and scaled to kilo-counts.
    let series = log.can.extract(&SignalSpec {
        id: 0x18A,
        offset: 0,
        kind: FieldKind::U16,
        scale: 0.001,
        unit: "k".to_string(),
        name: "counter".to_string(),
        ..Default::default()
    });
    assert_eq!(series.name, "counter");
    assert_eq!(series.len(), 200);
    assert_eq!(series.unit.as_deref(), Some("k"));
    assert_eq!(series.value_at(0.0, 0.0), Some(0.0));
    assert!((series.value_at(19.9, 0.0).unwrap() - 0.199).abs() < 1e-9);
}
