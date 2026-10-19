# Sprint

A fast, modern 3D-printer host written in Rust: a replacement for
[Printrun](https://github.com/kliment/Printrun) (Pronterface and pronsole)
with a desktop GUI, a full-screen terminal UI, a pronsole-style shell,
scriptable command-line commands and a built-in virtual printer.

![Sprint printing a calibration cube on the virtual printer](docs/images/gui-printing.png)

## Features

* **Loads big files quickly and shows them on the GPU.** Lit 3D beads or
  lines, a layer view, colors by feature, speed, width, layer height, tool,
  fan, layer or time, layer ranges, scrubbing through a layer's moves, and
  live print progress with the tool head. Slicer metadata and thumbnails
  from PrusaSlicer, SuperSlicer, OrcaSlicer, Bambu Studio, Cura and others.
* **Close print-time estimates** from a planner that models acceleration,
  junction deviation or jerk and look-ahead the way the firmware does,
  plus filament use, part size and per-feature statistics.
* **Streams reliably** over USB serial or TCP: line numbers and checksums,
  resends, recovery from lost replies, baud-rate auto-detection, and
  several commands in flight when the firmware keeps up.
* **Pause, resume, cancel and recover.** Templated pause, resume and cancel
  scripts, start from any line or layer with a generated resume preamble,
  recover a failed print from the line where it stopped, pause at a layer,
  skip objects while printing (`M486`, Klipper `EXCLUDE_OBJECT`, slicer
  object labels), SD-card printing and uploads.
* **Shows far more about the printer**: firmware and capabilities, every
  setting from M503 with descriptions, endstops, lifetime statistics,
  thermal stability, link quality, the bed mesh as a heatmap and 3D
  surface.
* **Tunes the printer**: a catalog-driven settings and EEPROM editor,
  calibration wizards for PID autotune, extruder E-steps, Z offset and
  flow, a bed-screw helper, live speed, flow, fan and babystepping.
* **Four front ends, one engine**: the GUI, a terminal UI that runs over
  SSH, a shell with pronsole's commands, and headless `print`, `send` and
  `analyze` commands for scripts. A raw G-code console is everywhere.
* **Safe by default**: an always-visible emergency stop, one set of rules
  for which commands need a confirmation, files that are not G-code are
  never streamed, and Sprint never connects to a printer you did not
  choose.
* **A virtual printer** (a simulated Marlin 2.1 board with heaters, a
  planner, an SD card, a bed mesh, host prompts and fault injection) to try
  everything without hardware, also served over TCP for other hosts.

| | |
|---|---|
| ![Start-print dialog with the resume preamble](docs/images/gui-start-print.png) | ![Printer information](docs/images/gui-printer.png) |
| Start from a layer or line, with a resume preamble you can check and edit | Firmware, thermal health, link quality, capabilities and settings |
| ![Bed mesh and bed-screw helper](docs/images/gui-bed-mesh.png) | ![Settings editor](docs/images/gui-tuning.png) |
| Bed mesh heatmap, 3D surface and how far to turn each screw | Every firmware setting, edited and saved to EEPROM |
| ![Calibration wizard](docs/images/gui-calibration.png) | ![Terminal UI](docs/images/tui-printing.png) |
| Step-by-step calibration wizards | The terminal UI, following a print |

## Quick start

Sprint builds with the stable Rust toolchain (1.95 or newer):

```bash
cargo build --release
```

The binary is `target/release/sprint` (`sprint.exe` on Windows). Put it on
your `PATH`, or run the commands below as `cargo run --release -- <args>`.

Open the GUI, optionally with a file:

```bash
sprint
sprint samples/calibration-cube.gcode
```

Try everything on the built-in virtual printer:

```bash
sprint --sim                     # the GUI, connected to the virtual printer
sprint gui --sim samples/calibration-cube.gcode
sprint tui --connect sim         # the full-screen terminal UI
sprint shell --connect sim       # the pronsole-style shell (type help)
sprint analyze samples/calibration-cube.gcode
sprint print samples/calibration-cube.gcode --connect sim
```

The virtual printer runs in real time, so the last command takes as long as
the real print (about 18 minutes). Its options make it faster:

```bash
sprint print samples/calibration-cube.gcode --connect "sim://?time_scale=50&instant_moves=true"
```

`samples/` holds three generated files to play with: a PrusaSlicer-style
calibration cube, a Cura-style vase and an arcs test (see
[samples/README.md](samples/README.md)).

## Connecting a real printer

List the serial ports (they are only enumerated, never opened):

```bash
sprint ports
```

Then connect with a *target*: a port with an optional baud rate, a TCP
address, or `sim`:

| Target | Meaning |
|---|---|
| `COM3` | Serial port at 115200 baud |
| `COM3@250000`, `/dev/ttyUSB0@250000` | Serial port at that baud rate |
| `COM3@auto` | Try the common baud rates until the printer answers |
| `tcp://192.168.1.50:23` | A serial-to-network bridge (ESP3D, ser2net, ...) |
| `sim` | The built-in virtual printer |

```bash
sprint tui --connect COM3@auto
sprint shell --connect /dev/ttyUSB0@115200
```

In the GUI, pick the port and baud rate in the connection bar and press
**Connect**; **Tools > Printer profiles** stores the port, bed size, scripts
and options per printer. The [user guide](docs/user-guide.md) covers
profiles, the configuration file and troubleshooting.

## Safety

* **Emergency stop.** The red **E-STOP** button in the GUI (always visible,
  even above dialogs), F8 pressed twice in the terminal UI, or `M112` typed
  anywhere sends M112 at once, ahead of everything queued. The firmware
  halts and turns everything off. To go on, reset the board (Sprint offers
  it after a halt; a serial board resets through DTR) or power-cycle it and
  reconnect: Marlin does not accept `M999` after M112.
* **Confirmations.** Every front end applies the same rules: dangerous
  commands (factory reset, firmware restart, ...) ask before they are sent,
  and commands that would ruin a running print (homing, moves, mode
  changes, ...) ask or are refused while it prints. Jogging and extrusion
  work while a host print is paused, since resuming restores the position
  and modes.
* **No surprise connections.** Sprint opens a port only when you connect,
  or at startup when you turn on *Connect when Sprint starts*
  (`auto_connect`) for a profile (off by default). Opening a serial port
  resets many boards, which stops a print running from the printer's own
  SD card.
* **Unattended failures.** When a host print fails while the printer still
  answers, Sprint turns the heaters and the part fan off (configurable per
  profile).
* **Not G-code, not printed.** STL, 3MF, OBJ and STEP models and binary
  files are recognised and refused with an explanation.

## Documentation

* [User guide](docs/user-guide.md): the GUI, the terminal UI, the shell, the
  command line, configuration, the virtual printer and troubleshooting.
* [Sprint and Printrun](docs/printrun-comparison.md): a feature-by-feature
  comparison, including what Sprint does not do yet.
* [Architecture](ARCHITECTURE.md): the crates and the contract between
  them.
* [Changelog](CHANGELOG.md).

## Development

The workspace has one crate per layer: `sprint-gcode` (parsing, analysis,
estimation), `sprint-proto` (the firmware protocol), `sprint-sim` (the
virtual printer), `sprint-host` (the engine), `sprint-viewer` (the GPU
viewer), `sprint-gui`, `sprint-tui` and the `sprint` binary.
[ARCHITECTURE.md](ARCHITECTURE.md) describes each of them.

```bash
cargo test --workspace
```

Tests, examples and demos use the virtual printer or TCP loopback to it and
never open a serial port.

## License

Sprint is licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
* MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in Sprint by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
