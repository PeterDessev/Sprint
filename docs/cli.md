# The command line

```text
sprint [FILE] [--sim]                 the GUI (the default)
sprint gui [FILE] [--sim] [--connect T]
sprint tui [FILE] [--connect T] [--profile NAME]
sprint shell [FILE] [-e CMD]... [--connect T] [--profile NAME]
sprint print FILE --connect T [options]
sprint send --connect T [--profile NAME] [--timeout SECS] CMD...
sprint analyze FILE [--json] [--filament-diameter MM]
sprint ports
sprint sim [options]
sprint help [COMMAND]
```

`sprint --help` and `sprint <command> --help` print the options;
`sprint --version` prints the version.

`T` is a [connection target](user-guide.md#connection-targets): `COM3`,
`COM3@250000`, `COM3@auto`, `/dev/ttyUSB0@115200`, `tcp://host:port`,
`sim` or `sim://?time_scale=50&instant_moves=true`. `print` and `send`
also take `profile`, the printer stored in the printer profile (named on
purpose, so that no serial port is opened by surprise). Nothing connects
to a printer unless a command is given a target, or the GUI, the terminal
UI or the shell starts with a profile that has `auto_connect` turned on.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | The command failed; the reason is printed on stderr as `sprint: error: ...` (in a message box for a GUI started from Explorer) |
| 2 | Usage error: an unknown option, a missing argument or a value out of range |
| 130 | Interrupted with Ctrl+C (`print` cancels the job first) |

## Environment

| Variable | Effect |
|---|---|
| `SPRINT_CONFIG` | The configuration file to use instead of the default one, for every front end, the GUI included (see [Configuration](configuration.md)). Blank counts as unset. |
| `RUST_LOG` | Log filter for messages on stderr, for example `RUST_LOG=debug` or `RUST_LOG=sprint_host=trace`. Default `warn`; `sim` also shows client connections; the terminal UI logs nothing. |
| `SPRINT_EDITOR`, `VISUAL`, `EDITOR` | The external editor of the GUI's **Edit in external editor** |
| `NO_COLOR`, `TERM`, `COLORTERM` | Colors of the terminal UI, the shell and the reports |
| `WGPU_BACKEND` | Restricts the GPU backends the GUI may use (`dx12`, `vulkan`, `metal`, `gl`) |

## sprint gui

Opens the desktop GUI, also when no command is given.

| Option | Meaning |
|---|---|
| `FILE` | G-code file to open |
| `--sim` | Connect to the built-in virtual printer at startup |
| `--connect T` | Connect to this target at startup |

See [The desktop GUI](gui.md).

## sprint tui

The full-screen terminal UI; see [The terminal UI](tui.md).

| Option | Meaning |
|---|---|
| `FILE` | G-code file to open |
| `-c`, `--connect T` | Printer to connect to at startup |
| `-p`, `--profile NAME` | Printer profile to use instead of the active one (its name, a unique prefix of it, or its number in the shell's profile list) |

## sprint shell

The pronsole-style shell; see [The shell](shell.md).

| Option | Meaning |
|---|---|
| `FILE` | G-code file to load at startup |
| `-e`, `--execute CMD` | Run this command (repeatable, in order) instead of reading commands, then leave as at the end of a script |
| `-c`, `--connect T` | Printer to connect to at startup |
| `-p`, `--profile NAME` | Printer profile to use instead of the active one |

Commands come from the terminal, from a pipe (`sprint shell -c T <
script.txt`) or from `-e`. Piped and `-e` commands run in order without a
prompt or questions (a factory reset then needs `eeprom reset yes`), and
at the end the shell waits for the queued commands and for a running
print to finish (Ctrl+C cancels the print) before it disconnects; a print
the script paused and did not resume is cancelled.

```bash
sprint shell -c sim -e "preheat PLA" -e "load cube.gcode" -e print
```

## sprint print

Prints a file without a UI, showing progress on stderr.

```bash
sprint print cube.gcode --connect COM3@250000
sprint print cube.gcode --connect profile --profile "Ender 3"
sprint print cube.gcode -c "sim://?time_scale=50&instant_moves=true"
```

| Option | Meaning |
|---|---|
| `FILE` | G-code file to print |
| `-c`, `--connect T` | Printer to print on, or `profile` for the profile's printer (required) |
| `-p`, `--profile NAME` | Printer profile to use instead of the active one: its scripts, options and, with `--connect profile`, its printer |
| `--start-line N` | Resume an interrupted print from line N of the file (1 = the first line): heat, home X and Y, and move back to where line N starts |
| `--board-reset` | With `--start-line`: the board was reset (a power loss, or a reconnect that reset an 8-bit board) while the nozzle stayed where the print stopped, so its Z is declared with G92 Z before lifting. Without it the firmware's Z is trusted, which after a reset is 0 and would print the rest in the air. |
| `--z-clearance MM` | With `--start-line`: how far to lift the nozzle above the part before moving to the resume position (0.5 to 50, default 2) |
| `--no-wait-temps` | With `--start-line`: set the temperatures in effect at line N but do not wait for them (for heaters that are already hot) |
| `--pause-at-layer L` | Pause (with the pause script) when the print reaches layer L |
| `--progress-on-printer` | Show the progress on the printer's display (M73), whatever the profile says |
| `--no-progress` | Do not show the progress line on stderr |
| `--answer-prompts HOW` | What to do when the printer asks something during the print: `ask` (default) or `continue` (see below) |

Sprint loads and analyses the file (so progress, remaining time and layers
are known), connects, waits for the printer's setup and starts the job. The
progress line (status, percent, layer, remaining time, temperatures, Z) is
redrawn in place on a terminal and printed every 10% otherwise. At the end
`Completed <file>: 100.0% in <time>` goes to stdout.

**Ctrl+C** cancels the job (the cancel script runs) and exits with 130; a
second Ctrl+C stops waiting for the cancel, but the heaters are still
turned off before the link closes.

**Failures** exit with 1 and say why. When a print fails while the printer
still answers, Sprint turns the heaters off before disconnecting. When a
host print fails or is cancelled, Sprint prints the line to resume from:

```text
resume with: sprint print cube.gcode --start-line 4721
```

Add `--board-reset` when the board was reset or lost power in between.

**The printer's questions** (a filament change, an M0 "insert magnets", a
runout) are printed as they come. With `--answer-prompts ask` the answer is
a line on standard input: the number of a button, or an empty line for the
first one; when standard input has ended, the question can still be
answered at the printer. On a terminal, lines typed before a question are
ignored, so a stray Enter cannot answer it; piped answers are used in
order. With `--answer-prompts continue` Sprint presses the button that
lets the print go on (Continue) and releases a firmware waiting for the
user (M108) at once; a pause made at the printer (its menu, or a pause
without a question) is left to the person there.

## sprint send

Sends commands and prints the replies; each command waits for its `ok`.

```bash
sprint send --connect COM3 M115
sprint send -c sim G28 "G1 X10 Y10 F3000" M114
sprint send -c profile --timeout 120 "M109 S200"
```

| Option | Meaning |
|---|---|
| `CMD...` | Commands to send, in order; each argument is one command (quote commands with parameters) |
| `-c`, `--connect T` | Printer to send to, or `profile` (required) |
| `-p`, `--profile NAME` | Printer profile to use instead of the active one |
| `--timeout SECS` | How long the printer may stay silent about a command before Sprint gives up (default 30). Busy messages (homing, probing) and, while M109, M190 or M191 waits for a temperature, temperature reports restart the wait. |

Exit status: 0 when every command was acknowledged; 1 after a timeout, a
lost connection, an error reply, or a command the firmware does not know
(`Unknown command`), so scripts notice typos; 130 on Ctrl+C.

## sprint analyze

Analyses a G-code file and prints its statistics; it needs no printer.

```bash
sprint analyze cube.gcode
sprint analyze cube.gcode --json > cube.json
```

| Option | Meaning |
|---|---|
| `FILE` | G-code file to analyse |
| `--json` | Print JSON instead of a report |
| `--filament-diameter MM` | Filament diameter used when the file does not state one (0.1 to 10, default 1.75) |

The report gives the file size and line counts, the slicer, the print-time
estimate next to the slicer's, filament length, volume and weight, the
material, the part size and bounds, layers and layer height, print and
travel speeds, moves and retractions, tools, temperatures, thumbnails and
a table of the time and filament per feature. A progress line is drawn on
stderr while a big file is analysed (only on a terminal).

The JSON has the keys `file` (`name`, `path`, `bytes`, `lines`,
`commands`), `stats`, `metadata` (thumbnails without their image data),
`bounds` (the extent of every extrusion, purge lines included),
`travel_bounds`, `part_bounds` (the printed part) and `layers` (`z`,
`height`, `time_start`, `duration`, `filament_mm`, `first_line`). Empty
bounds and numbers that are not finite are `null`. Times are in seconds,
lengths in millimetres.

## sprint ports

Lists the serial ports with their description and USB IDs, marks likely
printers, and suggests a `--connect` value. Ports are only enumerated,
never opened.

```text
PORT   DESCRIPTION                          VID:PID
COM10  Silicon Labs CP210x USB to UART ...  10C4:EA60  ● likely printer
COM1   Serial port                          --

Connect with: sprint tui --connect COM10@115200 (or @250000, @auto)
```

## sprint sim

Runs the virtual printer as a TCP server, for Sprint itself or for other
hosts (Pronterface, OctoPrint, ...). Each client gets a freshly booted
printer; one client at a time. It serves until Ctrl+C, and exits with an
error if the listener fails.

```bash
sprint sim                                   # 127.0.0.1:5000, real time
sprint sim --tcp 127.0.0.1:0 --time-scale 20 # any free port, 20x faster
sprint tui --connect tcp://127.0.0.1:5000    # in another terminal
```

| Option | Meaning |
|---|---|
| `--tcp ADDR` | Address to listen on; port 0 picks a free port (default `127.0.0.1:5000`). `0.0.0.0` lets other machines connect, without a password. |
| `--time-scale X` | Simulation speed: 1 is real time, 10 is ten times faster (0.01 to 10000, default 1) |
| `--extruders N` | Number of extruders, 1 to 8 (default 1) |
| `--checksum-error-rate R` | Probability (0 to 1) that a numbered line is rejected as corrupted, to exercise resends |
| `--drop-ok-rate R` | Probability (0 to 1) that an `ok` is lost, to exercise timeouts |
| `--no-heated-bed` | Simulate a printer without a heated bed |
| `--heated-chamber` | Simulate a heated chamber |
| `--instant-moves` | Moves take no time |
| `--advanced-ok` | Answer `ok N.. P.. B..` (ADVANCED_OK) |
| `--boot-delay-ms MS` | Delay before the boot banner, scaled by the time scale (default 300) |
| `--seed N` | Seed for fault injection and sensor noise |
| `--name NAME` | Machine name reported by M115 |

The first line on stdout is `Virtual printer listening on <address>` (the
address is its last word, handy with port 0), followed by connection
hints. See [The virtual printer](virtual-printer.md).

## Scripting examples

```bash
# Preheat, wait, then report the temperatures.
sprint send -c COM3 "M140 S60" "M190 S60" "M104 S205" "M109 S205" M105

# Print several files one after the other, stopping at the first failure.
for f in part1.gcode part2.gcode; do
  sprint print "$f" -c COM3@250000 --no-progress || break
done

# Estimate a folder of files.
for f in *.gcode; do
  sprint analyze "$f" --json | jq -r '"\(.file.name)\t\(.stats.estimated_time)"'
done

# A test setup that never touches your configuration.
SPRINT_CONFIG=/tmp/sprint-test.toml sprint shell -c sim -e status
```
