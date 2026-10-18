//! The simulated firmware: line protocol, command queue, executor and the
//! periodic tasks. A [`Machine`] is owned by the simulation thread but is
//! fully deterministic and can be driven directly in unit tests: feed
//! bytes, advance simulated time, collect output lines.

mod commands;
mod host;
mod motion;
mod reports;
mod sdcmds;
mod temps;
#[cfg(test)]
mod tests;

use crate::config::SimConfig;
use crate::line::{self, Emergency};
use crate::link::SimSnapshot;
use crate::mesh::{BedShape, Mesh};
use crate::parse::Command;
use crate::planner::{MoveKind, Planner};
use crate::rng::{Rng, crc16};
use crate::sd::SdCard;
use crate::settings::{EEPROM_VERSION, ReportContext, Settings};
use crate::thermal::{Control, Heater, HeaterParams};
use std::collections::VecDeque;

pub(crate) use motion::Motion;

/// Largest simulated step in seconds.
const MAX_STEP: f64 = 0.05;
/// Bound on steps per `advance` call (1000 simulated seconds).
const MAX_STEPS_PER_ADVANCE: usize = 20_000;
/// Longest accepted line; extra characters are dropped like Marlin does.
const MAX_LINE: usize = 512;
/// Bytes of complete lines that may wait for queue space.
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
/// Marlin DEFAULT_KEEPALIVE_INTERVAL.
const KEEPALIVE: f64 = 2.0;
/// Number of fans (P0..P2).
pub(crate) const FAN_COUNT: usize = 3;
/// Marlin EXTRUDE_MINTEMP default.
const EXTRUDE_MINTEMP: f64 = 170.0;

/// Where a queued command came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// From the host; "ok" is sent when it completes.
    Serial { line: Option<i64> },
    /// Read from the SD card while printing.
    Sd,
    /// Injected by the firmware itself (runout, prompt responses).
    Injected,
}

/// A command waiting in the command buffer.
#[derive(Clone, Debug)]
struct Queued {
    text: String,
    origin: Origin,
}

/// Board state.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RunState {
    /// Resetting; input is discarded until the banner is printed.
    Booting {
        until: f64,
    },
    Running,
    /// Killed by M112; ignores everything until reset.
    Halted,
}

/// A planner segment waiting for a free slot.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Segment {
    pub target: [f64; 4],
    pub feed: f64,
    pub kind: MoveKind,
}

/// A command that takes (simulated) time.
pub(crate) struct Task {
    origin: Origin,
    kind: TaskKind,
    /// Waiting for the planner to drain before starting.
    sync: bool,
    /// Time of the next "busy:" keepalive.
    busy_next: f64,
}

/// What a running command is doing.
pub(crate) enum TaskKind {
    /// Waiting for planner slots for these segments.
    Moves(VecDeque<Segment>),
    /// M400 / plain synchronisation.
    Sync,
    /// G4.
    Dwell { secs: f64, until: f64 },
    /// G28.
    Home(motion::Homing),
    /// G29.
    Probe(motion::Probing),
    /// M109 / M190 / M191.
    HeatWait(temps::HeatWait),
    /// M303.
    Autotune(temps::Tune),
    /// M600.
    FilamentChange(host::FilamentChange),
    /// End of an SD print (Marlin's M1001).
    SdFinish,
    /// M18/M84 after the planner drained.
    DisableSteppers,
}

/// Result of starting a command.
pub(crate) enum Outcome {
    /// Finished; send "ok".
    Done,
    /// Finished; the handler already replied (or no reply is due).
    NoOk,
    /// Runs for a while.
    Task { kind: TaskKind, sync: bool },
}

/// Heater identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaterId {
    Hotend(usize),
    Bed,
    Chamber,
}

/// The simulated printer.
pub(crate) struct Machine {
    cfg: SimConfig,
    now: f64,
    run: RunState,
    out: Vec<String>,

