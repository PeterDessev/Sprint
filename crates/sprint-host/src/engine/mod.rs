//! The engine: a reader thread per connection turns bytes into lines, and
//! the engine thread owns the writer and runs the protocol in a
//! `crossbeam_channel::select!` loop over received lines, commands from the
//! [`Printer`](crate::Printer) handles and a 20 ms tick.

mod flow;
mod jobs;
mod meta;
mod reader;
mod replies;

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use parking_lot::{Mutex, RwLock};
use sprint_gcode::{GcodeFile, MachineState, parse_line};
use sprint_proto::{FirmwareFlavor, PrinterSettings, ReplyParser, SettingCommand, StatsReport};
use sprint_sim::SimControl;

use crate::config::{ChecksumMode, ConnectOptions, PrinterProfile};
use crate::console::{Console, ConsoleKind};
use crate::error::HostError;
use crate::event::{Event, EventBus};
use crate::job::{JobKind, PrintOptions};
use crate::script::format_number;
use crate::state::{AutotuneState, ConnectionStatus, Heater, HostPrompt, LinkStats, PrinterState};
use crate::transport::{Link, LinkHandle};

use flow::{Flow, LINE_NUMBER_LIMIT, Wire};
use jobs::JobRuntime;
use meta::{CmdMeta, Meta, PollKind, Source};
use reader::ReaderMsg;

/// Engine tick: timeouts, polling, job pacing, stats.
const TICK: Duration = Duration::from_millis(20);
/// The waker is called at most this often.
const WAKE_INTERVAL: Duration = Duration::from_millis(16);
/// With no line from the printer, the first M110 goes out after this.
const FIRST_HANDSHAKE_ATTEMPT: Duration = Duration::from_secs(1);
/// M110 is repeated this often until it is acknowledged.
const HANDSHAKE_RETRY: Duration = Duration::from_secs(2);
/// After a handshake that needed retries, stray `ok`s are ignored this long.
const HANDSHAKE_SETTLE: Duration = Duration::from_millis(300);
/// `JobProgress` events are sent at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Window for `lines_per_sec`.
const RATE_WINDOW: Duration = Duration::from_secs(2);
/// M27 polling interval without SD auto-reports.
const SD_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// `apply_setting` refreshes the settings this long after the last edit.
const SETTINGS_REFRESH_DELAY: Duration = Duration::from_millis(1500);
/// Long-running commands get this many ok timeouts.
const LONG_TIMEOUT_FACTOR: u32 = 6;
/// How long shutting down waits for each thread before detaching it.
const JOIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Lines handled per batch before the engine sends again.
const LINE_BATCH: usize = 256;

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct WakeState {
    last: Option<Instant>,
    pending: bool,
}

/// State shared between the handles and the engine threads.
pub(crate) struct Shared {
    pub(crate) state: RwLock<PrinterState>,
    pub(crate) console: Mutex<Console>,
    pub(crate) bus: EventBus,
    pub(crate) profile: RwLock<PrinterProfile>,
    /// When the `Printer` was created (console times).
    pub(crate) epoch: Instant,
    waker: Mutex<Option<Waker>>,
    wake: Mutex<WakeState>,
}

impl Shared {
    pub(crate) fn new(console_capacity: usize) -> Shared {
        Shared {
            state: RwLock::new(PrinterState::default()),
            console: Mutex::new(Console::new(console_capacity)),
            bus: EventBus::default(),
            profile: RwLock::new(PrinterProfile::default()),
            epoch: Instant::now(),
            waker: Mutex::new(None),
            wake: Mutex::new(WakeState::default()),
        }
    }

    /// Append a console line (and announce it).
    pub(crate) fn log(
        &self,
        kind: ConsoleKind,
        text: impl Into<String>,
        auto: bool,
        line: Option<u32>,
    ) {
        let time = self.epoch.elapsed().as_secs_f64();
        let entry = self.console.lock().push(time, kind, text, auto, line);
        self.bus.emit(Event::Console(entry));
        self.request_wake();
    }

    pub(crate) fn emit(&self, event: Event) {
        self.bus.emit(event);
        self.request_wake();
    }

    /// Change the state.
    pub(crate) fn update<R>(&self, f: impl FnOnce(&mut PrinterState) -> R) -> R {
        let r = f(&mut self.state.write());
        self.request_wake();
        r
    }

    pub(crate) fn set_waker(&self, waker: Waker) {
        *self.waker.lock() = Some(waker);
    }

    /// Call the waker now, or soon if it was called less than 16 ms ago
    /// ([`Shared::flush_wake`] delivers the trailing call).
    pub(crate) fn request_wake(&self) {
        let call = {
            let mut w = self.wake.lock();
            let now = Instant::now();
            if w.last
                .is_none_or(|t| now.duration_since(t) >= WAKE_INTERVAL)
            {
                w.last = Some(now);
                w.pending = false;
                true
            } else {
                w.pending = true;
                false
            }
        };
        if call {
            self.call_waker();
        }
    }

