# Sprint and Printrun

[Printrun](https://github.com/kliment/Printrun) (Pronterface, pronsole and
printcore) is the classic open-source printer host, and Sprint is meant to
replace it. This page compares the two feature by feature, including what
Sprint does not do yet. "Printrun" below means Printrun 2.x as commonly
used; check its documentation for details of a particular version.

## At a glance

| | Printrun | Sprint |
|---|---|---|
| Language, toolkit | Python, wxPython, OpenGL | Rust, egui with wgpu (GPU), ratatui |
| Front ends | Pronterface (GUI), pronsole (shell), printcore (library) | GUI, terminal UI, shell, headless `print` / `send` / `analyze` commands, and the `sprint-host` library |
| Install | Python and its packages, or a bundled build | One self-contained program |
| Configuration | `.pronsolerc` (`set` commands, macros) | One TOML file with printer profiles (`SPRINT_CONFIG` picks another) |
| Virtual printer | None built in | A simulated Marlin 2.1 printer with fault injection, also served over TCP |

## Connecting and streaming

| Feature | Printrun | Sprint |
|---|---|---|
| Serial ports | Yes | Yes, with baud-rate auto-detection and a list that marks likely printers |
| TCP (network bridges) | Yes | Yes (`tcp://host:port`) |
| Line numbers and checksums, resends | Yes | Yes, configurable (always, while printing, never) |
| Recovery from a lost `ok` | Basic | Timeouts aware of long commands (homing, heating), busy messages, link statistics |
| Several commands in flight | No | Optional (`max_in_flight`) |
| Temperature and position reports | Polled (M105) | M155/M154 auto-reports when the firmware has them, polling otherwise |
| Emergency stop and recovery | Reset button, M112 from the console | Always-visible E-STOP sent ahead of the queue; board reset (DTR) or M999 afterwards, depending on the halt |
| Profiles for several printers | One configuration per `.pronsolerc` | Any number of printer profiles, switched in every front end |

## Printing

| Feature | Printrun | Sprint |
|---|---|---|
| Print, pause, resume, cancel | Yes | Yes, with templated pause, resume and cancel scripts (park, prime, `{heaters_off}`) |
| Start from a line or layer | Partly | Start dialog with layer or line and a generated resume preamble (temperatures, modes, safe homing, Z clearance, board-reset mode) |
| Recover a failed print | Partly | Remembers the first line the printer did not confirm and offers to resume there with a preamble |
| Pause at a layer, `;@pause` | `;@pause` in the file | `;@pause`, `@pause`, `@cancel`, and pause at a chosen layer |
| Skip objects while printing | The Excluder (draw rectangles) | Object labels from the file (`M486`, Klipper `EXCLUDE_OBJECT`, PrusaSlicer, Cura), handled by the host on any firmware |
| Progress on the printer's display | Yes ("Display progress on printer") | Yes (M73), per profile and per print |
| Start, finish and failure commands | Yes | Yes (`on_start`, `on_finish`, `on_fail`, `on_cancel`), with the file, status, elapsed time, printer and reason in environment variables |
| SD card: list, print, upload, delete | Yes | Yes, with progress for prints and uploads |
| Host prompts (M876), filament change, M0 | Limited | Questions shown with their buttons in every front end; `sprint print --answer-prompts` |
| Refuse to print non-G-code files | No | STL, 3MF, OBJ, STEP and binary files are recognised and refused |
| Safety rules for typed commands | No | One classifier for every front end: dangerous commands and commands that would ruin a running print ask first |
| Heaters off when a print fails | No | Yes (unless the profile keeps them on) |
| Headless printing from scripts | `pronsole -e` | `sprint print` with progress and exit codes, `sprint shell -e`, `sprint send` |

## Viewing and analysis

| Feature | Printrun | Sprint |
|---|---|---|
| 2D layer view | Yes | Yes (an orthographic layer view in the GUI, a Braille layer view in the terminal) |
| 3D view | Yes | GPU-rendered lit beads or lines, millions of segments, camera presets |
| Color modes | Few | Feature, speed, width, layer height, tool, fan, layer, time |
| Print progress in the view | Partly | Printed part solid, the rest ghosted, tool head, follow the print |
| Scrub through a layer, pick a move | No | Yes, linked to the G-code text |
| Print-time estimate | A simple acceleration model | A planner with acceleration, junction deviation or jerk and look-ahead, using the firmware's limits |
| Slicer metadata and thumbnails | Limited | PrusaSlicer, SuperSlicer, OrcaSlicer, Bambu Studio, Cura, Simplify3D, ideaMaker, KISSlicer |
| Statistics | Basic | Per-feature time and filament, speeds, part size, objects, `sprint analyze --json` |
| G-code text with reference | No | Syntax colors, a G-code reference on hover and in the shell's `help` |

## The printer

| Feature | Printrun | Sprint |
|---|---|---|
| Temperature controls and graph | Yes | Yes, with heater power, statistics and CSV export |
| Jogging, homing, extrusion | Yes | Yes, plus filament load/unload and keyboard jogging |
| Speed, flow, fan overrides, babystepping | Partly | Yes, live during prints |
| Firmware info and capabilities | Console only | Firmware, capabilities with descriptions, statistics, endstops, link quality and thermal health |
| Settings and EEPROM | Console only | A settings editor for every M503 setting with descriptions, units and ranges, apply, save, load, factory reset, export and import |
| Calibration | No | PID autotune, extruder E-steps, Z offset and flow wizards |
| Bed mesh | No | Heatmap, 3D surface, leveling on/off and fade height, bed-screw helper |
| Macros | G-code or Python | G-code with placeholders and arguments (`{0}`, `{1=200}`, `{args}`), shared by every front end |

## What Sprint does not do (yet)

* **Slicing integration.** Printrun can call a slicer and has a plater for
  arranging STL models. Sprint does not slice: slice in your slicer and
  open the G-code.
* **Spool manager.** Printrun tracks filament spools and how much is left;
  Sprint shows the filament a print uses but keeps no spool inventory.
* **Localization.** Printrun is translated into many languages; Sprint is
  English only.
* **RPC server.** Printrun can expose its status over XML-RPC for remote
  monitoring. Sprint has no network API; hook commands can report job
  events to other programs.
* **Printrun configuration import.** Sprint does not read `.pronsolerc`;
  set up printer profiles and macros in Sprint (the shell's
  `connect --save` and `macro define` help).
* **Round beds.** The viewer draws rectangular beds only; a bed-centre
  origin for deltas is supported, but the bed outline stays square.
* **Persistent communication log.** The console lives in memory (5000
  lines by default); export it from the GUI's console when you need it.
  Printrun can write its log to a file continuously.
* **Python macros and plugins.** Sprint's macros are G-code templates; it
  does not run scripts or other programs, except the hook commands.

## pronsole commands

The shell accepts most pronsole commands as they are; the
[shell page](shell.md#coming-from-pronsole) lists the ones that differ
(`ls`, `sdprint` and `upload` are `sd ls`, `sd print` and `sd upload`;
`set port` and `set baudrate` are `connect --save`; `run_script`, `slice`
and `loadrc` have no direct equivalent).