    // Serial link and command buffer.
    partial: Vec<u8>,
    pending: VecDeque<String>,
    pending_bytes: usize,
    last_n: i64,
    queue: VecDeque<Queued>,
    injected: VecDeque<String>,
    task: Option<Task>,
    received: Vec<String>,
    commands_received: usize,
    resends: usize,
    fault_rng: Rng,
    noise_rng: Rng,
    checksum_error_rate: f32,
    drop_ok_rate: f32,

    // Motion.
    motion: Motion,
    planner: Planner,

    // Thermal.
    hotends: Vec<Heater>,
    bed: Option<Heater>,
    chamber: Option<Heater>,
    fans: [u8; FAN_COUNT],
    allow_cold_extrude: bool,
    extrude_min_temp: f64,

    // Settings and EEPROM.
    settings: Settings,
    eeprom: Settings,

    // Leveling.
    bed_shape: BedShape,
    mesh: Option<Mesh>,
    eeprom_mesh: Option<Mesh>,
    leveling: bool,
    probe_runs: u64,

    // SD card.
    sd: SdCard,
    sd_mounted: bool,
    sd_job: Option<sdcmds::SdJob>,
    saving: Option<usize>,

    // Print job timer and statistics.
    timer: reports::JobTimer,
    stats: reports::Stats,

    // Auto-reports (interval 0 = off).
    temp_interval: f64,
    next_temp: f64,
    pos_interval: f64,
    next_pos: f64,
    sd_interval: f64,
    next_sd: f64,

    // Host interaction.
    wait_for_user: bool,
    wait_for_heatup: bool,
    prompt: host::Prompt,
    runout: bool,
    lcd_message: String,
}

impl Machine {
    /// A powered-on printer that will print its banner after the boot delay.
    pub(crate) fn new(cfg: SimConfig) -> Machine {
        let cfg = cfg.sanitized();
        let n = usize::from(cfg.extruders);
        let ambient = f64::from(cfg.ambient);
        let settings = Settings::defaults(n);
        let hotends = (0..n)
            .map(|_| Heater::new(HeaterParams::hotend(), Control::Pid, ambient))
            .collect();
        let bed = cfg
            .heated_bed
            .then(|| Heater::new(HeaterParams::bed(), Control::Pid, ambient));
        let chamber = cfg
            .heated_chamber
            .then(|| Heater::new(HeaterParams::chamber(), Control::BangBang, ambient));
        let mut m = Machine {
            now: 0.0,
            run: RunState::Booting { until: 0.0 },
            out: Vec::new(),
            partial: Vec::new(),
            pending: VecDeque::new(),
            pending_bytes: 0,
            last_n: 0,
            queue: VecDeque::new(),
            injected: VecDeque::new(),
            task: None,
            received: Vec::new(),
            commands_received: 0,
            resends: 0,
            fault_rng: Rng::new(cfg.seed, 1),
            noise_rng: Rng::new(cfg.seed, 2),
            checksum_error_rate: cfg.checksum_error_rate,
            drop_ok_rate: cfg.drop_ok_rate,
            motion: Motion::new(n),
            planner: Planner::new(cfg.planner_buffer, cfg.instant_moves),
            hotends,
            bed,
            chamber,
            fans: [0; FAN_COUNT],
            allow_cold_extrude: false,
            extrude_min_temp: EXTRUDE_MINTEMP,
            eeprom: settings.clone(),
            settings,
            bed_shape: BedShape::new(cfg.seed),
            mesh: None,
            eeprom_mesh: None,
            leveling: false,
            probe_runs: 0,
            sd: SdCard::new(&cfg.sd_files, cfg.seed),
            sd_mounted: true,
            sd_job: None,
            saving: None,
            timer: reports::JobTimer::default(),
            stats: reports::Stats::default(),
            temp_interval: 0.0,
            next_temp: 0.0,
            pos_interval: 0.0,
            next_pos: 0.0,
            sd_interval: 0.0,
            next_sd: 0.0,
            wait_for_user: false,
            wait_for_heatup: false,
            prompt: host::Prompt::None,
            runout: false,
            lcd_message: String::new(),
            cfg,
        };
        m.power_on();
        m
    }

