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
- **Multi-series graphs**: any number of series share one graph, so a tank's
  pressure and temperature can be read against each other. Tick a series to
  open it in its own graph, or use ➕ next to it to add it to an existing
  one; each graph has a title (auto-generated from its contents, editable)
  and an optional 0..1 normalization for series whose scales differ.
- **Timeline**: play/pause/step, click-to-seek on the scrubber or directly on
  any graph, adjustable playback speed.
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

Requires system packages for audio (`cpal`/`rodio`'s ALSA backend) and
`ffmpeg` for video frame extraction / audio waveform generation:

```sh
sudo apt install pkg-config libasound2-dev ffmpeg
cargo build --release
```

Without `ffmpeg` on `PATH`, everything else still works; video/audio import
will fail with a clear error (a warning is also shown in the sidebar).
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
- Raw `CAN_FRAME` payloads are imported as bytes (`CAN_FRAME.data[0..7]`)
  and not split per CAN id or decoded into ADC channels -- that mapping is
  vehicle-specific and lives outside the dialect.
