//! Received lines: console, reply dispatch, attribution of multi-line
//! responses to the command that produced them, and state updates.

use std::time::{Duration, Instant};

use sprint_proto::{
    FirmwareInfo, HeaterReading, HostAction, Reply, SdReply, StatsReport, TempReport,
};

use super::flow::{OkOutcome, ResendAction, classify_line_error};
use super::meta::{CmdMeta, Meta, PollKind, Source};
use super::{Engine, Phase};
use crate::console::ConsoleKind;
use crate::event::Event;
use crate::state::{AutotuneState, Heater, HostPrompt, PrinterState, TEMP_HISTORY_LEN, TempSample};

/// One temperature history sample per this interval at most.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// A `wait` from the firmware means lost `ok`s only for commands older than
/// this (it may have been printed before a fresh command arrived).
const WAIT_MIN_AGE: Duration = Duration::from_secs(1);

impl Engine {
    /// A line from the reader thread.
    pub(super) fn on_line(&mut self, line: String) {
        let now = Instant::now();
        self.flow.begin_line();
        let replies = self.parser.feed(&line);
        let auto = self.is_auto(&replies);
        self.shared.log(ConsoleKind::Received, line, auto, None);
        if let Phase::Handshake { .. } = self.phase {
            self.on_handshake_replies(replies, now);
            return;
        }
        // What an `ok` line carries ("ok T:...", "ok FIRMWARE_NAME:...")
        // belongs to the command it acknowledges: apply it first.
        let (oks, others): (Vec<Reply>, Vec<Reply>) = replies
            .into_iter()
            .partition(|r| matches!(r, Reply::Ok { .. }));
        for reply in others.into_iter().chain(oks) {
            self.on_reply(reply, now);
            if self.exit.is_some() {
                return;
            }
        }
    }

    /// Automatic traffic: temperature reports, auto-reports and the `ok`s of
    /// polls.
    fn is_auto(&self, replies: &[Reply]) -> bool {
        if replies.is_empty() {
            return false;
        }
        replies.iter().all(|r| match r {
            Reply::Temperatures(_) => true,
            Reply::Ok { .. } => self.flow.front().is_some_and(|e| e.meta.auto),
            Reply::Position(_) => !self.flow.in_flight().any(|e| e.meta.is('M', 114)),
            Reply::SdCard(SdReply::Progress { .. } | SdReply::NotPrinting) => true,
            _ => false,
        })
    }

    fn on_handshake_replies(&mut self, replies: Vec<Reply>, now: Instant) {
        if let Phase::Handshake { line_seen, .. } = &mut self.phase {
            *line_seen = true;
        }
        for reply in replies {
            match reply {
                Reply::Ok { line, .. } => {
                    self.stats.oks += 1;
                    if let OkOutcome::Acked(acked) = self.flow.on_ok(line, now)
                        && acked.iter().any(|a| a.meta.is('M', 110))
                    {
                        self.handshake_done(now);
                        return;
                    }
                }
                Reply::Resend(n) => {
                    self.stats.resends += 1;
                    let _ = self.flow.on_resend(n, now);
                }
                Reply::Error { message, fatal } => {
                    self.stats.errors += 1;
                    if fatal {
                        self.shared.update(|s| s.last_error = Some(message));
                    } else if let Some(kind) = classify_line_error(&message) {
                        self.flow.on_error(kind);
                    }
                }
                Reply::Firmware(info) => self.on_firmware(info),
                Reply::Capability { name, enabled } => self.on_capability(name, enabled),
                _ => {}
            }
        }
    }

