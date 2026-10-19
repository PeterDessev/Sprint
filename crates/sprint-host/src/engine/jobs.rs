//! Jobs: host prints (stream, pause/resume/cancel scripts, firmware
//! pauses, progress), SD prints and SD uploads.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sprint_gcode::{GcodeFile, MachineState, ParsedLine, clean_line, state_at_line};

use super::meta::{CmdMeta, JobLine, Meta, Source};
use super::objects::{Objects, is_object_motion, resync};
use super::scripts::{
    ResumeExtras, guarded, hotend_hot, in_script_modes, modes_after, render_job_script,
    resume_script, without_extruder_moves,
};
use super::{Engine, PROGRESS_INTERVAL, Phase};
use crate::console::ConsoleKind;
use crate::error::HostError;
use crate::event::Event;
use crate::hooks::Hook;
use crate::job::{JobInfo, JobKind, JobStatus, PrintOptions, ProgressModel, is_user_wait};
use crate::script::format_number;

/// A pause repeats its M108 this often while the firmware still waits.
const WAIT_BREAK_REPEAT: Duration = Duration::from_secs(1);
/// A "busy: paused for user" that arrives this soon after the host sent
/// M108 may have left the printer before it got the M108: it is not taken
/// for a new wait (Marlin reports busy every 2 s by default).
const BUSY_LAG: Duration = Duration::from_secs(2);

/// A running job.
pub(super) struct JobRuntime {
    pub(super) info: JobInfo,
    file: Option<Arc<GcodeFile>>,
    options: PrintOptions,
    /// Next file line to look at.
    next_line: usize,
    preamble: VecDeque<String>,
    /// Job commands sent before anything else (scripts, SD control).
    scripts: VecDeque<Meta>,
    /// Status to enter once every script line is acknowledged.
    after_scripts: Option<JobStatus>,
    /// Machine state over the streamed lines (as sent).
    machine: MachineState,
    /// State saved when the pause script ran (resume restores it).
    pause_snapshot: Option<MachineState>,
    pause_script_queued: bool,
    progress: ProgressModel,
    last_acked: Option<usize>,
    acked_any: bool,
    /// The last acknowledgement of a job command (the job start before
    /// the first): a wait's time is bounded by the time since then.
    last_ack_at: Instant,
    active_since: Option<Instant>,
    accumulated: Duration,
    /// Time spent in commands estimates count as zero (heat waits,
    /// homing, probing, user waits; see `CmdMeta::unestimated`).
    waited: Duration,
    pending_m73: Option<String>,
    last_m73: Option<u32>,
    dirty: bool,
    last_publish: Option<Instant>,
    /// SD upload: M28 confirmed ("Writing to file").
    upload_open: bool,
    m29_queued: bool,
    /// SD print: M524 was not understood and M25 + M26 S0 were sent.
    abort_fallback: bool,
    /// The pause script did not retract (the hotend was not at
    /// temperature), so the resume script does not prime either.
    retract_skipped: bool,
    /// When a pause last sent M108 to end a wait of the stream.
    wait_broken_at: Option<Instant>,
    /// A host command the stream reached (`@pause`, a pause point); the
    /// stream holds until the engine acts on it.
    held: Option<(HostCommand, usize)>,
    /// Object labels and the excluded objects.
    objects: Objects,
    /// Moves of an excluded object were left out since the printer last
    /// matched the file's position.
    skipped_motion: bool,
    /// Lines that bring the printer back to the file's position.
    resync: VecDeque<String>,
}

/// A command for the host, not the printer, met in the job stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HostCommand {
    /// `@pause` / `;@pause`, or a pause point: pause with the pause script.
    Pause,
    /// `@cancel` / `@abort`: cancel with the cancel script.
    Cancel,
    /// Another `@` command: not sent, reported.
    Unknown(String),
}

impl HostCommand {
    /// The host command of a raw file line: OctoPrint's `@pause`,
    /// `@cancel` and `@abort`, Printrun's `;@pause`. `;@` lines are
    /// comments, so only the ones Sprint knows count; every `@` line is a
    /// host command (the firmware would answer "Unknown command").
    pub(super) fn parse(raw: &str) -> Option<HostCommand> {
        let t = raw.trim_start();
        let (comment, rest) = match t.strip_prefix(';') {
            Some(r) => (true, r.trim_start().strip_prefix('@')?),
            None => (false, t.strip_prefix('@')?),
        };
        let word = rest
            .split(|c: char| c.is_whitespace() || c == ';')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match word.as_str() {
            "pause" => Some(HostCommand::Pause),
            "cancel" | "abort" => Some(HostCommand::Cancel),
            _ if comment => None,
            _ => Some(HostCommand::Unknown(rest.trim().to_string())),
        }
    }
}

impl JobRuntime {
    fn new(
        info: JobInfo,
        file: Option<Arc<GcodeFile>>,
        options: PrintOptions,
        now: Instant,
    ) -> JobRuntime {
        let start_line = options.start_line;
        let bytes_start = info.bytes_done;
        let progress = ProgressModel::new(
            options.toolpath.clone(),
            start_line,
            bytes_start,
            info.bytes_total,
        );
        JobRuntime {
            info,
            file,
            next_line: start_line,
            preamble: VecDeque::new(),
            scripts: VecDeque::new(),
            after_scripts: None,
            machine: MachineState::new(),
            pause_snapshot: None,
            pause_script_queued: false,
            progress,
            last_acked: None,
            acked_any: false,
            last_ack_at: now,
            active_since: Some(now),
            accumulated: Duration::ZERO,
            waited: Duration::ZERO,
            pending_m73: None,
            last_m73: None,
            dirty: true,
            last_publish: None,
            upload_open: false,
            m29_queued: false,
            abort_fallback: false,
            retract_skipped: false,
            wait_broken_at: None,
            held: None,
            objects: Objects::default(),
            skipped_motion: false,
            resync: VecDeque::new(),
            options,
        }
    }

    pub(super) fn kind(&self) -> JobKind {
        self.info.kind
    }

    pub(super) fn is_active(&self) -> bool {
        self.info.status.is_active()
    }

    /// SD status should be polled.
    pub(super) fn is_polling_sd(&self) -> bool {
        matches!(
            self.info.status,
            JobStatus::Printing | JobStatus::Pausing | JobStatus::Resuming
        )
    }

