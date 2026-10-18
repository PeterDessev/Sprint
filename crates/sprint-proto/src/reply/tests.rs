//! Table-driven parser tests built from real firmware captures.

use super::*;

fn feed(line: &str) -> Vec<Reply> {
    ReplyParser::new().feed(line)
}

fn one(line: &str) -> Reply {
    let mut replies = feed(line);
    assert_eq!(replies.len(), 1, "{line:?} -> {replies:?}");
    replies.remove(0)
}

fn ok(line: Option<u32>, planner_free: Option<u32>, buffer_free: Option<u32>) -> Reply {
    Reply::Ok {
        line,
        planner_free,
        buffer_free,
    }
}

fn err(message: &str, fatal: bool) -> Reply {
    Reply::Error {
        message: message.to_string(),
        fatal,
    }
}

#[test]
fn ok_variants() {
    let cases = [
        ("ok", ok(None, None, None)),
        ("ok\r\n", ok(None, None, None)),
        ("OK", ok(None, None, None)),
        ("ok N12", ok(Some(12), None, None)),
        ("ok N12 P15 B3", ok(Some(12), Some(15), Some(3))),
        ("ok P15 B3", ok(None, Some(15), Some(3))),
        ("ok 123", ok(Some(123), None, None)),
        ("ok N0 P0 B0", ok(Some(0), Some(0), Some(0))),
    ];
    for (line, expected) in cases {
        assert_eq!(one(line), expected, "{line:?}");
    }
    assert_eq!(one("okay then"), Reply::Other("okay then".into()));
}

#[test]
fn ok_with_temperatures_or_position() {
    let r = feed("ok T:210.0 /210.0 B:60.0 /60.0 @:64 B@:0");
    assert_eq!(r.len(), 2);
    assert_eq!(r[0], ok(None, None, None));
    let Reply::Temperatures(t) = &r[1] else {
        panic!("{r:?}")
    };
    assert_eq!(t.hotends[0].current, 210.0);
    assert_eq!(t.bed.as_ref().map(|b| b.current), Some(60.0));

    let r = feed("ok B:60.0 /60.0 T0:210.0 /210.0");
    assert!(matches!(&r[1], Reply::Temperatures(t) if t.hotends.len() == 1));

    let r = feed("ok C: X:0.00 Y:0.00 Z:0.00 E:0.00");
    assert_eq!(r.len(), 2);
    assert!(matches!(&r[1], Reply::Position(p) if p.x == 0.0));

    let r = feed("ok N5 T:20.0 /0.0");
    assert_eq!(r[0], ok(Some(5), None, None));
    assert!(matches!(&r[1], Reply::Temperatures(_)));

    // Unknown remainder: just the ok.
    assert_eq!(feed("ok whatever"), vec![ok(None, None, None)]);
}

#[test]
fn resend_variants() {
    for (line, n) in [
        ("Resend: 3", 3),
        ("Resend:3", 3),
        ("Resend: N3", 3),
        ("rs N3", 3),
        ("rs 3", 3),
        ("Resend: 4294967295", u32::MAX),
    ] {
        assert_eq!(one(line), Reply::Resend(n), "{line:?}");
    }
    assert!(matches!(one("Resend: x"), Reply::Other(_)));
    assert!(matches!(one("Resend: 99999999999"), Reply::Other(_)));
    assert!(matches!(one("rsx 3"), Reply::Other(_)));
}