    fn on_reply(&mut self, reply: Reply, now: Instant) {
        match reply {
            Reply::Ok { line, .. } => self.on_ok(line, now),
            Reply::Resend(n) => self.on_resend(n, now),
            Reply::Temperatures(t) => self.on_temperatures(t, now),
            Reply::Position(p) => {
                self.shared.update(|s| s.position = Some(p.clone()));
                self.shared.emit(Event::Position(p));
            }
            Reply::Busy(message) => {
                self.flow.note_activity(now);
                self.shared.update(|s| s.busy = Some(message));
            }
            Reply::Wait => self.on_wait(now),
            Reply::Start => self.board_reset(now),
            Reply::Error { message, fatal } => self.on_error(message, fatal, now),
            Reply::UnknownCommand(cmd) => self.on_unknown_command(cmd),
            Reply::Firmware(info) => self.on_firmware(info),
            Reply::Capability { name, enabled } => self.on_capability(name, enabled),
            Reply::Setting(cmd) => {
                if self.flow.in_flight().any(|e| e.meta.is('M', 503)) {
                    self.pending_settings
                        .get_or_insert_with(Default::default)
                        .insert(cmd);
                }
            }
            Reply::SdCard(sd) => self.on_sd(sd, now),
            Reply::Endstop { name, triggered } => {
                if self.flow.in_flight().any(|e| e.meta.is('M', 119)) {
                    self.pending_endstops.push((name, triggered));
                } else {
                    let all = self.shared.update(|s| {
                        match s.endstops.iter_mut().find(|(n, _)| *n == name) {
                            Some(e) => e.1 = triggered,
                            None => s.endstops.push((name, triggered)),
                        }
                        s.endstops.clone()
                    });
                    self.shared.emit(Event::Endstops(all));
                }
            }
            Reply::Mesh(mesh) => {
                self.shared.update(|s| s.mesh = Some(mesh.clone()));
                self.shared.emit(Event::Mesh(mesh));
            }
            Reply::AutotuneResult { kp, ki, kd } => self.on_autotune_result(kp, ki, kd),
            Reply::AutotuneFailed(message) => self.on_autotune_failed(message),
            Reply::Stats(stats) => {
                if self.flow.in_flight().any(|e| e.meta.is('M', 78)) {
                    self.pending_stats
                        .get_or_insert_with(StatsReport::default)
                        .merge(&stats);
                } else {
                    let merged = self.shared.update(|s| {
                        let merged = s.stats.get_or_insert_with(StatsReport::default);
                        merged.merge(&stats);
                        merged.clone()
                    });
                    self.shared.emit(Event::Stats(merged));
                }
            }
            Reply::HostAction(action) => self.on_host_action(action, now),
            Reply::ActiveExtruder(tool) => self.shared.update(|s| s.active_tool = tool),
            Reply::Echo(message) | Reply::Info(message) => self.firmware_message(message),
            Reply::PrintTime(_) | Reply::Other(_) => {}
        }
    }

    fn on_ok(&mut self, line: Option<u32>, now: Instant) {
        self.stats.oks += 1;
        self.leave_halt();
        match self.flow.on_ok(line, now) {
            OkOutcome::Acked(acked) => {
                for a in acked {
                    let ms = a.rtt.as_secs_f32() * 1000.0;
                    self.stats.rtt_last_ms = ms;
                    self.stats.rtt_max_ms = self.stats.rtt_max_ms.max(ms);
                    self.stats.rtt_avg_ms = if self.stats.oks <= 1 || self.stats.rtt_avg_ms == 0.0 {
                        ms
                    } else {
                        self.stats.rtt_avg_ms * 0.9 + ms * 0.1
                    };
                    self.ack_times.push_back(now);
                    self.complete_with_rtt(&a.meta, Some(a.rtt), now);
                }
            }
            OkOutcome::Swallowed => {}
            OkOutcome::Spurious => log::debug!("ok without a command in flight"),
        }
        let busy = self.shared.state.read().busy.is_some();
        if busy {
            self.shared.update(|s| s.busy = None);
        }
        self.advance_job(now);
    }

    fn on_resend(&mut self, n: u32, now: Instant) {
        self.stats.resends += 1;
        self.leave_halt();
        if self.halted {
            return;
        }
        match self.flow.on_resend(n, now) {
            ResendAction::Rewind { from, stale } => {
                log::debug!("resend from line {from} ({stale} stale lines)");
            }
            ResendAction::Duplicate | ResendAction::Barrier | ResendAction::NothingToResend => {}
            ResendAction::Desync { expected } => {
                self.warn(format!(
                    "The printer expects line {expected}, which was never sent; resetting the line counter"
                ));
                self.renumber_pending = true;
            }
            ResendAction::Unavailable { line } => {
                let message =
                    format!("The printer asked for line {line}, which is no longer available");
                self.shared
                    .log(ConsoleKind::Error, message.clone(), false, None);
                self.shared.emit(Event::Error(message.clone()));
                if self.job.is_some() {
                    self.fail_job(&message);
                }
                self.renumber_pending = true;
            }
        }
    }

