# The virtual printer

Sprint has a simulated printer built in: a Marlin 2.1 board with heaters,
a motion planner, an SD card, a bed mesh and EEPROM settings. It answers
like the real firmware, byte for byte where that matters, so everything
in Sprint can be tried, learned and tested without hardware and without
risk. It is a separate implementation of the printer side of the protocol
(it shares no code with Sprint's own parser), so it also catches host bugs
that a simulator built from the same code would hide.

## Using it

| Where | How |
|---|---|
| GUI | `sprint --sim`, or pick **Virtual printer** in the connection bar |
| Terminal UI, shell | `--connect sim`, F2 > Virtual, or `connect sim` |
| Headless commands | `--connect sim` |
| Any host over TCP | `sprint sim` (see below), then connect to `tcp://127.0.0.1:5000` |

The default profile ("Virtual printer") is connected to it, so a fresh
configuration never talks to real hardware.

## Options

Options go in the target as `sim://?key=value&key=value`, in the GUI's
printer profile (Speed, Extruders, Heated bed, Instant moves, Line errors,
Lost replies), or in the `[profiles.connection.Simulator]` table of the
[configuration file](configuration.md):

| Key | Default | Meaning |
|---|---|---|
| `time_scale` | 1 | Simulation speed: 1 is real time, 10 is ten times faster. It applies to heating, moves, dwells, autotune, keepalive and report intervals and the boot delay. |
| `instant_moves` | false | Moves, homing and probing take no time |
| `extruders` | 1 | Hotends and extruders, 1 to 8 |
| `heated_bed` | true | Has a heated bed |
| `heated_chamber` | false | Has a heated chamber |
| `build_volume` | `220,220,250` | X, Y, Z in mm |
| `boot_delay_ms` | 300 | Time before the boot banner (scaled); input before it is dropped, like a real board |
| `checksum_error_rate` | 0 | Probability (0 to 1) that a numbered line arrives "corrupted": the printer answers with a checksum error and asks for it again |
| `drop_ok_rate` | 0 | Probability (0 to 1) that an `ok` is lost, so the host must notice and recover |
| `planner_buffer` | 16 | Planner buffer size in moves; `ok` is held back while it is full |
| `rx_buffer` | 4 | Command buffer size (Marlin's BUFSIZE) |
| `auto_report_temps` | true | Advertise and honour M155 temperature auto-reports |
| `auto_report_position` | true | Advertise and honour M154 position auto-reports |
| `advanced_ok` | false | Answer `ok N<line> P<planner free> B<buffer free>` |
| `mesh_size` | `5x5` | G29 grid, columns x rows, each 2 to 15 |
| `ambient` | 22 | Ambient temperature (°C) |
| `machine_name` | Sprint Virtual Printer | Reported by M115 |
| `seed` | fixed | Seed for fault injection, sensor noise and the shape of the simulated bed; the same seed repeats the same faults |

Unknown keys are an error. The table shows the values as a target takes
them; in the configuration file `build_volume` and `mesh_size` are arrays
(`build_volume = [220.0, 220.0, 250.0]`, `mesh_size = [5, 5]`; text such
as `"5x5"` is dropped with a warning there), and `sd_files` sets the card's
files as `[name, size in bytes]` pairs
(`sd_files = [["cube.gcode", 48213]]`). Examples:

```bash
sprint tui --connect "sim://?time_scale=20"
sprint print cube.gcode -c "sim://?time_scale=50&instant_moves=true"
sprint shell -c "sim://?checksum_error_rate=0.02&drop_ok_rate=0.005&seed=7"
sprint gui --connect "sim://?extruders=2&heated_chamber=true&mesh_size=7x7"
```

Quote the target in shells where `&` and `?` mean something.

## What it does

* Boots with Marlin's banner, then answers M115 with capabilities
  (`Cap:` lines), M503 with a full settings report, M105, M114, M119, M78
  and M31 like Marlin.
* Checks line numbers and checksums exactly like Marlin (`Error:checksum
  mismatch, Last Line: N` followed by `Resend: N+1`, wrong line numbers,
  M110), with a command buffer and an `ok` that waits while the planner is
  full, so streaming behaves as on a real board.
* Plans moves with acceleration and look-ahead, homing and probing take
  time, arcs (G2/G3), babystepping, soft endstops and cold-extrusion
  prevention work.
* Heats with a thermal model (heat capacity, losses, sensor lag) under
  Marlin's PID: realistic heating curves, overshoot and heater power;
  M109/M190 wait like Marlin; M303 runs a relay autotune with Marlin's
  output.
* Stores M92, M201, M203, M204, M205, M301, M304, M851, M900 and more in a
  simulated EEPROM (M500, M501, M502).
* Probes a plausible warped bed with G29 and reports it with M420 V.
* Has an SD card with a few demo files: listing (M20, long names),
  printing (M23, M24, M25, M26, M27 with auto-reports), uploads (M28/M29),
  deleting (M30) and print statistics.
* Handles M600 filament changes and M0/M1 waits for the user with
  `//action:` host prompts (answered with M876), and M108.
* Acts on M112, M108, M410 and M876 as soon as they arrive (the emergency
  parser). M112 halts it until the board is reset: Sprint's **Reset
  board** (shell `reset`) restarts it, as a DTR pulse would restart a real
  board.

## Serving it over TCP

`sprint sim` runs the virtual printer as a TCP server so that another
program can connect to it: Pronterface, OctoPrint, a second Sprint, your
own scripts. Each client gets a freshly booted printer, one client at a
time.

```bash
sprint sim --tcp 127.0.0.1:5000 --time-scale 10
```

```text
Virtual printer listening on 127.0.0.1:5000
  Sprint Virtual Printer, 1 extruder, time scale 10x
Connect with:  sprint tui --connect tcp://127.0.0.1:5000
          or:  Printrun/Pronterface, port 127.0.0.1:5000
Each client gets a freshly booted printer. Press Ctrl+C to stop.
```

The [command line page](cli.md#sprint-sim) lists every option. Listen on
`127.0.0.1` (the default) unless other machines should reach it.

## Things to try

* Print a sample with `checksum_error_rate=0.05` and watch the resends in
  the console and the link statistics (status bar, Printer tab).
* Pause a print, jog away, and resume: the resume script returns the
  nozzle to the part.
* Send `M600` during a print: the printer asks for a filament change and
  Sprint shows its question.
* Press **E-STOP**, then **Reset board**.
* Run a PID autotune or the E-steps wizard in the Calibration tab, probe the
  bed in the Bed mesh tab, change settings in the Tuning tab and save them
  to the simulated EEPROM.
* Upload a file to the SD card and print it from there.
