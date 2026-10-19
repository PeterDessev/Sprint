//! Sprint terminal interfaces: the full-screen TUI and the line-mode shell.
//! See `ARCHITECTURE.md` for the public contract.
//!
//! * [`run_tui`]: the full-screen terminal UI (temperatures, position,
//!   job, a Braille layer view following the print, the console and a
//!   G-code line), driven by function keys and a command palette;
//! * [`theme`]: the Sprint palette for true-color, 256-color, 16-color and
//!   colorless (`NO_COLOR`) terminals;
//! * [`widgets`]: ratatui widgets for the layer view, temperatures, job
//!   progress, the console, the command line with G-code completion, key
//!   hints, help and toolpath statistics;
//! * [`format`](mod@format): number, length and duration formatting shared
//!   with the command-line reports;
//! * [`commands`]: the shell command set and its parser, shared by the
//!   TUI's command palette and the line shell;
//! * [`report`]: text reports of the printer state (status, settings, bed
//!   mesh, firmware, ...);
//! * [`loader`], [`paths`], [`actions`], [`config`]: loading files in the
//!   background, path completion, small decisions shared by the front ends
//!   and where the configuration lives;
//! * [`session`], [`interrupt`]: blocking connects and waits, and Ctrl+C as
//!   a request, for the shell and the headless commands.
//!
//! Widgets take their own small input types, never host types, and draw
//! with a [`Theme`]. `cargo run -p sprint-tui --example widgets_demo` shows
//! them all with synthetic data.
//!
//! Neither front end connects to a printer unless it is asked to (the
//! `connect` option) or the active profile enables `auto_connect`.

#![warn(missing_docs)]

pub mod actions;
mod app;
pub mod commands;
pub mod config;
pub mod format;
pub mod interrupt;
pub mod loader;
pub mod paths;
pub mod report;
pub mod session;
#[cfg(test)]
mod snapshot;
pub mod theme;
pub mod widgets;

use std::path::PathBuf;

use sprint_host::ConnectTarget;

pub use app::terminal::run_tui;
pub use theme::{ColorSupport, Theme};
pub use widgets::*;

/// What the terminal interfaces start with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TuiOptions {
    /// G-code file to load at startup.
    pub file: Option<PathBuf>,
    /// Printer to connect to at startup. Without one, only a profile with
    /// `auto_connect` connects by itself.
    pub connect: Option<ConnectTarget>,
}
