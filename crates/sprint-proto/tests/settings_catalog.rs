//! SettingCommand parse / to_gcode round trips and catalog consistency with
//! the parser and the G-code reference.

use sprint_proto::{
    PrinterSettings, Reply, ReplyParser, SettingCategory, SettingCommand, gcode_doc, setting_def,
    setting_defs,
};

/// Real M503 lines (Marlin 1.1, 2.0, 2.1, Prusa, Smoothieware) with the key
/// and G-code each must produce.
const LINES: &[(&str, &str, &str)] = &[
    (
        "echo:  M92 X80.00 Y80.00 Z400.00 E93.00",
        "M92",
        "M92 X80 Y80 Z400 E93",
    ),
    ("echo:  M92 T1 E140.00", "M92 T1", "M92 T1 E140"),
    (
        "echo:  M203 X500.00 Y500.00 Z5.00 E25.00",
        "M203",
        "M203 X500 Y500 Z5 E25",
    ),
    (
        "echo:  M201 X500.00 Y500.00 Z100.00 E5000.00",
        "M201",
        "M201 X500 Y500 Z100 E5000",
    ),
    (
        "echo:  M204 P500.00 R500.00 T500.00",
        "M204",
        "M204 P500 R500 T500",
    ),
    (
        "echo:  M205 B20000.00 S0.00 T0.00 J0.08",
        "M205",
        "M205 B20000 S0 T0 J0.08",
    ),
    (
        "echo:  M205 Q20000 S0.00 T0.00 X20.00 Y20.00 Z0.40 E5.00",
        "M205",
        "M205 Q20000 S0 T0 X20 Y20 Z0.4 E5",
    ),
    ("echo:  M206 X0.00 Y0.00 Z0.00", "M206", "M206 X0 Y0 Z0"),
    (
        "echo:  M301 P21.73 I1.54 D76.55",
        "M301",
        "M301 P21.73 I1.54 D76.55",
    ),
    (
        "echo:  M301 E1 P22.20 I1.08 D114.00",
        "M301 E1",
        "M301 E1 P22.2 I1.08 D114",
    ),
    (
        "echo:  M304 P10.00 I0.02 D305.40",
        "M304",
        "M304 P10 I0.02 D305.4",
    ),
    (
        "echo:  M309 P1.00 I0.10 D100.00",
        "M309",
        "M309 P1 I0.1 D100",
    ),
    (
        "echo:  M306 E0 P40.00 C16.70 R0.1670 A0.0835 F0.0000 H0.0107",
        "M306 E0",
        "M306 E0 P40 C16.7 R0.167 A0.0835 F0 H0.0107",
    ),
    (
        "echo:  M851 X-40.00 Y-10.00 Z-1.25 ; (mm)",
        "M851",
        "M851 X-40 Y-10 Z-1.25",
    ),
    ("echo:  M900 K0.05", "M900", "M900 K0.05"),
    ("echo:  M900 T1 K0.10", "M900 T1", "M900 T1 K0.1"),
    ("echo:  M200 S0 D1.75", "M200", "M200 S0 D1.75"),
    ("echo:  M200 T1 D2.85", "M200 T1", "M200 T1 D2.85"),
    (
        "echo:  M145 S0 H185.00 B45.00 F255",
        "M145 S0",
        "M145 S0 H185 B45 F255",
    ),
    (
        "echo:  M145 S1 H240.00 B110.00 F255",
        "M145 S1",
        "M145 S1 H240 B110 F255",
    ),
    (
        "echo:  M420 S0 Z10.00 ; Leveling OFF",
        "M420",
        "M420 S0 Z10",
    ),
    ("echo:  M413 S1", "M413", "M413 S1"),
    ("echo:  M906 X580 Y580 Z580", "M906", "M906 X580 Y580 Z580"),
    ("echo:  M906 T0 E650", "M906 T0", "M906 T0 E650"),
    ("echo:  M906 I1 X800", "M906 I1", "M906 I1 X800"),
    ("echo:  M913 X100 Y100 Z3", "M913", "M913 X100 Y100 Z3"),
    ("echo:  M913 T0 E30", "M913 T0", "M913 T0 E30"),
    ("echo:  M914 X50 Y50", "M914", "M914 X50 Y50"),
    ("echo:  M914 I1 X50", "M914 I1", "M914 I1 X50"),
    ("echo:  M569 S1 X Y Z", "M569", "M569 S1 X Y Z"),
    ("echo:  M569 S1 T0 E", "M569 T0", "M569 S1 T0 E"),
    ("echo:  M569 S1 I1 X Y", "M569 I1", "M569 S1 I1 X Y"),
    ("echo:  M593 X F40.00 D0.15", "M593 X", "M593 X F40 D0.15"),
    ("echo:  M593 Y F37.50 D0.12", "M593 Y", "M593 Y F37.5 D0.12"),
    (
        "echo:  M207 S3.00 W13.00 F2700.00 Z0.00",
        "M207",
        "M207 S3 W13 F2700 Z0",
    ),
    ("echo:  M208 S0.00 W0.00 F480.00", "M208", "M208 S0 W0 F480"),
    ("echo:  M209 S0", "M209", "M209 S0"),
    (
        "echo:  M217 S12.00 B10.00 E0.00 P600 R3600 U3600 F0 G0 X20.00 Y20.00 Z2.00",
        "M217",
        "M217 S12 B10 E0 P600 R3600 U3600 F0 G0 X20 Y20 Z2",
    ),
    (
        "echo:  M218 T1 X10.00 Y0.00 Z0.000",
        "M218 T1",
        "M218 T1 X10 Y0 Z0",
    ),
    ("echo:  M149 C ; Units in Celsius", "M149", "M149 C"),
    (
        "echo:  M665 L250.00 R125.00 H250.00 S200.00 X0.00 Y0.00 Z0.00",
        "M665",
        "M665 L250 R125 H250 S200 X0 Y0 Z0",
    ),
    ("echo:  M666 X0.00 Y0.00 Z0.00", "M666", "M666 X0 Y0 Z0"),
    ("echo:  M250 C26", "M250", "M250 C26"),
    ("echo:  M255 S5", "M255", "M255 S5"),
    ("echo:  M256 B255", "M256", "M256 B255"),
    ("echo:  M592 A0.00 B0.00 C1.00", "M592", "M592 A0 B0 C1"),
    ("echo:  M86 S0 T0 E0 B0", "M86", "M86 S0 T0 E0 B0"),
    ("echo:  M412 S1 D10.00", "M412", "M412 S1 D10"),
    (
        "echo:  M425 F1.00 S3.00 X0.100 Y0.100 Z0.000",
        "M425",
        "M425 F1 S3 X0.1 Y0.1 Z0",
    ),
    ("echo:  M603 L0.00 U100.00 ; (mm)", "M603", "M603 L0 U100"),
    ("echo:  M603 T1 L50.00 U80.00", "M603 T1", "M603 T1 L50 U80"),
    ("echo:  M710 S255 I0 A1 D60", "M710", "M710 S255 I0 A1 D60"),
    ("echo:  M281 P0 L10 U90", "M281 P0", "M281 P0 L10 U90"),
    ("echo:  M422 S1 X10.0 Y20.0", "M422 S1", "M422 S1 X10 Y20"),
    ("echo:  M422 W2 X200.0 Y20.0", "M422 W2", "M422 W2 X200 Y20"),
    ("echo:  G21    ; Units in mm (mm)", "G21", "G21"),
    (
        "echo:   M301 P16.13 I1.16 D56.23",
        "M301",
        "M301 P16.13 I1.16 D56.23",
    ),
    (
        "M92 X80.00000 Y80.00000 Z1600.00000",
        "M92",
        "M92 X80 Y80 Z1600",
    ),
];

