//! Connecting without blocking the UI: [`Connector`] runs `connect` /
//! `disconnect` on worker threads (serial auto-baud probes for seconds),
//! [`PortScanner`] enumerates serial ports in the background (enumeration
//! only, ports are never opened), and [`LinkQuality`] grades the link from
//! the engine's statistics.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use sprint_host::{ConnectOptions, ConnectTarget, JobKind, LinkStats, PortInfo, Printer};

/// Seconds between automatic port scans while disconnected.
pub const PORT_REFRESH_SECS: f64 = 3.0;

/// How long closing the link waits, at most, for a cancelled job to stop
/// (the cancel script parks the head and turns the heaters off).
pub const CANCEL_WAIT: Duration = Duration::from_secs(30);

/// The job [`close_link`] cancels before closing the link: a print Sprint
/// streams or an SD upload (a print from the printer's own card goes on by
/// itself and is left alone).
pub fn job_to_cancel(state: &sprint_host::PrinterState) -> Option<(&str, JobKind)> {
    state
        .job
        .as_ref()
        .filter(|j| j.status.is_active() && j.kind != JobKind::SdPrint)
        .map(|j| (j.name.as_str(), j.kind))
}

/// Close the link safely: a job the host runs (see [`job_to_cancel`]) is
/// cancelled first and the link stays open, at most `timeout`, until it has
/// stopped. Only closing the link would leave the firmware heating with
/// nobody watching (a host print), or writing everything that comes next
/// into a half-written SD file (an upload). Blocks: call it from a worker
/// thread (or on exit). Returns the name of the job cancelled.
pub fn close_link(printer: &Printer, timeout: Duration) -> Option<String> {
    let cancelled = cancel_and_wait(printer, timeout);
    printer.disconnect();
    cancelled
}

