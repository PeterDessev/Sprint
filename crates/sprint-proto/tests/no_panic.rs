//! Deterministic fuzz-style tests: thousands of pseudo-random and mutated
//! lines must never panic any parser, and parsed values must stay within
//! their documented ranges.

use sprint_proto::{
    FirmwareFlavor, Reply, ReplyParser, SettingCommand, capability_description, gcode_doc,
    is_emergency_command, parse_duration, parse_firmware_line, parse_framed, parse_position,
    parse_temperatures, search_gcode, setting_def,
};

/// xorshift64* — small, deterministic, good enough for test input.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const FRAGMENTS: &[&str] = &[
    "ok",
    "OK",
    "N",
    "N12",
    "P15",
    "B3",
    "T:",
    "T0:",
    "T1:",
    "T99:",
    "B:",
    "C:",
    "P:",
    "A:",
    "M:",
    "R:",
    "L:",
    "/",
    " / ",
    "@:",
    "@0:",
    "B@:",
    "C@:",
    "@",
    "@255",
    "X:",
    "Y:",
    "Z:",
    "E:",
    "E0:",
    "Count",
    "Machine",
    "W:?",
    "W:3",
    "echo:",
    "echo:busy:",
    "busy:",
    "Error:",
    "!!",
    "//",
    "// ",
    "action:",
    "//action:",
    "prompt_begin",
    "Resend:",
    "rs",
    "wait",
    "start",
    "Begin file list",
    "End file list",
    "Bilinear Leveling Grid:",
    "Bed Topography Report",
    "Measured points:",
    "Num X,Y:",
    "Z search height:",
    "// probed_matrix",
    "// mesh_matrix",
    "// Mesh Leveling Probed Z positions:",
    "[[",
    "]]",
    "[",
    "]",
    ",",
    "|",
    "=====",
    ".",
    "+0.050",
    "-0.012",
    "0.000",
    "1e309",
    "-1e309",
    "nan",
    "NaN",
    "inf",
    "-inf",
    "99999999999999999999",
    "4294967296",
    "-1",
    "0x5A3B",
    "Cap:",
    "Cap:EEPROM:",
    "FIRMWARE_NAME:",
    "FIRMWARE_VERSION:",
    "MACHINE_TYPE:",
    "EXTRUDER_COUNT:300",
    "PID Autotune start",
    "PID Autotune finished! from below",
    "PID Autotune failed!",
    "#define DEFAULT_Kp",
    "#define DEFAULT_bedKd",
    "Kp:",
    "Ki:",
    "Kd:",
    "PID parameters:",
    "pid_Kp=",
    "Stats:",
    "Prints:",
    "Total time:",
    "Filament used:",
    "Print time:",
    "1h",
    "2m",
    "3s",
    "min,",
    "sec",
    "File opened:",
    "Size:",
    "SD printing byte",
    "Not SD printing",
    "File deleted:",
    "Deletion failed, File:",
    "open failed",
    "M92",
    "M301",
    "M569",
    "M593",
    "M906",
    "G21",
    "M117",
    "M999",
    "M112",
    "(",
    ")",
    ";",
    "\"",
    "é",
    "Ω",
    "😀",
    "\t",
    "\r",
    "\u{0}",
    "\u{1b}[0m",
    " ",
    "  ",
    "1",
    "-",
    "+",
    "*",
    "*255",
    "x_min:",
    "open",
    "TRIGGERED",
    "Endstops - ",
    "not stopped",
    "Unknown command:",
    "Active Extruder:",
    "Reporting endstop status",
    "%3A",
    "%",
    "%G",
];

