//! Links to printers: connection targets, serial port enumeration and the
//! byte-stream [`Link`] the engine talks through.
//!
//! **Opening a serial port talks to real hardware.** Tests, examples and
//! demos use [`ConnectTarget::Simulator`] or TCP loopback to the simulator;
//! [`list_ports`] only enumerates port names and never opens anything.

mod link;
mod ports;
mod serial;
mod target;
mod tcp;

use serde::{Deserialize, Serialize};
use sprint_sim::SimConfig;

pub use link::{Link, LinkHandle, READ_TIMEOUT, is_no_data};
pub use ports::{PRINTER_USB_VENDORS, is_printer_vendor, list_ports};

/// Baud rates tried by auto-detection (baud 0), most common first.
pub const BAUD_RATES: &[u32] = &[
    250_000, 115_200, 230_400, 500_000, 1_000_000, 57_600, 38_400, 19_200, 9_600,
];

/// Baud rate used when a serial target names none (`COM3`).
pub const DEFAULT_BAUD: u32 = 115_200;

/// Where to connect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ConnectTarget {
    /// A serial port (`COM3`, `/dev/ttyUSB0`). `baud` 0 means auto-detect.
    Serial {
        /// Port name as the operating system knows it.
        port: String,
        /// Baud rate; 0 = try [`BAUD_RATES`] until the printer answers.
        baud: u32,
    },
    /// A raw TCP socket (serial-to-network bridges, `sprint sim --tcp`).
    Tcp {
        /// Host name or IP address (IPv6 without brackets).
        host: String,
        /// TCP port.
        port: u16,
    },
    /// The built-in virtual printer.
    Simulator(SimConfig),
}

impl Default for ConnectTarget {
    /// The virtual printer with its default configuration.
    fn default() -> Self {
        ConnectTarget::Simulator(SimConfig::default())
    }
}

/// A serial port found by [`list_ports`].
#[derive(Clone, Debug, PartialEq)]
pub struct PortInfo {
    /// Port name to connect to (`COM3`, `/dev/ttyACM0`).
    pub name: String,
    /// Human-readable description (USB product name when known).
    pub description: String,
    /// USB vendor ID.
    pub vid: Option<u16>,
    /// USB product ID.
    pub pid: Option<u16>,
    /// USB manufacturer string.
    pub manufacturer: Option<String>,
    /// USB serial number.
    pub serial_number: Option<String>,
    /// The USB vendor is a serial bridge or board commonly used by 3D
    /// printers (CH340, FTDI, CP210x, Arduino, Prusa, STM32 VCP, ...).
    pub likely_printer: bool,
}
