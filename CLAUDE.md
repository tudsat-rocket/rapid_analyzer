# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`rapid-analyzer` is a Rust/egui desktop app for reviewing multi-source experiment data on one
synchronized timeline: MAVLink `.tlog` telemetry, a custom SQLite sensor log, video, and audio —
all scrubbed, played back, and zoomed together. See `README.md` for the user-facing feature list
and known limitations.

## Commands

```sh
cargo build --release
cargo run --release -- log.tlog telemetry.sqlite video.mp4   # files are optional
cargo test                                     # unit tests, plus headless UI tests in tests/ui.rs
cargo test slice_matches_raw_when_small        # single test (unit tests live in src/series.rs)
cargo clippy --all-targets
cargo run --release --example bench_import -- <large.tlog>          # import + LOD query timings
cargo run --release --example bench_import -- <large.tlog> --list   # every series: samples, span, unit
```

System prerequisites: `pkg-config`, ALSA headers (for `rodio`), and `ffmpeg`/`ffprobe`
on `PATH` — per-distribution package names are in `README.md`. With Nix, `nix develop`
provides all of these plus the `LD_LIBRARY_PATH` the unwrapped `cargo run` binary needs for
wgpu/winit's `dlopen`ed libraries — without it a `cargo run` build will fail to open a window
on NixOS.

