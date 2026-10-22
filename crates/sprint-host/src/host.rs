//! The [`Printer`] handle: the API UIs use to connect, send, print and read
//! the printer state.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use parking_lot::Mutex;
use sprint_gcode::{GcodeFile, MachineState, clean_line};
use sprint_proto::{FirmwareFlavor, SettingCommand, is_emergency_command};
use sprint_sim::SimControl;

use crate::config::{ConnectOptions, DEFAULT_CONSOLE_LINES, Macro, PrinterProfile};
use crate::console::{ConsoleEntry, ConsoleKind};
use crate::engine::{self, Command, EngineHandle, Shared};
use crate::error::HostError;
use crate::event::Event;
use crate::job::{JobKind, JobStatus, PrintOptions};
use crate::script::{expand_macro_args, format_number, render_script_with};
use crate::state::{Axes, Axis, ConnectionStatus, Heater, PrinterState};
use crate::transport::{ConnectTarget, Link};

/// How long a request that needs the engine's answer may take.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// What became of a command sent with [`Printer::send_tracked`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Queued or sent; the firmware has not acknowledged it yet.
    Pending,
    /// The firmware acknowledged it (its `ok`).
    Acknowledged,
    /// It will not be acknowledged: the connection closed, or the command
    /// was dropped or released without an `ok` (a halt, a timeout).
    Lost,
}

/// Follows one command sent with [`Printer::send_tracked`]; poll it, for
/// example once a frame.
#[derive(Debug)]
pub struct Tracked {
    rx: Option<Receiver<()>>,
    state: Delivery,
}

impl Tracked {
    fn new(rx: Receiver<()>) -> Tracked {
        Tracked {
            rx: Some(rx),
            state: Delivery::Pending,
        }
    }

    fn lost() -> Tracked {
        Tracked {
            rx: None,
            state: Delivery::Lost,
        }
    }

    /// Where the command is now (never blocks).
    pub fn poll(&mut self) -> Delivery {
        if self.state == Delivery::Pending {
            self.state = match self.rx.as_ref().map(Receiver::try_recv) {
                Some(Ok(())) => Delivery::Acknowledged,
                Some(Err(TryRecvError::Empty)) => Delivery::Pending,
                Some(Err(TryRecvError::Disconnected)) | None => Delivery::Lost,
            };
            if self.state != Delivery::Pending {
                self.rx = None;
            }
        }
        self.state
    }
}

/// What [`Printer::disconnect_safely`] did before it closed the
/// connection. [`Display`](fmt::Display) words it for a status line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisconnectSummary {
    /// A connection was open (or being opened) and is closed now.
    pub was_connected: bool,
    /// The host print or SD upload that was cancelled before closing, or
    /// the SD print whose cancel ([`Printer::cancel`]) was already under
    /// way (waited for like the others): its kind and name.
    pub cancelled: Option<(JobKind, String)>,
    /// How that job ended before the link closed: `Cancelled` when the
    /// printer acknowledged the cancel (the cancel script ran, the
    /// partial upload was closed and deleted, or the SD print stopped),
    /// `Failed` when it failed meanwhile (a halt, a lost link),
    /// `Completed` when it finished by itself before the cancel reached
    /// it. `None` when it was still stopping at the timeout: then every
    /// heater and the part fan were switched off (host print) or the file
    /// was closed (upload) as the link closed; an SD print is left to the
    /// printer ([`DisconnectSummary::sd_print_continues`]).
    pub cancel_status: Option<JobStatus>,
    /// An SD print that goes on on the printer, unwatched: its file name.
    /// It was not cancelled (cancel it first to stop it), or its cancel
    /// was not acknowledged in time.
    pub sd_print_continues: Option<String>,
}

impl DisconnectSummary {
    /// The printer acknowledged the cancel before the link closed.
    pub fn acknowledged(&self) -> bool {
        self.cancel_status == Some(JobStatus::Cancelled)
    }
}

impl fmt::Display for DisconnectSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.was_connected {
            return f.write_str("Not connected");
        }
        if let Some((kind, name)) = &self.cancelled {
            // (A file name keeps its case at the start of the sentence.)
            let (job, subject) = match kind {
                JobKind::SdUpload => (
                    format!("the upload of {name}"),
                    format!("The upload of {name}"),
                ),
                _ => (name.clone(), name.clone()),
            };
            match &self.cancel_status {
                Some(JobStatus::Cancelled) => {
                    let what = match kind {
                        JobKind::SdUpload => "the partial file was deleted",
                        JobKind::SdPrint => "the printer stopped printing it",
                        JobKind::HostPrint => "the cancel script ran",
                    };
                    write!(f, "Cancelled {job} ({what}), then disconnected")?;
                }
                Some(JobStatus::Failed(reason)) => {
                    write!(f, "Cancelling {job} failed ({reason}); disconnected")?;
                }
                // It ended by itself before the cancel reached it.
                Some(_) => {
                    write!(
                        f,
                        "{subject} finished before it was cancelled, then disconnected"
                    )?;
                }
                // (An SD print: the printer goes on, said below.)
                None if *kind == JobKind::SdPrint => {
                    write!(f, "{subject} did not stop in time")?;
                }
                None => {
                    let what = match kind {
                        JobKind::SdUpload => "the file was closed",
                        _ => "the heaters and the part fan were switched off",
                    };
                    write!(
                        f,
                        "{subject} did not stop in time: {what} as the link closed"
                    )?;
                }
            }
        } else {
            f.write_str("Disconnected")?;
        }
        if let Some(name) = &self.sd_print_continues {
            write!(
                f,
                "; the printer goes on printing {name} from its SD card, unwatched"
            )?;
        }
        Ok(())
    }
}

