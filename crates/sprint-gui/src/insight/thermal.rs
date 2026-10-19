//! Thermal health of the heaters, from the host's temperature history.
//!
//! The host keeps about one [`TempSample`] per second. For one heater this
//! module extracts a series of [`HeaterPoint`]s and derives:
//!
//! * [`Stability`]: how steadily the heater holds its target over the last
//!   [`STABILITY_WINDOW`] seconds (mean, standard deviation, peak-to-peak,
//!   mean heater power);
//! * [`HeatUp`]: the last heat-up — time to reach the target, overshoot and
//!   settling time;
//! * [`Finding`]s: oscillation beyond ±[`OSCILLATION_LIMIT`] °C (run a PID
//!   autotune), more than [`POWER_LIMIT`] of the heater's power needed to
//!   hold the target, a steady offset or a large overshoot.
//!
//! [`analyze`] bundles everything into a [`HeaterHealth`].

use sprint_host::{Heater, TempSample};

/// Seconds of history used for the stability statistics.
pub const STABILITY_WINDOW: f64 = 60.0;
/// Oscillation amplitude (°C, half the peak-to-peak) above which a PID
/// autotune is suggested.
pub const OSCILLATION_LIMIT: f32 = 1.0;
/// Mean heater power (0..=1) above which holding the target is flagged as
/// close to the heater's limit.
pub const POWER_LIMIT: f32 = 0.9;
/// A heater within this many °C of its target counts as having reached it
/// (Marlin's `TEMP_WINDOW` / `TEMP_BED_WINDOW` default).
pub const REACHED_BAND: f32 = 1.0;
/// A heater counts as settled once it stays within this many °C of its
/// target.
pub const SETTLE_BAND: f32 = 1.0;
/// How long a heater must stay within [`SETTLE_BAND`] to count as settled
/// (seconds; Marlin's `TEMP_RESIDENCY_TIME` default).
pub const SETTLE_HOLD: f64 = 10.0;
/// Overshoot (°C) above which the heat-up is flagged.
pub const OVERSHOOT_LIMIT: f32 = 5.0;
/// Minimum covered time (seconds) before stability statistics are given.
pub const MIN_STABILITY_SPAN: f64 = 10.0;
/// A target change counts as a heat-up when it is at least this far
/// (°C) above the temperature at the time.
const HEATUP_MIN_RISE: f32 = 5.0;
/// Targets closer than this (°C) are the same target.
const SAME_TARGET: f32 = 0.05;

/// One sample of one heater.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaterPoint {
    /// Seconds since the connection was opened.
    pub t: f64,
    /// Measured temperature (°C).
    pub current: f32,
    /// Target (°C, 0 = off).
    pub target: f32,
    /// Heater power 0..=1, when the firmware reports it.
    pub power: Option<f32>,
}

/// The samples of `heater` in `history`, oldest first.
///
/// Samples without a reading for the heater and samples with non-finite
/// values are skipped. When the time stamps jump backwards (a reconnect),
/// only the samples after the last jump are kept, so every series is
/// strictly increasing in time.
pub fn heater_series<'a>(
    history: impl IntoIterator<Item = &'a TempSample>,
    heater: Heater,
) -> Vec<HeaterPoint> {
    let mut points: Vec<HeaterPoint> = Vec::new();
    for sample in history {
        let reading = match heater {
            Heater::Hotend(tool) => {
                let i = usize::from(tool);
                sample
                    .hotends
                    .get(i)
                    .map(|&(c, t)| (c, t, sample.hotend_power.get(i).copied()))
            }
            Heater::Bed => sample.bed.map(|(c, t)| (c, t, sample.bed_power)),
            Heater::Chamber => sample.chamber.map(|(c, t)| (c, t, None)),
        };
        let Some((current, target, power)) = reading else {
            continue;
        };
        if !sample.t.is_finite() || !current.is_finite() {
            continue;
        }
        let target = if target.is_finite() {
            target.max(0.0)
        } else {
            0.0
        };
        let power = power.filter(|p| p.is_finite()).map(|p| p.clamp(0.0, 1.0));
        if points.last().is_some_and(|last| sample.t <= last.t) {
            // Time went backwards: a new connection. Keep only the new run.
            if points.last().is_some_and(|last| sample.t < last.t) {
                points.clear();
            } else {
                continue;
            }
        }
        points.push(HeaterPoint {
            t: sample.t,
            current,
            target,
            power,
        });
    }
    points
}

