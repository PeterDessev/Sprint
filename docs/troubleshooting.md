# Troubleshooting

The console (GUI, terminal UI) or the shell's output usually says what
went wrong; the link statistics (the GUI's status bar and Printer tab, the
shell's `status` and `stats`) show resends, errors and timeouts. For more
detail, start Sprint from a terminal with `RUST_LOG=debug` (PowerShell:
`$env:RUST_LOG = "debug"`).

## Connection problems

**The port is not listed.** `sprint ports` shows what the system sees.

* Check the USB cable (some only charge) and try another USB port.
* Install the driver of the board's USB chip (CH340, CP210x and FTDI chips
  need one on older Windows versions).
* On Linux, your user needs access to the port: usually membership in the
  `dialout` group (`uucp` on some distributions), then log in again.

**"Access denied" or "port busy".** Another program has the port open:
Pronterface, a slicer's USB printing, OctoPrint, a serial monitor, a
firmware updater. Close it; only one program can use a serial port.

**Nothing answers, or the console fills with garbage.** The baud rate is
wrong. Use **Auto** in the GUI or `@auto` in a target (`COM3@auto`); it
tries 250000, 115200 and the other common rates. Most Marlin boards use
115200 or 250000, Prusa printers 115200.

**The handshake times out.** Many boards reset when the port opens and
take a few seconds to boot (longer with a bootloader or a big EEPROM).
Raise the profile's handshake timeout (`handshake_timeout_secs`, default
15 s). Boards that do not reset by themselves may need *Reset the board when
connecting* (`reset_on_connect`) to come up in a known state.

**Connecting stopped the printer.** Opening a serial port resets most
boards (the DTR line), which ends a print started from the printer's own
screen or SD card. Do not connect to a printer that is printing from its
card unless you mean to; Sprint never connects by itself unless a profile
has `auto_connect` turned on.