    /// Marlin prints `wait` when its queue has been empty for a while; with
    /// commands in flight that means their `ok`s were lost.
    fn on_wait(&mut self, now: Instant) {
        if self.halted || !matches!(self.phase, Phase::Ready) {
            return;
        }
        let stale = self
            .flow
            .front()
            .is_some_and(|e| now.saturating_duration_since(e.sent_at) >= WAIT_MIN_AGE);
        if !stale {
            return;
        }
        self.stats.timeouts += 1;
        let released = self.flow.release_all(now);
        self.warn(format!(
            "The printer is idle but {} command(s) were not acknowledged; resynchronising",
            released.len()
        ));
        for m in &released {
            self.complete(m, now);
        }
        self.internal
            .push_front(CmdMeta::new("M105", Source::Recovery));
        self.advance_job(now);
    }

    fn on_error(&mut self, message: String, fatal: bool, now: Instant) {
        self.stats.errors += 1;
        if fatal {
            if self.job.is_some() {
                self.fail_job(&message);
            }
            self.enter_halt(&message, now);
            return;
        }
        if let Some(kind) = classify_line_error(&message) {
            self.flow.on_error(kind);
            return;
        }
        let text = format!("Error: {message}");
        self.shared.update(|s| {
            s.last_error = Some(message.clone());
            s.push_message(text.clone());
        });
        self.shared.emit(Event::FirmwareMessage(text));
        if message.to_ascii_lowercase().contains("open failed") {
            self.job_open_failed(&message);
        }
    }

    fn on_unknown_command(&mut self, cmd: String) {
        let text = format!("Unknown command: {cmd}");
        self.shared.update(|s| s.push_message(text.clone()));
        self.shared.emit(Event::FirmwareMessage(text));
        if cmd.trim().to_ascii_uppercase().starts_with("M524")
            || self.flow.front().is_some_and(|e| e.meta.is('M', 524))
        {
            self.sd_abort_unsupported();
        }
    }

    fn on_firmware(&mut self, info: FirmwareInfo) {
        self.shared.update(|s| {
            s.firmware = Some(info.clone());
            s.capabilities.clear();
        });
        self.shared.emit(Event::Firmware(info));
    }

    fn on_capability(&mut self, name: String, enabled: bool) {
        self.shared.update(
            |s| match s.capabilities.iter_mut().find(|(n, _)| *n == name) {
                Some(c) => c.1 = enabled,
                None => s.capabilities.push((name, enabled)),
            },
        );
    }

    fn on_temperatures(&mut self, report: TempReport, now: Instant) {
        if self.flow.in_flight().any(|e| e.meta.blocks_polling()) {
            // Heat waits and autotune report temperatures: they are alive.
            self.flow.note_activity(now);
        }
        let sample = self
            .last_temp_sample
            .is_none_or(|t| now.duration_since(t) >= SAMPLE_INTERVAL);
        if sample {
            self.last_temp_sample = Some(now);
        }
        let t = now.duration_since(self.opened_at).as_secs_f64();
        let merged = self.shared.update(|s| {
            merge_temperatures(&mut s.temps, &report);
            if sample {
                push_sample(s, t);
            }
            s.temps.clone()
        });
        self.shared.emit(Event::Temperatures(merged));
    }