    /// Deliver a postponed wake-up once the interval has passed.
    pub(crate) fn flush_wake(&self) {
        let call = {
            let mut w = self.wake.lock();
            let now = Instant::now();
            if w.pending
                && w.last
                    .is_none_or(|t| now.duration_since(t) >= WAKE_INTERVAL)
            {
                w.last = Some(now);
                w.pending = false;
                true
            } else {
                false
            }
        };
        if call {
            self.call_waker();
        }
    }

    /// Call the waker now if a wake-up is pending, ignoring the rate limit
    /// (when no engine tick will deliver it later: engine exit, changes
    /// made by a handle).
    pub(crate) fn wake_now(&self) {
        let call = {
            let mut w = self.wake.lock();
            let pending = std::mem::take(&mut w.pending);
            if pending {
                w.last = Some(Instant::now());
            }
            pending
        };
        if call {
            self.call_waker();
        }
    }

    fn call_waker(&self) {
        let waker = self.waker.lock().clone();
        if let Some(w) = waker {
            w();
        }
    }
}

/// Requests from the handles.
pub(crate) enum Command {
    /// Cleaned, non-emergency commands for the priority queue.
    Send(Vec<String>),
    /// Bypass queue and flow control.
    Immediate(String),
    EmergencyStop,
    StartPrint {
        file: Arc<GcodeFile>,
        options: PrintOptions,
        reply: Sender<Result<(), HostError>>,
    },
    StartSdPrint {
        name: String,
        reply: Sender<Result<(), HostError>>,
    },
    UploadToSd {
        file: Arc<GcodeFile>,
        name: String,
        reply: Sender<Result<(), HostError>>,
    },
    Pause,
    Resume,
    Cancel,
    AnswerPrompt(usize),
    Extrude {
        tool: u8,
        length: f32,
        feedrate: f32,
    },
    ApplySetting(SettingCommand),
    Autotune {
        heater: Heater,
        target: f32,
        cycles: u32,
    },
    ProbeBedMesh,
    FirmwareRestart,
    Disconnect,
}

/// The handle side of a running engine.
pub(crate) struct EngineHandle {
    pub(crate) tx: Sender<Command>,
    engine: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    pub(crate) sim: Option<SimControl>,
}

impl EngineHandle {
    /// The engine thread has exited (disconnected, failed or lost).
    pub(crate) fn is_finished(&self) -> bool {
        self.engine.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Stop the engine and wait (bounded) for its threads.
    pub(crate) fn shutdown(mut self) {
        let _ = self.tx.send(Command::Disconnect);
        let deadline = Instant::now() + JOIN_TIMEOUT;
        join_until(self.engine.take(), deadline);
        self.stop.store(true, Ordering::Release);
        join_until(self.reader.take(), deadline + JOIN_TIMEOUT);
    }
}

/// Join `handle` if it finishes before `deadline`, otherwise detach it.
fn join_until(handle: Option<JoinHandle<()>>, deadline: Instant) {
    let Some(handle) = handle else {
        return;
    };
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    if handle.is_finished() {
        let _ = handle.join();
    } else {
        log::warn!("an engine thread did not stop in time; detaching it");
    }
}

/// Start the reader and engine threads for an open link.
pub(crate) fn start(
    shared: Arc<Shared>,
    link: Link,
    label: String,
    options: ConnectOptions,
) -> Result<EngineHandle, HostError> {
    let (reader, writer, link) = link.split();
    let sim = link.sim_control().cloned();
    let (line_tx, line_rx) = crossbeam_channel::unbounded();
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let stop = Arc::new(AtomicBool::new(false));
    let bytes = Arc::new(AtomicU64::new(0));

    let reader_stop = Arc::clone(&stop);
    let reader_bytes = Arc::clone(&bytes);
    let reader = thread::Builder::new()
        .name("sprint-host-reader".to_string())
        .spawn(move || reader::run(reader, line_tx, reader_stop, reader_bytes))?;

    let engine_stop = Arc::clone(&stop);
    let panic_stop = Arc::clone(&stop);
    let panic_shared = Arc::clone(&shared);
    let engine = Engine::new(
        shared,
        writer,
        link,
        line_rx,
        cmd_rx,
        engine_stop,
        bytes,
        label,
        options,
    );
    let engine = thread::Builder::new()
        .name("sprint-host-engine".to_string())
        .spawn(move || {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.run()));
            if run.is_err() {
                // Never leave a "connected" state behind a dead engine.
                panic_stop.store(true, Ordering::Release);
                let message = "Internal error in the printer engine; disconnected".to_string();
                panic_shared.log(ConsoleKind::Error, message.clone(), false, None);
                panic_shared.update(|s| {
                    s.status = ConnectionStatus::Failed(message.clone());
                    if let Some(job) = &mut s.job
                        && job.status.is_active()
                    {
                        job.status = crate::job::JobStatus::Failed(message.clone());
                    }
                });
                panic_shared.emit(Event::Status(ConnectionStatus::Failed(message.clone())));
                panic_shared.emit(Event::Error(message));
                panic_shared.wake_now();
            }
        });
    let engine = match engine {
        Ok(e) => e,
        Err(e) => {
            stop.store(true, Ordering::Release);
            return Err(e.into());
        }
    };
    Ok(EngineHandle {
        tx: cmd_tx,
        engine: Some(engine),
        reader: Some(reader),
        stop,
        sim,
    })
}