The video pane can only decode what the installed `ffmpeg` can: distribution builds
(Fedora's `ffmpeg-free`, notably) ship with H.264/HEVC removed, and such a file decodes to
*nothing* while the process still exits cleanly. That is why `FrameStream` captures ffmpeg's
stderr and `VideoWorker` reports it when a fresh stream yields no frames — without it the
symptom is a spinner that never resolves.

`nix build .` / `nix flake check` only see git-tracked files, so run `git add -A` first in a
fresh clone (a commit is not needed).

## Architecture

Everything hangs off two shared pieces of state, both owned by `App` (`src/app.rs`):

- **`Project`** (`src/model.rs`) — the list of imported `Source`s. Each source has a `kind`
  (`Log` / `Video` / `Audio`), a color, and a user-adjustable `offset_seconds`.
- **`Timeline`** (`src/timeline.rs`) — the master clock: the visible window (`view_start`/
  `view_end`), the playhead `cursor`, play state, and the `box_zoom` drag mode. Every pane reads
  and writes this, which is what makes graphs, video, and audio move together. Panning/zooming
  *any* plot writes the new x-bounds back into `Timeline`, and every other pane picks them up on
  the next frame. Keyboard transport lives in `App::handle_shortcuts`, which drives the same
  methods the toolbar buttons do.

### The time model (the thing to get right)

There is one master timeline in **absolute UTC seconds**, and each source converts into it:

- Log sources store raw UTC timestamps already; their base is `0.0`.
- Video/audio run on a 0-based local clock, so their base is `start_utc`, resolved by
  `import/start_time.rs`: container `creation_time`, else a timestamp parsed out of the file
  name (read as UTC), else file mtime. `start_utc_source` records which, so the UI can say how
  much to trust it.

`Source::to_local_time` / `to_master_time` (`src/model.rs`) are the only correct way to cross that
boundary; `offset_seconds` is applied inside them. Anything that touches a plot's x-axis, a video
seek, or an audio position must go through them rather than adding timestamps by hand.

### Import

`import::import_path` (`src/import/mod.rs`) dispatches by extension, falling back to content
sniffing (SQLite magic, or a tlog's 8-byte timestamp followed by a MAVLink `0xFD`/`0xFE` STX).
Imports run on a spawned thread and come back to the UI over an `mpsc` channel (`App::poll_imports`).

- `import/tlog.rs` — decodes against the generated `rapid` dialect and extracts fields
  **generically**: each message is serialized to JSON and every field becomes a series. There is
  deliberately no per-message hardcoding; adding a message to the dialect XML is enough to make
  it plottable. Series are keyed by `(message, system, component, instance)`, which is what keeps
  the six `PRESSURE_VESSEL`s and nine `VALVE`s in a log apart — see `src/mavlink_meta.rs`. The
  resulting names are `MSG[instance].field`, with `@sys:comp` added only when a message arrives
  from more than one sender. Enum fields (serialized as `{"type": "ENTRY"}`) and bitmasks
  (`"A | B"`) are resolved back to numbers; values equal to a field's `invalid` sentinel are
  dropped; `cdegC`-style units are scaled to the unit they name.
  `CAN_FRAME` is the one message pulled out of that generic path — see "CAN" below.
- `import/sqlite_log.rs` — pivots long-form `sensor_data(timestamp, sensor_name, value)` rows into
  one series per `sensor_name`.
- `import/video.rs`, `import/audio.rs` — shell out to `ffprobe` (metadata) and `ffmpeg` (a
  streamed sequence of downscaled RGBA frames, see `FrameStream`; PCM for the waveform
  envelope). No decoder is linked in.

### Nitrous oxide phase

`src/n2o.rs` is data and physics with no UI: the NIST saturation table for N₂O
(reproduced verbatim -- see the module docs for why not a correlation),
`psat_kpa`/`tsat_k` either way across it, and `state()`, which classifies a
(T, P) pair into a `Phase`. `src/vapor.rs` is the pane on top: series picking
(with a guesser that prefers a temperature and a pressure describing the same
vessel), unit handling, and the two views. Things that are easy to get wrong
and are therefore pinned by tests:

- **Units.** Series carry `°C`/`bar`/`kPa`; the curve is K and kPa absolute.
  A *difference* converts differently from a *reading* (`delta_from_kelvin`),
  and a gauge reading needs an atmosphere added — a bar, which at tank
  pressures is the width of the saturated band, so it is a switch in the UI.
- **Pairing.** The two series are logged independently, so the sparser one is
  interpolated onto the denser one's timestamps, and only where the other
  series actually has data — holding its last value would invent a phase.
- **Gaps.** Above the critical temperature there is no saturation state, so
  the trace and its zones break into runs rather than being drawn through.

### Tank level

`src/tank.rs` is a port of `rapid-scope`'s live tank window (`../rapid-scope`,
`src/ui/tank.rs`) with its clock swapped: there the strip's right edge is *now* and
scrolls, here the horizontal axis is the master timeline, so the pane shows the same
window as every graph and the playhead is a line through the picture. Ten temperatures
become a mesh -- height up the screen, temperature as colour, time across -- so
stratification is one picture instead of ten graphs. Things that are easy to get wrong
and are therefore pinned by tests:

- **Which series.** `sensor_groups` finds runs that differ only by a trailing number
  (`CAN_SENSOR[10].slot0..slot9`). The number *is* the height, offset so the lowest
  member is the bottom; a skipped number leaves that height blank rather than packing
  the rest down into it, which would silently move every sensor above it.
- **Units.** Everything below `TankSpec::unit` is °C -- the field, the ramp, the
  readouts, the legend. That setting is what the *series* are in, not a display unit.
  `SensorUnit::CentiCelsius` is the one that matters in practice: `can/iocan.rs` leaves
  a slot as `counts` scale 1.0 until its node announces a unit code, and those counts
  are hundredths of a degree. The example tlog's tank (node 10) is entirely in that
  state, so guessing has to accept a unitless row -- while still refusing anything
  declaring pressure.
- **Gaps.** A column takes the last sample at or before it, and only if it is within
  the hold; holding across a dropout would paint a tank nobody measured. The hold grows
  to one column's width when zoomed out, because the LOD slice is coarser than the log
  there and a fixed hold would hollow the strip out.
- **Geometry.** `sensor_band` and `column_cell` tile the vessel and the window exactly,
  which is what blocks mode promises. `layout` refuses a pane too small to draw in
  rather than letting a width go negative.

Being a painter rather than an `egui_plot`, it gets no gestures for free: `interact`
writes pan/zoom/seek back into `Timeline` itself.

### Exporting a figure

`src/export/` writes the open graphs as an SVG or a PNG for a report. It is
deliberately not a screenshot -- wrong resolution, wrong colours for paper, and
the sidebar comes with it -- so the series are laid out a second time:

- `export/figure.rs` is the drawing, and knows nothing about what it draws
  into: every mark goes through the `Canvas` trait. Coordinates are "figure
  units", CSS pixels at 96 to the inch, which is what an SVG `viewBox` is in;
  the raster backend scales by `dpi / 96`. One layout, one set of ticks, two
  files that agree.
- A `Panel` is a graph *or* a tank (`figure::Content`), and both are laid out
  against the same plot rect, which is what lets a stack of them share one
  time axis. The tank panel is the pane's own picture redrawn at the figure's
  size: `tank::Field` sampled by `TankSpec::sample`, and `tank::sensor_y` /
  `sensor_band` / `column_cell` for the geometry, so the exported vessel is
  divided exactly as the pane's is. Its shading is one vertical gradient per
  column rather than a mesh, since that is what an SVG can carry.
- Cells that tile -- the tank strip -- go through `Canvas::fill_cell` and
  `vertical_gradient`, which are deliberately *not* anti-aliased: two
  neighbours each covering their side of a shared edge let the background show
  through as a hairline, and a few hundred of those is a picture of stripes.
  The SVG backend also has to print the two cells' shared edge as the same
  number (`svg::cell_geometry`), or a renderer snapping each to the pixel grid
  reintroduces the seam.
- `export/svg.rs` writes vector text. It cannot embed a font, so it names a
  font stack and *anchors* every string; `figure::TEXT_SLACK` is the few
  percent of extra room every measured label gets. That slack is applied in
  the layout, not in the backend, or the two backends would choose different
  tick spacings for the same settings and the preview would not be of the file.
- `export/raster.rs` is a small software rasterizer: analytic coverage for
  polylines and rects, and text *blitted* out of egui's own font atlas, laid
  out at the export's resolution so a glyph's atlas entry and its destination
  are the same size. Compositing is premultiplied alpha in gamma space, which
  is both what egui's shader does and what an SVG renderer does by default.
  It also writes the PNG's `pHYs` chunk, without which "300 dpi" means nothing
  to a word processor.
- `export/text.rs` owns the one `epaint::Fonts` both backends measure with.
  The atlas can only be read *after* everything has been laid out, which is
  why the raster backend queues text and blits it at the end.
- `export/mod.rs` turns `Plots` into a `Figure`, and `export/dialog.rs` is the
  window. Both axes' ranges come from `panes::value_ranges` -- the same
  function the pane on screen uses -- so an exported graph is the graph the
  user was looking at, including a range pinned by a box zoom (which the right
  axis follows through `panes::AxisMap`, exactly as it does on screen).

The dialog's preview is the figure rendered by the raster backend at screen
resolution, recomputed only when its `preview_key` changes -- settings, the
selection, the window, or the data behind it.

### Theme

`App::theme` is an `egui::ThemePreference` applied to the context every frame
(egui owns the preference, not us) and the root `Ui`'s style is overwritten
with it, or a switch would leave the panels a frame behind. Anything drawn by
hand therefore has to come from `ui.visuals()`; a hardcoded `Color32::RED`
warning is illegible on a light background. The two exceptions are the
playhead colour and `colors.rs`' series palette, which are deliberately fixed:
a source's colour is picked once at import and an exported figure is usually
the opposite theme of the app, so every palette entry clears 3:1 contrast
against both a white page and the dark theme's near-black background.

### CAN

`src/can/` exists because a CAN frame is a container, not a measurement: the generic tlog path
would fold every node's traffic into one `CAN_FRAME.data[0]` series. `import/tlog.rs` diverts
`CAN_FRAME` into `CanFrames` (kept whole on the `LogSource`, ~24 bytes a frame) and then:

- `can/iocan.rs` decodes the IO boards' protocol into named series (`CAN_HCO[5].out2_pwm_us`,
  `CAN_SENSOR[6].slot0`, ...). It **mirrors `iocan-proto` from the io board firmware repo** —
  a protocol change there has to be repeated here. The kind table's *order* is the wire encoding,
  so an inserted variant silently relabels everything after it; the tests pin it, along with each
  frame's field offsets, against real frames. Note `device-conf/can-io.toml`'s comment table in
  that repo is stale — `iocan-proto/src/ids.rs` is the authority.
