//! Printer firmware protocol: line framing (line numbers and checksums),
//! reply parsing, firmware and settings models, and a G-code reference.
//!
//! Pure logic, no I/O. See `ARCHITECTURE.md` for the public contract.

#![warn(missing_docs)]

pub mod framing;

pub use framing::*;
