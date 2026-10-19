//! Arithmetic of the calibration wizards: E-steps, PID gains, flow and
//! Z offset, plus the setting commands that apply their results.

use sprint_host::Heater;
use sprint_proto::{PrinterSettings, SettingCommand};

use super::thermal::HeaterPoint;

/// How far above the extruder inlet the filament is marked (mm).
pub const ESTEPS_MARK_MM: f32 = 120.0;
/// How much filament the E-steps test asks the printer to extrude (mm).
pub const ESTEPS_EXTRUDE_MM: f32 = 100.0;
/// Feedrate of the E-steps test extrusion (mm/min), slow enough that the
/// hotend keeps up and the extruder does not skip.
pub const ESTEPS_FEEDRATE_MM_MIN: f32 = 50.0;
/// The hotend counts as ready for the E-steps test within this many °C of
/// its target.
pub const ESTEPS_TEMP_TOLERANCE: f32 = 2.0;
/// Relative change (fraction) above which an E-steps result is flagged for
/// a second measurement.
pub const ESTEPS_WARN_CHANGE: f32 = 0.10;

/// Why an E-steps measurement cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstepsError {
    /// The current steps/mm are unknown or not plausible.
    InvalidSteps,
    /// The mark distance or the extrusion length is not plausible.
    InvalidSetup,
    /// The remaining length is not a number or negative.
    InvalidMeasurement,
    /// Nothing (or less than nothing) was extruded: the remaining length is
    /// at least the mark distance.
    NothingExtruded,
    /// The mark reached the extruder: more than the mark distance was
    /// extruded, so the length cannot be measured.
    MarkReached,
    /// The extruded length is less than half or more than twice the
    /// requested one: almost certainly a measuring mistake (or a skipping
    /// extruder).
    Implausible,
}

impl EstepsError {
    /// A sentence for the user.
    pub fn message(&self) -> &'static str {
        match self {
            EstepsError::InvalidSteps => {
                "The current extruder steps/mm are unknown. Load the settings first."
            }
            EstepsError::InvalidSetup => "The mark distance must be longer than the extrusion.",
            EstepsError::InvalidMeasurement => "Enter the remaining length in millimetres.",
            EstepsError::NothingExtruded => {
                "The remaining length must be shorter than the mark distance."
            }
            EstepsError::MarkReached => {
                "The mark went into the extruder. Mark the filament higher and run the test again."
            }
            EstepsError::Implausible => {
                "That is less than half or more than twice the requested length. Measure again: the extruder may be skipping."
            }
        }
    }
}

/// The result of an E-steps measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EstepsResult {
    /// What actually went through the extruder (mm).
    pub extruded: f32,
    /// Corrected steps/mm.
    pub new_steps: f32,
    /// Relative change (fraction, +0.05 = 5% more steps).
    pub change: f32,
}

impl EstepsResult {
    /// The change is large enough to deserve a second measurement.
    pub fn needs_double_check(&self) -> bool {
        self.change.abs() > ESTEPS_WARN_CHANGE
    }
}

/// E-steps from a mark test: the filament was marked `mark` mm above the
/// extruder, `requested` mm were extruded and `remaining` mm are left
/// between the mark and the extruder.
///
/// `new = current × requested / (mark − remaining)`.
pub fn esteps(
    current: f32,
    requested: f32,
    mark: f32,
    remaining: f32,
) -> Result<EstepsResult, EstepsError> {
    if !(current.is_finite() && current > 0.0 && current <= 100_000.0) {
        return Err(EstepsError::InvalidSteps);
    }
    if !(requested.is_finite() && mark.is_finite() && requested > 0.0 && mark > requested) {
        return Err(EstepsError::InvalidSetup);
    }
    if !remaining.is_finite() || remaining < 0.0 {
        return Err(EstepsError::InvalidMeasurement);
    }
    if remaining == 0.0 {
        return Err(EstepsError::MarkReached);
    }
    if remaining >= mark {
        return Err(EstepsError::NothingExtruded);
    }
    let extruded = mark - remaining;
    let ratio = requested / extruded;
    if !(0.5..=2.0).contains(&ratio) {
        return Err(EstepsError::Implausible);
    }
    let new_steps = current * ratio;
    Ok(EstepsResult {
        extruded,
        new_steps,
        change: ratio - 1.0,
    })
}

