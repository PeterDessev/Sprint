//! Thermal model: heaters with heat capacity, heater power and losses to
//! ambient, a lagging temperature sensor, and Marlin's PID controller.
//!
//! The plant per heater is
//!
//! * heater element: power follows the commanded PWM with a lag
//!   (`heater_tau`), modelling the cartridge/pad heating up;
//! * block: `C dT/dt = P - h (T - ambient)` (first order);
//! * sensor: follows the block with a lag (`sensor_tau`).
//!
//! The two small lags make the relay oscillation during PID autotune and
//! the overshoot after heat-up look like a real printer.

use crate::rng::Rng;

/// Marlin `PID_FUNCTIONAL_RANGE`: outside this error band the heater is
/// driven fully on/off and the integral is reset.
pub(crate) const PID_FUNCTIONAL_RANGE: f64 = 10.0;
/// Marlin `PID_K1` (derivative smoothing).
const PID_K1: f64 = 0.95;
/// PID sample period in simulated seconds (Marlin's `PID_dT` is ~0.1 s).
pub(crate) const PID_DT: f64 = 0.1;
/// Maximum PWM value (`PID_MAX` / `MAX_BED_POWER`).
pub(crate) const PWM_MAX: f64 = 255.0;

/// Physical parameters of one heater.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HeaterParams {
    /// Heat capacity of the block in J/K.
    pub capacity: f64,
    /// Heater power at full PWM in W.
    pub max_power: f64,
    /// Loss to ambient in W/K.
    pub loss: f64,
    /// Additional loss at 100 % part-cooling fan in W/K.
    pub fan_loss: f64,
    /// Heater element lag in s.
    pub heater_tau: f64,
    /// Sensor lag in s.
    pub sensor_tau: f64,
    /// Peak-to-peak sensor noise in °C.
    pub noise: f64,
    /// Highest settable target (MAXTEMP minus overshoot margin).
    pub max_target: f64,
}

impl HeaterParams {
    /// A typical 40 W E3D-style hotend.
    pub(crate) fn hotend() -> HeaterParams {
        HeaterParams {
            capacity: 9.0,
            max_power: 40.0,
            loss: 0.095,
            fan_loss: 0.04,
            heater_tau: 4.0,
            sensor_tau: 3.5,
            noise: 0.12,
            max_target: 260.0,
        }
    }

    /// A 240 W aluminium bed (235 x 235 mm class).
    pub(crate) fn bed() -> HeaterParams {
        HeaterParams {
            capacity: 600.0,
            max_power: 240.0,
            loss: 2.0,
            fan_loss: 0.0,
            heater_tau: 12.0,
            sensor_tau: 6.0,
            noise: 0.08,
            max_target: 140.0,
        }
    }

    /// An enclosure heater.
    pub(crate) fn chamber() -> HeaterParams {
        HeaterParams {
            capacity: 2500.0,
            max_power: 300.0,
            loss: 6.0,
            fan_loss: 0.0,
            heater_tau: 20.0,
            sensor_tau: 15.0,
            noise: 0.06,
            max_target: 60.0,
        }
    }
}

/// How a heater is controlled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    /// Marlin PID (PIDTEMP / PIDTEMPBED).
    Pid,
    /// Bang-bang with hysteresis (chamber).
    BangBang,
}

/// PID gains as entered by the user (M301/M304 units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Gains {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
}

/// One simulated heater with its controller.
#[derive(Clone, Debug)]
pub(crate) struct Heater {
    params: HeaterParams,
    control: Control,
    block: f64,
    sensor: f64,
    power: f64,
    /// Last measured value (sensor + noise), as reported.
    measured: f64,
    /// Target in °C (0 = off).
    pub target: f64,
    /// PWM output 0..=255.
    output: f64,
    /// Relay output used by autotune; overrides the controller.
    pub forced_output: Option<f64>,
    i_state: f64,
    d_state: f64,
    d_term: f64,
    pid_reset: bool,
    sample_timer: f64,
}

impl Heater {
    /// A heater at ambient temperature.
    pub(crate) fn new(params: HeaterParams, control: Control, ambient: f64) -> Heater {
        Heater {
            params,
            control,
            block: ambient,
            sensor: ambient,
            power: 0.0,
            measured: ambient,
            target: 0.0,
            output: 0.0,
            forced_output: None,
            i_state: 0.0,
            d_state: ambient,
            d_term: 0.0,
            pid_reset: true,
            sample_timer: 0.0,
        }
    }

