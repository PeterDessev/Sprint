//! The byte-stream link between the engine and a printer.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sprint_sim::{SimConfig, SimControl};

use super::{ConnectTarget, serial, tcp};
use crate::error::HostError;

/// How long a link read waits for data before reporting "no data yet".
pub const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// A bidirectional byte stream to a printer, split into a reader half and a
/// writer half so a reader thread can block on input while the engine
/// writes.
///
/// The reader must return within a short time (about 100 ms) when no data
/// arrives, with an [`io::ErrorKind::TimedOut`] or
/// [`io::ErrorKind::WouldBlock`] error ("no data yet"); `Ok(0)` means the
/// link was closed. The built-in backends ([`Link::open`]) behave like
/// that; a custom link passed to
/// [`Printer::connect_link`](crate::Printer::connect_link) must too, or
/// disconnecting waits for its reads.
pub struct Link {
    pub(crate) reader: Box<dyn Read + Send>,
    pub(crate) writer: Box<dyn Write + Send>,
    pub(crate) control: LinkControl,
}

/// Backend-specific handle kept by the engine.
pub(crate) enum LinkControl {
    /// Nothing to do on close.
    None,
    /// The virtual printer: shut it down on close.
    Sim(SimControl),
    /// A TCP socket: shut it down on close so blocked reads return.
    Tcp(TcpStream),
    /// A serial port driven by an I/O thread: tell the thread to stop (it
    /// closes the port).
    Serial(Arc<AtomicBool>),
}

impl Link {
    /// A link from any reader/writer pair (custom transports, tests). See
    /// the type documentation for the reader's timeout contract.
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Link {
        Link {
            reader: Box::new(reader),
            writer: Box::new(writer),
            control: LinkControl::None,
        }
    }

    /// Start a virtual printer and link to it.
    pub fn simulator(config: SimConfig) -> Link {
        let mut sim = sprint_sim::spawn(config);
        sim.reader.set_timeout(READ_TIMEOUT / 2);
        Link {
            reader: Box::new(sim.reader),
            writer: Box::new(sim.writer),
            control: LinkControl::Sim(sim.control),
        }
    }

    /// Connect to a raw TCP socket.
    pub fn tcp(host: &str, port: u16) -> Result<Link, HostError> {
        tcp::connect(host, port)
    }

    /// Open a serial port at `baud` (0 = auto-detect, see [`BAUD_RATES`](super::BAUD_RATES)),
    /// 8N1 without flow control. With `reset`, DTR is pulsed after opening,
    /// which resets most printer boards. `log` receives progress messages
    /// (auto-detection attempts).
    ///
    /// **This talks to real hardware.**
    pub fn serial(
        port: &str,
        baud: u32,
        reset: bool,
        log: &mut dyn FnMut(&str),
    ) -> Result<Link, HostError> {
        Link::serial_until(port, baud, reset, log, &|| false)
    }

    fn serial_until(
        port: &str,
        baud: u32,
        reset: bool,
        log: &mut dyn FnMut(&str),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Link, HostError> {
        if baud == 0 {
            let (_, link) = serial::detect_baud_until(
                super::BAUD_RATES,
                |b| serial::open(port, b, reset),
                serial::AUTO_BAUD_WAIT,
                log,
                cancelled,
            )?;
            Ok(link)
        } else {
            serial::open(port, baud, reset)
        }
    }

    /// Open the link `target` describes.
    pub fn open(
        target: &ConnectTarget,
        reset_on_connect: bool,
        log: &mut dyn FnMut(&str),
    ) -> Result<Link, HostError> {
        Link::open_until(target, reset_on_connect, log, &|| false)
    }

    /// [`Link::open`] that gives up with [`HostError::Cancelled`] once
    /// `cancelled` says so: auto-baud stops between (and during) its
    /// attempts.
    pub(crate) fn open_until(
        target: &ConnectTarget,
        reset_on_connect: bool,
        log: &mut dyn FnMut(&str),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Link, HostError> {
        match target {
            ConnectTarget::Serial { port, baud } => {
                Link::serial_until(port, *baud, reset_on_connect, log, cancelled)
            }
            ConnectTarget::Tcp { host, port } => Link::tcp(host, *port),
            ConnectTarget::Simulator(config) => Ok(Link::simulator(config.clone())),
        }
    }

    /// The control handle of a simulator link.
    pub fn sim_control(&self) -> Option<&SimControl> {
        self.control.sim()
    }

    /// Split into the reader half, the writer half and a handle that can
    /// close the link from another thread.
    pub fn split(self) -> (Box<dyn Read + Send>, Box<dyn Write + Send>, LinkHandle) {
        (
            self.reader,
            self.writer,
            LinkHandle {
                control: self.control,
            },
        )
    }
}

/// Closes a split [`Link`] and gives access to backend controls.
pub struct LinkHandle {
    control: LinkControl,
}

impl LinkHandle {
    /// Close the link so a blocked reader sees the end of input soon (shuts
    /// a simulator or TCP socket down, stops a serial port's I/O thread,
    /// which closes the port).
    pub fn close(&self) {
        match &self.control {
            LinkControl::None => {}
            LinkControl::Sim(c) => c.shutdown(),
            LinkControl::Tcp(s) => {
                let _ = s.shutdown(Shutdown::Both);
            }
            LinkControl::Serial(stop) => stop.store(true, Ordering::Release),
        }
    }

    /// The simulator control handle, if this is a simulator link.
    pub fn sim_control(&self) -> Option<&SimControl> {
        self.control.sim()
    }
}

impl LinkControl {
    fn sim(&self) -> Option<&SimControl> {
        match self {
            LinkControl::Sim(c) => Some(c),
            _ => None,
        }
    }
}

/// Whether a link read error only means "no data yet" (timeouts).
pub fn is_no_data(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn simulator_link_reads_the_banner_and_closes() {
        let link = Link::simulator(SimConfig {
            boot_delay_ms: 10,
            time_scale: 20.0,
            ..SimConfig::default()
        });
        assert!(link.sim_control().is_some());
        let (mut reader, _writer, handle) = link.split();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut text = Vec::new();
        let mut buf = [0u8; 256];
        while !text.starts_with(b"start\n") {
            assert!(Instant::now() < deadline, "no banner");
            match reader.read(&mut buf) {
                Ok(n) => text.extend_from_slice(&buf[..n]),
                Err(e) => assert!(is_no_data(&e), "{e}"),
            }
        }
        assert!(handle.sim_control().is_some_and(|c| c.is_running()));
        handle.close();
        loop {
            assert!(Instant::now() < deadline, "reader did not see the close");
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => assert!(is_no_data(&e), "{e}"),
            }
        }
    }

    #[test]
    fn custom_links_have_no_backend_controls() {
        let link = Link::new(io::empty(), io::sink());
        assert!(link.sim_control().is_none());
        let (_, _, handle) = link.split();
        handle.close();
        assert!(handle.sim_control().is_none());
    }
}
