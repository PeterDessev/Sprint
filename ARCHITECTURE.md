# Sprint architecture

Sprint is a 3D-printer host in Rust: a modern replacement for Printrun
(Pronterface / pronsole). It loads and visualizes G-code, streams it to a
printer over serial or TCP, exposes the printer's firmware state and
settings in detail, offers tuning and calibration tools, and has a GUI, a
full-screen terminal UI, a line-mode shell and scriptable CLI commands —
while always allowing raw G-code to be sent.

This document is the **contract between crates**. Public items listed here
must exist with these names and meanings. Crates may add more public API;
they must not silently change what is listed. When a signature has to
change, update this file in the same commit.

## Workspace layout

```
crates/
  sprint-gcode   G-code lexer, file model, machine-state tracking, toolpath
                 analysis, kinematic time estimation, slicer metadata,
                 thumbnails, resume-from-line preamble. Pure, no I/O except
                 file loading.
  sprint-proto   Firmware protocol (pure): line numbers + checksums, reply
                 parsing (ok/resend/temps/position/M115/M503/SD/mesh/...),
                 firmware info, settings model + catalog, G-code reference.
  sprint-sim     Virtual Marlin printer (thermal model, planner timing,
                 checksum validation, fault injection, SD card, mesh, PID
                 autotune, host prompts). Std-only; independent of the other
                 crates on purpose so it acts as a second implementation.
  sprint-host    Host engine: transports (serial, TCP, simulator), handshake,
                 send queue + flow control + resend, print jobs (pause/resume/
                 cancel/start-from-line/SD), printer state, events, console log,
                 config + printer profiles, script templating, port listing.
  sprint-viewer  wgpu toolpath renderer as an egui widget (3D orbit view and
                 2D layer view), offscreen MSAA rendering, picking, legends.
  sprint-gui     eframe desktop app.
  sprint-tui     ratatui full-screen UI and a pronsole-style line shell.
  sprint         The `sprint` binary: clap subcommands that dispatch to the
                 GUI, TUI, shell and headless commands.
samples/         Generated sample G-code files (see sprint-gcode examples).
```

Dependency graph (arrows = "depends on"):

```
sprint ──► sprint-gui ──► sprint-viewer ──► sprint-gcode
   │           └────────► sprint-host ───► sprint-gcode, sprint-proto, sprint-sim
   └─────► sprint-tui ──► sprint-host
```

## Ground rules

* **Never open a real serial port in tests, examples or while developing.**
  A real printer is attached to the development machine. All host testing
  uses the simulator (`ConnectTarget::Simulator`) or TCP loopback to the
  simulator. Enumerating port names is fine; opening them is not. The GUI and
  TUI must never auto-connect unless the user enabled it in their profile
  (default: off).
* Library crates use `thiserror` error types; binaries/UI use `anyhow`.
* No `unwrap()` on data from files, the network or the printer. Malformed
  input degrades gracefully.
* Rust 2024 edition, `cargo clippy --workspace --all-targets` clean,
  `cargo fmt` formatted.
* Units: positions in mm, feedrates **in G-code are mm/min**, everything in
  Rust structs is **mm/s** unless the field name says `_mm_min`.
  Temperatures in °C. Times in seconds (`f32`/`f64`) or `Duration`.
* Git: atomic commits, Conventional Commits (`feat(gcode): ...`,
  `fix(host): ...`, `test(sim): ...`, `docs: ...`, `chore: ...`), each commit
  builds and passes tests.

---

## sprint-gcode

Modules: `parse`, `file`, `state`, `analyze`, `estimate`, `meta`, `resume`,
`toolpath` (already defined — see `crates/sprint-gcode/src/toolpath.rs`).
Everything below is re-exported at the crate root.

### parse

```rust
/// A G/M/T (or other letter) command code such as G1, M104, T0, G29.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Code { pub letter: u8 /* uppercase ASCII */, pub number: u16, pub sub: Option<u8> }
impl Code {
    pub fn g(n: u16) -> Code; pub fn m(n: u16) -> Code;
    pub fn is(&self, letter: char, number: u16) -> bool;
}
impl std::fmt::Display for Code { /* "G1", "M104", "G29.1" */ }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Word { pub letter: u8 /* uppercase */, pub value: f64 }

/// One parsed line. Borrowed from the source text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedLine<'a> {
    pub line_number: Option<u32>,      // N word
    pub code: Option<Code>,            // first command word
    pub words: Vec<Word>,              // parameters after the code
    /// Free-text argument for commands that take one: M23/M28/M30/M32/M33
    /// (filenames), M117/M118 (messages), M876? no. Raw remainder, trimmed.
    pub text: Option<&'a str>,
    pub comment: Option<&'a str>,      // text after ';' or inside '(...)'
    pub checksum: Option<u8>,          // '*nn'
}
impl ParsedLine<'_> {
    pub fn get(&self, letter: char) -> Option<f64>;
    pub fn has(&self, letter: char) -> bool;
    pub fn is_empty(&self) -> bool;    // no code, no words
}

/// Parse one line. Accepts `G1X10Y20`, lowercase, extra spaces, `N12 ... *34`,
/// `;` and `(...)` comments. Never panics. Unknown/extended commands
/// (Klipper `SET_FAN_SPEED SPEED=1`) yield `code: None` and no words.
pub fn parse_line(line: &str) -> ParsedLine<'_>;

/// The text to send to a printer: comments removed (`;` and `(...)`), any
/// existing `N` word and `*checksum` removed, whitespace trimmed and runs of
/// spaces collapsed. Returns `None` when nothing is left. Text arguments of
/// M117/M118/M23/... are preserved verbatim (a `;` inside an M117 message
/// still starts a comment, as in Marlin).
pub fn clean_line(line: &str) -> Option<std::borrow::Cow<'_, str>>;
```

### file

```rust
pub struct GcodeFile { /* whole text + line start offsets; cheap random access */ }
impl GcodeFile {
    pub fn load(path: impl AsRef<std::path::Path>) -> std::io::Result<GcodeFile>; // lossy UTF-8
    pub fn from_text(name: impl Into<String>, text: impl Into<String>) -> GcodeFile;
    pub fn name(&self) -> &str;                 // file name for display
    pub fn path(&self) -> Option<&std::path::Path>;
    pub fn text(&self) -> &str;
    pub fn len(&self) -> usize;                 // number of lines
    pub fn is_empty(&self) -> bool;
    pub fn line(&self, index: usize) -> &str;   // raw line, no line ending; "" if out of range
    pub fn command(&self, index: usize) -> Option<std::borrow::Cow<'_, str>>; // clean_line(line(index))
    pub fn byte_len(&self) -> usize;                  // length of text()
    pub fn byte_offset(&self, index: usize) -> usize; // offset of line start in text()
    /// Size as stored (on disk): report this as the file size.
    pub fn source_len(&self) -> u64;
    /// Offset of line start in the source bytes (progress through the file).
    pub fn source_offset(&self, index: usize) -> u64;
    pub fn lines(&self) -> impl Iterator<Item = &str> + '_;
    /// Number of lines that produce a command (precomputed at load).
    pub fn command_count(&self) -> usize;
}
```
Handles `\n`, `\r\n` and lone `\r` line endings and a trailing line without
newline. Loading 100 MB must be fast (single pass, no per-line allocation).

### state

```rust
/// Interpreter state while walking a file: positions and modes.
#[derive(Clone, Debug, PartialEq)]
pub struct MachineState {
    pub position: [f64; 4],        // logical X Y Z E (mm) after G92 offsets
    pub absolute: bool,            // G90/G91
    pub absolute_e: bool,          // M82/M83 (G91 also makes E relative)
    pub inches: bool,              // G20/G21
    pub feedrate_mm_min: f64,      // last F
    pub tool: u8,
    pub hotend_targets: Vec<f32>,  // per tool, from M104/M109
    pub bed_target: f32,           // M140/M190
    pub chamber_target: f32,
    pub fan: u8,                   // M106/M107 (0..=255)
    pub feedrate_pct: f32,         // M220
    pub flow_pct: f32,             // M221
    pub firmware_retracted: bool,  // G10/G11
}
impl MachineState {
    pub fn new() -> MachineState;     // Default too
    /// Apply one parsed line, returning the resulting move if it was one.
    pub fn apply(&mut self, line: &ParsedLine) -> Option<Move>;
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Move { pub from: [f64; 4], pub to: [f64; 4], pub feedrate_mm_min: f64, pub kind: MoveCommand }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveCommand { Rapid /*G0*/, Linear /*G1*/, ArcCw /*G2*/, ArcCcw /*G3*/, Home /*G28*/ }
/// Machine state after executing lines `0..line` (exclusive).
pub fn state_at_line(file: &GcodeFile, line: usize) -> MachineState;
```
Arcs: `apply` returns the arc's endpoints; `analyze` tessellates (I/J
centre form and R form, P full turns, Z helix, E distributed).
G28 sets the homed axes to 0 (or listed axes only).

### analyze / estimate

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)] #[serde(default)]
pub struct AnalyzeOptions {
    pub filament_diameter: f32,    // mm, default 1.75 (overridden by slicer metadata when present)
    pub filament_density: f32,     // g/cm^3, default 1.24 (PLA)
    pub arc_tolerance: f32,        // max chord error when tessellating arcs, mm, default 0.02
    pub limits: KinematicLimits,   // defaults; M201/M203/M204/M205 in the file override while walking
}
impl Default for AnalyzeOptions;

// Copy + serde so PrinterProfile (sprint-host) can store it.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)] #[serde(default)]
pub struct KinematicLimits {
    pub max_feedrate: [f32; 4],    // mm/s per axis X Y Z E, default [500, 500, 12, 120]
    pub max_accel: [f32; 4],       // mm/s^2 per axis, default [3000, 3000, 100, 10000]
    pub print_accel: f32,          // M204 P/S, default 1500
    pub retract_accel: f32,        // M204 R, default 3000
    pub travel_accel: f32,         // M204 T, default 1500
    pub junction_deviation: Option<f32>, // M205 J, default Some(0.013); None = classic jerk
    pub jerk: [f32; 4],            // M205 X Y Z E, default [8, 8, 0.4, 5]
    pub min_feedrate: f32,         // default 0
}
impl Default for KinematicLimits;

/// Analyze a whole file. `progress` gets values in 0..=1 roughly every 1% and
/// returns false to cancel (then `None` is returned).
pub fn analyze(file: &GcodeFile, options: &AnalyzeOptions,
               progress: &mut dyn FnMut(f32) -> bool) -> Option<Toolpath>;
/// Convenience: analyze with no progress reporting.
pub fn analyze_simple(file: &GcodeFile, options: &AnalyzeOptions) -> Toolpath;
```
The estimator plans moves with trapezoidal velocity profiles and look-ahead
(forward/backward pass, junction deviation or jerk), honours G4 dwells,
counts M109/M190 waits as zero time. Target accuracy: within ~5% of a
real Marlin print for typical slicer output. `Segment::time`/`duration`
and `Layer::time_start`/`duration` come from the same plan.

Layers: a new layer starts when an extrusion happens at a Z different
(by > 1e-4) from the current layer's Z; Z hops (travel-only Z changes) do not
create layers. Slicer markers (`;LAYER_CHANGE`, `;LAYER:n`, `;Z:`) are hints
only. Extrusion width = `extruded * filament_area / (length * height)`,
overridden by `;WIDTH:`; height from layer Z delta, overridden by `;HEIGHT:`.

### meta

```rust
/// Scan comments for slicer metadata and thumbnails (PrusaSlicer, SuperSlicer,
/// OrcaSlicer, Bambu Studio, Cura, Simplify3D, ideaMaker, KISSlicer basics).
pub fn read_metadata(file: &GcodeFile) -> SlicerMetadata;
```

### resume

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)] #[serde(default)]
pub struct ResumeOptions {
    pub home_xy: bool,          // G28 X Y before resuming (default true)
    pub home_z: bool,           // default false: never crash into the part
    pub z_clearance: f32,       // lift above resume Z before travelling, default 2.0 (min 0.5 enforced)
    pub wait_for_temps: bool,   // M190/M109 with the targets in effect at the line, default true
    pub restore_z_position: bool,     // G92 Z<resume z> before lifting (board reset in place), default false
    pub travel_feedrate_mm_min: f32,  // default 3000
    pub z_feedrate_mm_min: f32,       // default 600
}
impl Default for ResumeOptions;
/// G-code lines to run before streaming from `line` to continue an
/// interrupted print: temps, fan, units/modes, homing, safe travel to the
/// position in effect at `line`, `G92 E` restore, feedrate.
pub fn resume_preamble(file: &GcodeFile, line: usize, options: &ResumeOptions) -> Vec<String>;
```

### Implementation notes (additive API and clarified semantics)

