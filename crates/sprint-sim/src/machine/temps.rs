//! Temperature commands: targets, waits, reports, fans and PID autotune.

use super::{FAN_COUNT, HeaterId, Machine, Outcome, TaskKind};
use crate::format::{f2, fixed};
use crate::parse::Command;
use crate::thermal::{Autotune, Gains, Heater, TuneEvent};

/// TEMP_WINDOW / TEMP_BED_WINDOW / TEMP_CHAMBER_WINDOW.
const TEMP_WINDOW: f64 = 1.0;
/// TEMP_HYSTERESIS and friends.
const TEMP_HYSTERESIS: f64 = 3.0;
/// TEMP_RESIDENCY_TIME and friends (seconds).
const RESIDENCY: f64 = 10.0;
/// MIN_COOLING_SLOPE_DEG / MIN_COOLING_SLOPE_TIME.
const COOLING_SLOPE_DEG: f64 = 1.5;
const COOLING_SLOPE_TIME: f64 = 60.0;

/// M109/M190/M191 waiting for a temperature.
#[derive(Clone, Debug)]
pub(crate) struct HeatWait {
    heater: HeaterId,
    /// S: only wait while heating; R: wait for heating or cooling.
    no_wait_for_cooling: bool,
    residency_start: Option<f64>,
    next_report: f64,
    next_cool_check: f64,
    old_temp: f64,
    /// Target the cooling decision was made for (re-evaluated on change).
    target_seen: f64,
    wants_to_cool: bool,
}

/// M303 in progress.
#[derive(Clone, Debug)]
pub(crate) struct Tune {
    heater: HeaterId,
    tune: Autotune,
    apply: bool,
    last_sample: u64,
}

impl Machine {
    pub(super) fn heater(&self, id: HeaterId) -> Option<&Heater> {
        match id {
            HeaterId::Hotend(i) => self.hotends.get(i),
            HeaterId::Bed => self.bed.as_ref(),
            HeaterId::Chamber => self.chamber.as_ref(),
        }
    }

    /// Marlin's `disable_all_heaters()`: hotends, bed and chamber off.
    pub(super) fn disable_all_heaters(&mut self) {
        for h in &mut self.hotends {
            h.off();
        }
        for h in self.bed.iter_mut().chain(self.chamber.iter_mut()) {
            h.off();
        }
    }

    pub(super) fn heater_mut(&mut self, id: HeaterId) -> Option<&mut Heater> {
        match id {
            HeaterId::Hotend(i) => self.hotends.get_mut(i),
            HeaterId::Bed => self.bed.as_mut(),
            HeaterId::Chamber => self.chamber.as_mut(),
        }
    }

    /// Marlin `print_heater_states`, e.g. " T:20.0 /0.0 B:20.0 /0.0 @:0 B@:0".
    pub(super) fn temp_line(&self, decimals: usize, tool: usize) -> String {
        let f = |v: f64| fixed(v, decimals);
        let mut s = String::new();
        let tool = tool.min(self.hotends.len().saturating_sub(1));
        if let Some(h) = self.hotends.get(tool) {
            s.push_str(&format!(" T:{} /{}", f(h.temp()), f(h.target)));
        }
        if let Some(b) = &self.bed {
            s.push_str(&format!(" B:{} /{}", f(b.temp()), f(b.target)));
        }
        if let Some(c) = &self.chamber {
            s.push_str(&format!(" C:{} /{}", f(c.temp()), f(c.target)));
        }
        let multi = self.hotends.len() > 1;
        if multi {
            for (i, h) in self.hotends.iter().enumerate() {
                s.push_str(&format!(" T{i}:{} /{}", f(h.temp()), f(h.target)));
            }
        }
        let power = self.hotends.get(tool).map_or(0, Heater::reported_power);
        s.push_str(&format!(" @:{power}"));
        if let Some(b) = &self.bed {
            s.push_str(&format!(" B@:{}", b.reported_power()));
        }
        if let Some(c) = &self.chamber {
            s.push_str(&format!(" C@:{}", c.reported_power()));
        }
        if multi {
            for (i, h) in self.hotends.iter().enumerate() {
                s.push_str(&format!(" @{i}:{}", h.reported_power()));
            }
        }
        s
    }

