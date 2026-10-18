//! A virtual Marlin 2.1 printer for tests, demos and development without
//! hardware. See `ARCHITECTURE.md` for the public contract.
//!
//! The simulator is an independent implementation of the host protocol
//! (it does not use `sprint-gcode` or `sprint-proto`), so host code tested
//! against it is checked against a second implementation rather than
//! against itself.
//!
//! # What is emulated
//!
//! Marlin 2.1.x built with EMERGENCY_PARSER, HOST_ACTION_COMMANDS,
//! HOST_PROMPT_SUPPORT, AUTO_REPORT_TEMPERATURES, AUTO_REPORT_POSITION,
//! EEPROM_SETTINGS, SDSUPPORT (+ long file names and SD writes),
//! AUTO_BED_LEVELING_BILINEAR, PIDTEMP/PIDTEMPBED, LIN_ADVANCE,
//! BABYSTEPPING, PRINTCOUNTER, FWRETRACT and ADVANCED_PAUSE_FEATURE:
//!
//! * boot banner after a (scaled) boot delay; input before that is dropped;
//! * `N<n> <cmd>*<cs>` framing with Marlin's error, `Resend:` and `ok`
//!   replies, M110, a BUFSIZE command buffer and an `ok` delayed while the
//!   planner is full; optional ADVANCED_OK;
//! * M108/M112/M410/M876 acted on immediately on receipt (except while
//!   writing to SD, where Marlin stores them in the file);
//! * a planner with trapezoidal profiles and look-ahead, homing and probing
//!   times, arcs, leveling, babystepping, soft endstops, cold extrusion
//!   prevention;
//! * heaters with heat capacity, losses and sensor lag under Marlin's PID,
//!   M109/M190 waits with residency, M303 relay autotune;
//! * EEPROM settings (M500/M501/M502/M503), M115 capabilities, M119;
//! * an in-memory SD card with uploads (M28/M29), printing, auto-reports
//!   and statistics;
//! * M600 filament change with `//action:` prompts, filament runout;
//! * fault injection (corrupted lines, lost `ok`s) from a deterministic RNG.
//!
//! Simulated time runs `time_scale` times faster than real time and applies
//! to heating, moves, dwells, autotune, keepalive and report intervals.
//!
//! # Example
//!
//! ```
//! use std::io::{Read, Write};
//! use std::time::Duration;
//!
//! let config = sprint_sim::SimConfig {
//!     boot_delay_ms: 10,
//!     time_scale: 50.0,
//!     ..Default::default()
//! };
//! let mut link = sprint_sim::spawn(config);
//! link.reader.set_timeout(Duration::from_millis(50));
//!
//! let mut text = String::new();
//! let mut buf = [0u8; 1024];
//! let mut read_until = |text: &mut String, want: &str| {
//!     while !text.contains(want) {
//!         match link.reader.read(&mut buf) {
//!             Ok(0) => panic!("simulator stopped"),
//!             Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
//!             Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::TimedOut),
//!         }
//!     }
//! };
//! read_until(&mut text, "echo:SD card ok\n");
//! assert!(text.starts_with("start\n"));
//!
//! link.writer.write_all(b"M105\n").unwrap();
//! read_until(&mut text, "ok T:");
//! assert_eq!(link.control.received_commands(), vec!["M105"]);
//! link.control.shutdown();
//! ```

#![warn(missing_docs)]

mod config;
mod format;
mod line;
mod link;
mod machine;
mod mesh;
mod parse;
mod planner;
mod rng;
mod sd;
mod settings;
mod tcp;
mod thermal;

pub use config::{MAX_SYNTHETIC_FILE_SIZE, SimConfig};
pub use link::{
    RECEIVED_LOG_CAPACITY, SimControl, SimLink, SimReader, SimSnapshot, SimWriter, spawn,
};
pub use tcp::{SimServer, serve_tcp};
