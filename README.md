# rapid-analyzer

A Rust/egui tool for reviewing multi-source experiment data on one
synchronized timeline: MAVLink `.tlog` telemetry, a custom SQLite sensor log,
video (mp4/...), and audio (m4a/...), all scrubbed, played back, and
zoomed together.

## Features

- **Import**: `.tlog` (MAVLink v1/v2, the `rapid` dialect -- the common
  message set plus our own `PRESSURE_VESSEL`/`VALVE`/`ROCKET_INFO`
  messages, see "Custom MAVLink dialect" below; fields are extracted
  generically, no per-message hardcoding), a
  `sensor_data(timestamp, sensor_name, value)` SQLite log, and video/audio
  files (via `ffmpeg`/`ffprobe`).
- **One series per instance**: messages that carry an `instance` field
  describe a different thing on every send -- each of the six
  `PRESSURE_VESSEL`s and nine `VALVE`s in a log gets its own
  `PRESSURE_VESSEL[1].pressure1` / `VALVE[MAIN].state` series rather than
  all of them being interleaved into one line. Units come from the dialect
  (and centidegrees etc. are scaled to the unit they name), and MAVLink's
  "no reading" sentinels are dropped instead of drawn as spikes.
- **CAN bus**: `CAN_FRAME` traffic is split by identifier and decoded against
  the vehicle's own IO board protocol, so a log yields
  `CAN_HCO[5].out2_pwm_us`, `CAN_SENSOR[6].slot0` (already in bar or °C, per
  the node's declared unit), `CAN_VALVE[5].measured0`, rail voltages, link
  state and per-node heartbeats -- see "CAN decoding" below. Anything else on
  the bus can be plotted by hand: "＋ signal…" next to a log's frame count
  opens a picker for an identifier, a byte offset, a type and a scale.
- **Multi-series graphs**: any number of series share one graph, so a tank's
  pressure and temperature can be read against each other. Tick a series to
  open it in its own graph, or use ➕ next to it to add it to an existing
  one; each graph has a title (auto-generated from its contents, editable)
  and an optional 0..1 normalization for series whose scales differ.
- **Two value axes**: a series whose scale would flatten the others -- a
  thrust curve in newtons next to a pressure in bar -- can be drawn against
  the graph's right-hand axis instead, with its own numbers and unit. Add it
  there with `→R` in the ➕ menu, or flip an existing one with the `L`/`R`
  button in the graph's ⚙ menu.
- **Timeline**: play/pause/step, click-to-seek on the scrubber or directly on
  any graph, adjustable playback speed, and keyboard shortcuts for all of it
  (see below).
- **Zooming**: scroll to zoom the time axis, drag to pan. `▣` (or `B`) turns
  on box zoom -- drag a rectangle in any graph to zoom into it, in time *and*
  value; without it, the same works with the right mouse button. `⟲` (or `R`)
  zooms back out to everything loaded.
- **Unloading**: `✖` next to a source drops it, along with every graph and
  panel that was showing it, and re-fits the timeline to what is left.
- **Per-source offset**: every source has its own UTC start time (from log
  timestamps, or -- for media -- container `creation_time` metadata, else a
  timestamp parsed out of the file name, else the file's mtime); drag its
  `offset` field to correct clock drift between sources.
- **Linked graphs**: panning/zooming any log or waveform plot updates the
  shared time window everywhere -- other graphs, the video frame, and the
  audio playhead all follow.
- **Rearrangeable layout**: pick which series/panels to show from the
  sidebar, where a log's series are grouped by message; drag tabs to
  reorder, drop on an edge to split, close a tab to drop the pane (via
  `egui_tiles`).
- **Performance**: each time series is indexed with a min/max mipmap at
  import time, so plots only ever draw a couple thousand points regardless
  of zoom level or file size. See `examples/bench_import.rs` (`--list`
  prints every imported series with its sample count, span and unit).
  Video is decoded by a long-running `ffmpeg` that streams downscaled
  frames, re-seeking only when the playhead jumps.

## Building

Needs a Rust toolchain (2024 edition, so 1.85 or newer -- get it from
[rustup](https://rustup.rs) if your distribution's is older), the ALSA
development headers for audio (`cpal`/`rodio`), `pkg-config`, and
`ffmpeg`/`ffprobe` at run time for video frames and audio waveforms.

**Debian / Ubuntu**

```sh
sudo apt install build-essential pkg-config libasound2-dev ffmpeg
```

**Arch**

```sh
sudo pacman -S base-devel pkgconf alsa-lib ffmpeg
```

**Fedora**

```sh
sudo dnf install @development-tools pkgconf-pkg-config alsa-lib-devel
# Fedora's own ffmpeg-free is built without the H.264/HEVC decoders, so
# footage from most cameras will not play. Get the full build from RPM Fusion:
sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm
sudo dnf install --allowerasing ffmpeg
```

Then, on any of them:

```sh
cargo build --release
```

> **Codecs.** The video pane decodes whatever the `ffmpeg` on your `PATH` can
> decode, and distributions do ship builds with codecs removed for licensing
> reasons -- Fedora's `ffmpeg-free` has no `h264`, `hevc`, `vc1` or `vvc`
> decoder at all. Such a file imports fine (its duration and timing are read
> from the container) but the pane shows ffmpeg's own message, e.g.
> *"no decoder found for: hevc"*. `ffmpeg -decoders | grep hevc` says whether
> yours has it; the RPM Fusion package above, or the Nix flake below, does.

Without `ffmpeg` on `PATH` at all, everything else still works; video/audio
import fails with a clear error (a warning is also shown in the sidebar).
Without an audio output device, waveforms still display but playback is
silently disabled (a note appears in the audio panel).

### With Nix

A flake is provided; it pins the toolchain and every system library, so
none of the `apt` packages above are needed:

```sh
nix build .              # -> ./result/bin/rapid-analyzer
nix run .                # build and launch
nix run . -- log.tlog    # ...with files
nix develop              # dev shell: cargo, clippy, rust-analyzer, ffmpeg
```

The installed binary is wrapped so `ffmpeg`/`ffprobe` are on its `PATH` and
the libraries wgpu/winit `dlopen` at run time (Vulkan, GL, X11, Wayland,
xkbcommon) are on its `LD_LIBRARY_PATH` — the binary itself links only
`libasound`, so those cannot be found by RPATH alone. `nix develop` exports
the same library path, since `cargo run` produces an unwrapped binary.

The build is hermetic: `build.rs` reads the dialect XML from
`mavlink_dialects/` in-tree and never touches the network, so it works
inside nix's sandbox. Dependencies are pinned by `Cargo.lock` alone
(`cargoLock.lockFile`) — all of them come from crates.io, so there is no
vendor hash to update when they change.

> **Note:** flakes only see files that git tracks. In a fresh clone with
> nothing committed yet, `nix build .` fails with *"does not contain a
> '/flake.nix' file"*. Run `git add -A` first (a commit is not required).
> This is also what keeps the multi-gigabyte `target/` out of the build —
> and the derivation applies its own `lib.fileset` allowlist as a second
> line of defence.

## Running

```sh
cargo run --release
# or, to open files immediately:
cargo run --release -- path/to/log.tlog path/to/telemetry.sqlite path/to/video.mp4
```

Use "+ Import file..." in the sidebar to add more sources afterward.

### Keyboard

| Key | |
| --- | --- |
| `Space` | play / pause |
| `←` `→` | step 1 s (`Shift` 10 s, `Alt` 0.1 s) |
| `Home` `End` | jump to the start / end of the experiment |
| `↑` `↓` | playback speed up / down |
| `+` `-` | zoom the time axis in / out around the playhead |
| `B` | box zoom: drag a rectangle in a graph to zoom into it |
| `R` | zoom back out to everything loaded |

Shortcuts are ignored while a text field (the series filter, a graph title)
has focus.

## Custom MAVLink dialect

`.tlog` files are decoded against our own `rapid` dialect
(`mavlink_dialects/Rapid.xml`, plus the vendored `common.xml` /
`standard.xml` / `minimal.xml` it includes), generated into Rust bindings
at build time by `build.rs` via `mavlink-bindgen` -- see `src/dialect.rs`.
We generate our own dialect instead of using the `mavlink` crate's bundled
`ardupilotmega`/`common` dialects because those don't know about
project-specific messages (`PRESSURE_VESSEL`, `VALVE`, `ROCKET_INFO`).
Decoding against a dialect that's missing a message doesn't corrupt the
import -- mavlink-core resyncs by CRC on the next frame -- but every
field of every message outside the dialect is silently absent from the
plots, which is what made the custom telemetry (tank pressures, valve
state) invisible before this dialect was added.

To add or change project messages, edit `mavlink_dialects/Rapid.xml`
(mirrors the upstream
[rapid-dialect](https://github.com/tudsat-rocket/rapid-dialect) format)
and rebuild; `cargo` reruns `build.rs` automatically when that file
changes.

## CAN decoding

A `.tlog` carries forwarded bus traffic as `CAN_FRAME` messages: an
identifier and eight opaque bytes. Expanding those the way every other
MAVLink message is expanded gives `CAN_FRAME.data[0]` -- one line
interleaving byte 0 of every frame from every node, which is nothing. So
frames are pulled out of the generic path and handled by `src/can/`:

- `src/can/iocan.rs` implements the IO boards' own protocol (identifier
  `0x200 | kind << 4 | node_id` for process data, `0x700 + node_id` for
  heartbeats), mirroring `iocan-proto` from the firmware repository. Every
  recognized frame becomes named series:

  | group | what |
  | --- | --- |
  | `CAN_VALVE[n]` | commanded / target / measured position (‰) and whether the drive is released, per-valve current, status, HCO ownership, relief state |
  | `CAN_HCO[n]` | each high current output's state (`out1`..`out4`, 1 = energized) and, for the PWM-driven ones, `outN_pwm_us` |
  | `CAN_SENSOR[n]` | the calibrated sensor slots, scaled into the unit the node declares for each slot (bar, °C, or raw counts) |
  | `CAN_ADC[n]` | raw amplifier readings per I2C bus and channel |
  | `CAN_RAIL[n]` | logic / HCO1+2 / HCO3+4 rail voltage (mV) and current (mA) |
  | `CAN_STATUS[n]` | master link state, raw debug flag, stalled-valve mask, ms since the last master heartbeat |
  | `CAN_I2C[n]` | amplifier presence bitmaps and the sweep counter |
  | `CAN_NODE[n]` | the node's own heartbeat -- constant in value, so what it shows is exactly when a node went quiet |

  `[n]` is the node id (`[bus2:5]` if a log carries more than one bus).
  Slots and channels a node never read are left out rather than drawn as a
  flat line at their "no reading" sentinel.

- Everything else on the bus -- a bought-in sensor, another team's board --
  goes through the picker behind "＋ signal…": choose an identifier (the list
  says how many frames each carries, and names the ones the protocol above
  recognizes), a byte offset, a type (`u8`/`i8`/`u16`/`i16`/`u32`/`i32`/`f32`
  or a single bit), byte order, and a `raw × scale + offset` conversion. It
  previews the sample count and value range before you commit, and the result
  joins that source's series list like any other. Re-adding under the same
  name replaces it, so a scale factor can be refined against an open graph.

Since this mirrors a protocol defined elsewhere, a change to the firmware's
`iocan-proto` has to be made here too; `src/can/iocan.rs`'s tests pin the
identifier layout and each frame's field offsets against known-good frames
from a real log.

## Data model notes

- Log timestamps are assumed to already be UTC seconds (as in both example
  formats). Video/audio use a 0-based local clock plus a best-guess UTC
  start time; if that guess is wrong (no container metadata), the source
  browser shows a warning -- fix it with the offset field.
- MAVLink field extraction is generic: every message is serialized to JSON
  and every field becomes a `MSG_NAME.field` series -- numbers directly,
  enums and bitmasks by resolving their entry names back to the numbers
  they stand for. Fixed-size byte/char array fields (e.g. text fields) get
  pulled in too since they're numeric arrays under the hood; they're
  harmless, just not meaningful to plot.
- Series names carry whatever it takes to keep them apart:
  `MSG[instance].field` for messages with an `instance` field, and
  `MSG@sys:comp.field` when more than one system on the link sends the same
  message (a log where both the vehicle and the ground station send
  `HEARTBEAT`, say).
- The schema behind all of that -- instance fields, units, enums, `invalid`
  sentinels -- is re-read from the dialect XML by `build.rs` into static
  tables (`src/mavlink_meta.rs`), since `mavlink-bindgen` only emits the
  message structs. Adding a message to the XML is still all it takes.

## Known limitations

- Video/audio decoding shells out to `ffmpeg`/`ffprobe` rather than linking
  a decoder -- there's no mature pure-Rust decoder for arbitrary mp4/H.264.
  Video seeking is frame-accurate at typical scrub rates but not
  guaranteed exact-frame on every codec.
- Audio seek/playback goes through `rodio`'s symphonia backend; AAC seeking
  is best-effort (nearest keyframe).
- `.tlog` parsing stops if the file is truncated mid-frame; a bad/unknown
  message elsewhere in the stream is skipped via mavlink-core's own
  CRC-based resync and doesn't stop the import.
- Enum and bitmask fields are plotted as their raw numeric value; the axis
  shows `2`, not `NITROGEN`. Bitmasks are plotted as the combined value
  rather than one line per bit.
- "No reading" filtering keeps to the unambiguous MAVLink sentinels: an
  explicit `invalid="NaN"`/`invalid="*_MAX"`, or a unit-carrying integer
  field sitting at its type's maximum. `invalid="0"` and `invalid="-1"` are
  left alone, since those are values a sensor can genuinely report.
- A media file with no container `creation_time` and no timestamp in its
  name still falls back to the file's mtime, which is usually wrong; the
  source browser says where the guess came from, and the offset field
  corrects it.
- Video playback is only as capable as the `ffmpeg` on your `PATH`; a build
  without the file's codec shows ffmpeg's error in the pane rather than a
  frame (see "Building").
- CAN decoding covers the IO boards' process data and heartbeats. SDO
  (config) traffic is deliberately left alone -- it is a request/response
  protocol, not samples of anything -- and CAN FD frames aren't decoded.
- A hand-built CAN signal lives for the session; it isn't saved, since
  nothing else in the app is either.
- The two value axes share their grid lines: the right-hand axis is the
  left-hand one relabelled, so its ticks land on the left axis' round
  numbers rather than its own.
