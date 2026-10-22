//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A fresh directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("sprint-host-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    ConnectOptions, ConnectTarget, ConsoleEntry, Delivery, Event, Printer, PrinterState, Tracked,
};

/// Generous upper bound for anything a test waits for.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// A fast simulator: short boot, instant moves, accelerated time.
pub fn fast_sim(time_scale: f32) -> SimConfig {
    SimConfig {
        boot_delay_ms: 20,
        time_scale,
        instant_moves: true,
        ..SimConfig::default()
    }
}

/// Options for tests: short timeouts.
pub fn options() -> ConnectOptions {
    ConnectOptions {
        ok_timeout_secs: 3.0,
        handshake_timeout_secs: 10.0,
        ..ConnectOptions::default()
    }
}

/// Connect to a simulator and wait until the post-handshake queries are
/// done (settings and position known).
pub fn connect(config: SimConfig, options: ConnectOptions) -> Printer {
    let printer = Printer::new();
    printer
        .connect(ConnectTarget::Simulator(config), options)
        .expect("connect");
    wait_for(&printer, "the setup queries", |s| {
        s.is_connected() && s.settings_loaded && s.position.is_some()
    });
    wait_idle(&printer);
    printer
}

/// Poll until `cond` holds, failing with a state and console dump after
/// [`PATIENCE`].
pub fn wait_for(printer: &Printer, what: &str, cond: impl Fn(&PrinterState) -> bool) {
    wait_for_within(printer, what, PATIENCE, cond);
}

pub fn wait_for_within(
    printer: &Printer,
    what: &str,
    patience: Duration,
    cond: impl Fn(&PrinterState) -> bool,
) {
    let deadline = Instant::now() + patience;
    loop {
        if printer.with_state(&cond) {
            return;
        }
        if Instant::now() > deadline {
            let state = printer.snapshot();
            let console = tail(printer, 40);
            panic!(
                "timed out waiting for {what}\nstatus: {:?}\njob: {:?}\nlink: {:?}\nhalted: {}\nconsole:\n{}",
                state.status,
                state.job,
                state.link,
                state.halted,
                console.join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Read `events` until `pick` returns something for one of them, failing
/// after [`PATIENCE`]. The engine changes the state before it announces
/// the change, so a test that saw the state must wait for the event rather
/// than look for it with `try_iter`.
pub fn wait_for_event<T>(
    events: &Receiver<Event>,
    what: &str,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> T {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match events.recv_deadline(deadline) {
            Ok(event) => {
                if let Some(found) = pick(&event) {
                    return found;
                }
            }
            Err(_) => panic!("timed out waiting for the event: {what}"),
        }
    }
}

/// Every event up to and including the first one `last` accepts (failing
/// after [`PATIENCE`]).
pub fn events_until(
    events: &Receiver<Event>,
    what: &str,
    last: impl Fn(&Event) -> bool,
) -> Vec<Event> {
    let mut seen = Vec::new();
    wait_for_event(events, what, |e| {
        seen.push(e.clone());
        last(e).then_some(())
    });
    seen
}

/// Wait until nothing is queued or in flight.
pub fn wait_idle(printer: &Printer) {
    // Stats are published on the 20 ms tick: require two quiet looks.
    let deadline = Instant::now() + PATIENCE;
    let mut quiet = 0;
    while quiet < 3 {
        let idle = printer.with_state(|s| s.link.in_flight == 0 && s.link.queue_len == 0);
        quiet = if idle { quiet + 1 } else { 0 };
        assert!(Instant::now() < deadline, "the link never became idle");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Poll `tracked` until it is no longer pending (failing after
/// [`PATIENCE`]).
pub fn settle(tracked: &mut Tracked) -> Delivery {
    let start = Instant::now();
    while tracked.poll() == Delivery::Pending {
        assert!(start.elapsed() < PATIENCE, "still pending");
        std::thread::sleep(Duration::from_millis(10));
    }
    tracked.poll()
}

/// The last `n` console lines, formatted.
pub fn tail(printer: &Printer, n: usize) -> Vec<String> {
    let all = printer.console_since(0, usize::MAX);
    all[all.len().saturating_sub(n)..]
        .iter()
        .map(format_entry)
        .collect()
}

pub fn format_entry(e: &ConsoleEntry) -> String {
    match e.line {
        Some(n) => format!("{:?} N{n} {}", e.kind, e.text),
        None => format!("{:?} {}", e.kind, e.text),
    }
}

/// Every console entry so far.
pub fn console(printer: &Printer) -> Vec<ConsoleEntry> {
    printer.console_since(0, usize::MAX)
}

/// Sequence number of the newest console entry.
pub fn last_seq(printer: &Printer) -> u64 {
    printer
        .console_since(0, usize::MAX)
        .last()
        .map_or(0, |e| e.seq)
}
