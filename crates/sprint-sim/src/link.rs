//! The simulation thread and the handles hosts and tests use to talk to it.

use crate::config::{SimConfig, sanitize_rate, sanitize_time_scale};
use crate::machine::Machine;
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Longest the simulation thread sleeps between steps (real time).
const TICK: Duration = Duration::from_millis(5);
/// Default read timeout of [`SimReader`].
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(100);
/// Most commands kept for [`SimControl::received_commands`]; older ones
/// are dropped first so a simulator running for days stays bounded.
pub const RECEIVED_LOG_CAPACITY: usize = 1_000_000;

/// State of the virtual printer at one moment (see [`SimControl::snapshot`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SimSnapshot {
    /// Logical X Y Z E position as reported by M114.
    pub position: [f32; 4],
    /// Current hotend temperatures (°C), one per extruder.
    pub hotend_temps: Vec<f32>,
    /// Hotend targets (°C).
    pub hotend_targets: Vec<f32>,
    /// Bed temperature (°C; ambient when there is no heated bed).
    pub bed_temp: f32,
    /// Bed target (°C).
    pub bed_target: f32,
    /// Part-cooling fan 0 speed (0..=255).
    pub fan: u8,
    /// Feedrate override (M220), percent.
    pub feedrate_pct: f32,
    /// Flow override of the active tool (M221), percent.
    pub flow_pct: f32,
    /// Last accepted line number.
    pub last_line_number: u32,
    /// Moves currently in the planner.
    pub planner_queue: usize,
    /// Killed (M112) and waiting for a reset.
    pub halted: bool,
    /// Printing from the SD card (not paused).
    pub sd_printing: bool,
    /// Commands accepted since the simulator started (not reset by
    /// [`SimControl::clear_received`]).
    pub commands_received: usize,
    /// "Resend:" requests sent since the simulator started.
    pub resends_requested: usize,
}

/// Messages to the simulation thread.
pub(crate) enum Input {
    Data(Vec<u8>),
    Inject(String),
    Runout,
    Reset,
    Shutdown,
}

/// State shared between the thread and the handles.
struct Shared {
    received: VecDeque<String>,
    snapshot: SimSnapshot,
    checksum_error_rate: f32,
    drop_ok_rate: f32,
    time_scale: f32,
}

struct Inner {
    shared: Mutex<Shared>,
    running: AtomicBool,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        // A panic while holding the lock cannot leave Shared inconsistent
        // in a way that matters, so recover from poisoning.
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The three handles of a running virtual printer.
pub struct SimLink {
    /// Bytes from the printer.
    pub reader: SimReader,
    /// Bytes to the printer.
    pub writer: SimWriter,
    /// Test/demo control handle.
    pub control: SimControl,
}

/// Bytes the printer sends. `read` blocks up to the read timeout (default
/// 100 ms) and then returns `Err(io::ErrorKind::TimedOut)`; it returns
/// `Ok(0)` once the printer is shut down and all output has been read.
pub struct SimReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
    timeout: Duration,
}

impl SimReader {
    /// Change how long `read` waits for data.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The current read timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl io::Read for SimReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.buf.len() {
            let chunk = match self.rx.try_recv() {
                Ok(c) => c,
                Err(TryRecvError::Disconnected) => return Ok(0),
                Err(TryRecvError::Empty) => match self.rx.recv_timeout(self.timeout) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "virtual printer read timed out",
                        ));
                    }
                    Err(RecvTimeoutError::Disconnected) => return Ok(0),
                },
            };
            self.buf = chunk;
            self.pos = 0;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Bytes the host sends. Writing fails with `BrokenPipe` once the printer
/// is shut down.
pub struct SimWriter {
    tx: Sender<Input>,
    inner: Arc<Inner>,
}

fn broken_pipe() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "virtual printer is shut down")
}

impl io::Write for SimWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.inner.running.load(Ordering::Acquire) {
            return Err(broken_pipe());
        }
        if buf.is_empty() {
            return Ok(0);
        }
        self.tx
            .send(Input::Data(buf.to_vec()))
            .map_err(|_| broken_pipe())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.inner.running.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(broken_pipe())
        }
    }
}

/// Test/demo control handle (Clone + Send + Sync).
#[derive(Clone)]
pub struct SimControl {
    tx: Sender<Input>,
    inner: Arc<Inner>,
}

impl SimControl {
    /// Every command accepted for execution, in order, cleaned (no line
    /// number, checksum or comment). Lines stored to the SD card during
    /// M28 are included; commands read from SD files are not.
    pub fn received_commands(&self) -> Vec<String> {
        self.inner.lock().received.iter().cloned().collect()
    }

    /// Forget the commands recorded so far.
    pub fn clear_received(&self) {
        self.inner.lock().received.clear();
    }

    /// The printer state as of the last simulation step.
    pub fn snapshot(&self) -> SimSnapshot {
        self.inner.lock().snapshot.clone()
    }

    /// Probability (0..=1) that a valid numbered line is rejected as corrupt.
    pub fn set_checksum_error_rate(&self, rate: f32) {
        self.inner.lock().checksum_error_rate = sanitize_rate(rate);
    }

