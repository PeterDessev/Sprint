//! Ratatui widgets for the Sprint terminal interfaces.
//!
//! Every widget takes its own small input types (no host types), so it can
//! be fed from the live printer state, a recorded session or synthetic
//! data, and is drawn with a [`Theme`](crate::theme::Theme).

pub mod console;
pub mod heater;
pub mod help;
pub mod input;
pub mod job;
pub mod layer_canvas;
pub mod temp_chart;
#[cfg(test)]
pub(crate) mod testutil;
pub(crate) mod util;

pub use console::{ConsoleKind, ConsoleLine, ConsoleState, ConsoleView, find_matches};
pub use heater::{HeaterGauge, HeaterKind, HeaterState};
pub use help::{HelpOverlay, HelpSection, KeyHints};
pub use input::{
    Candidate, CommandInput, CommandLine, Completer, CompletionPopup, InputEvent, gcode_candidates,
};
pub use job::{JobProgress, JobTone};
pub use layer_canvas::{FeatureLegend, LayerCanvas, LayerFit, object_bounds};
pub use temp_chart::{TempChart, TempPoint};