    fn on_sd(&mut self, sd: SdReply, now: Instant) {
        match sd {
            SdReply::ListBegin => {
                self.pending_files.clear();
                self.shared.update(|s| s.sd.listing = true);
            }
            SdReply::File(f) => self.pending_files.push(f),
            SdReply::ListEnd => {
                let files = std::mem::take(&mut self.pending_files);
                self.shared.update(|s| {
                    s.sd.files = files.clone();
                    s.sd.listing = false;
                    s.sd.card_present = Some(true);
                });
                self.shared.emit(Event::SdFiles(files));
            }
            SdReply::CardOk => self.shared.update(|s| s.sd.card_present = Some(true)),
            SdReply::CardFailed(_) | SdReply::NoCard => {
                self.shared.update(|s| s.sd.card_present = Some(false));
            }
            SdReply::Opened { name, size } => {
                self.shared.update(|s| s.sd.selected = Some(name));
                self.sd_opened(size);
            }
            SdReply::Selected | SdReply::WritingTo(_) => {
                if matches!(sd, SdReply::WritingTo(_)) {
                    self.upload_confirmed(now);
                }
            }
            SdReply::Progress { done, total } => {
                self.shared.update(|s| s.sd.printing = Some((done, total)));
                self.sd_progress(done, total);
            }
            SdReply::NotPrinting => self.shared.update(|s| s.sd.printing = None),
            SdReply::DonePrinting => {
                self.shared.update(|s| s.sd.printing = None);
                self.sd_done(now);
            }
            SdReply::Saved => self.upload_saved(now),
            SdReply::Deleted(name) => {
                let files = self.shared.update(|s| {
                    s.sd.files.retain(|f| {
                        !f.name.eq_ignore_ascii_case(&name)
                            && f.long_name
                                .as_deref()
                                .is_none_or(|l| !l.eq_ignore_ascii_case(&name))
                    });
                    s.sd.files.clone()
                });
                self.shared.emit(Event::SdFiles(files));
            }
            SdReply::DeleteFailed(name) => {
                self.warn(format!("Could not delete {name} from the SD card"))
            }
        }
    }

    fn on_autotune_result(&mut self, kp: f32, ki: f32, kd: f32) {
        let heater = self.shared.update(|s| {
            let a = s.autotune.get_or_insert(AutotuneState {
                heater: Heater::Hotend(0),
                target: 0.0,
                cycles: 0,
                running: false,
                result: None,
                error: None,
            });
            a.running = false;
            a.result = Some((kp, ki, kd));
            a.error = None;
            a.heater
        });
        self.info(format!("PID autotune finished: Kp {kp} Ki {ki} Kd {kd}"));
        self.shared
            .emit(Event::AutotuneFinished { heater, kp, ki, kd });
        self.internal.push_back(CmdMeta::new("M503", Source::User));
    }

    fn on_autotune_failed(&mut self, message: String) {
        self.shared.update(|s| {
            if let Some(a) = &mut s.autotune {
                a.running = false;
                a.error = Some(message.clone());
            }
        });
        self.warn(format!("PID autotune failed: {message}"));
        self.shared.emit(Event::AutotuneFailed(message));
    }

    fn on_host_action(&mut self, action: HostAction, now: Instant) {
        match action {
            HostAction::Pause | HostAction::Paused => self.firmware_pause(now),
            HostAction::Resume | HostAction::Resumed => self.firmware_resume(now),
            HostAction::Cancel => {
                self.info("The printer asked to cancel the job");
                self.cancel_job(now);
            }
            HostAction::Disconnect => {
                self.close(
                    crate::state::ConnectionStatus::Disconnected,
                    "Disconnected (requested by the printer)",
                );
            }
            HostAction::OutOfFilament(tool) => {
                let text = match tool {
                    Some(t) => format!("Filament runout ({t})"),
                    None => "Filament runout".to_string(),
                };
                self.warn(text.clone());
                self.firmware_message(text);
            }
            HostAction::Notification(text) => self.firmware_message(text),
            HostAction::PromptBegin(message) => {
                self.prompt = Some(HostPrompt {
                    message,
                    buttons: Vec::new(),
                });
            }
            HostAction::PromptButton(label) | HostAction::PromptChoice(label) => {
                self.prompt
                    .get_or_insert_with(|| HostPrompt {
                        message: String::new(),
                        buttons: Vec::new(),
                    })
                    .buttons
                    .push(label);
            }
            HostAction::PromptShow => {
                let prompt = self.prompt.clone().unwrap_or(HostPrompt {
                    message: String::new(),
                    buttons: Vec::new(),
                });
                self.shared.update(|s| s.prompt = Some(prompt.clone()));
                self.shared.emit(Event::Prompt(Some(prompt)));
            }
            HostAction::PromptEnd => {
                self.prompt = None;
                let had = self.shared.update(|s| s.prompt.take().is_some());
                if had {
                    self.shared.emit(Event::Prompt(None));
                }
            }
            HostAction::Start | HostAction::Other(_) => {}
        }
    }