#[test]
fn errors_and_fatality() {
    let cases = [
        (
            "Error:checksum mismatch, Last Line: 2",
            "checksum mismatch, Last Line: 2",
            false,
        ),
        (
            "Error:Line Number is not Last Line Number+1, Last Line: 2",
            "Line Number is not Last Line Number+1, Last Line: 2",
            false,
        ),
        (
            "Error:No Checksum with line number, Last Line: 2",
            "No Checksum with line number, Last Line: 2",
            false,
        ),
        (
            "Error:No Line Number with checksum, Last Line: 2",
            "No Line Number with checksum, Last Line: 2",
            false,
        ),
        (
            "Error:Printer halted. kill() called!",
            "Printer halted. kill() called!",
            true,
        ),
        (
            "Error:Thermal Runaway, system stopped! Heater_ID: 0",
            "Thermal Runaway, system stopped! Heater_ID: 0",
            true,
        ),
        (
            "Error:Heating failed, system stopped! Heater_ID: bed",
            "Heating failed, system stopped! Heater_ID: bed",
            true,
        ),
        (
            "Error:MINTEMP triggered, system stopped! Heater_ID: 0",
            "MINTEMP triggered, system stopped! Heater_ID: 0",
            true,
        ),
        (
            "Error:MAXTEMP triggered, system stopped! Heater_ID: 0",
            "MAXTEMP triggered, system stopped! Heater_ID: 0",
            true,
        ),
        (
            "Error:Printer stopped due to errors. Fix the error and use M999 to restart. (Temperature is reset. Set it after restarting)",
            "Printer stopped due to errors. Fix the error and use M999 to restart. (Temperature is reset. Set it after restarting)",
            true,
        ),
        (
            "!! Must home axis first: 0.000 0.000 0.000 [0.000]",
            "Must home axis first: 0.000 0.000 0.000 [0.000]",
            false,
        ),
        (
            "!! Lost communication with MCU 'mcu'",
            "Lost communication with MCU 'mcu'",
            true,
        ),
        (
            "!! Shutdown due to M112 command",
            "Shutdown due to M112 command",
            true,
        ),
        (
            "!! MCU 'mcu' shutdown: Timer too close",
            "MCU 'mcu' shutdown: Timer too close",
            true,
        ),
        (
            "Error: G0/G1: insufficient axes homed",
            "G0/G1: insufficient axes homed",
            false,
        ),
        ("Error:Probing Failed", "Probing Failed", false),
        (
            "Error:STOP called because of BLTouch error - restart with M999",
            "STOP called because of BLTouch error - restart with M999",
            true,
        ),
        ("Error:", "", false),
    ];
    for (line, message, fatal) in cases {
        assert_eq!(one(line), err(message, fatal), "{line:?}");
    }
}

#[test]
fn unknown_commands() {
    assert_eq!(
        one("echo:Unknown command: \"M9999\""),
        Reply::UnknownCommand("M9999".into())
    );
    assert_eq!(
        one("echo:Unknown command: \"G12345 X1\""),
        Reply::UnknownCommand("G12345 X1".into())
    );
    assert_eq!(
        one("// Unknown command:\"FOO\""),
        Reply::UnknownCommand("FOO".into())
    );
    assert_eq!(
        one("!! Unknown command:\"BAR\""),
        Reply::UnknownCommand("BAR".into())
    );
}

#[test]
fn busy_wait_start() {
    for (line, msg) in [
        ("echo:busy: processing", "processing"),
        ("echo:busy: paused for user", "paused for user"),
        ("echo:busy: paused for input", "paused for input"),
        ("busy: processing", "processing"),
    ] {
        assert_eq!(one(line), Reply::Busy(msg.into()), "{line:?}");
    }
    assert_eq!(one("wait"), Reply::Wait);
    assert_eq!(one("start"), Reply::Start);
    assert_eq!(one("\u{0}\u{0}start\r"), Reply::Start);
    assert!(matches!(one("starting"), Reply::Other(_)));
}

#[test]
fn echo_info_and_extruder() {
    assert_eq!(one("echo:Active Extruder: 1"), Reply::ActiveExtruder(1));
    assert_eq!(
        one("echo: cold extrusion prevented"),
        Reply::Echo("cold extrusion prevented".into())
    );
    assert_eq!(
        one("echo:; Steps per unit:"),
        Reply::Echo("; Steps per unit:".into())
    );
    assert_eq!(
        one("echo:Marlin 2.1.2.1"),
        Reply::Echo("Marlin 2.1.2.1".into())
    );
    assert_eq!(
        one("echo:Active Extruder: 999"),
        Reply::Echo("Active Extruder: 999".into())
    );
    assert_eq!(
        one("// Klipper state: Ready"),
        Reply::Info("Klipper state: Ready".into())
    );
    assert_eq!(one("//"), Reply::Info(String::new()));
    assert_eq!(one("echo:"), Reply::Echo(String::new()));
}

