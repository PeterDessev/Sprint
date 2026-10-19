//! Pure data behind the temperature graph: heater channels and their
//! samples from the history, line runs, window statistics, stability,
//! view ranges, time labels and CSV export.

use std::collections::VecDeque;
use std::fmt::Write as _;

use sprint_host::TempSample;

/// The most hotends graphed (guards against absurd reports).
pub(crate) const MAX_HOTENDS: usize = 8;
/// A line bridges gaps up to this many seconds; longer ones (lost reports,
/// a reconnect) break it.
pub(crate) const MAX_GAP: f64 = 10.0;
/// "Stable" means within this many °C of the target ...
pub(crate) const STABLE_BAND: f32 = 0.5;
/// ... for at least this many seconds.
pub(crate) const STABLE_SECS: f64 = 30.0;
/// Readings arrive about once a second: the stability window may start
/// this much after its nominal start.
const COVERAGE_SLACK: f64 = 2.5;

/// A heater recorded in the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Channel {
    /// Hotend by tool index.
    Hotend(usize),
    /// Heated bed.
    Bed,
    /// Heated chamber.
    Chamber,
}

impl Channel {
    /// Display name ("Hotend", "Hotend T1", "Bed", "Chamber").
    pub(crate) fn name(self, hotends: usize) -> String {
        match self {
            Channel::Hotend(0) if hotends <= 1 => "Hotend".to_owned(),
            Channel::Hotend(i) => format!("Hotend T{i}"),
            Channel::Bed => "Bed".to_owned(),
            Channel::Chamber => "Chamber".to_owned(),
        }
    }

    /// Column prefix in the CSV ("T0", "bed", "chamber").
    fn key(self) -> String {
        match self {
            Channel::Hotend(i) => format!("T{i}"),
            Channel::Bed => "bed".to_owned(),
            Channel::Chamber => "chamber".to_owned(),
        }
    }

    /// (current, target) at a sample, if the heater was reported.
    pub(crate) fn reading(self, s: &TempSample) -> Option<(f32, f32)> {
        match self {
            Channel::Hotend(i) => s.hotends.get(i).copied(),
            Channel::Bed => s.bed,
            Channel::Chamber => s.chamber,
        }
    }

    /// Heater power 0..=1 at a sample, if reported (the chamber has none).
    pub(crate) fn power(self, s: &TempSample) -> Option<f32> {
        match self {
            Channel::Hotend(i) => s.hotend_power.get(i).copied(),
            Channel::Bed => s.bed_power,
            Channel::Chamber => None,
        }
        .filter(|p| p.is_finite())
    }
}

/// Every heater that appears anywhere in the history, in display order.
pub(crate) fn channels(history: &VecDeque<TempSample>) -> Vec<Channel> {
    let hotends = history
        .iter()
        .map(|s| s.hotends.len())
        .max()
        .unwrap_or(0)
        .min(MAX_HOTENDS);
    let mut list: Vec<Channel> = (0..hotends).map(Channel::Hotend).collect();
    if history.iter().any(|s| s.bed.is_some()) {
        list.push(Channel::Bed);
    }
    if history.iter().any(|s| s.chamber.is_some()) {
        list.push(Channel::Chamber);
    }
    list
}

/// One reading of one heater.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Point {
    /// Seconds since connecting.
    pub t: f64,
    /// °C (may be NaN for a missing reading).
    pub current: f32,
    /// °C, 0 = off.
    pub target: f32,
    /// 0..=1.
    pub power: Option<f32>,
}

/// The readings of `channel`, oldest first (samples without it or with a
/// non-finite time are skipped).
pub(crate) fn points(history: &VecDeque<TempSample>, channel: Channel) -> Vec<Point> {
    history
        .iter()
        .filter(|s| s.t.is_finite())
        .filter_map(|s| {
            channel.reading(s).map(|(current, target)| Point {
                t: s.t,
                current,
                target,
                power: channel.power(s),
            })
        })
        .collect()
}