    pub(super) fn scripts_queued(&self) -> usize {
        self.scripts.len() + self.preamble.len()
    }

    pub(super) fn next_script(&mut self) -> Option<Meta> {
        self.scripts.pop_front()
    }

    /// Script lines the link dropped unacknowledged (a lost line counter)
    /// go again, before the ones not sent yet.
    pub(super) fn resend_scripts(&mut self, lines: impl DoubleEndedIterator<Item = Meta>) {
        for line in lines.rev() {
            self.scripts.push_front(line);
        }
    }

    fn queue_scripts(&mut self, lines: Vec<String>, then: Option<JobStatus>) {
        self.scripts.extend(
            lines
                .into_iter()
                .map(|l| CmdMeta::new(l, Source::Job(JobLine::Script))),
        );
        self.after_scripts = then;
    }

    /// Whether stream lines may be sent now.
    fn streaming(&self) -> bool {
        if !self.scripts.is_empty() || self.held.is_some() {
            return false;
        }
        match self.info.kind {
            JobKind::SdPrint => false,
            JobKind::SdUpload => self.upload_open && self.info.status == JobStatus::Printing,
            JobKind::HostPrint => matches!(
                self.info.status,
                JobStatus::Starting | JobStatus::Printing | JobStatus::Resuming
            ),
        }
    }

    /// Nothing more to stream.
    fn stream_exhausted(&self) -> bool {
        self.held.is_none()
            && self.preamble.is_empty()
            && self.pending_m73.is_none()
            && self.file.as_ref().is_none_or(|f| self.next_line >= f.len())
    }

    /// The next preamble, progress or file command.
    pub(super) fn next_stream_command(&mut self) -> Option<Meta> {
        if !self.streaming() {
            return None;
        }
        if let Some(line) = self.resync.pop_front() {
            return Some(CmdMeta::new(line, Source::Job(JobLine::Resync)));
        }
        if let Some(m73) = self.pending_m73.take() {
            return Some(CmdMeta::new(m73, Source::Job(JobLine::Progress)));
        }
        if let Some(line) = self.preamble.pop_front() {
            return Some(CmdMeta::new(line, Source::Job(JobLine::Preamble)));
        }
        let file = Arc::clone(self.file.as_ref()?);
        while self.next_line < file.len() {
            let index = self.next_line;
            if self.info.kind == JobKind::HostPrint
                && let Some(at) = self.info.pause_at.iter().position(|&l| l == index)
            {
                // A pause point: pause before this line, then stream it.
                self.info.pause_at.remove(at);
                self.held = Some((HostCommand::Pause, index));
                return None;
            }
            self.next_line += 1;
            if let Some(host) = HostCommand::parse(file.line(index)) {
                // Never sent: the firmware does not know them (uploads
                // leave them out too).
                if self.info.kind == JobKind::HostPrint {
                    self.held = Some((host, index));
                    return None;
                }
                continue;
            }
            let host_print = self.info.kind == JobKind::HostPrint;
            if host_print && self.objects.track(file.line(index)) {
                self.info.current_object.clone_from(&self.objects.current);
                self.dirty = true;
            }
            let Some(cmd) = file.command(index) else {
                continue;
            };
            // (Only parsed while an excluded object is involved: this is
            // the hot path of every print.)
            if host_print
                && (self.skipped_motion || self.objects.skipping())
                && is_object_motion(&cmd)
            {
                if self.objects.skipping() {
                    // Not sent, but the file's position moves on.
                    self.machine.apply(&sprint_gcode::parse_line(&cmd));
                    self.skipped_motion = true;
                    continue;
                }
                if std::mem::take(&mut self.skipped_motion) {
                    // Back to where the file is, then this line.
                    self.resync.extend(resync(&self.machine));
                    self.next_line = index;
                    let line = self.resync.pop_front()?;
                    return Some(CmdMeta::new(line, Source::Job(JobLine::Resync)));
                }
            }
            if self.info.kind == JobKind::SdUpload {
                // The file's own M28/M29 would end the upload early.
                let parsed = sprint_gcode::parse_line(&cmd);
                if parsed.is('M', 28) || parsed.is('M', 29) {
                    continue;
                }
            }
            return Some(CmdMeta::new(
                cmd.into_owned(),
                Source::Job(JobLine::Stream(index)),
            ));
        }
        None
    }

    /// Track the machine state over streamed lines as they are sent.
    pub(super) fn on_sent(&mut self, meta: &CmdMeta, parsed: &ParsedLine<'_>, _now: Instant) {
        if matches!(meta.source, Source::Job(JobLine::Stream(_))) {
            self.machine.apply(parsed);
        }
    }

    fn elapsed(&self, now: Instant) -> Duration {
        self.accumulated
            + self
                .active_since
                .map_or(Duration::ZERO, |t| now.saturating_duration_since(t))
    }

    /// Recompute progress fields into `info`.
    fn refresh(&mut self, now: Instant) {
        let elapsed = self.elapsed(now);
        let p = self.progress.update(
            self.last_acked,
            self.info.bytes_done,
            elapsed.as_secs_f64(),
            self.waited.as_secs_f64(),
        );
        self.info.elapsed = elapsed;
        self.info.progress = p.progress;
        self.info.eta = p.eta;
        self.info.estimated_total = p.estimated_total;
        self.info.layer = p.layer;
        self.info.filament_used_mm = p.filament_used_mm;
        if self.options.emit_progress
            && self.info.kind == JobKind::HostPrint
            && self.info.status == JobStatus::Printing
        {
            let pct = (p.progress.clamp(0.0, 1.0) * 100.0).floor() as u32;
            if self.last_m73 != Some(pct) {
                self.last_m73 = Some(pct);
                let minutes = p.eta.map_or(0, |e| e.as_secs().div_ceil(60));
                self.pending_m73 = Some(format!("M73 P{pct} R{minutes}"));
            }
        }
    }
}

impl Engine {
    fn check_can_start(&self) -> Result<(), HostError> {
        if !matches!(self.phase, Phase::Ready) {
            return Err(HostError::NotConnected);
        }
        if self.halted {
            return Err(HostError::Halted);
        }
        if let Some(job) = &self.job
            && job.is_active()
        {
            return Err(HostError::Busy(format!(
                "a job is already running ({})",
                job.info.name
            )));
        }
        Ok(())
    }