/// How steadily a heater holds its target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stability {
    /// The target held (°C).
    pub target: f32,
    /// Seconds covered by the statistics (at most [`STABILITY_WINDOW`];
    /// less right after reaching the target).
    pub span: f64,
    /// Number of samples used.
    pub samples: usize,
    /// Mean temperature (°C).
    pub mean: f32,
    /// Population standard deviation (°C).
    pub std_dev: f32,
    /// Lowest temperature (°C).
    pub min: f32,
    /// Highest temperature (°C).
    pub max: f32,
    /// `max - min` (°C).
    pub peak_to_peak: f32,
    /// `mean - target` (°C).
    pub mean_error: f32,
    /// Largest absolute deviation from the target (°C).
    pub max_error: f32,
    /// Mean heater power 0..=1 while holding, when reported.
    pub mean_power: Option<f32>,
}

impl Stability {
    /// Oscillation amplitude: half the peak-to-peak (°C).
    pub fn amplitude(&self) -> f32 {
        self.peak_to_peak / 2.0
    }

    /// The oscillation exceeds ±[`OSCILLATION_LIMIT`].
    pub fn is_oscillating(&self) -> bool {
        self.amplitude() > OSCILLATION_LIMIT
    }
}

/// Index of the first point of the run of points (ending with the last
/// one) that share the last point's target.
fn current_target_start(points: &[HeaterPoint]) -> Option<usize> {
    let last = points.last()?;
    let mut start = points.len() - 1;
    while start > 0 && (points[start - 1].target - last.target).abs() < SAME_TARGET {
        start -= 1;
    }
    Some(start)
}

/// Stability over the trailing `window` seconds, at the current target.
///
/// Only samples after the heater first reached its current target (within
/// [`REACHED_BAND`]) count, so a heat-up does not spoil the statistics.
/// `None` while the heater is off, has not reached its target yet, or when
/// less than [`MIN_STABILITY_SPAN`] seconds (or fewer than 5 samples) are
/// available.
pub fn stability(points: &[HeaterPoint], window: f64) -> Option<Stability> {
    let last = points.last()?;
    let target = last.target;
    if target <= 0.0 {
        return None;
    }
    let start = current_target_start(points)?;
    let segment = &points[start..];
    let reached = segment
        .iter()
        .position(|p| (p.current - target).abs() <= REACHED_BAND)?;
    let window = if window.is_finite() && window > 0.0 {
        window
    } else {
        STABILITY_WINDOW
    };
    let from = last.t - window;
    let used: Vec<&HeaterPoint> = segment[reached..].iter().filter(|p| p.t >= from).collect();
    let first = used.first()?;
    let span = last.t - first.t;
    if used.len() < 5 || span < MIN_STABILITY_SPAN.min(window) {
        return None;
    }
    let n = used.len() as f64;
    let mean = used.iter().map(|p| f64::from(p.current)).sum::<f64>() / n;
    let var = used
        .iter()
        .map(|p| (f64::from(p.current) - mean).powi(2))
        .sum::<f64>()
        / n;
    let min = used.iter().map(|p| p.current).fold(f32::INFINITY, f32::min);
    let max = used
        .iter()
        .map(|p| p.current)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_error = used
        .iter()
        .map(|p| (p.current - target).abs())
        .fold(0.0, f32::max);
    let powers: Vec<f32> = used.iter().filter_map(|p| p.power).collect();
    let mean_power = if powers.is_empty() {
        None
    } else {
        let mean = powers.iter().sum::<f32>() / powers.len() as f32;
        // Holding a temperature well above ambient with zero power is
        // impossible: the firmware does not report power after all.
        if mean <= 0.0 && target > 40.0 {
            None
        } else {
            Some(mean)
        }
    };
    Some(Stability {
        target,
        span,
        samples: used.len(),
        mean: mean as f32,
        std_dev: var.sqrt() as f32,
        min,
        max,
        peak_to_peak: max - min,
        mean_error: mean as f32 - target,
        max_error,
        mean_power,
    })
}

