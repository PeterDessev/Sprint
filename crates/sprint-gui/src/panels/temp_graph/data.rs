//! Pure data behind the temperature graph: heater channels and their
//! samples from the history, line runs, window statistics, stability,
//! view ranges, time labels and CSV export.
//!
//! Everything works on the [`usable`] samples of the history: corrupt
//! times and implausible readings never reach the chart (egui_plot asserts
//! on degenerate bounds and would draw grids with billions of lines).

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
/// Sample times beyond ±this many seconds are corrupt (the history counts
/// from connecting and spans about 30 minutes).
pub(crate) const MAX_TIME: f64 = 1e9;
/// Readings and targets beyond ±this many °C are corrupt (sensor
/// faults report values far outside any real heater range).
pub(crate) const MAX_TEMP: f32 = 5000.0;

/// The usable samples of `history`, oldest first: those with a finite,
/// plausible time. Should time ever run backwards, only the newest
/// consistent run is kept so the chart recovers instead of freezing.
pub(crate) fn usable(history: &VecDeque<TempSample>) -> Vec<&TempSample> {
    let mut out: Vec<&TempSample> = Vec::with_capacity(history.len());
    for s in history {
        if !(s.t.is_finite() && s.t.abs() <= MAX_TIME) {
            continue;
        }
        if out.last().is_some_and(|prev| s.t < prev.t) {
            out.clear();
        }
        out.push(s);
    }
    out
}