/// Where the connection is in its life.
#[derive(Clone, Copy, Debug)]
enum Phase {
    /// Waiting for the M110 `ok`.
    Handshake {
        started: Instant,
        line_seen: bool,
        attempts: u32,
        last_attempt: Option<Instant>,
    },
    /// Handshake done; ignoring stray `ok`s of repeated M110s.
    Settle { until: Instant },
    /// Talking.
    Ready,
}

impl Phase {
    fn handshake(now: Instant, line_seen: bool) -> Phase {
        Phase::Handshake {
            started: now,
            line_seen,
            attempts: 0,
            last_attempt: None,
        }
    }
}

/// What the next queued item is.
enum Next {
    Command(Meta),
    /// `M110 N<value>` from the queue.
    LineReset(u32, Meta),
}

/// The engine thread's state.
pub(crate) struct Engine {
    shared: Arc<Shared>,
    writer: Box<dyn Write + Send>,
    link: LinkHandle,
    lines: Receiver<ReaderMsg>,
    commands: Receiver<Command>,
    stop: Arc<AtomicBool>,
    bytes_received: Arc<AtomicU64>,
    options: ConnectOptions,
    target: String,
    opened_at: Instant,

    flow: Flow<Meta>,
    parser: ReplyParser,
    phase: Phase,
    /// Set to leave the loop with this final status.
    exit: Option<ConnectionStatus>,

    /// Setup and recovery commands (sent first).
    internal: VecDeque<Meta>,
    /// User commands.
    manual: VecDeque<Meta>,
    job: Option<JobRuntime>,

    halted: bool,
    /// M999 was sent while halted.
    restarting: bool,
    /// The line counter must be reset (M110 N0) before numbered commands.
    renumber_pending: bool,
    /// M115 of the post-handshake setup is outstanding.
    setup_pending: bool,
    temp_autoreport: bool,
    sd_autoreport: bool,
    next_temp_poll: Instant,
    temp_poll_outstanding: bool,
    next_sd_poll: Instant,
    sd_poll_outstanding: bool,
    last_temp_sample: Option<Instant>,
    homed: bool,
    /// Modes and positions over every command sent.
    printer_modes: MachineState,
    settings_refresh_at: Option<Instant>,

    pending_settings: Option<PrinterSettings>,
    pending_endstops: Vec<(String, bool)>,
    pending_stats: Option<StatsReport>,
    pending_files: Vec<sprint_proto::SdFile>,
    prompt: Option<HostPrompt>,

    stats: LinkStats,
    ack_times: VecDeque<Instant>,
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    fn new(
        shared: Arc<Shared>,
        writer: Box<dyn Write + Send>,
        link: LinkHandle,
        lines: Receiver<ReaderMsg>,
        commands: Receiver<Command>,
        stop: Arc<AtomicBool>,
        bytes_received: Arc<AtomicU64>,
        target: String,
        options: ConnectOptions,
    ) -> Engine {
        let now = Instant::now();
        Engine {
            shared,
            writer,
            link,
            lines,
            commands,
            stop,
            bytes_received,
            flow: Flow::new(options.max_in_flight, now),
            options,
            target,
            opened_at: now,
            parser: ReplyParser::new(),
            phase: Phase::handshake(now, false),
            exit: None,
            internal: VecDeque::new(),
            manual: VecDeque::new(),
            job: None,
            halted: false,
            restarting: false,
            renumber_pending: false,
            setup_pending: false,
            temp_autoreport: false,
            sd_autoreport: false,
            next_temp_poll: now,
            temp_poll_outstanding: false,
            next_sd_poll: now,
            sd_poll_outstanding: false,
            last_temp_sample: None,
            homed: false,
            printer_modes: MachineState::new(),
            settings_refresh_at: None,
            pending_settings: None,
            pending_endstops: Vec::new(),
            pending_stats: None,
            pending_files: Vec::new(),
            prompt: None,
            stats: LinkStats::default(),
            ack_times: VecDeque::new(),
        }
    }

    // ----- the loop -----