    /// Reset every volatile part of the firmware state and start booting.
    /// Physical state (temperatures), the EEPROM, statistics and the SD
    /// card survive.
    fn power_on(&mut self) {
        let n = usize::from(self.cfg.extruders);
        let ambient = f64::from(self.cfg.ambient);
        let boot = self.cfg.boot_delay_ms as f64 / 1000.0;
        self.run = RunState::Booting {
            until: self.now + boot,
        };
        self.partial.clear();
        self.pending.clear();
        self.pending_bytes = 0;
        self.last_n = 0;
        self.queue.clear();
        self.injected.clear();
        self.task = None;
        self.motion = Motion::new(n);
        self.planner = Planner::new(self.cfg.planner_buffer, self.cfg.instant_moves);
        let renew = |old: &Heater, params: HeaterParams, control: Control| {
            let mut h = Heater::new(params, control, ambient);
            h.keep_physics_of(old);
            h
        };
        self.hotends = self
            .hotends
            .iter()
            .map(|h| renew(h, HeaterParams::hotend(), Control::Pid))
            .collect();
        self.bed = self
            .bed
            .as_ref()
            .map(|h| renew(h, HeaterParams::bed(), Control::Pid));
        self.chamber = self
            .chamber
            .as_ref()
            .map(|h| renew(h, HeaterParams::chamber(), Control::BangBang));
        self.fans = [0; FAN_COUNT];
        self.allow_cold_extrude = false;
        self.extrude_min_temp = EXTRUDE_MINTEMP;
        self.settings = self.eeprom.clone();
        self.mesh = self.eeprom_mesh.clone();
        self.leveling = false;
        self.sd_mounted = true;
        self.sd_job = None;
        self.saving = None;
        self.timer = reports::JobTimer::default();
        self.temp_interval = 0.0;
        self.pos_interval = 0.0;
        self.sd_interval = 0.0;
        self.wait_for_user = false;
        self.wait_for_heatup = false;
        self.prompt = host::Prompt::None;
        self.runout = false;
        self.lcd_message.clear();
    }

    // ----- interface used by the simulation thread -----

    /// Bytes from the host.
    pub(crate) fn on_bytes(&mut self, data: &[u8]) {
        for &b in data {
            if self.run != RunState::Running {
                // A resetting or killed board drops everything it receives.
                self.partial.clear();
                return;
            }
            if b == b'\n' || b == b'\r' {
                if !self.partial.is_empty() {
                    let raw = String::from_utf8_lossy(&self.partial).into_owned();
                    self.partial.clear();
                    self.on_line(raw);
                }
            } else if self.partial.len() < MAX_LINE {
                self.partial.push(b);
            }
        }
    }

    /// A raw line to emit as if the firmware printed it.
    pub(crate) fn inject_line(&mut self, line: &str) {
        for l in line.split(['\n', '\r']) {
            if !l.is_empty() {
                self.out.push(l.to_string());
            }
        }
    }

    /// Simulate the filament runout sensor triggering.
    pub(crate) fn filament_runout(&mut self) {
        if self.run != RunState::Running {
            return;
        }
        self.runout_event();
    }

    /// Simulate a board reset.
    pub(crate) fn reset(&mut self) {
        if let Some(job) = &self.sd_job
            && job.started
        {
            self.timer.abort(self.now, &mut self.stats);
        }
        self.power_on();
    }

    /// Change the fault-injection probabilities.
    pub(crate) fn set_fault_rates(&mut self, checksum_error_rate: f32, drop_ok_rate: f32) {
        self.checksum_error_rate = crate::config::sanitize_rate(checksum_error_rate);
        self.drop_ok_rate = crate::config::sanitize_rate(drop_ok_rate);
    }

