//! Printer firmware protocol: line framing (line numbers and checksums),
//! reply parsing, firmware and settings models, a G-code reference and the
//! command-safety rules the front ends apply before sending.
//!
//! Pure logic, no I/O. See `ARCHITECTURE.md` for the public contract.
//!
//! ```
//! use sprint_proto::{PrinterSettings, Reply, ReplyParser, frame_numbered, gcode_doc};
//!
//! // What the host sends.
//! assert_eq!(frame_numbered(1, "G28"), "N1 G28*18");
//!
//! // What the printer answers.
//! let mut parser = ReplyParser::new();
//! let replies = parser.feed("ok T:210.0 /210.0 B:60.0 /60.0 @:64 B@:0");
//! assert!(matches!(replies[0], Reply::Ok { .. }));
//! let Reply::Temperatures(temps) = &replies[1] else { panic!("no temperatures") };
//! assert_eq!(temps.hotends[0].target, Some(210.0));
//!
//! // M503 output becomes typed settings.
//! let mut settings = PrinterSettings::default();
//! for reply in parser.feed("echo:  M92 X80.00 Y80.00 Z400.00 E93.00") {
//!     if let Reply::Setting(cmd) = reply {
//!         settings.insert(cmd);
//!     }
//! }
//! assert_eq!(settings.steps_per_mm(), Some([80.0, 80.0, 400.0, 93.0]));
//!
//! // Reference lookups for tooltips and confirmations.
//! assert!(gcode_doc("m502").is_some_and(|d| d.dangerous));
//! ```

#![warn(missing_docs)]

pub mod firmware;
pub mod framing;
pub mod mesh;
mod nan;
pub mod reference;
pub mod reply;
pub mod safety;
pub mod settings;

pub use firmware::*;
pub use framing::*;
pub use mesh::*;
pub use reference::*;
pub use reply::*;
pub use safety::*;
pub use settings::*;
