# Configuration

Every front end (the GUI, the terminal UI, the shell and the headless
commands) reads and writes one configuration file. It holds the printer
profiles, the active profile, the macros, the recent files and the console
size. The GUI's own state (window size and layout, the console history,
view options) is kept separately by the app.

## Where it is

| Platform | Default location |
|---|---|
| Windows | `%APPDATA%\Sprint\config.toml` |
| Linux | `$XDG_CONFIG_HOME/Sprint/config.toml` (usually `~/.config/Sprint/config.toml`) |
| macOS | `~/Library/Application Support/Sprint/config.toml` |

The environment variable **`SPRINT_CONFIG`** names another file, for every
front end, the GUI included: a portable setup, a second configuration for
a different workshop, or a test that must not touch your real one. A blank
value counts as unset. The GUI's preferences show the path in use.

```bash
SPRINT_CONFIG=~/printers/lab.toml sprint tui
```

```powershell
$env:SPRINT_CONFIG = "D:\sprint\config.toml"; sprint
```

The file is created the first time Sprint saves something (a profile, a
macro, a recent file); until then the defaults apply. The shell keeps its
command history in `shell-history.txt` next to it.

Sprint writes the file atomically (a temporary file renamed over it), so a
crash never leaves half a file. Edit it with any text editor while Sprint
is closed; changes made by a running GUI or shell may otherwise be saved
over.

### Damaged files

Sprint never refuses to start because of the configuration. A file that
does not fully parse (a typo, a value of the wrong type, a setting from a
newer version) is read line by line: every valid profile, macro and value
is kept, and only the broken ones fall back to their defaults. Sprint
lists what it dropped (the GUI in a notification and in its settings, the
terminal front ends on startup) and first copies the original to a backup
next to it: `config.toml.bak`, then `config.toml.1.bak`, `config.toml.2.bak`,
...; an existing backup is never overwritten.

## The format

