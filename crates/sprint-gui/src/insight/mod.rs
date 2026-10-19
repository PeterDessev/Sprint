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

#![warn(missing_docs)]

pub mod format;
pub mod thermal;
