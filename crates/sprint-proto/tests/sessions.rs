//! Whole-session transcripts (as a host sees them after connecting) fed
//! through one `ReplyParser`, modelled on real Marlin 2.1, Marlin 1.1,
//! Prusa, Klipper, RepRapFirmware and Smoothieware captures.

use sprint_proto::{FirmwareFlavor, HostAction, PrinterSettings, Reply, ReplyParser, SdReply};

fn run(transcript: &str) -> Vec<Reply> {
    let mut parser = ReplyParser::new();
    let mut out: Vec<Reply> = transcript.lines().flat_map(|l| parser.feed(l)).collect();
    out.extend(parser.flush());
    out
}

fn count(replies: &[Reply], pred: impl Fn(&Reply) -> bool) -> usize {
    replies.iter().filter(|r| pred(r)).count()
}

const MARLIN_21: &str = "\
start
echo:Marlin 2.1.2.1
echo: Last Updated: 2023-07-29 | Author: (none, default config)
echo: Compiled: Jul 29 2023
echo: Free Memory: 2925  PlannerBufferBytes: 1232
echo:V87 stored settings retrieved (622 bytes; crc 38912)
echo:SD card ok
FIRMWARE_NAME:Marlin 2.1.2.1 (Jul 29 2023 12:00:00) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:Ender-3 V2 EXTRUDER_COUNT:1 UUID:cede2a2f-41a2-4748-9b12-c55c62f367ff
Cap:SERIAL_XON_XOFF:0
Cap:BINARY_FILE_TRANSFER:0
Cap:EEPROM:1
Cap:VOLUMETRIC:1
Cap:AUTOREPORT_POS:0
Cap:AUTOREPORT_TEMP:1
Cap:PROGRESS:0
Cap:PRINT_JOB:1
Cap:AUTOLEVEL:1
Cap:RUNOUT:0
Cap:Z_PROBE:1
Cap:LEVELING_DATA:1
Cap:BUILD_PERCENT:0
Cap:SOFTWARE_POWER:0
Cap:TOGGLE_LIGHTS:0
Cap:CASE_LIGHT_BRIGHTNESS:0
Cap:EMERGENCY_PARSER:1
Cap:HOST_ACTION_COMMANDS:1
Cap:PROMPT_SUPPORT:1
Cap:SDCARD:1
Cap:REPEAT:0
Cap:SD_WRITE:1
Cap:AUTOREPORT_SD_STATUS:0
Cap:LONG_FILENAME:1
Cap:LFN_WRITE:0
Cap:CUSTOM_FIRMWARE_UPLOAD:0
Cap:EXTENDED_M20:1
Cap:THERMAL_PROTECTION:1
Cap:MOTION_MODES:0
Cap:ARCS:1
Cap:BABYSTEPPING:1
Cap:CHAMBER_TEMPERATURE:0
Cap:COOLER_TEMPERATURE:0
Cap:MEATPACK:0
Cap:CONFIG_EXPORT:0
ok
echo:; Linear Units:
echo:  G21 ; (mm)
echo:; Temperature Units:
echo:  M149 C ; Units in Celsius
echo:; Filament settings (Disabled):
echo:  M200 S0 D1.75
echo:; Steps per unit:
echo:  M92 X80.00 Y80.00 Z400.00 E93.00
echo:; Max feedrates (units/s):
echo:  M203 X500.00 Y500.00 Z5.00 E25.00
echo:; Max Acceleration (units/s2):
echo:  M201 X500.00 Y500.00 Z100.00 E5000.00
echo:; Acceleration (units/s2) (P<print-accel> R<retract-accel> T<travel-accel>):
echo:  M204 P500.00 R500.00 T500.00
echo:; Advanced (B<min_segment_time_us> S<min_feedrate> T<min_travel_feedrate> J<junc_dev>):
echo:  M205 B20000.00 S0.00 T0.00 J0.08
echo:; Home offset:
echo:  M206 X0.00 Y0.00 Z0.00
echo:; Auto Bed Leveling:
echo:  M420 S1 Z10.00 ; Leveling ON
echo:; Material heatup parameters:
echo:  M145 S0 H185.00 B45.00 F255
echo:  M145 S1 H240.00 B110.00 F255
echo:; Hotend PID:
echo:  M301 P21.73 I1.54 D76.55
echo:; Bed PID:
echo:  M304 P10.00 I0.02 D305.40
echo:; Z-Probe Offset:
echo:  M851 X-40.00 Y-10.00 Z-1.25 ; (mm)
echo:; Stepper driver current:
echo:  M906 X580 Y580 Z580
echo:  M906 T0 E650
echo:; Hybrid Threshold:
echo:  M913 X100 Y100 Z3
echo:  M913 T0 E30
echo:; Driver stepping mode:
echo:  M569 S1 X Y Z
echo:  M569 S1 T0 E
echo:; Linear Advance:
echo:  M900 K0.05
echo:; Filament load/unload:
echo:  M603 L0.00 U100.00 ; (mm)
echo:; Input Shaping:
echo:  M593 X F40.00 D0.15
echo:  M593 Y F37.50 D0.12
ok
 T:22.81 /0.00 B:23.44 /0.00 @:0 B@:0
