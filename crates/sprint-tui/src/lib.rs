//! Sprint terminal interfaces: the full-screen TUI and the line-mode shell.
//! See `ARCHITECTURE.md` for the public contract.
//!
//! This crate provides the building blocks of the terminal UI:
//!
//! * [`theme`]: the Sprint palette for true-color, 256-color, 16-color and
//!   colorless (`NO_COLOR`) terminals;
//! * [`widgets`]: ratatui widgets for the layer view, temperatures, job
//!   progress, the console, the command line with G-code completion, key
//!   hints, help and toolpath statistics;
//! * [`format`](mod@format): number, length and duration formatting shared
//!   with the command-line reports.
//!
//! Widgets take their own small input types, never host types, and draw
//! with a [`Theme`]. `cargo run -p sprint-tui --example widgets_demo` shows
//! them all with synthetic data.

#![warn(missing_docs)]

pub mod format;
pub mod theme;
pub mod widgets;

pub use theme::{ColorSupport, Theme};
pub use widgets::*;