/// A handle to one printer connection (or the lack of one).
///
/// Cheap to clone; every clone talks to the same engine. Dropping the last
/// clone disconnects. All methods are non-blocking except [`connect`],
/// which opens the link, and the `start_*` methods, which wait briefly for
/// the engine to accept the job.
///
/// ```no_run
/// use sprint_host::{ConnectOptions, ConnectTarget, Printer};
///
/// let printer = Printer::new();
/// printer.connect(ConnectTarget::parse("sim")?, ConnectOptions::default())?;
/// printer.send("G28\nM105");
/// let temps = printer.with_state(|s| s.temps.clone());
/// # Ok::<(), sprint_host::HostError>(())
/// ```
///
/// [`connect`]: Printer::connect
#[derive(Clone)]
pub struct Printer {
    inner: Arc<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    slot: Mutex<Slot>,
}

/// The engine of the current connection.
#[derive(Default)]
struct Slot {
    engine: Option<EngineHandle>,
    /// A `connect` is opening the link.
    connecting: bool,
    /// Incremented by `disconnect` so a connect in progress gives up.
    generation: u64,
    /// Target and options of the last [`Printer::connect`] (None for
    /// [`Printer::connect_link`]), for [`Printer::reset_board`].
    target: Option<(ConnectTarget, ConnectOptions)>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(engine) = self.slot.get_mut().engine.take() {
            engine.shutdown();
        }
    }
}

impl Default for Printer {
    fn default() -> Self {
        Printer::new()
    }
}

impl std::fmt::Debug for Printer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = self.with_state(|s| s.status.clone());
        f.debug_struct("Printer").field("status", &status).finish()
    }
}

fn command_lines(gcode: &str) -> impl Iterator<Item = String> + '_ {
    gcode
        .split(['\n', '\r'])
        .filter_map(|l| clean_line(l).map(|c| c.into_owned()))
}

impl Printer {
    /// A disconnected printer with the default profile and a 5000-line
    /// console.
    pub fn new() -> Printer {
        Printer::with_console_capacity(DEFAULT_CONSOLE_LINES)
    }

    /// A disconnected printer keeping `lines` console lines
    /// ([`AppConfig::console_max_lines`](crate::AppConfig::console_max_lines)).
    pub fn with_console_capacity(lines: usize) -> Printer {
        Printer {
            inner: Arc::new(Inner {
                shared: Arc::new(Shared::new(lines)),
                slot: Mutex::new(Slot::default()),
            }),
        }
    }

    fn shared(&self) -> &Shared {
        &self.inner.shared
    }

    // ----- connection -----

    /// Open the link and start the engine. Returns once the link is open
    /// (or failed); the handshake continues in the background and is
    /// reported through the state and events. Fails if a connection is open
    /// or being opened, or the link cannot be opened. Serial auto-baud
    /// (baud 0) probes each rate before returning. A [`Printer::disconnect`]
    /// meanwhile cancels the attempt: [`HostError::Cancelled`], status
    /// `Disconnected` and no failure event.
    pub fn connect(&self, target: ConnectTarget, options: ConnectOptions) -> Result<(), HostError> {
        let label = target.label();
        let generation = self.begin_connect(&label)?;
        self.shared().log(
            ConsoleKind::Info,
            format!("Connecting to {label}"),
            false,
            None,
        );
        let shared = Arc::clone(&self.inner.shared);
        let cancelled = || self.inner.slot.lock().generation != generation;
        let link = Link::open_until(
            &target,
            options.reset_on_connect,
            &mut |message| {
                shared.log(ConsoleKind::Info, message, false, None);
            },
            &cancelled,
        );
        match link {
            Ok(link) => self.finish_connect(link, label, options, generation, Some(target)),
            Err(_) if cancelled() => {
                // disconnect() already reported Disconnected: no failure.
                self.inner.slot.lock().connecting = false;
                self.connect_cancelled();
                Err(HostError::Cancelled)
            }
            Err(e) => {
                self.connect_failed(&e);
                Err(e)
            }
        }
    }

    /// Connect with a profile's target and options (and use its scripts and
    /// geometry, see [`Printer::set_profile`]).
    pub fn connect_profile(&self, profile: &PrinterProfile) -> Result<(), HostError> {
        self.set_profile(profile.clone());
        self.connect(profile.connection.clone(), profile.options.clone())
    }

    /// Run the engine over an already open link (custom transports, tests).
    /// `label` is shown as the connection target.
    pub fn connect_link(
        &self,
        link: Link,
        label: &str,
        options: ConnectOptions,
    ) -> Result<(), HostError> {
        let generation = self.begin_connect(label)?;
        self.finish_connect(link, label.to_string(), options, generation, None)
    }

