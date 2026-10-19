//! What the headless printer commands (`print`, `send`) share: connecting
//! with Ctrl+C as a way out, and the exit code that reports it.

use std::time::Duration;

use anyhow::{Result, anyhow};
use sprint_host::{ConnectOptions, ConnectTarget, Printer};
use sprint_tui::interrupt::Interrupt;
use sprint_tui::session::{self, ConnectError, WaitError};

/// Exit code after Ctrl+C.
pub const EXIT_INTERRUPTED: u8 = 130;

/// How long the setup queries after connecting may take.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);

/// How connecting ended, when it did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connected {
    /// Connected, and the setup queries (M115, M503, M114) were answered
    /// (or took too long), so their replies do not mix with what follows.
    Ready,
    /// Ctrl+C while connecting; the printer is disconnected again.
    Interrupted,
}

/// Connect to `target` and wait for the printer's setup. A failure to
/// connect is an error; Ctrl+C is [`Connected::Interrupted`].
pub fn connect(
    printer: &Printer,
    target: ConnectTarget,
    options: ConnectOptions,
    interrupt: &Interrupt,
) -> Result<Connected> {
    let label = target.label();
    match session::connect_blocking(printer, target, options, interrupt) {
        Ok(()) => {}
        Err(ConnectError::Interrupted(_)) => return Ok(Connected::Interrupted),
        Err(e) => return Err(anyhow!("{e}")),
    }
    match session::wait_setup(printer, SETUP_TIMEOUT, interrupt) {
        Ok(()) | Err(WaitError::TimedOut) => Ok(Connected::Ready),
        Err(WaitError::Interrupted) => {
            printer.disconnect();
            Ok(Connected::Interrupted)
        }
        Err(WaitError::Disconnected(why)) => Err(anyhow!(closed(&label, &why))),
    }
}

/// "<label> closed the connection (reason)".
pub fn closed(label: &str, why: &str) -> String {
    if why.is_empty() {
        format!("{label} closed the connection")
    } else {
        format!("{label} closed the connection ({why})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::sprint_sim::SimConfig;

    #[test]
    fn connects_to_the_simulator() {
        let printer = Printer::new();
        let target = ConnectTarget::Simulator(SimConfig {
            boot_delay_ms: 20,
            time_scale: 100.0,
            instant_moves: true,
            ..SimConfig::default()
        });
        let got = connect(
            &printer,
            target,
            ConnectOptions::default(),
            &Interrupt::new(),
        )
        .expect("connect");
        assert_eq!(got, Connected::Ready);
        assert!(printer.with_state(|s| s.firmware.is_some()));
        printer.disconnect();
    }

    #[test]
    fn ctrl_c_while_connecting_is_not_a_failure() {
        // A simulator that boots slowly: still handshaking when Ctrl+C
        // arrives.
        let printer = Printer::new();
        let target = ConnectTarget::Simulator(SimConfig {
            boot_delay_ms: 60_000,
            time_scale: 1.0,
            ..SimConfig::default()
        });
        let interrupt = Interrupt::new();
        let raiser = interrupt.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            raiser.raise();
        });
        let got = connect(&printer, target, ConnectOptions::default(), &interrupt);
        let _ = thread.join();
        assert_eq!(got.ok(), Some(Connected::Interrupted));
        assert!(!printer.with_state(|s| s.status.is_open()));
    }

    #[test]
    fn an_unreachable_printer_is_an_error() {
        // Port 1 on loopback refuses connections; nothing leaves the
        // machine.
        let printer = Printer::new();
        let target = ConnectTarget::parse("tcp://127.0.0.1:1").expect("target");
        let err = connect(
            &printer,
            target,
            ConnectOptions::default(),
            &Interrupt::new(),
        )
        .expect_err("refused");
        assert!(err.to_string().starts_with("cannot connect to"), "{err}");
        assert_eq!(closed("sim", ""), "sim closed the connection");
        assert_eq!(closed("sim", "reset"), "sim closed the connection (reset)");
    }
}
