//! SD card commands, SD printing and the print job timer commands.

use super::{FAN_COUNT, Machine, Origin, Outcome, Queued, TaskKind, host::Prompt};
use crate::format::{duration, whole_secs};
use crate::line::strip_comment;
use crate::parse::Command;

/// A selected (and possibly running) SD print.
#[derive(Clone, Debug)]
pub(crate) struct SdJob {
    /// Index of the file on the card.
    pub file: usize,
    /// Byte offset of the next line to read.
    pub pos: usize,
    /// Reading commands (M24) rather than paused/selected.
    pub printing: bool,
    /// M24 was sent at least once.
    pub started: bool,
    /// The whole file has been read; M1001 is queued.
    pub done_reading: bool,
}

impl Machine {
    /// M20-M33, M524, M1001 and M75-M78.
    pub(super) fn sd_or_job_command(&mut self, c: &Command) -> Outcome {
        let needs_media = matches!(c.code, 20 | 23 | 28 | 30 | 32 | 33);
        if needs_media && !self.sd_mounted {
            self.emit("echo:No media");
            return Outcome::Done;
        }
        match c.code {
            20 => {
                self.emit("Begin file list");
                let listing = self.sd.listing(c.seen('L'));
                self.out.extend(listing);
                self.emit("End file list");
            }
            21 => {
                self.sd_mounted = true;
                self.emit("echo:SD card ok");
            }
            22 => {
                if !self.sd_job.as_ref().is_some_and(|j| j.printing) {
                    self.sd_mounted = false;
                    self.sd_job = None;
                    self.emit("echo:SD card released");
                }
            }
            23 => {
                self.sd_select(c.args);
            }
            24 => self.sd_start(c),
            25 => self.sd_pause(),
            26 => {
                if let (Some(job), Some(pos)) = (self.sd_job.as_mut(), c.value('S')) {
                    let size = self.sd.file(job.file).map_or(0, |f| f.size as usize);
                    job.pos = if pos.is_finite() {
                        (pos.max(0.0) as usize).min(size)
                    } else {
                        0
                    };
                    job.done_reading = false;
                }
            }
            27 => {
                if c.seen('C') {
                    let line = match self.sd_job.as_ref().and_then(|j| self.sd.file(j.file)) {
                        Some(f) => format!("Current file: {} {}", f.short_name, f.long_name),
                        None => "Current file: (no file)".to_string(),
                    };
                    self.emit(line);
                } else if let Some(interval) = super::commands::auto_interval(c) {
                    self.sd_interval = interval;
                    self.next_sd = self.now + interval;
                } else {
                    let line = self.sd_status_line();
                    self.emit(line);
                }
            }
            28 => {
                let name = c.args.trim();
                match self.sd.create(name) {
                    Some(index) => {
                        self.saving = Some(index);
                        self.emit(format!("Writing to file: {name}"));
                    }
                    None => self.emit(format!("open failed, File: {name}.")),
                }
            }
            // M29 outside of a write does nothing.
            29 => {}
            30 => self.sd_delete(c.args.trim()),
            31 => {
                let d = self.timer.duration(self.now);
                self.emit(format!("echo:Print time: {}", duration(whole_secs(d))));
            }
            32 => {
                let args = c.args;
                let name = match args.find('!') {
                    Some(i) => {
                        let rest = &args[i + 1..];
                        rest.split('#').next().unwrap_or("")
                    }
                    None => args,
                };
                if self.sd_select(name.trim()) {
                    self.sd_start(&Command::parse("M24"));
                }
            }
            33 => {
                if let Some(f) = self.sd.find(c.args).and_then(|i| self.sd.file(i)) {
                    let name = f.long_name.clone();
                    self.emit(name);
                }
            }
            524 => self.sd_abort(),
            1001 => {
                return Outcome::Task {
                    kind: TaskKind::SdFinish,
                    sync: true,
                };
            }
            75 => self.timer.start(self.now, &mut self.stats),
            76 => self.timer.pause(self.now),
            77 => self.timer.stop(self.now, &mut self.stats),
            78 => {
                if c.int('S') == Some(78) {
                    self.stats = super::reports::Stats::default();
                } else {
                    let report = self.stats.report(&self.timer, self.now);
                    self.out.extend(report);
                }
            }
            _ => return self.unknown(c),
        }
        Outcome::Done
    }

