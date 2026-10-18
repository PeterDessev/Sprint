//! A virtual Marlin-style 3D printer used for tests, demos and development
//! without real hardware. See `ARCHITECTURE.md` for the public contract.
#![allow(dead_code)]

mod config;
mod format;
mod line;
mod mesh;
mod parse;
mod planner;
mod rng;
mod sd;
mod settings;
mod thermal;

pub use config::{MAX_SYNTHETIC_FILE_SIZE, SimConfig};
