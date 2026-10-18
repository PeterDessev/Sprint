//! The settings a firmware reported, with typed accessors.

use serde::{Deserialize, Serialize};

use super::command::SettingCommand;

/// The settings a firmware reported, keyed by [`SettingCommand::key`], in
/// report order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PrinterSettings {
    entries: Vec<(String, SettingCommand)>,
}

/// Motion limits reported by M201/M203/M204/M205, in the units of the
/// estimator's `KinematicLimits` (mm/s, mm/s²). `None` = not reported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MotionLimits {
    /// M203 X Y Z E (mm/s).
    pub max_feedrate: Option<[f32; 4]>,
    /// M201 X Y Z E (mm/s²).
    pub max_accel: Option<[f32; 4]>,
    /// M204 P (or legacy S).
    pub print_accel: Option<f32>,
    /// M204 R.
    pub retract_accel: Option<f32>,
    /// M204 T (or legacy S).
    pub travel_accel: Option<f32>,
    /// M205 J (mm).
    pub junction_deviation: Option<f32>,
    /// M205 X Y Z E classic jerk (mm/s), when all four are reported.
    pub jerk: Option<[f32; 4]>,
}

impl PrinterSettings {
    /// Empty settings.
    pub fn new() -> PrinterSettings {
        PrinterSettings::default()
    }

    /// Insert a setting, replacing (in place) any setting with the same key.
    pub fn insert(&mut self, cmd: SettingCommand) {
        let key = cmd.key();
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = cmd,
            None => self.entries.push((key, cmd)),
        }
    }

    fn position(&self, key: &str) -> Option<usize> {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            return Some(pos);
        }
        // Accept non-canonical spellings ("m301 e1", "M0092").
        let canonical = SettingCommand::parse(key)?.key();
        self.entries.iter().position(|(k, _)| *k == canonical)
    }

    /// Setting by key (`"M92"`, `"M301 E1"`; case-insensitive).
    pub fn get(&self, key: &str) -> Option<&SettingCommand> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    /// Mutable setting by key.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut SettingCommand> {
        self.position(key).map(|i| &mut self.entries[i].1)
    }

    /// Remove a setting by key.
    pub fn remove(&mut self, key: &str) -> Option<SettingCommand> {
        self.position(key).map(|i| self.entries.remove(i).1)
    }

    /// Numeric value of one parameter of a setting. Flags (NaN) yield `None`.
    pub fn value(&self, key: &str, letter: char) -> Option<f64> {
        self.get(key)?.get(letter).filter(|v| v.is_finite())
    }

    /// Settings in report order.
    pub fn iter(&self) -> impl Iterator<Item = &SettingCommand> {
        self.entries.iter().map(|(_, c)| c)
    }

    /// Keys in report order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    /// Number of settings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no settings are stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remove all settings.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn value_f32(&self, key: &str, letter: char) -> Option<f32> {
        self.value(key, letter).map(|v| v as f32)
    }

    /// X Y Z E of a per-axis setting; E falls back to the `T0` instance
    /// (DISTINCT_E_FACTORS reports E separately per extruder).
    fn axes4(&self, code: &str) -> Option<[f32; 4]> {
        let x = self.value_f32(code, 'X')?;
        let y = self.value_f32(code, 'Y')?;
        let z = self.value_f32(code, 'Z')?;
        let e = self
            .value_f32(code, 'E')
            .or_else(|| self.value_f32(&format!("{code} T0"), 'E'))?;
        Some([x, y, z, e])
    }

    /// M92 steps/mm for X Y Z E.
    pub fn steps_per_mm(&self) -> Option<[f32; 4]> {
        self.axes4("M92")
    }

    /// M203 maximum feedrates (mm/s) for X Y Z E.
    pub fn max_feedrate(&self) -> Option<[f32; 4]> {
        self.axes4("M203")
    }

    /// M201 maximum accelerations (mm/s²) for X Y Z E.
    pub fn max_accel(&self) -> Option<[f32; 4]> {
        self.axes4("M201")
    }

    /// M301 (Kp, Ki, Kd) for a hotend: the `M301 E<tool>` instance, or the
    /// shared `M301` when the firmware has one set for all hotends.
    pub fn hotend_pid(&self, tool: u8) -> Option<(f32, f32, f32)> {
        let keyed = format!("M301 E{tool}");
        let key = if self.get(&keyed).is_some() {
            keyed
        } else {
            "M301".to_string()
        };
        Some((
            self.value_f32(&key, 'P')?,
            self.value_f32(&key, 'I')?,
            self.value_f32(&key, 'D')?,
        ))
    }

    /// M304 (Kp, Ki, Kd) of the bed.
    pub fn bed_pid(&self) -> Option<(f32, f32, f32)> {
        Some((
            self.value_f32("M304", 'P')?,
            self.value_f32("M304", 'I')?,
            self.value_f32("M304", 'D')?,
        ))
    }

    /// M851 probe offset X Y Z. Older firmware reports only Z; X and Y are
    /// then 0.
    pub fn probe_offset(&self) -> Option<[f32; 3]> {
        let z = self.value_f32("M851", 'Z')?;
        Some([
            self.value_f32("M851", 'X').unwrap_or(0.0),
            self.value_f32("M851", 'Y').unwrap_or(0.0),
            z,
        ])
    }

    /// M900 K (linear advance) of the first extruder.
    pub fn linear_advance(&self) -> Option<f32> {
        self.value_f32("M900", 'K')
            .or_else(|| self.value_f32("M900 T0", 'K'))
    }

    /// Kinematic limits (M201/M203/M204/M205) for the time estimator.
    pub fn motion_limits(&self) -> MotionLimits {
        let legacy = self.value_f32("M204", 'S');
        let jerk = (|| {
            Some([
                self.value_f32("M205", 'X')?,
                self.value_f32("M205", 'Y')?,
                self.value_f32("M205", 'Z')?,
                self.value_f32("M205", 'E')?,
            ])
        })();
        MotionLimits {
            max_feedrate: self.max_feedrate(),
            max_accel: self.max_accel(),
            print_accel: self.value_f32("M204", 'P').or(legacy),
            retract_accel: self.value_f32("M204", 'R'),
            travel_accel: self.value_f32("M204", 'T').or(legacy),
            junction_deviation: self.value_f32("M205", 'J'),
            jerk,
        }
    }
}

