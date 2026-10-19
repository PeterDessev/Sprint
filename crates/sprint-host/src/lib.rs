//! Printer host engine for Sprint: transports, the send queue with flow
//! control and resends, print jobs, printer state, events, the console log
//! and configuration. `ARCHITECTURE.md` at the repository root is the
//! contract between the crates.
//!
//! # Overview
//!
//! A [`Printer`] is a cheap, cloneable handle. [`Printer::connect`] opens a
//! [`Link`] to a [`ConnectTarget`] (serial port, TCP socket or the built-in
//! virtual printer) and starts two threads: a reader that turns bytes into
//! lines, and the engine, which owns the writer and runs the protocol
//! (handshake, line numbers and checksums, the in-flight window, resends,
//! timeouts, polling, jobs). Everything the engine learns lands in the
//! [`PrinterState`] ([`Printer::with_state`], [`Printer::snapshot`]), in
//! the console ([`Printer::console_since`]) and as [`Event`]s
//! ([`Printer::subscribe`]); [`Printer::set_waker`] lets a UI repaint when
//! something changed.
//!
//! * [`transport`]: targets, port listing (never opens a port), links.
//! * [`config`]: profiles and options, stored as TOML.
//! * [`state`], [`console`], [`event`], [`job`]: what the engine reports.
//! * [`script`]: placeholder templating for pause/resume/cancel scripts.
//!
//! **Never point tests or examples at a real serial port**; use
//! [`ConnectTarget::Simulator`] (or TCP loopback to the simulator).
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use std::time::{Duration, Instant};
//! use sprint_host::sprint_gcode::GcodeFile;
//! use sprint_host::sprint_sim::SimConfig;
//! use sprint_host::{ConnectOptions, ConnectTarget, JobStatus, PrintOptions, Printer};
//!
//! let sim = SimConfig { boot_delay_ms: 10, time_scale: 50.0, instant_moves: true, ..SimConfig::default() };
//! let printer = Printer::new();
//! printer.connect(ConnectTarget::Simulator(sim), ConnectOptions::default())?;
//!
//! let wait = |what: &str, cond: &dyn Fn(&sprint_host::PrinterState) -> bool| {
//!     let deadline = Instant::now() + Duration::from_secs(30);
//!     while !printer.with_state(cond) {
//!         assert!(Instant::now() < deadline, "timed out waiting for {what}");
//!         std::thread::sleep(Duration::from_millis(5));
//!     }
//! };
//! wait("the handshake", &|s| s.is_connected());
//!
//! printer.send("G28\nM114");
//! let file = Arc::new(GcodeFile::from_text("demo.gcode", "G1 X10 F3000\nG1 Y10\n"));
//! printer.start_print(file, PrintOptions::default())?;
//! wait("the print", &|s| s.job.as_ref().is_some_and(|j| j.status == JobStatus::Completed));
//! printer.disconnect();
//! # Ok::<(), sprint_host::HostError>(())
//! ```

#![warn(missing_docs)]

pub use sprint_gcode;
pub use sprint_proto;
pub use sprint_sim;

pub mod config;
pub mod console;
mod engine;
mod error;
pub mod event;
mod host;
pub mod job;
pub mod script;
pub mod state;
pub mod transport;

pub use config::{
    AppConfig, BedOrigin, ChecksumMode, ConnectOptions, Macro, PrinterProfile, TempPreset,
};
pub use console::{ConsoleEntry, ConsoleKind};
pub use error::HostError;
pub use event::{Event, EventBus};
pub use host::Printer;
pub use job::{JobInfo, JobKind, JobStatus, PrintOptions, Progress, ProgressModel};
pub use script::{ScriptContext, render_script, render_script_with};
pub use state::{
    AutotuneState, Axes, Axis, ConnectionStatus, Heater, HostPrompt, LinkStats, PrinterState,
    SdState, TempSample,
};
pub use transport::{BAUD_RATES, ConnectTarget, DEFAULT_BAUD, Link, PortInfo, list_ports};