#[test]
fn reply_parser_yields_settings_with_expected_keys() {
    let mut parser = ReplyParser::new();
    for &(line, key, gcode) in LINES {
        let replies = parser.feed(line);
        let [Reply::Setting(cmd)] = replies.as_slice() else {
            panic!("{line:?} -> {replies:?}");
        };
        assert_eq!(cmd.key(), key, "{line:?}");
        assert_eq!(cmd.to_gcode(), gcode, "{line:?}");
    }
}

#[test]
fn to_gcode_round_trips() {
    for &(line, _, _) in LINES {
        let cmd = SettingCommand::parse(line).expect(line);
        let again = SettingCommand::parse(&cmd.to_gcode()).expect("reparse");
        assert_eq!(again.code, cmd.code);
        assert_eq!(again.key(), cmd.key());
        assert_eq!(again.params.len(), cmd.params.len());
        for ((l1, v1), (l2, v2)) in cmd.params.iter().zip(&again.params) {
            assert_eq!(l1, l2);
            assert!(
                (v1.is_nan() && v2.is_nan()) || (v1 - v2).abs() < 1e-9,
                "{line:?}: {v1} vs {v2}"
            );
        }
        // Idempotent.
        assert_eq!(again.to_gcode(), cmd.to_gcode());
    }
}