/// Polyline runs of `value` over `points`: broken where the value is
/// missing or non-finite, and across gaps longer than [`MAX_GAP`].
pub(crate) fn runs(points: &[Point], value: impl Fn(&Point) -> Option<f64>) -> Vec<Vec<[f64; 2]>> {
    let mut runs: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();
    for p in points {
        match value(p).filter(|v| v.is_finite()) {
            Some(v) => {
                if current.last().is_some_and(|last| p.t - last[0] > MAX_GAP) {
                    runs.push(std::mem::take(&mut current));
                }
                current.push([p.t, v]);
            }
            None => {
                if !current.is_empty() {
                    runs.push(std::mem::take(&mut current));
                }
            }
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// The current temperature of a point (finite only).
pub(crate) fn current_value(p: &Point) -> Option<f64> {
    Some(f64::from(p.current)).filter(|v| v.is_finite())
}

/// The target of a point while the heater is on.
pub(crate) fn target_value(p: &Point) -> Option<f64> {
    (p.target.is_finite() && p.target > 0.0).then_some(f64::from(p.target))
}

/// The power of a point in percent.
pub(crate) fn power_value(p: &Point) -> Option<f64> {
    p.power.map(|w| f64::from(w.clamp(0.0, 1.0)) * 100.0)
}

/// Statistics of the readings inside a time window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stats {
    pub count: usize,
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    /// Population standard deviation.
    pub std_dev: f32,
}

/// Min / max / mean / standard deviation of the finite readings with
/// `from <= t <= to`; `None` when there are none.
pub(crate) fn window_stats(points: &[Point], from: f64, to: f64) -> Option<Stats> {
    let (from, to) = if from <= to { (from, to) } else { (to, from) };
    let mut count = 0usize;
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut sum, mut sum_sq) = (0.0f64, 0.0f64);
    for p in points.iter().filter(|p| p.t >= from && p.t <= to) {
        let v = f64::from(p.current);
        if !v.is_finite() {
            continue;
        }
        count += 1;
        min = min.min(v);
        max = max.max(v);
        sum += v;
        sum_sq += v * v;
    }
    if count == 0 {
        return None;
    }
    let n = count as f64;
    let mean = sum / n;
    let variance = (sum_sq / n - mean * mean).max(0.0);
    Some(Stats {
        count,
        min: min as f32,
        max: max as f32,
        mean: mean as f32,
        std_dev: variance.sqrt() as f32,
    })
}

/// Whether the heater has held its (non-zero) target within ±`band` °C
/// for the last `span` seconds of readings. The history must cover the
/// whole span.
pub(crate) fn is_stable(points: &[Point], span: f64, band: f32) -> bool {
    let Some(last) = points.last() else {
        return false;
    };
    let since = last.t - span;
    let start = points.partition_point(|p| p.t < since);
    let window = &points[start..];
    let covered = window
        .first()
        .is_some_and(|p| p.t <= since + COVERAGE_SLACK);
    covered
        && window.len() >= 2
        && window.iter().all(|p| {
            p.target.is_finite()
                && p.target > 0.0
                && p.current.is_finite()
                && (p.current - p.target).abs() <= band
        })
}

/// The time range shown while following: the last `window` seconds (from
/// the first sample while the history is shorter), or everything (at least
/// a minute) for `None`.
pub(crate) fn follow_range(window: Option<f64>, first: f64, last: f64) -> (f64, f64) {
    let (first, last) = if first.is_finite() && last.is_finite() && first <= last {
        (first, last)
    } else {
        (0.0, 0.0)
    };
    match window.filter(|w| w.is_finite() && *w > 0.0) {
        Some(w) => {
            let end = last.max(first + w);
            (end - w, end)
        }
        None => (first, last.max(first + 60.0)),
    }
}

/// A temperature range covering the readings and set targets of `series`
/// between `from` and `to`, padded and rounded to 5 °C so the axis does not
/// jitter; at least 20 °C tall and never below 0 for non-negative data.
pub(crate) fn y_range<'a>(
    series: impl IntoIterator<Item = &'a [Point]>,
    from: f64,
    to: f64,
) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for points in series {
        for p in points.iter().filter(|p| p.t >= from && p.t <= to) {
            for v in [current_value(p), target_value(p)].into_iter().flatten() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return (0.0, 100.0);
    }
    let non_negative = lo >= 0.0;
    let pad = ((hi - lo) * 0.08).max(4.0);
    let (mut lo, mut hi) = (lo - pad, hi + pad);
    if hi - lo < 20.0 {
        let mid = (hi + lo) / 2.0;
        lo = mid - 10.0;
        hi = mid + 10.0;
    }
    if non_negative && lo < 0.0 {
        hi -= lo;
        lo = 0.0;
    }
    ((lo / 5.0).floor() * 5.0, (hi / 5.0).ceil() * 5.0)
}