/// The last heat-up of a heater.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeatUp {
    /// When the target was set (seconds since connecting; the first
    /// sample when the history starts mid-heat-up).
    pub start_t: f64,
    /// Temperature at the start (°C).
    pub start_temp: f32,
    /// The target (°C).
    pub target: f32,
    /// Seconds from the start until the heater came within
    /// [`REACHED_BAND`] of the target; `None` while still heating (or when
    /// the target changed before it was reached).
    pub time_to_target: Option<f64>,
    /// Highest temperature after reaching the target (°C).
    pub peak: f32,
    /// How far the peak exceeded the target (°C, 0 when it did not).
    pub overshoot: f32,
    /// Seconds from the start until the heater stayed within
    /// [`SETTLE_BAND`] for good (at least [`SETTLE_HOLD`] seconds and until
    /// the end of the data or of this target); `None` while not settled.
    pub settling_time: Option<f64>,
    /// The target is still the current one.
    pub ongoing: bool,
    /// The history begins after the heat-up started, so the times are
    /// lower bounds.
    pub partial: bool,
}

impl HeatUp {
    /// Average heating rate until the target was reached (°C/s).
    pub fn heating_rate(&self) -> Option<f32> {
        let t = self.time_to_target?;
        (t > 0.0).then(|| (self.target - self.start_temp) / t as f32)
    }
}

/// The last heat-up in `points`: the last time the target was raised at
/// least 5 °C above the temperature at that moment (or the start of the
/// history, when it begins below a set target).
pub fn last_heatup(points: &[HeaterPoint]) -> Option<HeatUp> {
    // Find the start index of the last heat-up event.
    let mut event: Option<(usize, bool)> = None;
    for (i, p) in points.iter().enumerate() {
        let is_start = if i == 0 {
            p.target > 0.0 && p.target - p.current >= HEATUP_MIN_RISE
        } else {
            let prev = &points[i - 1];
            (p.target - prev.target).abs() >= SAME_TARGET
                && p.target > 0.0
                && p.target - prev.current.min(p.current) >= HEATUP_MIN_RISE
        };
        if is_start {
            event = Some((i, i == 0));
        }
    }
    let (start, partial) = event?;
    let target = points[start].target;
    let end = points[start..]
        .iter()
        .position(|p| (p.target - target).abs() >= SAME_TARGET)
        .map_or(points.len(), |n| start + n);
    let segment = &points[start..end];
    let ongoing = end == points.len();
    // The temperature when the target was set: the previous sample's
    // reading is the best estimate when there is one.
    let start_temp = if start > 0 {
        points[start - 1].current
    } else {
        points[start].current
    };
    // The target changed somewhere between the two samples; the later one
    // is when the host saw it.
    let start_t = points[start].t;
    let reached = segment
        .iter()
        .position(|p| p.current >= target - REACHED_BAND);
    let time_to_target = reached.map(|i| segment[i].t - start_t);
    let (peak, overshoot) = match reached {
        Some(i) => {
            let peak = segment[i..]
                .iter()
                .map(|p| p.current)
                .fold(f32::NEG_INFINITY, f32::max);
            (peak, (peak - target).max(0.0))
        }
        None => (
            segment
                .iter()
                .map(|p| p.current)
                .fold(f32::NEG_INFINITY, f32::max),
            0.0,
        ),
    };
    let settling_time = reached.and_then(|i| {
        let after = &segment[i..];
        // The first point after the last excursion outside the band.
        let last_out = after
            .iter()
            .rposition(|p| (p.current - target).abs() > SETTLE_BAND);
        let settled_from = match last_out {
            Some(j) if j + 1 < after.len() => j + 1,
            Some(_) => return None,
            None => 0,
        };
        let settled_t = after[settled_from].t;
        let held = after.last().map_or(0.0, |p| p.t) - settled_t;
        (held >= SETTLE_HOLD).then_some(settled_t - start_t)
    });
    Some(HeatUp {
        start_t,
        start_temp,
        target,
        time_to_target,
        peak,
        overshoot,
        settling_time,
        ongoing,
        partial,
    })
}