const REAL_LINES: &[&str] = &[
    "ok T:210.0 /210.0 B:60.0 /60.0 @:64 B@:0",
    "ok N12 P15 B3",
    "T:21.6 /0.0 B:21.9 /0.0 T0:21.6 /0.0 @:0 B@:0 P:0.0 A:30.9",
    "X:0.00 Y:0.00 Z:0.15 E:0.00 Count X: 0.00 Y:-3.00 Z:0.15 E:0.00",
    "X:0.000 Y:0.000 Z:0.000 E:0.000 E0:-0.0 Count 0 0 0 Machine 0.000 0.000 0.000 Bed comp 0.000",
    "Resend: 3",
    "Error:Printer stopped due to errors. Fix the error and use M999 to restart. (Temperature is reset. Set it after restarting)",
    "FIRMWARE_NAME:Marlin 2.1.2.1 (Jul 29 2023 12:00:00) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:Ender-3 V2 EXTRUDER_COUNT:1 UUID:cede2a2f-41a2-4748-9b12-c55c62f367ff",
    "FIRMWARE_NAME:Smoothieware, FIRMWARE_URL:http%3A//smoothieware.org, X-SOURCE_CODE_URL:https://github.com/Smoothieware/Smoothieware, FIRMWARE_VERSION:edge-1234abc",
    "Cap:AUTOREPORT_TEMP:1",
    "echo:  M92 X80.00 Y80.00 Z400.00 E93.00",
    "echo:  M569 S1 X Y Z",
    "echo:  M593 X F40.00 D0.15",
    "echo:  M420 S0 Z10.00 ; Leveling OFF",
    "Begin file list",
    "CUBE~1.GCO 12345 cube_long_name.gcode",
    "End file list",
    "File opened: x.gco Size: 12345",
    "SD printing byte 1234/5678",
    "Bilinear Leveling Grid:",
    " 0 +0.050 +0.025 -0.012 =====",
    "Bed Topography Report:",
    " 4 | +0.025  +0.050 [+0.075] -0.012   .   ",
    "    (  0,220)                                     (220,220)",
    "// probed_matrix: [[0.1, 0.2], [0.3, 0.4]]",
    "// Mesh Leveling Probed Z positions:",
    "//  0.012500 0.025000 0.037500",
    "PID Autotune start",
    " Kp: 22.20 Ki: 1.08 Kd: 114.00",
    "#define DEFAULT_Kp 22.20",
    "PID Autotune failed! timeout",
    "echo:Stats: Total time: 1d 2h 3m 4s, Longest job: 5h 6m 7s",
    "echo:Print time: 2d 3h 4m 5s",
    "//action:prompt_begin Filament runout",
    "x:open y:open z:TRIGGERED",
    "Endstops - X: not stopped, Y: at min stop, Z probe: not stopped",
    "echo:Unknown command: \"M9999\"",
];

fn random_line(rng: &mut Rng) -> String {
    let mut line = String::new();
    match rng.below(4) {
        // Token soup.
        0 | 1 => {
            for _ in 0..rng.below(14) {
                line.push_str(rng.pick(FRAGMENTS));
                if rng.below(3) == 0 {
                    line.push(' ');
                }
                if rng.below(4) == 0 {
                    line.push_str(&format!("{}", rng.next() as i64 % 100_000));
                }
                if rng.below(5) == 0 {
                    line.push_str(&format!("{}.{}", rng.below(1000), rng.below(1000)));
                }
            }
        }
        // Arbitrary characters, including control and multi-byte ones.
        2 => {
            for _ in 0..rng.below(80) {
                let c = match rng.below(4) {
                    0 => char::from(rng.below(128) as u8),
                    1 => char::from_u32(rng.below(0x3000) as u32).unwrap_or('?'),
                    _ => char::from(b' ' + rng.below(95) as u8),
                };
                line.push(c);
            }
        }
        // Mutated real line.
        _ => {
            let base = rng.pick(REAL_LINES);
            let chars: Vec<char> = base.chars().collect();
            let mut out: Vec<char> = chars.clone();
            for _ in 0..1 + rng.below(4) {
                if out.is_empty() {
                    break;
                }
                let at = rng.below(out.len());
                match rng.below(5) {
                    0 => out.truncate(at),
                    1 => {
                        out.remove(at);
                    }
                    2 => out.insert(at, rng.pick(FRAGMENTS).chars().next().unwrap_or('x')),
                    3 => {
                        if out[at].is_ascii_digit() {
                            out[at] = char::from(b'0' + rng.below(10) as u8);
                        } else {
                            out[at] = ':';
                        }
                    }
                    _ => {
                        let tail: Vec<char> = out[at..].to_vec();
                        out.extend(tail);
                    }
                }
            }
            line = out.into_iter().collect();
        }
    }
    line
}