    /// Claim the slot for a new connection and set the status to
    /// `Connecting`; returns the generation to check when the link is open.
    fn begin_connect(&self, label: &str) -> Result<u64, HostError> {
        let (old, generation) = {
            let mut slot = self.inner.slot.lock();
            let alive = slot.engine.as_ref().is_some_and(|e| !e.is_finished());
            if slot.connecting || alive {
                return Err(HostError::AlreadyConnected);
            }
            slot.connecting = true;
            (slot.engine.take(), slot.generation)
        };
        // The previous engine has finished (disconnected, failed, lost).
        if let Some(old) = old {
            old.shutdown();
        }
        self.shared().update(|state| {
            *state = PrinterState {
                target: Some(label.to_string()),
                status: ConnectionStatus::Connecting,
                // Keep what the user may still want to look at.
                job: state.job.take(),
                ..PrinterState::default()
            };
        });
        self.shared()
            .emit(Event::Status(ConnectionStatus::Connecting));
        self.shared().wake_now();
        Ok(generation)
    }

    fn finish_connect(
        &self,
        link: Link,
        label: String,
        options: ConnectOptions,
        generation: u64,
        target: Option<ConnectTarget>,
    ) -> Result<(), HostError> {
        let mut slot = self.inner.slot.lock();
        slot.connecting = false;
        if slot.generation != generation {
            // disconnect() was called while the link was opening.
            drop(slot);
            let (_, _, handle) = link.split();
            handle.close();
            self.connect_cancelled();
            return Err(HostError::Cancelled);
        }
        match engine::start(
            Arc::clone(&self.inner.shared),
            link,
            label,
            options.sanitized(),
        ) {
            Ok(handle) => {
                slot.engine = Some(handle);
                slot.target = target.map(|t| (t, options));
                Ok(())
            }
            Err(e) => {
                drop(slot);
                self.connect_failed(&e);
                Err(e)
            }
        }
    }

    /// A connect ended by `disconnect()`: Disconnected, without a failure.
    fn connect_cancelled(&self) {
        self.shared().log(
            ConsoleKind::Info,
            "Connection attempt cancelled",
            false,
            None,
        );
        self.set_status(ConnectionStatus::Disconnected);
    }

    fn connect_failed(&self, e: &HostError) {
        self.inner.slot.lock().connecting = false;
        let message = format!("Could not connect: {e}");
        self.shared()
            .log(ConsoleKind::Error, message.clone(), false, None);
        self.shared()
            .update(|s| s.last_error = Some(message.clone()));
        self.set_status(ConnectionStatus::Failed(message));
    }

    fn set_status(&self, status: ConnectionStatus) {
        let changed = self.shared().update(|s| {
            let changed = s.status != status;
            s.status = status.clone();
            changed
        });
        if changed {
            self.shared().emit(Event::Status(status));
        }
        self.shared().wake_now();
    }

    /// Close the connection (a running job fails with "disconnected").
    /// Never blocks for long: the engine threads are given a moment to stop
    /// and are detached otherwise.
    pub fn disconnect(&self) {
        let handle = {
            let mut slot = self.inner.slot.lock();
            slot.generation += 1;
            slot.engine.take()
        };
        if let Some(handle) = handle {
            handle.shutdown();
        }
        // The engine reports Disconnected itself; make sure even a detached
        // engine or a connect in progress leaves a consistent state.
        let open = self.with_state(|s| s.status.is_open());
        if open {
            self.set_status(ConnectionStatus::Disconnected);
        }
    }

    /// Close the connection without leaving the printer unsafe: a host
    /// print or an SD upload is cancelled first (the cancel script, which
    /// switches the heaters off; the partial upload closed and deleted),
    /// and the link closes once the printer acknowledged that, or after
    /// `timeout`. A cancel still running then gets every heater and the
    /// part fan switched off (the upload: its file closed) as the link
    /// closes. An SD print is not cancelled: it goes on on the printer,
    /// unwatched; one whose cancel is already under way is waited for like
    /// a host print (nothing is switched off for it at the timeout). Blocks
    /// for up to `timeout` (call it off the UI thread); the summary says
    /// what happened.
    ///
    /// ```no_run
    /// # let printer = sprint_host::Printer::new();
    /// let summary = printer.disconnect_safely(std::time::Duration::from_secs(10));
    /// println!("{summary}");
    /// ```
    pub fn disconnect_safely(&self, timeout: Duration) -> DisconnectSummary {
        let (open, job) = self.with_state(|s| {
            (
                s.status.is_open(),
                s.job
                    .as_ref()
                    .filter(|j| j.status.is_active())
                    .map(|j| (j.kind, j.name.clone(), j.status == JobStatus::Cancelling)),
            )
        });
        let mut summary = DisconnectSummary {
            was_connected: open,
            ..DisconnectSummary::default()
        };
        match job {
            Some((JobKind::SdPrint, name, false)) if open => {
                summary.sd_print_continues = Some(name);
            }
            Some((kind, name, _)) if open => {
                summary.cancel_status = self.cancel_and_wait(timeout);
                if kind == JobKind::SdPrint && summary.cancel_status.is_none() {
                    // Its M524 was not acknowledged: as far as anyone
                    // knows, the card goes on.
                    summary.sd_print_continues = Some(name.clone());
                }
                summary.cancelled = Some((kind, name));
            }
            _ => {}
        }
        self.disconnect();
        summary
    }