/// What a heater is doing right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThermalState {
    /// No target set (cooling down or cold).
    Off,
    /// Below its target, heating.
    Heating {
        /// Degrees still to go.
        remaining: f32,
    },
    /// Above its target by more than the band, cooling towards it.
    Cooling {
        /// Degrees above the target.
        excess: f32,
    },
    /// At the target, but not long enough for statistics yet.
    Settling,
    /// Holding the target within ±[`OSCILLATION_LIMIT`].
    Stable,
    /// Holding the target, but oscillating beyond ±[`OSCILLATION_LIMIT`].
    Unstable,
    /// No data for this heater.
    Unknown,
}

impl ThermalState {
    /// Short label: "Off", "Heating", "Stable", ...
    pub fn label(&self) -> &'static str {
        match self {
            ThermalState::Off => "Off",
            ThermalState::Heating { .. } => "Heating",
            ThermalState::Cooling { .. } => "Cooling",
            ThermalState::Settling => "Settling",
            ThermalState::Stable => "Stable",
            ThermalState::Unstable => "Oscillating",
            ThermalState::Unknown => "No data",
        }
    }
}

/// A notable thermal behaviour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Finding {
    /// The temperature swings more than ±[`OSCILLATION_LIMIT`] around the
    /// target: the PID gains need tuning.
    Oscillating {
        /// Half the peak-to-peak (°C).
        amplitude: f32,
    },
    /// Holding the target takes more than [`POWER_LIMIT`] of the heater's
    /// power: an underpowered heater, a draught or missing insulation.
    PowerLimited {
        /// Mean power 0..=1.
        power: f32,
    },
    /// Stable, but consistently off target (an integral term too weak).
    SteadyOffset {
        /// `mean - target` (°C).
        offset: f32,
    },
    /// The last heat-up overshot by more than [`OVERSHOOT_LIMIT`] °C.
    Overshoot {
        /// °C above the target.
        overshoot: f32,
    },
}

impl Finding {
    /// Whether a PID autotune is the suggested remedy.
    pub fn suggests_pid_tune(&self) -> bool {
        matches!(
            self,
            Finding::Oscillating { .. } | Finding::SteadyOffset { .. } | Finding::Overshoot { .. }
        )
    }

    /// A short headline.
    pub fn title(&self) -> String {
        match self {
            Finding::Oscillating { amplitude } => {
                format!("Oscillating ±{amplitude:.1} °C")
            }
            Finding::PowerLimited { power } => {
                format!("Needs {:.0}% power to hold", power * 100.0)
            }
            Finding::SteadyOffset { offset } => {
                format!("Holding {offset:+.1} °C off target")
            }
            Finding::Overshoot { overshoot } => {
                format!("Overshot by {overshoot:.1} °C")
            }
        }
    }

    /// A sentence explaining what it means and what to do.
    pub fn advice(&self) -> &'static str {
        match self {
            Finding::Oscillating { .. } => {
                "The temperature swings more than ±1 °C around the target. Run a PID autotune to find better gains."
            }
            Finding::PowerLimited { .. } => {
                "Holding the target takes more than 90% of the heater's power. Check the heater, the supply voltage, insulation (silicone sock, bed underside) and draughts."
            }
            Finding::SteadyOffset { .. } => {
                "The heater settles away from its target. A PID autotune usually fixes a weak integral term."
            }
            Finding::Overshoot { .. } => {
                "The last heat-up overshot the target noticeably. A PID autotune reduces overshoot."
            }
        }
    }
}

/// Everything known about one heater's thermal behaviour.
#[derive(Clone, Debug, PartialEq)]
pub struct HeaterHealth {
    /// The heater.
    pub heater: Heater,
    /// Latest temperature (°C; NaN without data).
    pub current: f32,
    /// Latest target (°C, 0 = off).
    pub target: f32,
    /// What it is doing.
    pub state: ThermalState,
    /// Statistics at the current target.
    pub stability: Option<Stability>,
    /// The last heat-up.
    pub heatup: Option<HeatUp>,
    /// Findings, most important first.
    pub findings: Vec<Finding>,
    /// Temperatures of the last [`STABILITY_WINDOW`] seconds, oldest first
    /// (for a sparkline).
    pub recent: Vec<f32>,
}