#[test]
fn host_actions() {
    let cases = [
        ("//action:pause", HostAction::Pause),
        ("//action:paused", HostAction::Paused),
        ("//action:resume", HostAction::Resume),
        ("//action:resumed", HostAction::Resumed),
        ("//action:cancel", HostAction::Cancel),
        ("//action:disconnect", HostAction::Disconnect),
        ("//action:start", HostAction::Start),
        (
            "//action:out_of_filament T0",
            HostAction::OutOfFilament(Some("T0".into())),
        ),
        ("//action:out_of_filament", HostAction::OutOfFilament(None)),
        (
            "//action:notification Heating done",
            HostAction::Notification("Heating done".into()),
        ),
        (
            "//action:prompt_begin Filament runout",
            HostAction::PromptBegin("Filament runout".into()),
        ),
        (
            "//action:prompt_choice Continue",
            HostAction::PromptChoice("Continue".into()),
        ),
        (
            "//action:prompt_button Purge more",
            HostAction::PromptButton("Purge more".into()),
        ),
        ("//action:prompt_show", HostAction::PromptShow),
        ("//action:prompt_end", HostAction::PromptEnd),
        ("// action:cancel", HostAction::Cancel),
        (
            "//action:probe_rewipe",
            HostAction::Other("probe_rewipe".into()),
        ),
    ];
    for (line, action) in cases {
        assert_eq!(one(line), Reply::HostAction(action), "{line:?}");
    }
}

#[test]
fn firmware_and_capabilities() {
    let r = one(
        "FIRMWARE_NAME:Marlin 2.1.2.1 (Jul 29 2023 12:00:00) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:Ender-3 V2 EXTRUDER_COUNT:1 UUID:cede2a2f-41a2-4748-9b12-c55c62f367ff",
    );
    let Reply::Firmware(info) = r else { panic!() };
    assert_eq!(info.version.as_deref(), Some("2.1.2.1"));
    assert_eq!(
        one("Cap:AUTOREPORT_TEMP:1"),
        Reply::Capability {
            name: "AUTOREPORT_TEMP".into(),
            enabled: true
        }
    );
    assert_eq!(
        one("Cap:EEPROM:0"),
        Reply::Capability {
            name: "EEPROM".into(),
            enabled: false
        }
    );
    assert!(matches!(one("Cap:EEPROM:maybe"), Reply::Other(_)));
    assert!(matches!(one("Cap::1"), Reply::Other(_)));
}

#[test]
fn settings_lines() {
    let cases = [
        (
            "echo:  M92 X80.00 Y80.00 Z400.00 E93.00",
            "M92",
            "M92 X80 Y80 Z400 E93",
        ),
        (
            "echo:  M301 E1 P22.20 I1.08 D114.00",
            "M301 E1",
            "M301 E1 P22.2 I1.08 D114",
        ),
        (
            "echo:  M145 S0 H185.00 B45.00 F255",
            "M145 S0",
            "M145 S0 H185 B45 F255",
        ),
        ("echo:  M906 T0 E650", "M906 T0", "M906 T0 E650"),
        ("echo:  M906 I1 X800", "M906 I1", "M906 I1 X800"),
        ("echo:  M593 X F40.00 D0.15", "M593 X", "M593 X F40 D0.15"),
        ("echo:  M569 S1 X Y Z", "M569", "M569 S1 X Y Z"),
        (
            "echo:  M420 S0 Z10.00 ; Leveling OFF",
            "M420",
            "M420 S0 Z10",
        ),
        ("echo:  M149 C ; Units in Celsius", "M149", "M149 C"),
        ("echo:  G21    ; Units in mm (mm)", "G21", "G21"),
        (
            "echo:   M301 P16.13 I1.16 D56.23",
            "M301",
            "M301 P16.13 I1.16 D56.23",
        ),
        (
            "echo:M205 S0.00 T0.00 B20000 X8.00 Y8.00 Z0.40 E4.50",
            "M205",
            "M205 S0 T0 B20000 X8 Y8 Z0.4 E4.5",
        ),
        (
            "M92 X80.00000 Y80.00000 Z1600.00000",
            "M92",
            "M92 X80 Y80 Z1600",
        ),
    ];
    for (line, key, gcode) in cases {
        let Reply::Setting(cmd) = one(line) else {
            panic!("{line:?} is not a setting")
        };
        assert_eq!(cmd.key(), key, "{line:?}");
        assert_eq!(cmd.to_gcode(), gcode, "{line:?}");
    }
    // Unprefixed lines of unknown codes and messages stay Other.
    assert!(matches!(one("M117 Hello"), Reply::Other(_)));
    assert!(matches!(one("G28"), Reply::Other(_)));
    assert!(matches!(one("echo:M117 Printing..."), Reply::Echo(_)));
}