    fn run(mut self) {
        self.set_status(ConnectionStatus::Handshaking);
        self.info(format!(
            "Link open ({}); waiting for the printer",
            self.target
        ));
        let ticker = crossbeam_channel::tick(TICK);
        while self.exit.is_none() {
            // Wake up when a held-back retransmission becomes due.
            let gate = self
                .flow
                .retransmit_due_at()
                .map_or_else(crossbeam_channel::never, crossbeam_channel::at);
            select! {
                recv(self.lines) -> msg => match msg {
                    Ok(ReaderMsg::Line(line)) => {
                        self.on_line(line);
                        for _ in 1..LINE_BATCH {
                            match self.lines.try_recv() {
                                Ok(ReaderMsg::Line(line)) => self.on_line(line),
                                Ok(ReaderMsg::Lost(reason)) => {
                                    self.link_lost(&reason);
                                    break;
                                }
                                Err(_) => break,
                            }
                            if self.exit.is_some() {
                                break;
                            }
                        }
                    }
                    Ok(ReaderMsg::Lost(reason)) => self.link_lost(&reason),
                    Err(_) => self.link_lost("the reader stopped"),
                },
                recv(self.commands) -> msg => match msg {
                    Ok(cmd) => self.on_command(cmd),
                    // Every Printer handle is gone.
                    Err(_) => self.close(ConnectionStatus::Disconnected, "Disconnected"),
                },
                recv(ticker) -> _ => self.on_tick(),
                recv(gate) -> _ => {}
            }
            if self.exit.is_none() {
                self.pump();
            }
        }
        self.finish();
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.writer.flush();
        self.link.close();
        self.publish_stats(Instant::now());
        if let Some(status) = self.exit.clone() {
            self.set_status(status);
        }
        // No tick will deliver a postponed wake-up any more.
        self.shared.wake_now();
    }

    /// Leave the loop with `status` (job failed if one is running).
    fn close(&mut self, status: ConnectionStatus, message: &str) {
        if self.exit.is_some() {
            return;
        }
        if self.job.is_some() {
            let reason = match &status {
                ConnectionStatus::Disconnected => "disconnected".to_string(),
                ConnectionStatus::Failed(r) => r.clone(),
                _ => "connection closed".to_string(),
            };
            self.fail_job(&reason);
        }
        match &status {
            ConnectionStatus::Failed(_) => {
                self.shared.log(ConsoleKind::Error, message, false, None);
                self.shared
                    .update(|s| s.last_error = Some(message.to_string()));
                self.shared.emit(Event::Error(message.to_string()));
            }
            _ => self.info(message),
        }
        self.exit = Some(status);
    }

    fn link_lost(&mut self, reason: &str) {
        let message = format!("Connection lost: {reason}");
        self.close(ConnectionStatus::Failed(message.clone()), &message);
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        match self.phase {
            Phase::Handshake { started, .. } => {
                let limit = Duration::from_secs_f32(self.options.handshake_timeout_secs);
                if now.duration_since(started) > limit {
                    let message = format!(
                        "Handshake failed: no answer from the printer within {} s",
                        format_number(f64::from(self.options.handshake_timeout_secs), 1)
                    );
                    self.close(ConnectionStatus::Failed(message.clone()), &message);
                    return;
                }
            }
            Phase::Settle { .. } => {}
            Phase::Ready => self.check_timeout(now),
        }
        if let Some(at) = self.settings_refresh_at
            && now >= at
        {
            self.settings_refresh_at = None;
            if !self.manual.iter().any(|m| m.is('M', 503)) {
                self.internal.push_back(CmdMeta::new("M503", Source::User));
            }
        }
        self.advance_job(now);
        self.job_tick(now);
        self.publish_stats(now);
        self.shared.flush_wake();
    }

    fn check_timeout(&mut self, now: Instant) {
        let base = Duration::from_secs_f32(self.options.ok_timeout_secs);
        let timeout = |m: &Meta| {
            if m.long {
                base.saturating_mul(LONG_TIMEOUT_FACTOR)
            } else {
                base
            }
        };
        if !self.flow.timed_out(now, timeout) {
            return;
        }
        let released = self.flow.release_all(now);
        if self.halted {
            // A halted printer answers nothing; just free the window.
            for m in &released {
                self.complete(m, now);
            }
            return;
        }
        self.stats.timeouts += 1;
        self.warn(format!(
            "No answer for {} s; assuming lost ok for {} command(s) and resynchronising",
            format_number(f64::from(self.options.ok_timeout_secs), 1),
            released.len()
        ));
        for m in &released {
            self.complete(m, now);
        }
        self.internal
            .push_front(CmdMeta::new("M105", Source::Recovery));
        self.advance_job(now);
    }

    // ----- handshake -----

    fn handshake_numbered(&self) -> bool {
        self.options.checksum == ChecksumMode::Always
    }