    fn begin_job(&mut self, job: JobRuntime) {
        let info = job.info.clone();
        self.job = Some(job);
        self.info(format!(
            "{} {}",
            match info.kind {
                JobKind::HostPrint => "Printing",
                JobKind::SdPrint => "Printing from SD:",
                JobKind::SdUpload => "Uploading to SD:",
            },
            info.name
        ));
        let status = info.status.clone();
        self.job_hook(&info, &status);
        self.shared.update(|s| s.job = Some(info));
        self.shared.emit(Event::Job(status));
    }

    /// Run the profile's hook for a print entering `status` (start,
    /// finish, fail, cancel), if it has one.
    fn job_hook(&self, info: &JobInfo, status: &JobStatus) {
        if info.kind == JobKind::SdUpload {
            return;
        }
        let profile = self.shared.profile.read();
        let (event, command, word) = match status {
            JobStatus::Starting => ("start", &profile.on_start_command, "started"),
            JobStatus::Completed => ("finish", &profile.on_finish_command, "completed"),
            JobStatus::Failed(_) => ("fail", &profile.on_fail_command, "failed"),
            JobStatus::Cancelled => ("cancel", &profile.on_cancel_command, "cancelled"),
            _ => return,
        };
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        let file = info
            .path
            .as_ref()
            .map_or_else(|| info.name.clone(), |p| p.display().to_string());
        let mut env = vec![
            ("SPRINT_FILE", file),
            ("SPRINT_STATUS", word.to_string()),
            ("SPRINT_ELAPSED_SECS", info.elapsed.as_secs().to_string()),
            ("SPRINT_PRINTER", profile.name.clone()),
        ];
        // Only set when there is one: Windows drops empty variables.
        if let JobStatus::Failed(reason) = status {
            env.push(("SPRINT_REASON", reason.clone()));
        }
        let hook = Hook {
            event,
            command: command.to_string(),
            env,
        };
        drop(profile);
        hook.spawn(Arc::clone(&self.shared));
    }

    pub(super) fn start_print(
        &mut self,
        file: Arc<GcodeFile>,
        mut options: PrintOptions,
        now: Instant,
    ) -> Result<(), HostError> {
        self.check_can_start()?;
        options.emit_progress |= self.shared.profile.read().emit_progress;
        let start = options.start_line;
        if start > file.len() {
            return Err(HostError::InvalidArgument(format!(
                "start line {start} is past the end of the file ({} lines)",
                file.len()
            )));
        }
        let bytes_total = file.byte_len() as u64;
        let mut info = JobInfo::new(JobKind::HostPrint, file.name(), file.len(), bytes_total);
        info.path = file.path().map(Path::to_path_buf);
        info.current_line = start;
        info.bytes_done = if start >= file.len() {
            bytes_total
        } else {
            file.byte_offset(start) as u64
        };
        let preamble = options
            .preamble
            .iter()
            .flat_map(|l| l.split(['\n', '\r']))
            .filter_map(|l| clean_line(l).map(|c| c.into_owned()))
            .collect();
        let machine = if start > 0 {
            state_at_line(&file, start)
        } else {
            MachineState::new()
        };
        let mut objects = Objects::scan(&file);
        for i in 0..start.min(file.len()) {
            objects.track(file.line(i));
        }
        info.objects.clone_from(&objects.names);
        info.current_object.clone_from(&objects.current);
        let mut job = JobRuntime::new(info, Some(file), options, now);
        job.preamble = preamble;
        job.machine = machine;
        job.objects = objects;
        job.refresh(now);
        self.begin_job(job);
        Ok(())
    }

    pub(super) fn start_sd_print(&mut self, name: &str, now: Instant) -> Result<(), HostError> {
        self.check_can_start()?;
        let name = valid_sd_name(name)?;
        let info = JobInfo::new(JobKind::SdPrint, name.clone(), 0, 0);
        let mut job = JobRuntime::new(info, None, PrintOptions::default(), now);
        job.queue_scripts(
            vec![format!("M23 {name}"), "M24".to_string()],
            Some(JobStatus::Printing),
        );
        self.begin_job(job);
        self.manual_before_scripts();
        Ok(())
    }

    pub(super) fn start_upload(
        &mut self,
        file: Arc<GcodeFile>,
        name: &str,
        now: Instant,
    ) -> Result<(), HostError> {
        self.check_can_start()?;
        let name = valid_sd_name(name)?;
        let mut info = JobInfo::new(
            JobKind::SdUpload,
            name.clone(),
            file.len(),
            file.byte_len() as u64,
        );
        info.path = file.path().map(Path::to_path_buf);
        let mut job = JobRuntime::new(info, Some(file), PrintOptions::default(), now);
        job.queue_scripts(vec![format!("M28 {name}")], None);
        self.begin_job(job);
        self.manual_before_scripts();
        Ok(())
    }

    // ----- user controls -----

    pub(super) fn pause_job(&mut self, now: Instant) {
        let Some(job) = &mut self.job else {
            return;
        };
        // Paused by the firmware at an M0/M1 of the stream: a pause from
        // here ends the wait and parks with the pause script, as from
        // Printing (M600 keeps its own dialogue).
        let at_stream_user_wait = job.info.kind == JobKind::HostPrint
            && job.info.status == JobStatus::Paused
            && job.info.paused_by_firmware
            && self.flow.in_flight().any(|e| {
                matches!(e.meta.job_line(), Some(JobLine::Stream(_)))
                    && (e.meta.is('M', 0) || e.meta.is('M', 1))
            });
        match (job.info.kind, &job.info.status) {
            (
                JobKind::HostPrint,
                JobStatus::Starting | JobStatus::Printing | JobStatus::Resuming,
            ) => self.pause_host_print(now),
            (JobKind::HostPrint, JobStatus::Paused) if at_stream_user_wait => {
                self.pause_host_print(now);
            }
            (JobKind::SdPrint, JobStatus::Printing | JobStatus::Resuming) => {
                job.queue_scripts(vec!["M25".to_string()], Some(JobStatus::Paused));
                self.manual_before_scripts();
                self.set_job_status(JobStatus::Pausing, now);
            }
            (JobKind::SdUpload, _) => self.warn("An SD upload cannot be paused"),
            _ => {}
        }
    }

