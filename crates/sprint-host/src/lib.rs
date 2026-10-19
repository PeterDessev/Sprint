//! Printer host engine: transports, send queue and flow control, print jobs,
//! printer state, events and configuration.
//! See `ARCHITECTURE.md` for the public contract.

#![warn(missing_docs)]

pub use sprint_gcode;
pub use sprint_proto;
pub use sprint_sim;

mod error;
pub mod script;
pub mod transport;

pub use error::HostError;
pub use script::{ScriptContext, render_script, render_script_with};
pub use transport::{BAUD_RATES, ConnectTarget, DEFAULT_BAUD, PortInfo, list_ports};
