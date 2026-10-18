//! JSON round trips of the serialisable data types, including the NaN
//! sentinels (setting flags, missing mesh points, placeholder hotends) that
//! JSON writes as `null`.

use sprint_proto::{
    BedMesh, PrinterSettings, Reply, ReplyParser, SettingCommand, parse_temperatures,
};

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serialise");
    serde_json::from_str(&json).unwrap_or_else(|e| panic!("deserialise {json}: {e}"))
}

#[test]
fn setting_flags_survive_json() {
    let cmd = SettingCommand::parse("echo:  M569 S1 X Y Z").unwrap();
    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.contains("null"), "{json}");
    let back = round_trip(&cmd);
    assert_eq!(back, cmd);
    assert_eq!(back.to_gcode(), "M569 S1 X Y Z");
    assert!(back.get('X').unwrap().is_nan());

    let settings: PrinterSettings = [
        "M92 X80 Y80 Z400 E93",
        "M593 X F40 D0.15",
        "M149 C",
        "M569 S1 T0 E",
    ]
    .iter()
    .map(|l| SettingCommand::parse(l).unwrap())
    .collect();
    let back = round_trip(&settings);
    assert_eq!(back, settings);
    assert_eq!(
        back.get("M569 T0").map(|c| c.to_gcode()),
        Some("M569 S1 T0 E".into())
    );
}

#[test]
fn mesh_holes_survive_json() {
    let mut mesh = BedMesh::from_rows(
        vec![vec![0.1, f32::NAN, -0.2], vec![f32::NAN, 0.0, 0.05]],
        "bilinear",
    );
    mesh.set_bounds(10.0, 210.0, 10.0, 210.0);
    let back = round_trip(&mesh);
    assert_eq!(back, mesh);
    assert_eq!(back.count(), 4);
    assert!(back.points[1][0].is_nan());
}

#[test]
fn placeholder_hotends_survive_json() {
    let temps = parse_temperatures("T1:200.0 /205.0 B:60.0 /60.0 @1:64").unwrap();
    assert!(temps.hotends[0].current.is_nan());
    let back = round_trip(&temps);
    assert_eq!(back, temps);
    assert!(back.hotends[0].current.is_nan());
    assert_eq!(back.hotends[1].target, Some(205.0));
}

#[test]
fn a_whole_session_of_replies_survives_json() {
    let lines = [
        "start",
        "FIRMWARE_NAME:Marlin 2.1.2.1 (Jul 29 2023 12:00:00) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:Ender-3 V2 EXTRUDER_COUNT:1 UUID:cede2a2f-41a2-4748-9b12-c55c62f367ff",
        "Cap:AUTOREPORT_TEMP:1",
        "ok N3 P15 B3",
        "echo:  M569 S1 X Y Z",
        "echo:  M593 Y F37.50 D0.12",
        "ok T:210.0 /210.0 B:60.0 /60.0 T1:25.0 /0.0 @:64 B@:0",
        "X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0",
        "Bilinear Leveling Grid:",
        "      0      1",
        " 0 +0.100 =====",
        " 1 +0.080 +0.030",
        "",
        "Begin file list",
        "CUBE~1.GCO 12345 cube.gcode",
        "End file list",
        "echo:Stats: Filament used: 1.50m",
        "//action:prompt_begin Filament runout",
        "Error:Printer halted. kill() called!",
    ];
    let mut parser = ReplyParser::new();
    let replies: Vec<Reply> = lines.iter().flat_map(|l| parser.feed(l)).collect();
    assert!(replies.iter().any(|r| matches!(r, Reply::Mesh(_))));
    let back: Vec<Reply> = round_trip(&replies);
    assert_eq!(back, replies);
}