    /// Pause a host print: drain the stream (ending a wait of it), then
    /// the pause script.
    fn pause_host_print(&mut self, now: Instant) {
        if let Some(job) = &mut self.job {
            job.pause_script_queued = false;
            job.wait_broken_at = None;
        }
        self.set_job_status(JobStatus::Pausing, now);
        if let Some(what) = self.break_stream_wait(now) {
            self.info(format!("Pausing: ending {what} (M108)"));
        }
        self.advance_job(now);
    }

    /// Send M108 to end the firmware's wait (for the user or a heat-up).
    fn release_wait(&mut self, now: Instant) {
        self.send_immediate("M108", Source::User);
        self.wait_released(now);
    }

    /// An M108 went out: the wait it ends is over, and a "busy: paused for
    /// user" that left the printer before it got the M108 is not a new
    /// one.
    pub(super) fn wait_released(&mut self, now: Instant) {
        self.wait_released_at = Some(now);
        if self.user_wait {
            self.user_wait = false;
            let waiting = self.shared.state.read().waiting_for_user();
            if waiting {
                self.shared.update(|s| s.busy = None);
            }
        }
    }

    /// A firmware busy message.
    pub(super) fn on_busy(&mut self, message: String, now: Instant) {
        if !is_user_wait(&message) {
            self.user_wait = false;
            self.shared.update(|s| s.busy = Some(message));
            return;
        }
        let stale = self
            .wait_released_at
            .is_some_and(|t| now.saturating_duration_since(t) < BUSY_LAG);
        if stale {
            return;
        }
        self.shared.update(|s| s.busy = Some(message));
        self.user_wait = true;
        // A host print the firmware holds at a wait for the user (M0/M1,
        // M600, the rest of a filament change) is paused by the firmware.
        // (Not while a resume script runs: its end sets Printing, and the
        // next report pauses the job then.)
        let held = self.job.as_ref().is_some_and(|j| {
            j.kind() == JobKind::HostPrint
                && matches!(j.info.status, JobStatus::Starting | JobStatus::Printing)
        });
        if held {
            if let Some(job) = &mut self.job {
                job.info.paused_by_firmware = true;
            }
            self.info("The printer waits for the user: the job is paused until it continues");
            self.set_job_status(JobStatus::Paused, now);
        }
    }

    /// An M0/M1/M600 was acknowledged: the firmware no longer waits for
    /// the user. A host print it held continues.
    pub(super) fn user_wait_ended(&mut self, now: Instant) {
        let held = self.job.as_ref().is_some_and(|j| {
            j.kind() == JobKind::HostPrint
                && j.info.status == JobStatus::Paused
                && j.info.paused_by_firmware
        });
        if held {
            self.info("The printer continued the job");
            self.set_job_status(JobStatus::Printing, now);
        }
    }

