# The terminal UI

```bash
sprint tui                                   # start disconnected
sprint tui samples/calibration-cube.gcode    # with a file
sprint tui --connect sim                     # connected to the virtual printer
sprint tui --connect COM3@250000             # connected to a printer
sprint tui --profile "Ender 3"               # with another printer profile
```

| Option | Meaning |
|---|---|
| `FILE` | G-code file to load at startup |
| `-c`, `--connect T` | Printer to connect to at startup (a [target](user-guide.md#connection-targets)) |
| `-p`, `--profile NAME` | Use this printer profile instead of the active one: its name, a unique prefix of it, or its number in the shell's `profile` list. The configuration file is not changed. |

Without `--connect`, the terminal UI connects at startup only when the
profile has `auto_connect` turned on.

![The terminal UI following a print](images/tui-printing.png)

## Layout

* **Header**: the connection status, target and firmware; on the right the
  printer's state (*HALTED*, *Waits for the user*, busy messages), a
  notice when a question from the printer is hidden, and the job's status,
  progress, layer and remaining time.
* **Left column**: heater gauges with power, a temperature chart of the
  last ten minutes, the position, the job (file, status, progress bar,
  elapsed, remaining, layer), and fan, speed and flow.
* **Layer view**: the current layer drawn with Braille characters,
  following the print (printed moves bright, the rest dimmed, the tool
  head marked), with a feature legend. `s` swaps it for the file's
  statistics.
* **Console**: the printer traffic. Automatic traffic (temperature
  polling and reports) and the lines a print streams are hidden by default.
* **Input line**: G-code or commands, with completion.
* **Key bar**: the keys that work right now.

The layout adapts to the terminal size, from 80 x 24 up. Colors follow the
terminal: 24-bit color on Windows consoles and in terminals that set
`COLORTERM` (or are known to support it), 256 colors where `TERM` ends in
`256color`, 16 colors otherwise, and no colors (only bold, dim and
reverse) with `NO_COLOR` set or `TERM=dumb`.

## Keys

Press F1 or `?` for the same list inside the app.

**General**

| Key | Action |
|---|---|
| F1 / `?` | Help |
| F2 | Connect: virtual printer, serial port or TCP |
| F3 | Open a G-code file |
| Tab / Shift+Tab | Move between the layer view, the console and the input line |
| `:` | Command palette (the shell's commands) |
| `i` / Enter | Type G-code (typing a capital letter, as in `G28`, starts it too) |
| F10 / Ctrl+Q / `q` | Quit (asks while printing) |

**Printing**

| Key | Action |
|---|---|
| F5 | Print the loaded file |
| F6 | Pause / resume; continue when the printer waits for the user |
| F7 | Cancel the job (print, SD print or upload) |
| F8 F8 | Emergency stop (M112): press twice; any other key in between disarms it |
| `j` | Jog mode (see below) |
| `t` | Temperatures and presets |
| `p` | Show the printer's question again (after Esc hid it) |

**Layer view**

| Key | Action |
|---|---|
| ← → / `[` `]` (↑ ↓ too) | Previous / next layer |
| PgUp / PgDn | Ten layers up / down |
| Home / End | First / last layer |
| `f` | Follow the printing layer |
| `b` | Fit to the bed or to the object |
| `v` | Show travel moves |
| `s` | Statistics of the file (↑ ↓ PgUp PgDn Home scroll them) |

**Console**

| Key | Action |
|---|---|
| ↑ ↓ PgUp PgDn, mouse wheel | Scroll |
| Home / End | Oldest line / follow new output |
| `a` | Show automatic traffic (temperature polling) |
| `/` | Search; `n` clears it |
| `c` | Clear the console |

**Input line**

| Key | Action |
|---|---|
| Enter | Send / run |
| Tab | Complete a G-code or command |
| ↑ ↓ | History |
| Ctrl+A / Ctrl+E | Start / end of the line |
| Ctrl+W / Ctrl+U / Ctrl+K | Delete a word / to the start / to the end |
| Ctrl+C | Clear the line (on an empty line: quit) |
| Esc | Back to the panels |

**Jog mode** (`j`; not while a print runs, but while a host print is
paused)

| Key | Action |
|---|---|
| ← → | X − / + |
| ↑ ↓ | Y + / − |
| PgUp / PgDn | Z + / − (at most 10 mm per step) |
| `+` / `-` | Step: 0.1, 1, 10, 50 or 100 mm (10 mm to start) |
| `h` | Home all axes |
| `x` / `y` / `z` | Home one axis |
| Esc / `j` / `q` | Leave jog mode |

Jogging uses the profile's jog speeds.

## Dialogs

* **Connect** (F2): the virtual printer, the serial ports found (likely
  printers marked) and a network address. ←/→ picks the baud rate (Auto
  first), F2 scans the ports again, Enter connects. Connecting or
  disconnecting while a job runs asks first.
* **Open** (F3): type a path (Tab completes, G-code files first) or pick a
  match or a recent file.
* **Temperatures** (`t`): a target per heater, the profile's presets and
  *All off*. Turning a heater off during a print asks first.
* **The printer's questions**: a filament change, an M0 "insert magnets"
  or a runout shows the question with its buttons (←/→, Enter); Esc hides
  it, `p` shows it again. A wait for the user without a question is
  continued with F6.
* **Confirmations** for anything risky, using the same rules as the other
  front ends (see the [user guide](user-guide.md#safety)).
* **Reports** from palette commands (`:settings`, `:firmware`, `:mesh`,
  `:stats`, ...) open in a scrollable window.

## The command palette

`:` opens a command line for the [shell's commands](shell.md): `:load
cube.gcode`, `:connect COM3 250000`, `:preheat PLA`, `:macro purge`,
`:sd ls`, `:settings M92`, `:mesh`, `:profile`, `:help home`, ... Tab
completes command names (and G-codes).

The palette also has the recovery commands: `:reset` resets the board
after a halt, `:recover` starts a print again from where a failed or
cancelled one stopped, and `:objects`, `:exclude NAME` and
`:include NAME` list and skip the objects of the running print.

## Halts

After an emergency stop or a firmware error the header shows *HALTED* and
the console says what happened. Recover with `:reset` (a board reset: DTR
pulse on a serial port, a reset of the virtual printer), or reconnect
(F2); see [Troubleshooting](troubleshooting.md#halts-and-board-resets).

## Notes

* The terminal UI writes no log lines to the screen (`RUST_LOG` is off for
  it); everything Sprint has to say goes to the console panel.
* Pasting works (bracketed paste): one command goes into the input line;
  several lines of G-code are shown and sent line by line after you
  confirm.
* The configuration, profiles, macros and recent files are the same as in
  the GUI and the shell.
