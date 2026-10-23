# Changelog

All notable changes to Sprint are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Sprint uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.2.0 - 2026-10-22

### Added

- Delta printers: a printer profile's kinematics can be Delta, with a round
  bed of a given diameter around its centre. The 3D and layer views draw a
  round plate and a cylindrical build volume, the terminal UI's layer view
  a circle; jog targets stay on the round bed, the parking point is at its
  back, and the starter macros check the towers instead of the corners.
- `Printer::send_tracked` follows commands until the printer acknowledges
  them, or reports what it answered when it rejects one (`Error:...`,
  "Unknown command"); the settings, temperature, fan, speed, flow and
  babystep controls and `start_sd_print` return such a tracker, and
  `Tracked::all` follows several.

### Fixed

- Messages that a command succeeded wait for the printer: "Saved to
  EEPROM", "Applied", "E-steps applied", "Flow set", "Printing" (an SD
  print) and the like in the GUI, the terminal UI and the shell appear once
  the printer has taken the commands, not as soon as they are queued (a
  printer busy with a heat wait takes them only afterwards), and never when
  it answered one with an error: they say what it answered instead.
- A delta's geometry (M665) is no longer imported into a printer that is not
  a delta, and M665 shows Marlin 2.1's diagonal rod trims (A, B, C).
- A recent file is listed once however its path was spelled, also on Linux
  and macOS (where `..` in a path is not resolved).

## 0.1.0 - 2026-10-19

The first release: a 3D-printer host written in Rust that replaces
Printrun (Pronterface and pronsole), with a desktop GUI, a full-screen
terminal UI, a pronsole-style shell, scriptable command-line commands and a
built-in virtual printer.

### Added

#### Printing and the printer link

- Serial (with baud-rate auto-detection), raw TCP and the built-in virtual
  printer as connection targets; ports are only opened when you connect.
- Line numbers and checksums, resend handling, lost-`ok` recovery, an
  optional window of several commands in flight, temperature and position
  auto-reports (polling as a fallback) and link statistics.
- Host prints with pause, resume and cancel scripts (templated with the
  position, temperatures, park position and `{heaters_off}`), start from
  any line or layer with a generated resume preamble, pause points
  (`;@pause`, `@pause`, pause at a layer) and `@cancel`.
- Recovery after a failed or cancelled print: the job remembers the first
  line the printer did not acknowledge, and a new print can resume there.
- Object exclusion while printing (Marlin `M486`, Klipper
  `EXCLUDE_OBJECT_*`, PrusaSlicer and Cura object labels).
- SD card: list, print, pause, resume, cancel, upload and delete.
- Firmware host prompts (`//action:prompt_*`, M876), filament runout and
  M0/M1/M600 waits for the user.
- Progress on the printer's display (M73) and hook commands run when a print
  starts, finishes, fails or is cancelled.
- Emergency stop (M112) that bypasses the queue, firmware restart (M999)
  and board reset (DTR pulse on serial) after a halt.
- One command-safety classifier for every front end: dangerous commands and
  commands that would disturb a running print ask first; jogging, moves and
  extrusion are allowed while a host print is paused.
- Files that are not G-code (STL, 3MF, OBJ, STEP, binary data) are
  recognised and never streamed.

#### Analysis and viewing

- Fast G-code loading and analysis: layers, features, speeds, widths, tools,
  fan, objects, part size, filament use, and a print-time estimate from a
  kinematic planner (acceleration, junction deviation or jerk, look-ahead).
- Slicer metadata and thumbnails from PrusaSlicer, SuperSlicer, OrcaSlicer,
  Bambu Studio, Cura, Simplify3D, ideaMaker and KISSlicer.
- A GPU toolpath viewer: lit 3D beads or lines, a layer view, color by
  feature, speed, width, layer height, tool, fan, layer or time, layer
  ranges, scrubbing through a layer's moves, print progress with the tool
  head, picking of beads and travel moves.

#### Desktop GUI (`sprint` / `sprint gui`)

- Connection bar with profiles, ports, baud rate, status and an always
  visible E-STOP; controls sidebar for temperatures, jogging, extrusion,
  live tuning (speed, flow, fan, babystepping) and macros.
- Tabs for the 3D and layer view, the G-code text (search, go to line,
  reference tooltips, start from a line), the temperature graph, printer
  information, a catalog-driven settings and EEPROM editor, calibration
  wizards (PID autotune, extruder E-steps, Z offset, flow), the bed mesh
  with a bed-screw helper, and the SD card.
- Console with filters, history and G-code completion; job bar with
  progress, elapsed and remaining time, layer and filament.
- Dark and light themes, drag and drop, keyboard shortcuts, toasts and
  confirmations for risky actions.

#### Terminal

- Full-screen terminal UI (`sprint tui`) with temperatures, position, job
  progress, a Braille layer view that follows the print, the console, a
  G-code line with completion, jogging, dialogs and a command palette.
- Line shell (`sprint shell`) with pronsole's commands, history, completion,
  macros with arguments, profiles, scripts from files, pipes or `-e`.
- Headless commands: `sprint print`, `sprint send`, `sprint analyze`
  (report or JSON), `sprint ports` and `sprint sim`.

#### Virtual printer

- A simulated Marlin 2.1 printer with a thermal model, planner timing, an
  SD card, a bed mesh, PID autotune, host prompts and fault injection
  (corrupted lines, lost replies), built in and served over TCP with
  `sprint sim` for other hosts.

#### Configuration

- One TOML configuration file for every front end (`SPRINT_CONFIG` picks
  another one), with printer profiles, macros, temperature presets and
  scripts; damaged files keep every valid value and are backed up.