#[test]
fn temperature_and_position_lines() {
    assert!(matches!(one(" T:205.12 E:0 W:3"), Reply::Temperatures(_)));
    assert!(matches!(one(" T:205.1 E:0 B:59.8"), Reply::Temperatures(_)));
    assert!(matches!(
        one("T:21.6 /0.0 B:21.9 /0.0 T0:21.6 /0.0 @:0 B@:0 P:0.0 A:30.9"),
        Reply::Temperatures(t) if t.board == Some(30.9)
    ));
    assert!(matches!(
        one("X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0"),
        Reply::Position(_)
    ));
    assert!(matches!(
        one("X:0.00 Y:0.00 Z:0.15 E:0.00 Count X: 0.00 Y:-3.00 Z:0.15 E:0.00"),
        Reply::Position(_)
    ));
    assert!(matches!(
        one("X:0.000 Y:0.000 Z:0.000 E:0.000"),
        Reply::Position(_)
    ));
    assert!(matches!(
        one(
            "X:0.000 Y:0.000 Z:0.000 E:0.000 E0:-0.0 Count 0 0 0 Machine 0.000 0.000 0.000 Bed comp 0.000"
        ),
        Reply::Position(_)
    ));
    assert!(matches!(
        one("Bed X: 110.00 Y: 110.00 Z: 0.42"),
        Reply::Other(_)
    ));
}

#[test]
fn other_lines_are_kept_raw() {
    assert_eq!(
        one("  some random text  "),
        Reply::Other("some random text".into())
    );
    assert!(feed("").is_empty());
    assert!(feed("   \r\n").is_empty());
}

fn feed_all(parser: &mut ReplyParser, lines: &[&str]) -> Vec<Reply> {
    lines.iter().flat_map(|l| parser.feed(l)).collect()
}

fn file(name: &str, size: Option<u64>, long: Option<&str>) -> Reply {
    Reply::SdCard(SdReply::File(SdFile {
        name: name.into(),
        long_name: long.map(Into::into),
        size,
    }))
}

#[test]
fn sd_file_listing() {
    let mut p = ReplyParser::new();
    let replies = feed_all(
        &mut p,
        &[
            "Begin file list",
            "CUBE~1.GCO 12345",
            "CUBE~2.GCO 12345 cube_long_name.gcode",
            " T:210.00 /210.00 B:60.00 /60.00 @:64 B@:0",
            "/SUBDIR/FILE.GCO 123",
            "End file list",
            "ok",
            "CUBE~1.GCO 12345",
        ],
    );
    assert_eq!(replies[0], Reply::SdCard(SdReply::ListBegin));
    assert_eq!(replies[1], file("CUBE~1.GCO", Some(12345), None));
    assert_eq!(
        replies[2],
        file("CUBE~2.GCO", Some(12345), Some("cube_long_name.gcode"))
    );
    assert!(matches!(replies[3], Reply::Temperatures(_)));
    assert_eq!(replies[4], file("/SUBDIR/FILE.GCO", Some(123), None));
    assert_eq!(replies[5], Reply::SdCard(SdReply::ListEnd));
    assert_eq!(replies[6], ok(None, None, None));
    // Outside a listing, file-like lines are not files.
    assert_eq!(replies[7], Reply::Other("CUBE~1.GCO 12345".into()));
    assert_eq!(replies.len(), 8);
}

#[test]
fn sd_listing_closed_by_ok_when_end_is_lost() {
    let mut p = ReplyParser::new();
    let replies = feed_all(&mut p, &["Begin file list", "A.GCO 1", "ok", "B.GCO 2"]);
    assert_eq!(
        replies,
        vec![
            Reply::SdCard(SdReply::ListBegin),
            file("A.GCO", Some(1), None),
            Reply::SdCard(SdReply::ListEnd),
            ok(None, None, None),
            Reply::Other("B.GCO 2".into()),
        ]
    );
}

