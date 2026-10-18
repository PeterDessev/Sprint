//! Host interaction: //action: prompts, M876 responses, M0/M1 user
//! pauses, M600 filament change and the filament runout sensor.

use super::{Machine, Outcome, TaskKind};
use crate::parse::Command;

/// NOZZLE_PARK_POINT offsets and feedrates.
const PARK_XY_FEED: f64 = 100.0;
const PARK_Z_FEED: f64 = 5.0;
/// FILAMENT_CHANGE_UNLOAD_FEEDRATE (mm/s).
const UNLOAD_FEED: f64 = 10.0;
/// ADVANCED_PAUSE_PURGE_LENGTH / FEEDRATE.
const PURGE_LENGTH: f64 = 50.0;
const PURGE_FEED: f64 = 3.0;

/// The kind of host prompt that is open (Marlin's PromptReason).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Prompt {
    #[default]
    None,
    /// M600 waiting for the user ("Nozzle Parked" / "Continue").
    UserContinue,
    /// SD print paused by M25 ("Pause SD" / "Resume").
    PauseResume,
}

/// Phase of an M600 filament change.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Parking,
    WaitUser,
    Loading,
}

/// M600 in progress.
#[derive(Clone, Debug)]
pub(crate) struct FilamentChange {
    phase: Phase,
    until: f64,
    resume: [f64; 4],
    load_time: f64,
}

impl FilamentChange {
    /// Waiting for the user (busy: paused for user).
    pub(crate) fn waiting_for_user(&self) -> bool {
        self.phase == Phase::WaitUser
    }
}

/// M0 / M1 waiting for the user (or for its timeout).
#[derive(Clone, Debug)]
pub(crate) struct UserWait {
    /// 0 for M0, 1 for M1 (the prompt says "M0 Stop" / "M1 Stop").
    code: u16,
    /// Free-text message to echo.
    message: Option<String>,
    /// P/S timeout in seconds; 0 waits forever.
    timeout: f64,
    /// When the timeout expires.
    until: f64,
}

/// Marlin's string_arg for M0/M1: the text after any leading parameters
/// with a value ("M0 S10 Insert magnets" -> "Insert magnets").
fn user_message(args: &str) -> Option<String> {
    let mut rest = args.trim();
    loop {
        let b = rest.as_bytes();
        let is_param = b.first().is_some_and(u8::is_ascii_alphabetic)
            && b.get(1)
                .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+'));
        if !is_param {
            break;
        }
        rest = rest.find(' ').map_or("", |i| rest[i..].trim_start());
    }
    (!rest.is_empty()).then(|| rest.to_string())
}

impl Machine {
    /// Open a prompt: prompt_end, prompt_begin, buttons, prompt_show.
    pub(super) fn open_prompt(&mut self, kind: Prompt, message: &str, buttons: &[&str]) {
        self.prompt = kind;
        self.emit("//action:prompt_end");
        self.emit(format!("//action:prompt_begin {message}"));
        for b in buttons {
            self.emit(format!("//action:prompt_button {b}"));
        }
        self.emit("//action:prompt_show");
    }

    /// M876 S<n> (handled by the emergency parser).
    pub(super) fn prompt_response(&mut self, _response: i64) {
        match self.prompt {
            Prompt::UserContinue => self.wait_for_user = false,
            Prompt::PauseResume => self.injected.push_back("M24".to_string()),
            Prompt::None => {}
        }
        self.prompt = Prompt::None;
    }

    /// M0 / M1 [P<ms>] [S<s>] [message]: wait for the user
    /// (HAS_RESUME_CONTINUE via EMERGENCY_PARSER) after the planner drains.
    pub(super) fn cmd_user_wait(&mut self, c: &Command) -> Outcome {
        let mut secs = c.value('P').map_or(0.0, |ms| ms / 1000.0);
        if let Some(s) = c.value('S') {
            secs = s;
        }
        let timeout = if secs.is_finite() {
            secs.clamp(0.0, 86_400.0)
        } else {
            0.0
        };
        Outcome::Task {
            kind: TaskKind::UserWait(UserWait {
                code: c.code,
                message: user_message(c.args),
                timeout,
                until: 0.0,
            }),
            sync: true,
        }
    }

