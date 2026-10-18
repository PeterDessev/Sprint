//! Kinematic print-time estimation: a Marlin-like motion planner.
//!
//! Moves become blocks with a nominal speed (commanded feedrate limited per
//! axis), an acceleration (print, travel or retract, limited per axis) and a
//! maximum entry speed from the junction with the previous block (Marlin's
//! junction-deviation algorithm, including its small-segment arc handling,
//! or classic jerk). Blocks are planned with trapezoidal velocity profiles
//! and a backward/forward pass over a bounded look-ahead window, like
//! Marlin's 16-block planner buffer: each block is committed once the window
//! behind it is full, assuming the machine must be able to stop at the end of
//! the window. Long runs of tiny segments therefore cannot reach speeds the
//! real firmware could not.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::parse::{ExtendedCommand, ParsedLine};

/// Firmware motion limits used by the estimator. M201/M203/M204/M205 found
/// in a file override these while it is analysed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KinematicLimits {
    /// Maximum feedrate per axis X Y Z E, mm/s (M203).
    pub max_feedrate: [f32; 4],
    /// Maximum acceleration per axis X Y Z E, mm/s² (M201).
    pub max_accel: [f32; 4],
    /// Printing acceleration, mm/s² (M204 P, or S).
    pub print_accel: f32,
    /// Retract/prime (E-only move) acceleration, mm/s² (M204 R).
    pub retract_accel: f32,
    /// Travel acceleration, mm/s² (M204 T, or S).
    pub travel_accel: f32,
    /// Junction deviation in mm (M205 J); `None` selects classic jerk.
    pub junction_deviation: Option<f32>,
    /// Classic jerk per axis X Y Z E, mm/s (M205 X Y Z E). With junction
    /// deviation these are unused.
    pub jerk: [f32; 4],
    /// Minimum feedrate, mm/s (M205 S).
    pub min_feedrate: f32,
}

impl Default for KinematicLimits {
    fn default() -> Self {
        KinematicLimits {
            max_feedrate: [500.0, 500.0, 12.0, 120.0],
            max_accel: [3000.0, 3000.0, 100.0, 10000.0],
            print_accel: 1500.0,
            retract_accel: 3000.0,
            travel_accel: 1500.0,
            junction_deviation: Some(0.013),
            jerk: [8.0, 8.0, 0.4, 5.0],
            min_feedrate: 0.0,
        }
    }
}

const AXES: [char; 4] = ['X', 'Y', 'Z', 'E'];

fn positive(v: Option<f64>) -> Option<f32> {
    v.filter(|v| *v > 0.0 && v.is_finite())
        .map(|v| v.min(1e9) as f32)
}

impl KinematicLimits {
    /// Apply a limit-setting command (M201, M203, M204, M205). Returns true
    /// when the line was one of them. Values are in Marlin units (mm/s,
    /// mm/s²); non-positive values are ignored.
    ///
    /// `M204 S` sets the print and travel accelerations; combined with `T`
    /// (the legacy Marlin 1.x / Prusa form `M204 S.. T..`) the `T` value is
    /// the retract acceleration. `M205 J` outside 0.01..=0.3 mm is ignored,
    /// as Marlin rejects it.
    pub fn apply_gcode(&mut self, line: &ParsedLine) -> bool {
        let Some(code) = line.code else {
            return false;
        };
        if code.letter != b'M' || code.sub.is_some() {
            return false;
        }
        match code.number {
            201 => {
                for (i, axis) in AXES.into_iter().enumerate() {
                    if let Some(v) = positive(line.get(axis)) {
                        self.max_accel[i] = v;
                    }
                }
            }
            203 => {
                for (i, axis) in AXES.into_iter().enumerate() {
                    if let Some(v) = positive(line.get(axis)) {
                        self.max_feedrate[i] = v;
                    }
                }
            }
            204 => {
                if let Some(s) = positive(line.get('S')) {
                    self.print_accel = s;
                    self.travel_accel = s;
                    if let Some(t) = positive(line.get('T')) {
                        self.retract_accel = t;
                    }
                } else {
                    if let Some(p) = positive(line.get('P')) {
                        self.print_accel = p;
                    }
                    if let Some(r) = positive(line.get('R')) {
                        self.retract_accel = r;
                    }
                    if let Some(t) = positive(line.get('T')) {
                        self.travel_accel = t;
                    }
                }
            }
            205 => {
                for (i, axis) in AXES.into_iter().enumerate() {
                    if let Some(v) = line.get(axis).filter(|v| *v >= 0.0 && v.is_finite()) {
                        self.jerk[i] = v.min(1e6) as f32;
                    }
                }
                // Marlin rejects junction deviations outside 0.01..=0.3 mm
                // ("?J out of range") and keeps the previous value.
                if let Some(j) = line.get('J').filter(|v| (0.01..=0.3).contains(v)) {
                    self.junction_deviation = Some(j as f32);
                }
                if let Some(s) = line.get('S').filter(|v| *v >= 0.0 && v.is_finite()) {
                    self.min_feedrate = s.min(1e6) as f32;
                }
            }
            _ => return false,
        }
        true
    }