/// Seconds as a clock: "4:05", "1:02:03" from an hour, "−0:10" before
/// zero, "—" when not finite.
pub(crate) fn format_clock(secs: f64) -> String {
    if !secs.is_finite() {
        return "—".to_owned();
    }
    let sign = if secs < -0.5 { "\u{2212}" } else { "" };
    let total = secs.abs().round().min(1e9) as u64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{sign}{h}:{m:02}:{s:02}")
    } else {
        format!("{sign}{m}:{s:02}")
    }
}

/// A duration in words: "45 s", "5 min", "1 h 05 min".
pub(crate) fn format_span(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "—".to_owned();
    }
    let total = secs.round().min(1e9) as u64;
    if total < 60 {
        format!("{total} s")
    } else if total < 3600 {
        format!("{} min", (total + 30) / 60)
    } else {
        let minutes = (total + 30) / 60;
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    }
}

/// Three nested grid step sizes (each a multiple of the previous): the
/// first ladder entry whose minor step is at least `min_step`.
fn nested_steps(min_step: f64, ladder: &[[f64; 3]]) -> [f64; 3] {
    let min_step = if min_step.is_finite() { min_step } else { 0.0 };
    ladder
        .iter()
        .find(|steps| steps[0] >= min_step)
        .or(ladder.last())
        .copied()
        .unwrap_or([1.0, 5.0, 10.0])
}

/// Grid steps (s) for a time axis whose finest visible step is `base`:
/// round, nested durations (10 s / 30 s / 1 min, 30 s / 1 min / 5 min, ...)
/// with the minor lines at least three times `base` apart.
pub(crate) fn time_steps(base: f64) -> [f64; 3] {
    const LADDER: [[f64; 3]; 13] = [
        [1.0, 5.0, 10.0],
        [2.0, 10.0, 30.0],
        [5.0, 15.0, 60.0],
        [10.0, 30.0, 60.0],
        [15.0, 60.0, 300.0],
        [30.0, 60.0, 300.0],
        [60.0, 300.0, 600.0],
        [120.0, 600.0, 1800.0],
        [300.0, 900.0, 1800.0],
        [600.0, 1800.0, 3600.0],
        [900.0, 1800.0, 3600.0],
        [1800.0, 3600.0, 7200.0],
        [3600.0, 7200.0, 14400.0],
    ];
    nested_steps(base * 3.0, &LADDER)
}

/// Grid steps (degrees C or %) for a value axis whose finest visible step
/// is `base`: 5 / 25 / 50, 10 / 50 / 100, ...
pub(crate) fn value_steps(base: f64) -> [f64; 3] {
    const LADDER: [[f64; 3]; 7] = [
        [1.0, 5.0, 10.0],
        [2.0, 10.0, 20.0],
        [5.0, 25.0, 50.0],
        [10.0, 50.0, 100.0],
        [25.0, 50.0, 100.0],
        [50.0, 100.0, 500.0],
        [100.0, 500.0, 1000.0],
    ];
    nested_steps(base * 2.0, &LADDER)
}

/// Index of the sample nearest to `t`, if any.
pub(crate) fn nearest(history: &VecDeque<TempSample>, t: f64) -> Option<usize> {
    if history.is_empty() || !t.is_finite() {
        return None;
    }
    let i = history.partition_point(|s| s.t < t);
    let candidates = [i.checked_sub(1), (i < history.len()).then_some(i)];
    candidates.into_iter().flatten().min_by(|a, b| {
        let da = (history[*a].t - t).abs();
        let db = (history[*b].t - t).abs();
        da.total_cmp(&db)
    })
}