    pub(super) fn firmware_message(&self, message: String) {
        self.shared.update(|s| s.push_message(message.clone()));
        self.shared.emit(Event::FirmwareMessage(message));
    }

    /// A command is done (acknowledged, or released after a timeout).
    pub(super) fn complete(&mut self, meta: &Meta, now: Instant) {
        self.complete_with_rtt(meta, None, now);
    }

    fn complete_with_rtt(&mut self, meta: &Meta, rtt: Option<Duration>, now: Instant) {
        match meta.source {
            Source::Poll(PollKind::Temperature) => self.temp_poll_outstanding = false,
            Source::Poll(PollKind::SdStatus) => self.sd_poll_outstanding = false,
            Source::Setup if meta.is('M', 115) => self.continue_setup(),
            _ => {}
        }
        if let Some(code) = meta.code {
            self.apply_effects(meta, code, now);
        }
        if let Some(line) = meta.job_line() {
            self.job_ack(meta, line, rtt, now);
        }
    }

    /// State changes implied by a completed command.
    fn apply_effects(&mut self, meta: &CmdMeta, code: sprint_gcode::Code, now: Instant) {
        let _ = now;
        let parsed = sprint_gcode::parse_line(&meta.text);
        let int = |letter: char| {
            parsed
                .get(letter)
                .filter(|v| v.is_finite())
                .map(|v| v.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64)
        };
        match (code.letter, code.number) {
            (b'M', 503) => {
                let settings = self.pending_settings.take().unwrap_or_default();
                self.shared.update(|s| {
                    s.settings = settings;
                    s.settings_loaded = true;
                });
                self.shared.emit(Event::SettingsUpdated);
            }
            (b'M', 119) => {
                let endstops = std::mem::take(&mut self.pending_endstops);
                self.shared.update(|s| s.endstops = endstops.clone());
                self.shared.emit(Event::Endstops(endstops));
            }
            (b'M', 78) => {
                if let Some(stats) = self.pending_stats.take() {
                    self.shared.update(|s| s.stats = Some(stats.clone()));
                    self.shared.emit(Event::Stats(stats));
                }
            }
            (b'M', 155) => {
                if let Some(s) = int('S') {
                    self.temp_autoreport = s > 0;
                }
            }
            (b'M', 27) => {
                if let Some(s) = int('S') {
                    self.sd_autoreport = s > 0;
                }
            }
            (b'M', 220) => {
                if let Some(pct) = parsed.get('S').filter(|v| v.is_finite()) {
                    self.shared.update(|s| s.feedrate_pct = pct as f32);
                }
            }
            (b'M', 221) => {
                if let Some(pct) = parsed.get('S').filter(|v| v.is_finite()) {
                    let tool = int('T').map_or_else(
                        || usize::from(self.shared.state.read().active_tool),
                        |t| t.clamp(0, 255) as usize,
                    );
                    self.shared.update(|s| {
                        if s.flow_pct.len() <= tool {
                            s.flow_pct.resize(tool + 1, 100.0);
                        }
                        s.flow_pct[tool] = pct as f32;
                    });
                }
            }
            (b'M', 106) | (b'M', 107) => {
                let fan = int('P').map_or(0, |p| p.clamp(0, 255) as usize);
                let speed = if code.number == 107 {
                    0
                } else {
                    parsed
                        .get('S')
                        .filter(|v| v.is_finite())
                        .map_or(255, |v| v.clamp(0.0, 255.0).round() as u8)
                };
                self.shared.update(|s| {
                    if s.fan_speeds.len() <= fan {
                        s.fan_speeds.resize(fan + 1, 0);
                    }
                    s.fan_speeds[fan] = speed;
                });
            }
            (b'T', tool) => {
                if let Ok(t) = u8::try_from(tool) {
                    self.shared.update(|s| s.active_tool = t);
                }
            }
            (b'G', 28) => {
                let all = ['X', 'Y', 'Z'].iter().all(|a| parsed.has(*a))
                    || !['X', 'Y', 'Z'].iter().any(|a| parsed.has(*a));
                if all {
                    self.homed = true;
                }
            }
            (b'M', 18) | (b'M', 84) => {
                if !parsed.has('S') {
                    self.homed = false;
                }
            }
            (b'M', 28) => self.upload_open_acknowledged(),
            _ => {}
        }
    }
}