    pub(super) fn begin_user_wait(&mut self, w: &mut UserWait) {
        if let Some(m) = &w.message {
            self.emit(format!("echo:{m}"));
        }
        let title = if w.code == 1 { "M1 Stop" } else { "M0 Stop" };
        self.open_prompt(Prompt::UserContinue, title, &["Continue"]);
        self.wait_for_user = true;
        w.until = if w.timeout > 0.0 {
            self.now + w.timeout
        } else {
            f64::INFINITY
        };
    }

    /// Done once M108 / M876 cleared `wait_for_user` or the timeout passed.
    pub(super) fn progress_user_wait(&mut self, w: &mut UserWait) -> bool {
        if self.wait_for_user && self.now < w.until {
            return false;
        }
        self.wait_for_user = false;
        true
    }

    /// The runout sensor fired: tell the host and run M600.
    pub(super) fn runout_event(&mut self) {
        let already = matches!(&self.task, Some(t) if matches!(t.kind, TaskKind::FilamentChange(_)))
            || self.injected.iter().any(|c| c == "M600");
        if already || !self.settings.runout_enabled {
            return;
        }
        self.runout = true;
        let tool = self.motion.tool;
        self.emit(format!("//action:out_of_filament T{tool}"));
        self.injected.push_front("M600".to_string());
    }

    /// M600.
    pub(super) fn cmd_filament_change(&mut self) -> Outcome {
        Outcome::Task {
            kind: TaskKind::FilamentChange(FilamentChange {
                phase: Phase::Parking,
                until: 0.0,
                resume: self.motion.native,
                load_time: 0.0,
            }),
            sync: true,
        }
    }

    pub(super) fn begin_filament_change(&mut self, fc: &mut FilamentChange) {
        self.emit("//action:paused");
        fc.resume = self.motion.native;
        let vol = self.cfg.build_volume.map(f64::from);
        let from = self.motion.native;
        let park = [
            10.0f64.min(vol[0]),
            (vol[1] - 10.0).max(0.0),
            (from[2] + 2.0).max(20.0).min(vol[2]),
            from[3],
        ];
        let tool = self.motion.tool;
        let unload = self
            .settings
            .unload_length
            .get(tool)
            .copied()
            .unwrap_or(100.0);
        let (park_time, load_time) = if self.cfg.instant_moves {
            (0.0, 0.0)
        } else {
            let xy = (park[0] - from[0]).hypot(park[1] - from[1]);
            let z = (park[2] - from[2]).abs();
            (
                0.1 + z / PARK_Z_FEED + xy / PARK_XY_FEED + unload / UNLOAD_FEED,
                PURGE_LENGTH / PURGE_FEED + xy / PARK_XY_FEED + z / PARK_Z_FEED,
            )
        };
        fc.until = self.now + park_time;
        fc.load_time = load_time;
        self.motion.native = park;
        let e = self.planner.target()[3];
        let p = self.physical(park, e);
        self.planner.set_position(p);
    }

    pub(super) fn progress_filament_change(&mut self, fc: &mut FilamentChange) -> bool {
        match fc.phase {
            Phase::Parking => {
                if self.now >= fc.until {
                    fc.phase = Phase::WaitUser;
                    self.wait_for_user = true;
                    self.open_prompt(Prompt::UserContinue, "Nozzle Parked", &["Continue"]);
                }
                false
            }
            Phase::WaitUser => {
                if self.wait_for_user {
                    return false;
                }
                self.prompt = Prompt::None;
                fc.phase = Phase::Loading;
                fc.until = self.now + fc.load_time;
                false
            }
            Phase::Loading => {
                if self.now < fc.until {
                    return false;
                }
                self.motion.native = fc.resume;
                let e = self.planner.target()[3];
                let p = self.physical(fc.resume, e);
                self.planner.set_position(p);
                self.runout = false;
                self.emit("//action:prompt_end");
                self.emit("//action:resumed");
                true
            }
        }
    }
}
