# Sprint user guide

Sprint is a 3D-printer host: it loads G-code, shows it, streams it to a
printer over USB serial or TCP, and gives you the printer's controls,
settings and calibration tools. One engine drives four front ends:

| Front end | Start it with | Use it for |
|---|---|---|
| [Desktop GUI](gui.md) | `sprint` or `sprint gui` | Everyday printing, viewing files, tuning and calibration |
| [Terminal UI](tui.md) | `sprint tui` | A full-screen dashboard in a terminal, also over SSH |
| [Shell](shell.md) | `sprint shell` | Commands and G-code, typed or scripted |
| [Command line](cli.md) | `sprint print`, `send`, `analyze`, `ports`, `sim` | One-shot jobs and scripts |

The other pages cover what they share:

* [Configuration](configuration.md): the configuration file, printer
  profiles, pause/resume/cancel scripts and their placeholders, macros,
  hook commands and progress on the printer's display.
* [The virtual printer](virtual-printer.md): the built-in simulated Marlin
  printer and its fault injection.
* [Troubleshooting](troubleshooting.md): connection problems, resends,
  timeouts, halts and board resets, Windows consoles.

## Installing

Build Sprint from source with the stable Rust toolchain (1.95 or newer):

```bash
cargo build --release
```

This produces one program, `target/release/sprint` (`sprint.exe` on
Windows), that contains every front end. Copy it anywhere on your `PATH`.
Sprint keeps no other files next to it; its configuration lives in your
user profile (see [Configuration](configuration.md)).

On Windows, double-clicking `sprint.exe` opens the GUI and closes the
console window Windows opens for it; started from a terminal, Sprint keeps
the terminal for its messages.

## First steps without a printer

The built-in virtual printer behaves like a Marlin 2.1 board: it heats up,
moves, reports, has an SD card and a bed mesh, and can be told to lose or
corrupt lines. Everything in this guide works on it.

```bash
sprint --sim samples/calibration-cube.gcode   # GUI, connected, with a file
sprint tui --connect sim                       # terminal UI
sprint shell --connect sim                     # shell; type help
```

In the GUI, press **Start print** (Ctrl+P), then try pausing, the live
tuning sliders and the other tabs. The virtual printer runs in real time;
`sim://?time_scale=10` makes it ten times faster (see
[The virtual printer](virtual-printer.md)).

## Connection targets

Wherever Sprint asks where the printer is (`--connect`, the shell's
`connect`, a profile's connection) it takes a *target*:

| Target | Meaning |
|---|---|
| `COM3`, `/dev/ttyUSB0`, `/dev/ttyACM0` | A serial port at 115200 baud |
| `COM3@250000` | A serial port at a given baud rate |
| `COM3@auto` (or `@0`) | Try 250000, 115200, 230400, 500000, 1000000, 57600, 38400, 19200 and 9600 baud until the printer answers |
| `tcp://host:port`, `tcp://[::1]:23` | A raw TCP socket: a serial-to-network bridge, or `sprint sim` |
| `sim` | The built-in virtual printer |
| `sim://?time_scale=10&extruders=2` | The virtual printer with options |

`sprint ports` lists the serial ports and marks the ones whose USB chip is
common on printer boards (CH340, FTDI, CP210x, Arduino, Prusa, STM32, ...).
Ports are only listed, never opened, until you connect.

`sprint print` and `sprint send` also accept `profile` as the target: the
printer stored in the active (or `--profile`) printer profile.

## Printer profiles

A profile holds everything Sprint knows about one printer: where it is
connected, the protocol options, bed size and origin, extruders and heaters,
temperature presets, jog speeds, the pause, resume and cancel scripts, hook
commands and motion limits for the print-time estimate. Edit profiles in
the GUI (**Tools > Printer profiles**), list and switch them with the
shell's `profile` command, or pick one for a single run with
`--profile NAME` (`tui`, `shell`, `print`, `send`). See
[Configuration](configuration.md).

## Safety

* **Emergency stop**: the red **E-STOP** button in the GUI, F8 twice in the
  terminal UI, or `M112` typed in any console. M112 is sent at once, ahead
  of everything queued, and is never asked about. The firmware halts and
  switches heaters and motors off; Sprint then offers to **reset the
  board** (see [Troubleshooting](troubleshooting.md#halts-and-board-resets)).
* **Confirmations**: every front end decides with the same rules which
  typed commands, macros and settings edits need a question:
  * always, for dangerous commands: `M502` (factory reset), `M999`
    (restart after a stop), `M997` (firmware update), `M994` (restore the
    settings flash), `M30` (delete an SD file), `M42` (switch a pin), `M81`
    (power off), and Klipper's `FIRMWARE_RESTART`, `RESTART`,
    `SAVE_CONFIG`, `SET_KINEMATIC_POSITION` and `FORCE_MOVE`;
  * while a print runs (streamed by Sprint or from the printer's SD card)
    or an SD upload, for anything that would ruin it: moves, homing and
    probing, mode changes (`G90`/`G91`, `M82`/`M83`, units),
    coordinate shifts, tool changes, motors off, turning a heater off,
    temperature waits, calibration and leveling changes; starting another
    SD print or writing to the card is refused. Temperatures, fans, speed
    and flow overrides, babystepping and object exclusion are allowed;
  * while a host print is paused, moves, extrusion, mode changes and
    heaters are allowed (resuming restores the position, the modes and the
    temperatures); homing Z, probing, tool changes and the like still ask.
* **Only the printer you chose**: nothing connects unless you connect, or
  a profile has *Connect when Sprint starts* (`auto_connect`) turned on;
  it is off by default. Opening a serial port resets many boards, which
  stops a print running from the printer's own SD card.
* **Only G-code is printed**: 3D models (STL, 3MF, OBJ, STEP), binary
  G-code and other binary files are recognised by their content and refused
  with an explanation.
* **Failures turn the heat off**: when a host print fails while the printer
  still answers, Sprint switches every heater and the part fan off (unless
  the profile's `keep_heaters_on_failure` is set).

## Files Sprint reads

Sprint reads plain-text G-code from any slicer (`.gcode`, `.gco`, `.g`,
`.gc`, `.nc`, `.ngc`; the extension does not matter, the content does).
It understands slicer comments for features (`;TYPE:`), layers
(`;LAYER_CHANGE`, `;LAYER:`), widths and heights, objects (`M486`, Klipper
`EXCLUDE_OBJECT_*`, PrusaSlicer `; printing object`, Cura `;MESH:`),
thumbnails and the slicer's own estimates, and the host commands
`;@pause`, `@pause`, `@cancel` and `@abort` in a file (the stream pauses
or cancels there; other `@` lines are reported and not sent).

The print-time estimate uses the profile's custom motion limits when it
has them; otherwise the GUI uses the limits the connected printer reported
(M503) and the other front ends typical Marlin defaults. M201, M203, M204
and M205 lines in the file override them from where they appear.
