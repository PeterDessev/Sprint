//! Command dispatch and the simple commands that do not belong to a
//! subsystem module.

use super::{Machine, Origin, Outcome, TaskKind};
use crate::parse::Command;
use crate::rng::{crc16, fnv1a};
use crate::settings::{EEPROM_VERSION, Settings};

impl Machine {
    /// Execute one command.
    pub(super) fn dispatch(&mut self, c: &Command, origin: Origin) -> Outcome {
        if c.text.is_empty() {
            return Outcome::Done;
        }
        if c.sub.is_some() {
            return self.unknown(c);
        }
        if let Some(applied) = self.settings.apply(c, self.report_context()) {
            // M206 with offsets ends with report_current_position().
            let moved = c.is(b'M', 206) && c.has_params() && applied.output.is_empty();
            self.out.extend(applied.output);
            if moved {
                let line = self.position_line();
                self.emit(line);
            }
            return Outcome::Done;
        }
        match (c.letter, c.code) {
            (b'G', 0 | 1) => self.cmd_linear_move(c),
            (b'G', 2 | 3) => self.cmd_arc(c),
            (b'G', 4) => self.cmd_dwell(c),
            (b'G', 10) => self.cmd_firmware_retract(true),
            (b'G', 11) => self.cmd_firmware_retract(false),
            (b'G', 20) => {
                self.motion.inches = true;
                Outcome::Done
            }
            (b'G', 21) => {
                self.motion.inches = false;
                Outcome::Done
            }
            (b'G', 28) => self.cmd_home(c),
            (b'G', 29) => self.cmd_probe(),
            (b'G', 90) => {
                self.motion.absolute = true;
                self.motion.e_relative = None;
                Outcome::Done
            }
            (b'G', 91) => {
                self.motion.absolute = false;
                self.motion.e_relative = None;
                Outcome::Done
            }
            (b'G', 92) => self.cmd_set_position(c),
            (b'M', 17) => {
                self.motion.steppers = true;
                Outcome::Done
            }
            (b'M', 18 | 84) => {
                if c.seen('S') {
                    Outcome::Done
                } else {
                    Outcome::Task {
                        kind: TaskKind::DisableSteppers,
                        sync: true,
                    }
                }
            }
            // M1001 is queued internally at the end of an SD print.
            (b'M', 1001) if origin != Origin::Sd => self.unknown(c),
            (b'M', 20..=33 | 524 | 1001) | (b'M', 75..=78) => self.sd_or_job_command(c),
            (b'M', 73 | 300 | 111 | 120 | 121) => Outcome::Done,
            (b'M', 82) => {
                self.motion.e_relative = Some(false);
                Outcome::Done
            }
            (b'M', 83) => {
                self.motion.e_relative = Some(true);
                Outcome::Done
            }
            (b'M', 104) => self.cmd_set_hotend(c, false),
            (b'M', 109) => self.cmd_set_hotend(c, true),
            (b'M', 105) => self.cmd_report_temps(c),
            (b'M', 106) => self.cmd_fan(c, true),
            (b'M', 107) => self.cmd_fan(c, false),
            // Handled by the emergency parser on receipt.
            (b'M', 108 | 410 | 876) => Outcome::Done,
            // Line numbers are applied when the line is accepted.
            (b'M', 110) => Outcome::Done,
            (b'M', 112) => {
                self.kill();
                Outcome::NoOk
            }
            (b'M', 114) => {
                let line = self.position_line();
                self.emit(line);
                Outcome::Done
            }
            (b'M', 115) => self.cmd_firmware_info(),
            (b'M', 117) => {
                self.lcd_message = c.args.to_string();
                Outcome::Done
            }
            (b'M', 118) => self.cmd_serial_print(c),
            (b'M', 119) => self.cmd_endstops(),
            (b'M', 140) => self.cmd_set_bed(c, false),
            (b'M', 190) => self.cmd_set_bed(c, true),
            (b'M', 141) => self.cmd_set_chamber(c, false),
            (b'M', 191) => self.cmd_set_chamber(c, true),
            (b'M', 154) if self.cfg.auto_report_position => {
                if let Some(interval) = auto_interval(c) {
                    self.pos_interval = interval;
                    self.next_pos = self.now + interval;
                }
                Outcome::Done
            }
            (b'M', 155) if self.cfg.auto_report_temps => {
                if let Some(interval) = auto_interval(c) {
                    self.temp_interval = interval;
                    self.next_temp = self.now + interval;
                }
                Outcome::Done
            }
            (b'M', 211) => self.cmd_soft_endstops(c),
            (b'M', 220) => self.cmd_feedrate_pct(c),
            (b'M', 221) => self.cmd_flow_pct(c),
            (b'M', 290) => self.cmd_babystep(c),
            (b'M', 302) => self.cmd_cold_extrude(c),
            (b'M', 303) => self.cmd_autotune(c),
            (b'M', 400) => Outcome::Task {
                kind: TaskKind::Sync,
                sync: true,
            },
            (b'M', 420) => self.cmd_leveling(c),
            (b'M', 500) => {
                self.eeprom = self.settings.clone();
                self.eeprom_mesh = self.mesh.clone();
                let image = self.eeprom_image();
                self.emit(format!(
                    "echo:Settings Stored ({} bytes; crc {})",
                    image.len(),
                    crc16(&image)
                ));
                Outcome::Done
            }
            (b'M', 501) => {
                self.settings = self.eeprom.clone();
                self.mesh = self.eeprom_mesh.clone();
                if self.mesh.is_none() {
                    self.leveling = false;
                }
                let image = self.eeprom_image();
                self.emit(format!(
                    "echo:{EEPROM_VERSION} stored settings retrieved ({} bytes; crc {})",
                    image.len(),
                    crc16(&image)
                ));
                let report = self.settings.report(self.report_context(), false);
                self.out.extend(report);
                Outcome::Done
            }
            (b'M', 502) => {
                self.settings = Settings::defaults(self.hotends.len());
                self.emit("echo:Hardcoded Default Settings Loaded");
                Outcome::Done
            }
            (b'M', 503) => {
                let for_replay = c.value('S') == Some(0.0);
                let report = self.settings.report(self.report_context(), for_replay);
                self.out.extend(report);
                Outcome::Done
            }
            (b'M', 504) => {
                self.emit("echo:EEPROM OK");
                Outcome::Done
            }
            (b'M', 600) => self.cmd_filament_change(),
            (b'M', 999) => {
                if c.flag('S') {
                    Outcome::Done
                } else {
                    self.request_resend();
                    Outcome::NoOk
                }
            }
            (b'T', tool) => self.cmd_tool_change(usize::from(tool)),
            _ => self.unknown(c),
        }
    }

