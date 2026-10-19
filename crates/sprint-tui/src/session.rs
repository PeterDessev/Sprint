//! Blocking helpers for the line shell and the headless commands:
//! connecting and waiting for the printer.
//!
//! Every wait polls the printer state (never longer than a few
//! milliseconds at a time) and gives up on Ctrl+C ([`Interrupt`]) or a
//! timeout, so a silent printer cannot hang the program.

use std::time::{Duration, Instant};

use sprint_host::{
    ConnectOptions, ConnectTarget, ConnectionStatus, Heater, JobKind, Printer, PrinterState,
};

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

/// How long closing the link waits for a cancelled job to stop (the cancel
/// script parks the head).
pub const CANCEL_WAIT: Duration = Duration::from_secs(30);

/// Before closing the link while a job runs: cancel a print Sprint
/// streams (or an SD upload) and wait, at most `timeout`, for it to stop.
/// The cancel script turns the heaters off and parks; only closing the
/// link would leave the printer heating with nobody watching. `announce`
/// gets the job's name before the wait. A print from the printer's SD card
/// goes on by itself and is left alone. Returns the name of the job
/// cancelled.
pub fn cancel_before_closing(
    printer: &Printer,
    timeout: Duration,
    announce: impl FnOnce(&str),
) -> Option<String> {
    let name = printer.with_state(|s| {
        s.job
            .as_ref()
            .filter(|j| j.status.is_active() && j.kind != JobKind::SdPrint)
            .map(|j| j.name.clone())
    })?;
    announce(&name);
    printer.cancel();
    // Ctrl+C is not wanted here: the cancel script must run.
    let _ = wait_for(printer, timeout, &Interrupt::new(), true, |s| {
        !s.job_active()
    });
    Some(name)
}

/// How long [`heaters_off`] waits for the printer to report its heaters
/// off.
pub const HEATERS_OFF_WAIT: Duration = Duration::from_secs(10);

/// Turn every heater off (`M104 T<n> S0` for each tool, `M140 S0`, `M141
/// S0` when the printer has a chamber) and the part fan (`M107`), then
/// wait, at most `timeout`, until the printer reports every target at 0.
/// Use it before closing the link when a job ended without its cancel
/// script (a failure, or a cancel that was not waited for). Sending the
/// same commands again after a cancel script is harmless.
///
/// Returns whether the printer reported the heaters off. Nothing is sent
/// unless the printer is connected and not halted (a halted firmware has
/// switched them off itself and ignores commands); that returns false.
pub fn heaters_off(printer: &Printer, timeout: Duration) -> bool {
    let profile = printer.profile();
    let heaters = printer.with_state(|s| {
        (s.is_connected() && !s.halted).then(|| crate::actions::heaters(s, &profile))
    });
    let Some(heaters) = heaters else {
        return false;
    };
    for &heater in &heaters {
        printer.set_temperature(heater, 0.0);
    }
    printer.set_fan(0, 0);
    // Ctrl+C is not wanted here: the heaters must go off.
    wait_for(printer, timeout, &Interrupt::new(), true, |s| {
        heaters.iter().all(|&h| !heater_on(s, h))
    })
    .is_ok()
}

/// pronsole's `off`: every heater and fan off and the motors disabled
/// (`M84`), without waiting. Returns the heaters turned off.
pub fn everything_off(printer: &Printer) -> Vec<Heater> {
    let profile = printer.profile();
    let (heaters, fans) = printer.with_state(|s| {
        (
            crate::actions::heaters(s, &profile),
            s.fan_speeds.len().clamp(1, usize::from(u8::MAX)),
        )
    });
    for &heater in &heaters {
        printer.set_temperature(heater, 0.0);
    }
    for fan in (0..fans).filter_map(|f| u8::try_from(f).ok()) {
        printer.set_fan(fan, 0);
    }
    printer.motors_off();
    heaters
}