impl<'a> IntoIterator for &'a PrinterSettings {
    type Item = &'a SettingCommand;
    type IntoIter = std::iter::Map<
        std::slice::Iter<'a, (String, SettingCommand)>,
        fn(&'a (String, SettingCommand)) -> &'a SettingCommand,
    >;

    fn into_iter(self) -> Self::IntoIter {
        fn second(entry: &(String, SettingCommand)) -> &SettingCommand {
            &entry.1
        }
        self.entries.iter().map(second as fn(&_) -> &_)
    }
}

impl Extend<SettingCommand> for PrinterSettings {
    fn extend<I: IntoIterator<Item = SettingCommand>>(&mut self, iter: I) {
        for cmd in iter {
            self.insert(cmd);
        }
    }
}

impl FromIterator<SettingCommand> for PrinterSettings {
    fn from_iter<I: IntoIterator<Item = SettingCommand>>(iter: I) -> Self {
        let mut s = PrinterSettings::new();
        s.extend(iter);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(lines: &[&str]) -> PrinterSettings {
        lines
            .iter()
            .map(|l| SettingCommand::parse(l).unwrap_or_else(|| panic!("{l}")))
            .collect()
    }

    #[test]
    fn insert_replaces_same_key_in_place() {
        let mut s = settings(&["M92 X80 Y80 Z400 E93", "M301 P1 I2 D3", "M301 E1 P4 I5 D6"]);
        assert_eq!(s.len(), 3);
        s.insert(SettingCommand::parse("M92 X100 Y100 Z400 E93").unwrap());
        assert_eq!(s.len(), 3);
        assert_eq!(s.keys().collect::<Vec<_>>(), vec!["M92", "M301", "M301 E1"]);
        assert_eq!(s.value("M92", 'X'), Some(100.0));
        assert_eq!(s.value("m301 e1", 'P'), Some(4.0));
        assert_eq!(s.get("M0092").map(|c| c.code.as_str()), Some("M92"));
        assert!(s.get("M999").is_none());
        assert_eq!(s.remove("M301").map(|c| c.key()), Some("M301".to_string()));
        assert_eq!(s.len(), 2);
        s.get_mut("M92").unwrap().set('E', 95.0);
        assert_eq!(s.value("M92", 'E'), Some(95.0));
        assert_eq!((&s).into_iter().count(), 2);
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn typed_helpers() {
        let s = settings(&[
            "M92 X80.00 Y80.00 Z400.00 E93.00",
            "M203 X500.00 Y500.00 Z5.00 E25.00",
            "M201 X500.00 Y500.00 Z100.00 E5000.00",
            "M204 P500.00 R1000.00 T1500.00",
            "M205 B20000.00 S0.00 T0.00 J0.08",
            "M301 P21.73 I1.54 D76.55",
            "M304 P10.00 I0.02 D305.40",
            "M851 X-40.00 Y-10.00 Z-1.25",
            "M900 K0.22",
        ]);
        assert_eq!(s.steps_per_mm(), Some([80.0, 80.0, 400.0, 93.0]));
        assert_eq!(s.max_feedrate(), Some([500.0, 500.0, 5.0, 25.0]));
        assert_eq!(s.max_accel(), Some([500.0, 500.0, 100.0, 5000.0]));
        assert_eq!(s.hotend_pid(0), Some((21.73, 1.54, 76.55)));
        assert_eq!(s.hotend_pid(1), Some((21.73, 1.54, 76.55)), "shared M301");
        assert_eq!(s.bed_pid(), Some((10.0, 0.02, 305.4)));
        assert_eq!(s.probe_offset(), Some([-40.0, -10.0, -1.25]));
        assert_eq!(s.linear_advance(), Some(0.22));
        let m = s.motion_limits();
        assert_eq!(m.print_accel, Some(500.0));
        assert_eq!(m.retract_accel, Some(1000.0));
        assert_eq!(m.travel_accel, Some(1500.0));
        assert_eq!(m.junction_deviation, Some(0.08));
        assert_eq!(m.jerk, None);
        assert_eq!(m.max_feedrate, Some([500.0, 500.0, 5.0, 25.0]));
    }

    #[test]
    fn per_extruder_and_legacy_forms() {
        let s = settings(&[
            "M92 X80 Y80 Z400",
            "M92 T0 E93",
            "M92 T1 E140",
            "M301 E0 P1 I2 D3",
            "M301 E1 P4 I5 D6",
            "M204 S800",
            "M205 S0.00 T0.00 B20000 X8.00 Y8.00 Z0.40 E4.50",
            "M851 Z-1.50",
            "M900 T0 K0.05",
        ]);
        assert_eq!(s.steps_per_mm(), Some([80.0, 80.0, 400.0, 93.0]));
        assert_eq!(s.hotend_pid(0), Some((1.0, 2.0, 3.0)));
        assert_eq!(s.hotend_pid(1), Some((4.0, 5.0, 6.0)));
        assert_eq!(s.hotend_pid(2), None);
        assert_eq!(s.probe_offset(), Some([0.0, 0.0, -1.5]));
        assert_eq!(s.linear_advance(), Some(0.05));
        let m = s.motion_limits();
        assert_eq!(m.print_accel, Some(800.0));
        assert_eq!(m.travel_accel, Some(800.0));
        assert_eq!(m.jerk, Some([8.0, 8.0, 0.4, 4.5]));
        assert_eq!(m.junction_deviation, None);
        assert_eq!(m.max_accel, None);
    }

    #[test]
    fn empty_settings_have_no_values() {
        let s = PrinterSettings::new();
        assert_eq!(s.steps_per_mm(), None);
        assert_eq!(s.bed_pid(), None);
        assert_eq!(s.motion_limits(), MotionLimits::default());
        let flags = settings(&["M569 S1 X Y Z"]);
        assert_eq!(flags.value("M569", 'X'), None);
        assert_eq!(flags.value("M569", 'S'), Some(1.0));
    }
}