    /// While a host print is pausing, end a heat-up or a wait for the user
    /// (M0/M1) of the stream with M108, so the pause does not wait for
    /// minutes (M600 keeps its own dialogue). Called again while the
    /// firmware reports that it still waits (at most once a second): an
    /// M108 that arrives before the wait begins is lost. Only for firmware
    /// with EMERGENCY_PARSER: any other reads nothing while it waits, so an
    /// M108 cannot end the wait and repeating it overflows the receive
    /// buffer. Returns what was ended.
    pub(super) fn break_stream_wait(&mut self, now: Instant) -> Option<&'static str> {
        let wait = self.flow.in_flight().find_map(|e| {
            if !matches!(e.meta.job_line(), Some(JobLine::Stream(_))) {
                None
            } else if e.meta.is_heat_wait() {
                Some("the heat-up")
            } else if e.meta.is('M', 0) || e.meta.is('M', 1) {
                Some("the wait for the user")
            } else {
                None
            }
        })?;
        if !self.shared.state.read().has_capability("EMERGENCY_PARSER") {
            return None;
        }
        let job = self.job.as_mut()?;
        let pausing = job.info.kind == JobKind::HostPrint
            && job.info.status == JobStatus::Pausing
            && !job.pause_script_queued;
        let due = job
            .wait_broken_at
            .is_none_or(|t| now.saturating_duration_since(t) >= WAIT_BREAK_REPEAT);
        if !pausing || !due {
            return None;
        }
        job.wait_broken_at = Some(now);
        self.release_wait(now);
        Some(wait)
    }

    pub(super) fn resume_job(&mut self, now: Instant) {
        let Some(job) = &mut self.job else {
            return;
        };
        match (job.info.kind, &job.info.status) {
            (JobKind::HostPrint, JobStatus::Paused) => {
                if job.info.paused_by_firmware || job.pause_snapshot.is_none() {
                    // The firmware paused (and parks) itself: no resume
                    // script, let it continue. One M108 releases a wait
                    // for the user (never one the firmware is not in, nor
                    // a second for the same wait).
                    if self.user_wait {
                        self.info(
                            "Resuming: the printer waits for the user; M108 lets it continue",
                        );
                        self.release_wait(now);
                    }
                    self.set_job_status(JobStatus::Printing, now);
                    return;
                }
                let Some(job) = &mut self.job else {
                    return;
                };
                let snapshot = job
                    .pause_snapshot
                    .take()
                    .unwrap_or_else(|| job.machine.clone());
                let template = job
                    .options
                    .resume_script
                    .clone()
                    .unwrap_or_else(|| self.shared.profile.read().resume_script.clone());
                let mut lines = render_job_script(&self.shared, &template, &snapshot);
                if std::mem::take(&mut job.retract_skipped) {
                    // Nothing was retracted at the pause: no prime either.
                    lines = without_extruder_moves(lines);
                }
                // The return to the file's position is a move like the
                // park: from an unknown position (a pause before the
                // file's G28 returns to X0 Y0 Z0) it could hit the frame.
                let lines = guarded(&self.shared, lines, self.homed, "resume");
                let printer = modes_after(&self.printer_modes, &self.manual);
                let extras = {
                    let s = self.shared.state.read();
                    ResumeExtras::needed(&snapshot, &s.temps, s.fan_speeds.first().copied())
                };
                if !extras.reheat.is_empty() {
                    self.shared.log(
                        ConsoleKind::Info,
                        "Heating back to the job's temperatures before resuming",
                        false,
                        None,
                    );
                }
                let lines = resume_script(lines, &snapshot, &printer, extras);
                job.queue_scripts(lines, Some(JobStatus::Printing));
                // Commands typed while paused (a purge, the rest of a
                // filament change) run before the head returns to the part.
                self.manual_before_scripts();
                self.set_job_status(JobStatus::Resuming, now);
                self.advance_job(now);
            }
            (JobKind::HostPrint, JobStatus::Pausing) if !job.pause_script_queued => {
                // Not parked yet: simply keep printing.
                self.set_job_status(JobStatus::Printing, now);
            }
            (JobKind::SdPrint, JobStatus::Paused) => {
                job.queue_scripts(vec!["M24".to_string()], Some(JobStatus::Printing));
                self.manual_before_scripts();
                self.set_job_status(JobStatus::Resuming, now);
            }
            _ => {}
        }
    }

    pub(super) fn cancel_job(&mut self, now: Instant) {
        let waiting = self
            .flow
            .in_flight()
            .any(|e| e.meta.is_wait() && e.meta.job_line().is_some());
        let Some(job) = &mut self.job else {
            return;
        };
        if !job.is_active() || job.info.status == JobStatus::Cancelling {
            return;
        }
        job.preamble.clear();
        job.pending_m73 = None;
        job.scripts.clear();
        job.after_scripts = None;
        if let Some(file) = &job.file {
            job.next_line = file.len();
        }
        match job.info.kind {
            JobKind::HostPrint => {
                let template = job
                    .options
                    .cancel_script
                    .clone()
                    .unwrap_or_else(|| self.shared.profile.read().cancel_script.clone());
                let lines = render_job_script(&self.shared, &template, &job.machine);
                let lines = guarded(&self.shared, lines, self.homed, "cancel");
                let printer = modes_after(&self.printer_modes, &self.manual);
                let lines = in_script_modes(lines, &printer);
                job.queue_scripts(lines, Some(JobStatus::Cancelled));
                if waiting {
                    // Break out of a heat wait or user pause first.
                    self.release_wait(now);
                }
            }
            JobKind::SdPrint => {
                job.queue_scripts(vec!["M524".to_string()], Some(JobStatus::Cancelled));
                // M524 is not an emergency command: break a heat wait first.
                self.release_wait(now);
            }
            JobKind::SdUpload => {
                let name = job.info.name.clone();
                job.queue_scripts(
                    vec!["M29".to_string(), format!("M30 {name}")],
                    Some(JobStatus::Cancelled),
                );
                job.m29_queued = true;
            }
        }
        self.manual_before_scripts();
        self.set_job_status(JobStatus::Cancelling, now);
        self.advance_job(now);
    }

    // ----- firmware-initiated -----

    pub(super) fn firmware_pause(&mut self, now: Instant) {
        let Some(job) = &mut self.job else {
            return;
        };
        let pause = match job.info.kind {
            JobKind::HostPrint => matches!(
                job.info.status,
                JobStatus::Starting | JobStatus::Printing | JobStatus::Resuming
            ),
            JobKind::SdPrint => job.info.status == JobStatus::Printing,
            JobKind::SdUpload => false,
        };
        if pause {
            job.info.paused_by_firmware = true;
            self.info("The printer paused the job");
            self.set_job_status(JobStatus::Paused, now);
        }
    }

    pub(super) fn firmware_resume(&mut self, now: Instant) {
        let Some(job) = &mut self.job else {
            return;
        };
        if job.info.status != JobStatus::Paused {
            return;
        }
        match job.info.kind {
            JobKind::HostPrint => {
                if job.info.paused_by_firmware {
                    self.info("The printer resumed the job");
                    self.set_job_status(JobStatus::Printing, now);
                } else {
                    self.resume_job(now);
                }
            }
            JobKind::SdPrint => {
                self.set_job_status(JobStatus::Printing, now);
            }
            JobKind::SdUpload => {}
        }
    }

    // ----- progress -----

    /// (stream/preamble/progress lines, script lines) still awaiting their
    /// `ok` or retransmission.
    fn job_pending(&self) -> (usize, usize) {
        let mut stream = 0;
        let mut script = 0;
        let metas = self
            .flow
            .in_flight()
            .map(|e| &e.meta)
            .chain(self.flow.backlog());
        for m in metas {
            match m.job_line() {
                Some(JobLine::Script) => script += 1,
                Some(_) => stream += 1,
                None => {}
            }
        }
        (stream, script)
    }

    /// A job command was acknowledged (or released after a timeout).
    pub(super) fn job_ack(
        &mut self,
        meta: &CmdMeta,
        line: JobLine,
        rtt: Option<Duration>,
        now: Instant,
    ) {
        let Some(job) = &mut self.job else {
            return;
        };
        if !job.is_active() {
            return;
        }
        if meta.unestimated() {
            // The first line counts too (start G-code often begins with a
            // heat-up or G28).
            let since_last = now.saturating_duration_since(job.last_ack_at);
            job.waited += rtt.map_or(since_last, |r| r.min(since_last));
        }
        job.last_ack_at = now;
        let first = !job.acked_any;
        job.acked_any = true;
        if let JobLine::Stream(index) = line {
            job.last_acked = Some(index);
            job.info.current_line = index + 1;
            if let Some(file) = &job.file {
                job.info.bytes_done = if index + 1 >= file.len() {
                    file.byte_len() as u64
                } else {
                    file.byte_offset(index + 1) as u64
                };
            }
            job.dirty = true;
        }
        if first && job.info.kind == JobKind::HostPrint && job.info.status == JobStatus::Starting {
            self.set_job_status(JobStatus::Printing, now);
        }
    }

    /// Move the job along: pause drain, script completion, completion.
    pub(super) fn advance_job(&mut self, now: Instant) {
        self.run_host_command(now);
        let (stream_pending, script_pending) = self.job_pending();
        let Some(job) = &mut self.job else {
            return;
        };
        if !job.is_active() {
            return;
        }
        // Pausing a host print: once the stream is drained, park.
        if job.info.kind == JobKind::HostPrint
            && job.info.status == JobStatus::Pausing
            && !job.pause_script_queued
            && stream_pending == 0
        {
            job.pause_script_queued = true;
            let snapshot = job.machine.clone();
            let template = job
                .options
                .pause_script
                .clone()
                .unwrap_or_else(|| self.shared.profile.read().pause_script.clone());
            let mut lines = render_job_script(&self.shared, &template, &snapshot);
            let hot = hotend_hot(&self.shared.state.read().temps, snapshot.tool);
            if !hot {
                // The firmware would refuse a cold retract ("cold extrusion
                // prevented"); the resume leaves out the prime to match.
                lines = without_extruder_moves(lines);
                job.retract_skipped = true;
                self.shared.log(
                    ConsoleKind::Warning,
                    "The hotend is not at temperature: the pause script does not retract",
                    false,
                    None,
                );
            }
            let lines = guarded(&self.shared, lines, self.homed, "pause");
            let printer = modes_after(&self.printer_modes, &self.manual);
            let lines = in_script_modes(lines, &printer);
            job.pause_snapshot = Some(snapshot);
            job.queue_scripts(lines, Some(JobStatus::Paused));
            self.manual_ahead = self.manual.len();
        }
        // Every script line acknowledged: enter the status that follows.
        if job.scripts.is_empty()
            && script_pending == 0
            && let Some(next) = job.after_scripts.take()
        {
            self.set_job_status(next, now);
            return;
        }
        match job.info.kind {
            JobKind::HostPrint => {
                let done = matches!(job.info.status, JobStatus::Printing | JobStatus::Starting)
                    && job.after_scripts.is_none()
                    && job.scripts.is_empty()
                    && job.stream_exhausted()
                    && stream_pending == 0
                    && script_pending == 0;
                if done {
                    self.set_job_status(JobStatus::Completed, now);
                }
            }
            JobKind::SdUpload => {
                let all_sent = job.info.status == JobStatus::Printing
                    && job.upload_open
                    && job.stream_exhausted()
                    && stream_pending == 0
                    && !job.m29_queued;
                if all_sent {
                    job.m29_queued = true;
                    job.queue_scripts(vec!["M29".to_string()], None);
                }
            }
            JobKind::SdPrint => {}
        }
    }

    /// Act on a host command the stream reached: pause or cancel the way
    /// the user would, or report an unknown one and go on.
    fn run_host_command(&mut self, now: Instant) {
        let Some(job) = &mut self.job else {
            return;
        };
        let Some((command, index)) = job.held.take() else {
            return;
        };
        job.dirty = true;
        let line = index + 1;
        match command {
            HostCommand::Pause => {
                self.info(format!(
                    "Pausing at line {line} (requested by the file or a pause point)"
                ));
                self.pause_job(now);
            }
            HostCommand::Cancel => {
                self.info(format!("Cancelling at line {line} (requested by the file)"));
                self.cancel_job(now);
            }
            HostCommand::Unknown(text) => {
                self.warn(format!(
                    "Line {line}: host command \"@{text}\" is not known; not sent"
                ));
            }
        }
    }

    /// Exclude (`exclude`) an object of the running host print, or print it
    /// again: effective for the parts of it not streamed yet.
    pub(super) fn exclude_object(&mut self, name: &str, exclude: bool) {
        let name = name.trim();
        let Some(job) = self
            .job
            .as_mut()
            .filter(|j| j.kind() == JobKind::HostPrint && j.is_active())
        else {
            self.warn(format!(
                "No host print is running: object \"{name}\" left as it is"
            ));
            return;
        };
        if !job.objects.names.iter().any(|n| n == name) {
            self.warn(format!("This print has no object named \"{name}\""));
            return;
        }
        let excluded = job.objects.excluded.iter().any(|n| n == name);
        if excluded == exclude {
            return;
        }
        if exclude {
            job.objects.excluded.push(name.to_string());
        } else {
            job.objects.excluded.retain(|n| n != name);
        }
        job.info.excluded_objects.clone_from(&job.objects.excluded);
        let info = job.info.clone();
        self.shared.update(|s| s.job = Some(info));
        self.info(if exclude {
            format!("Excluded \"{name}\": the rest of it is not printed")
        } else {
            format!("Printing \"{name}\" again from here")
        });
    }

    /// Pause the host print before each of `lines` (0-based file lines)
    /// that has not been streamed yet, replacing the earlier pause points.
    pub(super) fn set_pause_points(&mut self, mut lines: Vec<usize>) {
        let Some(job) = &mut self.job else {
            self.warn("No print is running: pause points ignored");
            return;
        };
        if job.info.kind != JobKind::HostPrint || !job.is_active() {
            self.warn("Pause points work only while the host streams a print");
            return;
        }
        lines.retain(|&l| l >= job.next_line && l < job.info.total_lines);
        lines.sort_unstable();
        lines.dedup();
        job.info.pause_at = lines;
        let info = job.info.clone();
        self.shared.update(|s| s.job = Some(info));
    }

    /// Periodic progress publishing.
    pub(super) fn job_tick(&mut self, now: Instant) {
        let Some(job) = &mut self.job else {
            return;
        };
        if !job.is_active() {
            return;
        }
        let due = job
            .last_publish
            .is_none_or(|t| now.saturating_duration_since(t) >= PROGRESS_INTERVAL);
        let running = job.active_since.is_some();
        if !due || !(job.dirty || running) {
            return;
        }
        job.last_publish = Some(now);
        let line_changed = std::mem::take(&mut job.dirty);
        job.refresh(now);
        let info = job.info.clone();
        self.shared.update(|s| s.job = Some(info.clone()));
        if line_changed {
            self.shared.emit(Event::JobProgress {
                line: info.current_line,
                total: info.total_lines,
            });
        }
    }

    /// Enter `status`: timers, publishing, and the end of the job.
    pub(super) fn set_job_status(&mut self, status: JobStatus, now: Instant) {
        let autoreport_off = self.sd_autoreport;
        let sd_autoreport_cap = self
            .shared
            .state
            .read()
            .has_capability("AUTOREPORT_SD_STATUS");
        let interval = self.options.autoreport_interval_secs.max(1);
        let Some(job) = &mut self.job else {
            return;
        };
        if job.info.status == status {
            return;
        }
        let previous = std::mem::replace(&mut job.info.status, status.clone());
        if status != JobStatus::Paused {
            job.info.paused_by_firmware = false;
        }
        match &status {
            JobStatus::Paused => {
                job.accumulated = job.elapsed(now);
                job.active_since = None;
            }
            s if s.is_finished() => {
                job.accumulated = job.elapsed(now);
                job.active_since = None;
            }
            _ => {
                if job.active_since.is_none() {
                    job.active_since = Some(now);
                }
            }
        }
        if job.info.kind == JobKind::HostPrint
            && matches!(status, JobStatus::Failed(_) | JobStatus::Cancelled)
        {
            // Where a recovery resumes: lines in flight may have run, but
            // only acknowledged ones are known to have.
            job.info.stopped_at = Some(job.last_acked.map_or(job.options.start_line, |l| l + 1));
            job.info.pause_at.clear();
        }
        if status == JobStatus::Completed {
            job.info.pause_at.clear();
            if job.info.kind != JobKind::SdPrint {
                job.info.current_line = job.info.total_lines;
                job.info.bytes_done = job.info.bytes_total;
                job.last_acked = job.info.total_lines.checked_sub(1);
            } else {
                job.info.bytes_done = job.info.bytes_total;
            }
        }
        job.refresh(now);
        if status == JobStatus::Completed {
            job.info.progress = 1.0;
            job.info.eta = Some(Duration::ZERO);
            job.info.estimated_total = Some(job.info.elapsed);
        }
        let info = job.info.clone();
        let kind = job.info.kind;
        if status.is_finished() {
            self.job = None;
            self.job_hook(&info, &status);
            if kind == JobKind::SdPrint {
                // Whatever ended it, the card is not printing it any more
                // (cleared before the status shows, so no UI sees both).
                self.sd_stopped();
            }
        }
        self.shared.update(|s| s.job = Some(info.clone()));
        self.shared.emit(Event::Job(status.clone()));
        if status.is_finished() || previous == JobStatus::Starting {
            self.shared.emit(Event::JobProgress {
                line: info.current_line,
                total: info.total_lines,
            });
        }
        match &status {
            JobStatus::Completed => self.info(format!(
                "{} finished in {}",
                info.name,
                format_duration(info.elapsed)
            )),
            JobStatus::Cancelled => self.info(format!("{} cancelled", info.name)),
            JobStatus::Failed(reason) => {
                let message = format!("{} failed: {reason}", info.name);
                self.shared
                    .log(ConsoleKind::Error, message.clone(), false, None);
                self.shared.emit(Event::Error(message));
            }
            _ => {}
        }
        if kind == JobKind::SdPrint {
            if status == JobStatus::Printing && previous == JobStatus::Starting && sd_autoreport_cap
            {
                self.internal
                    .push_back(CmdMeta::new(format!("M27 S{interval}"), Source::Setup));
            }
            if status.is_finished() && autoreport_off {
                self.internal
                    .push_back(CmdMeta::new("M27 S0", Source::Setup));
            }
        }
    }

    /// Stop the job with an error. For failures where the printer stopped
    /// on its own or cannot be reached (halt, reset, lost link); see
    /// [`Engine::abandon_job`] when it keeps running.
    pub(super) fn fail_job(&mut self, reason: &str) {
        if self.job.as_ref().is_some_and(JobRuntime::is_active) {
            self.set_job_status(JobStatus::Failed(reason.to_string()), Instant::now());
        }
    }

    /// The engine gives up on the job while the printer keeps running (a
    /// line it keeps rejecting, a lost line counter, a quick stop): fail it
    /// and leave the printer safe. A host print gets every heater and the
    /// part fan switched off (unless the profile keeps them on); an SD
    /// upload gets its file closed (M29). These commands go out after the
    /// line counter reset, before anything else, and are written at once
    /// if the connection is closed first.
    pub(super) fn abandon_job(&mut self, reason: &str) {
        let Some(job) = self.job.as_ref().filter(|j| j.is_active()) else {
            return;
        };
        let kind = job.kind();
        let machine = job.machine.clone();
        self.fail_job(reason);
        if self.halted || self.exit.is_some() {
            return;
        }
        let lines = match kind {
            JobKind::HostPrint => {
                let profile = self.shared.profile.read().clone();
                if profile.keep_heaters_on_failure {
                    self.warn("The heaters stay on (the profile keeps them on after a failure)");
                    return;
                }
                let temps = self.shared.state.read().temps.clone();
                let mut lines = profile
                    .script_context(&machine)
                    .covering(&temps)
                    .heaters_off();
                lines.push("M107".to_string());
                self.warn("Switching the heaters and the part fan off after the failed print");
                lines
            }
            JobKind::SdUpload => vec!["M29".to_string()],
            JobKind::SdPrint => Vec::new(),
        };
        for line in lines {
            self.internal
                .push_back(CmdMeta::new(line, Source::SafeStop));
        }
    }

    /// The cancel of a host print or an upload was not acknowledged in time
    /// and the connection is about to close
    /// ([`Printer::disconnect_safely`](crate::Printer::disconnect_safely)):
    /// switch every heater and the part fan off (host print, whatever the
    /// profile keeps on after failures: nobody watches the printer any
    /// more) or close the file (upload). The commands go out with the
    /// other safe-stop commands, written at once when the link closes.
    pub(super) fn safe_stop_before_close(&mut self) {
        let Some(job) = self.job.as_ref().filter(|j| j.is_active()) else {
            return;
        };
        let lines = match job.kind() {
            JobKind::HostPrint => {
                let profile = self.shared.profile.read().clone();
                let temps = self.shared.state.read().temps.clone();
                let mut lines = profile
                    .script_context(&job.machine)
                    .covering(&temps)
                    .heaters_off();
                lines.push("M107".to_string());
                self.warn(format!(
                    "{} did not stop in time: switching the heaters and the part fan off before disconnecting",
                    job.info.name
                ));
                lines
            }
            JobKind::SdUpload => {
                self.warn("The upload did not stop in time: closing the file before disconnecting");
                vec!["M29".to_string()]
            }
            JobKind::SdPrint => return,
        };
        for line in lines {
            self.internal
                .push_back(CmdMeta::new(line, Source::SafeStop));
        }
    }

    // ----- SD replies -----

    /// "open failed" while starting an SD print or upload.
    pub(super) fn job_open_failed(&mut self, message: &str) {
        let starting = self.job.as_ref().is_some_and(|j| {
            matches!(j.info.kind, JobKind::SdPrint | JobKind::SdUpload)
                && j.info.status == JobStatus::Starting
        });
        if starting {
            self.fail_job(message);
        }
    }

    /// "No media" / "SD init fail": an SD print or upload that is starting
    /// cannot run.
    pub(super) fn sd_card_missing(&mut self, message: &str) {
        let starting = self.job.as_ref().is_some_and(|j| {
            matches!(j.info.kind, JobKind::SdPrint | JobKind::SdUpload)
                && j.info.status == JobStatus::Starting
        });
        if starting {
            self.fail_job(message);
        }
    }

    /// "Not SD printing" while an SD print runs: the printer has no file
    /// open, so the print is over (or never started) and "Done printing
    /// file" did not arrive. Complete it if the whole file was reported
    /// read, fail it otherwise, rather than polling forever.
    pub(super) fn sd_not_printing(&mut self, now: Instant) {
        let Some(job) = &self.job else {
            return;
        };
        if job.info.kind != JobKind::SdPrint || job.info.status != JobStatus::Printing {
            return;
        }
        if job.info.bytes_total > 0 && job.info.bytes_done >= job.info.bytes_total {
            self.set_job_status(JobStatus::Completed, now);
        } else {
            let reason = format!("the printer is not printing {}", job.info.name);
            self.fail_job(&reason);
        }
    }

    /// M524 was not understood: stop with M25 + M26 S0 instead.
    pub(super) fn sd_abort_unsupported(&mut self) {
        let Some(job) = &mut self.job else {
            return;
        };
        if job.info.kind == JobKind::SdPrint
            && job.info.status == JobStatus::Cancelling
            && !job.abort_fallback
        {
            job.abort_fallback = true;
            job.queue_scripts(
                vec!["M25".to_string(), "M26 S0".to_string()],
                Some(JobStatus::Cancelled),
            );
        }
    }

    pub(super) fn sd_opened(&mut self, size: u64) {
        let Some(job) = &mut self.job else {
            return;
        };
        if job.info.kind == JobKind::SdPrint && size > 0 && job.info.bytes_total != size {
            job.info.bytes_total = size;
            job.progress = ProgressModel::new(None, 0, 0, size);
            job.dirty = true;
        }
    }

    pub(super) fn sd_progress(&mut self, done: u64, total: u64) {
        let Some(job) = &mut self.job else {
            return;
        };
        if job.info.kind != JobKind::SdPrint || !job.is_active() {
            return;
        }
        if total > 0 && total != job.info.bytes_total {
            job.info.bytes_total = total;
            job.progress = ProgressModel::new(None, 0, 0, total);
        }
        job.info.bytes_done = done.min(job.info.bytes_total.max(done));
        job.dirty = true;
    }

    pub(super) fn sd_done(&mut self, now: Instant) {
        if self
            .job
            .as_ref()
            .is_some_and(|j| j.info.kind == JobKind::SdPrint && j.is_active())
        {
            self.set_job_status(JobStatus::Completed, now);
        }
    }

    /// "Writing to file": the upload may stream.
    pub(super) fn upload_confirmed(&mut self, now: Instant) {
        let Some(job) = &mut self.job else {
            return;
        };
        if job.info.kind == JobKind::SdUpload && job.info.status == JobStatus::Starting {
            job.upload_open = true;
            self.set_job_status(JobStatus::Printing, now);
        }
    }

    /// M28 acknowledged: without "Writing to file" the file is not open and
    /// streaming would execute the G-code instead of storing it.
    pub(super) fn upload_open_acknowledged(&mut self) {
        let failed = self.job.as_ref().is_some_and(|j| {
            j.info.kind == JobKind::SdUpload
                && j.info.status == JobStatus::Starting
                && !j.upload_open
        });
        if failed {
            self.fail_job("the printer could not open the file for writing");
        }
    }

    /// The final M29 was acknowledged: the firmware closed the file. Normally
    /// "Done saving file." came first; if that line was lost the upload is
    /// still done.
    pub(super) fn upload_closed(&mut self, now: Instant) {
        let closing = self.job.as_ref().is_some_and(|j| {
            j.info.kind == JobKind::SdUpload && j.info.status == JobStatus::Printing && j.m29_queued
        });
        if closing {
            self.upload_saved(now);
        }
    }

    /// "Done saving file.".
    pub(super) fn upload_saved(&mut self, now: Instant) {
        let uploading = self.job.as_ref().is_some_and(|j| {
            j.info.kind == JobKind::SdUpload && j.info.status == JobStatus::Printing
        });
        if !uploading {
            return;
        }
        self.set_job_status(JobStatus::Completed, now);
        let extended = self.shared.state.read().has_capability("EXTENDED_M20");
        let list = if extended { "M20 L" } else { "M20" };
        self.internal.push_back(CmdMeta::new(list, Source::User));
    }
}