- `can/mod.rs`'s `SignalSpec` + `can_builder.rs` are the manual path for every other device on
  the bus: identifier, byte offset, type, byte order, `raw × scale + offset`. The resulting
  `TimeSeries` is appended to the source's `series` (kept sorted, since the sidebar groups by
  contiguous name prefix), so it behaves like an imported one from there on.

### Performance model

`TimeSeries` (`src/series.rs`) builds a min/max mipmap at import time: progressively coarser
levels, each holding two points (min and max, in time order) per bucket of `LOD_FACTOR^n` raw
points. `slice_for_range` picks the coarsest level that still yields `target_points` samples in
the requested window, so a pane draws ~2000 points regardless of file size or zoom. Keep plot
rendering going through `slice_for_range` / `value_bounds_in_range`; never hand raw points to
`egui_plot`. `examples/bench_import.rs` measures both import and per-frame query cost.

Video decoding is off-thread (`src/video_worker.rs`) and *streaming*: one long-running `ffmpeg`
(`video::FrameStream`) emits downscaled RGBA frames at a constant rate, so the frame after the
current one costs a pipe read rather than a process spawn. Restarting it — a seek — costs ~0.3 s,
so the worker decodes forward across small gaps and only re-seeks when the jump is big enough to
be worth it; both costs are measured at run time (`Pacing`) because the trade-off depends
entirely on the file. Scrub requests are coalesced so only the latest position is decoded, and
the resulting texture is updated in place rather than reallocated.