    /// Advance simulated time by `dt` seconds, processing commands as they
    /// become possible.
    pub(crate) fn advance(&mut self, dt: f64) {
        self.execute();
        let mut remaining = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        let mut steps = 0;
        while remaining > 1e-12 && steps < MAX_STEPS_PER_ADVANCE {
            let d = remaining.min(MAX_STEP);
            self.step(d);
            remaining -= d;
            steps += 1;
        }
    }

    /// Output lines produced since the last call.
    pub(crate) fn take_output(&mut self) -> Vec<String> {
        std::mem::take(&mut self.out)
    }

    /// Commands accepted since the last call.
    pub(crate) fn take_received(&mut self) -> Vec<String> {
        std::mem::take(&mut self.received)
    }

    /// Current state for [`SimControl::snapshot`](crate::SimControl::snapshot).
    pub(crate) fn snapshot(&self) -> SimSnapshot {
        let pos = self.logical_position();
        let ambient = f64::from(self.cfg.ambient);
        SimSnapshot {
            position: pos.map(|v| v as f32),
            hotend_temps: self.hotends.iter().map(|h| h.temp() as f32).collect(),
            hotend_targets: self.hotends.iter().map(|h| h.target as f32).collect(),
            bed_temp: self.bed.as_ref().map_or(ambient, Heater::temp) as f32,
            bed_target: self.bed.as_ref().map_or(0.0, |h| h.target) as f32,
            fan: self.fans[0],
            feedrate_pct: self.motion.feedrate_pct as f32,
            flow_pct: self
                .motion
                .flow_pct
                .get(self.motion.tool)
                .copied()
                .unwrap_or(100.0) as f32,
            last_line_number: self.last_n.clamp(0, i64::from(u32::MAX)) as u32,
            planner_queue: self.planner.len(),
            halted: self.run == RunState::Halted,
            sd_printing: self.sd_job.as_ref().is_some_and(|j| j.printing),
            commands_received: self.commands_received,
            resends_requested: self.resends,
        }
    }

    // ----- time -----

    fn step(&mut self, dt: f64) {
        self.now += dt;
        self.physics(dt);
        match self.run {
            RunState::Booting { until } => {
                if self.now >= until {
                    self.boot();
                }
            }
            RunState::Running => {
                self.execute();
                self.periodic();
            }
            RunState::Halted => {}
        }
    }

    fn physics(&mut self, dt: f64) {
        let ambient = f64::from(self.cfg.ambient);
        let fan = f64::from(self.fans[0]) / 255.0;
        for (i, h) in self.hotends.iter_mut().enumerate() {
            let gains = self
                .settings
                .hotend_pid
                .get(i)
                .copied()
                .unwrap_or(crate::settings::DEFAULT_HOTEND_PID);
            h.step(dt, ambient, fan, gains, &mut self.noise_rng);
        }
        let bed_gains = self.settings.bed_pid;
        if let Some(b) = &mut self.bed {
            b.step(dt, ambient, 0.0, bed_gains, &mut self.noise_rng);
        }
        if let Some(c) = &mut self.chamber {
            c.step(dt, ambient, 0.0, bed_gains, &mut self.noise_rng);
        }
        let extruded = self.planner.advance(dt);
        self.stats.filament_mm += extruded;
    }

    fn boot(&mut self) {
        self.run = RunState::Running;
        let image = self.eeprom_image();
        let mut lines = vec![
            "start".to_string(),
            "echo:Marlin 2.1.2.1".to_string(),
            "echo: Last Updated: 2023-07-23 | Author: (Sprint, virtual printer)".to_string(),
            "echo: Compiled: Jul 23 2023".to_string(),
            format!(
                "echo: Free Memory: {}  PlannerBufferBytes: {}",
                4013usize.saturating_sub(self.cfg.planner_buffer * 8),
                self.cfg.planner_buffer * 118
            ),
            format!(
                "echo:{EEPROM_VERSION} stored settings retrieved ({} bytes; crc {})",
                image.len(),
                crc16(&image)
            ),
        ];
        lines.extend(self.settings.report(self.report_context(), false));
        lines.push("echo:SD card ok".to_string());
        self.out.extend(lines);
    }

