//! Jobs: host prints (stream, pause/resume/cancel scripts, firmware
//! pauses, progress), SD prints and SD uploads.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sprint_gcode::{GcodeFile, MachineState, ParsedLine, clean_line, state_at_line};

use super::meta::{CmdMeta, JobLine, Meta, Source};
use super::scripts::{guarded, in_millimetres, render_job_script};
use super::{Engine, PROGRESS_INTERVAL, Phase};
use crate::console::ConsoleKind;
use crate::error::HostError;
use crate::event::Event;
use crate::job::{JobInfo, JobKind, JobStatus, PrintOptions, ProgressModel};
use crate::script::format_number;

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
    paused_by_firmware: bool,
    pause_script_queued: bool,
    progress: ProgressModel,
    last_acked: Option<usize>,
    acked_any: bool,
    last_ack_at: Option<Instant>,
    active_since: Option<Instant>,
    accumulated: Duration,
    /// Time spent in heat waits (estimates count them as zero).
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
            paused_by_firmware: false,
            pause_script_queued: false,
            progress,
            last_acked: None,
            acked_any: false,
            last_ack_at: None,
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
        if !self.scripts.is_empty() {
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
        self.preamble.is_empty()
            && self.pending_m73.is_none()
            && self.file.as_ref().is_none_or(|f| self.next_line >= f.len())
    }

    /// The next preamble, progress or file command.
    pub(super) fn next_stream_command(&mut self) -> Option<Meta> {
        if !self.streaming() {
            return None;
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
            self.next_line += 1;
            let Some(cmd) = file.command(index) else {
                continue;
            };
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
        self.shared.update(|s| s.job = Some(info));
        self.shared.emit(Event::Job(status));
    }

    pub(super) fn start_print(
        &mut self,
        file: Arc<GcodeFile>,
        options: PrintOptions,
        now: Instant,
    ) -> Result<(), HostError> {
        self.check_can_start()?;
        let start = options.start_line;
        if start > file.len() {
            return Err(HostError::InvalidArgument(format!(
                "start line {start} is past the end of the file ({} lines)",
                file.len()
            )));
        }
        let bytes_total = file.byte_len() as u64;
        let mut info = JobInfo::new(JobKind::HostPrint, file.name(), file.len(), bytes_total);
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
        let mut job = JobRuntime::new(info, Some(file), options, now);
        job.preamble = preamble;
        job.machine = machine;
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
        let info = JobInfo::new(
            JobKind::SdUpload,
            name.clone(),
            file.len(),
            file.byte_len() as u64,
        );
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
        match (job.info.kind, &job.info.status) {
            (
                JobKind::HostPrint,
                JobStatus::Starting | JobStatus::Printing | JobStatus::Resuming,
            ) => {
                job.pause_script_queued = false;
                self.set_job_status(JobStatus::Pausing, now);
                self.advance_job(now);
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

    pub(super) fn resume_job(&mut self, now: Instant) {
        let Some(job) = &mut self.job else {
            return;
        };
        match (job.info.kind, &job.info.status) {
            (JobKind::HostPrint, JobStatus::Paused) => {
                if std::mem::take(&mut job.paused_by_firmware) || job.pause_snapshot.is_none() {
                    // The firmware paused (and parks) itself: just continue;
                    // release a firmware waiting for the user.
                    let waiting = self
                        .shared
                        .state
                        .read()
                        .busy
                        .as_deref()
                        .is_some_and(|b| b.to_ascii_lowercase().contains("paused for user"));
                    if waiting {
                        self.send_immediate("M108", Source::User);
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
                // Restore the stream's modes (the scripts switch to G91/G90).
                if !snapshot.absolute {
                    lines.push("G91".to_string());
                } else if !snapshot.absolute_e {
                    lines.push("M83".to_string());
                }
                let lines = in_millimetres(lines, &snapshot, true);
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
                job.paused_by_firmware = false;
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
                let lines = in_millimetres(lines, &job.machine, false);
                job.queue_scripts(lines, Some(JobStatus::Cancelled));
                if waiting {
                    // Break out of a heat wait or user pause first.
                    self.send_immediate("M108", Source::User);
                }
            }
            JobKind::SdPrint => {
                job.queue_scripts(vec!["M524".to_string()], Some(JobStatus::Cancelled));
                // M524 is not an emergency command: break a heat wait first.
                self.send_immediate("M108", Source::User);
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
            job.paused_by_firmware = true;
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
                if job.paused_by_firmware {
                    job.paused_by_firmware = false;
                    self.info("The printer resumed the job");
                    self.set_job_status(JobStatus::Printing, now);
                } else {
                    self.resume_job(now);
                }
            }
            JobKind::SdPrint => {
                job.paused_by_firmware = false;
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
        if meta.is_heat_wait() {
            let since_last = job
                .last_ack_at
                .map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
            job.waited += rtt.map_or(since_last, |r| r.min(since_last));
        }
        job.last_ack_at = Some(now);
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
            let lines = render_job_script(&self.shared, &template, &snapshot);
            let lines = guarded(&self.shared, lines, self.homed, "pause");
            let lines = in_millimetres(lines, &snapshot, false);
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
        if status == JobStatus::Completed {
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