fn check_reply(reply: &Reply) {
    match reply {
        Reply::Temperatures(t) => {
            assert!(t.hotends.len() <= 32);
            for h in t
                .hotends
                .iter()
                .chain(&t.bed)
                .chain(&t.chamber)
                .chain(&t.cooler)
            {
                if let Some(p) = h.power {
                    assert!((0.0..=1.0).contains(&p), "power {p}");
                }
                assert!(h.target.is_none_or(f32::is_finite));
            }
        }
        Reply::Mesh(m) => {
            assert!(
                m.rows() <= 257 && m.cols() <= 256,
                "{}x{}",
                m.rows(),
                m.cols()
            );
            for row in &m.points {
                assert_eq!(row.len(), m.cols());
            }
        }
        Reply::Position(p) => {
            assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite() && p.e.is_finite());
        }
        Reply::Setting(cmd) => {
            let _ = cmd.key();
            let again = SettingCommand::parse(&cmd.to_gcode());
            assert!(again.is_some(), "{cmd:?} does not re-parse");
        }
        _ => {}
    }
}

#[test]
fn parsers_never_panic_on_random_lines() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut parser = ReplyParser::new();
    let mut produced = 0usize;
    for i in 0..30_000 {
        let line = random_line(&mut rng);
        for reply in parser.feed(&line) {
            check_reply(&reply);
            produced += 1;
        }
        let _ = SettingCommand::parse(&line);
        let _ = parse_firmware_line(&line);
        let _ = parse_temperatures(&line);
        let _ = parse_position(&line);
        let _ = parse_duration(&line);
        let _ = parse_framed(&line);
        let _ = is_emergency_command(&line);
        let _ = gcode_doc(&line);
        let _ = setting_def(&line);
        let _ = capability_description(&line);
        let _ = FirmwareFlavor::detect(&line);
        if i % 10 == 0 {
            let _ = search_gcode(&line);
        }
        if i % 997 == 0 {
            for reply in parser.flush() {
                check_reply(&reply);
            }
        }
    }
    assert!(produced > 10_000, "only {produced} replies");
}

#[test]
fn every_prefix_of_real_lines_is_safe() {
    let mut parser = ReplyParser::new();
    for line in REAL_LINES {
        for (i, _) in line.char_indices() {
            for reply in parser.feed(&line[..i]) {
                check_reply(&reply);
            }
            for reply in parser.feed(&line[i..]) {
                check_reply(&reply);
            }
        }
    }
}

#[test]
fn huge_inputs_are_bounded() {
    let mut parser = ReplyParser::new();
    let long = "T:1.0 ".repeat(200_000);
    assert!(matches!(
        parser.feed(&long).as_slice(),
        [Reply::Temperatures(_)]
    ));
    let wide_row = format!(" 0 {}", "+0.1 ".repeat(5000));
    let _ = parser.feed("Bilinear Leveling Grid:");
    for _ in 0..2000 {
        assert!(parser.feed(&wide_row).is_empty());
    }
    let replies = parser.feed("ok");
    let [Reply::Mesh(mesh), Reply::Ok { .. }] = replies.as_slice() else {
        panic!("{:?}", replies.len());
    };
    assert!(mesh.cols() <= 256 && mesh.rows() <= 256);
    let _ = parser.feed("Begin file list");
    for i in 0..10_000 {
        let _ = parser.feed(&format!("FILE{i}.GCO {i}"));
    }
    assert_eq!(parser.feed("End file list").len(), 1);
}