    /// The EEPROM image (settings + mesh) whose size and CRC are reported.
    fn eeprom_image(&self) -> Vec<u8> {
        let mut image = self.eeprom.serialize();
        if let Some(mesh) = &self.eeprom_mesh {
            for v in mesh.z.iter().flatten() {
                image.extend_from_slice(&(*v as f32).to_le_bytes());
            }
        }
        image
    }

    fn report_context(&self) -> ReportContext {
        ReportContext {
            extruders: self.hotends.len(),
            heated_bed: self.bed.is_some(),
            leveling_active: self.leveling,
            bed_size: [
                f64::from(self.cfg.build_volume[0]),
                f64::from(self.cfg.build_volume[1]),
            ],
        }
    }

    fn periodic(&mut self) {
        if let Some(task) = &mut self.task
            && self.now >= task.busy_next
        {
            task.busy_next = self.now + KEEPALIVE;
            let waiting_user =
                matches!(&task.kind, TaskKind::FilamentChange(fc) if fc.waiting_for_user());
            self.out.push(
                if waiting_user {
                    "echo:busy: paused for user"
                } else {
                    "echo:busy: processing"
                }
                .to_string(),
            );
        }
        let now = self.now;
        if self.temp_interval > 0.0 && due(now, &mut self.next_temp, self.temp_interval) {
            let line = self.temp_line(1, self.motion.tool);
            self.out.push(line);
        }
        if self.pos_interval > 0.0 && due(now, &mut self.next_pos, self.pos_interval) {
            let line = self.position_line();
            self.out.push(line);
        }
        if self.sd_interval > 0.0 && due(now, &mut self.next_sd, self.sd_interval) {
            let line = self.sd_status_line();
            self.out.push(line);
        }
    }

    // ----- line protocol -----

    fn on_line(&mut self, raw: String) {
        // Marlin disables the emergency parser while writing to SD (M28 to
        // M29), so such lines are stored in the file like any other.
        if self.saving.is_none()
            && let Some(e) = line::emergency(&raw)
        {
            self.emergency(e);
            if self.run != RunState::Running {
                return;
            }
        }
        if self.pending_bytes + raw.len() > MAX_PENDING_BYTES {
            log::warn!("virtual printer receive buffer overflow; line dropped");
            return;
        }
        self.pending_bytes += raw.len();
        self.pending.push_back(raw);
    }

    /// EMERGENCY_PARSER: act on receipt, before the command queue.
    fn emergency(&mut self, e: Emergency) {
        match e {
            Emergency::M112 => {
                self.record("M112".to_string());
                self.kill();
            }
            Emergency::M108 => {
                self.wait_for_heatup = false;
                self.wait_for_user = false;
            }
            Emergency::M410 => self.quickstop(),
            Emergency::M876(response) => self.prompt_response(response),
        }
    }

    fn record(&mut self, cmd: String) {
        self.commands_received += 1;
        self.received.push(cmd);
    }

    fn kill(&mut self) {
        self.out
            .push("Error:Printer halted. kill() called!".to_string());
        self.run = RunState::Halted;
        self.pending.clear();
        self.pending_bytes = 0;
        self.partial.clear();
        self.queue.clear();
        self.injected.clear();
        self.task = None;
        self.planner.quick_stop();
        self.disable_all_heaters();
        self.fans = [0; FAN_COUNT];
        if self.timer.is_active() {
            self.timer.abort(self.now, &mut self.stats);
        }
        self.sd_job = None;
        self.saving = None;
        self.temp_interval = 0.0;
        self.pos_interval = 0.0;
        self.sd_interval = 0.0;
        self.prompt = host::Prompt::None;
    }