/// Whether the history started over between two frames, given the time of
/// its newest sample then and now: it emptied (a new connection clears it)
/// or its newest time went back (a new connection counts from 0 s).
pub(crate) fn restarted(before: Option<f64>, now: Option<f64>) -> bool {
    match (before, now) {
        (Some(before), Some(now)) => now < before,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// A reading as plotted: an implausible temperature becomes NaN (missing)
/// and an implausible target 0 (off).
fn plausible((current, target): (f32, f32)) -> (f32, f32) {
    let current = if current.is_finite() && current.abs() <= MAX_TEMP {
        current
    } else {
        f32::NAN
    };
    let target = if target.is_finite() && target.abs() <= MAX_TEMP {
        target
    } else {
        0.0
    };
    (current, target)
}

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

    /// (current, target) at a sample, if the heater was reported; an
    /// implausible current is NaN and an implausible target 0.
    pub(crate) fn reading(self, s: &TempSample) -> Option<(f32, f32)> {
        match self {
            Channel::Hotend(i) => s.hotends.get(i).copied(),
            Channel::Bed => s.bed,
            Channel::Chamber => s.chamber,
        }
        .map(plausible)
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

/// Whether `channel`'s heater power is known: its latest report carries
/// one (`reported_now`) or some sample shows it heating. Firmware without
/// power reports (no `@:`) leaves the host's history with placeholder
/// zeros for its hotends, which are not readings.
pub(crate) fn power_reported(
    samples: &[&TempSample],
    channel: Channel,
    reported_now: bool,
) -> bool {
    reported_now
        || samples
            .iter()
            .any(|s| channel.power(s).is_some_and(|p| p > 0.0))
}

/// Every heater that appears anywhere in `samples`, in display order.
pub(crate) fn channels(samples: &[&TempSample]) -> Vec<Channel> {
    let hotends = samples
        .iter()
        .map(|s| s.hotends.len())
        .max()
        .unwrap_or(0)
        .min(MAX_HOTENDS);
    let mut list: Vec<Channel> = (0..hotends).map(Channel::Hotend).collect();
    if samples.iter().any(|s| s.bed.is_some()) {
        list.push(Channel::Bed);
    }
    if samples.iter().any(|s| s.chamber.is_some()) {
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

/// The readings of `channel`, oldest first (samples without it are
/// skipped).
pub(crate) fn points(samples: &[&TempSample], channel: Channel) -> Vec<Point> {
    samples
        .iter()
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

/// `run` without the inner points of flat stretches (a target held for
/// minutes becomes two points), keeping its shape. Long straight segments
/// keep a dashed line's pattern even.
pub(crate) fn simplify_flat(run: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    if run.len() <= 2 {
        return run;
    }
    let last = run.len() - 1;
    run.iter()
        .enumerate()
        .filter(|&(i, p)| i == 0 || i == last || run[i - 1][1] != p[1] || run[i + 1][1] != p[1])
        .map(|(_, p)| *p)
        .collect()
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
/// first ladder entry whose minor step is at least `min_step`. Beyond the
/// ladder its last entry is scaled by powers of ten, so a far zoomed-out
/// axis still gets a few dozen lines rather than billions.
fn nested_steps(min_step: f64, ladder: &[[f64; 3]]) -> [f64; 3] {
    let min_step = if min_step.is_nan() { 0.0 } else { min_step };
    if let Some(steps) = ladder.iter().find(|steps| steps[0] >= min_step) {
        return *steps;
    }
    let Some(&[a, b, c]) = ladder.last() else {
        return [1.0, 5.0, 10.0];
    };
    let exponent = (min_step / a).log10().ceil().clamp(1.0, 300.0);
    let scale = 10f64.powi(exponent as i32);
    [a * scale, b * scale, c * scale]
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

/// The sample nearest to `t` (`samples` sorted by time), if any.
pub(crate) fn nearest<'a>(samples: &[&'a TempSample], t: f64) -> Option<&'a TempSample> {
    if !t.is_finite() {
        return None;
    }
    let i = samples.partition_point(|s| s.t < t);
    let candidates = [i.checked_sub(1), (i < samples.len()).then_some(i)];
    candidates
        .into_iter()
        .flatten()
        .filter_map(|i| samples.get(i).copied())
        .min_by(|a, b| (a.t - t).abs().total_cmp(&(b.t - t).abs()))
}

/// The sample nearest to `t` if it is at most `tolerance` seconds away.
pub(crate) fn nearest_within<'a>(
    samples: &[&'a TempSample],
    t: f64,
    tolerance: f64,
) -> Option<&'a TempSample> {
    nearest(samples, t).filter(|s| (s.t - t).abs() <= tolerance)
}

/// `samples` as CSV: a header and one row per sample with the time, each
/// heater's temperature and target and, for the `powered` heaters (see
/// [`power_reported`]), its power in percent. Missing values are empty
/// cells.
pub(crate) fn history_csv(samples: &[&TempSample], powered: &[Channel]) -> String {
    let channels = channels(samples);
    let has_power =
        |c: Channel| powered.contains(&c) && samples.iter().any(|s| c.power(s).is_some());
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
    for s in samples {
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
        assert!(channels(&usable(&h)).is_empty());
        h.push_back(sample(0.0, 20.0, 0.0));
        h.push_back(TempSample {
            t: 1.0,
            hotends: vec![(20.0, 0.0), (21.0, 0.0)],
            chamber: Some((30.0, 0.0)),
            ..TempSample::default()
        });
        assert_eq!(
            channels(&usable(&h)),
            [Channel::Hotend(0), Channel::Hotend(1), Channel::Chamber]
        );
        h.push_back(TempSample {
            t: 2.0,
            hotends: vec![(20.0, 0.0); 50],
            bed: Some((60.0, 60.0)),
            ..TempSample::default()
        });
        let c = channels(&usable(&h));
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
        let h = usable(&h);
        assert_eq!(points(&h, Channel::Hotend(0)).len(), 1);
        let bed = points(&h, Channel::Bed);
        assert_eq!(bed.len(), 1);
        assert_eq!(bed[0].power, Some(0.5));
        assert!(points(&h, Channel::Chamber).is_empty());
    }

    #[test]
    fn usable_samples_drop_corrupt_times() {
        let times =
            |h: &VecDeque<TempSample>| -> Vec<f64> { usable(h).iter().map(|s| s.t).collect() };
        let h: VecDeque<_> = [0.0, f64::NAN, 1.0, f64::INFINITY, 2.0, 1e300, -1e12, 3.0]
            .into_iter()
            .map(|t| sample(t, 20.0, 0.0))
            .collect();
        assert_eq!(times(&h), [0.0, 1.0, 2.0, 3.0]);
        // Time running backwards keeps the newest consistent run.
        let h: VecDeque<_> = [100.0, 101.0, 102.0, 5.0, 6.0]
            .into_iter()
            .map(|t| sample(t, 20.0, 0.0))
            .collect();
        assert_eq!(times(&h), [5.0, 6.0]);
        // Equal times are kept (two reports in the same instant).
        let h: VecDeque<_> = [1.0, 1.0, 2.0]
            .into_iter()
            .map(|t| sample(t, 20.0, 0.0))
            .collect();
        assert_eq!(times(&h), [1.0, 1.0, 2.0]);
        assert!(usable(&VecDeque::new()).is_empty());
    }

    #[test]
    fn placeholder_power_is_not_a_reading() {
        let mut h = VecDeque::new();
        for i in 0..5 {
            h.push_back(TempSample {
                t: f64::from(i),
                hotends: vec![(20.0, 200.0), (20.0, 0.0)],
                hotend_power: vec![0.0, 0.0],
                bed: Some((20.0, 60.0)),
                bed_power: (i == 3).then_some(0.0),
                ..TempSample::default()
            });
        }
        let s = usable(&h);
        // Zeros only: reported now, or not at all.
        assert!(!power_reported(&s, Channel::Hotend(0), false));
        assert!(power_reported(&s, Channel::Hotend(0), true));
        assert!(!power_reported(&s, Channel::Bed, false));
        // Any real power counts.
        h[2].hotend_power[1] = 0.4;
        let s = usable(&h);
        assert!(power_reported(&s, Channel::Hotend(1), false));
        assert!(!power_reported(&s, Channel::Chamber, false));
    }

    #[test]
    fn restarts_are_detected() {
        assert!(!restarted(None, None));
        assert!(!restarted(None, Some(5.0)));
        assert!(!restarted(Some(5.0), Some(5.0)));
        assert!(!restarted(Some(5.0), Some(6.0)));
        assert!(restarted(Some(600.0), Some(2.0)));
        assert!(restarted(Some(600.0), None));
    }

    #[test]
    fn implausible_readings_are_missing() {
        let s = TempSample {
            t: 0.0,
            hotends: vec![
                (f32::MAX, -f32::MAX),
                (f32::NAN, f32::INFINITY),
                (215.0, 215.0),
            ],
            bed: Some((-6000.0, 60.0)),
            ..TempSample::default()
        };
        let (current, target) = Channel::Hotend(0).reading(&s).expect("reported");
        assert!(current.is_nan());
        assert_eq!(target, 0.0);
        let (current, target) = Channel::Hotend(1).reading(&s).expect("reported");
        assert!(current.is_nan());
        assert_eq!(target, 0.0);
        assert_eq!(Channel::Hotend(2).reading(&s), Some((215.0, 215.0)));
        assert!(
            Channel::Bed
                .reading(&s)
                .is_some_and(|(c, t)| c.is_nan() && t == 60.0)
        );
        assert_eq!(Channel::Hotend(3).reading(&s), None);
        assert_eq!(Channel::Chamber.reading(&s), None);
    }

    #[test]
    fn flat_stretches_collapse() {
        let run: Vec<[f64; 2]> = (0..10).map(|i| [f64::from(i), 215.0]).collect();
        assert_eq!(simplify_flat(run), [[0.0, 215.0], [9.0, 215.0]]);
        let step = vec![
            [0.0, 215.0],
            [1.0, 215.0],
            [2.0, 215.0],
            [3.0, 230.0],
            [4.0, 230.0],
            [5.0, 230.0],
        ];
        assert_eq!(
            simplify_flat(step),
            [[0.0, 215.0], [2.0, 215.0], [3.0, 230.0], [5.0, 230.0]]
        );
        assert_eq!(simplify_flat(vec![[0.0, 1.0]]), [[0.0, 1.0]]);
        assert!(simplify_flat(Vec::new()).is_empty());
        let wavy = vec![[0.0, 1.0], [1.0, 2.0], [2.0, 1.0]];
        assert_eq!(simplify_flat(wavy.clone()), wavy);
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
        assert_eq!(time_steps(1000.0), [3600.0, 7200.0, 14400.0]);
        // Far beyond the ladder the steps grow by powers of ten.
        assert_eq!(time_steps(2000.0), [36000.0, 72000.0, 144000.0]);
        assert_eq!(time_steps(1e9)[0], 3.6e9);
        assert_eq!(time_steps(f64::NAN), [1.0, 5.0, 10.0]);
        assert!(time_steps(f64::INFINITY)[0].is_finite());
        assert_eq!(value_steps(1e6), [1e7, 5e7, 1e8]);
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
        let h = usable(&h);
        let at = |t: f64| nearest(&h, t).map(|s| s.t);
        assert_eq!(at(-5.0), Some(0.0));
        assert_eq!(at(1.4), Some(1.0));
        assert_eq!(at(1.6), Some(2.0));
        assert_eq!(at(7.0), Some(10.0));
        assert_eq!(at(99.0), Some(10.0));
        assert_eq!(at(f64::NAN), None);
        assert!(nearest(&[], 1.0).is_none());
        // Only close enough to the pointer.
        let within = |t: f64, tol: f64| nearest_within(&h, t, tol).map(|s| s.t);
        assert_eq!(within(5.5, 5.0), Some(2.0));
        assert_eq!(within(5.5, 3.0), None);
        assert_eq!(within(99.0, MAX_GAP), None);
        assert_eq!(within(19.0, MAX_GAP), Some(10.0));
        assert_eq!(within(1.0, f64::NAN), None);
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
        let all = [Channel::Hotend(0), Channel::Bed];
        let csv = history_csv(&usable(&h), &all);
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "time_s,T0_temp_c,T0_target_c,T0_power_pct,bed_temp_c,bed_target_c,bed_power_pct"
        );
        assert_eq!(lines[1], "0.0,24.50,0.0,0.0,22.00,60.0,100.0");
        assert_eq!(lines[2], "1.0,,215.0,64.6,,,");
        assert_eq!(lines.len(), 3);
        assert_eq!(history_csv(&[], &all), "time_s\n");
        // Heaters without power reports get no power column.
        let csv = history_csv(&usable(&h), &[Channel::Bed]);
        assert_eq!(
            csv.lines().next(),
            Some("time_s,T0_temp_c,T0_target_c,bed_temp_c,bed_target_c,bed_power_pct")
        );
        assert_eq!(csv.lines().nth(2), Some("1.0,,215.0,,,"));
    }
}