    pub(super) fn unknown(&mut self, c: &Command) -> Outcome {
        self.emit(format!("echo:Unknown command: \"{}\"", c.text));
        Outcome::Done
    }

    fn cmd_firmware_info(&mut self) -> Outcome {
        let uuid = self.uuid();
        self.emit(format!(
            "FIRMWARE_NAME:Marlin 2.1.2.1 (Sprint Virtual) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:{} EXTRUDER_COUNT:{} UUID:{}",
            self.cfg.machine_name,
            self.hotends.len(),
            uuid
        ));
        let caps: [(&str, bool); 35] = [
            ("SERIAL_XON_XOFF", false),
            ("BINARY_FILE_TRANSFER", false),
            ("EEPROM", true),
            ("VOLUMETRIC", true),
            ("AUTOREPORT_POS", self.cfg.auto_report_position),
            ("AUTOREPORT_TEMP", self.cfg.auto_report_temps),
            ("PROGRESS", false),
            ("PRINT_JOB", true),
            ("AUTOLEVEL", true),
            ("RUNOUT", true),
            ("Z_PROBE", true),
            ("LEVELING_DATA", true),
            ("BUILD_PERCENT", true),
            ("SOFTWARE_POWER", false),
            ("TOGGLE_LIGHTS", false),
            ("CASE_LIGHT_BRIGHTNESS", false),
            ("EMERGENCY_PARSER", true),
            ("HOST_ACTION_COMMANDS", true),
            ("PROMPT_SUPPORT", true),
            ("SDCARD", true),
            ("MULTI_VOLUME", false),
            ("REPEAT", false),
            ("SD_WRITE", true),
            ("AUTOREPORT_SD_STATUS", true),
            ("LONG_FILENAME", true),
            ("LFN_WRITE", true),
            ("CUSTOM_FIRMWARE_UPLOAD", false),
            ("EXTENDED_M20", true),
            ("THERMAL_PROTECTION", true),
            ("MOTION_MODES", false),
            ("ARCS", true),
            ("BABYSTEPPING", true),
            ("CHAMBER_TEMPERATURE", self.chamber.is_some()),
            ("COOLER_TEMPERATURE", false),
            ("ADVANCED_OK", self.cfg.advanced_ok),
        ];
        for (name, on) in caps {
            self.emit(format!("Cap:{name}:{}", u8::from(on)));
        }
        Outcome::Done
    }

    /// Deterministic UUID derived from the machine name and seed.
    pub(super) fn uuid(&self) -> String {
        let a = fnv1a(self.cfg.machine_name.as_bytes()) ^ self.cfg.seed;
        let b = fnv1a(&a.to_le_bytes()).rotate_left(17) ^ self.cfg.seed.rotate_right(9);
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&a.to_be_bytes());
        bytes[8..].copy_from_slice(&b.to_be_bytes());
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }

    /// M118: echo text to the host (A1 prefixes "//", E1 prefixes "echo:").
    fn cmd_serial_print(&mut self, c: &Command) -> Outcome {
        let mut rest = c.args;
        let mut echo = false;
        let mut action = false;
        for _ in 0..3 {
            let b = rest.as_bytes();
            if b.len() >= 2 && (b[0] == b'A' || b[0] == b'E') && b[1] == b'1' {
                if b[0] == b'A' {
                    action = true;
                } else {
                    echo = true;
                }
            } else if b.len() >= 2 && b[0] == b'P' && b[1].is_ascii_digit() {
            } else {
                break;
            }
            rest = rest[2..].trim_start();
        }
        let mut line = String::new();
        if echo {
            line.push_str("echo:");
        }
        if action {
            line.push_str("//");
        }
        line.push_str(rest);
        self.emit(line);
        Outcome::Done
    }

    fn cmd_tool_change(&mut self, tool: usize) -> Outcome {
        if tool >= self.hotends.len() {
            self.emit(format!("echo:T{tool} Invalid extruder"));
            return Outcome::Done;
        }
        if self.hotends.len() > 1 {
            self.motion.tool = tool;
            self.emit(format!("echo:Active Extruder: {tool}"));
        }
        Outcome::Done
    }
}

/// Interval in seconds for M154/M155/M27 S<n> (clamped to 0..=60), or
/// `None` without an S value (Marlin's `seenval('S')`: leave it as is).
pub(super) fn auto_interval(c: &Command) -> Option<f64> {
    c.value('S').map(|v| {
        if v.is_finite() {
            v.clamp(0.0, 60.0).trunc()
        } else {
            0.0
        }
    })
}