    /// Cancel the job and wait (at most `timeout`) until it ends or the
    /// link closes by itself; its final status, or `None` (and a safe
    /// stop) at the timeout.
    fn cancel_and_wait(&self, timeout: Duration) -> Option<JobStatus> {
        self.cancel();
        let deadline = Instant::now() + timeout;
        loop {
            let (status, open) =
                self.with_state(|s| (s.job.as_ref().map(|j| j.status.clone()), s.status.is_open()));
            if let Some(status) = status.filter(JobStatus::is_finished) {
                return Some(status);
            }
            if !open {
                return None;
            }
            if Instant::now() >= deadline {
                self.to_engine(Command::SafeStop);
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The handshake has completed and the printer answers.
    pub fn is_connected(&self) -> bool {
        self.with_state(|s| s.status == ConnectionStatus::Connected)
    }

    /// Reset the printer's board, the way to recover after an emergency
    /// stop or a thermal runaway: Marlin's kill() ignores everything, M999
    /// included, until the board restarts (see
    /// [`PrinterState::restartable`](crate::PrinterState::restartable)).
    ///
    /// * Serial: the port is closed and opened again with a DTR pulse (as
    ///   [`ConnectOptions::reset_on_connect`] does), then the handshake
    ///   runs again; like [`Printer::connect`] this returns once the port
    ///   is open.
    /// * Simulator: the virtual board resets; the engine notices its
    ///   `start` and handshakes again.
    /// * TCP and custom links: [`HostError::Unsupported`]; such a board
    ///   needs its reset button or a power cycle.
    ///
    /// A running job fails. Commands queued before the reset are dropped.
    pub fn reset_board(&self) -> Result<(), HostError> {
        let (target, sim) = {
            let slot = self.inner.slot.lock();
            let Some(engine) = slot.engine.as_ref().filter(|e| !e.is_finished()) else {
                return Err(HostError::NotConnected);
            };
            (slot.target.clone(), engine.sim.clone())
        };
        let log = |text: &str| {
            self.shared().log(ConsoleKind::Info, text, false, None);
            self.shared().wake_now();
        };
        match (target, sim) {
            (_, Some(sim)) => {
                log("Resetting the board");
                sim.reset();
                Ok(())
            }
            (Some((target @ ConnectTarget::Serial { .. }, options)), None) => {
                log("Resetting the board: reopening the port with a DTR pulse");
                self.disconnect();
                let reset = ConnectOptions {
                    reset_on_connect: true,
                    ..options.clone()
                };
                self.connect(target.clone(), reset)?;
                // Later resets (and the remembered options) keep the
                // profile's own setting.
                if let Some(slot) = self.inner.slot.lock().target.as_mut() {
                    *slot = (target, options);
                }
                Ok(())
            }
            (Some((ConnectTarget::Tcp { .. }, _)), None) => Err(HostError::Unsupported(
                "a board behind a network bridge cannot be reset from here; press its reset \
                 button or power-cycle it, then reconnect"
                    .to_string(),
            )),
            _ => Err(HostError::Unsupported(
                "this connection cannot reset the board; press its reset button or \
                 power-cycle it, then reconnect"
                    .to_string(),
            )),
        }
    }

    /// The control handle of the virtual printer, while connected to one
    /// (tests and demos: fault injection, runout, reset).
    pub fn sim_control(&self) -> Option<SimControl> {
        self.inner
            .slot
            .lock()
            .engine
            .as_ref()
            .and_then(|e| e.sim.clone())
    }

    /// Use `profile` for script templates, parking and geometry (pause,
    /// resume and cancel scripts unless [`PrintOptions`] overrides them).
    pub fn set_profile(&self, profile: PrinterProfile) {
        *self.shared().profile.write() = profile;
    }

    /// The profile in use.
    pub fn profile(&self) -> PrinterProfile {
        self.shared().profile.read().clone()
    }

    // ----- sending -----

    /// Send to the engine, if there is one.
    fn to_engine(&self, cmd: Command) -> bool {
        self.inner
            .slot
            .lock()
            .engine
            .as_ref()
            .is_some_and(|e| e.tx.send(cmd).is_ok())
    }

    fn command(&self, cmd: Command) -> bool {
        let sent = self.to_engine(cmd);
        if !sent {
            self.shared().log(
                ConsoleKind::Warning,
                "Not connected: command ignored",
                false,
                None,
            );
            self.shared().wake_now();
        }
        sent
    }

    fn request(
        &self,
        make: impl FnOnce(crossbeam_channel::Sender<Result<(), HostError>>) -> Command,
    ) -> Result<(), HostError> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let sent = self.to_engine(make(tx));
        if !sent {
            return Err(HostError::NotConnected);
        }
        rx.recv_timeout(REPLY_TIMEOUT)
            .unwrap_or_else(|_| Err(HostError::Timeout("the engine did not answer".to_string())))
    }

    /// Queue G-code typed by the user or produced by UI controls. Several
    /// lines are allowed; comments and blank lines are dropped. These are
    /// sent before pending print-stream lines, and before a pause, resume
    /// or cancel script requested after them (a purge typed while paused
    /// runs before the resume script moves back onto the part). Emergency
    /// commands (M112, M108, M410, M876) bypass the queue automatically;
    /// M410 during a host print also stops the print (the firmware threw
    /// its planned moves away).
    pub fn send(&self, gcode: &str) {
        self.send_lines(gcode);
    }

    /// [`Printer::send`]; false when there was no engine to take them.
    fn send_lines(&self, gcode: &str) -> bool {
        let mut taken = true;
        let mut queued = Vec::new();
        for line in command_lines(gcode) {
            if is_emergency_command(&line) {
                if !queued.is_empty() {
                    taken &= self.command(Command::Send(std::mem::take(&mut queued)));
                }
                if line.to_ascii_uppercase().starts_with("M112") {
                    taken &= self.command(Command::EmergencyStop);
                } else {
                    taken &= self.command(Command::Immediate(line));
                }
            } else {
                queued.push(line);
            }
        }
        if !queued.is_empty() {
            taken &= self.command(Command::Send(queued));
        }
        taken
    }

    /// [`Printer::send`], following the last queued command: the returned
    /// [`Tracked`] tells when the firmware acknowledged it (its `ok`),
    /// which is later than now when the printer is busy, for example
    /// waiting for a heater (the commands before it are then done too).
    /// Emergency commands bypass the queue and are not followed; with no
    /// other command the tracker reports [`Delivery::Lost`].
    pub fn send_tracked(&self, gcode: &str) -> Tracked {
        let lines: Vec<String> = command_lines(gcode).collect();
        let Some(at) = lines.iter().rposition(|line| !is_emergency_command(line)) else {
            self.send_lines(gcode);
            return Tracked::lost();
        };
        if at > 0 {
            self.send_lines(&lines[..at].join("\n"));
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let queued = self.command(Command::SendTracked {
            line: lines[at].clone(),
            ack: tx,
        });
        if at + 1 < lines.len() {
            self.send_lines(&lines[at + 1..].join("\n"));
        }
        if queued {
            Tracked::new(rx)
        } else {
            Tracked::lost()
        }
    }

    /// Send right away, bypassing the queue and flow control (unnumbered).
    pub fn send_immediate(&self, gcode: &str) {
        for line in command_lines(gcode) {
            self.command(Command::Immediate(line));
        }
    }

    /// M112 now: the firmware halts, the job is aborted, the queue is
    /// dropped and the printer is marked halted.
    pub fn emergency_stop(&self) {
        self.command(Command::EmergencyStop);
    }

    /// Start streaming `file`. Fails unless connected, not halted and no
    /// job is running.
    pub fn start_print(
        &self,
        file: Arc<GcodeFile>,
        options: PrintOptions,
    ) -> Result<(), HostError> {
        self.request(|reply| Command::StartPrint {
            file,
            options,
            reply,
        })
    }

    /// Pause the job (host prints: wait for in-flight commands, then run the
    /// pause script; SD prints: M25). A host print the firmware holds at
    /// an M0/M1 of the file is paused the same way: the wait is ended
    /// (M108, on firmware that reports EMERGENCY_PARSER; any other cannot
    /// read it while it waits, so the pause waits for the printer's
    /// button) and the pause script runs.
    pub fn pause(&self) {
        self.command(Command::Pause);
    }

    /// Resume a paused job (host prints: run the resume script, then
    /// continue streaming; SD prints: M24). A host print the firmware
    /// paused ([`JobInfo::paused_by_firmware`](crate::JobInfo::paused_by_firmware))
    /// gets no resume script: the firmware continues, with one M108 when
    /// it waits for the user ([`PrinterState::waiting_for_user`]).
    pub fn resume(&self) {
        self.command(Command::Resume);
    }

    /// Pause the running host print (with the pause script) before each of
    /// `lines`, 0-based file lines not streamed yet, as if the file had a
    /// `;@pause` there; replaces earlier pause points (an empty list
    /// removes them). "Pause at layer N" is the first line of the layer
    /// ([`Toolpath`](crate::sprint_gcode::Toolpath) layers). The points
    /// still ahead are [`JobInfo::pause_at`](crate::JobInfo::pause_at).
    pub fn pause_at_lines(&self, lines: &[usize]) {
        self.command(Command::PauseAt(lines.to_vec()));
    }

    /// Stop printing one object of the running host print (cancel
    /// object): the moves, extrusions, retractions and primes
    /// of its parts not streamed yet are left out, and after each of them
    /// the printer travels to where the file is (with `G92 E` for absolute
    /// extrusion). `name` is one of
    /// [`JobInfo::objects`](crate::JobInfo::objects) (object labels in the
    /// file); unknown names are reported in the console.
    pub fn exclude_object(&self, name: &str) {
        self.command(Command::ExcludeObject {
            name: name.to_string(),
            exclude: true,
        });
    }

    /// Undo [`Printer::exclude_object`]: the parts of the object not
    /// streamed yet are printed again.
    pub fn include_object(&self, name: &str) {
        self.command(Command::ExcludeObject {
            name: name.to_string(),
            exclude: false,
        });
    }

    /// Cancel the job (host prints: drop the stream and run the cancel
    /// script; SD prints: M524; uploads: close and delete the file).
    pub fn cancel(&self) {
        self.command(Command::Cancel);
    }

    /// Print a file from the SD card: `M23` + `M24`, then `M27 S<n>` auto-reports
    /// (or M27 polling).
    pub fn start_sd_print(&self, filename: &str) -> Result<(), HostError> {
        let name = filename.to_string();
        self.request(|reply| Command::StartSdPrint { name, reply })
    }

    /// Write `file` to the SD card as `sd_name` (M28 ... M29, every line
    /// numbered with a checksum) as a job.
    pub fn upload_to_sd(&self, file: Arc<GcodeFile>, sd_name: &str) -> Result<(), HostError> {
        let name = sd_name.to_string();
        self.request(|reply| Command::UploadToSd { file, name, reply })
    }

    /// Answer the active host prompt (`M876 S<index>`) and close it.
    pub fn answer_prompt(&self, button: usize) {
        self.command(Command::AnswerPrompt(button));
    }

    // ----- state -----

    /// Run `f` with the current state (read lock held; keep it short).
    pub fn with_state<R>(&self, f: impl FnOnce(&PrinterState) -> R) -> R {
        f(&self.shared().state.read())
    }

    /// A full copy of the state (the console is separate).
    pub fn snapshot(&self) -> PrinterState {
        self.shared().state.read().clone()
    }

    /// A receiver of every future event.
    pub fn subscribe(&self) -> Receiver<Event> {
        self.shared().bus.subscribe()
    }

    /// Called (from engine threads, at most every 16 ms, never losing the
    /// last change) whenever the state changes; UIs use it to request a
    /// repaint. The link clock ([`LinkStats::connected_secs`](crate::LinkStats::connected_secs))
    /// alone wakes it once per second. Replaces any previous waker.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        self.shared().set_waker(Arc::new(waker));
    }

    /// Console entries with `seq > after`, at most `max`, oldest first.
    pub fn console_since(&self, after: u64, max: usize) -> Vec<ConsoleEntry> {
        self.shared().console.lock().since(after, max)
    }

    /// Remove every console entry.
    pub fn clear_console(&self) {
        self.shared().console.lock().clear();
        self.shared().request_wake();
        self.shared().wake_now();
    }

    /// Change how many console lines are kept.
    pub fn set_console_capacity(&self, lines: usize) {
        self.shared().console.lock().set_capacity(lines);
    }

    // ----- convenience controls -----

    /// Home the given axes (`G28 [X] [Y] [Z]`).
    pub fn home(&self, axes: Axes) {
        self.send(&axes.home_command());
    }

    /// Move one axis relative to its position (`G91`, `G1`, `G90`). Jogging
    /// E extrudes with the active tool.
    pub fn jog(&self, axis: Axis, distance_mm: f32, feedrate_mm_min: f32) {
        if !distance_mm.is_finite() || distance_mm == 0.0 {
            return;
        }
        if axis == Axis::E {
            let tool = self.with_state(|s| s.active_tool);
            self.extrude(tool, distance_mm, feedrate_mm_min);
            return;
        }
        self.send(&format!(
            "G91\nG1 {}{} F{}\nG90",
            axis.letter(),
            format_number(f64::from(distance_mm), 3),
            feed(feedrate_mm_min)
        ));
    }

    /// Move to an absolute position (`G90`, `G1 X.. Y.. Z..`); `None` axes
    /// stay where they are.
    pub fn move_to(&self, x: Option<f32>, y: Option<f32>, z: Option<f32>, feedrate_mm_min: f32) {
        let mut cmd = String::from("G1");
        for (letter, v) in [('X', x), ('Y', y), ('Z', z)] {
            if let Some(v) = v.filter(|v| v.is_finite()) {
                cmd.push_str(&format!(" {letter}{}", format_number(f64::from(v), 3)));
            }
        }
        if cmd.len() == 2 {
            return;
        }
        cmd.push_str(&format!(" F{}", feed(feedrate_mm_min)));
        self.send(&format!("G90\n{cmd}"));
    }

    /// Extrude (negative = retract) with `tool`: `M83`, `G1 E..`, and `M82`
    /// again if the printer was in absolute extrusion mode.
    pub fn extrude(&self, tool: u8, length_mm: f32, feedrate_mm_min: f32) {
        self.command(Command::Extrude {
            tool,
            length: length_mm,
            feedrate: feedrate_mm_min,
        });
    }

    /// Set a heater target (`M104 T<n> S`, `M140 S`, `M141 S`); 0 = off.
    pub fn set_temperature(&self, heater: Heater, celsius: f32) {
        let t = format_number(f64::from(clamp_temp(celsius)), 1);
        self.send(&match heater {
            Heater::Hotend(tool) => format!("M104 T{tool} S{t}"),
            Heater::Bed => format!("M140 S{t}"),
            Heater::Chamber => format!("M141 S{t}"),
        });
    }

    /// Set a fan speed 0..=255 (`M106 P<fan> S<speed>`, `M107` for 0).
    pub fn set_fan(&self, fan: u8, speed: u8) {
        let p = if fan == 0 {
            String::new()
        } else {
            format!(" P{fan}")
        };
        if speed == 0 {
            self.send(&format!("M107{p}"));
        } else {
            self.send(&format!("M106{p} S{speed}"));
        }
    }

    /// Feedrate override in percent (`M220 S`).
    pub fn set_feedrate_override(&self, percent: f32) {
        self.send(&format!("M220 S{}", percent_value(percent)));
    }

    /// Flow override of `tool` in percent (`M221 T S`).
    pub fn set_flow(&self, tool: u8, percent: f32) {
        self.send(&format!("M221 T{tool} S{}", percent_value(percent)));
    }

    /// Babystep Z by `mm` (`M290 Z`).
    pub fn babystep_z(&self, mm: f32) {
        if mm.is_finite() && mm != 0.0 {
            self.send(&format!(
                "M290 Z{}",
                format_number(f64::from(mm.clamp(-5.0, 5.0)), 3)
            ));
        }
    }

    /// Disable the steppers (`M84`).
    pub fn motors_off(&self) {
        self.send("M84");
    }

    /// Select a tool (`T<n>`).
    pub fn select_tool(&self, tool: u8) {
        self.send(&format!("T{tool}"));
    }

    /// Queue a query of the host's own: like [`Printer::send`], but the
    /// firmware's `echo:` replies and "Unknown command" stay in the console
    /// instead of becoming [`Event::FirmwareMessage`]s (UIs send these on
    /// their own, the user did not ask for messages).
    fn query(&self, gcode: &str) {
        let lines: Vec<String> = command_lines(gcode).collect();
        if !lines.is_empty() {
            self.command(Command::Query(lines));
        }
    }

    /// `M105`.
    pub fn request_temperatures(&self) {
        self.query("M105");
    }

    /// `M114`.
    pub fn request_position(&self) {
        self.query("M114");
    }

    /// `M115`.
    pub fn request_firmware_info(&self) {
        self.query("M115");
    }

    /// `M503`: replaces `state.settings` when the report completes.
    pub fn request_settings(&self) {
        self.query("M503");
    }

    /// `M500`: store the settings in EEPROM. The tracker tells when the
    /// firmware has done it, which can be much later than now when it is
    /// busy (a heat wait, a long move).
    pub fn save_settings(&self) -> Tracked {
        self.send_tracked("M500")
    }

    /// `M501` (load from EEPROM), then `M503`.
    pub fn load_settings(&self) {
        self.send("M501\nM503");
    }

    /// `M502` (factory defaults; the UI must confirm first), then `M503`.
    pub fn factory_reset_settings(&self) {
        self.send("M502\nM503");
    }

    /// Send `setting.to_gcode()`, update `state.settings` optimistically
    /// and refresh them with M503 shortly after the last edit.
    ///
    /// A [report-only](SettingCommand::is_report_only) setting (Marlin's
    /// Autotemp, reported as `M104 S B F`) is not sent: the console gets a
    /// warning instead, as sending it would heat or move the printer.
    pub fn apply_setting(&self, setting: &SettingCommand) {
        self.command(Command::ApplySetting(setting.clone()));
    }

    /// `M119`.
    pub fn request_endstops(&self) {
        self.query("M119");
    }

    /// `M21` if the card is not known to be mounted, then `M20 L` (long
    /// names, when the firmware supports EXTENDED_M20) or `M20`.
    pub fn request_sd_files(&self) {
        let (mounted, extended) = self.with_state(|s| {
            (
                s.sd.card_present == Some(true),
                s.has_capability("EXTENDED_M20"),
            )
        });
        let mut gcode = String::new();
        if !mounted {
            gcode.push_str("M21\n");
        }
        gcode.push_str(if extended { "M20 L" } else { "M20" });
        self.query(&gcode);
    }

    /// Delete a file from the SD card (`M30`).
    pub fn delete_sd_file(&self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !name.contains(['\n', '\r']) {
            self.send(&format!("M30 {name}"));
        }
    }

    /// Report the bed mesh: `M420 V` (Marlin) or `BED_MESH_OUTPUT`
    /// (Klipper).
    pub fn request_bed_mesh(&self) {
        let klipper = self.with_state(|s| {
            s.firmware
                .as_ref()
                .is_some_and(|f| f.flavor == FirmwareFlavor::Klipper)
        });
        self.query(if klipper { "BED_MESH_OUTPUT" } else { "M420 V" });
    }

    /// Probe the bed: `G28` (unless every axis is known to be homed, see
    /// [`PrinterState::homed`](crate::PrinterState::homed)), `G29`, then
    /// report the mesh.
    pub fn probe_bed_mesh(&self) {
        self.command(Command::ProbeBedMesh);
    }

    /// `M78` (print statistics).
    pub fn request_stats(&self) {
        self.query("M78");
    }

    /// PID autotune: `M303 E<n|-1|-2> S<target> C<cycles> U1`. The result
    /// arrives as `state.autotune` and [`Event::AutotuneFinished`], after
    /// which the settings are refreshed. A failure ends it with
    /// `running` false, `error` set and [`Event::AutotuneFailed`]: one the
    /// firmware reports, or a halt, a board reset or the connection
    /// closing that cuts it short. While the printer is halted it fails
    /// at once, without sending M303.
    pub fn start_pid_autotune(&self, heater: Heater, target: f32, cycles: u32) {
        self.command(Command::Autotune {
            heater,
            target: clamp_temp(target),
            cycles: cycles.clamp(1, 100),
        });
    }

    /// `M999` (`FIRMWARE_RESTART` on Klipper while it is shut down; never
    /// otherwise, since it reboots the MCUs and would abort a print):
    /// restart a halted firmware. If the board resets instead, the engine
    /// reconnects by itself.
    ///
    /// M999 alone does not recover Marlin after M112 or a thermal runaway
    /// (kill() ignores it); check
    /// [`PrinterState::restartable`](crate::PrinterState::restartable) and
    /// offer [`Printer::reset_board`] otherwise.
    pub fn firmware_restart(&self) {
        self.command(Command::FirmwareRestart);
    }

    /// Send a macro without arguments: [`Printer::run_macro_with_args`]
    /// with none, so `{n=default}` placeholders get their defaults and
    /// `{args}` is empty. A macro that needs arguments (a `{n}` without a
    /// default) is not sent: the console says why.
    pub fn run_macro(&self, m: &Macro) {
        let _ = self.run_macro_with_args(m, &[]);
    }

    /// Send a macro with arguments: `{0}`, `{1}`, ... (`{1=60}` with a
    /// default) and `{args}` are filled in from `args` as the shell's and
    /// the TUI's `macro` command do ([`expand_macro_args`]), then the
    /// machine-state placeholders are expanded with the current state (see
    /// [`script`](crate::script)).
    ///
    /// A macro whose arguments cannot be filled in (one without a default
    /// is missing, or arguments for a macro that takes none) is not sent:
    /// [`HostError::InvalidArgument`] and an error in the console, never
    /// placeholders sent verbatim. [`HostError::NotConnected`] when there
    /// is no connection (the commands are dropped with a console warning).
    pub fn run_macro_with_args(&self, m: &Macro, args: &[String]) -> Result<(), HostError> {
        let gcode = match expand_macro_args(&m.gcode, args) {
            Ok(gcode) => gcode,
            Err(why) => {
                let message = format!("Macro {} not sent: it {why}", m.name);
                self.shared()
                    .log(ConsoleKind::Error, message.clone(), false, None);
                self.shared().wake_now();
                return Err(HostError::InvalidArgument(message));
            }
        };
        let (state, temps) = self.with_state(|s| (machine_state(s), s.temps.clone()));
        let lines = {
            let profile = self.shared().profile.read();
            render_script_with(&gcode, &profile.script_context(&state).covering(&temps))
        };
        if self.send_lines(&lines.join("\n")) {
            Ok(())
        } else {
            Err(HostError::NotConnected)
        }
    }
}

/// A [`MachineState`] from what the host knows about the printer, for
/// templating macros.
fn machine_state(s: &PrinterState) -> MachineState {
    let mut m = MachineState::new();
    if let Some(p) = &s.position {
        m.position = [p.x, p.y, p.z, p.e].map(f64::from);
    }
    m.tool = s.active_tool;
    m.hotend_targets = s
        .temps
        .hotends
        .iter()
        .map(|h| h.target.unwrap_or(0.0))
        .collect();
    if m.hotend_targets.is_empty() {
        m.hotend_targets.push(0.0);
    }
    m.bed_target = s.temps.bed.as_ref().and_then(|b| b.target).unwrap_or(0.0);
    m.fan = s.fan_speeds.first().copied().unwrap_or(0);
    m.feedrate_pct = s.feedrate_pct;
    m
}

fn feed(feedrate_mm_min: f32) -> String {
    let f = if feedrate_mm_min.is_finite() && feedrate_mm_min > 0.0 {
        feedrate_mm_min.min(100_000.0)
    } else {
        1000.0
    };
    format_number(f64::from(f), 1)
}

fn clamp_temp(celsius: f32) -> f32 {
    if celsius.is_finite() {
        celsius.clamp(0.0, 500.0)
    } else {
        0.0
    }
}

fn percent_value(percent: f32) -> String {
    let p = if percent.is_finite() {
        percent.clamp(1.0, 1000.0)
    } else {
        100.0
    };
    format_number(f64::from(p), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(feed(f32::NAN), "1000");
        assert_eq!(feed(3000.0), "3000");
        assert_eq!(clamp_temp(-5.0), 0.0);
        assert_eq!(clamp_temp(f32::INFINITY), 0.0);
        assert_eq!(percent_value(0.0), "1");
        assert_eq!(percent_value(f32::NAN), "100");
        let lines: Vec<String> = command_lines("G28 ; home\n\n m105\r\n;x").collect();
        assert_eq!(lines, ["G28", "M105"]);
    }

    #[test]
    fn commands_without_a_connection_are_ignored_with_a_warning() {
        let p = Printer::new();
        p.send("G28");
        p.home(Axes::ALL);
        assert!(!p.is_connected());
        assert!(matches!(
            p.start_print(
                Arc::new(GcodeFile::from_text("x", "G28")),
                PrintOptions::default()
            ),
            Err(HostError::NotConnected)
        ));
        let console = p.console_since(0, 10);
        assert!(console.iter().all(|e| e.kind == ConsoleKind::Warning));
        assert_eq!(console.len(), 2);
        p.disconnect();
        assert_eq!(
            p.with_state(|s| s.status.clone()),
            ConnectionStatus::Disconnected
        );
    }

    #[test]
    fn machine_state_from_printer_state() {
        let s = PrinterState {
            position: Some(sprint_proto::PositionReport {
                x: 1.0,
                y: 2.0,
                z: 3.0,
                e: 4.0,
                stepper_counts: None,
            }),
            active_tool: 1,
            ..PrinterState::default()
        };
        let m = machine_state(&s);
        assert_eq!(m.position, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(m.tool, 1);
        assert_eq!(m.hotend_targets, [0.0]);
    }
}