X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0
ok
Reporting endstop status
x_min: open
y_min: open
z_min: TRIGGERED
z_probe: open
ok
Begin file list
CUBE~1.GCO 12345 cube.gcode
/PARTS/BENCHY~1.GCO 98765 3DBenchy.gcode
End file list
ok
Bilinear Leveling Grid:
      0      1      2
 0 +0.050 +0.025 -0.012
 1 +0.012 +0.000 -0.025
 2 -0.012 -0.037 -0.062

echo:Bed Leveling ON
echo:Fade Height 10.00
ok
echo:busy: processing
 T:150.12 /210.00 B:59.80 /60.00 @:127 B@:64 W:?
echo:busy: processing
 T:210.03 /210.00 B:60.01 /60.00 @:52 B@:30 W:5
ok
//action:prompt_end
//action:prompt_begin Filament Runout T0
//action:prompt_button Continue
//action:prompt_show
echo:busy: paused for user
ok
Error:Thermal Runaway, system stopped! Heater_ID: 0
Error:Printer halted. kill() called!
";

#[test]
fn marlin_21_session() {
    let replies = run(MARLIN_21);
    assert_eq!(replies[0], Reply::Start);

    let fw = replies.iter().find_map(|r| match r {
        Reply::Firmware(f) => Some(f),
        _ => None,
    });
    let fw = fw.expect("firmware");
    assert_eq!(fw.flavor, FirmwareFlavor::Marlin);
    assert_eq!(fw.machine_type.as_deref(), Some("Ender-3 V2"));

    let caps: Vec<(&str, bool)> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::Capability { name, enabled } => Some((name.as_str(), *enabled)),
            _ => None,
        })
        .collect();
    assert_eq!(caps.len(), 35);
    assert!(caps.contains(&("AUTOREPORT_TEMP", true)));
    assert!(caps.contains(&("SERIAL_XON_XOFF", false)));
    for (name, _) in &caps {
        assert!(
            sprint_proto::capability_description(name).is_some(),
            "{name}"
        );
    }

    let mut settings = PrinterSettings::default();
    for r in &replies {
        if let Reply::Setting(cmd) = r {
            settings.insert(cmd.clone());
        }
    }
    assert_eq!(settings.len(), 25);
    assert_eq!(settings.steps_per_mm(), Some([80.0, 80.0, 400.0, 93.0]));
    assert_eq!(settings.hotend_pid(0), Some((21.73, 1.54, 76.55)));
    assert_eq!(settings.bed_pid(), Some((10.0, 0.02, 305.4)));
    assert_eq!(settings.probe_offset(), Some([-40.0, -10.0, -1.25]));
    assert_eq!(settings.linear_advance(), Some(0.05));
    assert_eq!(settings.motion_limits().junction_deviation, Some(0.08));
    assert_eq!(settings.value("M593 Y", 'F'), Some(37.5));
    assert_eq!(settings.value("M906 T0", 'E'), Some(650.0));
    assert_eq!(settings.value("M145 S1", 'H'), Some(240.0));
    assert!(settings.get("M569 T0").is_some());
    assert!(settings.get("G21").is_some());

    // Echo comment headers are not settings.
    assert!(replies.contains(&Reply::Echo("; Steps per unit:".into())));

    assert_eq!(count(&replies, |r| matches!(r, Reply::Temperatures(_))), 3);
    assert_eq!(count(&replies, |r| matches!(r, Reply::Position(_))), 1);
    assert_eq!(count(&replies, |r| matches!(r, Reply::Endstop { .. })), 4);
    assert!(replies.contains(&Reply::Endstop {
        name: "z_min".into(),
        triggered: true
    }));
    assert_eq!(
        count(&replies, |r| matches!(r, Reply::SdCard(SdReply::File(_)))),
        2
    );
    let mesh = replies
        .iter()
        .find_map(|r| match r {
            Reply::Mesh(m) => Some(m),
            _ => None,
        })
        .expect("mesh");
    assert_eq!((mesh.rows(), mesh.cols()), (3, 3));
    assert!((mesh.range().unwrap() - 0.112).abs() < 1e-5);
    assert_eq!(count(&replies, |r| matches!(r, Reply::Busy(_))), 3);
    assert!(replies.contains(&Reply::HostAction(HostAction::PromptBegin(
        "Filament Runout T0".into()
    ))));
    assert!(replies.contains(&Reply::Busy("paused for user".into())));
    let errors: Vec<bool> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::Error { fatal, .. } => Some(*fatal),
            _ => None,
        })
        .collect();
    assert_eq!(errors, vec![true, true]);
    let others: Vec<&Reply> = replies
        .iter()
        .filter(|r| matches!(r, Reply::Other(_)))
        .collect();
    assert_eq!(
        others,
        vec![&Reply::Other("Reporting endstop status".into())]
    );
}