    /// Probability (0..=1) that an "ok" is lost.
    pub fn set_drop_ok_rate(&self, rate: f32) {
        self.inner.lock().drop_ok_rate = sanitize_rate(rate);
    }

    /// Change the simulation speed (1.0 = real time).
    pub fn set_time_scale(&self, scale: f32) {
        self.inner.lock().time_scale = sanitize_time_scale(scale);
    }

    /// Emit a raw line from the printer (e.g. "//action:pause").
    pub fn inject_line(&self, line: &str) {
        let _ = self.tx.send(Input::Inject(line.to_string()));
    }

    /// Simulate filament runout (M600-style: host prompt + busy: paused
    /// for user until M876 S0 or M108).
    pub fn trigger_filament_runout(&self) {
        let _ = self.tx.send(Input::Runout);
    }

    /// Simulate the board resetting (prints "start" again after the boot
    /// delay, resets line numbers, loses unsaved settings).
    pub fn reset(&self) {
        let _ = self.tx.send(Input::Reset);
    }

    /// Stop the simulator. Reads then return `Ok(0)` and writes fail.
    pub fn shutdown(&self) {
        self.inner.running.store(false, Ordering::Release);
        let _ = self.tx.send(Input::Shutdown);
    }

    /// Whether the simulation thread is still running.
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Acquire)
    }
}

/// Spawn a virtual printer on a background thread.
pub fn spawn(config: SimConfig) -> SimLink {
    let config = config.sanitized();
    let (in_tx, in_rx) = mpsc::channel();
    let (out_tx, out_rx) = mpsc::channel();
    let inner = Arc::new(Inner {
        shared: Mutex::new(Shared {
            received: VecDeque::new(),
            snapshot: SimSnapshot::default(),
            checksum_error_rate: config.checksum_error_rate,
            drop_ok_rate: config.drop_ok_rate,
            time_scale: config.time_scale,
        }),
        running: AtomicBool::new(true),
    });
    let thread_inner = Arc::clone(&inner);
    let spawned = thread::Builder::new()
        .name("sprint-sim".to_string())
        .spawn(move || {
            let guard = RunningGuard(Arc::clone(&thread_inner));
            let machine = Machine::new(config);
            run(machine, in_rx, out_tx, &thread_inner);
            drop(guard);
        });
    if let Err(e) = spawned {
        log::error!("could not start the virtual printer thread: {e}");
        inner.running.store(false, Ordering::Release);
    }
    SimLink {
        reader: SimReader {
            rx: out_rx,
            buf: Vec::new(),
            pos: 0,
            timeout: DEFAULT_READ_TIMEOUT,
        },
        writer: SimWriter {
            tx: in_tx.clone(),
            inner: Arc::clone(&inner),
        },
        control: SimControl { tx: in_tx, inner },
    }
}

/// Marks the simulator stopped when the thread ends, even by panic.
struct RunningGuard(Arc<Inner>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}

/// Apply one message; returns false to stop.
fn handle(machine: &mut Machine, msg: Input) -> bool {
    match msg {
        Input::Data(bytes) => machine.on_bytes(&bytes),
        Input::Inject(line) => machine.inject_line(&line),
        Input::Runout => machine.filament_runout(),
        Input::Reset => machine.reset(),
        Input::Shutdown => return false,
    }
    true
}

fn run(mut machine: Machine, rx: Receiver<Input>, tx: Sender<Vec<u8>>, inner: &Inner) {
    let mut last = Instant::now();
    let mut reader_gone = false;
    loop {
        let mut keep_going = match rx.recv_timeout(TICK) {
            Ok(msg) => handle(&mut machine, msg),
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => false,
        };
        while keep_going {
            match rx.try_recv() {
                Ok(msg) => keep_going = handle(&mut machine, msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => keep_going = false,
            }
        }
        if !keep_going || !inner.running.load(Ordering::Acquire) {
            break;
        }
        let (scale, checksum_rate, drop_rate) = {
            let s = inner.lock();
            (s.time_scale, s.checksum_error_rate, s.drop_ok_rate)
        };
        machine.set_fault_rates(checksum_rate, drop_rate);
        let now = Instant::now();
        let real = now.duration_since(last);
        last = now;
        machine.advance(real.as_secs_f64() * f64::from(scale));
        let output = machine.take_output();
        let received = machine.take_received();
        {
            let mut s = inner.lock();
            for cmd in received {
                if s.received.len() >= RECEIVED_LOG_CAPACITY {
                    s.received.pop_front();
                }
                s.received.push_back(cmd);
            }
            s.snapshot = machine.snapshot();
        }
        if !output.is_empty() && !reader_gone {
            let mut bytes = Vec::with_capacity(output.iter().map(|l| l.len() + 1).sum());
            for line in &output {
                bytes.extend_from_slice(line.as_bytes());
                bytes.push(b'\n');
            }
            if tx.send(bytes).is_err() {
                // Nobody reads any more; keep simulating for the other handles.
                reader_gone = true;
            }
        }
    }
}
