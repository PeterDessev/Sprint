# The shell

```bash
sprint shell                                  # start disconnected
sprint shell --connect sim                    # connected to the virtual printer
sprint shell -c COM3@250000 cube.gcode        # connected, with a file loaded
sprint shell -c sim -e "temp 200" -e status   # run commands, then leave
sprint shell -c COM3 < job.txt                # run a script from a pipe
```

| Option | Meaning |
|---|---|
| `FILE` | G-code file to load at startup |
| `-e`, `--execute CMD` | Run this command instead of reading commands (repeatable, in order), then leave as at the end of a script |
| `-c`, `--connect T` | Printer to connect to at startup (a [target](user-guide.md#connection-targets)) |
| `-p`, `--profile NAME` | Use this printer profile instead of the active one (name, unique prefix or number) |

The shell works like pronsole: type a command, or G-code, at the prompt.
What the printer says is printed above the prompt as it arrives. The
prompt shows the state: `sprint (offline)>`, `sprint (connecting)>`,
`sprint>`, `sprint (halted)>`, or the job and its progress
(`sprint [printing 42.0%]>`).

## Typing

* **G-code is sent as typed**: a line whose first word looks like G-code
  (`G28`, `m105`, `T1`, Klipper's `BED_MESH_CALIBRATE`) goes to the printer.
  Prefix a line with `!` to send it exactly as typed, even when it looks
  like a command (`!status` sends `status`) and without the safety
  questions. A line starting with `;` is a comment.
* Command words ignore case (`Home`, `HOME`). A mistyped command gets a
  suggestion.
* **Tab** completes commands, G-codes, file names, ports and presets; ↑/↓
  walk the history, which is kept in `shell-history.txt` next to the
  configuration file.
* `help` lists the commands, `help <command>` explains one, and
  `help <G-code>` shows the G-code reference (`help M303`).
* **Ctrl+C** stops a wait or `monitor`; at the prompt it only reminds you
  that `exit` leaves and `cancel` stops a print. `exit` (or `quit`, or
  Ctrl+D) leaves; while a job runs it asks first and cancels the print
  (with the cancel script) before disconnecting.
* Typed commands follow the same safety rules as the other front ends (see
  the [user guide](user-guide.md#safety)): dangerous commands and commands
  that would disturb a running print ask `[y/N]` first.

## Commands

### Connection

| Command | Does |
|---|---|
| `connect [target] [baud] [--save]` | Connect to a [target](user-guide.md#connection-targets) (`sim`, `COM3`, `/dev/ttyUSB0@250000`, `tcp://host:port`), or to the profile's printer without one. pronsole's `connect COM3 250000` works too. `--save` makes the target the profile's printer. |
| `profile [name\|number]` (`profiles`) | List the printer profiles, or switch to one (saved as the active profile) |
| `disconnect` | Close the connection |
| `ports` | List the serial ports (they are not opened) |
| `reset` | Reset the board after a halt: on a serial port the port is opened again with a DTR pulse; the virtual printer resets itself; a TCP bridge cannot be reset this way |
| `block_until_online` (`wait_online`) | Wait until the printer is connected (scripts; Ctrl+C stops) |
| `status` | Connection, temperatures, position and the job at a glance |
| `monitor [secs]` | A live status line every `secs` seconds (default 2) until Ctrl+C |

### Files and jobs

| Command | Does |
|---|---|
| `load <file>` (`open`) | Load and analyse a G-code file (refused when it is not G-code) |
| `info` | Statistics of the loaded file |
| `print` | Print the loaded file (with the profile's M73 progress setting) |
| `pause` / `resume` | Pause the job (with the pause script) / resume it; while paused you may jog, move, extrude and type G-code |
| `cancel` | Cancel the job (the cancel script runs) |
| `recover` | After a failed or cancelled print of the loaded file: start it again from the first line the printer did not confirm, with a resume preamble |
| `objects` | List the objects of the loaded file and, during a print, which are excluded and which is printing |
| `exclude <name>` / `include <name>` | Skip an object of the running print / print it again |
| `sd ls` | List the SD card |
| `sd print <name>` | Print a file from the SD card |
| `sd upload [file] [name]` | Write the loaded (or a given) file to the SD card, optionally under another 8.3 name |
| `sd rm <name>` | Delete a file from the SD card (asks first) |
| `eta` | Progress, elapsed and remaining time of the job |
| `source <file>` | Run the commands in a file, one per line, like a script |

### Temperatures and fans

| Command | Does |
|---|---|
| `temp <celsius\|preset\|off> [tool]` (`settemp`) | Set a hotend target (the active tool unless given); `temp pla` uses the PLA preset |
| `bedtemp <celsius\|preset\|off>` (`bed`) | Set the bed target |
| `gettemp` (`temps`) | Ask for the temperatures and show them with their targets |
| `preheat <preset>` | Heat the hotend and bed to a preset (PLA, PETG, ...) |
| `cooldown` | Turn every heater off |
| `fan <0-100\|off>` | Part-cooling fan speed in percent |
| `off` | Turn everything off: heaters, fans and motors (M84) |
| `autotune <hotend\|bed> <celsius> [cycles]` (`pid`) | PID autotune (M303); the firmware applies the result |

### Motion

| Command | Does |
|---|---|
| `home [x] [y] [z] [e]` | Home all axes or the ones given; `e` sets the extruder position to 0 (G92 E0) |
| `tool <n>` | Select the active extruder (T*n*) |
| `move <x\|y\|z\|e> <mm> [feed mm/min]` | Move one axis relative to its position |
| `extrude <mm> [feed mm/min]` | Push filament with the active tool |
| `reverse <mm> [feed mm/min]` (`retract`) | Pull filament back |
| `speed <percent>` | Feedrate override (M220) |
| `flow <percent>` | Flow override of the active tool (M221) |
| `babystep <mm>` | Nudge Z while printing (M290) |

Moves use the profile's jog speeds unless a feedrate is given. Sprint warns
when the axes are not known to be homed.

### Printer settings and reports

| Command | Does |
|---|---|
| `settings [code\|category]` | The firmware settings from M503 as a table (`settings M92`, `settings motion`) |
| `set <code> <params>` | Change a setting, e.g. `set M92 E415`; the other parameters of that setting are kept |
| `eeprom save \| load \| reset` | Store (M500), reload (M501) or factory-reset (M502, asks first; in scripts `eeprom reset yes`) the settings |
| `firmware` | Firmware name, version and capabilities (M115) |
| `endstops` | Endstop and probe states (M119) |
| `mesh [probe]` | The bed mesh as a heatmap; `probe` runs G29 first |
| `stats` | Printer statistics (M78) and link statistics |
| `macro [name [args]]` (`macros`) | List the macros, or run one with arguments |
| `macro define <name> [gcode \| gcode]` | Define a macro (see below) |
| `macro edit <name>` / `macro rm <name>` | Change or delete a macro |

### The shell itself

| Command | Does |
|---|---|
| `help [command\|G-code]` (`?`) | List the commands, or explain one command or G-code |
| `log [on\|off]` | Echo all printer traffic (sent lines, polling) or only replies |
| `exit` (`quit`) | Leave (asks first while a job runs) |

## Macros

Macros are shared with the GUI and the terminal UI (they live in the
configuration file). Define one on a single line, with `|` between G-code
lines, or type its lines after `macro define <name>` and end with a blank
line:

```text
sprint> macro define wipe G1 X{0=5} F3000 | G1 Y{1}
Saved macro wipe (2 lines)
sprint> macro wipe 10 20
sprint> macro define purge
... G1 Z0.3 F600
... G1 X5 Y20 F3000
... G1 Y180 E15 F1200
...
Saved macro purge (3 lines)
```

Arguments work like pronsole's: `{0}`, `{1}`, ... are the arguments in
order, `{1=200}` gives a default, and `{args}` is all of them. A macro
whose arguments are missing is not sent. The [script
placeholders](configuration.md#placeholders) (`{x}`, `{hotend}`,
`{heaters_off}`, ...) are filled in from the printer's state when the macro
runs. Quote a name with spaces: `macro define "level corners" ...`.

## Scripts

Commands can come from a pipe, from `-e` options or from a file run with
`source`. They run in order, without a prompt and without questions: G-code
the script sends on purpose is sent, a command that would disturb a running
print is skipped with a note, and a factory reset needs
`eeprom reset yes`. At the end of the input the shell waits for the queued
commands and for a running print to finish (Ctrl+C cancels the print), then
disconnects; a print the script paused and did not resume is cancelled
(nothing could resume it any more).

```text
; heat, home and print a file, then switch everything off
connect COM3@250000
preheat PLA
home
load cube.gcode
print
```

```bash
sprint shell < job.txt
```

`source` files may run other files, eight deep.

## Coming from pronsole

Most pronsole commands work as they did: `connect`, `load`, `print`,
`pause`, `resume`, `settemp pla`, `bedtemp`, `gettemp`, `home e`, `move`,
`extrude`, `reverse`, `tool`, `off`, `reset`, `monitor`, `eta`, `macro`,
`block_until_online`, `exit`. These differ:

| pronsole | Sprint | Why |
|---|---|---|
| `ls` | `sd ls` | List the SD card |
| `sdprint` | `sd print` | Print a file from the SD card |
| `upload` | `sd upload` | Write a file to the SD card |
| `run_script` | (none) | Sprint does not run other programs; `source <file>` runs shell commands from a file |
| `run_gcode_script` | (none) | Sprint does not run other programs; `source <file>` runs G-code and commands from a file |
| `slice`, `skein` | (none) | Sprint does not slice: slice in your slicer, then load the G-code |
| `loadrc` | (none) | Sprint reads no `.pronsolerc`: profiles, macros and presets live in the configuration file |
| `set port`, `set baudrate` | `connect --save` | `set` changes firmware settings here; `profile` switches printers |
| `!python` | `!G-code` | `!` sends the rest of the line to the printer as typed |
