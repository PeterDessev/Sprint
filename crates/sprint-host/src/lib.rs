//! Printer host engine: transports, send queue and flow control, print jobs,
//! printer state, events and configuration.
//! See `ARCHITECTURE.md` for the public contract.

#![warn(missing_docs)]

pub use sprint_gcode;
pub use sprint_proto;
pub use sprint_sim;

pub mod config;
pub mod console;
mod engine;
mod error;
pub mod event;
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
pub use job::{JobInfo, JobKind, JobStatus, PrintOptions, Progress, ProgressModel};
pub use script::{ScriptContext, render_script, render_script_with};
pub use state::{
    AutotuneState, Axes, Axis, ConnectionStatus, Heater, HostPrompt, LinkStats, PrinterState,
    SdState, TempSample,
};
pub use transport::{BAUD_RATES, ConnectTarget, DEFAULT_BAUD, Link, PortInfo, list_ports};