    /// Apply Klipper's `SET_VELOCITY_LIMIT VELOCITY= ACCEL=
    /// SQUARE_CORNER_VELOCITY=`. Returns true when it was that command.
    pub fn apply_extended(&mut self, cmd: &ExtendedCommand<'_>) -> bool {
        if !cmd.is("SET_VELOCITY_LIMIT") {
            return false;
        }
        if let Some(v) = positive(cmd.param_f64("VELOCITY")) {
            self.max_feedrate[0] = v;
            self.max_feedrate[1] = v;
        }
        if let Some(a) = positive(cmd.param_f64("ACCEL")) {
            self.print_accel = a;
            self.travel_accel = a;
        }
        if let Some(scv) = cmd
            .param_f64("SQUARE_CORNER_VELOCITY")
            .filter(|v| *v >= 0.0)
        {
            // Klipper derives its junction deviation from the square corner
            // velocity: jd = scv² (√2 − 1) / accel.
            let accel = f64::from(self.print_accel.max(1.0));
            let jd = scv * scv * (std::f64::consts::SQRT_2 - 1.0) / accel;
            self.junction_deviation = Some(jd.min(100.0) as f32);
        }
        true
    }
}

/// Marlin's `MINIMUM_PLANNER_SPEED` (mm/s), used for full reversals.
const MIN_PLANNER_SPEED: f64 = 0.05;
/// Lowest nominal speed a block can have (mm/s), so `F0` cannot stall.
const MIN_NOMINAL_SPEED: f64 = 0.1;
/// Default look-ahead window, matching Marlin's `BLOCK_BUFFER_SIZE`.
pub const DEFAULT_PLANNER_WINDOW: usize = 16;

/// A block whose velocity profile is final.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlannedBlock {
    /// Caller-supplied tag passed to [`Planner::push`].
    pub tag: Option<u32>,
    /// Path length (XYZ, or |E| for E-only moves), mm.
    pub length: f64,
    /// Speed at the start of the block, mm/s.
    pub entry_speed: f64,
    /// Speed at the end of the block, mm/s.
    pub exit_speed: f64,
    /// Highest speed reached, mm/s.
    pub peak_speed: f64,
    /// Duration, seconds.
    pub duration: f64,
}

#[derive(Clone, Copy, Debug)]
struct Block {
    length: f64,
    accel: f64,
    nominal: f64,
    max_entry: f64,
    /// Fixed entry speed; valid for the block at the front of the queue.
    entry: f64,
    tag: Option<u32>,
}

/// What the junction computation needs to know about the previous block.
#[derive(Clone, Copy, Debug)]
struct PrevBlock {
    unit: [f64; 4],
    axis_speed: [f64; 4],
    nominal: f64,
    safe_speed: f64,
}

/// Marlin-like look-ahead motion planner.
///
/// Feed moves with [`Planner::push`]; each call may return a block whose
/// profile became final. [`Planner::flush`] brings the machine to a stop
/// (dwell, heater wait, end of file) and returns the remaining blocks.
#[derive(Clone, Debug)]
pub struct Planner {
    limits: KinematicLimits,
    window: usize,
    queue: VecDeque<Block>,
    prev: Option<PrevBlock>,
}

impl Planner {
    /// Planner with the default 16-block window.
    pub fn new(limits: KinematicLimits) -> Planner {
        Planner::with_window(limits, DEFAULT_PLANNER_WINDOW)
    }

    /// Planner with a custom look-ahead window (at least 1 block; 1 means
    /// every move starts and ends at rest).
    pub fn with_window(limits: KinematicLimits, window: usize) -> Planner {
        let window = window.max(1);
        Planner {
            limits,
            window,
            queue: VecDeque::with_capacity(window + 1),
            prev: None,
        }
    }

    /// Current limits.
    pub fn limits(&self) -> &KinematicLimits {
        &self.limits
    }

    /// Change limits; applies to moves pushed afterwards (as in Marlin).
    pub fn limits_mut(&mut self) -> &mut KinematicLimits {
        &mut self.limits
    }

