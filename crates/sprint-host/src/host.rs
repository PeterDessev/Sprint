//! The [`Printer`] handle: the API UIs use to connect, send, print and read
//! the printer state.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use sprint_gcode::{GcodeFile, MachineState, clean_line};
use sprint_proto::{FirmwareFlavor, SettingCommand, is_emergency_command};
use sprint_sim::SimControl;

use crate::config::{ConnectOptions, DEFAULT_CONSOLE_LINES, Macro, PrinterProfile};
use crate::console::{ConsoleEntry, ConsoleKind};
use crate::engine::{self, Command, EngineHandle, Shared};
use crate::error::HostError;
use crate::event::Event;
use crate::job::PrintOptions;
use crate::script::{format_number, render_script_with};
use crate::state::{Axes, Axis, ConnectionStatus, Heater, PrinterState};
use crate::transport::{ConnectTarget, Link};

/// How long a request that needs the engine's answer may take.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// (baud 0) probes each rate before returning.
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
        let link = Link::open(&target, options.reset_on_connect, &mut |message| {
            shared.log(ConsoleKind::Info, message, false, None);
        });
        match link {
            Ok(link) => self.finish_connect(link, label, options, generation, Some(target)),
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
            self.set_status(ConnectionStatus::Disconnected);
            return Err(HostError::NotConnected);
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
        let mut queued = Vec::new();
        for line in command_lines(gcode) {
            if is_emergency_command(&line) {
                if !queued.is_empty() {
                    self.command(Command::Send(std::mem::take(&mut queued)));
                }
                if line.to_ascii_uppercase().starts_with("M112") {
                    self.command(Command::EmergencyStop);
                } else {
                    self.command(Command::Immediate(line));
                }
            } else {
                queued.push(line);
            }
        }
        if !queued.is_empty() {
            self.command(Command::Send(queued));
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
    /// pause script; SD prints: M25).
    pub fn pause(&self) {
        self.command(Command::Pause);
    }

    /// Resume a paused job (host prints: run the resume script, then
    /// continue streaming; SD prints: M24).
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

    /// Stop printing one object of the running host print (cancel object,
    /// Printrun's Excluder): the moves, extrusions, retractions and primes
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

    /// `M105`.
    pub fn request_temperatures(&self) {
        self.send("M105");
    }

    /// `M114`.
    pub fn request_position(&self) {
        self.send("M114");
    }

    /// `M115`.
    pub fn request_firmware_info(&self) {
        self.send("M115");
    }

    /// `M503`: replaces `state.settings` when the report completes.
    pub fn request_settings(&self) {
        self.send("M503");
    }

    /// `M500`: store the settings in EEPROM.
    pub fn save_settings(&self) {
        self.send("M500");
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
    pub fn apply_setting(&self, setting: &SettingCommand) {
        self.command(Command::ApplySetting(setting.clone()));
    }

    /// `M119`.
    pub fn request_endstops(&self) {
        self.send("M119");
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
        self.send(&gcode);
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
        self.send(if klipper { "BED_MESH_OUTPUT" } else { "M420 V" });
    }

    /// Probe the bed: `G28` (unless every axis is known to be homed, see
    /// [`PrinterState::homed`](crate::PrinterState::homed)), `G29`, then
    /// report the mesh.
    pub fn probe_bed_mesh(&self) {
        self.command(Command::ProbeBedMesh);
    }

    /// `M78` (print statistics).
    pub fn request_stats(&self) {
        self.send("M78");
    }

    /// PID autotune: `M303 E<n|-1|-2> S<target> C<cycles> U1`. The result
    /// arrives as `state.autotune` and [`Event::AutotuneFinished`], after
    /// which the settings are refreshed.
    pub fn start_pid_autotune(&self, heater: Heater, target: f32, cycles: u32) {
        self.command(Command::Autotune {
            heater,
            target: clamp_temp(target),
            cycles: cycles.clamp(1, 100),
        });
    }

    /// `M999` (`FIRMWARE_RESTART` on Klipper): restart a halted firmware.
    /// If the board resets instead, the engine reconnects by itself.
    ///
    /// M999 alone does not recover Marlin after M112 or a thermal runaway
    /// (kill() ignores it); check
    /// [`PrinterState::restartable`](crate::PrinterState::restartable) and
    /// offer [`Printer::reset_board`] otherwise.
    pub fn firmware_restart(&self) {
        self.command(Command::FirmwareRestart);
    }

    /// Send a macro, its placeholders expanded with the current state (see
    /// [`script`](crate::script)).
    pub fn run_macro(&self, m: &Macro) {
        let (state, temps) = self.with_state(|s| (machine_state(s), s.temps.clone()));
        let lines = {
            let profile = self.shared().profile.read();
            render_script_with(&m.gcode, &profile.script_context(&state).covering(&temps))
        };
        self.send(&lines.join("\n"));
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