    /// Marlin's `get_target_extruder_from_command()`: the T parameter (read
    /// as a byte, so T-1 is T0) or the active tool. `None`, after printing
    /// "echo:M<code> Invalid extruder <e>", when out of range.
    pub(super) fn target_tool(&mut self, c: &Command) -> Option<usize> {
        let Some(t) = c.int('T') else {
            return Some(self.motion.tool);
        };
        let e = t.clamp(0, 255) as usize;
        if e < self.hotends.len() {
            Some(e)
        } else {
            self.emit(format!("echo:M{} Invalid extruder {e}", c.code));
            None
        }
    }

    /// PRINTJOB_TIMER_AUTOSTART.
    fn auto_job_check(&mut self, can_start: bool, can_stop: bool) {
        let over = self.hotends.iter().any(|h| h.target > 170.0 / 2.0)
            || self.bed.as_ref().is_some_and(|b| b.target > 5.0)
            || self.chamber.as_ref().is_some_and(|c| c.target > 5.0);
        if over {
            if can_start {
                self.timer.start(self.now, &mut self.stats);
            }
        } else if can_stop && self.timer.is_active() {
            self.timer.stop(self.now, &mut self.stats);
        }
    }

    fn wait_task(&mut self, heater: HeaterId, no_wait_for_cooling: bool) -> Outcome {
        self.wait_for_heatup = true;
        Outcome::Task {
            kind: TaskKind::HeatWait(HeatWait {
                heater,
                no_wait_for_cooling,
                residency_start: None,
                next_report: self.now,
                next_cool_check: self.now,
                old_temp: 9999.0,
                target_seen: f64::NAN,
                wants_to_cool: false,
            }),
            sync: false,
        }
    }

    /// Target in °C and "don't wait for cooling", like Marlin: S wins
    /// (heat only); R (wait for heating or cooling) only counts for the
    /// waiting commands M109/M190/M191.
    fn temp_param(c: &Command, wait: bool) -> Option<(f64, bool)> {
        match (c.value('S'), c.value('R')) {
            (Some(s), _) => Some((s, true)),
            (None, Some(r)) if wait => Some((r, false)),
            _ => None,
        }
    }

    /// M104 / M109.
    pub(super) fn cmd_set_hotend(&mut self, c: &Command, wait: bool) -> Outcome {
        let Some(tool) = self.target_tool(c) else {
            return Outcome::Done;
        };
        let param = Machine::temp_param(c, wait);
        if let Some((t, _)) = param {
            if let Some(h) = self.hotends.get_mut(tool) {
                h.set_target(t);
            }
            self.auto_job_check(wait, true);
        }
        match (wait, param) {
            (true, Some((_, no_wait_for_cooling))) => {
                self.wait_task(HeaterId::Hotend(tool), no_wait_for_cooling)
            }
            _ => Outcome::Done,
        }
    }

    /// M140 / M190.
    pub(super) fn cmd_set_bed(&mut self, c: &Command, wait: bool) -> Outcome {
        if self.bed.is_none() {
            return self.unknown(c);
        }
        let param = Machine::temp_param(c, wait);
        if let (Some((t, _)), Some(b)) = (param, self.bed.as_mut()) {
            b.set_target(t);
            // M140 may stop the job timer, M190 may only start it.
            self.auto_job_check(wait, !wait);
        }
        match (wait, param) {
            (true, Some((_, no_wait_for_cooling))) => {
                self.wait_task(HeaterId::Bed, no_wait_for_cooling)
            }
            _ => Outcome::Done,
        }
    }