    /// Number of blocks waiting for their profile to become final.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Queue a move of `delta` = [dx, dy, dz, de] mm at `feedrate` mm/s
    /// (already scaled by any speed override). Zero-length moves are
    /// ignored. Returns the oldest block if its profile became final.
    pub fn push(
        &mut self,
        delta: [f64; 4],
        feedrate: f64,
        tag: Option<u32>,
    ) -> Option<PlannedBlock> {
        if !delta.iter().all(|v| v.is_finite()) {
            return None;
        }
        let [dx, dy, dz, de] = delta;
        let xyz = (dx * dx + dy * dy + dz * dz).sqrt();
        let e_only = xyz < 1e-9;
        let length = if e_only { de.abs() } else { xyz };
        if length < 1e-9 {
            return None;
        }
        let lim = self.limits;
        let inv_len = 1.0 / length;

        // Nominal speed, limited per axis.
        let feedrate = if feedrate.is_finite() { feedrate } else { 0.0 };
        let mut nominal = feedrate
            .max(f64::from(lim.min_feedrate))
            .max(MIN_NOMINAL_SPEED);
        for (axis, d) in delta.iter().enumerate() {
            let component = d.abs() * inv_len;
            let max = f64::from(lim.max_feedrate[axis]);
            if component > 0.0 && max > 0.0 && nominal * component > max {
                nominal = max / component;
            }
        }
        nominal = nominal.max(MIN_NOMINAL_SPEED);

        // Acceleration: retract for E-only moves, print when E moves,
        // travel otherwise; then limited per axis.
        let mut accel = f64::from(if e_only {
            lim.retract_accel
        } else if de != 0.0 {
            lim.print_accel
        } else {
            lim.travel_accel
        });
        for (axis, d) in delta.iter().enumerate() {
            let component = d.abs() * inv_len;
            let max = f64::from(lim.max_accel[axis]);
            if component > 0.0 && max > 0.0 && accel * component > max {
                accel = max / component;
            }
        }
        let accel = accel.max(1.0);

        // Junction unit vector: XYZE-normalised when E moves (as Marlin).
        let unit = if de != 0.0 {
            let n = (xyz * xyz + de * de).sqrt();
            [dx / n, dy / n, dz / n, de / n]
        } else {
            [dx * inv_len, dy * inv_len, dz * inv_len, 0.0]
        };
        let axis_speed = [
            dx * inv_len * nominal,
            dy * inv_len * nominal,
            dz * inv_len * nominal,
            de * inv_len * nominal,
        ];
        let safe_speed = safe_speed(nominal, &axis_speed, &lim.jerk);

        let from_rest = self.queue.is_empty();
        let max_entry = match (from_rest, self.prev) {
            (false, Some(prev)) => match lim.junction_deviation {
                Some(jd) => junction_deviation_speed(
                    &prev,
                    &unit,
                    nominal,
                    accel,
                    length,
                    f64::from(jd),
                    &lim.max_accel,
                ),
                None => classic_jerk_speed(&prev, &axis_speed, nominal, safe_speed, &lim.jerk),
            },
            _ => match lim.junction_deviation {
                Some(_) => 0.0,
                None => safe_speed,
            },
        };
        let max_entry = max_entry.clamp(0.0, nominal);
        let entry = if from_rest {
            // Starting from rest: also able to stop within this block.
            max_entry.min((2.0 * accel * length).sqrt())
        } else {
            0.0
        };
        self.queue.push_back(Block {
            length,
            accel,
            nominal,
            max_entry,
            entry,
            tag,
        });
        self.prev = Some(PrevBlock {
            unit,
            axis_speed,
            nominal,
            safe_speed,
        });
        if self.queue.len() >= self.window {
            self.finalize_front()
        } else {
            None
        }
    }

    /// Decelerate to a stop: plan and return every queued block.
    pub fn flush(&mut self, mut sink: impl FnMut(PlannedBlock)) {
        while let Some(block) = self.finalize_front() {
            sink(block);
        }
        self.prev = None;
    }

    /// Commit the oldest block: backward pass from a stop after the newest
    /// block, forward limit from the oldest block's fixed entry speed.
    fn finalize_front(&mut self) -> Option<PlannedBlock> {
        let n = self.queue.len();
        let mut next_entry = 0.0f64;
        for i in (1..n).rev() {
            if let Some(b) = self.queue.get(i) {
                next_entry = (next_entry * next_entry + 2.0 * b.accel * b.length)
                    .sqrt()
                    .min(b.max_entry);
            }
        }
        let front = self.queue.pop_front()?;
        let exit = if n > 1 {
            let max_exit = (front.entry * front.entry + 2.0 * front.accel * front.length).sqrt();
            next_entry.min(max_exit)
        } else {
            0.0
        };
        if let Some(next) = self.queue.front_mut() {
            next.entry = exit;
        }
        let (duration, peak) =
            trapezoid_profile(front.length, front.entry, exit, front.nominal, front.accel);
        Some(PlannedBlock {
            tag: front.tag,
            length: front.length,
            entry_speed: front.entry,
            exit_speed: exit,
            peak_speed: peak,
            duration,
        })
    }
}

