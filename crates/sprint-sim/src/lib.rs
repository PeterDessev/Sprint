//! A virtual Marlin-style 3D printer used for tests, demos and development
//! without real hardware. See `ARCHITECTURE.md` for the public contract.
#![allow(dead_code)]

mod config;
mod format;
mod rng;

pub use config::{MAX_SYNTHETIC_FILE_SIZE, SimConfig};