/// The extruder steps/mm of `tool`: `M92 T<tool> E` (distinct E factors)
/// or `M92 E`.
pub fn current_esteps(settings: &PrinterSettings, tool: u8) -> Option<f32> {
    settings
        .value(&format!("M92 T{tool}"), 'E')
        .or_else(|| settings.value("M92", 'E'))
        .map(|v| v as f32)
        .filter(|v| v.is_finite() && *v > 0.0)
}

/// `cmd` with `letter` set to `value`, cloned from the first of `keys`
/// that `settings` has (so its other parameters are kept), or a new command
/// `code` with `selector` and the value.
fn edited(
    settings: &PrinterSettings,
    keys: &[String],
    code: &str,
    selector: Option<(char, f64)>,
    values: &[(char, f64)],
) -> SettingCommand {
    let mut cmd = keys
        .iter()
        .find_map(|k| settings.get(k).cloned())
        .unwrap_or_else(|| {
            let params: Vec<(char, f64)> = selector.into_iter().collect();
            SettingCommand::new(code, &params)
        });
    for &(letter, value) in values {
        cmd.set(letter, value);
    }
    cmd.raw = cmd.to_gcode();
    cmd
}

/// The command that sets the extruder steps/mm of `tool` (`M92 E..` or
/// `M92 T<tool> E..`), keeping the other axes as reported.
pub fn esteps_command(settings: &PrinterSettings, tool: u8, steps: f32) -> SettingCommand {
    let keyed = format!("M92 T{tool}");
    let keys = if settings.get(&keyed).is_some() {
        vec![keyed]
    } else if tool == 0 {
        vec!["M92".to_owned()]
    } else {
        Vec::new()
    };
    let selector = (tool > 0).then_some(('T', f64::from(tool)));
    edited(
        settings,
        &keys,
        "M92",
        selector,
        &[('E', round_to(f64::from(steps), 2))],
    )
}

/// PID gains.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PidGains {
    /// Proportional gain.
    pub kp: f32,
    /// Integral gain.
    pub ki: f32,
    /// Derivative gain.
    pub kd: f32,
}

impl PidGains {
    /// From a `(kp, ki, kd)` tuple.
    pub fn from_tuple((kp, ki, kd): (f32, f32, f32)) -> PidGains {
        PidGains { kp, ki, kd }
    }

    /// All three are finite and non-negative.
    pub fn is_valid(&self) -> bool {
        [self.kp, self.ki, self.kd]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
    }

    /// The gains as `[kp, ki, kd]`.
    pub fn as_array(&self) -> [f32; 3] {
        [self.kp, self.ki, self.kd]
    }
}

/// The gains currently set for `heater`, from the settings report.
pub fn current_pid(settings: &PrinterSettings, heater: Heater) -> Option<PidGains> {
    let gains = match heater {
        Heater::Hotend(tool) => settings.hotend_pid(tool),
        Heater::Bed => settings.bed_pid(),
        Heater::Chamber => Some((
            settings.value("M309", 'P')? as f32,
            settings.value("M309", 'I')? as f32,
            settings.value("M309", 'D')? as f32,
        )),
    }?;
    Some(PidGains::from_tuple(gains))
}