/// Classic jerk: the fastest a block can start from, or stop to, a
/// standstill. Each axis then jumps between zero and its share of the speed
/// at once, and that jump must stay within the axis's jerk limit, so the
/// axis that needs the most slowing down sets the speed. (The same rule as
/// Marlin's classic-jerk planner.)
fn safe_speed(nominal: f64, axis_speed: &[f64; 4], jerk: &[f32; 4]) -> f64 {
    let mut speed = nominal;
    for (share, &limit) in axis_speed.iter().zip(jerk) {
        let jump = share.abs();
        let limit = f64::from(limit);
        if jump > limit {
            speed = speed.min(nominal * limit / jump);
        }
    }
    speed
}

/// Classic jerk at a junction: the slower of the two blocks' nominal
/// speeds, scaled down until no axis changes speed by more than its jerk
/// limit between the end of the previous block and the start of this one.
/// When both blocks could start and stop at about that speed anyway, their
/// safe speed is used instead. (The same rule as Marlin's classic-jerk
/// planner.)
fn classic_jerk_speed(
    prev: &PrevBlock,
    axis_speed: &[f64; 4],
    nominal: f64,
    safe: f64,
    jerk: &[f32; 4],
) -> f64 {
    if prev.nominal <= 1e-9 {
        return safe;
    }
    let ceiling = nominal.min(prev.nominal);
    // The previous block's axis speeds, slowed down to the ceiling.
    let slow_down = ceiling / prev.nominal;
    let mut scale = 1.0;
    for ((&before, &after), &limit) in prev.axis_speed.iter().zip(axis_speed).zip(jerk) {
        let change = speed_change(before * slow_down * scale, after * scale);
        let limit = f64::from(limit);
        if change > limit {
            scale *= limit / change;
        }
    }
    let junction = ceiling * scale;
    let close = junction * 0.99;
    if prev.safe_speed > close && safe > close {
        safe
    } else {
        junction
    }
}

/// How much an axis's speed jumps from `before` to `after`: the difference
/// while it keeps its direction, the larger of the two speeds when it
/// reverses (it has to stop and start again).
fn speed_change(before: f64, after: f64) -> f64 {
    if before * after > 0.0 {
        (before - after).abs()
    } else {
        before.abs().max(after.abs())
    }
}

/// Junction deviation at a junction: the speed at which the turn from the
/// previous direction into this one, taken along a circular arc that stays
/// within `jd` mm of the corner, needs no more than the junction's
/// acceleration: v² = a·δ·sin(θ/2) / (1 − sin(θ/2)) for a corner angle θ.
/// Going straight back nearly stops, and short segments of a curve are also
/// held to the centripetal acceleration of the arc they approximate. (Marlin
/// 2 plans its junctions the same way, small-segment rule included.)
fn junction_deviation_speed(
    prev: &PrevBlock,
    unit: &[f64; 4],
    nominal: f64,
    accel: f64,
    length: f64,
    jd: f64,
    max_accel: &[f32; 4],
) -> f64 {
    if prev.nominal <= 1e-9 {
        return 0.0;
    }
    // Cosine of the angle between the two directions of travel.
    let turn_cos: f64 = prev.unit.iter().zip(unit).map(|(a, b)| a * b).sum();
    let speed_sqr = if turn_cos < -0.999999 {
        // Straight back the way it came.
        MIN_PLANNER_SPEED * MIN_PLANNER_SPEED
    } else {
        // The acceleration available in the direction the velocity changes.
        let change: [f64; 4] = std::array::from_fn(|i| unit[i] - prev.unit[i]);
        let size = change.iter().map(|c| c * c).sum::<f64>().sqrt();
        let accel = if size > 0.0 {
            limit_along(accel, &change.map(|c| c / size), max_accel)
        } else {
            accel
        };
        // sin(θ/2) for the corner angle θ (180° minus the turn); dead
        // straight would divide by zero, so stop just short of it.
        let sin_half = (0.5 * (1.0 + turn_cos.min(0.999999))).sqrt();
        let mut v_sqr = accel * jd * sin_half / (1.0 - sin_half);
        if length < 1.0 && turn_cos > std::f64::consts::FRAC_1_SQRT_2 {
            // A short segment turning by less than 45°: part of a curve.
            // Marlin's angle approximation never goes below ~0.0328 rad.
            let turn = turn_cos.clamp(-1.0, 1.0).acos().max(0.0328);
            v_sqr = v_sqr.min(length * accel / turn);
        }
        v_sqr
    };
    speed_sqr
        .min(nominal * nominal)
        .min(prev.nominal * prev.nominal)
        .max(0.0)
        .sqrt()
}