impl HeaterHealth {
    /// Whether any finding suggests a PID autotune.
    pub fn suggests_pid_tune(&self) -> bool {
        self.findings.iter().any(Finding::suggests_pid_tune)
    }
}

/// Analyse one heater's history.
pub fn analyze<'a>(
    history: impl IntoIterator<Item = &'a TempSample>,
    heater: Heater,
) -> HeaterHealth {
    analyze_points(&heater_series(history, heater), heater)
}

/// [`analyze`] on an already extracted series.
pub fn analyze_points(points: &[HeaterPoint], heater: Heater) -> HeaterHealth {
    let Some(last) = points.last() else {
        return HeaterHealth {
            heater,
            current: f32::NAN,
            target: 0.0,
            state: ThermalState::Unknown,
            stability: None,
            heatup: None,
            findings: Vec::new(),
            recent: Vec::new(),
        };
    };
    let stability = stability(points, STABILITY_WINDOW);
    let heatup = last_heatup(points);
    let state = if last.target <= 0.0 {
        ThermalState::Off
    } else if let Some(s) = &stability {
        if s.is_oscillating() {
            ThermalState::Unstable
        } else {
            ThermalState::Stable
        }
    } else if last.current < last.target - REACHED_BAND
        && current_target_start(points).is_some_and(|start| {
            !points[start..]
                .iter()
                .any(|p| (p.current - last.target).abs() <= REACHED_BAND)
        })
    {
        ThermalState::Heating {
            remaining: last.target - last.current,
        }
    } else if last.current > last.target + REACHED_BAND
        && current_target_start(points).is_some_and(|start| {
            !points[start..]
                .iter()
                .any(|p| (p.current - last.target).abs() <= REACHED_BAND)
        })
    {
        ThermalState::Cooling {
            excess: last.current - last.target,
        }
    } else {
        ThermalState::Settling
    };
    let mut findings = Vec::new();
    if let Some(s) = &stability {
        if s.is_oscillating() {
            findings.push(Finding::Oscillating {
                amplitude: s.amplitude(),
            });
        } else if s.mean_error.abs() > OSCILLATION_LIMIT {
            findings.push(Finding::SteadyOffset {
                offset: s.mean_error,
            });
        }
        if let Some(power) = s.mean_power
            && power > POWER_LIMIT
        {
            findings.push(Finding::PowerLimited { power });
        }
    }
    if let Some(h) = &heatup
        && h.ongoing
        && h.overshoot > OVERSHOOT_LIMIT
    {
        findings.push(Finding::Overshoot {
            overshoot: h.overshoot,
        });
    }
    let from = last.t - STABILITY_WINDOW;
    let recent = points
        .iter()
        .filter(|p| p.t >= from)
        .map(|p| p.current)
        .collect();
    HeaterHealth {
        heater,
        current: last.current,
        target: last.target,
        state,
        stability,
        heatup,
        findings,
        recent,
    }
}