    /// Reported temperature.
    pub(crate) fn temp(&self) -> f64 {
        self.measured
    }

    /// Heater parameters.
    pub(crate) fn params(&self) -> &HeaterParams {
        &self.params
    }

    /// Current PWM (0..=255).
    pub(crate) fn pwm(&self) -> f64 {
        self.output
    }

    /// Marlin's reported power (`@:`), 0..=127.
    pub(crate) fn reported_power(&self) -> u32 {
        (self.output.clamp(0.0, PWM_MAX) as u32) >> 1
    }

    /// Set the target, clamped to the heater's range.
    pub(crate) fn set_target(&mut self, target: f64) {
        self.target = if target.is_finite() {
            target.clamp(0.0, self.params.max_target)
        } else {
            0.0
        };
    }

    /// Turn the heater off (target 0, no forced output).
    pub(crate) fn off(&mut self) {
        self.target = 0.0;
        self.forced_output = None;
        self.output = 0.0;
        self.pid_reset = true;
    }

    /// Advance the plant by `dt` seconds; runs the controller every
    /// [`PID_DT`]. Returns true when a new sample was taken.
    pub(crate) fn step(
        &mut self,
        dt: f64,
        ambient: f64,
        fan: f64,
        gains: Gains,
        rng: &mut Rng,
    ) -> bool {
        let mut sampled = false;
        self.sample_timer += dt;
        if self.sample_timer >= PID_DT {
            self.sample_timer -= PID_DT;
            if self.sample_timer > PID_DT {
                self.sample_timer = 0.0;
            }
            let noise = (rng.next_f64() - 0.5) * self.params.noise;
            self.measured = self.sensor + noise;
            self.output = match self.forced_output {
                Some(v) => v.clamp(0.0, PWM_MAX),
                None => self.controller(gains),
            };
            sampled = true;
        }
        let p = &self.params;
        let commanded = self.output / PWM_MAX * p.max_power;
        self.power += (commanded - self.power) * (dt / p.heater_tau).min(1.0);
        let loss = p.loss + p.fan_loss * fan.clamp(0.0, 1.0);
        self.block += (self.power - loss * (self.block - ambient)) * dt / p.capacity;
        self.sensor += (self.block - self.sensor) * (dt / p.sensor_tau).min(1.0);
        sampled
    }

    fn controller(&mut self, gains: Gains) -> f64 {
        let current = self.measured;
        let out = match self.control {
            Control::BangBang => {
                if self.target <= 0.0 {
                    0.0
                } else if current < self.target - 1.0 {
                    PWM_MAX
                } else if current > self.target + 1.0 {
                    0.0
                } else {
                    self.output
                }
            }
            Control::Pid => self.pid(current, gains),
        };
        self.d_state = current;
        out.clamp(0.0, PWM_MAX)
    }

    /// Heater output from the PID gains: full power while the reading is
    /// more than `PID_FUNCTIONAL_RANGE` below the target, off when it is that
    /// far above (or there is no target), and in between the sum of the
    /// proportional term, an integral that cannot wind up past full power
    /// and a smoothed derivative of the reading. Marlin 2.1 drives a PID
    /// hotend or bed the same way.
    fn pid(&mut self, current: f64, gains: Gains) -> f64 {
        let error = self.target - current;
        let saturated = if self.target <= 0.0 || error < -PID_FUNCTIONAL_RANGE {
            Some(0.0)
        } else if error > PID_FUNCTIONAL_RANGE {
            Some(PWM_MAX)
        } else {
            None
        };
        if let Some(output) = saturated {
            self.pid_reset = true;
            return output;
        }
        if std::mem::take(&mut self.pid_reset) {
            self.i_state = 0.0;
            self.d_term = 0.0;
        }
        // The gains are per second; the loop runs every PID_DT.
        let ki = gains.ki * PID_DT;
        let kd = gains.kd / PID_DT;
        let i_ceiling = if ki > 0.0 { PWM_MAX / ki } else { 0.0 };
        self.i_state = (self.i_state + error).clamp(0.0, i_ceiling);
        // Derivative of the reading (a falling temperature adds power),
        // through a first-order low-pass filter.
        let derivative = kd * (self.d_state - current);
        self.d_term += (1.0 - PID_K1) * (derivative - self.d_term);
        let output = gains.kp * error + ki * self.i_state + self.d_term;
        if output.is_finite() { output } else { 0.0 }
    }
}