/// The command that sets `heater`'s PID gains: `M301 [E<n>] P I D`,
/// `M304 P I D` or `M309 P I D`. Other parameters of the reported command
/// (M301 C/L/F) are kept.
pub fn pid_command(settings: &PrinterSettings, heater: Heater, gains: PidGains) -> SettingCommand {
    let values = [
        ('P', round_to(f64::from(gains.kp), 2)),
        ('I', round_to(f64::from(gains.ki), 2)),
        ('D', round_to(f64::from(gains.kd), 2)),
    ];
    match heater {
        Heater::Hotend(tool) => {
            let keyed = format!("M301 E{tool}");
            let (keys, selector) = if settings.get(&keyed).is_some() {
                (vec![keyed], Some(('E', f64::from(tool))))
            } else if tool == 0 && settings.get("M301").is_some() {
                (vec!["M301".to_owned()], None)
            } else {
                (Vec::new(), Some(('E', f64::from(tool))))
            };
            edited(settings, &keys, "M301", selector, &values)
        }
        Heater::Bed => edited(settings, &["M304".to_owned()], "M304", None, &values),
        Heater::Chamber => edited(settings, &["M309".to_owned()], "M309", None, &values),
    }
}

/// Upward crossings of `target` after time `since` in a heater series: an
/// estimate of the autotune cycles started so far (Marlin counts a cycle
/// each time the heater swings above the target).
pub fn autotune_cycles(points: &[HeaterPoint], target: f32, since: f64) -> usize {
    if !target.is_finite() {
        return 0;
    }
    points
        .windows(2)
        .filter(|w| w[1].t >= since && w[0].current < target && w[1].current >= target)
        .count()
}

/// Why a flow calculation cannot be done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowError {
    /// The current flow is not a plausible percentage.
    InvalidFlow,
    /// The expected wall width is not plausible.
    InvalidExpected,
    /// No usable measurement.
    NoMeasurement,
    /// A measurement is not a plausible wall thickness.
    InvalidMeasurement,
    /// Measured and expected differ by more than 50%.
    Implausible,
}

impl FlowError {
    /// A sentence for the user.
    pub fn message(&self) -> &'static str {
        match self {
            FlowError::InvalidFlow => "The current flow must be between 10% and 300%.",
            FlowError::InvalidExpected => "The expected wall width must be between 0.1 and 3 mm.",
            FlowError::NoMeasurement => "Enter at least one measured wall thickness.",
            FlowError::InvalidMeasurement => "Wall thicknesses must be between 0.05 and 5 mm.",
            FlowError::Implausible => {
                "The walls differ from the expected width by more than 50%. Check the test print's settings (one perimeter, no infill)."
            }
        }
    }
}

/// The result of a flow calculation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowResult {
    /// Mean measured wall thickness (mm).
    pub measured: f32,
    /// Corrected flow (%).
    pub new_flow: f32,
    /// Relative change (fraction).
    pub change: f32,
}

/// Flow from the wall-thickness method: a single-wall test print whose
/// walls should be `expected` mm thick measured `measurements` mm.
/// `new = current × expected / mean(measured)`. Non-positive or missing
/// measurements (empty fields) are skipped.
pub fn flow_from_walls(
    current_pct: f32,
    expected: f32,
    measurements: &[f32],
) -> Result<FlowResult, FlowError> {
    if !(current_pct.is_finite() && (10.0..=300.0).contains(&current_pct)) {
        return Err(FlowError::InvalidFlow);
    }
    if !(expected.is_finite() && (0.1..=3.0).contains(&expected)) {
        return Err(FlowError::InvalidExpected);
    }
    let used: Vec<f32> = measurements
        .iter()
        .copied()
        .filter(|m| !(m.is_nan() || *m == 0.0))
        .collect();
    if used.is_empty() {
        return Err(FlowError::NoMeasurement);
    }
    if used
        .iter()
        .any(|m| !(m.is_finite() && (0.05..=5.0).contains(m)))
    {
        return Err(FlowError::InvalidMeasurement);
    }
    let measured = used.iter().sum::<f32>() / used.len() as f32;
    let ratio = expected / measured;
    if !(0.5..=1.5).contains(&ratio) {
        return Err(FlowError::Implausible);
    }
    Ok(FlowResult {
        measured,
        new_flow: current_pct * ratio,
        change: ratio - 1.0,
    })
}

