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
use rapid_analyzer::vapor::{VaporMode, Vapors};

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
    draw_pane_with(project, plots, &mut Vapors::default(), timeline, pane);
}

fn draw_pane_with(
    project: &mut Project,
    plots: &mut Plots,
    vapors: &mut Vapors,
    timeline: &mut Timeline,
    pane: Pane,
) {
    let mut video_workers = HashMap::new();
    let mut audio_players = HashMap::new();
    draw(|ui| {
        let mut behavior = TreeBehavior {
            project,
            plots,
            vapors,
            timeline,
            video_workers: &mut video_workers,
            audio_players: &mut audio_players,
            closed: Vec::new(),
        };
        let _ = behavior.pane_ui(ui, TileId::from_u64(1), &mut pane.clone());
    });
}

/// A tank of nitrous whose pressure is walked down through its own vapour
/// pressure -- so the run covers liquid, saturated and vapour in turn -- and
/// which is then warmed past the critical temperature, where the curve stops
/// existing at all.
fn project_with_a_nitrous_tank() -> Project {
    let n = 600;
    let temperature: Vec<[f64; 2]> = (0..n)
        .map(|i| {
            let t = i as f64 * 0.5;
            let c = if t < 250.0 { 20.0 } else { 20.0 + (t - 250.0) * 0.5 };
            [t, c]
        })
        .collect();
    let pressure: Vec<[f64; 2]> = (0..n)
        .map(|i| {
            let t = i as f64 * 0.5;
            let bar = if t < 250.0 { 60.0 - t * 0.08 } else { 70.0 };
            [t, bar]
        })
        .collect();

    let mut project = Project::new();
    let id = project.alloc_id();
    project.sources.push(Source {
        id,
        name: "tank.tlog".to_string(),
        path: "tank.tlog".into(),
        offset_seconds: 0.0,
        color: egui::Color32::WHITE,
        enabled: true,
        kind: SourceKind::Log(LogSource {
            series: vec![
                TimeSeries::from_points("PRESSURE_VESSEL[1].pressure1", pressure).with_unit(Some("bar".into())),
                TimeSeries::from_points("PRESSURE_VESSEL[1].temperature1", temperature)
                    .with_unit(Some("°C".into())),
            ],
            format: LogFormat::Tlog,
            can: CanFrames::default(),
        }),
    });
    project
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

#[test]
fn the_phase_pane_draws_a_run_through_every_zone() {
    let mut project = project_with_a_nitrous_tank();
    let mut vapors = Vapors::default();
    let id = vapors.create(&project);

    // The guess has to find the pair on its own, from the units and the
    // shared message prefix -- it is what the pane opens with.
    let spec = vapors.get(id).expect("the pane exists");
    assert_eq!(
        spec.temperature.as_ref().map(|r| r.series.as_str()),
        Some("PRESSURE_VESSEL[1].temperature1")
    );
    assert_eq!(
        spec.pressure.as_ref().map(|r| r.series.as_str()),
        Some("PRESSURE_VESSEL[1].pressure1")
    );

    let mut timeline = Timeline::new(project.time_bounds().unwrap());
    timeline.cursor = 120.0;
    let mut plots = Plots::default();
    draw_pane_with(&mut project, &mut plots, &mut vapors, &mut timeline, Pane::Vapor(id));

    // ... and the same data on the curve itself.
    vapors.get_mut(id).unwrap().mode = VaporMode::Curve;
    draw_pane_with(&mut project, &mut plots, &mut vapors, &mut timeline, Pane::Vapor(id));
}

#[test]
fn the_phase_pane_draws_the_bare_curve_with_nothing_selected() {
    let mut project = Project::new();
    let mut vapors = Vapors::default();
    let id = vapors.create(&project);
    {
        let spec = vapors.get(id).unwrap();
        assert!(spec.temperature.is_none() && spec.pressure.is_none());
        // With nothing to compare against the curve, the curve itself is what
        // the pane opens on.
        assert_eq!(spec.mode, VaporMode::Curve);
    }
    let mut timeline = Timeline::new((0.0, 1.0));
    let mut plots = Plots::default();
    draw_pane_with(&mut project, &mut plots, &mut vapors, &mut timeline, Pane::Vapor(id));

    // The state plot has nothing to say without series, and must say so
    // rather than dividing by an empty range.
    vapors.get_mut(id).unwrap().mode = VaporMode::State;
    draw_pane_with(&mut project, &mut plots, &mut vapors, &mut timeline, Pane::Vapor(id));
}

#[test]
fn the_phase_pane_survives_a_window_with_no_overlapping_samples() {
    let mut project = project_with_a_nitrous_tank();
    let mut vapors = Vapors::default();
    let id = vapors.create(&project);
    let mut timeline = Timeline::new((0.0, 300.0));
    timeline.set_view(1e6, 1e6 + 10.0);
    let mut plots = Plots::default();
    draw_pane_with(&mut project, &mut plots, &mut vapors, &mut timeline, Pane::Vapor(id));
}

#[test]
fn unloading_a_source_leaves_the_phase_pane_pointing_at_nothing() {
    let project = project_with_a_nitrous_tank();
    let source = project.sources[0].id;
    let mut vapors = Vapors::default();
    let id = vapors.create(&project);
    assert!(vapors.get(id).unwrap().uses(source));
    vapors.forget_source(source);
    let spec = vapors.get(id).unwrap();
    assert!(spec.temperature.is_none() && spec.pressure.is_none());
    assert!(!spec.uses(source));
}