    /// M141 / M191.
    pub(super) fn cmd_set_chamber(&mut self, c: &Command, wait: bool) -> Outcome {
        if self.chamber.is_none() {
            return self.unknown(c);
        }
        let param = Machine::temp_param(c, wait);
        if let (Some((t, _)), Some(ch)) = (param, self.chamber.as_mut()) {
            ch.set_target(t);
        }
        match (wait, param) {
            (true, Some((_, no_wait_for_cooling))) => {
                self.wait_task(HeaterId::Chamber, no_wait_for_cooling)
            }
            _ => Outcome::Done,
        }
    }

    /// M105: "ok T:..." on one line.
    pub(super) fn cmd_report_temps(&mut self, c: &Command) -> Outcome {
        let Some(tool) = self.target_tool(c) else {
            return Outcome::Done;
        };
        let line = format!("ok{}", self.temp_line(1, tool));
        self.emit(line);
        Outcome::NoOk
    }

    /// M106 [P<fan>] [S<speed>] / M107 [P<fan>].
    pub(super) fn cmd_fan(&mut self, c: &Command, on: bool) -> Outcome {
        let index = c.int('P').unwrap_or(0);
        if !(0..FAN_COUNT as i64).contains(&index) {
            return Outcome::Done;
        }
        let speed = if on {
            c.value('S').map_or(255.0, |s| {
                if s.is_finite() {
                    s.clamp(0.0, 255.0)
                } else {
                    255.0
                }
            })
        } else {
            0.0
        };
        self.fans[index as usize] = speed.round() as u8;
        Outcome::Done
    }

    pub(super) fn progress_heat_wait(&mut self, w: &mut HeatWait) -> bool {
        if !self.wait_for_heatup {
            return true;
        }
        let Some(h) = self.heater(w.heater) else {
            return true;
        };
        let (temp, target) = (h.temp(), h.target);
        let tool = match w.heater {
            HeaterId::Hotend(i) => i,
            _ => self.motion.tool,
        };
        // Marlin decides heating vs cooling when the target (re)appears.
        if target != w.target_seen {
            w.target_seen = target;
            w.wants_to_cool = temp > target;
            if w.no_wait_for_cooling && w.wants_to_cool {
                return true;
            }
        }
        if super::due(self.now, &mut w.next_report, 1.0) {
            let remaining = match w.residency_start {
                Some(start) => {
                    let left = (RESIDENCY - (self.now - start)).max(0.0);
                    (left as i64).to_string()
                }
                None => "?".to_string(),
            };
            let line = format!("{} W:{remaining}", self.temp_line(2, tool));
            self.emit(line);
        }
        let diff = (target - temp).abs();
        match w.residency_start {
            None if diff < TEMP_WINDOW => w.residency_start = Some(self.now),
            Some(_) if diff > TEMP_HYSTERESIS => w.residency_start = Some(self.now),
            _ => {}
        }
        if w.wants_to_cool && self.now >= w.next_cool_check {
            // Give up if the heater is not cooling (e.g. M109 R0).
            if w.old_temp - temp < COOLING_SLOPE_DEG {
                return true;
            }
            w.next_cool_check = self.now + COOLING_SLOPE_TIME;
            w.old_temp = temp;
        }
        matches!(w.residency_start, Some(start) if self.now - start >= RESIDENCY)
    }