/// Which setting holds the printer's Z offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZOffsetSource {
    /// The probe's Z offset (`M851 Z`), for printers with a bed probe.
    Probe,
    /// The home offset (`M206 Z`), for printers homing on a Z endstop.
    HomeOffset,
}

impl ZOffsetSource {
    /// The setting command code.
    pub fn code(&self) -> &'static str {
        match self {
            ZOffsetSource::Probe => "M851",
            ZOffsetSource::HomeOffset => "M206",
        }
    }

    /// Display name.
    pub fn name(&self) -> &'static str {
        match self {
            ZOffsetSource::Probe => "Probe Z offset",
            ZOffsetSource::HomeOffset => "Home offset Z",
        }
    }
}

/// The Z offset source of a printer: the probe when M851 is reported, else
/// the home offset when M206 is, else `None`.
pub fn z_offset_source(settings: &PrinterSettings) -> Option<(ZOffsetSource, f32)> {
    if let Some(z) = settings.value("M851", 'Z') {
        return Some((ZOffsetSource::Probe, z as f32));
    }
    settings
        .value("M206", 'Z')
        .map(|z| (ZOffsetSource::HomeOffset, z as f32))
}

/// The offset that makes a babystep adjustment permanent.
///
/// `babystep` is the total babystepping (mm, negative = nozzle moved
/// closer to the bed) applied since `baseline` was the offset. A probe
/// offset moves with the nozzle (`baseline + babystep`); the home offset
/// shifts the coordinate system the other way (`baseline − babystep`).
pub fn z_offset_after_babystep(source: ZOffsetSource, baseline: f32, babystep: f32) -> f32 {
    let z = match source {
        ZOffsetSource::Probe => baseline + babystep,
        ZOffsetSource::HomeOffset => baseline - babystep,
    };
    round_to(f64::from(z), 3) as f32
}

/// The command that sets the Z offset, keeping X and Y as reported.
pub fn z_offset_command(
    settings: &PrinterSettings,
    source: ZOffsetSource,
    z: f32,
) -> SettingCommand {
    let code = source.code();
    edited(
        settings,
        &[code.to_owned()],
        code,
        None,
        &[('Z', round_to(f64::from(z), 3))],
    )
}

/// Whether a heater at `current` is within `tolerance` of a set `target`.
pub fn within_target(current: f32, target: f32, tolerance: f32) -> bool {
    current.is_finite()
        && target.is_finite()
        && target > 0.0
        && (current - target).abs() <= tolerance
}

