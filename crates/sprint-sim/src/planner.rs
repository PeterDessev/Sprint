//! Motion planner timing model.
//!
//! Moves are queued as blocks with trapezoidal velocity profiles. Junction
//! speeds come from Marlin's junction-deviation formula and a backward /
//! forward look-ahead pass over the blocks that have not started yet, so
//! streams of short segments flow at a realistic speed. The head block's
//! profile is fixed once it starts executing.

use std::collections::VecDeque;

/// Marlin `MINIMUM_PLANNER_SPEED` in mm/s.
const MIN_PLANNER_SPEED: f64 = 0.05;

/// Kinematic limits used to plan a block (M201/M203/M204/M205).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Limits {
    /// Per-axis maximum feedrate in mm/s (X Y Z E).
    pub max_feedrate: [f64; 4],
    /// Per-axis maximum acceleration in mm/s² (X Y Z E).
    pub max_accel: [f64; 4],
    /// Acceleration for printing moves.
    pub print_accel: f64,
    /// Acceleration for E-only moves (retract/recover).
    pub retract_accel: f64,
    /// Acceleration for travel moves.
    pub travel_accel: f64,
    /// Junction deviation in mm.
    pub junction_deviation: f64,
    /// Minimum printing feedrate in mm/s.
    pub min_feedrate: f64,
    /// Minimum travel feedrate in mm/s.
    pub min_travel_feedrate: f64,
}

/// Velocity profile of a started block.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Profile {
    v0: f64,
    peak: f64,
    accel: f64,
    t_acc: f64,
    t_cruise: f64,
    t_dec: f64,
}

impl Profile {
    fn plan(v0: f64, v1: f64, nominal: f64, accel: f64, length: f64) -> Profile {
        let a = accel.max(1e-6);
        let v0 = v0.max(0.0);
        let v1 = v1.max(0.0);
        let nominal = nominal.max(v0).max(v1).max(MIN_PLANNER_SPEED);
        let d_acc = (nominal * nominal - v0 * v0) / (2.0 * a);
        let d_dec = (nominal * nominal - v1 * v1) / (2.0 * a);
        if d_acc + d_dec <= length {
            Profile {
                v0,
                peak: nominal,
                accel: a,
                t_acc: (nominal - v0) / a,
                t_cruise: (length - d_acc - d_dec) / nominal,
                t_dec: (nominal - v1) / a,
            }
        } else {
            let peak = ((2.0 * a * length + v0 * v0 + v1 * v1) / 2.0)
                .sqrt()
                .max(v0)
                .max(v1)
                .max(MIN_PLANNER_SPEED);
            Profile {
                v0,
                peak,
                accel: a,
                t_acc: ((peak - v0) / a).max(0.0),
                t_cruise: 0.0,
                t_dec: ((peak - v1) / a).max(0.0),
            }
        }
    }

    fn duration(&self) -> f64 {
        self.t_acc + self.t_cruise + self.t_dec
    }

    /// Distance travelled after `t` seconds.
    fn distance_at(&self, t: f64) -> f64 {
        let a = self.accel;
        let t1 = t.min(self.t_acc).max(0.0);
        let mut d = self.v0 * t1 + 0.5 * a * t1 * t1;
        if t > self.t_acc {
            let t2 = (t - self.t_acc).min(self.t_cruise);
            d += self.peak * t2;
            if t > self.t_acc + self.t_cruise {
                let t3 = (t - self.t_acc - self.t_cruise).min(self.t_dec);
                d += self.peak * t3 - 0.5 * a * t3 * t3;
            }
        }
        d
    }
}

/// Duration of a trapezoidal move.
#[cfg(test)]
fn trapezoid_time(v0: f64, v1: f64, nominal: f64, accel: f64, length: f64) -> f64 {
    Profile::plan(v0, v1, nominal, accel, length).duration()
}