/// The first half of [`close_link`]: cancel the host's job and wait (at
/// most `timeout`) until it stopped or the link closed by itself.
fn cancel_and_wait(printer: &Printer, timeout: Duration) -> Option<String> {
    let name = printer.with_state(|s| job_to_cancel(s).map(|(name, _)| name.to_owned()))?;
    printer.cancel();
    let deadline = Instant::now() + timeout;
    loop {
        let (active, open) = printer.with_state(|s| (s.job_active(), s.status.is_open()));
        if !active || !open {
            break;
        }
        if Instant::now() >= deadline {
            log::warn!("{name} did not stop within {timeout:?}; closing the link anyway");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Some(name)
}

/// What a connect or disconnect worker reported.
#[derive(Clone, Debug, PartialEq)]
pub enum ConnectOutcome {
    /// The link is open; the handshake continues in the engine.
    Opened {
        /// The target's label.
        label: String,
    },
    /// Opening the link failed.
    Failed {
        /// The target's label.
        label: String,
        /// Why.
        error: String,
    },
    /// A connect that was cancelled (by [`Connector::disconnect`]) has
    /// ended. Not an error; `opened` when the link opened all the same (the
    /// cancel came too early for the engine to notice) and has to be
    /// closed again.
    Cancelled {
        /// The target's label.
        label: String,
        /// The link is open.
        opened: bool,
    },
    /// The connection is closed.
    Closed,
}

/// Runs connects and disconnects on worker threads.
pub struct Connector {
    tx: Sender<ConnectOutcome>,
    rx: Receiver<ConnectOutcome>,
    connecting: Option<String>,
    /// The running connect was cancelled.
    cancelled: bool,
    /// Disconnect workers that have not reported yet.
    disconnecting: usize,
    repaint: Option<egui::Context>,
}

impl Connector {
    /// A connector that requests a repaint when a worker finishes.
    pub fn new(repaint: Option<egui::Context>) -> Connector {
        let (tx, rx) = crossbeam_channel::unbounded();
        Connector {
            tx,
            rx,
            connecting: None,
            cancelled: false,
            disconnecting: 0,
            repaint,
        }
    }

    /// Open `target` on a worker thread. Ignored while a connect is
    /// already running.
    pub fn connect(&mut self, printer: &Printer, target: ConnectTarget, options: ConnectOptions) {
        let printer = printer.clone();
        self.spawn_connect(target.label(), move || {
            let result = printer.connect(target, options).map_err(|e| e.to_string());
            // The worker's clone must go before the UI hears about it, so a
            // quick reconnect never races a stale handle.
            drop(printer);
            result
        });
    }

    /// Run `open` on a worker thread as the connect to `label`.
    fn spawn_connect(
        &mut self,
        label: String,
        open: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        if self.connecting.is_some() {
            return;
        }
        self.connecting = Some(label.clone());
        self.cancelled = false;
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-connect".to_owned())
            .spawn(move || {
                let outcome = match open() {
                    Ok(()) => ConnectOutcome::Opened { label },
                    Err(error) => ConnectOutcome::Failed { label, error },
                };
                let _ = tx.send(outcome);
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            let label = self.connecting.take().unwrap_or_default();
            let _ = self.tx.send(ConnectOutcome::Failed {
                label,
                error: format!("could not start a worker thread: {e}"),
            });
        }
    }

    /// Close the connection on a worker thread, safely (see
    /// [`close_link`]): a print the host streams, or an SD upload, is
    /// cancelled first and the link closes once it has stopped. A connect
    /// in progress is cancelled: the engine gives up once the link opens,
    /// and the connect reports [`ConnectOutcome::Cancelled`] instead of a
    /// failure.
    pub fn disconnect(&mut self, printer: &Printer) {
        if self.connecting.is_some() {
            self.cancelled = true;
        }
        self.disconnecting += 1;
        let worker = printer.clone();
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-disconnect".to_owned())
            .spawn(move || {
                close_link(&worker, CANCEL_WAIT);
                drop(worker);
                let _ = tx.send(ConnectOutcome::Closed);
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        if spawned.is_err() {
            // No thread: close here. Waiting blocks the window, but a job
            // left running would keep the heaters on unattended.
            close_link(printer, CANCEL_WAIT);
            let _ = self.tx.send(ConnectOutcome::Closed);
        }
    }

    /// True while a disconnect worker runs (it may be waiting for a
    /// cancelled job to stop).
    pub fn is_closing(&self) -> bool {
        self.disconnecting > 0
    }

    /// True while a connect worker runs.
    pub fn is_connecting(&self) -> bool {
        self.connecting.is_some()
    }

    /// True while a cancelled connect is still running.
    pub fn is_cancelling(&self) -> bool {
        self.connecting.is_some() && self.cancelled
    }

    /// True when no connect or disconnect worker runs.
    pub fn is_idle(&self) -> bool {
        self.connecting.is_none() && self.disconnecting == 0
    }

    /// What the workers reported since the last call.
    pub fn poll(&mut self) -> Vec<ConnectOutcome> {
        let mut out = Vec::new();
        while let Ok(outcome) = self.rx.try_recv() {
            let outcome = match outcome {
                ConnectOutcome::Opened { label } => {
                    self.connecting = None;
                    if std::mem::take(&mut self.cancelled) {
                        ConnectOutcome::Cancelled {
                            label,
                            opened: true,
                        }
                    } else {
                        ConnectOutcome::Opened { label }
                    }
                }
                ConnectOutcome::Failed { label, error } => {
                    self.connecting = None;
                    if std::mem::take(&mut self.cancelled) {
                        ConnectOutcome::Cancelled {
                            label,
                            opened: false,
                        }
                    } else {
                        ConnectOutcome::Failed { label, error }
                    }
                }
                ConnectOutcome::Closed => {
                    self.disconnecting = self.disconnecting.saturating_sub(1);
                    ConnectOutcome::Closed
                }
                cancelled @ ConnectOutcome::Cancelled { .. } => cancelled,
            };
            out.push(outcome);
        }
        out
    }
}

/// Background serial port enumeration (never opens a port).
pub struct PortScanner {
    ports: Vec<PortInfo>,
    rx: Option<Receiver<Vec<PortInfo>>>,
    last_scan: Option<f64>,
    enabled: bool,
    repaint: Option<egui::Context>,
}

impl PortScanner {
    /// A scanner; a disabled one never enumerates (tests, snapshots).
    pub fn new(enabled: bool, repaint: Option<egui::Context>) -> PortScanner {
        PortScanner {
            ports: Vec::new(),
            rx: None,
            last_scan: None,
            enabled,
            repaint,
        }
    }

    /// The ports found by the last scan (likely printers first).
    pub fn ports(&self) -> &[PortInfo] {
        &self.ports
    }

    /// True while a scan runs.
    pub fn is_scanning(&self) -> bool {
        self.rx.is_some()
    }

    /// Start a scan now unless one is running.
    pub fn request(&mut self, now: f64) {
        if !self.enabled || self.rx.is_some() {
            return;
        }
        self.last_scan = Some(now);
        let (tx, rx) = crossbeam_channel::bounded(1);
        let repaint = self.repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-ports".to_owned())
            .spawn(move || {
                let _ = tx.send(sprint_host::list_ports());
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        if spawned.is_ok() {
            self.rx = Some(rx);
        }
    }

    /// Collect a finished scan, and start a new one every
    /// [`PORT_REFRESH_SECS`] while `auto` (disconnected).
    pub fn poll(&mut self, now: f64, auto: bool) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(ports) => {
                    self.ports = ports;
                    self.rx = None;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.rx = None,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        }
        let due = self
            .last_scan
            .is_none_or(|t| now - t >= PORT_REFRESH_SECS || now < t);
        if auto && due {
            self.request(now);
        }
    }

    /// Replace the port list (tests and demos).
    pub fn set_ports(&mut self, ports: Vec<PortInfo>) {
        self.ports = ports;
    }
}

/// One link statistics sample.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Sample {
    t: f64,
    lines: u64,
    bad: u64,
    timeouts: u64,
}

/// Grades the link from the last few seconds of [`LinkStats`].
#[derive(Clone, Debug, Default)]
pub struct LinkQuality {
    samples: VecDeque<Sample>,
}

impl LinkQuality {
    /// Seconds of history the grade looks at.
    pub const WINDOW_SECS: f64 = 10.0;

    /// Record the current statistics (`connected` false clears the
    /// history).
    pub fn update(&mut self, link: &LinkStats, connected: bool) {
        if !connected {
            self.samples.clear();
            return;
        }
        let sample = Sample {
            t: link.connected_secs,
            lines: link.lines_sent,
            bad: link.resends.saturating_add(link.errors),
            timeouts: link.timeouts,
        };
        if let Some(last) = self.samples.back()
            && (sample.t < last.t || sample.lines < last.lines)
        {
            // A new connection: start over.
            self.samples.clear();
        }
        if self
            .samples
            .back()
            .is_none_or(|last| sample.t - last.t >= 0.25)
        {
            self.samples.push_back(sample);
        }
        while self
            .samples
            .front()
            .is_some_and(|s| sample.t - s.t > Self::WINDOW_SECS)
        {
            self.samples.pop_front();
        }
    }

    /// Bars to show (0..=4) for the recent window and the average RTT.
    pub fn level(&self, rtt_avg_ms: f32) -> u8 {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return 0;
        };
        quality_level(
            last.lines.saturating_sub(first.lines),
            last.bad.saturating_sub(first.bad),
            last.timeouts.saturating_sub(first.timeouts),
            rtt_avg_ms,
        )
    }
}

/// The link grade: 4 = clean, 3 = rare resends, 2 = frequent resends or
/// slow replies, 1 = timeouts or a bad link.
pub fn quality_level(lines: u64, bad: u64, timeouts: u64, rtt_avg_ms: f32) -> u8 {
    let mut level = if bad == 0 {
        4
    } else {
        let per_100 = bad as f64 * 100.0 / lines.max(1) as f64;
        if per_100 < 1.0 {
            3
        } else if per_100 < 5.0 {
            2
        } else {
            1
        }
    };
    if rtt_avg_ms.is_finite() && rtt_avg_ms > 250.0 {
        level = level.min(2);
    }
    if timeouts > 0 {
        level = 1;
    }
    level
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use sprint_host::sprint_sim::SimConfig;

    use super::*;

    fn wait_for(
        connector: &mut Connector,
        what: impl Fn(&ConnectOutcome) -> bool,
    ) -> ConnectOutcome {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(o) = connector.poll().into_iter().find(|o| what(o)) {
                return o;
            }
            assert!(Instant::now() < deadline, "no outcome");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn connects_to_the_simulator_and_disconnects_off_thread() {
        let printer = Printer::new();
        let mut connector = Connector::new(None);
        let sim = SimConfig {
            boot_delay_ms: 5,
            ..SimConfig::default()
        };
        connector.connect(
            &printer,
            ConnectTarget::Simulator(sim),
            ConnectOptions::default(),
        );
        assert!(connector.is_connecting());
        // A second request while connecting is ignored.
        connector.connect(
            &printer,
            ConnectTarget::Simulator(SimConfig::default()),
            ConnectOptions::default(),
        );
        let opened = wait_for(&mut connector, |o| {
            matches!(o, ConnectOutcome::Opened { .. })
        });
        assert_eq!(
            opened,
            ConnectOutcome::Opened {
                label: "Virtual printer".into()
            }
        );
        assert!(!connector.is_connecting());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !printer.is_connected() {
            assert!(Instant::now() < deadline, "handshake");
            std::thread::sleep(Duration::from_millis(5));
        }
        connector.disconnect(&printer);
        wait_for(&mut connector, |o| *o == ConnectOutcome::Closed);
        assert!(!printer.is_connected());
    }

    #[test]
    fn failures_are_reported() {
        let printer = Printer::new();
        let mut connector = Connector::new(None);
        // Nothing listens on port 1 of the loopback interface.
        connector.connect(
            &printer,
            ConnectTarget::Tcp {
                host: "127.0.0.1".into(),
                port: 1,
            },
            ConnectOptions::default(),
        );
        let failed = wait_for(&mut connector, |o| {
            matches!(o, ConnectOutcome::Failed { .. })
        });
        let ConnectOutcome::Failed { label, error } = failed else {
            unreachable!()
        };
        assert_eq!(label, "tcp://127.0.0.1:1");
        assert!(!error.is_empty());
        assert!(!connector.is_connecting());
    }

    #[test]
    fn a_cancelled_connect_reports_a_cancel_not_a_failure() {
        // Never connected: its disconnect is a no-op.
        let printer = Printer::new();
        let mut connector = Connector::new(None);
        assert!(connector.is_idle());
        // A slow connect (serial auto-baud probing), cancelled meanwhile.
        let (release, wait) = crossbeam_channel::bounded::<()>(1);
        connector.spawn_connect("COM9".into(), move || {
            let _ = wait.recv_timeout(Duration::from_secs(20));
            Err("not connected".into())
        });
        connector.disconnect(&printer);
        assert!(connector.is_cancelling());
        wait_for(&mut connector, |o| *o == ConnectOutcome::Closed);
        assert!(!connector.is_idle(), "the connect still runs");
        let _ = release.send(());
        let ended = wait_for(&mut connector, |o| {
            matches!(o, ConnectOutcome::Cancelled { .. })
        });
        assert_eq!(
            ended,
            ConnectOutcome::Cancelled {
                label: "COM9".into(),
                opened: false
            }
        );
        assert!(connector.is_idle());
        assert!(!connector.is_cancelling());

        // The cancel came before the link opened: it says so.
        connector.spawn_connect("sim".into(), || Ok(()));
        connector.disconnect(&printer);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        while !connector.is_idle() {
            assert!(Instant::now() < deadline, "workers never finished");
            seen.extend(connector.poll());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(seen.contains(&ConnectOutcome::Closed), "{seen:?}");
        assert!(
            seen.contains(&ConnectOutcome::Cancelled {
                label: "sim".into(),
                opened: true
            }),
            "{seen:?}"
        );

        // Without a cancel a failure stays a failure.
        connector.spawn_connect("tcp".into(), || Err("refused".into()));
        let failed = wait_for(&mut connector, |o| {
            matches!(o, ConnectOutcome::Failed { .. })
        });
        assert_eq!(
            failed,
            ConnectOutcome::Failed {
                label: "tcp".into(),
                error: "refused".into()
            }
        );
        assert!(connector.is_idle());
    }

    #[test]
    fn disabled_scanner_never_enumerates() {
        let mut scanner = PortScanner::new(false, None);
        scanner.poll(0.0, true);
        scanner.request(1.0);
        assert!(!scanner.is_scanning());
        assert!(scanner.ports().is_empty());
        scanner.set_ports(vec![PortInfo {
            name: "COM9".into(),
            description: "Demo".into(),
            vid: None,
            pid: None,
            manufacturer: None,
            serial_number: None,
            likely_printer: false,
        }]);
        assert_eq!(scanner.ports().len(), 1);
    }

    #[test]
    fn quality_grades() {
        assert_eq!(quality_level(0, 0, 0, 0.0), 4);
        assert_eq!(quality_level(1000, 5, 0, 3.0), 3);
        assert_eq!(quality_level(1000, 20, 0, 3.0), 2);
        assert_eq!(quality_level(100, 30, 0, 3.0), 1);
        assert_eq!(quality_level(1000, 0, 0, 400.0), 2);
        assert_eq!(quality_level(1000, 0, 1, 3.0), 1);
        assert_eq!(quality_level(0, 3, 0, f32::NAN), 1);
    }

    #[test]
    fn quality_window_slides_and_resets() {
        let mut q = LinkQuality::default();
        assert_eq!(q.level(1.0), 0);
        let mut link = LinkStats::default();
        for i in 0..40 {
            link.connected_secs = f64::from(i);
            link.lines_sent = i as u64 * 100;
            q.update(&link, true);
        }
        assert_eq!(q.level(2.0), 4);
        // Resends in the window lower the grade...
        link.resends = 50;
        link.connected_secs = 40.0;
        link.lines_sent = 4000;
        q.update(&link, true);
        assert!(q.level(2.0) < 4);
        // ...and age out after the window.
        for i in 41..60 {
            link.connected_secs = f64::from(i);
            link.lines_sent = i as u64 * 100;
            q.update(&link, true);
        }
        assert_eq!(q.level(2.0), 4);
        q.update(&link, false);
        assert_eq!(q.level(2.0), 0);
    }
}