const MARLIN_11_STREAMING: &str = "\
start
echo:Marlin 1.1.9
echo: Last Updated: 2018-08-01 | Author: (none, default config)
echo:Steps per unit:
echo:  M92 X80.00 Y80.00 Z400.00 E93.00
echo:Maximum Acceleration (units/s2):
echo:  M201 X500 Y500 Z100 E5000
echo:Advanced: B<min_segment_time_us> S<min_feedrate> T<min_travel_feedrate> X<max_xy_jerk> Z<max_z_jerk> E<max_e_jerk>
echo:  M205 B20000 S0.00 T0.00 X10.00 Y10.00 Z0.30 E5.00
echo:Z-Probe Offset (mm):
echo:  M851 Z-1.50
ok
ok N1 P15 B3
Error:checksum mismatch, Last Line: 1
Resend: 2
ok
ok N2 P14 B3
Error:Line Number is not Last Line Number+1, Last Line: 2
Resend: 3
ok
echo:Unknown command: \"M9999\"
ok
";

#[test]
fn marlin_11_streaming_and_resends() {
    let replies = run(MARLIN_11_STREAMING);
    assert!(replies.contains(&Reply::Ok {
        line: Some(1),
        planner_free: Some(15),
        buffer_free: Some(3)
    }));
    assert!(replies.contains(&Reply::Resend(2)));
    assert!(replies.contains(&Reply::Resend(3)));
    assert!(replies.contains(&Reply::UnknownCommand("M9999".into())));
    assert_eq!(
        count(&replies, |r| matches!(r, Reply::Error { fatal: false, .. })),
        2
    );
    let settings: PrinterSettings = replies
        .iter()
        .filter_map(|r| match r {
            Reply::Setting(c) => Some(c.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(settings.len(), 4);
    assert_eq!(settings.probe_offset(), Some([0.0, 0.0, -1.5]));
    assert_eq!(settings.motion_limits().jerk, Some([10.0, 10.0, 0.3, 5.0]));
}

const PRUSA_MK3S: &str = "\
start
echo: 3.13.2-7080
echo: Last Updated: Jan 1 2024 00:00:00 | Author: (none, default config)
echo: Free Memory: 1890  PlannerBufferBytes: 1392
echo:Stored settings retrieved
FIRMWARE_NAME:Prusa-Firmware 3.13.2 based on Marlin FIRMWARE_URL:https://github.com/prusa3d/Prusa-Firmware PROTOCOL_VERSION:1.0 MACHINE_TYPE:Prusa i3 MK3S EXTRUDER_COUNT:1 UUID:00000000-0000-0000-0000-000000000000
Cap:AUTOREPORT_TEMP:1
Cap:AUTOREPORT_FANS:1
Cap:AUTOREPORT_POSITION:1
Cap:EXTENDED_M20:1
ok
echo:Steps per unit:
echo:  M92 X100.00 Y100.00 Z400.00 E280.00
echo:Maximum feedrates (mm/s):
echo:  M203 X200.00 Y200.00 Z12.00 E120.00
echo:PID settings:
echo:   M301 P16.13 I1.16 D56.23
echo:PID heatbed settings:
echo:   M304 P126.13 I4.30 D924.76
ok
T:21.6 /0.0 B:21.9 /0.0 T0:21.6 /0.0 @:0 B@:0 P:0.0 A:30.9
ok
X:0.00 Y:0.00 Z:0.15 E:0.00 Count X: 0.00 Y:-3.00 Z:0.15 E:0.00
ok
T:205.1 E:0 B:59.8
T:206.0 E:0 B:59.9
ok
Num X,Y: 3,3
Z search height: 5.00
Measured points:
  0.30000  0.31000  0.32000
  0.20000  0.21000  0.22000
  0.10000  0.11000  0.12000
ok
echo:1 min, 23 sec
ok
";

#[test]
fn prusa_session() {
    let replies = run(PRUSA_MK3S);
    let fw = replies
        .iter()
        .find_map(|r| match r {
            Reply::Firmware(f) => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(fw.flavor, FirmwareFlavor::Prusa);
    assert_eq!(fw.version.as_deref(), Some("3.13.2"));
    assert_eq!(count(&replies, |r| matches!(r, Reply::Setting(_))), 4);
    let temps: Vec<_> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::Temperatures(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(temps.len(), 3);
    assert_eq!(temps[0].board, Some(30.9));
    assert_eq!(temps[1].hotends[0].target, None);
    let pos = replies
        .iter()
        .find_map(|r| match r {
            Reply::Position(p) => Some(p),
            _ => None,
        })
        .unwrap();
    assert_eq!(pos.z, 0.15);
    let mesh = replies
        .iter()
        .find_map(|r| match r {
            Reply::Mesh(m) => Some(m),
            _ => None,
        })
        .unwrap();
    assert!((mesh.points[0][0] - 0.1).abs() < 1e-6, "front row first");
    assert!(replies.contains(&Reply::PrintTime(83)));
}

const KLIPPER: &str = "\
// Klipper state: Ready
ok
FIRMWARE_NAME:Klipper FIRMWARE_VERSION:v0.12.0-123-gabcdef
ok
ok B:60.0 /60.0 T0:210.0 /210.0
X:0.000 Y:0.000 Z:0.000 E:0.000
ok
!! Must home axis first: 0.000 0.000 0.000 [0.000]
ok
x:open y:open z:TRIGGERED
ok
// PID parameters: pid_Kp=22.865 pid_Ki=1.292 pid_Kd=101.178
// The SAVE_CONFIG command will update the printer config file
// with these parameters and restart the printer.
ok
// Mesh Leveling Probed Z positions:
//  0.012500 0.025000 0.037500
//  0.050000 0.062500 0.075000
// Mesh X,Y: 3,2
// Measured points:
//   0.050000  0.062500  0.075000
//   0.012500  0.025000  0.037500
ok
//action:cancel
!! Lost communication with MCU 'mcu'
";

#[test]
fn klipper_session() {
    let replies = run(KLIPPER);
    assert_eq!(replies[0], Reply::Info("Klipper state: Ready".into()));
    let fw = replies
        .iter()
        .find_map(|r| match r {
            Reply::Firmware(f) => Some(f),
            _ => None,
        })
        .unwrap();
    assert!(!fw.flavor.supports_m503());
    assert_eq!(count(&replies, |r| matches!(r, Reply::Temperatures(_))), 1);
    assert_eq!(count(&replies, |r| matches!(r, Reply::Position(_))), 1);
    assert_eq!(count(&replies, |r| matches!(r, Reply::Endstop { .. })), 3);
    assert_eq!(
        count(&replies, |r| matches!(r, Reply::AutotuneResult { .. })),
        1
    );
    assert_eq!(count(&replies, |r| matches!(r, Reply::Mesh(_))), 2);
    assert!(replies.contains(&Reply::HostAction(HostAction::Cancel)));
    let errors: Vec<bool> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::Error { fatal, .. } => Some(*fatal),
            _ => None,
        })
        .collect();
    assert_eq!(errors, vec![false, true]);
}

const RRF_AND_SMOOTHIE: &str = "\
FIRMWARE_NAME: RepRapFirmware for Duet 3 MB6HC FIRMWARE_VERSION: 3.4.5 ELECTRONICS: Duet 3 MB6HC v1.01 FIRMWARE_DATE: 2022-11-30 19:36:12
ok
ok T:25.3 /0.0 B:25.1 /0.0
Error: G0/G1: insufficient axes homed
ok
X:0.000 Y:0.000 Z:0.000 E:0.000 E0:-0.0 Count 0 0 0 Machine 0.000 0.000 0.000 Bed comp 0.000
ok
Endstops - X: not stopped, Y: not stopped, Z: at min stop, Z probe: not stopped
ok
FIRMWARE_NAME:Smoothieware, FIRMWARE_URL:http%3A//smoothieware.org, X-SOURCE_CODE_URL:https://github.com/Smoothieware/Smoothieware, FIRMWARE_VERSION:edge-1234abc, X-FIRMWARE_BUILD_DATE:Jan  1 2020 00:00:00, X-SYSTEM_CLOCK:100MHz, X-AXES:5
ok
ok T:21.0 /0.0 @0 B:21.2 /0.0 @0
ok C: X:0.0000 Y:0.0000 Z:0.0000 E:0.0000
;Steps per unit:
M92 X80.00000 Y80.00000 Z1600.00000
;Acceleration mm/sec^2:
M204 S1000.00000
ok
";

#[test]
fn rrf_and_smoothieware_sessions() {
    let replies = run(RRF_AND_SMOOTHIE);
    let flavors: Vec<FirmwareFlavor> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::Firmware(f) => Some(f.flavor),
            _ => None,
        })
        .collect();
    assert_eq!(
        flavors,
        vec![FirmwareFlavor::RepRapFirmware, FirmwareFlavor::Smoothieware]
    );
    assert_eq!(count(&replies, |r| matches!(r, Reply::Temperatures(_))), 2);
    assert_eq!(count(&replies, |r| matches!(r, Reply::Position(_))), 2);
    assert_eq!(count(&replies, |r| matches!(r, Reply::Endstop { .. })), 4);
    assert_eq!(count(&replies, |r| matches!(r, Reply::Setting(_))), 2);
    assert!(replies.contains(&Reply::Error {
        message: "G0/G1: insufficient axes homed".into(),
        fatal: false
    }));
    assert!(replies.contains(&Reply::Other(";Steps per unit:".into())));
}

const KLIPPER_RAW_OUTPUT: &str = "\
ok
Begin file list
benchy.gcode 1234567
my part v2.gcode 4321
subdir/other part.gcode 99
End file list
ok
// Mesh Leveling Probed Z positions:
//  0.012500 0.025000 0.037500
//  0.050000 0.062500 0.075000
Mesh X,Y: 3,2
Search Height: 5
Mesh Offsets: X=0.0000, Y=0.0000
Mesh Average: 0.04
Mesh Range: min=0.0125 max=0.0750
Interpolation Algorithm: lagrange
Measured points:
  0.050000  0.062500  0.075000
  0.012500  0.025000  0.037500

ok
File opened:my part v2.gcode Size:4321
File selected
ok
ok
SD printing byte 1000/4321
ok
!! Heater extruder not heating at expected rate
// See the 'verify_heater' section in docs/Config_Reference.md
// for the parameters that control this check.
ok
";

#[test]
fn klipper_raw_output_session() {
    let replies = run(KLIPPER_RAW_OUTPUT);
    let files: Vec<(&str, Option<u64>)> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::SdCard(SdReply::File(f)) => Some((f.name.as_str(), f.size)),
            _ => None,
        })
        .collect();
    assert_eq!(
        files,
        vec![
            ("benchy.gcode", Some(1_234_567)),
            ("my part v2.gcode", Some(4321)),
            ("subdir/other part.gcode", Some(99)),
        ]
    );
    let meshes: Vec<&sprint_proto::BedMesh> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::Mesh(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(meshes.len(), 2);
    assert_eq!(meshes[0].kind, "klipper_probed");
    assert_eq!(meshes[1].kind, "klipper_mesh");
    // Both are front row first, so they agree.
    assert_eq!(meshes[0].points, meshes[1].points);
    assert!(replies.contains(&Reply::SdCard(SdReply::Opened {
        name: "my part v2.gcode".into(),
        size: 4321
    })));
    assert!(replies.contains(&Reply::SdCard(SdReply::Progress {
        done: 1000,
        total: 4321
    })));
    assert!(replies.contains(&Reply::Error {
        message: "Heater extruder not heating at expected rate".into(),
        fatal: true
    }));
}

const PRUSA_AND_RRF3_LISTINGS: &str = "\
Begin file list
DIR_ENTER: /PARTS/ \"Parts\"
/PARTS/BENCHY~1.GCO 98765 0x5A3B1C2D \"3DBenchy.gcode\"
DIR_EXIT
CUBE~1.GCO 12345 0x5A3B1C2E \"cube.gcode\"
End file list
ok
Begin file list
benchy.gcode
my part.gcode
End file list
ok
Endstops - X: at min stop, Y: not stopped, Z: no endstop, Z probe: not stopped
ok
";

#[test]
fn prusa_and_rrf3_listing_session() {
    let replies = run(PRUSA_AND_RRF3_LISTINGS);
    let files: Vec<(&str, Option<&str>)> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::SdCard(SdReply::File(f)) => Some((f.name.as_str(), f.long_name.as_deref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        files,
        vec![
            ("/PARTS/BENCHY~1.GCO", Some("3DBenchy.gcode")),
            ("CUBE~1.GCO", Some("cube.gcode")),
            ("benchy.gcode", None),
            ("my part.gcode", None),
        ]
    );
    let endstops: Vec<(&str, bool)> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::Endstop { name, triggered } => Some((name.as_str(), *triggered)),
            _ => None,
        })
        .collect();
    assert_eq!(
        endstops,
        vec![("X", true), ("Y", false), ("Z probe", false)]
    );
}
