//! What the screen shows of the printer: a small copy of the host state,
//! taken whenever the engine reports a change, so drawing never holds the
//! state lock.

use std::collections::VecDeque;

use sprint_host::sprint_proto::{JobContext, PositionReport, TempReport};
use sprint_host::{
    AutotuneState, ConnectionStatus, HostPrompt, JobInfo, LinkStats, PrinterState,
    TEMP_HISTORY_LEN, TempSample,
};

use crate::widgets::TempPoint;

/// The parts of [`PrinterState`] the screen draws.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrinterView {
    pub status: ConnectionStatus,
    pub target: Option<String>,
    /// Firmware name from M115.
    pub firmware: Option<String>,
    /// Hotends the printer reports (at least 1).
    pub extruders: usize,
    pub temps: TempReport,
    pub position: Option<PositionReport>,
    pub job: Option<JobInfo>,
    /// Part fan 0..=255.
    pub fan: u8,
    pub feedrate_pct: f32,
    pub flow_pct: f32,
    pub busy: Option<String>,
    /// The firmware waits for the user without a prompt (M108 releases
    /// it; see [`crate::actions::waiting_for_user`]).
    pub waiting_for_user: bool,
    pub prompt: Option<HostPrompt>,
    pub halted: bool,
    /// The printer prints from its SD card (started here or at the
    /// printer).
    pub sd_printing: bool,
    pub active_tool: u8,
    pub link: LinkStats,
    pub autotune: Option<AutotuneState>,
    /// Newest firmware message (`M117`, `echo:`).
    pub message: Option<String>,
}

impl PrinterView {
    /// Copy what the screen needs.
    pub(crate) fn from_state(s: &PrinterState) -> PrinterView {
        let reported = s
            .firmware
            .as_ref()
            .and_then(|f| f.extruder_count)
            .map_or(0, usize::from);
        PrinterView {
            status: s.status.clone(),
            target: s.target.clone(),
            firmware: s.firmware.as_ref().map(|f| f.name.clone()),
            extruders: reported.max(s.temps.hotends.len()).max(1),
            temps: s.temps.clone(),
            position: s.position.clone(),
            job: s.job.clone(),
            fan: s.fan_speeds.first().copied().unwrap_or(0),
            feedrate_pct: s.feedrate_pct,
            flow_pct: s
                .flow_pct
                .get(usize::from(s.active_tool))
                .or(s.flow_pct.first())
                .copied()
                .unwrap_or(100.0),
            busy: s.busy.clone(),
            waiting_for_user: crate::actions::waiting_for_user(s),
            prompt: s.prompt.clone(),
            halted: s.halted,
            sd_printing: s.sd.printing.is_some(),
            active_tool: s.active_tool,
            link: s.link.clone(),
            autotune: s.autotune.clone(),
            message: s.firmware_messages.back().cloned(),
        }
    }

    /// A job is running (or paused).
    pub(crate) fn job_active(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.status.is_active())
    }

    /// What the job is doing, for the command-safety classifier (see
    /// [`crate::actions::job_context`]).
    pub(crate) fn job_context(&self) -> JobContext {
        crate::actions::context_of(self.job.as_ref(), self.sd_printing)
    }

    /// The handshake has completed.
    pub(crate) fn connected(&self) -> bool {
        self.status.is_connected()
    }
}

/// The temperature history as chart points, updated incrementally.
#[derive(Clone, Debug, Default)]
pub(crate) struct TempHistory {
    pub points: Vec<TempPoint>,
}

fn point(s: &TempSample) -> TempPoint {
    TempPoint {
        t: s.t,
        hotends: s.hotends.clone(),
        bed: s.bed,
        chamber: s.chamber,
    }
}

impl TempHistory {
    /// Bring the points up to date with the host's history.
    pub(crate) fn update(&mut self, history: &VecDeque<TempSample>) {
        let newest_known = self.points.last().map(|p| p.t);
        let rebuilt = match (newest_known, history.back()) {
            (_, None) => {
                self.points.clear();
                return;
            }
            (None, Some(_)) => true,
            // A new connection restarts the clock.
            (Some(known), Some(newest)) => newest.t < known,
        };
        if rebuilt {
            self.points = history.iter().map(point).collect();
        } else if let Some(known) = newest_known {
            let fresh = history.iter().rev().take_while(|s| s.t > known).count();
            let start = history.len() - fresh;
            self.points.extend(history.iter().skip(start).map(point));
        }
        let excess = self.points.len().saturating_sub(TEMP_HISTORY_LEN);
        if excess > 0 {
            self.points.drain(..excess);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t: f64) -> TempSample {
        TempSample {
            t,
            hotends: vec![(20.0 + t as f32, 200.0)],
            bed: Some((20.0, 60.0)),
            ..TempSample::default()
        }
    }

    #[test]
    fn history_grows_incrementally_and_restarts() {
        let mut history = TempHistory::default();
        let mut samples: VecDeque<TempSample> = (0..5).map(|t| sample(f64::from(t))).collect();
        history.update(&samples);
        assert_eq!(history.points.len(), 5);
        samples.push_back(sample(5.0));
        samples.push_back(sample(6.0));
        samples.pop_front();
        history.update(&samples);
        assert_eq!(history.points.len(), 7);
        assert_eq!(history.points.last().map(|p| p.t), Some(6.0));
        // A reconnect restarts the clock: the history is rebuilt.
        let restarted: VecDeque<TempSample> = (0..2).map(|t| sample(f64::from(t))).collect();
        history.update(&restarted);
        assert_eq!(history.points.len(), 2);
        history.update(&VecDeque::new());
        assert!(history.points.is_empty());
    }

    #[test]
    fn view_copies_the_state() {
        let state = PrinterState {
            status: ConnectionStatus::Connected,
            fan_speeds: vec![128],
            flow_pct: vec![95.0],
            ..PrinterState::default()
        };
        let view = PrinterView::from_state(&state);
        assert!(view.connected());
        assert_eq!(view.fan, 128);
        assert_eq!(view.flow_pct, 95.0);
        assert_eq!(view.extruders, 1);
        assert!(!view.job_active());
    }
}