    /// M303 E<heater> S<temp> C<cycles> U<apply>.
    pub(super) fn cmd_autotune(&mut self, c: &Command) -> Outcome {
        let e = c.int('E').unwrap_or(0);
        let heater = match e {
            -1 if self.bed.is_some() => HeaterId::Bed,
            i if i >= 0 && (i as usize) < self.hotends.len() => HeaterId::Hotend(i as usize),
            _ => {
                self.emit("PID Autotune failed! Bad heater id");
                return Outcome::Done;
            }
        };
        let is_bed = heater == HeaterId::Bed;
        let default_temp = if is_bed {
            self.settings.preheat[0].bed
        } else {
            self.settings.preheat[0].hotend
        };
        let target = c
            .value('S')
            .filter(|v| v.is_finite())
            .unwrap_or(default_temp);
        let cycles = c.int('C').unwrap_or(5).clamp(3, 20) as u32;
        let max = self.heater(heater).map_or(0.0, |h| h.params().max_target);
        if target > max {
            self.emit("PID Autotune failed! Temperature too high");
            return Outcome::Done;
        }
        self.emit("PID Autotune start");
        // Marlin turns every heater off before tuning.
        self.disable_all_heaters();
        let tune = Autotune::new(target, cycles, is_bed, self.now);
        let initial = tune.initial_output();
        let last_sample = self.heater(heater).map_or(0, Heater::samples);
        if let Some(h) = self.heater_mut(heater) {
            h.target = target;
            h.forced_output = Some(initial);
        }
        self.wait_for_heatup = true;
        Outcome::Task {
            kind: TaskKind::Autotune(Tune {
                heater,
                tune,
                apply: c.flag('U'),
                last_sample,
            }),
            sync: false,
        }
    }

    fn end_autotune(&mut self, heater: HeaterId) {
        if let Some(h) = self.heater_mut(heater) {
            h.off();
        }
        self.wait_for_heatup = false;
    }

    pub(super) fn progress_autotune(&mut self, t: &mut Tune) -> bool {
        if !self.wait_for_heatup {
            // M108 interrupts the tune.
            self.end_autotune(t.heater);
            return true;
        }
        let Some(h) = self.heater(t.heater) else {
            return true;
        };
        let (samples, temp) = (h.samples(), h.temp());
        let tool = match t.heater {
            HeaterId::Hotend(i) => i,
            _ => self.motion.tool,
        };
        if self.now >= t.tune.next_report {
            t.tune.next_report = self.now + 2.0;
            let line = self.temp_line(2, tool);
            self.emit(line);
        }
        if samples == t.last_sample {
            return false;
        }
        t.last_sample = samples;
        let output = match t.tune.sample(self.now, temp) {
            TuneEvent::Continue(o) => o,
            TuneEvent::Cycle(lines, o) => {
                self.out.extend(lines);
                o
            }
            TuneEvent::Finished(lines, g) => {
                self.out.extend(lines);
                self.finish_autotune(t, g);
                return true;
            }
            TuneEvent::Failed(msg) => {
                self.emit(format!("PID Autotune{msg}"));
                self.end_autotune(t.heater);
                return true;
            }
        };
        if let Some(h) = self.heater_mut(t.heater) {
            h.forced_output = Some(output);
        }
        false
    }

    fn finish_autotune(&mut self, t: &Tune, g: Gains) {
        self.emit(
            "PID Autotune finished! Put the last Kp, Ki and Kd constants from below into Configuration.h",
        );
        let prefix = match t.heater {
            HeaterId::Bed => "bed",
            HeaterId::Chamber => "chamber",
            HeaterId::Hotend(_) => "",
        };
        self.emit(format!("#define DEFAULT_{prefix}Kp {}", f2(g.kp)));
        self.emit(format!("#define DEFAULT_{prefix}Ki {}", f2(g.ki)));
        self.emit(format!("#define DEFAULT_{prefix}Kd {}", f2(g.kd)));
        if t.apply {
            // Apply the values as printed (Marlin rounds the same way).
            let round = |v: f64| (v * 100.0).round() / 100.0;
            let gains = Gains {
                kp: round(g.kp),
                ki: round(g.ki),
                kd: round(g.kd),
            };
            match t.heater {
                HeaterId::Hotend(i) => {
                    if let Some(slot) = self.settings.hotend_pid.get_mut(i) {
                        *slot = gains;
                    }
                }
                HeaterId::Bed => self.settings.bed_pid = gains,
                HeaterId::Chamber => {}
            }
        }
        self.end_autotune(t.heater);
    }
}