#[test]
fn edited_values_round_trip() {
    let mut cmd = SettingCommand::parse("M92 X80 Y80 Z400 E93").unwrap();
    for v in [93.123456, 0.000001, 1234567.5, -12.25, 0.1 + 0.2] {
        cmd.set('E', v);
        let again = SettingCommand::parse(&cmd.to_gcode()).unwrap();
        assert!((again.get('E').unwrap() - v).abs() < 1e-6, "{v}");
    }
    cmd.set('X', f64::NAN);
    assert_eq!(cmd.to_gcode(), "M92 X Y80 Z400 E0.3");
    cmd.set('Y', f64::INFINITY);
    assert_eq!(cmd.to_gcode(), "M92 X Y Z400 E0.3");
}

#[test]
fn every_report_key_is_unique_in_a_full_report() {
    let settings: PrinterSettings = LINES
        .iter()
        .filter(|(l, _, _)| !l.starts_with("echo:   M301") && !l.starts_with("M92"))
        .map(|(l, _, _)| SettingCommand::parse(l).unwrap())
        .collect();
    let distinct: std::collections::HashSet<_> = LINES
        .iter()
        .filter(|(l, _, _)| !l.starts_with("echo:   M301") && !l.starts_with("M92"))
        .map(|(_, k, _)| *k)
        .collect();
    assert_eq!(settings.len(), distinct.len());
}

#[test]
fn catalog_codes_have_reference_docs() {
    for def in setting_defs() {
        let doc =
            gcode_doc(def.code).unwrap_or_else(|| panic!("{} has no reference doc", def.code));
        assert_eq!(doc.code, def.code);
        assert!(!doc.dangerous, "{} should not need confirmation", def.code);
    }
}

#[test]
fn catalog_covers_sample_parameters() {
    // Every parameter that appears in the sample report is described by the
    // catalog (except Marlin 1.1's legacy M205 Q).
    for &(line, _, _) in LINES {
        let cmd = SettingCommand::parse(line).unwrap();
        let Some(def) = setting_def(&cmd.code) else {
            assert_eq!(cmd.code, "G21");
            continue;
        };
        for &(letter, value) in &cmd.params {
            if cmd.code == "M205" && letter == 'Q' {
                continue;
            }
            let p = def
                .param(letter)
                .unwrap_or_else(|| panic!("{}: no param {letter}", def.code));
            assert_eq!(p.flag, value.is_nan(), "{} {letter}", def.code);
            if value.is_finite() {
                assert!(
                    (p.min..=p.max).contains(&value),
                    "{} {letter}{value} outside {}..{}",
                    def.code,
                    p.min,
                    p.max
                );
            }
        }
    }
}

#[test]
fn every_category_is_used() {
    for cat in SettingCategory::ALL {
        assert!(
            setting_defs().iter().any(|d| d.category == cat),
            "{} unused",
            cat.name()
        );
    }
}