/// The heaters present in `history`, in display order: hotends, bed,
/// chamber.
pub fn heaters_in<'a>(history: impl IntoIterator<Item = &'a TempSample>) -> Vec<Heater> {
    let (mut hotends, mut bed, mut chamber) = (0usize, false, false);
    for s in history {
        hotends = hotends.max(s.hotends.len());
        bed |= s.bed.is_some();
        chamber |= s.chamber.is_some();
    }
    let mut out: Vec<Heater> = (0..hotends.min(usize::from(u8::MAX)))
        .map(|i| Heater::Hotend(i as u8))
        .collect();
    if bed {
        out.push(Heater::Bed);
    }
    if chamber {
        out.push(Heater::Chamber);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(t: f64, current: f32, target: f32) -> HeaterPoint {
        HeaterPoint {
            t,
            current,
            target,
            power: None,
        }
    }

    /// A first-order heat-up from `from` to `target` starting at `t0`,
    /// sampled every second for `secs`, then an oscillation of `amp`
    /// around the target with a 20 s period.
    fn profile(
        from: f32,
        target: f32,
        heat_secs: usize,
        hold_secs: usize,
        amp: f32,
    ) -> Vec<HeaterPoint> {
        let mut pts = vec![point(0.0, from, 0.0)];
        for i in 1..=heat_secs {
            let k = i as f32 / heat_secs as f32;
            pts.push(point(i as f64, from + (target - from) * k, target));
        }
        for i in 1..=hold_secs {
            let phase = i as f32 / 20.0 * std::f32::consts::TAU;
            pts.push(point(
                (heat_secs + i) as f64,
                target + amp * phase.sin(),
                target,
            ));
        }
        pts
    }

    #[test]
    fn series_skips_missing_and_restarts_on_reconnect() {
        let mut history = Vec::new();
        for i in 0..5 {
            history.push(TempSample {
                t: f64::from(i),
                hotends: vec![(20.0 + i as f32, 200.0)],
                bed: if i == 2 { None } else { Some((60.0, 60.0)) },
                hotend_power: vec![0.5],
                bed_power: Some(0.3),
                ..TempSample::default()
            });
        }
        // Reconnect: time restarts.
        history.push(TempSample {
            t: 0.5,
            hotends: vec![(30.0, 0.0)],
            ..TempSample::default()
        });
        let hotend = heater_series(&history, Heater::Hotend(0));
        assert_eq!(hotend.len(), 1);
        assert_eq!(hotend[0].current, 30.0);
        let bed = heater_series(&history[..5], Heater::Bed);
        assert_eq!(bed.len(), 4);
        assert_eq!(bed[0].power, Some(0.3));
        assert!(heater_series(&history, Heater::Chamber).is_empty());
        assert!(heater_series(&history, Heater::Hotend(3)).is_empty());
    }

    #[test]
    fn series_sanitizes_values() {
        let history = vec![
            TempSample {
                t: 0.0,
                hotends: vec![(f32::NAN, 200.0)],
                ..TempSample::default()
            },
            TempSample {
                t: 1.0,
                hotends: vec![(100.0, f32::NAN)],
                hotend_power: vec![7.0],
                ..TempSample::default()
            },
            TempSample {
                t: f64::NAN,
                hotends: vec![(100.0, 0.0)],
                ..TempSample::default()
            },
            // Duplicate time stamp: ignored.
            TempSample {
                t: 1.0,
                hotends: vec![(101.0, 0.0)],
                ..TempSample::default()
            },
        ];
        let s = heater_series(&history, Heater::Hotend(0));
        assert_eq!(
            s,
            vec![HeaterPoint {
                t: 1.0,
                current: 100.0,
                target: 0.0,
                power: Some(1.0)
            }]
        );
    }

    #[test]
    fn stable_heater_statistics() {
        let pts = profile(25.0, 210.0, 60, 120, 0.3);
        let s = stability(&pts, STABILITY_WINDOW).expect("stable");
        assert!((s.mean - 210.0).abs() < 0.1, "{s:?}");
        assert!((s.peak_to_peak - 0.6).abs() < 0.05, "{s:?}");
        assert!(s.std_dev > 0.15 && s.std_dev < 0.25, "{s:?}");
        assert!((s.span - 60.0).abs() < 1e-9);
        assert_eq!(s.samples, 61);
        assert!(!s.is_oscillating());
        let h = analyze_points(&pts, Heater::Hotend(0));
        assert_eq!(h.state, ThermalState::Stable);
        assert!(h.findings.is_empty());
        assert_eq!(h.recent.len(), 61);
    }

    #[test]
    fn oscillation_suggests_pid_tuning() {
        let pts = profile(25.0, 210.0, 60, 120, 2.5);
        let h = analyze_points(&pts, Heater::Hotend(0));
        assert_eq!(h.state, ThermalState::Unstable);
        match h.findings.first() {
            Some(Finding::Oscillating { amplitude }) => assert!((amplitude - 2.5).abs() < 0.1),
            other => panic!("{other:?}"),
        }
        assert!(h.suggests_pid_tune());
    }

    #[test]
    fn heating_up_has_no_stability_yet() {
        let mut pts = profile(25.0, 210.0, 60, 0, 0.0);
        pts.truncate(30);
        assert!(stability(&pts, STABILITY_WINDOW).is_none());
        let h = analyze_points(&pts, Heater::Hotend(0));
        assert!(matches!(h.state, ThermalState::Heating { remaining } if remaining > 80.0));
        let up = h.heatup.expect("heat-up");
        assert_eq!(up.time_to_target, None);
        assert!(up.ongoing);
        assert_eq!(up.overshoot, 0.0);
    }

    #[test]
    fn short_hold_is_settling() {
        let pts = profile(25.0, 60.0, 30, 5, 0.1);
        assert!(stability(&pts, STABILITY_WINDOW).is_none());
        assert_eq!(
            analyze_points(&pts, Heater::Bed).state,
            ThermalState::Settling
        );
    }

    #[test]
    fn off_and_empty() {
        let h = analyze_points(&[], Heater::Bed);
        assert_eq!(h.state, ThermalState::Unknown);
        assert!(h.current.is_nan());
        let pts: Vec<_> = (0..30).map(|i| point(f64::from(i), 25.0, 0.0)).collect();
        let h = analyze_points(&pts, Heater::Bed);
        assert_eq!(h.state, ThermalState::Off);
        assert!(h.heatup.is_none());
        assert!(h.stability.is_none());
    }

    #[test]
    fn cooling_towards_a_lower_target() {
        let mut pts: Vec<_> = (0..20).map(|i| point(f64::from(i), 210.0, 210.0)).collect();
        for i in 20..40 {
            pts.push(point(f64::from(i), 210.0 - (i - 20) as f32, 180.0));
        }
        let h = analyze_points(&pts, Heater::Hotend(0));
        assert!(
            matches!(h.state, ThermalState::Cooling { excess } if excess > 5.0),
            "{h:?}"
        );
    }

    #[test]
    fn heatup_measures_time_overshoot_and_settling() {
        // Heat from 25 to 200 in 40 s, overshoot to 206, then settle.
        let mut pts = vec![point(0.0, 25.0, 0.0), point(1.0, 25.0, 0.0)];
        for i in 2..=41 {
            let k = (i - 1) as f32 / 40.0;
            pts.push(point(f64::from(i), 25.0 + 175.0 * k, 200.0));
        }
        // Overshoot: 41 s at 200, peak 206 at 44 s, back within the band at 52 s.
        let tail = [
            202.0, 204.0, 206.0, 205.0, 204.0, 203.0, 202.0, 201.5, 201.1, 200.8, 200.5,
        ];
        for (k, v) in tail.iter().enumerate() {
            pts.push(point(42.0 + k as f64, *v, 200.0));
        }
        for k in 0..30 {
            pts.push(point(53.0 + f64::from(k), 200.2, 200.0));
        }
        let up = last_heatup(&pts).expect("heat-up");
        assert_eq!(up.start_t, 2.0);
        assert_eq!(up.start_temp, 25.0);
        assert_eq!(up.target, 200.0);
        // Within 1 °C of 200 at 199.0: i = 41 -> 39 s after the start.
        let ttt = up.time_to_target.expect("reached");
        assert!((ttt - 39.0).abs() < 1.01, "{ttt}");
        assert!((up.overshoot - 6.0).abs() < 1e-4);
        assert_eq!(up.peak, 206.0);
        // Last sample outside ±1: 201.1 at t = 50 -> settled from t = 51.
        let settle = up.settling_time.expect("settled");
        assert!((settle - 49.0).abs() < 1e-9, "{settle}");
        assert!(up.ongoing && !up.partial);
        assert!(up.heating_rate().is_some_and(|r| r > 4.0 && r < 5.0));
        let h = analyze_points(&pts, Heater::Hotend(0));
        assert!(
            h.findings
                .iter()
                .any(|f| matches!(f, Finding::Overshoot { .. }))
        );
    }

    #[test]
    fn heatup_not_settled_while_still_swinging() {
        let pts = profile(25.0, 210.0, 60, 120, 2.5);
        let up = last_heatup(&pts).expect("heat-up");
        assert!(up.time_to_target.is_some());
        assert_eq!(up.settling_time, None);
    }

    #[test]
    fn heatup_takes_the_last_raise_and_ignores_lowering() {
        let mut pts = profile(25.0, 200.0, 30, 30, 0.0);
        let t0 = pts.len() as f64;
        // Raise again to 240 (a heat-up), then lower to 230 (not one).
        for i in 0..20 {
            pts.push(point(t0 + f64::from(i), 200.0 + 2.0 * i as f32, 240.0));
        }
        let t1 = t0 + 20.0;
        for i in 0..20 {
            pts.push(point(t1 + f64::from(i), 236.0 - 0.3 * i as f32, 230.0));
        }
        let up = last_heatup(&pts).expect("heat-up");
        assert_eq!(up.target, 240.0);
        assert_eq!(up.start_t, t0);
        assert!(!up.ongoing);
        // The 240 target was abandoned before it was reached.
        assert_eq!(up.time_to_target, None);
    }

    #[test]
    fn heatup_from_history_start_is_partial() {
        let pts: Vec<_> = (0..=10)
            .map(|i| point(f64::from(i), 100.0 + 10.0 * i as f32, 200.0))
            .collect();
        let up = last_heatup(&pts).expect("heat-up");
        assert!(up.partial);
        assert_eq!(up.start_temp, 100.0);
        assert_eq!(up.time_to_target, Some(10.0));
    }

    #[test]
    fn small_target_changes_are_not_heatups() {
        let mut pts: Vec<_> = (0..10).map(|i| point(f64::from(i), 210.0, 210.0)).collect();
        pts.extend((10..20).map(|i| point(f64::from(i), 210.0, 212.0)));
        assert_eq!(last_heatup(&pts), None);
    }

    #[test]
    fn power_limit_and_unknown_power() {
        let mut pts = profile(25.0, 100.0, 30, 70, 0.2);
        for p in &mut pts {
            p.power = Some(0.95);
        }
        let h = analyze_points(&pts, Heater::Bed);
        assert!(
            h.findings.iter().any(
                |f| matches!(f, Finding::PowerLimited { power } if (power - 0.95).abs() < 1e-6)
            ),
            "{:?}",
            h.findings
        );
        // Zero power while holding 100 °C means "not reported".
        for p in &mut pts {
            p.power = Some(0.0);
        }
        let s = stability(&pts, STABILITY_WINDOW).expect("stable");
        assert_eq!(s.mean_power, None);
    }

    #[test]
    fn steady_offset_is_reported() {
        let pts = profile(25.0, 200.0, 30, 0, 0.0)
            .into_iter()
            .chain((0..70).map(|i| point(31.0 + f64::from(i), 198.5, 200.0)))
            .collect::<Vec<_>>();
        let h = analyze_points(&pts, Heater::Hotend(0));
        assert!(
            h.findings.iter().any(
                |f| matches!(f, Finding::SteadyOffset { offset } if (offset + 1.5).abs() < 0.1)
            ),
            "{:?}",
            h.findings
        );
        assert!(h.suggests_pid_tune());
    }

    #[test]
    fn finding_texts() {
        let f = Finding::Oscillating { amplitude: 1.84 };
        assert_eq!(f.title(), "Oscillating ±1.8 °C");
        assert!(f.advice().contains("PID"));
        assert_eq!(
            Finding::PowerLimited { power: 0.934 }.title(),
            "Needs 93% power to hold"
        );
        assert!(!Finding::PowerLimited { power: 1.0 }.suggests_pid_tune());
        assert_eq!(ThermalState::Unstable.label(), "Oscillating");
    }

    #[test]
    fn heaters_listed_in_order() {
        let history = vec![
            TempSample {
                hotends: vec![(20.0, 0.0)],
                bed: Some((20.0, 0.0)),
                ..TempSample::default()
            },
            TempSample {
                t: 1.0,
                hotends: vec![(20.0, 0.0), (21.0, 0.0)],
                chamber: Some((20.0, 0.0)),
                ..TempSample::default()
            },
        ];
        assert_eq!(
            heaters_in(&history),
            vec![
                Heater::Hotend(0),
                Heater::Hotend(1),
                Heater::Bed,
                Heater::Chamber
            ]
        );
        assert!(heaters_in(&[]).is_empty());
    }
}