/// Merge `new` into `cur`: readings present in `new` replace the old ones,
/// missing ones keep their last value.
pub(super) fn merge_temperatures(cur: &mut TempReport, new: &TempReport) {
    for (i, h) in new.hotends.iter().enumerate() {
        if h.current.is_nan() {
            continue;
        }
        if cur.hotends.len() <= i {
            cur.hotends.resize(
                i + 1,
                HeaterReading {
                    current: f32::NAN,
                    target: None,
                    power: None,
                },
            );
        }
        cur.hotends[i] = h.clone();
    }
    if new.bed.is_some() {
        cur.bed.clone_from(&new.bed);
    }
    if new.chamber.is_some() {
        cur.chamber.clone_from(&new.chamber);
    }
    if new.cooler.is_some() {
        cur.cooler.clone_from(&new.cooler);
    }
    if new.probe.is_some() {
        cur.probe = new.probe;
    }
    if new.board.is_some() {
        cur.board = new.board;
    }
    if new.redundant.is_some() {
        cur.redundant = new.redundant;
    }
}

/// Append a history sample of the current temperatures.
fn push_sample(s: &mut PrinterState, t: f64) {
    let pair = |h: &HeaterReading| (h.current, h.target.unwrap_or(0.0));
    let sample = TempSample {
        t,
        hotends: s.temps.hotends.iter().map(pair).collect(),
        bed: s.temps.bed.as_ref().map(pair),
        chamber: s.temps.chamber.as_ref().map(pair),
        hotend_power: s
            .temps
            .hotends
            .iter()
            .map(|h| h.power.unwrap_or(0.0))
            .collect(),
        bed_power: s.temps.bed.as_ref().and_then(|b| b.power),
    };
    while s.temp_history.len() >= TEMP_HISTORY_LEN {
        s.temp_history.pop_front();
    }
    s.temp_history.push_back(sample);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(current: f32, target: f32) -> HeaterReading {
        HeaterReading {
            current,
            target: Some(target),
            power: Some(0.5),
        }
    }

    #[test]
    fn merging_keeps_missing_readings() {
        let mut cur = TempReport::default();
        merge_temperatures(
            &mut cur,
            &TempReport {
                hotends: vec![reading(200.0, 210.0)],
                bed: Some(reading(60.0, 60.0)),
                ..TempReport::default()
            },
        );
        // A report with only T1 keeps T0 and the bed.
        merge_temperatures(
            &mut cur,
            &TempReport {
                hotends: vec![
                    HeaterReading {
                        current: f32::NAN,
                        target: None,
                        power: None,
                    },
                    reading(150.0, 0.0),
                ],
                ..TempReport::default()
            },
        );
        assert_eq!(cur.hotends.len(), 2);
        assert_eq!(cur.hotends[0].current, 200.0);
        assert_eq!(cur.hotends[1].current, 150.0);
        assert_eq!(cur.bed.as_ref().map(|b| b.current), Some(60.0));
    }

    #[test]
    fn history_is_bounded() {
        let mut s = PrinterState::default();
        s.temps.hotends = vec![reading(20.0, 0.0)];
        for i in 0..(TEMP_HISTORY_LEN + 10) {
            push_sample(&mut s, i as f64);
        }
        assert_eq!(s.temp_history.len(), TEMP_HISTORY_LEN);
        let last = s.temp_history.back().unwrap();
        assert_eq!(last.hotends, [(20.0, 0.0)]);
        assert_eq!(last.hotend_power, [0.5]);
        assert_eq!(last.bed, None);
    }
}