    /// M23: open a file for printing. Returns whether it was found.
    fn sd_select(&mut self, name: &str) -> bool {
        let name = name.trim();
        match self.sd.find_lenient(name) {
            Some(index) => {
                let size = self.sd.file(index).map_or(0, |f| f.size);
                self.sd_job = Some(SdJob {
                    file: index,
                    pos: 0,
                    printing: false,
                    started: false,
                    done_reading: false,
                });
                self.emit(format!("File opened: {name} Size: {size}"));
                self.emit("File selected");
                true
            }
            None => {
                self.emit(format!("open failed, File: {name}."));
                false
            }
        }
    }

    /// M24: start or resume.
    fn sd_start(&mut self, c: &Command) {
        if let Some(job) = self.sd_job.as_mut() {
            if let Some(pos) = c.value('S')
                && pos.is_finite()
            {
                job.pos = pos.max(0.0) as usize;
            }
            job.printing = true;
            job.started = true;
            self.timer.start(self.now, &mut self.stats);
            if self.prompt == Prompt::PauseResume {
                self.prompt = Prompt::None;
                self.emit("//action:prompt_end");
            }
        }
    }

    /// M25: pause reading (queued moves still finish).
    fn sd_pause(&mut self) {
        if let Some(job) = self.sd_job.as_mut()
            && job.printing
        {
            job.printing = false;
            self.timer.pause(self.now);
            self.open_prompt(Prompt::PauseResume, "Pause SD", &["Resume"]);
        }
    }

    /// M30.
    fn sd_delete(&mut self, name: &str) {
        match self.sd.delete(name) {
            Some(index) => {
                if let Some(job) = &mut self.sd_job {
                    if job.file == index {
                        self.sd_job = None;
                    } else if job.file > index {
                        job.file -= 1;
                    }
                }
                if let Some(s) = self.saving {
                    if s == index {
                        self.saving = None;
                    } else if s > index {
                        self.saving = Some(s - 1);
                    }
                }
                self.emit(format!("File deleted:{name}"));
            }
            None => self.emit(format!("Deletion failed, File: {name}.")),
        }
    }

    /// M524: abort the SD print (stop moves, heaters and fans off).
    fn sd_abort(&mut self) {
        let Some(job) = self.sd_job.take() else {
            return;
        };
        if !job.started {
            return;
        }
        self.queue.retain(|q| q.origin != Origin::Sd);
        if self.task.as_ref().is_some_and(|t| t.origin == Origin::Sd) {
            self.task = None;
        }
        self.quickstop();
        self.timer.abort(self.now, &mut self.stats);
        self.disable_all_heaters();
        self.fans = [0; FAN_COUNT];
        self.wait_for_heatup = false;
    }

    /// "SD printing byte x/y" or "Not SD printing".
    pub(super) fn sd_status_line(&self) -> String {
        match self.sd_job.as_ref() {
            Some(job) => {
                let size = self.sd.file(job.file).map_or(0, |f| f.size);
                format!("SD printing byte {}/{size}", job.pos)
            }
            None => "Not SD printing".to_string(),
        }
    }

    /// Read the next command of a running SD print into the queue.
    pub(super) fn read_sd_line(&mut self, budget: &mut usize) -> bool {
        let Some((file, mut pos)) = self
            .sd_job
            .as_ref()
            .filter(|j| j.printing && !j.done_reading)
            .map(|j| (j.file, j.pos))
        else {
            return false;
        };
        loop {
            if *budget == 0 {
                if let Some(job) = &mut self.sd_job {
                    job.pos = pos;
                }
                return false;
            }
            *budget -= 1;
            let data = self.sd.data(file);
            if pos >= data.len() {
                if let Some(job) = &mut self.sd_job {
                    job.pos = data.len();
                    job.done_reading = true;
                }
                self.queue.push_back(Queued {
                    text: "M1001".to_string(),
                    origin: Origin::Sd,
                });
                return true;
            }
            let end = data[pos..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(data.len(), |i| pos + i);
            let raw = String::from_utf8_lossy(&data[pos..end]).into_owned();
            pos = (end + 1).min(data.len());
            let cmd = strip_comment(&raw).trim();
            if cmd.is_empty() {
                continue;
            }
            let text = cmd.to_string();
            if let Some(job) = &mut self.sd_job {
                job.pos = pos;
            }
            self.queue.push_back(Queued {
                text,
                origin: Origin::Sd,
            });
            return true;
        }
    }

    /// Marlin's M1001: report, stop the timer, announce, release motors.
    pub(super) fn sd_finish(&mut self) {
        let d = self.timer.duration(self.now);
        if d > 60.0 {
            self.emit(format!("echo:Print time: {}", duration(whole_secs(d))));
        }
        self.timer.stop(self.now, &mut self.stats);
        self.emit("Done printing file");
        self.motion.steppers = false;
        self.sd_job = None;
    }
}