/// A relay PID autotune like Marlin's M303: the heater switches between
/// `bias + swing` and `bias - swing` as the reading crosses the target, the
/// bias is rebalanced after every cycle so that heating and cooling take
/// equally long, and from the third cycle on the oscillation's amplitude
/// and period give the gains by the Ziegler-Nichols rules.
#[derive(Clone, Debug)]
pub(crate) struct Autotune {
    /// Target temperature.
    pub target: f64,
    /// Requested cycles.
    pub ncycles: u32,
    /// Bed/chamber (true) use the "no overshoot" rule.
    pub no_overshoot: bool,
    /// Relay cycles started so far.
    cycles: u32,
    /// Whether the heater is on (the rising half of a cycle).
    heating: bool,
    /// When the heater last switched on.
    switched_on: f64,
    /// When the heater last switched off.
    switched_off: f64,
    /// How long the last rising half lasted.
    rise_time: f64,
    /// How long the last falling half lasted.
    fall_time: f64,
    /// Middle of the relay's two outputs (PWM units).
    bias: f64,
    /// Distance of each output from the bias (PWM units).
    swing: f64,
    /// Highest temperature since the heater switched off.
    peak: f64,
    /// Lowest temperature since the heater switched on.
    trough: f64,
    /// Last computed (Kp, Ki, Kd).
    pub result: Option<Gains>,
    /// Time of the next heater-state report.
    pub next_report: f64,
}

/// What the autotune wants the caller to do after a sample.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TuneEvent {
    /// Keep going; set the heater output to the value.
    Continue(f64),
    /// One relay cycle completed; print the line(s).
    Cycle(Vec<String>, f64),
    /// Finished successfully.
    Finished(Gains),
    /// Failed with Marlin's message suffix (" failed! Temperature too high").
    Failed(&'static str),
}

impl Autotune {
    /// Start an autotune at simulated time `now`.
    pub(crate) fn new(target: f64, ncycles: u32, no_overshoot: bool, now: f64) -> Autotune {
        // Start symmetric around half power.
        let half = (PWM_MAX as i64 >> 1) as f64;
        Autotune {
            target,
            ncycles: ncycles.clamp(3, 20),
            no_overshoot,
            cycles: 0,
            heating: true,
            switched_on: now,
            switched_off: now,
            rise_time: 0.0,
            fall_time: 0.0,
            bias: half,
            swing: half,
            peak: 0.0,
            trough: 10_000.0,
            result: None,
            next_report: now,
        }
    }

    /// Initial relay output.
    pub(crate) fn initial_output(&self) -> f64 {
        self.output()
    }

    /// The relay output for the current half of the cycle. The firmware
    /// drives the heater at half the PWM resolution, so it is rounded down
    /// to an even value.
    fn output(&self) -> f64 {
        let level = if self.heating {
            self.bias + self.swing
        } else {
            self.bias - self.swing
        };
        ((level as i64).max(0) >> 1) as f64 * 2.0
    }

    /// Feed one temperature sample taken at `now`.
    pub(crate) fn sample(&mut self, now: f64, temp: f64) -> TuneEvent {
        self.peak = self.peak.max(temp);
        self.trough = self.trough.min(temp);
        let mut lines = Vec::new();
        // The relay switches when the reading crosses the target, but not
        // within 5 s of the last switch.
        if self.heating && temp > self.target && now - self.switched_on > 5.0 {
            self.heating = false;
            self.switched_off = now;
            self.rise_time = now - self.switched_on;
            self.peak = self.target;
        }
        if !self.heating && temp < self.target && now - self.switched_off > 5.0 {
            self.heating = true;
            self.switched_on = now;
            self.fall_time = now - self.switched_off;
            if self.cycles > 0 {
                lines = self.finish_cycle();
            }
            self.cycles += 1;
            self.trough = self.target;
        }
        if temp > self.target + 30.0 {
            return TuneEvent::Failed(" failed! Temperature too high");
        }
        if now - self.switched_off.min(self.switched_on) > 20.0 * 60.0 {
            return TuneEvent::Failed(" failed! timeout");
        }
        if self.cycles > self.ncycles
            && self.cycles > 2
            && let Some(g) = self.result
        {
            return TuneEvent::Finished(g);
        }
        let output = self.output();
        if lines.is_empty() {
            TuneEvent::Continue(output)
        } else {
            TuneEvent::Cycle(lines, output)
        }
    }