* `parse`: `parse_into(line, &mut ParsedLine)` reuses the word buffer
  (allocation-free analysis); `has_command(line)` == `clean_line(line).is_some()`;
  `ParsedLine::{is, clear}`; `Code::{new, t}`; `ExtendedCommand::{parse, is,
  param, param_f64}` for Klipper-style `KEY=value` commands. A word without a
  number (`G28 X`), a quoted RRF string or an RRF `{...}` expression is
  stored with `value: NaN`: `has` sees it, `get` returns `None`. Extended
  commands (no `code`) carry the command text in `ParsedLine::text`.
  `clean_line` additionally uppercases classic commands (`g1 x10` ->
  `G1 X10`; text arguments, quoted strings, `{...}` expressions and extended
  commands stay verbatim) and is idempotent. Besides the listed text
  commands, M16 (expected-printer check) and M810-M819 (macro definitions)
  carry their argument in `text` verbatim, as Marlin reads them. M0/M1
  keep their leading numeric `S<sec>`/`P<ms>` words (upper-cased, in
  `words`) and pass the LCD message after them through verbatim (in
  `text`): `"m0 s30 Swap filament"` -> `"M0 S30 Swap filament"`.
* `file`: `GcodeFile::{from_bytes, line_at_offset}`; `Clone + Default`,
  compact `Debug`; a leading BOM is dropped. Lossy decoding and the dropped
  BOM make `text()` differ in length from the source, so `byte_len`,
  `byte_offset` and `line_at_offset` index the decoded text (for searching
  it) while `source_len` and `source_offset` count source bytes: file sizes
  in reports and file info, `JobInfo::bytes_total`/`bytes_done`. The BOM
  counts as part of line 0 (`source_offset(0) == 0`).