/// One planned move.
#[derive(Clone, Debug)]
struct Block {
    start: [f64; 4],
    end: [f64; 4],
    length: f64,
    unit: [f64; 4],
    nominal: f64,
    accel: f64,
    max_entry: f64,
    entry: f64,
    exit: f64,
    profile: Profile,
    duration: f64,
    elapsed: f64,
    started: bool,
}

impl Block {
    fn fraction_done(&self) -> f64 {
        if !self.started || self.length <= 0.0 {
            return 0.0;
        }
        (self.profile.distance_at(self.elapsed) / self.length).clamp(0.0, 1.0)
    }
}

/// The kind of move, which selects the acceleration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MoveKind {
    /// XY(Z) move with extrusion.
    Print,
    /// Move without extrusion.
    Travel,
    /// E-only move.
    Retract,
}

/// The planner: a bounded queue of blocks executed in simulated time.
#[derive(Clone, Debug)]
pub(crate) struct Planner {
    blocks: VecDeque<Block>,
    capacity: usize,
    instant: bool,
    /// Machine position at the end of the last finished block.
    position: [f64; 4],
}

impl Planner {
    /// An empty planner holding up to `capacity` blocks.
    pub(crate) fn new(capacity: usize, instant: bool) -> Planner {
        Planner {
            blocks: VecDeque::new(),
            capacity: capacity.max(1),
            instant,
            position: [0.0; 4],
        }
    }

    /// Number of queued blocks.
    pub(crate) fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether nothing is queued or executing.
    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Free block slots.
    pub(crate) fn free(&self) -> usize {
        self.capacity.saturating_sub(self.blocks.len())
    }

    /// Whether a block can be queued now.
    pub(crate) fn has_room(&self) -> bool {
        self.blocks.len() < self.capacity
    }

    /// Set the machine position directly (homing, G92 on physical axes,
    /// quickstop). Only valid while the planner is empty.
    pub(crate) fn set_position(&mut self, position: [f64; 4]) {
        self.position = position;
    }

    /// The physical tool position right now (interpolated within the
    /// executing block).
    pub(crate) fn current_position(&self) -> [f64; 4] {
        match self.blocks.front() {
            Some(b) if b.started => {
                let f = b.fraction_done();
                let mut p = [0.0; 4];
                for (i, v) in p.iter_mut().enumerate() {
                    *v = b.start[i] + (b.end[i] - b.start[i]) * f;
                }
                p
            }
            Some(b) => b.start,
            None => self.position,
        }
    }

    /// Queue a move from the end of the previous block to `end` at
    /// `feedrate` mm/s. Returns false (and queues nothing) when full.
    pub(crate) fn push(
        &mut self,
        end: [f64; 4],
        feedrate: f64,
        kind: MoveKind,
        limits: &Limits,
    ) -> bool {
        if !self.has_room() {
            return false;
        }
        if end.iter().any(|v| !v.is_finite()) {
            // Never let garbage coordinates poison the position.
            return true;
        }
        let start = self.blocks.back().map_or(self.position, |b| b.end);
        let mut delta = [0.0; 4];
        for i in 0..4 {
            delta[i] = end[i] - start[i];
        }
        let xyz = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
        let (length, unit) = if xyz > 1e-6 {
            (
                xyz,
                [
                    delta[0] / xyz,
                    delta[1] / xyz,
                    delta[2] / xyz,
                    delta[3] / xyz,
                ],
            )
        } else if delta[3].abs() > 1e-6 {
            let l = delta[3].abs();
            (l, [0.0, 0.0, 0.0, delta[3].signum()])
        } else {
            // Zero-length move: nothing to do.
            if self.blocks.is_empty() {
                self.position = end;
            } else if let Some(b) = self.blocks.back_mut() {
                b.end = end;
            }
            return true;
        };
        if self.instant || !length.is_finite() {
            self.position = end;
            self.blocks.clear();
            return true;
        }
        let min_feed = if kind == MoveKind::Travel {
            limits.min_travel_feedrate
        } else {
            limits.min_feedrate
        };
        let mut nominal = if feedrate.is_finite() { feedrate } else { 0.0 };
        nominal = nominal.max(min_feed).max(MIN_PLANNER_SPEED);
        let mut accel = match kind {
            MoveKind::Print => limits.print_accel,
            MoveKind::Travel => limits.travel_accel,
            MoveKind::Retract => limits.retract_accel,
        };
        for (i, u) in unit.iter().map(|u| u.abs()).enumerate() {
            if u > 1e-9 {
                let max_f = limits.max_feedrate[i].max(MIN_PLANNER_SPEED);
                if nominal * u > max_f {
                    nominal = max_f / u;
                }
                let max_a = limits.max_accel[i].max(1.0);
                if accel * u > max_a {
                    accel = max_a / u;
                }
            }
        }
        accel = accel.max(1.0);
        let max_entry = match self.blocks.back() {
            None => 0.0,
            Some(prev) => junction_speed(prev, &unit, nominal, accel, limits.junction_deviation),
        };
        self.blocks.push_back(Block {
            start,
            end,
            length,
            unit,
            nominal,
            accel,
            max_entry,
            entry: 0.0,
            exit: 0.0,
            profile: Profile::default(),
            duration: 0.0,
            elapsed: 0.0,
            started: false,
        });
        self.replan();
        true
    }

