//! Connecting without blocking the UI: [`Connector`] runs `connect` /
//! `disconnect` on worker threads (serial auto-baud probes for seconds),
//! [`PortScanner`] enumerates serial ports in the background (enumeration
//! only, ports are never opened), and [`LinkQuality`] grades the link from
//! the engine's statistics.

use std::collections::VecDeque;

use crossbeam_channel::{Receiver, Sender};
use sprint_host::{ConnectOptions, ConnectTarget, LinkStats, PortInfo, Printer};

/// Seconds between automatic port scans while disconnected.
pub const PORT_REFRESH_SECS: f64 = 3.0;

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
    /// The connection is closed.
    Closed,
}

/// Runs connects and disconnects on worker threads.
pub struct Connector {
    tx: Sender<ConnectOutcome>,
    rx: Receiver<ConnectOutcome>,
    connecting: Option<String>,
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
            repaint,
        }
    }

    /// Open `target` on a worker thread. Ignored while a connect is
    /// already running.
    pub fn connect(&mut self, printer: &Printer, target: ConnectTarget, options: ConnectOptions) {
        if self.connecting.is_some() {
            return;
        }
        let label = target.label();
        self.connecting = Some(label.clone());
        let printer = printer.clone();
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-connect".to_owned())
            .spawn(move || {
                let outcome = match printer.connect(target, options) {
                    Ok(()) => ConnectOutcome::Opened { label },
                    Err(e) => ConnectOutcome::Failed {
                        label,
                        error: e.to_string(),
                    },
                };
                // The worker's clone must go before the UI hears about
                // it, so a quick reconnect never races a stale handle.
                drop(printer);
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

    /// Close the connection on a worker thread (also stops a connect in
    /// progress: the engine gives up once the link opens).
    pub fn disconnect(&mut self, printer: &Printer) {
        let worker = printer.clone();
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-disconnect".to_owned())
            .spawn(move || {
                worker.disconnect();
                drop(worker);
                let _ = tx.send(ConnectOutcome::Closed);
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        if spawned.is_err() {
            // No thread: disconnect here (bounded to a couple of seconds).
            printer.disconnect();
            let _ = self.tx.send(ConnectOutcome::Closed);
        }
    }

    /// True while a connect worker runs.
    pub fn is_connecting(&self) -> bool {
        self.connecting.is_some()
    }

    /// What the workers reported since the last call.
    pub fn poll(&mut self) -> Vec<ConnectOutcome> {
        let mut out = Vec::new();
        while let Ok(outcome) = self.rx.try_recv() {
            if matches!(
                outcome,
                ConnectOutcome::Opened { .. } | ConnectOutcome::Failed { .. }
            ) {
                self.connecting = None;
            }
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