    /// A cycle is complete: rebalance the relay and report the cycle, with
    /// the gains from the third cycle on.
    fn finish_cycle(&mut self) -> Vec<String> {
        self.rebalance();
        let mut report = format!(
            " bias: {} d: {} min: {} max: {}",
            self.bias as i64,
            self.swing as i64,
            crate::format::f2(self.trough),
            crate::format::f2(self.peak)
        );
        if self.cycles <= 2 {
            return vec![report];
        }
        let (gains, ku, tu) = self.gains();
        report.push_str(&format!(
            " Ku: {} Tu: {}",
            crate::format::f2(ku),
            crate::format::f2(tu)
        ));
        self.result = Some(gains);
        let rule = if self.no_overshoot {
            " No overshoot"
        } else {
            " Classic PID"
        };
        vec![
            report,
            rule.to_string(),
            format!(
                " Kp: {} Ki: {} Kd: {}",
                crate::format::f2(gains.kp),
                crate::format::f2(gains.ki),
                crate::format::f2(gains.kd)
            ),
        ]
    }

    /// Move the bias toward whichever half was shorter, in proportion to
    /// how lopsided the cycle was, keep it 20 PWM steps from either end, and
    /// let the outputs swing as far as the nearer end allows.
    fn rebalance(&mut self) {
        let period = self.rise_time + self.fall_time;
        self.bias += (self.swing * (self.rise_time - self.fall_time) / period).trunc();
        self.bias = self.bias.clamp(20.0, PWM_MAX - 20.0);
        self.swing = if self.bias > (PWM_MAX as i64 >> 1) as f64 {
            PWM_MAX - 1.0 - self.bias
        } else {
            self.bias
        };
    }