The file is [TOML](https://toml.io). Every key is optional; missing keys
take the defaults shown below. This is the default configuration with
comments (Sprint does not write the comments back):

```toml
active_profile = 0          # index into [[profiles]], from 0
recent_files = []           # most recent first, at most 12
console_max_lines = 5000    # console lines kept in memory (100 to 1000000)

[[profiles]]
name = "Virtual printer"
auto_connect = false        # connect when Sprint starts (GUI, TUI, shell)
bed_size = [220.0, 220.0]   # X, Y in mm
max_z = 250.0               # mm
origin = "FrontLeft"        # or "Center" (deltas, some CoreXY machines)
extruders = 1
heated_bed = true
heated_chamber = false
filament_diameter = 1.75    # mm, for the analysis when the file does not say
nozzle_diameter = 0.4       # mm
jog_feedrate_xy = 3000.0    # mm/min
jog_feedrate_z = 600.0      # mm/min
extrude_feedrate = 300.0    # mm/min
emit_progress = false       # M73 progress on the printer's display
keep_heaters_on_failure = false
on_start_command = ""       # hook commands, see below
on_finish_command = ""
on_fail_command = ""
on_cancel_command = ""

# Scripts: one G-code command per line, with placeholders (see below).
pause_script = """
M400
G91
G1 E-2 F2700
G1 Z10 F600
G90
G1 X{park_x} Y{park_y} F6000
"""
resume_script = """
G1 X{x} Y{y} F6000
G1 Z{z} F600
G91
G1 E2 F2700
G90
G92 E{e}
G1 F{f}
"""
cancel_script = """
{heaters_off}
M107
G91
G1 Z10 F600
G90
G28 X Y
M84
"""

# Where the printer is: exactly one of the three tables.
[profiles.connection.Simulator]   # the built-in virtual printer
time_scale = 1.0
# ... every option of the virtual printer, see virtual-printer.md

# [profiles.connection.Serial]
# port = "COM3"                   # or "/dev/ttyUSB0"
# baud = 250000                   # 0 = auto-detect

# [profiles.connection.Tcp]
# host = "192.168.1.50"
# port = 23

[profiles.options]
checksum = "Always"           # "Always", "PrintOnly" or "Never"
ok_timeout_secs = 10.0        # no reply for this long counts as a lost "ok"
handshake_timeout_secs = 15.0 # how long the printer may take to answer after opening
temp_poll_secs = 2.0          # M105 polling without auto-reports; 0 = off
query_on_connect = true       # M115, M503 and M114 after the handshake
reset_on_connect = false      # pulse DTR when opening a serial port
max_in_flight = 1             # commands awaiting their "ok" at once (1 to 32)
autoreport_interval_secs = 2  # M155 / M27 S<n> when the firmware supports them

[[profiles.temp_presets]]
name = "PLA"
hotend = 205.0
bed = 60.0
# ... PETG 240/80, ABS 250/100 and TPU 225/50 by default

# Optional: motion limits for the print-time estimate.
# [profiles.kinematics]
# max_feedrate = [500.0, 500.0, 12.0, 120.0]   # X Y Z E, mm/s
# max_accel = [3000.0, 3000.0, 100.0, 10000.0] # mm/s²
# print_accel = 1500.0
# retract_accel = 3000.0
# travel_accel = 1500.0
# junction_deviation = 0.013                   # mm; false = classic jerk
# jerk = [8.0, 8.0, 0.4, 5.0]                  # mm/s, used with classic jerk
# min_feedrate = 0.0

[[macros]]
name = "Home"
gcode = "G28"
confirm = false               # ask before running

[[macros]]
name = "Bed mesh"
gcode = """
G28
G29
M420 V"""
confirm = true

[[macros]]
name = "Cool down"
gcode = """
{heaters_off}
M107"""

[[macros]]
name = "Motors off"
gcode = "M84"
# color = [255, 122, 26]      # optional button color (RGB)
```

### Profiles

Add a `[[profiles]]` table per printer; `active_profile` picks one by its
position. The GUI (**Tools > Printer profiles**), the shell's `profile`
command and `--profile NAME` switch between them. `connect --save` in the
shell stores a target as the profile's printer.

**Protocol options.** Leave them alone unless something does not work:

* `checksum = "Always"` numbers every line and adds a checksum, so the
  printer can ask for a corrupted line again. `"PrintOnly"` numbers only
  while printing or uploading; `"Never"` is for firmware without line
  numbers (SD uploads are still numbered, as Marlin requires).
* `ok_timeout_secs`: when the printer stays silent this long (no `ok`, no
  `busy:` message) after a command, Sprint assumes the `ok` was lost and
  recovers. Commands known to take long (homing, probing, heating, dwells)
  get six times as long.
* `max_in_flight` above 1 sends the next commands before the previous
  `ok` arrived (faster streaming of many short moves). Marlin's command
  buffer (BUFSIZE, usually 4) limits how far this helps; use it only with
  firmware that handles it.
* `reset_on_connect` pulses DTR when the port opens. Most boards reset on
  open anyway; this makes it reliable. It stops a print running from the
  printer's own SD card.

## Scripts and placeholders

The **pause script** runs when a host print pauses, the **resume script**
before it continues, and the **cancel script** when it is cancelled. The
defaults retract and park at the back left corner, return and prime, and
switch the heaters off, lift and home X/Y. Edit them in the GUI's printer
profile or in the file.

A script is G-code with one command per line; `;` comments and blank lines
are dropped. Macros use the same templating.

### Placeholders

| Placeholder | Value |
|---|---|
| `{x}` `{y}` `{z}` | The position (mm) when the script runs (for the resume script: where the print paused) |
| `{e}` | The extruder position (mm) |
| `{f}` | The feedrate (mm/min) |
| `{tool}` | The active tool number |
| `{hotend}` | The target temperature of the active tool (°C) |
| `{bed}` | The bed's target temperature (°C) |
| `{fan}` | The part-cooling fan speed (0 to 255) |
| `{max_x}` `{max_y}` `{max_z}` | The build volume's largest coordinates, from the profile (mm) |
| `{park_x}` `{park_y}` | The parking position: the back left corner, 10 mm in from the edges |
| `{heaters_off}` | Every heater off, one command per line: `M104 S0` (`M104 T<n> S0` for each hotend of a multi-hotend printer), `M140 S0` with a heated bed, `M141 S0` with a heated chamber; it also covers heaters the printer reports that the profile leaves out |

Unknown placeholders stay as written. A line that uses `{e}` is left out
while the extruder is in relative mode (M83 or G91): there is no absolute
position to restore then, which is why the default resume script's
`G92 E{e}` disappears for files that extrude relatively.

The GUI and the terminal front ends show scripts and macros with their
placeholders filled in wherever they ask for a confirmation.

### Macro arguments

Macros defined in the shell (and run from any front end) can take
arguments, the way pronsole's macros do:

| Placeholder | Value |
|---|---|
| `{0}`, `{1}`, ... | The arguments in order |
| `{1=200}` | The second argument, or 200 when it is not given |
| `{args}` | All arguments, separated by spaces |

```text
sprint> macro define heat M104 S{0=205} | M140 S{1=60}
sprint> macro heat 215 70
```

A macro with a missing argument (one without a default) is not sent; the
GUI asks for the arguments in a dialog.

### Host commands in G-code files

Lines in a printed file that are meant for the host:

| Line | Effect |
|---|---|
| `;@pause` (Printrun), `@pause` (OctoPrint) | Pause there with the pause script |
| `@cancel`, `@abort` | Cancel there with the cancel script |
| Another `@...` line | Reported in the console, not sent |

## Hook commands

A profile can run a shell command when a print starts, finishes, fails or
is cancelled (Printrun's start, final and error commands): to send a
notification, switch a relay, log the job or move the file. Hooks run for
host prints and SD prints (not for uploads), on a helper thread, so they
never hold up the printer. Windows runs them with `cmd /C`, other systems
with `sh -c`. A hook still running after two minutes is stopped. Its exit
status and the first lines of its output appear in the console.

| Key | Runs when |
|---|---|
| `on_start_command` | A print starts |
| `on_finish_command` | A print completes |
| `on_fail_command` | A print fails (a halt, a lost connection, an error) |
| `on_cancel_command` | A print is cancelled (by you or the printer) |

The command gets these environment variables:

| Variable | Value |
|---|---|
| `SPRINT_FILE` | The printed file's path (its name for SD prints and files not loaded from disk) |
| `SPRINT_STATUS` | `started`, `completed`, `failed` or `cancelled` |
| `SPRINT_ELAPSED_SECS` | Printing time so far, in whole seconds |
| `SPRINT_PRINTER` | The profile's name |
| `SPRINT_REASON` | Why the print failed (only for failures) |

Quote the variables: a file name such as `a & b.gcode`, a profile name or
a failure reason may contain characters the shell would act on.

On Windows (`cmd`):

```toml
on_finish_command = 'msg * "Sprint: %SPRINT_FILE% %SPRINT_STATUS% after %SPRINT_ELAPSED_SECS% s"'
on_fail_command = 'echo %DATE% %TIME% "%SPRINT_FILE%" failed: "%SPRINT_REASON%" >> "%USERPROFILE%\prints.log"'
```

On Linux and macOS (`sh`):

```toml
on_finish_command = 'notify-send "Print done" "$SPRINT_FILE ($SPRINT_PRINTER)"'
on_fail_command = 'curl -s -d "$SPRINT_PRINTER: $SPRINT_FILE failed: $SPRINT_REASON" https://ntfy.sh/my-printer'
```

## Progress on the printer's display

With `emit_progress = true` (in the GUI: the profile's printing options),
host prints send `M73 P<percent> R<minutes left>` whenever the percentage
changes, so the printer's screen shows Sprint's progress and remaining
time (Pronterface's "Display progress on printer"). The profile's setting
applies to host prints started from every front end. With it off, the
GUI's start dialog and `sprint print --progress-on-printer` turn it on for
a single print; with it on, every host print sends it (a single print
cannot turn it off). SD prints show the printer's own progress. Firmware
without M73 support ignores it.