**TCP connections.** `tcp://host:port` expects a raw byte stream to the
printer's serial port (ESP3D's or a WiFi module's raw TCP port, ser2net in
raw mode, `sprint sim`). Web interfaces and APIs (OctoPrint, Moonraker,
Duet's HTTP API) are not raw serial streams and do not work.

**Klipper.** Connect to the G-code pseudo-terminal the Klipper host
exposes (usually `/tmp/printer`), not to the micro-controller's USB port,
which speaks Klipper's own protocol.

## Resends

A resend means the printer received a line with a wrong checksum or line
number and asked for it again. Sprint sends it again automatically, so
occasional resends are harmless; the link statistics count them.

Many resends point to the link:

* electrical noise: a long or unshielded USB cable, a cable near the
  stepper or heater wires, a printer and computer on different power
  outlets;
* a baud rate the board cannot keep up with (try 115200);
* more commands in flight than the firmware's buffer takes: set *Commands
  in flight* (`max_in_flight`) back to 1.

A line the printer keeps rejecting, or a resend request for a line Sprint
no longer has, fails the print (the heaters are switched off unless the
profile keeps them on); see [Recovering](#recovering-a-print).

The virtual printer can produce resends on purpose to see how this looks:
`--connect "sim://?checksum_error_rate=0.05"`.

## Timeouts

Every command is answered with `ok`. When nothing comes back for the
profile's reply timeout (`ok_timeout_secs`, default 10 s, six times that
for homing, probing, heating and dwells) and the printer sends no `busy:`
messages either, Sprint assumes the `ok` was lost and goes on, and counts
a timeout. Frequent timeouts mean the link loses data (see resends) or the
firmware is busy for long stretches without saying so; raise the reply
timeout, or enable Marlin's `HOST_KEEPALIVE_FEATURE` so it reports
`busy: processing`.

`sprint send` waits for each command for `--timeout` seconds (default 30);
busy messages and, during M109/M190/M191, temperature reports restart the
wait.

## Halts and board resets

The firmware *halts* after an emergency stop (M112) and when it detects a
dangerous fault: thermal runaway, a heater or sensor outside its range
(MINTEMP, MAXTEMP), a failed homing. It switches the heaters and motors
off and stops listening. Sprint shows *Halted* with the reason.

How to get it running again:

1. **Reset the board.** The GUI's halted banner and **Printer** menu offer
   **Reset board**; in the terminal UI and the shell it is `reset`. On a
   serial port Sprint closes the port and opens it again with a DTR pulse,
   which restarts most boards, then handshakes again. The virtual printer
   resets itself.
2. If the board does not restart that way (some boards with native USB
   ignore DTR), or the printer is behind a network bridge, press its reset
   button or power-cycle it, then **Reconnect**.
3. Only when the firmware says it can recover in place (Marlin's "Printer
   stopped due to errors ... use M999", Klipper's `FIRMWARE_RESTART`),
   **Restart firmware (M999)** is enough. After M112 or a thermal runaway
   Marlin ignores M999 until the board is reset.

Fix the cause first: a thermal runaway or MINTEMP error means a loose
thermistor or heater wire, a failed heater or a fan blowing on the nozzle.

After a reset the printer has lost its position: home before moving.

## Recovering a print

When a host print fails or is cancelled, Sprint remembers the first line
the printer had not confirmed:

* the GUI's job bar offers **Recover…**, which opens the start dialog at
  that line;
* the shell and the terminal UI have `recover`;
* `sprint print` prints the command to run, for example
  `resume with: sprint print cube.gcode --start-line 4721`.

The resume starts with a preamble: heat to the temperatures of that line,
home X and Y (not Z, so the nozzle cannot crash into the part), lift, move
over the resume position, lower, and restore the extruder.

* If the board was reset or lost power while the nozzle stayed where the
  print stopped, turn on **Board was reset** (`--board-reset`): the
  preamble declares the nozzle's height instead of trusting the firmware,
  which thinks it is at 0.
* The printer confirms a line when it queues it, not when it has printed
  it, so the last few moves before that line may be missing after a halt
  or a power loss. Look at the part, and start a little earlier (at the
  start of the layer) when in doubt.
* Check the preamble in the start dialog before starting; it can be
  edited.

## Pauses that do not resume

* A pause made at the printer (its menu, a filament runout, M600, M0) is
  usually waiting for the user: answer the printer's question in Sprint or
  on its screen. When the firmware waits without a question (*Waits for
  the user*), **Continue** in the GUI, F6 in the terminal UI or `M108`
  releases it.
* `--answer-prompts continue` makes `sprint print` answer such questions
  by itself (a pause made at the printer's menu is still left to the
  person there).

## Files

**"This is a 3D model, not G-code: slice it first."** STL, 3MF, OBJ and
STEP files are models; slice them in your slicer and load the G-code.
Binary G-code (`.bgcode`) is not supported: export plain-text G-code from
the slicer.

**The print-time estimate is off.** Sprint plans with the printer's motion
limits when connected (GUI) or the profile's. Read the firmware's settings
(**Printer > Read firmware info and settings**), or set custom limits in
the profile's *Print time estimation*. Firmware features Sprint does not
model can still shift real times by a few percent.

## The configuration

**Sprint warns about the configuration file at startup.** Some lines or
values could not be read; everything else was kept and the original was
copied to `config.toml.bak` (or a numbered backup) next to it. Compare the
two and fix the lines named in the warning. See
[Configuration](configuration.md#damaged-files).

**A test or a second setup should not touch your configuration.** Point
`SPRINT_CONFIG` at another file.

## Windows

**Console windows.** `sprint.exe` is a console program. Double-clicked
(or opened through a file association), it closes the console window
Windows opens for it and shows errors in a message box instead. Started
from a terminal it keeps the terminal: log messages appear there, and
`cmd` and PowerShell wait until the GUI closes. To start the GUI without
waiting, use `start sprint` (cmd) or `Start-Process sprint` (PowerShell).

**The terminal UI.** Windows Terminal gives the best result (true colors,
mouse, bracketed paste); the classic console works too. If the layer view
shows boxes or question marks, the font lacks Braille characters: use a
font that has them, such as Cascadia Mono. Ctrl+C does not kill Sprint
in the middle of a print: the terminal UI asks before quitting, and the
shell only stops what it is waiting for (at the end of a script that
cancels the print, with the cancel script).

**The GUI does not open** ("could not open the Sprint window"). The
graphics driver may not support the default GPU backend, which happens in
virtual machines and some remote-desktop sessions. Try another backend:
`$env:WGPU_BACKEND = "dx12"` (or `vulkan`, `gl`) before starting `sprint`.