    /// Move complete lines into the command buffer while there is room.
    fn fill_queue(&mut self) {
        let capacity = self.cfg.rx_buffer;
        let mut sd_budget = 10_000usize;
        loop {
            let used = self.queue.len() + usize::from(self.task.is_some());
            if used >= capacity || self.run != RunState::Running {
                return;
            }
            if let Some(raw) = self.pending.pop_front() {
                self.pending_bytes = self.pending_bytes.saturating_sub(raw.len());
                self.accept_line(&raw);
                continue;
            }
            if sd_budget > 0 && self.read_sd_line(&mut sd_budget) {
                continue;
            }
            return;
        }
    }

    fn line_error(&mut self, msg: &str) {
        self.out.push(format!("Error:{msg}{}", self.last_n));
        self.request_resend();
    }

    /// "Resend: N" + "ok", flushing the receive buffer like Marlin.
    fn request_resend(&mut self) {
        self.pending.clear();
        self.pending_bytes = 0;
        self.out
            .push(format!("Resend: {}", self.last_n.saturating_add(1)));
        self.out.push("ok".to_string());
        self.resends += 1;
    }

    /// Validate framing and queue the command.
    fn accept_line(&mut self, raw: &str) {
        let Some(framed) = line::frame(raw) else {
            return;
        };
        let parsed = Command::parse(framed.command);
        let is_m29 = parsed.is(b'M', 29);
        let line_number = match framed.number {
            Some(mut n) => {
                let is_m110 = parsed.is(b'M', 110);
                if is_m110 && let Some(v) = parsed.int('N') {
                    n = v;
                }
                if !is_m110 && n != self.last_n.saturating_add(1) {
                    self.line_error("Line Number is not Last Line Number+1, Last Line: ");
                    return;
                }
                match framed.checksum {
                    Some(cs) if cs != i64::from(framed.computed) => {
                        self.line_error("checksum mismatch, Last Line: ");
                        return;
                    }
                    Some(_) => {}
                    None => {
                        self.line_error("No Checksum with line number, Last Line: ");
                        return;
                    }
                }
                if self.fault_rng.chance(self.checksum_error_rate) {
                    log::debug!("injecting checksum error for line {n}");
                    self.line_error("checksum mismatch, Last Line: ");
                    return;
                }
                self.last_n = n;
                Some(n)
            }
            None => {
                if framed.checksum.is_some() {
                    self.out.push(format!(
                        "Error:No Line Number with checksum, Last Line: {}",
                        self.last_n
                    ));
                    self.out.push("ok".to_string());
                    return;
                }
                if self.saving.is_some() && !is_m29 {
                    // Marlin requires line numbers while writing to SD.
                    self.line_error("No Checksum with line number, Last Line: ");
                    return;
                }
                if parsed.is(b'M', 110)
                    && let Some(v) = parsed.int('N')
                {
                    self.last_n = v;
                }
                None
            }
        };
        let text = framed.command.to_string();
        if !text.is_empty() {
            self.record(text.clone());
        }
        self.queue.push_back(Queued {
            text,
            origin: Origin::Serial { line: line_number },
        });
    }

    fn send_ok(&mut self, origin: Origin) {
        let Origin::Serial { line } = origin else {
            return;
        };
        if self.fault_rng.chance(self.drop_ok_rate) {
            log::debug!("dropping an ok");
            return;
        }
        if self.cfg.advanced_ok {
            let mut s = String::from("ok");
            if let Some(n) = line {
                s.push_str(&format!(" N{n}"));
            }
            let buffer_free = self.cfg.rx_buffer.saturating_sub(self.queue.len() + 1);
            s.push_str(&format!(" P{} B{}", self.planner.free(), buffer_free));
            self.out.push(s);
        } else {
            self.out.push("ok".to_string());
        }
    }

    // ----- executor -----

    fn execute(&mut self) {
        let mut guard = 0usize;
        while self.run == RunState::Running && guard < 20_000 {
            guard += 1;
            self.fill_queue();
            if let Some(task) = self.task.take() {
                match self.progress(task) {
                    Some(task) => {
                        self.task = Some(task);
                        return;
                    }
                    None => continue,
                }
            }
            let next = if let Some(text) = self.injected.pop_front() {
                Queued {
                    text,
                    origin: Origin::Injected,
                }
            } else if let Some(q) = self.queue.pop_front() {
                q
            } else {
                return;
            };
            self.start_command(next);
        }
    }

