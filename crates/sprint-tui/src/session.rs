//! Blocking helpers for the line shell and the headless commands:
//! connecting and waiting for the printer.
//!
//! Every wait polls the printer state (never longer than a few
//! milliseconds at a time) and gives up on Ctrl+C ([`Interrupt`]) or a
//! timeout, so a silent printer cannot hang the program.

use std::time::{Duration, Instant};

use sprint_host::{ConnectOptions, ConnectTarget, ConnectionStatus, Printer, PrinterState};

use crate::interrupt::Interrupt;

/// How often waits look at the printer.
pub const POLL: Duration = Duration::from_millis(10);

/// Extra time beyond the handshake timeout before a connect gives up.
const CONNECT_GRACE: Duration = Duration::from_secs(5);

/// How long the link must stay idle to count as idle.
pub const IDLE_SETTLE: Duration = Duration::from_millis(250);

/// Why a wait ended without what it waited for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitError {
    /// Ctrl+C.
    Interrupted,
    /// The time ran out.
    TimedOut,
    /// The connection failed or closed (with the reason when known).
    Disconnected(String),
}

impl std::fmt::Display for WaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaitError::Interrupted => f.write_str("interrupted"),
            WaitError::TimedOut => f.write_str("timed out"),
            WaitError::Disconnected(why) if why.is_empty() => f.write_str("disconnected"),
            WaitError::Disconnected(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for WaitError {}

/// Poll `cond` on the printer state until it holds, Ctrl+C, the timeout
/// (`Duration::MAX` waits for ever), or (with `need_link`) the link
/// closing.
pub fn wait_for(
    printer: &Printer,
    timeout: Duration,
    interrupt: &Interrupt,
    need_link: bool,
    mut cond: impl FnMut(&PrinterState) -> bool,
) -> Result<(), WaitError> {
    let deadline = Instant::now().checked_add(timeout);
    loop {
        let (done, closed) = printer.with_state(|s| (cond(s), closed_reason(s)));
        if done {
            return Ok(());
        }
        if need_link && let Some(why) = closed {
            return Err(WaitError::Disconnected(why));
        }
        if interrupt.is_raised() {
            return Err(WaitError::Interrupted);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(WaitError::TimedOut);
        }
        std::thread::sleep(POLL);
    }
}

/// Why the link is closed, if it is.
fn closed_reason(s: &PrinterState) -> Option<String> {
    match &s.status {
        ConnectionStatus::Failed(why) => Some(why.clone()),
        ConnectionStatus::Disconnected => Some(String::new()),
        _ => None,
    }
}

/// How long to wait for the handshake: the configured seconds (a broken
/// value, even NaN from a hand-edited file, falls back to the default).
fn handshake_wait(secs: f32) -> Duration {
    let secs = if secs.is_finite() {
        secs.clamp(1.0, 600.0)
    } else {
        ConnectOptions::default().handshake_timeout_secs
    };
    Duration::from_secs_f32(secs) + CONNECT_GRACE
}

/// Why a connect failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// The link could not be opened (the printer logged why on its
    /// console as well).
    Open(String),
    /// The link opened but the printer did not complete the handshake.
    Handshake(String),
    /// Ctrl+C while connecting.
    Interrupted(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Open(why)
            | ConnectError::Handshake(why)
            | ConnectError::Interrupted(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Connect and wait for the handshake. Opening a serial port with
/// auto-baud can take seconds; the handshake gets its configured timeout
/// plus a few seconds. On failure the printer is left disconnected.
pub fn connect_blocking(
    printer: &Printer,
    target: ConnectTarget,
    options: ConnectOptions,
    interrupt: &Interrupt,
) -> Result<(), ConnectError> {
    let label = target.label();
    let handshake = handshake_wait(options.handshake_timeout_secs);
    printer
        .connect(target, options)
        .map_err(|e| ConnectError::Open(format!("cannot connect to {label}: {e}")))?;
    match wait_for(
        printer,
        handshake,
        interrupt,
        true,
        PrinterState::is_connected,
    ) {
        Ok(()) => Ok(()),
        Err(e) => {
            printer.disconnect();
            Err(match e {
                WaitError::Interrupted => {
                    ConnectError::Interrupted(format!("connecting to {label} was interrupted"))
                }
                WaitError::TimedOut => {
                    ConnectError::Handshake(format!("{label} did not answer the handshake"))
                }
                WaitError::Disconnected(why) if why.is_empty() => {
                    ConnectError::Handshake(format!("{label} closed the connection"))
                }
                WaitError::Disconnected(why) => ConnectError::Handshake(why),
            })
        }
    }
}

/// Whether nothing waits to be sent or acknowledged.
pub fn link_idle(s: &PrinterState) -> bool {
    s.link.queue_len == 0 && s.link.in_flight == 0
}

/// Wait until the link has been idle for [`IDLE_SETTLE`]: everything
/// queued was sent and acknowledged.
pub fn wait_idle(
    printer: &Printer,
    timeout: Duration,
    interrupt: &Interrupt,
) -> Result<(), WaitError> {
    let mut idle_since: Option<Instant> = None;
    wait_for(printer, timeout, interrupt, true, |s| {
        if link_idle(s) {
            let since = *idle_since.get_or_insert_with(Instant::now);
            since.elapsed() >= IDLE_SETTLE
        } else {
            idle_since = None;
            false
        }
    })
}

/// After a connect: wait (at most `timeout`) for the setup queries (M115,
/// M503, M114) to be answered, so their replies do not mix with what comes
/// next. The engine sends them right after the handshake, so the link
/// stays busy until the last one is acknowledged.
pub fn wait_setup(
    printer: &Printer,
    timeout: Duration,
    interrupt: &Interrupt,
) -> Result<(), WaitError> {
    wait_idle(printer, timeout, interrupt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::sprint_sim::SimConfig;

    fn fast_sim() -> ConnectTarget {
        ConnectTarget::Simulator(SimConfig {
            boot_delay_ms: 20,
            time_scale: 100.0,
            instant_moves: true,
            ..SimConfig::default()
        })
    }

    #[test]
    fn connects_to_the_simulator_and_waits_for_the_setup() {
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        connect_blocking(&printer, fast_sim(), ConnectOptions::default(), &interrupt)
            .expect("connect");
        assert!(printer.is_connected());
        wait_setup(&printer, Duration::from_secs(30), &interrupt).expect("setup");
        assert!(printer.with_state(|s| s.firmware.is_some() && s.settings_loaded));
        printer.send("G28");
        wait_idle(&printer, Duration::from_secs(30), &interrupt).expect("idle");
        assert!(printer.with_state(link_idle));
        // Already connected: a second connect fails to open.
        let again = connect_blocking(&printer, fast_sim(), ConnectOptions::default(), &interrupt);
        assert!(matches!(again, Err(ConnectError::Open(_))), "{again:?}");
        printer.disconnect();
    }

    #[test]
    fn handshake_waits_survive_broken_settings() {
        assert_eq!(handshake_wait(15.0), Duration::from_secs(20));
        assert_eq!(handshake_wait(0.0), Duration::from_secs(6));
        assert_eq!(handshake_wait(1e9), Duration::from_secs(605));
        let default = handshake_wait(ConnectOptions::default().handshake_timeout_secs);
        assert_eq!(handshake_wait(f32::NAN), default);
        assert_eq!(handshake_wait(f32::NEG_INFINITY), default);
    }

    #[test]
    fn waits_end_on_ctrl_c_timeouts_and_closed_links() {
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        // Disconnected: a wait that needs the link ends at once.
        assert_eq!(
            wait_for(&printer, Duration::from_secs(5), &interrupt, true, |_| {
                false
            }),
            Err(WaitError::Disconnected(String::new()))
        );
        assert_eq!(
            wait_for(
                &printer,
                Duration::from_millis(30),
                &interrupt,
                false,
                |_| false
            ),
            Err(WaitError::TimedOut)
        );
        interrupt.raise();
        assert_eq!(
            wait_for(&printer, Duration::MAX, &interrupt, false, |_| false),
            Err(WaitError::Interrupted)
        );
        assert_eq!(
            wait_for(&printer, Duration::MAX, &interrupt, false, |_| true),
            Ok(())
        );
        assert_eq!(
            WaitError::Disconnected(String::new()).to_string(),
            "disconnected"
        );
        assert_eq!(WaitError::TimedOut.to_string(), "timed out");
    }

    #[test]
    fn an_unreachable_address_fails_to_open() {
        // Port 1 on loopback refuses connections; nothing leaves the machine.
        let printer = Printer::new();
        let target = ConnectTarget::parse("tcp://127.0.0.1:1").expect("target");
        let err = connect_blocking(
            &printer,
            target,
            ConnectOptions::default(),
            &Interrupt::new(),
        )
        .expect_err("refused");
        assert!(matches!(err, ConnectError::Open(_)), "{err:?}");
        assert!(err.to_string().starts_with("cannot connect to"), "{err}");
        assert!(!printer.with_state(|s| s.status.is_open()));
    }

    #[test]
    fn ctrl_c_interrupts_a_handshake() {
        // A simulator that boots slowly: the handshake is still running
        // when Ctrl+C arrives.
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        let slow = ConnectTarget::Simulator(SimConfig {
            boot_delay_ms: 60_000,
            time_scale: 1.0,
            ..SimConfig::default()
        });
        let raiser = interrupt.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            raiser.raise();
        });
        let err = connect_blocking(&printer, slow, ConnectOptions::default(), &interrupt)
            .expect_err("interrupted");
        let _ = thread.join();
        assert!(matches!(err, ConnectError::Interrupted(_)), "{err:?}");
        assert!(!printer.with_state(|s| s.status.is_open()));
    }
}