* `content`: any file loads, so `GcodeFile::content_kind()` says what it
  is, sniffed from the first 64 KiB of the source bytes at load:
  `ContentKind::{Gcode, Empty, AsciiStl, BinaryStl, Zip3mf, Obj, Step,
  BinaryGcode, Binary, OtherText}` with `is_gcode`, `is_model`,
  `description` and `problem` (the sentence a front end shows when it
  refuses to stream the file, e.g. "This is a 3D model, not G-code: slice
  it first."). `sniff_content(head, total_len)` and `sniff_file(path)`
  (reads only the head) classify without loading. Heuristics: ZIP and
  `GCDE` magic, the binary STL size equation, NUL bytes or over a quarter
  control/invalid bytes, `ISO-10303-21`, `solid` followed by
  `facet`/`endsolid`, OBJ statements, and finally whether at least half of
  the statements read as G-code (G/M/T codes, parameter words, extended
  commands that are upper-case or lower-case with an underscore and
  `KEY=value` arguments, `@` host commands, RepRapFirmware meta commands);
  comment-only text is G-code. Front ends must only stream `Gcode`.
* `state`: extra public fields `relative_e_mode` (sticky M83; G90 restores
  it, as Klipper/RRF/PrusaSlicer do) and `origin_offset` (G92 shift, machine =
  `position + origin_offset`, cleared by G28 for homed axes); helpers
  `machine_position()`, `hotend_target(tool)`. Default feedrate 1500 mm/min.
  G10 with `P`/`L` is RRF tool temperatures/offsets, not a retraction.
* `estimate`: `Planner` (`new`, `with_window`, `push(delta, feedrate_mm_s,
  tag) -> Option<PlannedBlock>`, `flush(sink)`, `limits_mut`),
  `PlannedBlock`, `trapezoid_time`, `DEFAULT_PLANNER_WINDOW` (16),
  `KinematicLimits::{apply_gcode, apply_extended}` (M201/M203/M204/M205,
  Klipper `SET_VELOCITY_LIMIT`).
* `analyze`: `feature_from_name`. Segments are in machine coordinates (G92
  shifts applied). `FeatureStat`s cover extruding segments only;
  `Stats::{travel_time, other_time}` were added so feature times + travel +
  other (E-only moves, firmware retraction, dwells) = `estimated_time`.
  `filament_density` from slicer metadata overrides the option like the
  diameter does. Layer `lines` ranges are contiguous and cover the file; a
  single command spanning several layers (a long helix) leaves empty ranges
  and starts those layers on its own line. `Toolpath::layer_starts_cleanly(i)`
  is false for such layers (streaming from `lines.start` would re-print the
  command from its bottom, and the resume preamble would descend below the
  part already printed); `Toolpath::next_clean_layer(i)` is the first layer
  at or after `i` that a start-from-layer dialog may offer instead.
  Spiral (vase) layers, whose extrusions climb in Z, start at slicer layer
  markers when the file has any, otherwise after every layer height of
  climb; a flat extrusion inside the Z range a spiral layer has climbed
  through (slicers round Z) continues that layer.
  Travel-only files get one layer so every segment has a layer. Arc
  tessellation is bounded per file (2M segments plus 32 per command line;
  past that an arc gets at most 16 segments), so crafted multi-turn arcs
  cannot exhaust memory. In files whose metadata flavor is RepRapFirmware
  (`gcode_flavor = reprapfirmware`, Cura `;FLAVOR:RepRap`) M203 is read in
  mm/min, as RepRapFirmware defines it; in Klipper-flavored files
  (`gcode_flavor = klipper`) `M204 P.. T..` sets print and travel
  acceleration to the smaller value, as Klipper does.
* Objects (cancel-object / exclusion): `Toolpath::objects: Vec<ObjectInfo>`
  (`name`, `id: Option<u32>`, extrusion `bounds`, `lines: Vec<Range<u32>>`,
  `time`, `extruded`, `segments`; `contains_line`) in first-seen order,
  `Toolpath::object_at_line`, and `Segment::object: u16` (index into
  `objects`, `Segment::NO_OBJECT` = `u16::MAX` outside any object;
  `Segment::object_index()`; `Segment::default()` has no object; the
  segment stays 64 bytes). Markers: `M486 S<id> [A<name>]` (`S-1` ends,
  `M486 A<name>` alone names the current M486 object, `T/P/U/C` change
  nothing), Klipper `EXCLUDE_OBJECT_DEFINE NAME=` (lists the object),
  `EXCLUDE_OBJECT_START NAME=` / `EXCLUDE_OBJECT_END`, PrusaSlicer `;
  printing object <name>` / `; stop printing object`, Cura `;MESH:<name>`
  (`;MESH:NONMESH` ends). A block runs from its start marker to the next
  start or end marker; `lines` holds the blocks that contain moves (start
  marker line included, end marker excluded). Names are the label exactly
  as written, trimmed and unquoted; M486 objects without a name are
  `"Object <id>"` (a name given by any M486 line for that id, before or
  after, wins). The same label written as a comment and as `M486 ... A`
  is one object. Purge lines, skirts and start G-code outside the markers
  belong to no object. The host's exclusion uses the same names.
  `ObjectInfo::bounds` is the object's physical extent
  (`Bounds::include_bead`).
* Part size: `Toolpath::part_bounds` is the physical extent of the printed
  part(s): `Bounds::include_bead` (the path widened by half the extrusion
  width in X/Y and reaching one layer height down) of the extrusions,
  leaving out `Feature::Custom` (start/end G-code, purge lines), `Skirt`
  and `WipeTower`, and, when the file labels objects, extrusions outside
  every object. It falls back to all extrusions when none qualifies and is
  empty when nothing extrudes. Front ends show it as the part size (the
  calibration cube sample reports 20 x 20 x 20 mm); `bounds` stays the path
  extent of every extrusion, purge line included.
* `meta`: `is_layer_marker`, `parse_duration`; `SlicerMetadata` gained
  `filament_density`, `layer_count`, `flavor`.

### Examples / samples

`cargo run -p sprint-gcode --example gen_samples` writes realistic synthetic
files into `samples/` (PrusaSlicer-style calibration cube with thumbnails and
`;TYPE:` annotations, Cura-style twisted vase with `;LAYER:`, an arcs test
with G2/G3). The generated files are committed.
`cargo run --release -p sprint-gcode --example gcode_bench` times loading,
cleaning and analysing a ~2M-line synthetic print (example names are unique
across the workspace: sprint-viewer's benchmark is `viewer_bench`).

---

## sprint-proto

Modules: `framing`, `reply`, `firmware`, `settings`, `mesh`, `reference`, `safety`.
Re-exported at the crate root.

### framing

```rust
pub fn checksum(payload: &str) -> u8;                 // XOR of bytes
/// "N{n} {cmd}*{cs}" exactly as Marlin expects (checksum covers "N{n} {cmd}").
pub fn frame_numbered(line_number: u32, command: &str) -> String;
/// Commands that must bypass the queue and flow control (M112, M108, M410, M876).
pub fn is_emergency_command(command: &str) -> bool;
```

### reply

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    /// "ok", "ok N12", "ok N12 P15 B3" (ADVANCED_OK). Temperatures on the same
    /// line are emitted as a separate `Temperatures` reply.
    Ok { line: Option<u32>, planner_free: Option<u32>, buffer_free: Option<u32> },
    /// "Resend: 12", "rs N12", "rs 12".
    Resend(u32),
    Temperatures(TempReport),
    Position(PositionReport),
    /// "echo:busy: processing", "busy: paused for user".
    Busy(String),
    /// Marlin idle "wait".
    Wait,
    /// Firmware boot "start".
    Start,
    /// "Error:..." / "!! ..." (Klipper). `fatal` for halts/kill/"Printer stopped".
    Error { message: String, fatal: bool },
    /// echo:Unknown command: "X"
    UnknownCommand(String),
    Firmware(FirmwareInfo),
    Capability { name: String, enabled: bool },
    /// One line of M503 output: "echo:  M92 X80.00 Y80.00 Z400.00 E93.00".
    Setting(SettingCommand),
    SdCard(SdReply),
    /// "x_min: open", "z_probe: TRIGGERED".
    Endstop { name: String, triggered: bool },
    /// Complete leveling grid (emitted once the multi-line block ends).
    Mesh(BedMesh),
    /// M303 result "Kp: 22.2 Ki: 1.08 Kd: 114".
    AutotuneResult { kp: f32, ki: f32, kd: f32 },
    /// "PID Autotune failed..." variants.
    AutotuneFailed(String),
    /// M78 lines aggregated per line ("Stats: Prints: 12, Finished: 10, Failed: 2").
    Stats(StatsReport),
    /// "//action:pause", "//action:prompt_begin Filament runout", ...
    HostAction(HostAction),
    /// "echo:Active Extruder: 1"
    ActiveExtruder(u8),
    /// M31 "echo:Print time: 1h 2m 3s" (seconds)
    PrintTime(u64),
    /// Any other "echo:" message (prefix stripped).
    Echo(String),
    /// "// ..." Klipper/Debug info lines (prefix stripped).
    Info(String),
    /// Anything unrecognised (raw line).
    Other(String),
}

/// Stateful line parser (handles multi-line blocks: SD file list, mesh
/// reports, M115 capability runs). Feed every received line, in order.
#[derive(Default)]
pub struct ReplyParser { /* ... */ }
impl ReplyParser {
    pub fn new() -> ReplyParser;
    pub fn feed(&mut self, line: &str) -> Vec<Reply>;
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaterReading { pub current: f32, pub target: Option<f32>, pub power: Option<f32> /* 0..=1 */ }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TempReport {
    pub hotends: Vec<HeaterReading>,     // index = tool; single-extruder "T:" -> hotends[0]
    pub bed: Option<HeaterReading>,
    pub chamber: Option<HeaterReading>,
    pub probe: Option<f32>, pub board: Option<f32>, pub redundant: Option<f32>,
    pub cooler: Option<HeaterReading>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PositionReport { pub x: f32, pub y: f32, pub z: f32, pub e: f32, pub stepper_counts: Option<Vec<i64>> }

#[derive(Clone, Debug, PartialEq)]
pub enum SdReply {
    ListBegin, File(SdFile), ListEnd,
    CardOk, CardFailed(String), NoCard,
    Opened { name: String, size: u64 }, Selected,
    Progress { done: u64, total: u64 }, NotPrinting, DonePrinting,
    WritingTo(String), Saved, Deleted(String), DeleteFailed(String),
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SdFile { pub name: String /* 8.3 */, pub long_name: Option<String>, pub size: Option<u64> }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsReport {
    pub prints: Option<u32>, pub finished: Option<u32>, pub failed: Option<u32>,
    pub total_time: Option<u64>, pub longest_job: Option<u64>, // seconds
    pub filament_used_mm: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HostAction {
    Pause, Paused, Resume, Resumed, Cancel, Disconnect, Start, OutOfFilament(Option<String>),
    Notification(String),
    PromptBegin(String), PromptChoice(String), PromptButton(String), PromptShow, PromptEnd,
    Other(String),
}
```
Tested against real-world reply samples from Marlin 1.1/2.0/2.1, Prusa
firmware, Klipper, RepRapFirmware and Smoothieware.

Parsing conventions (implemented in sprint-proto):
* A line may yield zero replies (rows of a pending mesh block, partial
  autotune lines) or several (`ok T:...` -> `Ok` + `Temperatures`; Klipper
  `x:open y:open z:TRIGGERED` -> three `Endstop`s).
* `HeaterReading::power` is normalised to 0..=1 (Marlin 0..127, or 0..255
  when any value on the line exceeds 127; Smoothieware `@<pwm>` 0..255).
  Gaps in `hotends` (e.g. only `T1:` reported) are filled with readings
  whose `current` is NaN.
* File-list entries are only recognised between `Begin file list` and
  `End file list`; an `ok` closes a listing whose end line was lost.
  An entry whose second word is not a size keeps its spaces (Klipper
  `my part.gcode 1234`, size last; RRF bare names). Prusa's `DIR_ENTER:` /
  `DIR_EXIT` markers and resend requests inside a listing are not files.
  `echo:Now fresh file:` / `Now doing file:` give `SdReply::Opened` with
  size 0; `open failed, File: x.` is a non-fatal `Reply::Error`.
* Mesh blocks end at a blank line, `ok` or any non-row line; interleaved
  temperature/busy lines pass through. `BedMesh::kind` is `"bilinear"`,
  `"mbl"`, `"ubl"`, `"klipper_probed"` or `"klipper_mesh"`. Unlabelled
  `Measured points:` rows are back row first after Prusa's `Z search
  height:` or Klipper's `Mesh X,Y:` header (Klipper prints the interpolated
  mesh without the `// ` prefix); such a Klipper block is `"klipper_mesh"`.
* PID autotune: exactly one `AutotuneResult` per M303 run (from the
  `#define DEFAULT_*K*` triple, the last estimate for Prusa's "from above",
  or Klipper's `PID parameters:` line); per-cycle estimates inside a run
  are `Other`.

Additional public API: `ReplyParser::flush()` (emit a pending mesh at end
of input) and `reset()`; standalone `parse_temperatures`, `parse_position`
and `parse_duration` ("1d 2h 3m 4s" -> seconds); `StatsReport::merge`;
`framing::parse_framed` / `FrameError` (verify `N.. cmd*cs` lines);
`FirmwareFlavor::detect`, `FirmwareInfo::get`, `known_capabilities`;
`BedMesh::from_rows`, `set_bounds`, `get`, `values`, `count`;
`SettingCommand::new`, `has`, `remove`; `PrinterSettings::new`, `get_mut`,
`remove`, `keys` (+ `FromIterator`/`Extend`); `SettingCategory::ALL`/`name`;
`GcodeCategory::name`. Reply, firmware, settings and mesh data types
(not the static catalog/reference structs) derive `serde::{Serialize,
Deserialize}`. The NaN sentinels (setting flags, missing mesh points,
placeholder hotend readings) compare equal to each other in `PartialEq`,
and JSON's `null` for them deserialises back to NaN.

### firmware

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FirmwareFlavor { #[default] Unknown, Marlin, Prusa, Klipper, RepRapFirmware, Smoothieware, Repetier, Sprinter, Teacup, Grbl }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FirmwareInfo {
    pub name: String,                  // "Marlin 2.1.2.1 (Github)"
    pub version: Option<String>,       // "2.1.2.1"
    pub flavor: FirmwareFlavor,
    pub source_url: Option<String>,
    pub protocol_version: Option<String>,
    pub machine_type: Option<String>,
    pub extruder_count: Option<u8>,
    pub uuid: Option<String>,
    pub fields: Vec<(String, String)>, // every KEY:value pair on the M115 line
    pub raw: String,
}
/// Parse an M115 "FIRMWARE_NAME:..." line (values may contain spaces).
pub fn parse_firmware_line(line: &str) -> Option<FirmwareInfo>;
impl FirmwareFlavor {
    pub fn name(&self) -> &'static str;
    pub fn supports_m503(&self) -> bool;  // false for Klipper
}
/// Known capability names with human descriptions (AUTOREPORT_TEMP, EEPROM, ...).
pub fn capability_description(name: &str) -> Option<&'static str>;
```

### settings

```rust
/// One configuration command as reported by M503 (or edited by the user).
#[derive(Clone, Debug, PartialEq)]
pub struct SettingCommand { pub code: String /* "M92" */, pub params: Vec<(char, f64)>, pub raw: String }
impl SettingCommand {
    pub fn parse(text: &str) -> Option<SettingCommand>;
    pub fn get(&self, letter: char) -> Option<f64>;
    pub fn set(&mut self, letter: char, value: f64);
    /// Identity within a settings report: code plus selector params
    /// (E/T/I/P/S where the catalog says they select an instance), e.g.
    /// "M301", "M301 E1", "M906 T0", "M145 S1".
    pub fn key(&self) -> String;
    /// G-code that applies this setting: "M92 X80 Y80 Z400 E93" (trims
    /// trailing zeros, keeps the original parameter order).
    pub fn to_gcode(&self) -> String;
}
/// The settings a firmware reported, keyed by `SettingCommand::key()`, in
/// report order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrinterSettings { /* ... */ }
impl PrinterSettings {
    pub fn insert(&mut self, cmd: SettingCommand); // replaces same key
    pub fn get(&self, key: &str) -> Option<&SettingCommand>;
    pub fn value(&self, key: &str, letter: char) -> Option<f64>;
    pub fn iter(&self) -> impl Iterator<Item = &SettingCommand>;
    pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool; pub fn clear(&mut self);
    // Typed helpers used across the app:
    pub fn steps_per_mm(&self) -> Option<[f32; 4]>;        // M92
    pub fn max_feedrate(&self) -> Option<[f32; 4]>;        // M203 (mm/s)
    pub fn max_accel(&self) -> Option<[f32; 4]>;           // M201
    pub fn hotend_pid(&self, tool: u8) -> Option<(f32, f32, f32)>; // M301
    pub fn bed_pid(&self) -> Option<(f32, f32, f32)>;      // M304
    pub fn probe_offset(&self) -> Option<[f32; 3]>;        // M851
    pub fn linear_advance(&self) -> Option<f32>;           // M900 K
    /// KinematicLimits-compatible values (M201/M203/M204/M205) for the estimator.
    pub fn motion_limits(&self) -> MotionLimits;
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MotionLimits {
    pub max_feedrate: Option<[f32; 4]>, pub max_accel: Option<[f32; 4]>,
    pub print_accel: Option<f32>, pub retract_accel: Option<f32>, pub travel_accel: Option<f32>,
    pub junction_deviation: Option<f32>, pub jerk: Option<[f32; 4]>,
}

/// Static description of a known setting command (used by the tuning UI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettingDef {
    pub code: &'static str, pub name: &'static str, pub category: SettingCategory,
    pub description: &'static str,
    /// Instance-selecting parameters (empty if none). A list, not a single
    /// char, because M906/M913/M569 select by T (extruder) *or* I (stepper
    /// index) and M593 by the bare axis letter X *or* Y. Every selector is
    /// also listed in `params`.
    pub selectors: &'static [char],
    pub params: &'static [ParamDef],
    pub requires_eeprom_save: bool,    // persists only after M500
}
impl SettingDef { pub fn param(&self, letter: char) -> Option<&'static ParamDef>; pub fn is_selector(&self, letter: char) -> bool; }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamDef { pub letter: char, pub label: &'static str, pub unit: &'static str,
                      pub min: f64, pub max: f64, pub decimals: u8,
                      /// Bare letter without a value (`M569 S1 X Y`, `M593 X`, `M149 C`);
                      /// stored as `f64::NAN` in `SettingCommand::params`.
                      pub flag: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SettingCategory { Motion, Thermal, Extrusion, Probing, Leveling, Homing, Drivers, Retraction, Other }
pub fn setting_defs() -> &'static [SettingDef];
pub fn setting_def(code: &str) -> Option<&'static SettingDef>;
```
Catalog covers at least: M92 M201 M203 M204 M205 M206 M301 M304 M309 M851 M900
M200 M145 M420 M413 M906 M913 M914 M569 M593 M207 M208 M209 M217 M218 M149
M665 M666 M250 M255 M302 M592? (Marlin 2.1 set), plus G29 for the mesh
points M503 prints.

Reports that mean different things keep separate keys, so inserting a
whole M503 report loses nothing:
* Prusa MK3 prints M203/M201 under "... - normal" and "... - stealth"
  headings. The reply parser marks settings after a stealth heading with
  `SettingCommand::mark_stealth` (a `; stealth` comment in `raw`;
  `is_stealth`), whose key ends in `" stealth"` (`"M203 stealth"`).
  `max_feedrate`/`max_accel`/`motion_limits` return the normal-mode
  values; `max_feedrate_stealth`/`max_accel_stealth` the stealth ones.
  `to_gcode` is the same for both (Prusa applies M201/M203 to the mode
  the printer is in).
* Marlin 1.1 prints `M200 D1.75` and then, with volumetric extrusion
  off, `M200 D0` (off, diameter kept): that line's key is `"M200 D0"`.
  `PrinterSettings::filament_diameter(tool)` and `volumetric_extrusion()`
  read both the Marlin 1.1 and the Marlin 2 (`M200 S0 D1.75`) forms.
* Mesh points (`G29 S3 I<i> J<j> Z`, Marlin 1.1 `G29 S3 X<i> Y<j> Z`,
  `G29 S4 Z`, bilinear `G29 W I<i> J<j> Z`) are keyed by S/W and the
  point indices (`"G29 S3 I0 J2"`), one setting per point. Only these
  forms parse as settings: any other G29 (`G29`, `G29 P1`, `G29 S1`)
  probes the bed, so `SettingCommand::parse` returns `None` for it and
  neither a reply line nor an imported file turns it into a setting.

### mesh

```rust
/// Bed leveling grid. `points[row][col]`, row 0 = front (min Y), col 0 = left (min X).
/// Missing points are NaN.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BedMesh { pub points: Vec<Vec<f32>>, pub x_positions: Option<Vec<f32>>, pub y_positions: Option<Vec<f32>>, pub kind: String }
impl BedMesh {
    pub fn rows(&self) -> usize; pub fn cols(&self) -> usize;
    pub fn min(&self) -> Option<f32>; pub fn max(&self) -> Option<f32>;
    pub fn range(&self) -> Option<f32>; pub fn mean(&self) -> Option<f32>; pub fn std_dev(&self) -> Option<f32>;
}
```
The reply parser recognises Marlin bilinear ("Bilinear Leveling Grid:"),
mesh bed leveling ("Num X,Y:" / "Measured points:"), UBL ("Bed Topography
Report" — printed back row first; normalise to row 0 = front) and Klipper
`BED_MESH_OUTPUT` ("// probed_matrix") formats.

### reference

```rust
pub struct GcodeDoc {
    pub code: &'static str,         // "G1", "M104"
    pub name: &'static str,         // "Linear move"
    pub usage: &'static str,        // "G1 [X<pos>] [Y<pos>] [Z<pos>] [E<pos>] [F<mm/min>]"
    pub description: &'static str,  // one or two sentences
    pub category: GcodeCategory,
    pub dangerous: bool,            // e.g. M502, M112, M500? -> ask before sending
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GcodeCategory { Motion, Temperature, Extruder, Fan, SdCard, Settings, Calibration, Leveling, Info, Control, Display, Other }
pub fn gcode_docs() -> &'static [GcodeDoc];         // ~150 entries
pub fn gcode_doc(code: &str) -> Option<&'static GcodeDoc>; // case-insensitive, "g1" == "G1" == "G01"
/// Prefix search for autocomplete ("M1" -> M104, M105, M106, ...), ordered by relevance.
pub fn search_gcode(prefix: &str) -> Vec<&'static GcodeDoc>;
```
`search_gcode` returns the exact code first, then codes starting with the
query in numeric order (a zero-padded query such as "G01" is treated as a
complete code); word queries ("temp", "bed level") match names first, then
descriptions. Empty queries return nothing. `gcode_doc` reads only the
leading code, so parameters or a comment glued to it (`"G1X10"`,
`"M502;reset"`) are ignored. Dangerous entries: M30, M42, M81, M112, M502,
M994, M997, M999.

### safety

One command-safety classifier for every front end (GUI console and macro
buttons, TUI, shell), so a typed line gets the same answer everywhere:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum JobContext { #[default] Idle, Printing /* job running, not paused */, Paused }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Decision { Allow, Confirm(SafetyReason), Block(SafetyReason) }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] #[non_exhaustive]
pub enum SafetyReason { Dangerous, Restarts, ForcesPosition, InterleavesWithPrint,
    ChangesModes, ShiftsCoordinates, HomesOrProbes, ToolChange, MotorsOff, HeaterOff,
    WaitsForTemperature, Calibration, ChangesLeveling, QuickStop, StartsAnotherPrint,
    EndsPrint, WritesToSd }
impl SafetyReason { pub fn message(self) -> &'static str; } // + Display
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    pub emergency: bool,              // M112/M108/M410/M876: send at once, outside the queue
    pub command: Option<String>,      // "G28", "G29.1", "T1", "BED_MESH_CALIBRATE"; None = nothing to send
    pub doc: Option<&'static GcodeDoc>,
}
impl Verdict { pub fn is_allowed(&self) -> bool; pub fn needs_confirmation(&self) -> bool;
               pub fn is_blocked(&self) -> bool; pub fn reason(&self) -> Option<SafetyReason>; }
pub fn classify(line: &str, ctx: JobContext) -> Verdict;
pub const MIN_EXTRUSION_TEMP: f64 = 170.0; pub const HEATER_OFF_BELOW: f64 = 30.0;
```
The line is read as the firmware reads it after the host cleaned it: `N<n>`
line numbers, `*checksums`, `;` and leading `(...)` comments are ignored,
codes are case-insensitive, parameters may be glued (`G1X10`), blanks
between letter and number are skipped (`M 112`), and Klipper extended
commands are known by name. Rules:
* Dangerous reference entries (`GcodeDoc::dangerous`) and Klipper
  `FIRMWARE_RESTART`/`RESTART`/`SAVE_CONFIG` (`Restarts`) and
  `SET_KINEMATIC_POSITION`/`FORCE_MOVE` (`ForcesPosition`) always Confirm,
  except M112: Allow with `emergency` (never asked).
* Printing: moves (G0-G3, G5, G6, G10/G11 retraction, G12, G27, G42, G61,
  M125, M701/M702 filament load/unload, parameter-only lines, `STEPPER_BUZZ`),
  homing and probing (G28, G26, G29-G30, G32-G35, G38.x, G76, G80, M45, M48,
  M401 probe deploy, `BED_MESH_CALIBRATE`, every `PROBE*`, `Z_TILT_ADJUST`,
  `QUAD_GANTRY_LEVEL`, ...), mode changes (G90/G91/G20/G21/M82/M83, G17-G19
  arc plane), coordinate shifts (G92, G53-G59, M206, M428, G10 L,
  `SET_GCODE_OFFSET X/Y`), tool changes (T<n>, T-1, Prusa MMU `T?`/`Tx`/`Tc`,
  `ACTIVATE_EXTRUDER`), motors off (M18/M84 except `S<timeout>` alone,
  `SET_STEPPER_ENABLE ENABLE=0`), heaters off (hotend target below
  `MIN_EXTRUSION_TEMP`, bed or chamber below `HEATER_OFF_BELOW`,
  `TURN_OFF_HEATERS`, `SET_HEATER_TEMPERATURE` without `TARGET`), temperature
  waits (M109/M190/M191/M116/M192, `TEMPERATURE_WAIT`), calibration (M303,
  M306 T, M915 Z stall alignment, M916-M918, M863, `PID_CALIBRATE`,
  `SHAPER_CALIBRATE`, ...), leveling changes (M420 S/Z, M561,
  `BED_MESH_CLEAR`, `BED_MESH_PROFILE LOAD=`), M410, start/end/cancel macros
  (`PRINT_START`, `START_PRINT`, `PRINT_END`, `END_PRINT`, `CANCEL_PRINT`) and
  M524 (abort SD print) Confirm; M28 and starting an SD print (M24, M32,
  `SDCARD_PRINT_FILE`) Block. Tuning (temperatures, fans, M220/M221,
  babysteps, M600, object exclusion) is allowed.
* Paused: moves, extrusion, mode changes, `G92 E`, heaters and waits, M410
  Allow (the host restores position and modes on resume); homing Z (G28 naming
  Z, or neither X nor Y: firmware homes every axis for `G28`, Prusa's `G28 W`,
  `G28 O`, ...), probing, the arc plane (not restored), other coordinate
  shifts, tool changes, motors off, calibration, leveling changes, SD prints
  and start/end macros Confirm; M28 Block.
* Idle: everything else is allowed. Unknown commands and macros are allowed.

---

## sprint-sim

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SimConfig {
    pub machine_name: String,        // "Sprint Virtual Printer"
    pub extruders: u8,               // 1
    pub heated_bed: bool,            // true
    pub heated_chamber: bool,        // false
    pub build_volume: [f32; 3],      // [220, 220, 250]
    pub time_scale: f32,             // 1.0 = real time; 10.0 = 10x faster (heating, moves, dwell, autotune)
    pub instant_moves: bool,         // moves take zero time (fast tests); default false
    pub boot_delay_ms: u64,          // delay before "start" + banner, default 300
    pub checksum_error_rate: f32,    // probability a numbered line is "corrupted" -> Error + Resend
    pub drop_ok_rate: f32,           // probability an "ok" is lost (tests timeout recovery)
    pub planner_buffer: usize,       // 16 moves
    pub rx_buffer: usize,            // 4 commands (BUFSIZE)
    pub auto_report_temps: bool,     // advertise + honour M155, default true
    pub auto_report_position: bool,  // M154, default true
    pub advanced_ok: bool,           // "ok N.. P.. B..", default false
    pub mesh_size: (usize, usize),   // G29 grid (cols, rows), default (5, 5)
    pub sd_files: Vec<(String, u64)>,// initial SD contents (name, size); default a few demo files
    pub ambient: f32,                // 22.0 °C
    pub seed: u64,                   // RNG seed for fault injection and noise
}
impl Default for SimConfig;

/// Spawn a virtual printer on a background thread.
pub fn spawn(config: SimConfig) -> SimLink;
pub struct SimLink { pub reader: SimReader, pub writer: SimWriter, pub control: SimControl }

/// Bytes the printer sends. `Read` blocks up to the read timeout (default
/// 100 ms) and then returns `Err(io::ErrorKind::TimedOut)`; returns `Ok(0)`
/// once the printer is shut down.
pub struct SimReader; impl std::io::Read for SimReader {}
impl SimReader { pub fn set_timeout(&mut self, timeout: std::time::Duration); }
/// Bytes the host sends.
pub struct SimWriter; impl std::io::Write for SimWriter {}

/// Test/demo control handle (Clone + Send + Sync).
#[derive(Clone)]
pub struct SimControl;
impl SimControl {
    /// Every command accepted for execution, in order, cleaned (no N/checksum).
    pub fn received_commands(&self) -> Vec<String>;
    pub fn clear_received(&self);
    pub fn snapshot(&self) -> SimSnapshot;
    pub fn set_checksum_error_rate(&self, rate: f32);
    pub fn set_drop_ok_rate(&self, rate: f32);
    pub fn set_time_scale(&self, scale: f32);
    /// Emit a raw line from the printer (e.g. "//action:pause").
    pub fn inject_line(&self, line: &str);
    /// Simulate filament runout (M600-style: host prompt + busy: paused for user).
    pub fn trigger_filament_runout(&self);
    /// Run G-code as if chosen on the printer's own menu (Marlin's queue.inject; e.g. "M23 x.gco\nM24"
    /// starts an SD print the host did not start): messages reach the host, no ok, not in received_commands.
    pub fn run_command(&self, gcode: &str);
    /// Press the printer's button (LCD knob): ends a wait for the user (M0/M1, M600) like M108/M876.
    pub fn press_button(&self);
    /// Simulate the board resetting (prints "start" again, resets line numbers).
    pub fn reset(&self);
    pub fn shutdown(&self);
    pub fn is_running(&self) -> bool;
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SimSnapshot {
    pub position: [f32; 4], pub hotend_temps: Vec<f32>, pub hotend_targets: Vec<f32>,
    pub bed_temp: f32, pub bed_target: f32, pub fan: u8, pub feedrate_pct: f32, pub flow_pct: f32,
    pub last_line_number: u32, pub planner_queue: usize, pub halted: bool,
    pub sd_printing: bool, pub commands_received: usize, pub resends_requested: usize,
}

/// Serve the virtual printer over TCP (one client at a time) so other hosts,
/// including Printrun or OctoPrint, can connect to it. Blocks.
pub fn serve_tcp(addr: &str, config: SimConfig) -> std::io::Result<()>;
/// The same on a background thread (dropping it stops it).
pub struct SimServer;
impl SimServer {
    pub fn bind(addr: impl ToSocketAddrs, config: SimConfig) -> std::io::Result<SimServer>;
    pub fn local_addr(&self) -> SocketAddr;
    pub fn is_finished(&self) -> bool;               // the accept loop ended (stopped, or the listener failed)
    pub fn wait(&self) -> std::io::Result<()>;      // block until it ends: Ok after stop(), else the listener's error (first caller only)
    pub fn stop(&self);                              // ask it to stop without waiting
    pub fn shutdown(self) -> std::io::Result<()>;   // stop + wait
}
```
Behaviour: Marlin 2.1-like replies byte-for-byte where practical (boot banner,
`ok`, `Error:checksum mismatch, Last Line: N` + `Resend: N+1`, `Error:Line
Number is not Last Line Number+1, Last Line: N`, `echo:busy: processing`,
`echo:Unknown command: "X"`, M105 `T:20.0 /0.0 B:20.0 /0.0 @:0 B@:0`, M114,
M115 with `Cap:` lines, M503 report with comment headers, M119, M20/M21/M23/
M24/M25/M26/M27/M28/M29/M30/M31/M32, M78, M106/M107, M220/M221, M290, M92/
M201/M203/M204/M205/M301/M304/M851/M900/M500/M501/M502 persisting into the
simulated EEPROM, G29 / M420 V (bilinear grid with a plausible warped bed),
M303 autotune with realistic oscillation output, M155/M154 auto-reports,
M112 halt, M108, M400, G4, M600 with `//action:` prompts, M876).
Thermal model: first-order heaters with PID control, so heating curves,
overshoot and `@:` power look real. Planner model: moves take time based on
feedrate/acceleration (scaled), `ok` is delayed while the planner buffer is
full, giving realistic streaming timing.

---

## sprint-host

```rust
pub use sprint_proto; pub use sprint_gcode; pub use sprint_sim; // re-exports for UIs
pub mod config; pub mod transport; pub mod state; pub mod event; pub mod job; pub mod console; pub mod script; pub mod fs;
pub use host::Printer;

// ---- fs ----
/// Write a file atomically (temporary file next to it, flushed, renamed over it; missing parent
/// directories created; on failure the old file stays and the temporary file is removed). For exports.
pub fn write_atomic(path: impl AsRef<std::path::Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()>;

// ---- transport ----
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ConnectTarget {
    Serial { port: String, baud: u32 },       // baud 0 = auto-detect
    Tcp { host: String, port: u16 },
    Simulator(sprint_sim::SimConfig),
}
impl ConnectTarget {
    /// "COM3", "COM3@250000", "/dev/ttyUSB0@115200", "tcp://host:port", "sim", "sim://".
    pub fn parse(s: &str) -> Result<ConnectTarget, HostError>;
    pub fn label(&self) -> String;            // for display
}
pub const BAUD_RATES: &[u32] = &[250000, 115200, 230400, 500000, 1000000, 57600, 38400, 19200, 9600];
#[derive(Clone, Debug, PartialEq)]
pub struct PortInfo {
    pub name: String, pub description: String,
    pub vid: Option<u16>, pub pid: Option<u16>,
    pub manufacturer: Option<String>, pub serial_number: Option<String>,
    pub likely_printer: bool, // known USB-serial/printer VID:PIDs (CH340, FTDI, CP210x, Arduino, Prusa, STM32 VCP, ...)
}
/// Enumerate serial ports (does not open them).
pub fn list_ports() -> Vec<PortInfo>;

// ---- connect options ----
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConnectOptions {
    pub checksum: ChecksumMode,              // Always (default) | PrintOnly | Never
    pub ok_timeout_secs: f32,                // no reply for this long (and not busy) -> recover; default 10
    pub handshake_timeout_secs: f32,         // default 15 (boards reset on open)
    pub temp_poll_secs: f32,                 // M105 interval when no auto-report, default 2
    pub query_on_connect: bool,              // M115, M503, M114 after handshake, default true
    pub reset_on_connect: bool,              // pulse DTR (serial), default false
    pub max_in_flight: usize,                // commands awaiting ok; 1 = strict ping-pong (default)
    pub autoreport_interval_secs: u32,       // M155 S<n> when supported, default 2
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChecksumMode { #[default] Always, PrintOnly, Never }

// ---- the handle ----
#[derive(Clone)] pub struct Printer { /* Arc<inner> */ }
impl Printer {
    pub fn new() -> Printer;
    /// Open the link and start the engine. Returns once the link is open
    /// (or failed); the handshake continues in the background and is reported
    /// through state/events. Err if already connected or the port can't open.
    pub fn connect(&self, target: ConnectTarget, options: ConnectOptions) -> Result<(), HostError>;
    /// (A disconnect() meanwhile cancels it: Err(HostError::Cancelled), status Disconnected, no failure
    /// event; auto-baud stops between and during its attempts.)
    pub fn disconnect(&self);
    /// Cancel a host print or SD upload first (cancel script / partial file deleted), wait up to `timeout`
    /// for the printer to acknowledge it (else heaters + part fan off / M29 written as the link closes),
    /// then disconnect. SD prints are not cancelled: they go on on the printer (said in the summary).
    pub fn disconnect_safely(&self, timeout: std::time::Duration) -> DisconnectSummary;
    pub fn is_connected(&self) -> bool;                 // handshake complete
    /// Queue G-code typed by the user or produced by UI controls. Multiple
    /// lines allowed; comments/blank lines dropped. Sent before pending
    /// print-stream lines (priority queue). Emergency commands (M112, M108,
    /// M410, M876) bypass the queue automatically.
    pub fn send(&self, gcode: &str);
    pub fn send_immediate(&self, gcode: &str);          // bypass queue + flow control
    pub fn emergency_stop(&self);                        // M112 now, abort job, mark halted
    pub fn start_print(&self, file: std::sync::Arc<GcodeFile>, options: PrintOptions) -> Result<(), HostError>;
    pub fn pause(&self); pub fn resume(&self); pub fn cancel(&self);
    /// Pause the running host print (pause script) before each of these 0-based file lines not streamed
    /// yet, as if the file had ";@pause" there; replaces earlier points ("pause at layer N").
    pub fn pause_at_lines(&self, lines: &[usize]);
    /// Exclude (cancel) one object of the running host print, or print it again: effective for
    /// the parts not streamed yet (names from JobInfo::objects; unknown names are reported).
    pub fn exclude_object(&self, name: &str); pub fn include_object(&self, name: &str);
    pub fn start_sd_print(&self, filename: &str) -> Result<(), HostError>;   // M23 + M24, then M27 S<n>
    pub fn upload_to_sd(&self, file: std::sync::Arc<GcodeFile>, sd_name: &str) -> Result<(), HostError>; // M28 ... M29 as a job
    /// Answer an active host prompt (M876 S<index>).
    pub fn answer_prompt(&self, button: usize);

    pub fn with_state<R>(&self, f: impl FnOnce(&PrinterState) -> R) -> R;
    pub fn snapshot(&self) -> PrinterState;              // full clone (console excluded)
    pub fn subscribe(&self) -> crossbeam_channel::Receiver<Event>;
    /// Called (from engine threads) whenever state changes; UIs use it to
    /// request a repaint. Replaces any previous waker.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static);
    /// Console entries with `seq > after`, at most `max`, oldest first.
    pub fn console_since(&self, after: u64, max: usize) -> Vec<ConsoleEntry>;
    pub fn clear_console(&self);

    // Convenience controls (build the G-code and `send` it):
    pub fn home(&self, axes: Axes);                      // G28 [X] [Y] [Z]
    pub fn jog(&self, axis: Axis, distance_mm: f32, feedrate_mm_min: f32); // G91, G1, G90
    pub fn move_to(&self, x: Option<f32>, y: Option<f32>, z: Option<f32>, feedrate_mm_min: f32);
    pub fn extrude(&self, tool: u8, length_mm: f32, feedrate_mm_min: f32); // negative = retract; M83 G1 E.. (restores M82 if needed)
    pub fn set_temperature(&self, heater: Heater, celsius: f32); // M104 T / M140 / M141
    pub fn set_fan(&self, fan: u8, speed: u8);           // M106 P S / M107
    pub fn set_feedrate_override(&self, percent: f32);   // M220 S
    pub fn set_flow(&self, tool: u8, percent: f32);      // M221 T S
    pub fn babystep_z(&self, mm: f32);                   // M290 Z
    pub fn motors_off(&self);                            // M84
    pub fn select_tool(&self, tool: u8);                 // T<n>
    pub fn request_temperatures(&self);                  // M105
    pub fn request_position(&self);                      // M114
    pub fn request_firmware_info(&self);                 // M115
    pub fn request_settings(&self);                      // M503 (clears + refills state.settings)
    pub fn save_settings(&self);                         // M500
    pub fn load_settings(&self);                         // M501 then M503
    pub fn factory_reset_settings(&self);                // M502 then M503 (UI must confirm first)
    pub fn apply_setting(&self, setting: &SettingCommand); // send to_gcode(), update state optimistically, M503 later
    pub fn request_endstops(&self);                      // M119
    pub fn request_sd_files(&self);                      // M21 if needed, M20 L
    pub fn delete_sd_file(&self, name: &str);            // M30
    pub fn request_bed_mesh(&self);                      // M420 V (Marlin) / BED_MESH_OUTPUT (Klipper)
    pub fn probe_bed_mesh(&self);                        // G28 (if needed) + G29, then report
    pub fn request_stats(&self);                         // M78
    pub fn start_pid_autotune(&self, heater: Heater, target: f32, cycles: u32); // M303 E<n|-1> S C U1
    pub fn firmware_restart(&self);                      // M999 (FIRMWARE_RESTART on a shut-down Klipper) (+ reconnect logic if board resets)
    /// Reset the board (E-stop recovery: M999 alone does not recover Marlin after M112/kill()).
    /// Serial: reopen the port with a DTR pulse and handshake again; simulator: SimControl::reset();
    /// TCP and custom links: Err(HostError::Unsupported) (reset button or power cycle needed).
    pub fn reset_board(&self) -> Result<(), HostError>;
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisconnectSummary {                         // impl Display: "Cancelled cube.gcode (the cancel script ran), then disconnected", ...
    pub was_connected: bool,
    pub cancelled: Option<(JobKind, String)>,          // the host print or upload cancelled first
    pub cancel_status: Option<JobStatus>,              // Cancelled (acknowledged), Failed (meanwhile), Completed (finished before the
                                                       //   cancel reached it), None (timed out: safe stop sent)
    pub sd_print_continues: Option<String>,            // an SD print left running on the printer
}
impl DisconnectSummary { pub fn acknowledged(&self) -> bool; }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Heater { Hotend(u8), Bed, Chamber }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis { X, Y, Z, E }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Axes { pub x: bool, pub y: bool, pub z: bool }
impl Axes { pub const ALL: Axes; pub const XY: Axes; pub const X: Axes; pub const Y: Axes; pub const Z: Axes; pub const NONE: Axes;
            pub fn has(&self, Axis) -> bool; pub fn union(self, Axes) -> Axes; pub fn without(self, Axes) -> Axes;
            pub fn is_empty(&self) -> bool; pub fn letters(&self) -> String /* "X Y" */; }

// ---- jobs ----
#[derive(Clone, Debug, Default)]
pub struct PrintOptions {
    pub start_line: usize,                     // stream from this line (0 = beginning)
    pub preamble: Vec<String>,                 // sent before the first streamed line (resume_preamble)
    pub toolpath: Option<std::sync::Arc<Toolpath>>, // enables time-based progress, ETA and layer tracking
    pub pause_script: Option<String>,          // overrides the profile scripts (templated, see `script`)
    pub resume_script: Option<String>,
    pub cancel_script: Option<String>,
    pub emit_progress: bool,                   // send M73 P<pct> R<min> as progress changes (default false)
}

// ---- state ----
#[derive(Clone, Debug, Default)]
pub struct PrinterState {
    pub status: ConnectionStatus,
    pub target: Option<String>,                 // ConnectTarget::label()
    pub firmware: Option<FirmwareInfo>,
    pub capabilities: Vec<(String, bool)>,      // in report order
    pub temps: TempReport,                      // latest
    pub temp_history: std::collections::VecDeque<TempSample>, // ~1 Hz, last 30 min
    pub position: Option<PositionReport>,       // last M114/M154 report
    pub settings: PrinterSettings,              // from M503
    pub settings_loaded: bool,
    pub job: Option<JobInfo>,
    pub sd: SdState,
    pub endstops: Vec<(String, bool)>,
    pub mesh: Option<BedMesh>,
    pub stats: Option<StatsReport>,
    pub homed: Option<Axes>,                    // None = unknown (since connecting); Some(axes homed by acked G28s; others count as not homed)
    pub babystep_z: f32,                        // M290 Z/S acked since Z was homed (G28 of Z), a reset/halt or connecting (mm)
    pub active_tool: u8,
    pub feedrate_pct: f32,                      // last set via M220 (100 default)
    pub flow_pct: Vec<f32>,                     // per tool (100 default)
    pub fan_speeds: Vec<u8>,                    // 0..=255 per fan index
    pub busy: Option<String>,                   // firmware busy message, cleared on ok
    pub prompt: Option<HostPrompt>,             // active //action:prompt
    pub autotune: Option<AutotuneState>,
    pub link: LinkStats,
    pub halted: bool,                           // firmware halted/killed (M112, thermal runaway...)
    pub restartable: bool,                      // while halted: firmware_restart() can recover (Marlin "stopped ... M999", Klipper); false after kill() -> offer reset_board()
    pub last_error: Option<String>,
    pub firmware_messages: std::collections::VecDeque<String>, // recent echo:/M117 messages (last 50)
}
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ConnectionStatus { #[default] Disconnected, Connecting, Handshaking, Connected, Failed(String) }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TempSample { pub t: f64 /* secs since connect */, pub hotends: Vec<(f32, f32)> /* (current, target) */,
                        pub bed: Option<(f32, f32)>, pub chamber: Option<(f32, f32)>, pub hotend_power: Vec<f32>, pub bed_power: Option<f32> }
#[derive(Clone, Debug, PartialEq)]
pub struct JobInfo {
    pub kind: JobKind,                 // HostPrint | SdPrint | SdUpload
    pub name: String,
    pub path: Option<std::path::PathBuf>, // GcodeFile::path of a host print or upload (match the job to the loaded file)
    pub status: JobStatus,
    pub total_lines: usize, pub current_line: usize, // last line acknowledged by the printer
    pub bytes_total: u64, pub bytes_done: u64,
    pub elapsed: std::time::Duration,  // excluding paused time
    pub progress: f32,                 // 0..=1, time-based when a toolpath is known
    pub eta: Option<std::time::Duration>, // remaining, corrected by actual/estimated speed ratio
    pub estimated_total: Option<std::time::Duration>,
    pub layer: Option<(usize, usize)>, // (current index, total)
    pub filament_used_mm: f32,
    pub pause_at: Vec<usize>,          // host prints: pause points still ahead (0-based lines, ascending)
    pub stopped_at: Option<usize>,     // failed/cancelled host print: first 0-based line not acknowledged (the recovery start_line); kept until the next job
    pub objects: Vec<String>,          // host prints: object labels of the file, first-seen order
    pub excluded_objects: Vec<String>, // excluded while printing
    pub current_object: Option<String>,// the object the stream is in (as sent)
    pub paused_by_firmware: bool,      // Paused by the firmware (//action:pause[d], a wait for the user holding a host print):
                                       //   resume() runs no resume script, it lets the firmware continue (M108 if it waits)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum JobKind { HostPrint, SdPrint, SdUpload }
#[derive(Clone, Debug, PartialEq)]
pub enum JobStatus { Starting, Printing, Pausing, Paused, Resuming, Cancelling, Completed, Cancelled, Failed(String) }
impl JobStatus { pub fn is_active(&self) -> bool; }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SdState { pub card_present: Option<bool>, pub files: Vec<SdFile>, pub listing: bool, pub printing: Option<(u64, u64)>, pub selected: Option<String> }
#[derive(Clone, Debug, PartialEq)]
pub struct HostPrompt { pub message: String, pub buttons: Vec<String> }
#[derive(Clone, Debug, PartialEq)]
pub struct AutotuneState { pub heater: Heater, pub target: f32, pub cycles: u32, pub running: bool,
                           pub result: Option<(f32, f32, f32)>, pub error: Option<String> }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkStats {
    pub connected_secs: f64, pub lines_sent: u64, pub bytes_sent: u64, pub bytes_received: u64,
    pub oks: u64, pub resends: u64, pub errors: u64, pub timeouts: u64,
    pub rtt_last_ms: f32, pub rtt_avg_ms: f32, pub rtt_max_ms: f32, // without commands that block in the firmware (G28/G29/G4/M400/heat and user waits/M303)
    pub queue_len: usize, pub in_flight: usize, pub lines_per_sec: f32,
}

// ---- events & console ----
#[derive(Clone, Debug)]
pub enum Event {
    Status(ConnectionStatus),
    Console(ConsoleEntry),
    Temperatures(TempReport),
    Position(PositionReport),
    Firmware(FirmwareInfo),
    SettingsUpdated,
    Job(JobStatus),
    JobProgress { line: usize, total: usize },
    SdFiles(Vec<SdFile>),
    Endstops(Vec<(String, bool)>),
    Mesh(BedMesh),
    Stats(StatsReport),
    AutotuneFinished { heater: Heater, kp: f32, ki: f32, kd: f32 },
    AutotuneFailed(String),
    Prompt(Option<HostPrompt>),
    FirmwareMessage(String),
    Error(String),
}
#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleEntry { pub seq: u64, pub time: f64 /* secs since Printer::new */, pub kind: ConsoleKind, pub text: String,
                          /// Automatic traffic (temperature polling/auto-reports and their oks). UIs hide these by default.
                          pub auto: bool,
                          /// Line number a sent command went out with (`N<n>`); None for unnumbered and received lines.
                          pub line: Option<u32>,
                          /// A line of a streamed job: sent file/preamble/progress lines (not scripts) and the oks that answer them.
                          pub stream: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum ConsoleKind { Sent, Received, Info, Warning, Error }

// ---- config ----
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)] #[serde(default)]
pub struct AppConfig { pub profiles: Vec<PrinterProfile>, pub active_profile: usize,
                       pub recent_files: Vec<std::path::PathBuf>, pub macros: Vec<Macro>, pub console_max_lines: usize }
impl AppConfig {
    pub fn load() -> AppConfig;                        // load_or_default(&path())
    /// Lenient load: keeps every valid profile, macro and value; drops (and reports) only broken lines and
    /// values; the file as it was goes to a new numbered backup (never overwriting one). UIs show report.warnings.
    pub fn load_checked(path: &std::path::Path) -> (AppConfig, ConfigReport);
    pub fn load_from(path: &std::path::Path) -> Result<AppConfig, HostError>;
    pub fn save(&self) -> Result<(), HostError>;
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), HostError>;
    pub fn path() -> std::path::PathBuf;               // $SPRINT_CONFIG if set, else default_path()
    pub fn default_path() -> std::path::PathBuf;       // <config dir>/Sprint/config.toml
    pub fn active(&self) -> &PrinterProfile; pub fn active_mut(&mut self) -> &mut PrinterProfile;
    pub fn push_recent(&mut self, path: std::path::PathBuf);
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)] #[serde(default)]
pub struct PrinterProfile {
    pub name: String,
    pub connection: ConnectTarget,       // default Simulator(SimConfig::default())
    pub options: ConnectOptions,
    pub auto_connect: bool,              // default false
    pub bed_size: [f32; 2],              // [220, 220]
    pub max_z: f32,                      // 250
    pub origin: BedOrigin,               // FrontLeft (default) | Center
    pub extruders: u8,                   // 1
    pub heated_bed: bool,                // true
    pub heated_chamber: bool,            // false
    pub filament_diameter: f32,          // 1.75
    pub nozzle_diameter: f32,            // 0.4
    pub temp_presets: Vec<TempPreset>,   // PLA 205/60, PETG 240/80, ABS 250/100, TPU 225/50
    pub jog_feedrate_xy: f32,            // mm/min 3000
    pub jog_feedrate_z: f32,             // mm/min 600
    pub extrude_feedrate: f32,           // mm/min 300
    pub pause_script: String,            // templated, see script module
    pub resume_script: String,
    pub cancel_script: String,
    pub emit_progress: bool,             // default false: host prints send M73 P R (as if PrintOptions::emit_progress)
    pub on_start_command: String,        // hooks (empty = none): shell commands run when a print (host or SD) starts,
    pub on_finish_command: String,       //   completes, fails or is cancelled (Printrun's start/final/error commands);
    pub on_fail_command: String,         //   env SPRINT_FILE, SPRINT_STATUS, SPRINT_ELAPSED_SECS, SPRINT_PRINTER (+ SPRINT_REASON
    pub on_cancel_command: String,       //   for failures); helper thread, `cmd /C` / `sh -c`, 2 min limit; result to the console
    pub keep_heaters_on_failure: bool,   // default false: a host print the engine gives up on gets every heater and the fan switched off
    pub kinematics: Option<KinematicLimits>, // None = defaults / from M503
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TempPreset { pub name: String, pub hotend: f32, pub bed: f32 }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BedOrigin { #[default] FrontLeft, Center }
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Macro { pub name: String, pub gcode: String, pub color: Option<[u8; 3]>, pub confirm: bool }

// ---- script ----
/// Expand `{x} {y} {z} {e} {f} {tool} {hotend} {bed} {fan}` placeholders
/// with values from a MachineState (position at pause time etc.).
pub fn render_script(template: &str, state: &MachineState) -> Vec<String>;
/// Plus the profile-derived {max_x} {max_y} {max_z} {park_x} {park_y} and
/// {heaters_off} (`PrinterProfile::script_context(&state)` builds one).
pub struct ScriptContext<'a> { pub state: &'a MachineState, pub max_x: f32, pub max_y: f32, pub max_z: f32, pub park_x: f32, pub park_y: f32,
                               pub hotends: u8, pub heated_bed: bool, pub heated_chamber: bool }
impl ScriptContext<'_> { pub fn new(state: &MachineState) -> ScriptContext<'_>; pub fn heaters_off(&self) -> Vec<String>; }
pub fn render_script_with(template: &str, ctx: &ScriptContext<'_>) -> Vec<String>;
/// "M104 S0" (one "M104 T<n> S0" per hotend when there are several), "M140 S0" with `bed`, "M141 S0" with `chamber`.
pub fn heaters_off(hotends: u8, bed: bool, chamber: bool) -> Vec<String>;
/// Macro arguments (pronsole's syntax, as the shell's and TUI's `macro` command): `{0}`, `{1}`, ... in order,
/// `{1=60}` with a default, `{args}` all of them joined by spaces; other `{...}` stay for render_script_with.
pub enum MacroArg { Index { index: usize, default: Option<String> }, All }
pub enum MacroArgsError { Missing { needed: usize, given: usize }, Unexpected { given: usize } } // Display + Error
pub fn expand_macro_args(text: &str, args: &[String]) -> Result<String, MacroArgsError>;
pub fn macro_args(text: &str) -> Vec<MacroArg>;        // each placeholder once, first-appearance order
pub fn macro_args_needed(text: &str) -> usize;         // highest `{n}` without a default + 1

#[derive(Debug, thiserror::Error)]
pub enum HostError { /* NotConnected, AlreadyConnected, Busy(String), Halted, Io(std::io::Error), Serial(String), InvalidTarget(String), Config(String),
                        InvalidArgument(String), Timeout(String), Unsupported(String), Cancelled, ... */ }
```

Engine design (inside sprint-host):
* `connect` opens the link and spawns a **reader thread** (bytes → lines →
  channel) and an **engine thread** that owns the writer and runs a
  `crossbeam_channel::select!` loop over: received lines, handle commands,
  and a 20 ms tick (timeouts, polling, job pacing, stats).
* Line numbering: with `ChecksumMode::Always` every queued command is sent as
  `N<n> <cmd>*<cs>`; after the handshake `M110 N0` resets the counter. A ring
  of the last 1000 sent framed lines serves `Resend:` requests (resend from
  N, dropping duplicates of the "ok" that follows). Emergency commands are
  sent unnumbered.
* Flow control: `max_in_flight` commands may await `ok`. `busy:` replies
  extend the timeout. On timeout (no reply, not busy) the engine assumes a
  lost `ok`: logs a warning, increments `timeouts` and sends `M105` to
  re-synchronise, without duplicating the outstanding command.
* Handshake: wait for `start` or any line, then send `M110 N0` (repeat every
  2 s) until `ok`; then (if enabled) `M115`, `M155 S<n>` if AUTOREPORT_TEMP,
  `M154 S1` if AUTOREPORT_POS, `M503`, `M114`. `Firmware`, `Capability` and
  `Setting` replies populate state.
* Jobs stream `GcodeFile::command(i)` lines; manual commands interleave with
  priority. Pause: stop pulling stream lines, wait for in-flight oks, `M400`,
  record `MachineState` (from the streamed lines), run the pause script.
  Resume: run the resume script (restores position/E/feedrate via
  placeholders) then continue. Cancel: drop the stream, run the cancel script.
  `//action:pause|resume|cancel` from the firmware drive the same paths.
* The waker is invoked at most every 16 ms.

### Implementation notes (additive API and clarified semantics)

Every public item is also re-exported at the crate root.

* `transport`: `DEFAULT_BAUD` (115200, used by `COM3` without `@baud`;
  `@auto`/`@0` = auto-detect); `Display` for `ConnectTarget` is the
  parseable form (`COM3@115200`, `tcp://[::1]:23`, `sim://?seed=7`);
  `ConnectTarget::is_simulator` and `Default` (the simulator).
  `sim://` query keys are the `SimConfig` field names (`build_volume=x,y,z`,
  `mesh_size=5x5`, booleans without a value mean true); unknown keys and
  bad values are `InvalidTarget`. `Link` (`new(reader, writer)`,
  `simulator`, `tcp`, `serial(port, baud, reset, log)`, `open(target,
  reset, log)`, `sim_control`, `split() -> (reader, writer, LinkHandle)`),
  `LinkHandle::{close, sim_control}`, `READ_TIMEOUT` (100 ms),
  `is_no_data(&io::Error)`, `PRINTER_USB_VENDORS`, `is_printer_vendor`.
  Auto-baud probes each rate with `M110 N0` + `M105` for 2.5 s and stops at
  the first open error. A serial port is driven by one I/O thread that both
  reads and writes (not a reader on `try_clone`): Windows serialises
  synchronous I/O on a handle and its clones, so a read waiting for data
  would hold every write back for the read timeout. After data arrives the
  thread waits up to 5 ms for the engine's answer before reading again
  (20 ms port timeout). Writes that make no progress (on Windows a
  timed-out write returns `Ok(0)`) are retried for up to 5 s before the
  link counts as lost; what was written before a close still goes out.
* `config`: `CONFIG_ENV` (`SPRINT_CONFIG`): `AppConfig::path()` (and so
  `load()`/`save()`) use the file it names when set and not blank, so every
  front end (GUI included) honours it for loading, auto-connecting and
  saving; `default_path()` is the platform location.
  `ConnectOptions` is `#[serde(default)]`; `sanitized()` clamps
  every value (`max_in_flight` 1..=32, `MAX_IN_FLIGHT_LIMIT`).
  `AppConfig::load_or_default(path)` (what `load()` uses:
  `load_checked` with the warnings logged). `load_checked` reads a file
  that does not fully parse leniently: lines that are not valid TOML (a
  decimal comma, binary junk; invalid UTF-8 is replaced first) are dropped,
  then every value is checked where it sits: table entries are kept when
  valid on their own (or the one entry breaking a table with required
  fields, like a macro, is dropped), array elements one by one, so a bad
  value (`checksum = "always"`, a connection kind from a newer version)
  falls back to its default without costing the profile or the other
  profiles and macros. `ConfigReport { warnings, backup }` lists each
  dropped line or value (`profiles[0].options.checksum: ignored "always"
  (...)`); the original bytes are copied to `<name>.bak`, `<name>.1.bak`,
  ... (an identical existing backup is reused, none is ever overwritten)
  and the file itself stays until the next save. `load_from` stays strict.
  `MAX_RECENT_FILES` (12), `DEFAULT_CONSOLE_LINES`.
  `PrinterProfile::{build_volume, park_position, script_context}`: parking is
  the back left corner 10 mm in (X10 Y210 by default, honouring `origin`).
  In the file, `kinematics.junction_deviation = false` stands for `None`
  (classic jerk), which TOML cannot write as a missing key.
* `script`: `DEFAULT_PAUSE_SCRIPT`, `DEFAULT_RESUME_SCRIPT`,
  `DEFAULT_CANCEL_SCRIPT`, `format_number`. `render_script` uses
  `ScriptContext::new` (the default 220 x 220 x 250 profile with one
  hotend and a heated bed). `{heaters_off}` expands to several lines
  (`heaters_off`): a bare `M104 S0` only reaches the active tool, so the
  default cancel script and the default "Cool down" macro
  (`{heaters_off}` + `M107`) use it. The engine (job scripts) and
  `run_macro` widen it to every hotend, bed and chamber the printer
  reports or the job heated, whatever the profile says. Profiles and
  macros still holding an earlier default (never edited) get the current
  one when the configuration loads. Unknown
  placeholders stay verbatim; comments and blank lines are dropped; a line
  using `{e}` is dropped while extrusion is relative (that is how the
  resume script's `G92 E{e}` disappears for M83 files).
* `fs`: `write_atomic` (also at the root) is what `AppConfig::save_to`
  uses. Each call writes its own `<name>.<pid>-<n>.tmp`, so concurrent
  writers of one path never mix (the last rename wins).
* `console`: `Console` (the bounded ring; `seq` starts at 1 and survives
  `clear`; `push_stream` adds a `stream` entry). Sent entries carry the clean command and its `line` number;
  resent lines are logged again.
* `state`: `PrinterState::{is_connected, has_capability, job_active,
  push_message, waiting_for_user}` (`busy` is "paused for user" or
  "paused for input"), `ConnectionStatus::{is_connected, is_open, label}`,
  `Heater::{name, autotune_index, from_autotune_index}`, `Axis::letter`,
  `Axes::home_command`, `TEMP_HISTORY_LEN`, `TEMP_HISTORY_SECS` (the
  history spans at most 30 minutes, whatever the report rate),
  `FIRMWARE_MESSAGES_LEN`. Temperature reports are merged (readings missing
  from a report keep their value); `Event::Temperatures` carries the merged
  report. Echo lines that arrive while an M503/M501 report is in flight
  belong to the report (console only, not `firmware_messages`). The
  `request_*` queries (temperatures, position, firmware info, settings,
  endstops, SD files, bed mesh, stats), which UIs
  send on their own, are marked as the host's: `echo:` lines and "Unknown
  command" answering them, or the setup queries, stay in the console and
  are neither `Event::FirmwareMessage`s nor `firmware_messages` (the same
  command sent with `send` still is).
* `event`: `EventBus` (public fan-out), `EVENT_QUEUE_LIMIT`: a subscriber
  that stops reading keeps at most 65536 events; newer ones are dropped for
  it (the state stays authoritative). Queues grow with the events queued
  (nothing is allocated up front).
* `job`: `JobStatus::{is_finished, is_paused, label}`, `JobInfo::new`,
  `ProgressModel` / `Progress` (the progress math, usable on its own).
  `current_line` counts processed file lines (1-based number of the last
  acknowledged line, `total_lines` at completion, `start_line` at the
  start). ETA: remaining estimate x (running time without the commands
  estimates count as zero: heat waits, homing, probing and leveling (also
  Prusa's G80), user and pin waits and filament changes, autotune, extended
  commands; not G4/M400 /
  estimated time of the printed part; a pause, a firmware-held wait for
  the user included, is neither), ratio clamped to 0.5..=3 and
  smoothed; raw estimate for the first 2%; without a toolpath a smoothed
  linear extrapolation by bytes after 2%. `JobProgress` at most every 100 ms.
* `Printer` additions: `with_console_capacity(lines)`,
  `set_console_capacity`, `set_profile` / `profile` (scripts, parking and
  geometry come from the profile unless `PrintOptions` overrides them),
  `connect_profile(&profile)`, `connect_link(link, label, options)` (custom
  transports), `sim_control()` (while connected to the simulator),
  `run_macro(&Macro)` and `run_macro_with_args(&Macro, &[String]) ->
  Result<(), HostError>`: the arguments are filled in first
  (`expand_macro_args`; `run_macro` gives none, so defaults apply and
  `{args}` is empty), then the placeholders are templated with the
  current position/targets. A macro whose arguments cannot be filled in
  is not sent: a console error ("Macro Heat not sent: it needs 1
  argument (0 given)") and `InvalidArgument`, never placeholders sent
  verbatim; `NotConnected` without a connection. A placeholder whose
  number is too large for `usize` can never be given (the TUI's own
  `macro_gcode` leaves it as text).
  Commands without a connection are dropped with a console warning;
  `start_*` wait up to 5 s for the engine's answer and fail with
  `NotConnected`, `Halted`, `Busy` (a job is active) or `InvalidArgument`.
  `send("M112")` is `emergency_stop()`, `send("M999")` is
  `firmware_restart()`. An `M410` (quick stop) sent while a host print
  streams fails the job the way a protocol failure does (heaters off): the
  firmware discarded its planned moves, so the print has lost segments;
  `M114` then reads the position back.
* Handshake: the first `M110 N0` goes out on the first received line or
  after 1 s of silence, again every 2 s and at once after a `start`; after a
  handshake that needed retries stray `ok`s are ignored for 300 ms. The
  status becomes `Connected` when M110 is acknowledged; then `M115`, and
  when it is answered `M155 S<autoreport_interval_secs>` (AUTOREPORT_TEMP),
  `M154 S1` (AUTOREPORT_POS), `M503` (unless the flavor has no M503) and
  `M114`. Replies on an `ok` line (`ok T:..`, `ok FIRMWARE_NAME:..`) are
  applied before the `ok`.
* Flow control (module docs of `engine/flow.rs` have the reasoning):
  duplicate `Resend:`s caused by lines that were in flight behind the
  rejected one are ignored (always before the line was resent, afterwards
  up to the number of such stale lines unless the error was a checksum
  error); with stale lines, retransmission waits until the accepted lines
  are acknowledged and either every stale line was answered or the link
  was quiet for 3 round trips (5..200 ms); after a rewind the window is one
  command until 16 (or 4 windows of) commands were acknowledged cleanly.
  A request beyond the last line sent or for an acknowledged line (the
  firmware's counter changed) and a line rejected 16 times in a row drop the
  numbered lines in flight, fail a host print or an upload (an SD print
  goes on: the printer reads it from its card, whatever the host's line
  numbers; its own commands among those dropped, a pause, resume or
  cancel under way, were rejected for their numbers and go again after
  the reset, so the job changes only once the printer acknowledged them;
  if the line given up on is one of them, the SD print fails) and reset
  the counter (resending
  could execute lines twice). The printer still runs then, so a failed host
  print gets `{heaters_off}` and `M107` right after the counter reset
  (unless `PrinterProfile::keep_heaters_on_failure`), a failed upload `M29`;
  a disconnect before they were sent writes them at once, unnumbered. `ok N<n>` acknowledges line n and every older
  in-flight command. Timeouts (no `ok`/`busy`/heat-wait report; homing,
  probing, heat waits, dwells and M600 get 6x `ok_timeout_secs`) and Marlin's
  `wait` with commands in flight release every in-flight command and send a
  numbered M105. M110 is a barrier (sent with nothing in flight, repeated on
  any resend request). Emergency commands go out unnumbered and wait for
  their `ok` outside the window (M112 and M999 expect none). `PrintOnly`
  numbers while any job is active; SD uploads are always numbered.
* Polling: M105 every `temp_poll_secs` without auto-reports, never while a
  poll is outstanding, during M109/M190/M191/M303, during uploads, while
  halted or before M115 was answered. An `M155 S<n>` (or `M27 S<n>`) stops
  polling only when the firmware has AUTOREPORT_TEMP (AUTOREPORT_SD_STATUS).
  Nothing but the upload reaches the printer while it writes to the card.
* Halt: a fatal error or `emergency_stop` sets `halted`, fails the job and
  frees the window (E-stop also drops the queues); the link stays open and
  commands are sent unnumbered meanwhile. `restartable` tells a Marlin
  stop() ("Printer stopped ... use M999", or Klipper) from a kill() (M112,
  thermal runaway, "kill() called"), which ignores everything until the
  board resets: UIs offer `reset_board` first then. `firmware_restart`
  sends M999 (FIRMWARE_RESTART on a shut-down Klipper only: it reboots
  the MCUs, which would abort a running print);
  the next `ok`/resend clears `halted` and the counter is reset. A `start` while connected re-runs the handshake and
  setup (status `Handshaking`, then `Connected`) and fails a job with
  "printer reset". Entering a halt and a board reset drop the user
  commands still queued (with a console warning): they were meant for a
  printer that has since lost its position. Both also close an open host
  prompt (`Event::Prompt(None)`): a halted firmware ignores M876. Both
  reset the positioning, extrusion and unit modes the engine tracks over
  the commands it sent (they decide what `extrude` restores and which
  modes the job scripts set first) to the firmware's defaults (G90, M82,
  G21): a reset must not leave the host thinking an M83 is active. (Not
  a halt Marlin restarts with M999, "Printer stopped ... use M999":
  stop() and M999 keep the modes. A kill is followed by a board reset,
  Klipper's FIRMWARE_RESTART restarts in the defaults.)
* Homing (`PrinterState::homed`): unknown after connecting; an
  acknowledged `G28` (any source, the job stream included) adds all or
  the listed axes; `M84`/`M18` without `S` clear all or the listed axes
  (`E` alone none); halts and board resets clear it to `Some(NONE)`, a
  disconnect to `None`. The pause, resume and cancel scripts never move
  an axis that is not known to be homed (Marlin has no soft endstops for
  it, and the resume's return to the file's position is X0 Y0 Z0 before
  the file moved at all): its
  words are left out of G0/G1 moves (a move left without axes is dropped,
  arcs entirely; a `G28` in the script counts for the lines after it),
  with a console warning. So a pause during a heat-up before the file's
  G28 retracts but neither lifts nor parks, and its resume does not move
  back either. `probe_bed_mesh` homes first
  unless all axes are homed.
* Jobs: preamble lines are cleaned and sent first; with `start_line` the
  tracked `MachineState` starts from `state_at_line`. Pause: `Pausing` until
  in-flight stream lines are acknowledged and the pause script is
  acknowledged, then `Paused` (manual commands work meanwhile); resume:
  `Resuming` while the resume script is acknowledged, streaming continues
  right behind it. User commands queued before a script (pause, resume,
  cancel, SD control) are sent before it, so a purge typed while paused
  runs before the head returns to the part; commands queued afterwards
  follow the script. Scripts run in the modes templates are written for,
  judged from every command sent plus those queued before the script: the
  pause and cancel scripts start with `G21` (printer in inches) and `G90`
  (printer relative) when needed; the resume script always starts with
  `G90` and the stream's extruder mode (`M82`/`M83`, after `G21` when the
  printer is in inches) and ends with the stream's `G90`/`G91`, its
  extruder mode again (Marlin's `G90`/`G91` reset it) and `G20` for an
  inch stream, so a `G91` or `G20` typed while paused cannot shift the
  rest of the print. Between the leading modes and the template the resume
  heats every heater the job used (targets of the pause-time
  `MachineState`) whose target was changed or that cooled more than 5 °C
  below it back up (all targets, then `M190`/`M191`/`M109`, hotends by
  `T<n>` when there are several), and after the template a part fan
  changed while paused gets its speed back (`M106 S<n>`/`M107`), so
  cooling down during a long pause never resumes with a cold nozzle.
  A pause while a heat wait (M109/M190/M191) or a wait for the user
  (M0/M1) of the stream is in flight ends it with M108 (repeated at most
  once a second while the firmware reports it still waits: an M108 that
  arrives before the wait begins is lost), so `Pausing` does not last
  minutes. Only with firmware reporting `Cap:EMERGENCY_PARSER:1`: any
  other reads nothing while it waits, so M108 could not end the wait and
  the repeats would overflow its receive buffer (the pause then waits for
  the wait to end). M600 keeps its own dialogue. When the active hotend is more
  than 5 °C short of its target the pause script's extruder-only moves
  (the retract) are left out, and the resume's (the prime) with them.
  Objects (exclude / cancel while printing): the engine reads the labels
  from the stream itself, one style per file, the first present of
  Klipper `EXCLUDE_OBJECT_DEFINE/START/END NAME=..`, Marlin `M486`
  (`T<count>`, `S<id>` starts, `S-1` ends, `A"name"`/`Aname` names;
  unnamed objects are "Object <id>"), `; printing object NAME` / `; stop
  printing object NAME`, Cura `;MESH:NAME` (until the next `;MESH:`;
  `;MESH:NONMESH` and the end of the layer, `;TIME_ELAPSED:` or `;LAYER:`,
  end it, since Cura leaves the last mesh of a layer open and the end
  G-code must not count as part of it); names are the marker text,
  trimmed. Inside an
  excluded object the host sends none of its G0..G3 and G10/G11 (moves,
  extrusions, retractions, primes) but follows them in the job's
  `MachineState`; everything else (fan, temperatures, `G92`, modes, the
  markers) is sent. Before the next move after skipped ones it sends a
  non-extruding `G0 X Y Z F6000` to the file's position, `G1 F<file
  feedrate>` and, for absolute extrusion, `G92 E<file E>` (in absolute mm,
  the file's modes restored after).
  Host commands in the file are never sent to the firmware: `@pause` and
  Printrun's `;@pause` pause the job there (pause script), `@cancel` and
  `@abort` cancel it, other `@...` lines are reported and skipped (uploads
  leave them all out). Pause points (`pause_at_lines`) act like `;@pause`
  before their line.
  `//action:pause|paused` pause
  without scripts and `//action:resume|resumed` continue. A host print
  the firmware holds at a wait for the user (`busy: paused for user` or
  `paused for input` while `Starting`/`Printing`: M0/M1, M600, a typed
  M0) is paused by the firmware too. `JobInfo::paused_by_firmware` marks
  such pauses (only while `Paused`): `resume()` runs no resume script but
  lets the firmware continue, with one M108 when it waits for the user
  (never when it does not, never a second one for the same wait: a
  "paused for user" arriving within 2 s of an M108 the host sent may have
  left the printer before it got the M108 and counts as no new wait); the
  job also continues when the printer acknowledges its M0/M1/M600 (its
  button, an answered prompt) or reports `//action:resumed`. `pause()`
  while the firmware holds the print at an M0/M1 of the stream ends the
  wait and runs the pause script, as from `Printing`. `cancel` sends M108 first when
  a heat or user wait is in flight. SD prints: M23/M24 (`Starting` until
  M24 is acknowledged), `M27 S<n>` with AUTOREPORT_SD_STATUS (switched off
  with M27 S0 afterwards) else M27 polling; "open failed" or a missing card
  fails the job; "Not SD printing" while printing ends it (Completed when
  the whole file was reported read, else Failed); cancel = M108 + M524.
  `SdState::printing` means "the card prints now": each progress report
  sets it (Sprint's SD print, or one started on the printer, which the
  host then follows with M27 polls every 2 s unless SD auto-reports run);
  "Not SD printing", "Done printing file", the end of Sprint's SD print
  (however it ends; cleared before the final job status is published), a
  halt, a board reset and the connection closing clear it, and so does
  missing 3 report intervals (6 s by default; 3 x n with `M27 S<n>`) while
  no M27 waits for its answer and Sprint's own SD print is not running.
  Uploads stream only after "Writing to file" (M28's `ok` without it fails
  the job), skip the file's own M28/M29, end with M29 (complete on "Done
  saving file." or on M29's `ok`); cancel = M29 + M30 <name>. An M303
  acknowledged without a result or failure message fails the autotune.
  A halt, a board reset or the connection closing ends a running
  autotune: `running` false, `error` "the printer halted" / "the printer
  reset" / the disconnect reason, and `Event::AutotuneFailed` (once).
  `start_pid_autotune` while halted fails the same way at once without
  sending M303 (an M303 typed while halted is sent but fails at once).
* `disconnect_safely(timeout)`: a host print or SD upload that is active
  is cancelled as with `cancel()` and the call waits (polling the state)
  until the job has ended, the link closed by itself, or `timeout`; at the
  timeout the engine queues every heater off (all the printer reports or
  the job heated, whatever `keep_heaters_on_failure` says) and `M107`
  (host print) or `M29` (upload) as safe-stop commands, which the
  disconnect writes at once. An active SD print is left alone; closing
  the connection then fails its record with "disconnected; the printer
  goes on printing <name> from its SD card" (as a plain `disconnect()`
  does). Not connected: just `disconnect()`.
* Threads: `disconnect()` and dropping the last handle join each thread for
  at most 2 s, then detach it. A panic in the engine leaves status (and any
  job) `Failed`. The waker is called without the rate limit when the engine
  stops and after changes made by a handle; the link clock
  (`connected_secs`) alone wakes it once per second.
* Throughput against the simulator with instant moves (`cargo test -p
  sprint-host --test print -- --ignored --nocapture`): about 28k lines/s
  ping-pong, 45k with 2, 79k with 4 and 117k with 8 commands in flight.

---

## sprint-viewer

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColorMode { #[default] Feature, Speed, Width, LayerHeight, Tool, Fan, Layer, Time }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderStyle { #[default] Tubes, Lines }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode { #[default] Perspective, LayerTop /* orthographic top-down of the top visible layer, lower layers ghosted */ }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewPreset { Iso, Top, Bottom, Front, Back, Left, Right }
#[derive(Clone, Debug, PartialEq)]
pub struct ViewerOptions {
    pub color_mode: ColorMode,
    pub style: RenderStyle,
    pub view_mode: ViewMode,
    pub show_travel: bool, pub show_retractions: bool,
    pub show_bed: bool, pub show_build_volume: bool, pub show_axes: bool,
    pub hidden_features: [bool; Feature::COUNT],   // true = hidden
    /// Inclusive layer range shown (clamped to the toolpath).
    pub layer_range: (usize, usize),
    /// Moves within the top layer are shown up to this source line (for
    /// "scrubbing" through a layer); None = whole layer.
    pub line_limit: Option<u32>,
    /// Print progress: segments with line > this are drawn ghosted.
    pub progress_line: Option<u32>,
    /// Source line to highlight (selection from the G-code text view).
    pub highlight_line: Option<u32>,
    pub show_tool_head: bool,         // marker at the current progress/scrub position
    pub background: [u8; 3],
}
impl Default for ViewerOptions;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BedSpec { pub size: [f32; 2], pub max_z: f32, pub origin_center: bool }

pub struct GcodeViewer { /* GPU resources, camera, cached uploads */ }
impl GcodeViewer {
    pub fn new(render_state: &egui_wgpu::RenderState) -> GcodeViewer;
    pub fn set_toolpath(&mut self, toolpath: std::sync::Arc<Toolpath>); // uploads on next frame; resets layer_range to all
    pub fn toolpath(&self) -> Option<&std::sync::Arc<Toolpath>>;
    pub fn clear(&mut self);
    pub fn set_bed(&mut self, bed: BedSpec);
    pub fn options(&self) -> &ViewerOptions; pub fn options_mut(&mut self) -> &mut ViewerOptions;
    pub fn set_view(&mut self, preset: ViewPreset);
    pub fn fit_to_view(&mut self);
    /// Allocate the remaining space and draw; handles orbit (LMB drag), pan
    /// (RMB/MMB drag or Shift+LMB), zoom (wheel, towards cursor), double-click
    /// to focus, click to pick.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> ViewerResponse;
    /// Compact color legend for the current color mode (features with counts,
    /// or a gradient with min/max values).
    pub fn legend_ui(&mut self, ui: &mut egui::Ui);
    /// Render one frame offscreen and return RGBA8 pixels (tests, thumbnails).
    pub fn render_to_image(&mut self, width: u32, height: u32) -> Option<(u32, u32, Vec<u8>)>;
    /// Physical pixels per logical point: the tool head (46 pt tall) and the
    /// retraction markers keep their size in points, like the pick
    /// tolerance. ui() takes it from the egui context; set it before
    /// render_to_image for a high-DPI image (default 1, clamped 0.25..=8).
    pub fn set_pixels_per_point(&mut self, pixels_per_point: f32);
    pub fn pixels_per_point(&self) -> f32;
}
pub struct ViewerResponse { pub response: egui::Response, pub clicked_line: Option<u32>, pub hovered_line: Option<u32> }
```
Rendering: one instance per segment (positions, width/height, attributes),
expanded to lit boxes in the vertex shader; color modes are computed in the
shader from per-instance attributes + a uniform palette, so switching modes,
layer ranges, progress and highlight never re-upload geometry. Layer ranges
are contiguous instance ranges. Travel moves are a separate line-list buffer.
The scene is drawn into an offscreen MSAA color+depth target and shown as an
egui image (re-rendered only when something changed). Handles 5M+ segments.

---

## sprint-gui

`pub fn run(options: GuiOptions) -> anyhow::Result<()>` (see `src/lib.rs`).
Layout (all panels resizable/collapsible):
* Top: menu bar; connection bar (profile, port picker incl. "Virtual
  printer", baud incl. Auto, connect/disconnect, status pill, firmware name),
  always-visible red **E-STOP**.
* Left: controls — XY jog pad + Z/E jog, homing, motors off, temperatures
  (hotends/bed/chamber with presets), fan, speed/flow overrides, babystep,
  live position, user macros.
* Centre: tabs — 3D view, Layer view, G-code text (line numbers, syntax
  colors, reference tooltips, synced with viewer selection), Temperature
  graph, Printer (firmware/capabilities/stats/endstops/link/thermal
  stability), Tuning (catalog-driven settings editor + EEPROM), Calibration
  (PID autotune, E-steps, bed mesh heatmap), SD card.
* Right: console (filters, search, colored kinds, autocomplete from the
  G-code reference, history).
* Bottom: job bar (open/print/pause/resume/cancel, progress, elapsed, ETA,
  layer, filament) and status bar (link stats).
Dark theme by default (custom palette), light theme option, Phosphor icons,
drag-and-drop G-code, background analysis with progress, toasts, keyboard
shortcuts, confirmation dialogs for dangerous actions.

## sprint-tui

```rust
pub struct TuiOptions { pub file: Option<std::path::PathBuf>, pub connect: Option<ConnectTarget> }
pub fn run_tui(options: TuiOptions) -> anyhow::Result<()>;     // full-screen ratatui app
pub fn run_shell(options: TuiOptions) -> anyhow::Result<()>;   // pronsole-style line shell
```

### Foundation widgets (implemented; `run_tui`/`run_shell` come with the app)

Modules `theme`, `format`, `widgets` (re-exported at the crate root). Every
widget takes its own small input types, never sprint-host types, so the app
maps host state onto them; all take `.theme(&Theme)` (default
`Theme::dark()`) and most a `.block(Block)`. Untrusted text (file comments,
printer lines) is sanitized when drawn, NaN/inf values show as `--`, and
any area size, including zero, is safe.

* `theme`: `Theme::{dark, ansi16, monochrome, detect, for_support}` with
  semantic colors and styles (`console(kind)`, `heater(kind)`,
  `heater_state(state)`, `feature(f)`/`feature_dim(f)`, `key`, `selected`,
  `search_highlight`, ...); `ColorSupport::{detect, from_env}`: `NO_COLOR`
  or `TERM=dumb` -> attributes only, `COLORTERM`/`WT_SESSION`/known
  `TERM_PROGRAM` or Windows without `TERM` -> 24-bit, `*256color` -> 256,
  else 16 hand-picked ANSI colors on the terminal background.
  `ACCENT_RGB` = `#FF7A1A`.
* `format`: `duration`, `clock`, `length_mm`, `percent`, `temperature`,
  `trimmed`, `fixed`, `count`, `bytes`, `relative_difference`.
* `LayerCanvas::new(&Toolpath, layer, progress_line: Option<u32>, bed_size)`
  (Braille canvas; `fit(LayerFit::{Bed, Object})`, `origin_center`,
  `show_previous` (ghost), `show_travel`, `show_tool`, `show_label`,
  `object_bounds`; segments after the progress line are dimmed);
  `FeatureLegend`; `object_bounds(&Toolpath)` (extrusions without custom
  G-code, for view fitting).
* `TempChart::new(&[TempPoint])`, `TempPoint { t, hotends: Vec<(cur, target)>,
  bed, chamber }` (`window`, `now`, `show_targets`; target 0 = off).
* `HeaterGauge::new(HeaterKind::{Hotend(n), Bed, Chamber}, current, target)`
  (`power`, `label`, `label_width`, `bar_width`); `HeaterState::classify`
  (within 2 °C = at target).
* `JobProgress::new(name, progress)` (`elapsed`, `eta`, `layer((index,
  total))`, `status(text, JobTone)`), 1-, 2- or 3-row layouts.
* `ConsoleView::new(&[ConsoleLine])`, a `StatefulWidget` over
  `ConsoleState` (follows the tail; `scroll_up/down`, `page_up/down`,
  `scroll_to_top`, `follow`, `handle_key`, `handle_mouse`; scrolled back,
  the view stays on the same lines as new ones arrive and when the
  auto-traffic filter changes, and `lines_removed(n)` keeps it there when
  the app drops the oldest lines of a full log); `show_auto`, `search`,
  `show_tags`. `ConsoleLine { kind: ConsoleKind, text, auto }` mirrors the
  host's `ConsoleEntry`.
* `CommandInput` (state; `handle_key(KeyEvent) -> InputEvent::{Ignored,
  Handled, Submitted(String)}`, `insert_str` for pastes, history,
  `with_completer`), `CommandLine` (draws it; `cursor_position(area)` for
  `Frame::set_cursor_position`), `CompletionPopup` (`area(input_area,
  bounds)`), `Candidate`, `gcode_candidates` (from `search_gcode`). Key
  releases are `Ignored`, AltGr (Ctrl+Alt) types characters.
* `KeyHints::new([(key, action)])`; `HelpOverlay::new(&[HelpSection])`,
  rendered over the whole screen (it centers and clears its own box).
* `StatsPanel::{new(&Stats, &SlicerMetadata), from_toolpath}`,
  `summary_rows`, `feature_rows`, `part_bounds` (extrusions without custom
  G-code, skirt/brim and wipe tower); `sprint analyze` prints the same rows.

`examples/widgets_demo.rs` shows every widget on a simulated print; its
tests draw each screen with a `TestBackend`.

## sprint (binary)

```
sprint [gui] [FILE] [--sim]          launch the GUI (default)
sprint tui [FILE] [--connect T]      full-screen terminal UI
sprint shell [--connect T]           interactive line shell
sprint print FILE --connect T        headless print with progress output
sprint analyze FILE [--json]         print toolpath statistics
sprint send --connect T CMD...       send commands, print replies
sprint ports                         list serial ports (no opening)
sprint sim [--tcp ADDR]              run the virtual printer over TCP
```
`T` is a `ConnectTarget::parse` string (`COM3@115200`, `tcp://host:port`,
`sim`).

Implementation notes (current state):
* Exit codes: 0 success, 1 failure (`sprint: error: <anyhow chain>` on
  stderr), 2 usage errors and the commands not implemented yet (`tui`,
  `shell`, `print`, `send`, `ports` print `sprint <cmd>: not implemented
  yet`). `--connect` values are only checked to be non-blank so far.
  Logging: env_logger on stderr, `RUST_LOG` (default `warn`; `sim` adds
  `sprint_sim=info`).
* `analyze FILE [--json] [--filament-diameter MM]` (the diameter applies
  when the file's metadata has none, like `AnalyzeOptions`). A progress
  line is drawn on stderr only when it is a terminal. `--json` prints
  `{file: {name, path, bytes, lines, commands}, stats, metadata,
  bounds, travel_bounds, part_bounds, layers: [{z, height, time_start,
  duration, filament_mm, first_line}]}` with thumbnails without their
  bytes; empty bounds and non-finite numbers are `null`.
* `sim [--tcp ADDR] [--time-scale X] [--extruders N]
  [--checksum-error-rate R] [--drop-ok-rate R] [--no-heated-bed]
  [--heated-chamber] [--instant-moves] [--advanced-ok] [--boot-delay-ms MS]
  [--seed N] [--name NAME]`: the first stdout line is `Virtual printer
  listening on <addr>` (the address is its last word; port 0 picks one),
  then connection hints; it serves until the process is stopped.
* GUI mode on Windows: when `GetConsoleProcessList` reports the console is
  this process's alone (started from Explorer), `FreeConsole()` closes it
  and errors are shown in a message box instead.