Audio playback (`src/audio_playback.rs`) is a `rodio` player re-seeked whenever it
drifts >0.3 s from the timeline cursor; a missing output device is cached as `None` in
`audio_players` so it isn't retried every frame.

### UI panes

The export window (`export::dialog`) and the CAN picker are `egui::Window`s
rather than panes: both are a detour from reviewing a run, and the graphs
behind them stay usable.

`egui_tiles` drives a rearrangeable tile tree of `Pane`s (`src/panes.rs`: `Plot(PlotId)`,
`Video`, `Audio`, `Vapor(VaporId)`, `Tank(TankId)`). `App` keeps a `pane_tiles: HashMap<Pane, TileId>` alongside the tree — sidebar
checkboxes and tab close buttons add/remove panes through it, so both must stay in sync
(`add_pane`/`remove_pane`). Closures inside `tree.ui` can't reach `App`, so `TreeBehavior` collects
panes to drop into `closed`, which `App` drains afterwards. Log series start hidden (a tlog can
carry hundreds of fields); media panes are shown on import.

A `Plot` pane doesn't name a series — it names a `PlotSpec` in `App::plots` (`Plots`), which holds
any number of `(source, series, colour, axis)` entries plus a title, a normalize flag, and an
optional manual y range. That is what lets one graph carry pressure and temperature together.
Ticking a series in the sidebar opens a new plot; the ➕ menu next to it adds the series to an
existing one.

