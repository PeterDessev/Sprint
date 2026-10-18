//! Printer firmware protocol: line framing (line numbers and checksums),
//! reply parsing, firmware and settings models, and a G-code reference.
//!
//! Pure logic, no I/O. See `ARCHITECTURE.md` for the public contract.

#![warn(missing_docs)]

pub mod firmware;
pub mod framing;
pub mod mesh;
pub mod settings;

pub use firmware::*;
pub use framing::*;
pub use mesh::*;
pub use settings::*;