    /// Look-ahead over the blocks that have not started.
    fn replan(&mut self) {
        let n = self.blocks.len();
        let Some(first) = self.blocks.iter().position(|b| !b.started) else {
            return;
        };
        let fixed_entry = if first == 0 {
            0.0
        } else {
            self.blocks[first - 1].exit
        };
        // Backward pass: every block must be able to stop at the end.
        let mut next_entry = 0.0;
        for i in (first..n).rev() {
            let b = &mut self.blocks[i];
            let reachable = (next_entry * next_entry + 2.0 * b.accel * b.length).sqrt();
            b.entry = b.max_entry.min(reachable).min(b.nominal);
            next_entry = b.entry;
        }
        // Forward pass: respect the acceleration from the fixed entry.
        let mut prev_exit = fixed_entry;
        for i in first..n {
            let next = if i + 1 < n {
                self.blocks[i + 1].entry
            } else {
                0.0
            };
            let b = &mut self.blocks[i];
            b.entry = if i == first {
                fixed_entry
            } else {
                b.entry.min(prev_exit)
            };
            let reachable = (b.entry * b.entry + 2.0 * b.accel * b.length).sqrt();
            b.exit = next.min(reachable);
            prev_exit = b.exit;
        }
    }

    fn start_head(&mut self) {
        let next_entry = self.blocks.get(1).map(|b| b.entry);
        if let Some(b) = self.blocks.front_mut() {
            if b.started {
                return;
            }
            let reachable = (b.entry * b.entry + 2.0 * b.accel * b.length).sqrt();
            b.exit = next_entry.unwrap_or(0.0).min(reachable);
            b.profile = Profile::plan(b.entry, b.exit, b.nominal, b.accel, b.length);
            b.duration = b.profile.duration();
            b.elapsed = 0.0;
            b.started = true;
        }
        if let (Some(exit), Some(next)) =
            (self.blocks.front().map(|b| b.exit), self.blocks.get_mut(1))
        {
            next.entry = exit;
        }
    }

    /// Advance execution by `dt` seconds. Returns the extrusion (positive
    /// E travel) of the blocks that finished.
    pub(crate) fn advance(&mut self, dt: f64) -> f64 {
        let mut remaining = dt.max(0.0);
        let mut extruded = 0.0;
        // Bound the work per call even if a caller passes a huge dt.
        let mut guard = 0usize;
        while remaining > 0.0 && guard < 100_000 {
            guard += 1;
            self.start_head();
            let Some(b) = self.blocks.front_mut() else {
                break;
            };
            let left = b.duration - b.elapsed;
            if remaining >= left {
                remaining -= left.max(0.0);
                let de = b.end[3] - b.start[3];
                if de > 0.0 {
                    extruded += de;
                }
                self.position = b.end;
                self.blocks.pop_front();
            } else {
                b.elapsed += remaining;
                remaining = 0.0;
            }
        }
        extruded
    }