    fn start_command(&mut self, q: Queued) {
        if let (Some(file), Origin::Serial { .. }) = (self.saving, q.origin) {
            let cmd = Command::parse(&q.text);
            if cmd.is(b'M', 29) {
                self.saving = None;
                self.out.push("Done saving file.".to_string());
            } else if !q.text.is_empty() && !self.sd.append_line(file, &q.text) {
                self.out.push("Error:error writing to file".to_string());
            }
            self.send_ok(q.origin);
            return;
        }
        let cmd = Command::parse(&q.text);
        match self.dispatch(&cmd, q.origin) {
            Outcome::Done => self.send_ok(q.origin),
            Outcome::NoOk => {}
            Outcome::Task { kind, sync } => {
                let mut task = Task {
                    origin: q.origin,
                    kind,
                    sync,
                    busy_next: self.now + KEEPALIVE,
                };
                if !sync {
                    self.begin_task(&mut task.kind);
                }
                self.task = Some(task);
            }
        }
    }

    /// Initialise a task once it may start (after synchronisation).
    fn begin_task(&mut self, kind: &mut TaskKind) {
        match kind {
            TaskKind::Dwell { secs, until } => *until = self.now + *secs,
            TaskKind::Home(h) => self.begin_homing(h),
            TaskKind::Probe(p) => self.begin_probing(p),
            TaskKind::FilamentChange(fc) => self.begin_filament_change(fc),
            TaskKind::SdFinish => self.sd_finish(),
            TaskKind::DisableSteppers => self.motion.steppers = false,
            TaskKind::Moves(_) | TaskKind::Sync | TaskKind::HeatWait(_) | TaskKind::Autotune(_) => {
            }
        }
    }

    /// Advance a running task; `None` when it finished (ok sent).
    fn progress(&mut self, mut task: Task) -> Option<Task> {
        if task.sync {
            if !self.planner.is_empty() {
                return Some(task);
            }
            task.sync = false;
            self.begin_task(&mut task.kind);
        }
        let done = match &mut task.kind {
            TaskKind::Moves(segs) => {
                while let Some(seg) = segs.front().copied() {
                    if !self.planner.has_room() {
                        break;
                    }
                    let limits = self.settings.limits();
                    self.planner.push(seg.target, seg.feed, seg.kind, &limits);
                    segs.pop_front();
                }
                segs.is_empty()
            }
            TaskKind::Sync | TaskKind::SdFinish | TaskKind::DisableSteppers => true,
            TaskKind::Dwell { until, .. } => self.now >= *until,
            TaskKind::Home(h) => self.progress_homing(h),
            TaskKind::Probe(p) => self.progress_probing(p),
            TaskKind::HeatWait(w) => self.progress_heat_wait(w),
            TaskKind::Autotune(t) => self.progress_autotune(t),
            TaskKind::FilamentChange(fc) => self.progress_filament_change(fc),
        };
        if done {
            self.send_ok(task.origin);
            None
        } else {
            Some(task)
        }
    }

    /// Quickstop (M410): flush the planner and stop where the tool is.
    fn quickstop(&mut self) {
        let physical = self.planner.quick_stop();
        if let Some(task) = &mut self.task
            && let TaskKind::Moves(segs) = &mut task.kind
        {
            segs.clear();
        }
        self.sync_native_from_physical(physical);
    }

    fn emit(&mut self, line: impl Into<String>) {
        self.out.push(line.into());
    }
}

/// Fixed-cadence timer check: true when `next` has passed; advances it by
/// whole intervals (restarting if it fell behind).
pub(crate) fn due(now: f64, next: &mut f64, interval: f64) -> bool {
    if now + 1e-9 < *next {
        return false;
    }
    *next += interval;
    if *next <= now {
        *next = now + interval;
    }
    true
}
