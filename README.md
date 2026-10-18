# Sprint

A fast, modern 3D-printer host written in Rust — a replacement for
[Printrun](https://github.com/kliment/Printrun) (Pronterface/pronsole).

* Loads and visualizes G-code (GPU 3D view, layer view, color by feature,
  speed, width, layer height, tool, fan, time).
* Streams prints over USB serial or TCP with line numbers, checksums, resend
  handling and flow control; pause/resume/cancel with park scripts,
  start-from-line recovery, SD-card printing and upload.
* Shows far more about the printer: firmware and capabilities, every
  EEPROM setting with descriptions, endstops, bed mesh heatmap, print
  statistics, thermal stability, link quality.
* Tunes the printer: catalog-driven settings editor, PID autotune, E-steps
  calibration, live speed/flow/fan/babystepping.
* GUI, full-screen terminal UI, pronsole-style shell and scriptable CLI —
  and a raw G-code console everywhere.
* Built-in virtual printer for trying everything without hardware.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the design.

## Building

```bash
cargo build --release
```

The binary is `target/release/sprint` (`sprint.exe` on Windows).