    /// Quickstop: drop every block and stop where the tool is now.
    pub(crate) fn quick_stop(&mut self) -> [f64; 4] {
        let p = self.current_position();
        self.blocks.clear();
        self.position = p;
        p
    }

    /// The position at the end of the last queued block.
    pub(crate) fn target(&self) -> [f64; 4] {
        self.blocks.back().map_or(self.position, |b| b.end)
    }
}

fn junction_speed(prev: &Block, unit: &[f64; 4], nominal: f64, accel: f64, jd: f64) -> f64 {
    let dot: f64 = (0..4).map(|i| prev.unit[i] * unit[i]).sum();
    // Ignore E in the junction angle for XYZ moves (Marlin uses XYZ).
    let xyz_dot: f64 = (0..3).map(|i| prev.unit[i] * unit[i]).sum();
    let both_xyz =
        prev.unit[..3].iter().any(|v| v.abs() > 0.0) && unit[..3].iter().any(|v| v.abs() > 0.0);
    let cos_theta = -(if both_xyz {
        let n1: f64 = prev.unit[..3].iter().map(|v| v * v).sum::<f64>().sqrt();
        let n2: f64 = unit[..3].iter().map(|v| v * v).sum::<f64>().sqrt();
        if n1 > 0.0 && n2 > 0.0 {
            xyz_dot / (n1 * n2)
        } else {
            dot
        }
    } else {
        dot
    });
    let limit = nominal.min(prev.nominal);
    if cos_theta > 0.999_999 {
        return MIN_PLANNER_SPEED.min(limit);
    }
    let cos_theta = cos_theta.max(-0.999_999);
    let sin_theta_d2 = (0.5 * (1.0 - cos_theta)).sqrt();
    let denom = 1.0 - sin_theta_d2;
    let v2 = if denom > 1e-12 {
        accel * jd.max(0.0) * sin_theta_d2 / denom
    } else {
        f64::INFINITY
    };
    v2.sqrt().min(limit).max(MIN_PLANNER_SPEED.min(limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            max_feedrate: [300.0, 300.0, 5.0, 25.0],
            max_accel: [3000.0, 3000.0, 100.0, 10_000.0],
            print_accel: 3000.0,
            retract_accel: 3000.0,
            travel_accel: 3000.0,
            junction_deviation: 0.013,
            min_feedrate: 0.0,
            min_travel_feedrate: 0.0,
        }
    }

    fn run_to_end(p: &mut Planner) -> f64 {
        let mut t = 0.0;
        while !p.is_empty() {
            p.advance(0.001);
            t += 0.001;
            assert!(t < 10_000.0);
        }
        t
    }

    #[test]
    fn trapezoid_matches_hand_calculation() {
        // 100 mm at 100 mm/s, 3000 mm/s²: 1 s cruise + 2 * (v/a)/2 extra.
        let t = trapezoid_time(0.0, 0.0, 100.0, 3000.0, 100.0);
        assert!((t - (1.0 + 100.0 / 3000.0)).abs() < 1e-9, "{t}");
        // Triangle: too short to reach nominal speed.
        let t = trapezoid_time(0.0, 0.0, 1000.0, 1000.0, 1.0);
        assert!((t - 2.0 * (1.0f64 / 1000.0).sqrt()).abs() < 1e-9, "{t}");
    }

    #[test]
    fn single_move_takes_expected_time() {
        let mut p = Planner::new(16, false);
        assert!(p.push([100.0, 0.0, 0.0, 0.0], 100.0, MoveKind::Travel, &limits()));
        let t = run_to_end(&mut p);
        assert!((t - 1.0333).abs() < 0.01, "{t}");
        assert_eq!(p.current_position(), [100.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn axis_limits_cap_speed() {
        let mut p = Planner::new(16, false);
        // Z is limited to 5 mm/s: 10 mm takes at least 2 s.
        p.push([0.0, 0.0, 10.0, 0.0], 100.0, MoveKind::Travel, &limits());
        let t = run_to_end(&mut p);
        assert!(t > 2.0 && t < 2.2, "{t}");
    }

    #[test]
    fn collinear_segments_flow_without_stopping() {
        let mut p = Planner::new(16, false);
        for i in 1..=10 {
            p.push(
                [f64::from(i) * 10.0, 0.0, 0.0, 0.0],
                100.0,
                MoveKind::Travel,
                &limits(),
            );
        }
        let t = run_to_end(&mut p);
        // Same as one 100 mm move, not ten stops.
        assert!((t - 1.0333).abs() < 0.02, "{t}");
    }

    #[test]
    fn corners_slow_down() {
        let mut p = Planner::new(16, false);
        for i in 0..10 {
            let x = if i % 2 == 0 { 10.0 } else { 0.0 };
            p.push(
                [x, f64::from(i + 1) * 0.0001, 0.0, 0.0],
                100.0,
                MoveKind::Travel,
                &limits(),
            );
        }
        let t = run_to_end(&mut p);
        // Ten reversals: each 10 mm segment stops at both ends.
        let single = trapezoid_time(0.0, 0.0, 100.0, 3000.0, 10.0);
        assert!(t > single * 9.5, "{t} vs {single}");
    }

    #[test]
    fn full_planner_rejects_and_drains() {
        let mut p = Planner::new(2, false);
        assert!(p.push([1.0, 0.0, 0.0, 0.0], 10.0, MoveKind::Travel, &limits()));
        assert!(p.push([2.0, 0.0, 0.0, 0.0], 10.0, MoveKind::Travel, &limits()));
        assert!(!p.has_room());
        assert!(!p.push([3.0, 0.0, 0.0, 0.0], 10.0, MoveKind::Travel, &limits()));
        assert_eq!(p.free(), 0);
        run_to_end(&mut p);
        assert_eq!(p.target(), [2.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn quick_stop_stops_midway() {
        let mut p = Planner::new(4, false);
        p.push([100.0, 0.0, 0.0, 0.0], 100.0, MoveKind::Travel, &limits());
        p.advance(0.5);
        let pos = p.quick_stop();
        assert!(pos[0] > 30.0 && pos[0] < 70.0, "{pos:?}");
        assert!(p.is_empty());
        assert_eq!(p.current_position(), pos);
    }

    #[test]
    fn instant_mode_and_zero_moves() {
        let mut p = Planner::new(4, true);
        assert!(p.push([100.0, 50.0, 0.0, 0.0], 100.0, MoveKind::Travel, &limits()));
        assert!(p.is_empty());
        assert_eq!(p.current_position(), [100.0, 50.0, 0.0, 0.0]);
        let mut p = Planner::new(4, false);
        assert!(p.push([0.0, 0.0, 0.0, 0.0], 100.0, MoveKind::Travel, &limits()));
        assert!(p.is_empty());
        assert!(p.push(
            [f64::NAN, 0.0, 0.0, 0.0],
            100.0,
            MoveKind::Travel,
            &limits()
        ));
        assert!(p.push([1.0, 0.0, 0.0, 0.0], f64::NAN, MoveKind::Travel, &limits()));
        run_to_end(&mut p);
    }

    #[test]
    fn e_only_moves_use_e_limits() {
        let mut p = Planner::new(4, false);
        // 50 mm of E at most 25 mm/s: at least 2 s.
        p.push([0.0, 0.0, 0.0, 50.0], 1000.0, MoveKind::Retract, &limits());
        let t = run_to_end(&mut p);
        assert!(t > 2.0 && t < 2.1, "{t}");
    }
}