    /// The relay's ultimate gain and period, and the PID gains from them:
    /// the classic Ziegler-Nichols rule for hotends, the "no overshoot" rule
    /// for beds and chambers.
    fn gains(&self) -> (Gains, f64, f64) {
        let amplitude = (self.peak - self.trough) * 0.5;
        // Describing-function estimate for a relay of amplitude `swing`.
        let ku = if amplitude > 1e-6 {
            (4.0 * self.swing) / (std::f64::consts::PI * amplitude)
        } else {
            0.0
        };
        let tu = self.rise_time + self.fall_time;
        let (p_factor, d_factor) = if self.no_overshoot {
            (0.2, 1.0 / 3.0)
        } else {
            (0.6, 1.0 / 8.0)
        };
        let kp = ku * p_factor;
        let ki = if tu > 0.0 { kp * 2.0 / tu } else { 0.0 };
        let kd = kp * tu * d_factor;
        (Gains { kp, ki, kd }, ku, tu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOTEND_GAINS: Gains = Gains {
        kp: 22.2,
        ki: 1.08,
        kd: 114.0,
    };
    const BED_GAINS: Gains = Gains {
        kp: 92.03,
        ki: 3.47,
        kd: 608.21,
    };

    /// Simulate heating and return (time to reach target-1, peak, final).
    fn heat(params: HeaterParams, gains: Gains, target: f64, secs: f64) -> (f64, f64, f64) {
        let mut h = Heater::new(params, Control::Pid, 20.0);
        let mut rng = Rng::new(1, 1);
        h.set_target(target);
        let dt = 0.05;
        let mut t = 0.0;
        let mut reached = None;
        let mut peak: f64 = 0.0;
        while t < secs {
            h.step(dt, 20.0, 0.0, gains, &mut rng);
            t += dt;
            if reached.is_none() && h.temp() >= target - 1.0 {
                reached = Some(t);
            }
            peak = peak.max(h.temp());
        }
        (reached.unwrap_or(f64::INFINITY), peak, h.temp())
    }

    #[test]
    fn hotend_heats_in_realistic_time() {
        let (t, peak, end) = heat(HeaterParams::hotend(), HOTEND_GAINS, 200.0, 300.0);
        assert!((40.0..=90.0).contains(&t), "reached after {t}s");
        assert!(peak > 200.3 && peak < 212.0, "peak {peak}");
        assert!((end - 200.0).abs() < 0.6, "settled at {end}");
    }

    #[test]
    fn bed_heats_in_realistic_time() {
        let (t, peak, end) = heat(HeaterParams::bed(), BED_GAINS, 60.0, 900.0);
        assert!((80.0..=180.0).contains(&t), "reached after {t}s");
        assert!(peak < 66.0, "peak {peak}");
        assert!((end - 60.0).abs() < 0.8, "settled at {end}");
    }

    #[test]
    fn cools_toward_ambient_when_off() {
        let mut h = Heater::new(HeaterParams::hotend(), Control::Pid, 20.0);
        let mut rng = Rng::new(1, 1);
        h.set_target(200.0);
        for _ in 0..3000 {
            h.step(0.05, 20.0, 0.0, HOTEND_GAINS, &mut rng);
        }
        let hot = h.temp();
        h.off();
        for _ in 0..3000 {
            h.step(0.05, 20.0, 1.0, HOTEND_GAINS, &mut rng);
        }
        assert!(h.temp() < hot - 50.0);
        assert_eq!(h.reported_power(), 0);
    }

    #[test]
    fn target_is_clamped() {
        let mut h = Heater::new(HeaterParams::bed(), Control::Pid, 20.0);
        h.set_target(500.0);
        assert_eq!(h.target, 140.0);
        h.set_target(f64::NAN);
        assert_eq!(h.target, 0.0);
        h.set_target(-5.0);
        assert_eq!(h.target, 0.0);
    }

    fn run_autotune(params: HeaterParams, target: f64, no_overshoot: bool) -> (Gains, Vec<String>) {
        let mut h = Heater::new(params, Control::Pid, 20.0);
        let mut rng = Rng::new(3, 3);
        let mut tune = Autotune::new(target, 5, no_overshoot, 0.0);
        h.forced_output = Some(tune.initial_output());
        let mut t = 0.0;
        let mut lines = Vec::new();
        while t < 4000.0 {
            if h.step(0.05, 20.0, 0.0, HOTEND_GAINS, &mut rng) {
                match tune.sample(t, h.temp()) {
                    TuneEvent::Continue(o) => h.forced_output = Some(o),
                    TuneEvent::Cycle(l, o) => {
                        lines.extend(l);
                        h.forced_output = Some(o);
                    }
                    TuneEvent::Finished(g) => return (g, lines),
                    TuneEvent::Failed(m) => panic!("autotune failed: {m}"),
                }
            }
            t += 0.05;
        }
        panic!("autotune did not finish");
    }

    #[test]
    fn hotend_autotune_gives_plausible_gains() {
        let (g, lines) = run_autotune(HeaterParams::hotend(), 200.0, false);
        assert!(lines.iter().any(|l| l.starts_with(" bias: ")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains(" Ku: ") && l.contains(" Tu: "))
        );
        assert!(lines.iter().any(|l| l == " Classic PID"));
        assert!((8.0..60.0).contains(&g.kp), "{g:?}");
        assert!((0.3..6.0).contains(&g.ki), "{g:?}");
        assert!((20.0..300.0).contains(&g.kd), "{g:?}");
        // The tuned gains must control the plant without oscillating.
        let (t, peak, end) = heat(HeaterParams::hotend(), g, 200.0, 300.0);
        assert!(t < 120.0, "{t}");
        assert!(peak < 215.0, "{peak}");
        assert!((end - 200.0).abs() < 1.0, "{end}");
    }

    #[test]
    fn bed_autotune_gives_plausible_gains() {
        let (g, lines) = run_autotune(HeaterParams::bed(), 60.0, true);
        assert!(lines.iter().any(|l| l == " No overshoot"));
        assert!(g.kp > 5.0 && g.kp < 400.0, "{g:?}");
        assert!(g.kd > 50.0, "{g:?}");
    }
}