/// An SD file name must be one line of printable text.
fn valid_sd_name(name: &str) -> Result<String, HostError> {
    let name = name.trim();
    if name.is_empty() || name.chars().any(|c| c.is_control() || c == '*' || c == ';') {
        return Err(HostError::InvalidArgument(format!(
            "invalid SD file name \"{}\"",
            name.escape_debug()
        )));
    }
    Ok(name.to_string())
}

/// "1h 02m 03s" style.
pub(super) fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{}s", format_number(d.as_secs_f64(), 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_commands_in_files() {
        use HostCommand::*;
        for (line, want) in [
            (";@pause", Some(Pause)),
            ("  ; @pause insert magnets", Some(Pause)),
            ("@pause", Some(Pause)),
            ("@PAUSE ; comment", Some(Pause)),
            ("@cancel", Some(Cancel)),
            ("@abort", Some(Cancel)),
            ("@resume", Some(Unknown("resume".into()))),
            ("@my_macro x=1", Some(Unknown("my_macro x=1".into()))),
            (";@something", None),
            ("; pause here", None),
            ("G1 X10 ; @pause", None),
            ("M117 @pause", None),
            ("", None),
        ] {
            assert_eq!(HostCommand::parse(line), want, "{line:?}");
        }
    }

    #[test]
    fn sd_names() {
        assert_eq!(valid_sd_name(" cube.gco ").unwrap(), "cube.gco");
        for bad in ["", "  ", "a\nb", "a*b", "x;y", "\u{7}"] {
            assert!(valid_sd_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(Duration::from_secs(3723)), "1h 02m 03s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m 05s");
        assert_eq!(format_duration(Duration::from_millis(2500)), "2.5s");
    }
}
