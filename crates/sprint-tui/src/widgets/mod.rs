//! Ratatui widgets for the Sprint terminal interfaces.
//!
//! Every widget takes its own small input types (no host types), so it can
//! be fed from the live printer state, a recorded session or synthetic
//! data, and is drawn with a [`Theme`](crate::theme::Theme).

pub mod console;
pub mod heater;

pub use console::{ConsoleKind, ConsoleLine};
pub use heater::{HeaterKind, HeaterState};