/// `value`, a speed or an acceleration along `direction` (a unit vector),
/// lowered until no axis's share of it exceeds that axis's maximum.
fn limit_along(value: f64, direction: &[f64; 4], max: &[f32; 4]) -> f64 {
    let mut value = value;
    for (share, &max) in direction.iter().map(|d| d.abs()).zip(max) {
        let max = f64::from(max);
        if share > 0.0 && max > 0.0 && value * share > max {
            value = max / share;
        }
    }
    value
}

/// Duration of a trapezoidal (or triangular) velocity profile over `length`
/// mm from `entry` to `exit` mm/s, cruising at most at `nominal`, with
/// constant acceleration `accel` mm/s².
pub fn trapezoid_time(length: f64, entry: f64, exit: f64, nominal: f64, accel: f64) -> f64 {
    trapezoid_profile(length, entry, exit, nominal, accel).0
}

/// (duration, peak speed) of a trapezoid profile; see [`trapezoid_time`].
fn trapezoid_profile(length: f64, entry: f64, exit: f64, nominal: f64, accel: f64) -> (f64, f64) {
    if !length.is_finite() || length <= 0.0 {
        return (0.0, 0.0);
    }
    let v0 = entry.max(0.0);
    let v1 = exit.max(0.0);
    let vn = nominal.max(v0).max(v1).max(MIN_NOMINAL_SPEED);
    if !accel.is_finite() || accel <= 0.0 {
        return (length / vn, vn);
    }
    let accel_dist = (vn * vn - v0 * v0) / (2.0 * accel);
    let decel_dist = (vn * vn - v1 * v1) / (2.0 * accel);
    if accel_dist + decel_dist <= length {
        let cruise = length - accel_dist - decel_dist;
        let t = (vn - v0) / accel + (vn - v1) / accel + cruise / vn;
        (t, vn)
    } else {
        let peak = ((2.0 * accel * length + v0 * v0 + v1 * v1) * 0.5).sqrt();
        if peak < v0.max(v1) {
            // Cannot change speed enough within the block: constant
            // acceleration between entry and exit.
            let avg = (v0 + v1) * 0.5;
            if avg > 0.0 {
                (length / avg, v0.max(v1))
            } else {
                (0.0, 0.0)
            }
        } else {
            ((peak - v0) / accel + (peak - v1) / accel, peak)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_line;

    fn limits_a1000() -> KinematicLimits {
        KinematicLimits {
            print_accel: 1000.0,
            travel_accel: 1000.0,
            retract_accel: 1000.0,
            ..KinematicLimits::default()
        }
    }

    fn run(planner: &mut Planner, moves: &[([f64; 4], f64)]) -> Vec<PlannedBlock> {
        let mut out = Vec::new();
        for (i, (d, f)) in moves.iter().enumerate() {
            if let Some(b) = planner.push(*d, *f, Some(i as u32)) {
                out.push(b);
            }
        }
        planner.flush(|b| out.push(b));
        out
    }

    fn total(blocks: &[PlannedBlock]) -> f64 {
        blocks.iter().map(|b| b.duration).sum()
    }

    #[test]
    fn single_move_trapezoid() {
        // 100 mm at F6000 (100 mm/s), a = 1000: 0.1 s up, 0.1 s down,
        // 5 mm each, 90 mm cruise at 100 mm/s = 0.9 s. Total 1.1 s.
        let mut p = Planner::new(limits_a1000());
        let b = run(&mut p, &[([100.0, 0.0, 0.0, 0.0], 100.0)]);
        assert_eq!(b.len(), 1);
        assert!((total(&b) - 1.1).abs() < 1e-9, "{}", total(&b));
        assert_eq!(b[0].entry_speed, 0.0);
        assert_eq!(b[0].exit_speed, 0.0);
        assert!((b[0].peak_speed - 100.0).abs() < 1e-9);
        assert_eq!(b[0].tag, Some(0));
    }

    #[test]
    fn short_move_triangle() {
        // 5 mm cannot reach 100 mm/s with a = 1000: peak sqrt(a d) = 70.71,
        // t = 2 sqrt(d / a) = 0.141421 s.
        let mut p = Planner::new(limits_a1000());
        let b = run(&mut p, &[([5.0, 0.0, 0.0, 0.0], 100.0)]);
        let expected = 2.0 * (5.0f64 / 1000.0).sqrt();
        assert!((total(&b) - expected).abs() < 1e-9);
        assert!((b[0].peak_speed - (5000.0f64).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn junction_deviation_corner() {
        // 50 mm X then 50 mm Y at 100 mm/s, a = 1000, JD 0.013.
        let lim = limits_a1000();
        let mut p = Planner::new(lim);
        let b = run(
            &mut p,
            &[
                ([50.0, 0.0, 0.0, 0.0], 100.0),
                ([0.0, 50.0, 0.0, 0.0], 100.0),
            ],
        );
        let sin_theta_d2 = 0.5f64.sqrt();
        let jd = f64::from(0.013f32);
        let vj = (1000.0 * jd * sin_theta_d2 / (1.0 - sin_theta_d2)).sqrt();
        assert!(
            (b[0].exit_speed - vj).abs() < 1e-9,
            "{} vs {vj}",
            b[0].exit_speed
        );
        assert!((b[1].entry_speed - vj).abs() < 1e-9);
        let one = trapezoid_time(50.0, 0.0, vj, 100.0, 1000.0);
        assert!((total(&b) - 2.0 * one).abs() < 1e-9);
        // Hand computation: 0.1 + (100 - vj)/1000 + cruise/100.
        let cruise = 50.0 - 5.0 - (100.0f64.powi(2) - vj * vj) / 2000.0;
        let hand = 0.1 + (100.0 - vj) / 1000.0 + cruise / 100.0;
        assert!((one - hand).abs() < 1e-12);
    }

    #[test]
    fn straight_continuation_keeps_speed() {
        let mut p = Planner::new(limits_a1000());
        let b = run(
            &mut p,
            &[
                ([50.0, 0.0, 0.0, 0.0], 100.0),
                ([50.0, 0.0, 0.0, 0.0], 100.0),
            ],
        );
        assert!((b[0].exit_speed - 100.0).abs() < 1e-9);
        assert!((total(&b) - 1.1).abs() < 1e-9);
    }

    #[test]
    fn reversal_nearly_stops() {
        let mut p = Planner::new(limits_a1000());
        let b = run(
            &mut p,
            &[
                ([50.0, 0.0, 0.0, 0.0], 100.0),
                ([-50.0, 0.0, 0.0, 0.0], 100.0),
            ],
        );
        assert!((b[0].exit_speed - MIN_PLANNER_SPEED).abs() < 1e-9);
    }

    #[test]
    fn classic_jerk_corner() {
        let lim = KinematicLimits {
            junction_deviation: None,
            ..limits_a1000()
        };
        let mut p = Planner::new(lim);
        let b = run(
            &mut p,
            &[
                ([50.0, 0.0, 0.0, 0.0], 100.0),
                ([0.0, 50.0, 0.0, 0.0], 100.0),
            ],
        );
        // X stops (jerk 100 -> limited to 8), Y starts: junction 8 mm/s.
        assert!((b[0].exit_speed - 8.0).abs() < 1e-9, "{}", b[0].exit_speed);
        // From rest classic jerk allows starting at the jerk speed.
        assert!((b[0].entry_speed - 8.0).abs() < 1e-9);
    }

    #[test]
    fn per_axis_feedrate_limit() {
        // Z limited to 12 mm/s.
        let mut p = Planner::new(limits_a1000());
        let b = run(&mut p, &[([0.0, 0.0, 10.0, 0.0], 100.0)]);
        // a_z limited to 100 mm/s²: 0.12 s up, 0.72 mm each side.
        let expected = trapezoid_time(10.0, 0.0, 0.0, 12.0, 100.0);
        assert!((total(&b) - expected).abs() < 1e-9);
    }

    #[test]
    fn e_only_moves_use_retract_accel() {
        let lim = KinematicLimits {
            retract_accel: 1250.0,
            ..KinematicLimits::default()
        };
        let mut p = Planner::new(lim);
        let b = run(&mut p, &[([0.0, 0.0, 0.0, -0.8], 35.0)]);
        let expected = trapezoid_time(0.8, 0.0, 0.0, 35.0, 1250.0);
        assert!((total(&b) - expected).abs() < 1e-12);
        // E feedrate is capped by max_feedrate[E].
        let mut p = Planner::new(KinematicLimits::default());
        let b = run(&mut p, &[([0.0, 0.0, 0.0, 600.0], 1000.0)]);
        assert!((b[0].peak_speed - 120.0).abs() < 1e-9);
    }

    #[test]
    fn window_limits_long_runs_of_short_segments() {
        // 1000 collinear 1 mm segments at 100 mm/s with a = 100: with a
        // 16-block window the planner must always be able to stop within the
        // window (~16 mm), capping the speed near sqrt(2 a d) = 57 mm/s, while
        // unlimited look-ahead plans one 1000 mm trapezoid (11 s).
        let lim = KinematicLimits {
            print_accel: 100.0,
            travel_accel: 100.0,
            ..KinematicLimits::default()
        };
        let moves: Vec<_> = (0..1000).map(|_| ([1.0, 0.0, 0.0, 0.0], 100.0)).collect();
        let mut unlimited = Planner::with_window(lim, 100_000);
        let fast = total(&run(&mut unlimited, &moves));
        assert!((fast - 11.0).abs() < 1e-6, "{fast}");
        let mut windowed = Planner::new(lim);
        let blocks = run(&mut windowed, &moves);
        let slow = total(&blocks);
        let vmax = (2.0f64 * 100.0 * 16.0).sqrt();
        assert!(blocks.iter().all(|b| b.peak_speed <= vmax + 1e-9));
        assert!(slow > 1000.0 / vmax, "slow {slow}");
        assert!(slow > fast * 1.5);
    }

    #[test]
    fn tiny_segments_follow_marlin_small_segment_rule() {
        // Collinear 0.1 mm segments: Marlin's JD_HANDLE_SMALL_SEGMENTS caps the
        // junction at sqrt(d a / 0.0328).
        let moves: Vec<_> = (0..100).map(|_| ([0.1, 0.0, 0.0, 0.0], 100.0)).collect();
        let mut p = Planner::new(limits_a1000());
        let blocks = run(&mut p, &moves);
        let cap = (0.1f64 * 1000.0 / 0.0328).sqrt();
        assert!(blocks.iter().all(|b| b.entry_speed <= cap + 1e-9));
        assert!(blocks[50].entry_speed > cap * 0.99);
    }

    #[test]
    fn window_of_one_stops_every_move() {
        let mut p = Planner::with_window(limits_a1000(), 1);
        let b = run(
            &mut p,
            &[
                ([50.0, 0.0, 0.0, 0.0], 100.0),
                ([50.0, 0.0, 0.0, 0.0], 100.0),
            ],
        );
        assert!(
            b.iter()
                .all(|b| b.entry_speed == 0.0 && b.exit_speed == 0.0)
        );
        let one = trapezoid_time(50.0, 0.0, 0.0, 100.0, 1000.0);
        assert!((total(&b) - 2.0 * one).abs() < 1e-9);
    }

    #[test]
    fn profiles_are_continuous_and_feasible() {
        // Zig-zag of varied segments: exit of one block equals the entry of
        // the next and every profile is reachable.
        let mut moves = Vec::new();
        for i in 0..200 {
            let a = i as f64 * 0.37;
            let len = 0.05 + (i % 7) as f64 * 0.8;
            moves.push((
                [a.cos() * len, a.sin() * len, 0.0, len * 0.03],
                20.0 + (i % 5) as f64 * 30.0,
            ));
        }
        let mut p = Planner::new(KinematicLimits::default());
        let blocks = run(&mut p, &moves);
        assert_eq!(blocks.len(), moves.len());
        for w in blocks.windows(2) {
            assert!((w[0].exit_speed - w[1].entry_speed).abs() < 1e-9);
        }
        for b in &blocks {
            assert!(b.duration.is_finite() && b.duration > 0.0);
            assert!(b.peak_speed + 1e-9 >= b.entry_speed.max(b.exit_speed));
        }
        assert_eq!(blocks.last().map(|b| b.exit_speed), Some(0.0));
    }

    #[test]
    fn degenerate_inputs() {
        let mut p = Planner::new(KinematicLimits::default());
        assert_eq!(p.push([0.0; 4], 100.0, None), None);
        assert_eq!(p.push([f64::NAN, 0.0, 0.0, 0.0], 100.0, None), None);
        assert_eq!(p.push([f64::INFINITY, 0.0, 0.0, 0.0], 100.0, None), None);
        let b = run(&mut p, &[([1.0, 0.0, 0.0, 0.0], 0.0)]);
        assert!(b[0].duration.is_finite() && b[0].duration > 0.0);
        let b = run(&mut p, &[([1.0, 0.0, 0.0, 0.0], f64::NAN)]);
        assert!(b[0].duration.is_finite());
        let zero = KinematicLimits {
            print_accel: 0.0,
            travel_accel: 0.0,
            max_accel: [0.0; 4],
            max_feedrate: [0.0; 4],
            ..KinematicLimits::default()
        };
        let mut p = Planner::new(zero);
        let b = run(&mut p, &[([10.0, 0.0, 0.0, 0.0], 50.0)]);
        assert!(b[0].duration.is_finite() && b[0].duration > 0.0);
        assert_eq!(trapezoid_time(0.0, 0.0, 0.0, 10.0, 100.0), 0.0);
        assert_eq!(trapezoid_time(-1.0, 0.0, 0.0, 10.0, 100.0), 0.0);
        assert!((trapezoid_time(10.0, 0.0, 0.0, 10.0, 0.0) - 1.0).abs() < 1e-12);
        // Infeasible deceleration falls back to constant acceleration.
        assert!((trapezoid_time(1.0, 100.0, 0.0, 100.0, 10.0) - 0.02).abs() < 1e-12);
    }

    #[test]
    fn gcode_limit_commands() {
        let mut l = KinematicLimits::default();
        assert!(l.apply_gcode(&parse_line("M201 X1000 Y1100 Z200 E5000")));
        assert_eq!(l.max_accel, [1000.0, 1100.0, 200.0, 5000.0]);
        assert!(l.apply_gcode(&parse_line("M203 X200 Y200 Z12 E120")));
        assert_eq!(l.max_feedrate, [200.0, 200.0, 12.0, 120.0]);
        assert!(l.apply_gcode(&parse_line("M204 P1250 R1500 T2000")));
        assert_eq!(
            (l.print_accel, l.retract_accel, l.travel_accel),
            (1250.0, 1500.0, 2000.0)
        );
        assert!(l.apply_gcode(&parse_line("M204 S800")));
        assert_eq!((l.print_accel, l.travel_accel), (800.0, 800.0));
        assert!(l.apply_gcode(&parse_line("M204 S1000 T900")));
        assert_eq!(
            (l.print_accel, l.travel_accel, l.retract_accel),
            (1000.0, 1000.0, 900.0)
        );
        assert!(l.apply_gcode(&parse_line("M205 X10 Y9 Z0.6 E4.5 J0.02 S1")));
        assert_eq!(l.jerk, [10.0, 9.0, 0.6, 4.5]);
        assert_eq!(l.junction_deviation, Some(0.02));
        assert_eq!(l.min_feedrate, 1.0);
        // Marlin rejects J outside 0.01..=0.3 and keeps the old value: J0
        // would otherwise stop the planner at every junction.
        for j in ["J0", "J0.005", "J0.31", "J-1"] {
            assert!(l.apply_gcode(&parse_line(&format!("M205 {j}"))));
            assert_eq!(l.junction_deviation, Some(0.02), "{j}");
        }
        assert!(l.apply_gcode(&parse_line("M205 J0.3")));
        assert_eq!(l.junction_deviation, Some(0.3));
        let before = l;
        assert!(l.apply_gcode(&parse_line("M201 X0 Y-5")));
        assert_eq!(l, before);
        assert!(!l.apply_gcode(&parse_line("M104 S200")));
        assert!(!l.apply_gcode(&parse_line("G1 X1")));
        assert!(!l.apply_gcode(&parse_line("; c")));
    }

    #[test]
    fn klipper_velocity_limit() {
        let mut l = KinematicLimits::default();
        let cmd = ExtendedCommand::parse(
            "SET_VELOCITY_LIMIT VELOCITY=300 ACCEL=5000 SQUARE_CORNER_VELOCITY=5",
        );
        assert!(cmd.is_some_and(|c| l.apply_extended(&c)));
        assert_eq!(l.max_feedrate[0], 300.0);
        assert_eq!(l.print_accel, 5000.0);
        let jd = l.junction_deviation.unwrap_or_default();
        assert!((f64::from(jd) - 25.0 * (2f64.sqrt() - 1.0) / 5000.0).abs() < 1e-6);
        let other = ExtendedCommand::parse("SET_FAN_SPEED SPEED=1");
        assert!(!other.is_some_and(|c| l.apply_extended(&c)));
    }

    #[test]
    fn limits_serde_round_trip() {
        let l = KinematicLimits {
            junction_deviation: None,
            ..KinematicLimits::default()
        };
        let json = serde_json::to_string(&l).unwrap_or_default();
        let back: KinematicLimits = serde_json::from_str(&json).unwrap_or_default();
        assert_eq!(back, l);
        let partial: KinematicLimits =
            serde_json::from_str(r#"{"print_accel": 900}"#).unwrap_or_default();
        assert_eq!(partial.print_accel, 900.0);
        assert_eq!(partial.travel_accel, 1500.0);
    }
}
