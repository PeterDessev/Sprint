//! Printer insight: the pure logic behind the Printer, Tuning, Calibration,
//! Bed mesh and SD card tabs.
//!
//! Everything here works on plain data (temperature history, meshes,
//! numbers typed by the user) and never talks to the printer or draws
//! anything, so it is unit-tested in isolation:
//!
//! * [`format`]: human-readable durations ("1d 2h 3m"), lengths, byte
//!   counts, rates and percentages.
//! * [`thermal`]: heater stability at target (mean, standard deviation,
//!   peak-to-peak), heat-up detection (time to target, overshoot, settling
//!   time), holding power and the findings derived from them (oscillation
//!   that warrants a PID autotune, an underpowered heater, ...).
//! * [`mesh`]: bed mesh geometry, bilinear interpolation and the bed-screw
//!   helper (how far to turn each leveling screw).
//! * [`calibrate`]: E-steps, PID, flow and Z-offset arithmetic used by the
//!   calibration wizards, and the setting commands that apply the results.
//! * [`sdname`]: 8.3 file names for SD card uploads.

#![warn(missing_docs)]

pub mod calibrate;
pub mod format;
pub mod mesh;
pub mod sdname;
pub mod thermal;
