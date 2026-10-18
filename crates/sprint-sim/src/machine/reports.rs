//! Print job timer (M75/M76/M77, M31) and print statistics (M78).

use crate::format::{duration, f2, whole_secs};

/// Print job timer state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum TimerState {
    #[default]
    Stopped,
    Running {
        since: f64,
    },
    Paused,
}

/// Marlin's print job timer.
#[derive(Clone, Debug, Default)]
pub(crate) struct JobTimer {
    state: TimerState,
    accumulated: f64,
}

/// PRINTCOUNTER statistics (persist across resets like Marlin's EEPROM copy).
#[derive(Clone, Debug, Default)]
pub(crate) struct Stats {
    pub total: u32,
    pub finished: u32,
    pub print_time: f64,
    pub longest: f64,
    /// Filament extruded in mm.
    pub filament_mm: f64,
}

impl JobTimer {
    /// Start or resume. Starting from stopped counts a new print.
    pub(crate) fn start(&mut self, now: f64, stats: &mut Stats) {
        match self.state {
            TimerState::Stopped => {
                stats.total = stats.total.saturating_add(1);
                self.accumulated = 0.0;
                self.state = TimerState::Running { since: now };
            }
            TimerState::Paused => self.state = TimerState::Running { since: now },
            TimerState::Running { .. } => {}
        }
    }

    /// Pause (M76).
    pub(crate) fn pause(&mut self, now: f64) {
        if let TimerState::Running { since } = self.state {
            self.accumulated += now - since;
            self.state = TimerState::Paused;
        }
    }

    /// Elapsed job time.
    pub(crate) fn duration(&self, now: f64) -> f64 {
        match self.state {
            TimerState::Running { since } => self.accumulated + (now - since),
            _ => self.accumulated,
        }
    }

    /// Running or paused.
    pub(crate) fn is_active(&self) -> bool {
        self.state != TimerState::Stopped
    }

    fn finish(&mut self, now: f64, stats: &mut Stats) -> f64 {
        let d = self.duration(now);
        self.accumulated = d;
        self.state = TimerState::Stopped;
        stats.print_time += d;
        d
    }

    /// Stop a finished job (M77, end of print). Only completed prints
    /// count as finished and as the longest job (Marlin's `_stop(true)`).
    pub(crate) fn stop(&mut self, now: f64, stats: &mut Stats) {
        if self.is_active() {
            let d = self.finish(now, stats);
            stats.finished = stats.finished.saturating_add(1);
            stats.longest = stats.longest.max(d);
        }
    }

    /// Abort (cancelled / killed job): counts as failed.
    pub(crate) fn abort(&mut self, now: f64, stats: &mut Stats) {
        if self.is_active() {
            self.finish(now, stats);
        }
    }
}

impl Stats {
    /// M78 report lines.
    pub(crate) fn report(&self, timer: &JobTimer, now: f64) -> Vec<String> {
        let active = timer.is_active();
        let running_time = if active { timer.duration(now) } else { 0.0 };
        let failed = self
            .total
            .saturating_sub(self.finished)
            .saturating_sub(u32::from(active));
        vec![
            format!(
                "Stats: Prints: {}, Finished: {}, Failed: {}",
                self.total, self.finished, failed
            ),
            format!(
                "Stats: Total time: {}, Longest job: {}",
                duration(whole_secs(self.print_time + running_time)),
                duration(whole_secs(self.longest))
            ),
            format!("Stats: Filament used: {}m", f2(self.filament_mm / 1000.0)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_counts_prints() {
        let mut t = JobTimer::default();
        let mut s = Stats::default();
        t.start(0.0, &mut s);
        t.pause(10.0);
        t.start(20.0, &mut s);
        assert_eq!(s.total, 1);
        assert!((t.duration(25.0) - 15.0).abs() < 1e-9);
        t.stop(30.0, &mut s);
        assert_eq!(s.finished, 1);
        assert!((s.print_time - 20.0).abs() < 1e-9);
        t.start(40.0, &mut s);
        // A running job counts towards the total time but, like Marlin's
        // PrintCounter, only completed prints can be the longest job.
        let r = s.report(&t, 70.0);
        assert_eq!(r[1], "Stats: Total time: 50s, Longest job: 20s");
        t.abort(100.0, &mut s);
        assert_eq!((s.total, s.finished), (2, 1));
        assert!((s.longest - 20.0).abs() < 1e-9);
        let r = s.report(&t, 100.0);
        assert_eq!(r[0], "Stats: Prints: 2, Finished: 1, Failed: 1");
        assert_eq!(r[1], "Stats: Total time: 1m 20s, Longest job: 20s");
        assert_eq!(r[2], "Stats: Filament used: 0.00m");
        t.start(200.0, &mut s);
        t.stop(290.0, &mut s);
        assert!((s.longest - 90.0).abs() < 1e-9);
    }
}