    /// Send or repeat the handshake M110 when it is due.
    fn pump_handshake(&mut self, now: Instant, out: &mut Vec<u8>) {
        let Phase::Handshake {
            started,
            line_seen,
            attempts,
            last_attempt,
        } = self.phase
        else {
            return;
        };
        // A resend request during the handshake repeats the M110.
        if let Some(w) = self.flow.poll_retransmit(now) {
            self.emit_wire(&w, out);
            return;
        }
        let due = match last_attempt {
            None => line_seen || now.duration_since(started) >= FIRST_HANDSHAKE_ATTEMPT,
            Some(t) => now.duration_since(t) >= HANDSHAKE_RETRY,
        };
        if !due {
            return;
        }
        let wire = if attempts == 0 || !self.flow.barrier_active() {
            self.flow.reset(now);
            let meta = CmdMeta::new("M110 N0", Source::Setup);
            Some(
                self.flow
                    .send_line_reset(0, self.handshake_numbered(), meta, now),
            )
        } else {
            self.flow.repeat_barrier(now)
        };
        if let Some(w) = wire {
            self.emit_wire(&w, out);
        }
        self.phase = Phase::Handshake {
            started,
            line_seen,
            attempts: attempts + 1,
            last_attempt: Some(now),
        };
    }

    /// The handshake M110 was acknowledged.
    fn handshake_done(&mut self, now: Instant) {
        let Phase::Handshake { attempts, .. } = self.phase else {
            return;
        };
        if attempts > 1 {
            self.phase = Phase::Settle {
                until: now + HANDSHAKE_SETTLE,
            };
        } else {
            self.phase = Phase::Ready;
            self.on_connected(now);
        }
    }

    fn on_connected(&mut self, now: Instant) {
        self.halted = false;
        self.restarting = false;
        self.next_temp_poll = now;
        self.shared.update(|s| s.halted = false);
        self.set_status(ConnectionStatus::Connected);
        self.info(format!("Connected to {}", self.target));
        if self.options.query_on_connect {
            self.setup_pending = true;
            self.internal.push_back(CmdMeta::new("M115", Source::Setup));
        }
    }

    /// M115 answered: configure auto-reports, then read settings and
    /// position.
    fn continue_setup(&mut self) {
        self.setup_pending = false;
        let (temp, pos, flavor) = {
            let s = self.shared.state.read();
            (
                s.has_capability("AUTOREPORT_TEMP"),
                s.has_capability("AUTOREPORT_POS"),
                s.firmware
                    .as_ref()
                    .map_or(FirmwareFlavor::Unknown, |f| f.flavor),
            )
        };
        let interval = self.options.autoreport_interval_secs;
        if temp && interval > 0 {
            self.internal
                .push_back(CmdMeta::new(format!("M155 S{interval}"), Source::Setup));
        }
        if pos {
            self.internal
                .push_back(CmdMeta::new("M154 S1", Source::Setup));
        }
        if self.options.query_on_connect {
            if flavor.supports_m503() {
                self.internal.push_back(CmdMeta::new("M503", Source::Setup));
            }
            self.internal.push_back(CmdMeta::new("M114", Source::Setup));
        }
    }

    /// The board reset itself (`start` while connected).
    fn board_reset(&mut self, now: Instant) {
        self.warn("The printer reset (\"start\" received); reconnecting");
        if self.job.is_some() {
            self.fail_job("printer reset");
        }
        self.flow.reset(now);
        self.parser.reset();
        self.internal.clear();
        self.halted = false;
        self.restarting = false;
        self.renumber_pending = false;
        self.setup_pending = false;
        self.temp_autoreport = false;
        self.sd_autoreport = false;
        self.temp_poll_outstanding = false;
        self.sd_poll_outstanding = false;
        self.homed = false;
        self.pending_settings = None;
        self.pending_endstops.clear();
        self.pending_stats = None;
        self.prompt = None;
        let had_prompt = self.shared.update(|s| {
            s.halted = false;
            s.busy = None;
            s.sd.printing = None;
            s.sd.listing = false;
            if let Some(a) = &mut s.autotune
                && a.running
            {
                a.running = false;
                a.error = Some("printer reset".to_string());
            }
            s.prompt.take().is_some()
        });
        if had_prompt {
            self.shared.emit(Event::Prompt(None));
        }
        self.phase = Phase::handshake(now, true);
        self.set_status(ConnectionStatus::Handshaking);
    }

    // ----- sending -----

    /// Whether new commands get line numbers.
    fn numbered(&self) -> bool {
        // A halted firmware has lost its line counter; a stopped Marlin still
        // takes unnumbered commands (M999 and friends).
        if self.halted {
            return false;
        }
        match self.options.checksum {
            ChecksumMode::Always => true,
            ChecksumMode::PrintOnly => self.job.as_ref().is_some_and(|j| j.is_active()),
            ChecksumMode::Never => self.upload_active(),
        }
    }