/// Whether `heater` reports a target above 0 °C.
pub fn heater_on(s: &PrinterState, heater: Heater) -> bool {
    let reading = match heater {
        Heater::Hotend(t) => s.temps.hotends.get(usize::from(t)),
        Heater::Bed => s.temps.bed.as_ref(),
        Heater::Chamber => s.temps.chamber.as_ref(),
    };
    reading
        .and_then(|r| r.target)
        .is_some_and(|t| t.is_finite() && t > 0.0)
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

/// How long a board reset may take until the printer answers again (an
/// 8-bit board boots in a few seconds, then the handshake and the setup
/// queries follow).
pub const RESET_WAIT: Duration = Duration::from_secs(30);

/// Why [`reset_board_blocking`] did not bring the printer back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResetError {
    /// The board cannot be reset from here (not connected, or a network
    /// link: its reset button or a power cycle is needed); the reason.
    Refused(String),
    /// The reset was started, but the printer did not come back.
    Wait(WaitError),
}

impl std::fmt::Display for ResetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResetError::Refused(why) => f.write_str(why),
            ResetError::Wait(WaitError::TimedOut) => {
                f.write_str("the printer did not answer after the reset")
            }
            ResetError::Wait(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ResetError {}

/// Reset the printer's board ([`Printer::reset_board`]: a DTR pulse on a
/// serial port, a reboot of the virtual printer), the way to recover from
/// an emergency stop or a kill() (M999 alone does not), and wait, at most
/// `timeout`, until the printer is back: handshaken again, not halted,
/// its setup queries answered.
pub fn reset_board_blocking(
    printer: &Printer,
    timeout: Duration,
    interrupt: &Interrupt,
) -> Result<(), ResetError> {
    let events = printer.subscribe();
    printer
        .reset_board()
        .map_err(|e| ResetError::Refused(e.to_string()))?;
    let deadline = Instant::now().checked_add(timeout);
    // The board has restarted once the link left the connected state
    // (the handshake runs again); before that the old state still shows.
    let mut restarted = false;
    loop {
        restarted |= events.try_iter().any(|event| {
            matches!(event, sprint_host::Event::Status(status) if status != ConnectionStatus::Connected)
        });
        let (back, closed) =
            printer.with_state(|s| (s.is_connected() && !s.halted, closed_reason(s)));
        if restarted && back {
            break;
        }
        if let Some(why) = closed {
            return Err(ResetError::Wait(WaitError::Disconnected(why)));
        }
        if interrupt.is_raised() {
            return Err(ResetError::Wait(WaitError::Interrupted));
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(ResetError::Wait(WaitError::TimedOut));
        }
        std::thread::sleep(POLL);
    }
    let left = deadline.map_or(Duration::MAX, |d| {
        d.saturating_duration_since(Instant::now())
    });
    wait_setup(printer, left, interrupt).map_err(ResetError::Wait)
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
    fn closing_the_link_cancels_a_streamed_print_first() {
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        // Moves in (fast) real time: still printing when the link closes.
        let sim = ConnectTarget::Simulator(SimConfig {
            boot_delay_ms: 20,
            time_scale: 5.0,
            ..SimConfig::default()
        });
        connect_blocking(&printer, sim, ConnectOptions::default(), &interrupt).expect("connect");
        wait_setup(&printer, Duration::from_secs(30), &interrupt).expect("setup");
        // Nothing runs: nothing to cancel.
        assert_eq!(cancel_before_closing(&printer, CANCEL_WAIT, |_| {}), None);
        let mut gcode = String::from(
            "M104 S200
G28
G1 Z0.3 F600
",
        );
        for i in 0..400 {
            gcode.push_str(&format!(
                "G1 X{} Y20 F600
",
                20 + (i % 2) * 5
            ));
        }
        let file = std::sync::Arc::new(sprint_host::sprint_gcode::GcodeFile::from_text(
            "slow.gcode",
            &gcode,
        ));
        printer
            .start_print(file, Default::default())
            .expect("start");
        wait_for(&printer, Duration::from_secs(30), &interrupt, true, |s| {
            s.job.as_ref().is_some_and(|j| j.current_line > 5)
        })
        .expect("printing");
        let control = printer.sim_control().expect("simulator");
        control.clear_received();
        let mut announced = String::new();
        let cancelled = cancel_before_closing(&printer, CANCEL_WAIT, |name| {
            announced = name.to_string();
        });
        assert_eq!(cancelled.as_deref(), Some("slow.gcode"));
        assert_eq!(announced, "slow.gcode");
        assert!(!printer.with_state(PrinterState::job_active));
        // The cancel script turned the heaters off.
        let got = control.received_commands();
        assert!(got.iter().any(|c| c == "M104 S0"), "{got:?}");
        printer.disconnect();
    }

    #[test]
    fn heaters_off_turns_every_heater_off_and_waits_for_the_report() {
        // Disconnected: nothing to send.
        assert!(!heaters_off(&Printer::new(), Duration::from_secs(1)));
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        connect_blocking(&printer, fast_sim(), ConnectOptions::default(), &interrupt)
            .expect("connect");
        wait_setup(&printer, Duration::from_secs(30), &interrupt).expect("setup");
        printer.send("M104 S200\nM140 S60\nM106 S255");
        wait_for(&printer, Duration::from_secs(30), &interrupt, true, |s| {
            heater_on(s, Heater::Hotend(0)) && heater_on(s, Heater::Bed)
        })
        .expect("heating");
        let control = printer.sim_control().expect("simulator");
        assert!(heaters_off(&printer, HEATERS_OFF_WAIT));
        assert!(
            printer.with_state(|s| !heater_on(s, Heater::Hotend(0)) && !heater_on(s, Heater::Bed))
        );
        let sim = control.snapshot();
        assert_eq!(sim.hotend_targets, [0.0]);
        assert_eq!(sim.bed_target, 0.0);
        assert_eq!(sim.fan, 0);
        let got = control.received_commands();
        for want in ["M104 T0 S0", "M140 S0", "M107"] {
            assert!(got.iter().any(|c| c == want), "missing {want} in {got:?}");
        }
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

    #[test]
    fn a_board_reset_recovers_from_an_emergency_stop() {
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        // Not connected: nothing to reset.
        let err = reset_board_blocking(&printer, RESET_WAIT, &interrupt).expect_err("offline");
        assert!(matches!(err, ResetError::Refused(_)), "{err:?}");
        connect_blocking(&printer, fast_sim(), ConnectOptions::default(), &interrupt)
            .expect("connect");
        wait_setup(&printer, Duration::from_secs(30), &interrupt).expect("setup");
        printer.emergency_stop();
        wait_for(&printer, Duration::from_secs(30), &interrupt, true, |s| {
            s.halted
        })
        .expect("halted");
        // M112 is a kill(): only a reset brings the board back.
        assert!(!printer.with_state(|s| s.restartable));
        reset_board_blocking(&printer, RESET_WAIT, &interrupt).expect("reset");
        assert!(printer.with_state(|s| s.is_connected() && !s.halted));
        // The printer answers again.
        printer.send("G28");
        wait_idle(&printer, Duration::from_secs(30), &interrupt).expect("idle");
        // A reset of a printer that was fine waits for it to come back
        // too, not just for the old state.
        reset_board_blocking(&printer, RESET_WAIT, &interrupt).expect("reset again");
        assert!(printer.with_state(PrinterState::is_connected));
        printer.disconnect();
        // A network link cannot reset the board; the reason says so.
        let refused = ResetError::Refused("press its reset button".into());
        assert_eq!(refused.to_string(), "press its reset button");
        assert_eq!(
            ResetError::Wait(WaitError::TimedOut).to_string(),
            "the printer did not answer after the reset"
        );
    }
}
