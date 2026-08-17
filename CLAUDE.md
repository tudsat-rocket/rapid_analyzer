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

`egui_tiles` drives a rearrangeable tile tree of `Pane`s (`src/panes.rs`: `Plot(PlotId)`,
`Video`, `Audio`). `App` keeps a `pane_tiles: HashMap<Pane, TileId>` alongside the tree — sidebar
checkboxes and tab close buttons add/remove panes through it, so both must stay in sync
(`add_pane`/`remove_pane`). Closures inside `tree.ui` can't reach `App`, so `TreeBehavior` collects
panes to drop into `closed`, which `App` drains afterwards. Log series start hidden (a tlog can
carry hundreds of fields); media panes are shown on import.

A `Plot` pane doesn't name a series — it names a `PlotSpec` in `App::plots` (`Plots`), which holds
any number of `(source, series, colour, axis)` entries plus a title, a normalize flag, and an
optional manual y range. That is what lets one graph carry pressure and temperature together.
Ticking a series in the sidebar opens a new plot; the ➕ menu next to it adds the series to an
existing one.

egui_plot draws one coordinate system, so the **second y axis** is a mapping, not a second plot:
right-axis series are squeezed into the left axis' auto range by `AxisMap` and the extra
`AxisHints` relabels the ticks on the way out. The map is built from both sides' *auto* ranges
and then held fixed for the frame, so a zoom moves both sets of curves together instead of
re-fitting one under the gesture.

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