A `PlotEntry` also carries a `value_offset`: the correction for a sensor that
reads high or low, in the series' own unit. It is applied through
`PlotEntry::correct_points` / `correct_bounds` / `corrected` -- samples *and*
the range they are drawn in, or the line leaves its own axis -- and both
`panes::plot_pane` and `export::build_graph_panel` have to apply it, which is
why those three live on the entry rather than at either call site. A corrected
line is renamed by `label_with_offset` (`pressure1 (+10 bar)`): the same axis
carries corrected and uncorrected lines, and an exported figure is read by
someone who was not there when the number was typed in. That is also why the
readout name comes from `field_of(&entry.series)` and not from the label -- a
label can carry a file name and a correction, both of which have dots in them.

egui_plot draws one coordinate system, so the **second y axis** is a mapping, not a second plot:
right-axis series are squeezed into the left axis' auto range by `AxisMap` and the extra
`AxisHints` relabels the ticks on the way out. The map is built from both sides' *auto* ranges
and then held fixed for the frame, so a zoom moves both sets of curves together instead of
re-fitting one under the gesture. `zero_aligned` runs both ranges through `align_zero` first,
which only ever *expands* them — a range that shrank would clip the data being compared.

The sidebar must never size itself by its text: egui gives a panel the width its contents ask
for and persists it, so one long file name would take half the window for the rest of the
session. Everything in it truncates (`wrap_mode` on the scroll area's `Ui`), and the source name
wraps into whatever the trailing controls leave it. `app.rs`'s test lays out a header row in a
320 px `Ui` and checks it stayed inside.

**Zoom** is deliberately one-dimensional by default: `allow_zoom`/`allow_drag` are x-only, so the
value axis stays fitted to what is visible. egui_plot's boxed zoom is the exception — it is the
one gesture that sets a y range, which the pane stores as `PlotSpec::y_manual` (cleared by `R`,
the ⚙ menu, or any change to the plot's contents). `Plot::show` applies interactions *after* the
build closure, so reading `response.transform.bounds()` back is how both axes' gestures are
picked up despite `set_plot_bounds` being called every frame.

Sidebar iteration borrows `self.project.sources` mutably, so every mutation is queued into a
`PendingAction` vec and applied after the loop. `App::remove_source` is the one that has to touch
everything: panes, plots, the video/audio workers (whose `Drop` is what stops ffmpeg and the
audio sink), and the timeline's bounds.

`tests/ui.rs` draws panes headlessly through `egui::Context::run_ui` — no window, no GPU. That is
where a panic in a layout closure or an axis computed from an empty range shows up.

## MAVLink dialect

`build.rs` runs `mavlink-bindgen` over `mavlink_dialects/` at build time; `src/dialect.rs`
`include!`s the output. It then makes a **second pass** over the same XML (via `quick-xml`) for
the schema the generator drops — `instance="true"`, `units`, `enum`, `invalid` — emitting static
tables that `src/mavlink_meta.rs` includes. The importer needs those to name and scale series
correctly, so a new field's XML attributes take effect without any Rust change.

To add or change a project message, edit `mavlink_dialects/Rapid.xml`
(currently `ROCKET_INFO`, `PRESSURE_VESSEL`, `VALVE` at ids 20000+) — cargo reruns `build.rs`
automatically. The `dialect-rapid` and `serde` Cargo features gate the generated module and must
stay enabled; `unexpected_cfgs = "allow"` in `Cargo.toml` exists because the generated code
checks feature names for integrations this crate doesn't use.

Decoding against a dialect missing a message doesn't corrupt the import (mavlink-core resyncs by
CRC), but every field of that message silently disappears from the plots — which is the failure
mode the custom dialect exists to prevent.

## Dependency versions

This targets egui/eframe 0.36 and egui_plot 0.37, whose APIs differ from older, more widely
documented versions — e.g. `eframe::App` is implemented via `fn ui(&mut self, ui, frame)` (not
`update(ctx, frame)`), panels are `egui::Panel::left(...)`, and `Line::new` takes a name as its
first argument. Match the surrounding code rather than recalling older egui idioms.