    fn upload_active(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|j| j.kind() == JobKind::SdUpload && j.is_active())
    }

    /// Send whatever is due.
    fn pump(&mut self) {
        let now = Instant::now();
        let mut out = Vec::new();
        match self.phase {
            Phase::Handshake { .. } => {
                self.pump_handshake(now, &mut out);
                self.write(&out);
                return;
            }
            Phase::Settle { until } => {
                if now < until {
                    return;
                }
                self.phase = Phase::Ready;
                self.on_connected(now);
            }
            Phase::Ready => {}
        }
        loop {
            if let Some(w) = self.flow.poll_retransmit(now) {
                self.emit_wire(&w, &mut out);
                continue;
            }
            if !self.flow.can_send_new() {
                break;
            }
            if self.flow.next_line() >= LINE_NUMBER_LIMIT {
                self.renumber_pending = true;
            }
            if self.renumber_pending && !self.halted {
                if self.flow.is_idle() {
                    self.renumber_pending = false;
                    let meta = CmdMeta::new("M110 N0", Source::Setup);
                    let w = self.flow.send_line_reset(0, self.numbered(), meta, now);
                    self.emit_wire(&w, &mut out);
                }
                break;
            }
            match self.next_command(now) {
                None => break,
                Some(Next::LineReset(value, meta)) => {
                    let w = self.flow.send_line_reset(value, self.numbered(), meta, now);
                    self.emit_wire(&w, &mut out);
                }
                Some(Next::Command(meta)) => {
                    let numbered = self.numbered();
                    let w = self.flow.send(&meta.text, numbered, Arc::clone(&meta), now);
                    self.on_sent(&meta, now);
                    self.emit_wire(&w, &mut out);
                }
            }
        }
        self.write(&out);
    }

    /// The next command to send, by priority: setup/recovery, job scripts,
    /// user commands, polls, progress reports, the job stream.
    fn next_command(&mut self, now: Instant) -> Option<Next> {
        // Nothing but the upload itself may reach the printer while it writes
        // to the card: it would be stored in the file.
        let uploading = self.upload_active();
        if !uploading && let Some(m) = self.internal.pop_front() {
            return Some(Next::Command(m));
        }
        if let Some(m) = self.job.as_mut().and_then(JobRuntime::next_script) {
            return Some(Next::Command(m));
        }
        if !uploading && let Some(front) = self.manual.front() {
            if front.is('M', 110) {
                // A line counter reset needs an idle link.
                if !self.flow.is_idle() {
                    return None;
                }
                let value = parse_line(&front.text)
                    .get('N')
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v < f64::from(LINE_NUMBER_LIMIT))
                    .map_or(0, |v| v as u32);
                let meta = self.manual.pop_front()?;
                return Some(Next::LineReset(value, meta));
            }
            return self.manual.pop_front().map(Next::Command);
        }
        if let Some(m) = self.poll_due(now) {
            return Some(Next::Command(m));
        }
        if self.halted {
            return None;
        }
        self.job
            .as_mut()
            .and_then(|j| j.next_stream_command())
            .map(Next::Command)
    }

    /// A temperature or SD status poll, if one is due.
    fn poll_due(&mut self, now: Instant) -> Option<Meta> {
        if !matches!(self.phase, Phase::Ready)
            || self.halted
            || self.setup_pending
            || self.upload_active()
        {
            return None;
        }
        let blocked = || {
            self.flow.in_flight().any(|e| e.meta.blocks_polling())
                || self.flow.backlog().any(|m| m.blocks_polling())
        };
        let interval = self.options.temp_poll_secs;
        if !self.temp_autoreport
            && interval > 0.0
            && !self.temp_poll_outstanding
            && now >= self.next_temp_poll
            && !blocked()
        {
            self.temp_poll_outstanding = true;
            self.next_temp_poll = now + Duration::from_secs_f32(interval);
            return Some(CmdMeta::new("M105", Source::Poll(PollKind::Temperature)));
        }
        let sd_printing = self
            .job
            .as_ref()
            .is_some_and(|j| j.kind() == JobKind::SdPrint && j.is_polling_sd());
        if sd_printing
            && !self.sd_autoreport
            && !self.sd_poll_outstanding
            && now >= self.next_sd_poll
        {
            self.sd_poll_outstanding = true;
            self.next_sd_poll = now + SD_POLL_INTERVAL;
            return Some(CmdMeta::new("M27", Source::Poll(PollKind::SdStatus)));
        }
        None
    }

    /// Bookkeeping for a command that was just sent for the first time.
    fn on_sent(&mut self, meta: &Meta, now: Instant) {
        let parsed = parse_line(&meta.text);
        self.printer_modes.apply(&parsed);
        if let Some(job) = &mut self.job {
            job.on_sent(meta, &parsed, now);
        }
        if meta.is('M', 303) {
            let heater = parsed
                .get('E')
                .filter(|e| e.is_finite())
                .and_then(|e| Heater::from_autotune_index(e as i64))
                .unwrap_or(Heater::Hotend(0));
            let target = parsed.get('S').unwrap_or(150.0) as f32;
            let cycles = parsed.get('C').map_or(5, |c| c.clamp(0.0, 1000.0) as u32);
            self.shared.update(|s| {
                let fresh = s.autotune.as_ref().is_none_or(|a| !a.running);
                if fresh {
                    s.autotune = Some(AutotuneState {
                        heater,
                        target,
                        cycles,
                        running: true,
                        result: None,
                        error: None,
                    });
                }
            });
        }
    }

    /// Queue a wire line for writing and log it.
    fn emit_wire(&mut self, w: &Wire<Meta>, out: &mut Vec<u8>) {
        out.extend_from_slice(w.text.as_bytes());
        out.push(b'\n');
        self.stats.lines_sent += 1;
        self.stats.bytes_sent += w.text.len() as u64 + 1;
        self.shared.log(
            ConsoleKind::Sent,
            w.meta.text.clone(),
            w.meta.auto,
            w.number,
        );
    }

    fn write(&mut self, out: &[u8]) {
        if out.is_empty() || self.exit.is_some() {
            return;
        }
        if let Err(e) = self
            .writer
            .write_all(out)
            .and_then(|()| self.writer.flush())
        {
            self.link_lost(&format!("write failed: {e}"));
        }
    }

    /// Send outside the queue and flow control.
    fn send_immediate(&mut self, text: &str, source: Source) {
        let meta = CmdMeta::new(text, source);
        let expects_ok = !meta.is('M', 112) && !meta.is('M', 999);
        let w = self
            .flow
            .send_immediate(&meta.text, expects_ok, Arc::clone(&meta), Instant::now());
        let mut out = Vec::new();
        self.emit_wire(&w, &mut out);
        self.write(&out);
    }

    // ----- commands from the handles -----

    fn on_command(&mut self, cmd: Command) {
        let now = Instant::now();
        match cmd {
            Command::Send(lines) => {
                for line in lines {
                    self.queue_user(line);
                }
            }
            Command::Immediate(text) => self.send_immediate(&text, Source::User),
            Command::EmergencyStop => self.emergency_stop(),
            Command::StartPrint {
                file,
                options,
                reply,
            } => {
                let r = self.start_print(file, options, now);
                let _ = reply.send(r);
            }
            Command::StartSdPrint { name, reply } => {
                let r = self.start_sd_print(&name, now);
                let _ = reply.send(r);
            }
            Command::UploadToSd { file, name, reply } => {
                let r = self.start_upload(file, &name, now);
                let _ = reply.send(r);
            }
            Command::Pause => self.pause_job(now),
            Command::Resume => self.resume_job(now),
            Command::Cancel => self.cancel_job(now),
            Command::AnswerPrompt(index) => self.answer_prompt(index),
            Command::Extrude {
                tool,
                length,
                feedrate,
            } => self.extrude(tool, length, feedrate),
            Command::ApplySetting(setting) => {
                let gcode = setting.to_gcode();
                self.shared.update(|s| s.settings.insert(setting));
                self.shared.emit(Event::SettingsUpdated);
                self.queue_user(gcode);
                self.settings_refresh_at = Some(now + SETTINGS_REFRESH_DELAY);
            }
            Command::Autotune {
                heater,
                target,
                cycles,
            } => {
                self.shared.update(|s| {
                    s.autotune = Some(AutotuneState {
                        heater,
                        target,
                        cycles,
                        running: true,
                        result: None,
                        error: None,
                    });
                });
                self.queue_user(format!(
                    "M303 E{} S{} C{cycles} U1",
                    heater.autotune_index(),
                    format_number(f64::from(target), 1)
                ));
            }
            Command::ProbeBedMesh => self.probe_bed_mesh(),
            Command::FirmwareRestart => self.firmware_restart(),
            Command::Disconnect => self.close(ConnectionStatus::Disconnected, "Disconnected"),
        }
    }

    fn queue_user(&mut self, text: String) {
        let meta = CmdMeta::new(text, Source::User);
        if meta.is('M', 999) {
            self.firmware_restart();
            return;
        }
        if meta.is('M', 112) {
            self.emergency_stop();
            return;
        }
        self.manual.push_back(meta);
    }

    fn firmware_restart(&mut self) {
        // M999 answers with a resend request (and its ok) rather than an ok
        // of its own, so it is not tracked.
        self.send_immediate("M999", Source::User);
        if self.halted {
            self.restarting = true;
            self.info("Restart requested (M999)");
        }
    }

    fn emergency_stop(&mut self) {
        self.send_immediate("M112", Source::User);
        self.manual.clear();
        self.internal.clear();
        let now = Instant::now();
        if self.job.is_some() {
            self.fail_job("emergency stop");
        }
        self.enter_halt("Emergency stop (M112)", now);
    }

    /// The firmware stopped: nothing will be acknowledged until it is
    /// restarted or reset.
    fn enter_halt(&mut self, message: &str, now: Instant) {
        let first = !self.halted;
        self.halted = true;
        self.restarting = false;
        self.renumber_pending = true;
        for m in self.flow.release_all(now) {
            self.complete(&m, now);
        }
        // Lines waiting for retransmission are dropped with the counter.
        self.flow.reset(now);
        self.shared.update(|s| {
            s.halted = true;
            s.last_error = Some(message.to_string());
            s.busy = None;
        });
        if first {
            self.shared.log(ConsoleKind::Error, message, false, None);
            self.shared.emit(Event::Error(message.to_string()));
        }
    }

    /// An `ok` (or resend request) after M999: the firmware runs again.
    fn leave_halt(&mut self) {
        if !self.restarting {
            return;
        }
        self.restarting = false;
        self.halted = false;
        self.renumber_pending = true;
        self.shared.update(|s| s.halted = false);
        self.info("The printer restarted");
        if self.temp_autoreport {
            let interval = self.options.autoreport_interval_secs;
            self.internal
                .push_back(CmdMeta::new(format!("M155 S{interval}"), Source::Setup));
        }
    }

    fn answer_prompt(&mut self, index: usize) {
        self.send_immediate(&format!("M876 S{index}"), Source::User);
        self.prompt = None;
        let had = self.shared.update(|s| s.prompt.take().is_some());
        if had {
            self.shared.emit(Event::Prompt(None));
        }
    }

    fn extrude(&mut self, tool: u8, length: f32, feedrate: f32) {
        if !length.is_finite() || length == 0.0 {
            return;
        }
        let active = self.shared.state.read().active_tool;
        if tool != active {
            self.queue_user(format!("T{tool}"));
        }
        let restore_absolute = self.printer_modes.absolute_e;
        let feed = if feedrate.is_finite() && feedrate > 0.0 {
            feedrate
        } else {
            300.0
        };
        self.queue_user("M83".to_string());
        self.queue_user(format!(
            "G1 E{} F{}",
            format_number(f64::from(length), 3),
            format_number(f64::from(feed), 1)
        ));
        if restore_absolute {
            self.queue_user("M82".to_string());
        }
    }

    fn probe_bed_mesh(&mut self) {
        let klipper = self
            .shared
            .state
            .read()
            .firmware
            .as_ref()
            .is_some_and(|f| f.flavor == FirmwareFlavor::Klipper);
        if !self.homed {
            self.queue_user("G28".to_string());
        }
        if klipper {
            self.queue_user("BED_MESH_CALIBRATE".to_string());
            self.queue_user("BED_MESH_OUTPUT".to_string());
        } else {
            self.queue_user("G29".to_string());
            self.queue_user("M420 V".to_string());
        }
    }

    // ----- state publishing -----

    fn set_status(&mut self, status: ConnectionStatus) {
        let changed = self.shared.update(|s| {
            if s.status == status {
                false
            } else {
                s.status = status.clone();
                true
            }
        });
        if changed {
            self.shared.emit(Event::Status(status));
        }
    }

    fn info(&self, text: impl Into<String>) {
        self.shared.log(ConsoleKind::Info, text, false, None);
    }

    fn warn(&self, text: impl Into<String>) {
        self.shared.log(ConsoleKind::Warning, text, false, None);
    }

    fn publish_stats(&mut self, now: Instant) {
        while self
            .ack_times
            .front()
            .is_some_and(|t| now.duration_since(*t) > RATE_WINDOW)
        {
            self.ack_times.pop_front();
        }
        let window = now.duration_since(self.opened_at).min(RATE_WINDOW);
        self.stats.lines_per_sec = if window.as_secs_f32() > 0.05 {
            self.ack_times.len() as f32 / window.as_secs_f32()
        } else {
            0.0
        };
        self.stats.connected_secs = now.duration_since(self.opened_at).as_secs_f64();
        self.stats.bytes_received = self.bytes_received.load(Ordering::Relaxed);
        self.stats.in_flight = self.flow.in_flight_len();
        self.stats.queue_len = self.manual.len()
            + self.internal.len()
            + self.flow.backlog_len()
            + self.job.as_ref().map_or(0, JobRuntime::scripts_queued);
        let stats = self.stats.clone();
        let mut state = self.shared.state.write();
        if state.link != stats {
            // The connection clock changes on every tick; on its own it is
            // worth a wake-up (a repaint) only once per second.
            let clock_only = LinkStats {
                connected_secs: state.link.connected_secs,
                ..stats.clone()
            } == state.link;
            let new_second = state.link.connected_secs.floor() != stats.connected_secs.floor();
            state.link = stats;
            drop(state);
            if !clock_only || new_second {
                self.shared.request_wake();
            }
        }
    }
}