/// The whole history as CSV: a header and one row per sample with the
/// time, each heater's temperature and target and (where reported) its
/// power in percent. Missing values are empty cells.
pub(crate) fn history_csv(history: &VecDeque<TempSample>) -> String {
    let channels = channels(history);
    let has_power = |c: Channel| history.iter().any(|s| c.power(s).is_some());
    let mut out = String::from("time_s");
    for c in &channels {
        let key = c.key();
        let _ = write!(out, ",{key}_temp_c,{key}_target_c");
        if has_power(*c) {
            let _ = write!(out, ",{key}_power_pct");
        }
    }
    out.push('\n');
    let cell = |out: &mut String, v: Option<f32>, decimals: usize| {
        out.push(',');
        if let Some(v) = v.filter(|v| v.is_finite()) {
            let _ = write!(out, "{v:.decimals$}");
        }
    };
    for s in history.iter().filter(|s| s.t.is_finite()) {
        let _ = write!(out, "{:.1}", s.t);
        for c in &channels {
            let reading = c.reading(s);
            cell(&mut out, reading.map(|r| r.0), 2);
            cell(&mut out, reading.map(|r| r.1), 1);
            if has_power(*c) {
                cell(&mut out, c.power(s).map(|p| p.clamp(0.0, 1.0) * 100.0), 1);
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t: f64, hot: f32, target: f32) -> TempSample {
        TempSample {
            t,
            hotends: vec![(hot, target)],
            ..TempSample::default()
        }
    }

    fn pts(values: &[(f64, f32, f32)]) -> Vec<Point> {
        values
            .iter()
            .map(|&(t, current, target)| Point {
                t,
                current,
                target,
                power: None,
            })
            .collect()
    }

    #[test]
    fn channels_cover_every_heater_seen() {
        let mut h = VecDeque::new();
        assert!(channels(&h).is_empty());
        h.push_back(sample(0.0, 20.0, 0.0));
        h.push_back(TempSample {
            t: 1.0,
            hotends: vec![(20.0, 0.0), (21.0, 0.0)],
            chamber: Some((30.0, 0.0)),
            ..TempSample::default()
        });
        assert_eq!(
            channels(&h),
            [Channel::Hotend(0), Channel::Hotend(1), Channel::Chamber]
        );
        h.push_back(TempSample {
            t: 2.0,
            hotends: vec![(20.0, 0.0); 50],
            bed: Some((60.0, 60.0)),
            ..TempSample::default()
        });
        let c = channels(&h);
        assert_eq!(c.len(), MAX_HOTENDS + 2);
        assert_eq!(Channel::Hotend(0).name(1), "Hotend");
        assert_eq!(Channel::Hotend(1).name(2), "Hotend T1");
        assert_eq!(Channel::Bed.name(2), "Bed");
    }

    #[test]
    fn points_skip_missing_heaters() {
        let mut h = VecDeque::new();
        h.push_back(sample(0.0, 20.0, 0.0));
        h.push_back(TempSample {
            t: 1.0,
            bed: Some((50.0, 60.0)),
            bed_power: Some(0.5),
            ..TempSample::default()
        });
        h.push_back(sample(f64::NAN, 1.0, 0.0));
        assert_eq!(points(&h, Channel::Hotend(0)).len(), 1);
        let bed = points(&h, Channel::Bed);
        assert_eq!(bed.len(), 1);
        assert_eq!(bed[0].power, Some(0.5));
        assert!(points(&h, Channel::Chamber).is_empty());
    }

    #[test]
    fn runs_break_at_gaps_and_missing_values() {
        let p = pts(&[
            (0.0, 20.0, 0.0),
            (1.0, 21.0, 200.0),
            (2.0, f32::NAN, 200.0),
            (3.0, 23.0, 200.0),
            (30.0, 24.0, 0.0),
            (31.0, 25.0, 0.0),
        ]);
        let current = runs(&p, current_value);
        assert_eq!(current.len(), 3);
        assert_eq!(current[0], [[0.0, 20.0], [1.0, 21.0]]);
        assert_eq!(current[1], [[3.0, 23.0]]);
        assert_eq!(current[2].len(), 2);
        let target = runs(&p, target_value);
        assert_eq!(target, [vec![[1.0, 200.0], [2.0, 200.0], [3.0, 200.0]]]);
        assert!(runs(&[], current_value).is_empty());
    }

    #[test]
    fn window_statistics() {
        let p = pts(&[
            (0.0, 10.0, 0.0),
            (1.0, 20.0, 0.0),
            (2.0, f32::NAN, 0.0),
            (3.0, 30.0, 0.0),
            (4.0, 100.0, 0.0),
        ]);
        let s = window_stats(&p, 0.0, 3.0).expect("stats");
        assert_eq!(s.count, 3);
        assert_eq!((s.min, s.max, s.mean), (10.0, 30.0, 20.0));
        assert!((s.std_dev - 8.164_966).abs() < 1e-4, "{}", s.std_dev);
        // Reversed bounds work; empty windows give nothing.
        assert_eq!(window_stats(&p, 3.0, 0.0), Some(s));
        assert_eq!(window_stats(&p, 10.0, 20.0), None);
        let flat = pts(&[(0.0, 215.0, 215.0), (1.0, 215.0, 215.0)]);
        assert_eq!(window_stats(&flat, 0.0, 1.0).map(|s| s.std_dev), Some(0.0));
    }

    #[test]
    fn stability_needs_thirty_covered_seconds_in_band() {
        let steady: Vec<_> = (0..=40)
            .map(|i| {
                (
                    f64::from(i),
                    215.0 + if i % 2 == 0 { 0.3 } else { -0.4 },
                    215.0,
                )
            })
            .collect();
        assert!(is_stable(&pts(&steady), STABLE_SECS, STABLE_BAND));
        // One excursion inside the last 30 s breaks it ...
        let mut bumped = steady.clone();
        bumped[30].1 = 216.0;
        assert!(!is_stable(&pts(&bumped), STABLE_SECS, STABLE_BAND));
        // ... but not one before.
        let mut early = steady.clone();
        early[5].1 = 230.0;
        assert!(is_stable(&pts(&early), STABLE_SECS, STABLE_BAND));
        // Too little history, the heater off, or no readings: not stable.
        assert!(!is_stable(&pts(&steady[30..]), STABLE_SECS, STABLE_BAND));
        let off: Vec<_> = (0..=40).map(|i| (f64::from(i), 22.0, 0.0)).collect();
        assert!(!is_stable(&pts(&off), STABLE_SECS, STABLE_BAND));
        assert!(!is_stable(&[], STABLE_SECS, STABLE_BAND));
        let mut nan = steady;
        nan[35].1 = f32::NAN;
        assert!(!is_stable(&pts(&nan), STABLE_SECS, STABLE_BAND));
    }

    #[test]
    fn follow_ranges() {
        // Short history: the window starts at the first sample.
        assert_eq!(follow_range(Some(300.0), 0.0, 100.0), (0.0, 300.0));
        // Long history: the last window.
        assert_eq!(follow_range(Some(120.0), 0.0, 1000.0), (880.0, 1000.0));
        // Everything, at least a minute.
        assert_eq!(follow_range(None, 10.0, 20.0), (10.0, 70.0));
        assert_eq!(follow_range(None, 10.0, 900.0), (10.0, 900.0));
        assert_eq!(follow_range(Some(f64::NAN), 0.0, 5.0), (0.0, 60.0));
        assert_eq!(follow_range(Some(60.0), f64::NAN, 5.0), (0.0, 60.0));
    }

    #[test]
    fn y_ranges_cover_targets_and_stay_steady() {
        let hot = pts(&[(0.0, 25.0, 0.0), (10.0, 180.0, 215.0)]);
        let bed = pts(&[(0.0, 22.0, 60.0), (10.0, 55.0, 60.0)]);
        let (lo, hi) = y_range([hot.as_slice(), bed.as_slice()], 0.0, 10.0);
        assert!((0.0..=22.0).contains(&lo), "{lo}");
        assert!(hi >= 215.0, "{hi}");
        assert_eq!(lo % 5.0, 0.0);
        assert_eq!(hi % 5.0, 0.0);
        // Outside the window nothing counts.
        assert_eq!(y_range([hot.as_slice()], 100.0, 200.0), (0.0, 100.0));
        // A flat line still gets a usable span.
        let flat = pts(&[(0.0, 215.0, 215.0)]);
        let (lo, hi) = y_range([flat.as_slice()], 0.0, 1.0);
        assert!(hi - lo >= 20.0 && lo < 215.0 && hi > 215.0);
        // Near zero the range does not go negative.
        let cold = pts(&[(0.0, 2.0, 0.0)]);
        assert_eq!(y_range([cold.as_slice()], 0.0, 1.0).0, 0.0);
    }

    #[test]
    fn clock_and_span_labels() {
        assert_eq!(format_clock(0.0), "0:00");
        assert_eq!(format_clock(65.4), "1:05");
        assert_eq!(format_clock(3723.0), "1:02:03");
        assert_eq!(format_clock(-10.0), "\u{2212}0:10");
        assert_eq!(format_clock(f64::NAN), "—");
        assert_eq!(format_span(45.0), "45 s");
        assert_eq!(format_span(300.0), "5 min");
        assert_eq!(format_span(3900.0), "1 h 05 min");
        assert_eq!(format_span(-1.0), "—");
    }

    #[test]
    fn grid_steps_are_round_and_nested() {
        assert_eq!(time_steps(0.3), [1.0, 5.0, 10.0]);
        assert_eq!(time_steps(5.3), [30.0, 60.0, 300.0]);
        assert_eq!(time_steps(45.0), [300.0, 900.0, 1800.0]);
        assert_eq!(time_steps(1e6), [3600.0, 7200.0, 14400.0]);
        assert_eq!(time_steps(f64::NAN), [1.0, 5.0, 10.0]);
        assert_eq!(value_steps(5.7), [25.0, 50.0, 100.0]);
        assert_eq!(value_steps(0.1), [1.0, 5.0, 10.0]);
        for base in [0.01, 0.5, 3.0, 20.0, 77.0, 400.0, 5000.0] {
            for [a, b, c] in [time_steps(base), value_steps(base)] {
                assert!(a <= b && b <= c);
                assert_eq!(b % a, 0.0, "{a} {b}");
                assert_eq!(c % b, 0.0, "{b} {c}");
            }
        }
    }

    #[test]
    fn nearest_sample() {
        let h: VecDeque<_> = [0.0, 1.0, 2.0, 10.0]
            .into_iter()
            .map(|t| sample(t, 20.0, 0.0))
            .collect();
        assert_eq!(nearest(&h, -5.0), Some(0));
        assert_eq!(nearest(&h, 1.4), Some(1));
        assert_eq!(nearest(&h, 1.6), Some(2));
        assert_eq!(nearest(&h, 7.0), Some(3));
        assert_eq!(nearest(&h, 99.0), Some(3));
        assert_eq!(nearest(&h, f64::NAN), None);
        assert_eq!(nearest(&VecDeque::new(), 1.0), None);
    }

    #[test]
    fn csv_has_a_column_per_heater() {
        let mut h = VecDeque::new();
        h.push_back(TempSample {
            t: 0.0,
            hotends: vec![(24.5, 0.0)],
            bed: Some((22.0, 60.0)),
            hotend_power: vec![0.0],
            bed_power: Some(1.0),
            ..TempSample::default()
        });
        h.push_back(TempSample {
            t: 1.04,
            hotends: vec![(f32::NAN, 215.0)],
            bed: None,
            hotend_power: vec![0.646],
            bed_power: None,
            ..TempSample::default()
        });
        let csv = history_csv(&h);
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "time_s,T0_temp_c,T0_target_c,T0_power_pct,bed_temp_c,bed_target_c,bed_power_pct"
        );
        assert_eq!(lines[1], "0.0,24.50,0.0,0.0,22.00,60.0,100.0");
        assert_eq!(lines[2], "1.0,,215.0,64.6,,,");
        assert_eq!(lines.len(), 3);
        assert_eq!(history_csv(&VecDeque::new()), "time_s\n");
    }
}