#[test]
fn sd_status_messages() {
    let cases = [
        ("echo:SD card ok", SdReply::CardOk),
        (
            "echo:SD init fail",
            SdReply::CardFailed("SD init fail".into()),
        ),
        ("echo:No SD card", SdReply::NoCard),
        (
            "File opened: x.gco Size: 12345",
            SdReply::Opened {
                name: "x.gco".into(),
                size: 12345,
            },
        ),
        (
            "echo:Now fresh file: x.gco",
            SdReply::Opened {
                name: "x.gco".into(),
                size: 0,
            },
        ),
        (
            "echo:Now doing file: x.gco",
            SdReply::Opened {
                name: "x.gco".into(),
                size: 0,
            },
        ),
        ("File selected", SdReply::Selected),
        (
            "SD printing byte 1234/5678",
            SdReply::Progress {
                done: 1234,
                total: 5678,
            },
        ),
        ("Not SD printing", SdReply::NotPrinting),
        ("Done printing file", SdReply::DonePrinting),
        ("Writing to file: x.gco", SdReply::WritingTo("x.gco".into())),
        ("Done saving file.", SdReply::Saved),
        ("File deleted:x.gco", SdReply::Deleted("x.gco".into())),
        (
            "Deletion failed, File: x.gco.",
            SdReply::DeleteFailed("x.gco".into()),
        ),
    ];
    for (line, expected) in cases {
        assert_eq!(one(line), Reply::SdCard(expected), "{line:?}");
    }
    assert_eq!(
        one("open failed, File: x.gco."),
        err("open failed, File: x.gco.", false)
    );
    assert_eq!(
        one("echo:open failed, File: x.gco."),
        err("open failed, File: x.gco.", false)
    );
}

#[test]
fn endstop_reports() {
    let mut p = ReplyParser::new();
    let replies = feed_all(
        &mut p,
        &[
            "Reporting endstop status",
            "x_min: open",
            "y_min: open",
            "z_probe: TRIGGERED",
            "filament: open",
            "ok",
        ],
    );
    let endstop = |name: &str, triggered: bool| Reply::Endstop {
        name: name.into(),
        triggered,
    };
    assert_eq!(
        replies,
        vec![
            Reply::Other("Reporting endstop status".into()),
            endstop("x_min", false),
            endstop("y_min", false),
            endstop("z_probe", true),
            endstop("filament", false),
            ok(None, None, None),
        ]
    );
    assert_eq!(
        feed("x:open y:open z:TRIGGERED"),
        vec![endstop("x", false), endstop("y", false), endstop("z", true)]
    );
}

#[test]
fn stats_and_print_time() {
    assert_eq!(
        one("echo:Stats: Prints: 12, Finished: 10, Failed: 2"),
        Reply::Stats(StatsReport {
            prints: Some(12),
            finished: Some(10),
            failed: Some(2),
            ..Default::default()
        })
    );
    assert_eq!(
        one("Stats: Total time: 2d 3h 4m 5s, Longest job: 1h 2m 3s"),
        Reply::Stats(StatsReport {
            total_time: Some(183_845),
            longest_job: Some(3723),
            ..Default::default()
        })
    );
    let Reply::Stats(s) = one("echo:Stats: Filament used: 1.50m") else {
        panic!("not stats")
    };
    assert_eq!(s.filament_used_mm, Some(1500.0));
    let mut merged = StatsReport::default();
    merged.merge(&StatsReport {
        prints: Some(3),
        ..Default::default()
    });
    merged.merge(&StatsReport {
        failed: Some(1),
        ..Default::default()
    });
    assert_eq!((merged.prints, merged.failed), (Some(3), Some(1)));

    assert_eq!(one("echo:Print time: 1h 2m 3s"), Reply::PrintTime(3723));
    assert_eq!(
        one("echo:Print time: 2d 3h 4m 5s"),
        Reply::PrintTime(183_845)
    );
    assert_eq!(one("echo:1 min, 23 sec"), Reply::PrintTime(83));
}