fn round_to(value: f64, decimals: i32) -> f64 {
    let scale = 10f64.powi(decimals);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(lines: &[&str]) -> PrinterSettings {
        lines
            .iter()
            .filter_map(|l| SettingCommand::parse(l))
            .collect()
    }

    #[test]
    fn esteps_formula() {
        // 93 steps, 120 mm mark, 100 mm requested, 22 mm left: 98 mm went in.
        let r = esteps(93.0, 100.0, 120.0, 22.0).expect("ok");
        assert!((r.extruded - 98.0).abs() < 1e-5);
        assert!((r.new_steps - 93.0 * 100.0 / 98.0).abs() < 1e-3);
        assert!((r.change - (100.0 / 98.0 - 1.0)).abs() < 1e-6);
        assert!(!r.needs_double_check());
        // Exactly right.
        let r = esteps(93.0, 100.0, 120.0, 20.0).expect("ok");
        assert_eq!(r.new_steps, 93.0);
        // Under-extruding by 15%.
        let r = esteps(93.0, 100.0, 120.0, 35.0).expect("ok");
        assert!(r.needs_double_check());
    }

    #[test]
    fn esteps_validation() {
        assert_eq!(
            esteps(f32::NAN, 100.0, 120.0, 20.0),
            Err(EstepsError::InvalidSteps)
        );
        assert_eq!(
            esteps(0.0, 100.0, 120.0, 20.0),
            Err(EstepsError::InvalidSteps)
        );
        assert_eq!(
            esteps(93.0, 100.0, 90.0, 20.0),
            Err(EstepsError::InvalidSetup)
        );
        assert_eq!(
            esteps(93.0, 100.0, 120.0, -1.0),
            Err(EstepsError::InvalidMeasurement)
        );
        assert_eq!(
            esteps(93.0, 100.0, 120.0, f32::NAN),
            Err(EstepsError::InvalidMeasurement)
        );
        assert_eq!(
            esteps(93.0, 100.0, 120.0, 0.0),
            Err(EstepsError::MarkReached)
        );
        assert_eq!(
            esteps(93.0, 100.0, 120.0, 120.0),
            Err(EstepsError::NothingExtruded)
        );
        assert_eq!(
            esteps(93.0, 100.0, 120.0, 130.0),
            Err(EstepsError::NothingExtruded)
        );
        assert_eq!(
            esteps(93.0, 100.0, 120.0, 90.0),
            Err(EstepsError::Implausible)
        );
        assert!(!EstepsError::MarkReached.message().is_empty());
    }

    #[test]
    fn esteps_commands_keep_other_axes() {
        let s = settings(&["M92 X80.00 Y80.00 Z400.00 E93.00"]);
        assert_eq!(current_esteps(&s, 0), Some(93.0));
        assert_eq!(
            esteps_command(&s, 0, 94.8979).to_gcode(),
            "M92 X80 Y80 Z400 E94.9"
        );
        // Distinct E factors.
        let s = settings(&["M92 X80 Y80 Z400", "M92 T0 E93", "M92 T1 E415"]);
        assert_eq!(current_esteps(&s, 1), Some(415.0));
        assert_eq!(esteps_command(&s, 1, 420.0).to_gcode(), "M92 T1 E420");
        // Nothing reported.
        let empty = PrinterSettings::default();
        assert_eq!(current_esteps(&empty, 0), None);
        assert_eq!(esteps_command(&empty, 0, 95.0).to_gcode(), "M92 E95");
        assert_eq!(esteps_command(&empty, 2, 95.0).to_gcode(), "M92 T2 E95");
    }

    #[test]
    fn pid_commands_follow_the_report() {
        let single = settings(&["M301 P22.20 I1.08 D114.00", "M304 P92.03 I3.47 D608.21"]);
        let gains = PidGains {
            kp: 21.734,
            ki: 1.5,
            kd: 78.678,
        };
        assert_eq!(
            current_pid(&single, Heater::Hotend(0)),
            Some(PidGains {
                kp: 22.2,
                ki: 1.08,
                kd: 114.0
            })
        );
        assert_eq!(
            pid_command(&single, Heater::Hotend(0), gains).to_gcode(),
            "M301 P21.73 I1.5 D78.68"
        );
        assert_eq!(
            pid_command(&single, Heater::Bed, gains).to_gcode(),
            "M304 P21.73 I1.5 D78.68"
        );
        let multi = settings(&["M301 E0 P22 I1 D100 C100 L20", "M301 E1 P20 I1 D90"]);
        assert_eq!(
            pid_command(&multi, Heater::Hotend(0), gains).to_gcode(),
            "M301 E0 P21.73 I1.5 D78.68 C100 L20"
        );
        assert_eq!(
            pid_command(&multi, Heater::Hotend(1), gains).key(),
            "M301 E1"
        );
        let empty = PrinterSettings::default();
        assert_eq!(
            pid_command(&empty, Heater::Hotend(1), gains).to_gcode(),
            "M301 E1 P21.73 I1.5 D78.68"
        );
        assert_eq!(
            pid_command(&empty, Heater::Chamber, gains).to_gcode(),
            "M309 P21.73 I1.5 D78.68"
        );
        assert_eq!(current_pid(&empty, Heater::Chamber), None);
        assert!(gains.is_valid());
        assert!(
            !PidGains {
                kp: f32::NAN,
                ki: 0.0,
                kd: 0.0
            }
            .is_valid()
        );
    }

    #[test]
    fn autotune_cycles_count_upward_crossings() {
        let pts: Vec<HeaterPoint> = (0..200)
            .map(|i| HeaterPoint {
                t: f64::from(i),
                current: 200.0 + 5.0 * (i as f32 / 20.0 * std::f32::consts::TAU).sin(),
                target: 200.0,
                power: None,
            })
            .collect();
        // One upward crossing per 20 s period.
        let n = autotune_cycles(&pts, 200.0, 0.0);
        assert!((9..=10).contains(&n), "{n}");
        assert!(autotune_cycles(&pts, 200.0, 100.0) < n);
        assert_eq!(autotune_cycles(&pts, f32::NAN, 0.0), 0);
        assert_eq!(autotune_cycles(&[], 200.0, 0.0), 0);
    }

    #[test]
    fn flow_from_wall_thickness() {
        let r = flow_from_walls(100.0, 0.45, &[0.48, 0.47, 0.49, 0.48]).expect("ok");
        assert!((r.measured - 0.48).abs() < 1e-6);
        assert!((r.new_flow - 93.75).abs() < 1e-3);
        assert!(r.change < 0.0);
        // Empty fields (0 / NaN) are skipped.
        let r = flow_from_walls(95.0, 0.45, &[0.45, 0.0, f32::NAN]).expect("ok");
        assert!((r.new_flow - 95.0).abs() < 1e-4);
        assert_eq!(
            flow_from_walls(0.0, 0.45, &[0.45]),
            Err(FlowError::InvalidFlow)
        );
        assert_eq!(
            flow_from_walls(100.0, 9.0, &[0.45]),
            Err(FlowError::InvalidExpected)
        );
        assert_eq!(
            flow_from_walls(100.0, 0.45, &[]),
            Err(FlowError::NoMeasurement)
        );
        assert_eq!(
            flow_from_walls(100.0, 0.45, &[-0.4]),
            Err(FlowError::InvalidMeasurement)
        );
        assert_eq!(
            flow_from_walls(100.0, 0.45, &[1.2]),
            Err(FlowError::Implausible)
        );
        assert!(!FlowError::Implausible.message().is_empty());
    }

    #[test]
    fn z_offset_from_babystepping() {
        let probe = settings(&["M851 X10.00 Y10.00 Z-1.50 ; (mm)"]);
        assert_eq!(z_offset_source(&probe), Some((ZOffsetSource::Probe, -1.5)));
        // Nozzle babystepped 0.1 mm closer: the probe offset grows more negative.
        let z = z_offset_after_babystep(ZOffsetSource::Probe, -1.5, -0.1);
        assert!((z + 1.6).abs() < 1e-6);
        assert_eq!(
            z_offset_command(&probe, ZOffsetSource::Probe, z).to_gcode(),
            "M851 X10 Y10 Z-1.6"
        );
        let home = settings(&["M206 X0 Y0 Z0"]);
        assert_eq!(
            z_offset_source(&home),
            Some((ZOffsetSource::HomeOffset, 0.0))
        );
        let z = z_offset_after_babystep(ZOffsetSource::HomeOffset, 0.0, -0.1);
        assert!((z - 0.1).abs() < 1e-6);
        assert_eq!(z_offset_source(&PrinterSettings::default()), None);
        assert_eq!(
            z_offset_command(&PrinterSettings::default(), ZOffsetSource::Probe, -0.25).to_gcode(),
            "M851 Z-0.25"
        );
        assert_eq!(ZOffsetSource::HomeOffset.code(), "M206");
    }

    #[test]
    fn within_target_checks() {
        assert!(within_target(203.5, 205.0, 2.0));
        assert!(!within_target(202.9, 205.0, 2.0));
        assert!(!within_target(25.0, 0.0, 30.0));
        assert!(!within_target(f32::NAN, 205.0, 2.0));
    }
}
