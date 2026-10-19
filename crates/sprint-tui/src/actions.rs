//! Small decisions shared by the shell and the TUI: resolving connection
//! targets, presets, macros, settings edits and print options.

use std::path::Path;
use std::sync::Arc;

use sprint_host::sprint_proto::{GcodeDoc, PrinterSettings, SettingCommand, gcode_doc};
use sprint_host::{
    ConnectTarget, Heater, Macro, PrintOptions, PrinterProfile, PrinterState, TempPreset,
};

use crate::loader::LoadedFile;

/// The target of `connect [target] [baud]`: the profile's connection when
/// none is given, with `baud` overriding a serial target's rate.
pub fn resolve_target(
    target: Option<&str>,
    baud: Option<u32>,
    profile: &PrinterProfile,
) -> Result<ConnectTarget, String> {
    let mut resolved = match target {
        None => profile.connection.clone(),
        Some(text) => ConnectTarget::parse(text).map_err(|e| e.to_string())?,
    };
    if let Some(rate) = baud {
        match &mut resolved {
            ConnectTarget::Serial { baud, .. } => *baud = rate,
            other => {
                return Err(format!(
                    "a baud rate only applies to serial ports, not {}",
                    other.label()
                ));
            }
        }
    }
    Ok(resolved)
}

/// A fan speed in percent as the 0..=255 value of M106.
pub fn fan_value(percent: f32) -> u8 {
    let p = if percent.is_finite() {
        percent.clamp(0.0, 100.0)
    } else {
        0.0
    };
    // In 0..=255 by the clamp.
    (p / 100.0 * 255.0).round() as u8
}

/// The preset called `name` (case-insensitive; a unique prefix works).
pub fn find_preset<'a>(presets: &'a [TempPreset], name: &str) -> Result<&'a TempPreset, String> {
    let name = name.trim();
    if let Some(exact) = presets.iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
        return Ok(exact);
    }
    let lower = name.to_lowercase();
    let matches: Vec<&TempPreset> = presets
        .iter()
        .filter(|p| p.name.to_lowercase().starts_with(&lower))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        _ => {
            let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
            Err(if names.is_empty() {
                "the profile has no temperature presets".to_string()
            } else {
                format!("no preset '{name}' (presets: {})", names.join(", "))
            })
        }
    }
}

/// The macro called `name` (case-insensitive; a unique prefix works).
pub fn find_macro<'a>(macros: &'a [Macro], name: &str) -> Result<&'a Macro, String> {
    let name = name.trim();
    if let Some(exact) = macros.iter().find(|m| m.name.eq_ignore_ascii_case(name)) {
        return Ok(exact);
    }
    let lower = name.to_lowercase();
    let matches: Vec<&Macro> = macros
        .iter()
        .filter(|m| m.name.to_lowercase().starts_with(&lower))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no macro '{name}' (macro lists them)")),
        _ => Err(format!("'{name}' matches several macros")),
    }
}

/// Every heater the printer has: the hotends it reports (or the profile's
/// extruder count), the bed and the chamber when present.
pub fn heaters(state: &PrinterState, profile: &PrinterProfile) -> Vec<Heater> {
    let reported = state
        .firmware
        .as_ref()
        .and_then(|f| f.extruder_count)
        .map_or(0, usize::from);
    let hotends = reported
        .max(state.temps.hotends.len())
        .max(usize::from(profile.extruders))
        .clamp(1, 8);
    let mut out: Vec<Heater> = (0..hotends)
        .filter_map(|t| u8::try_from(t).ok())
        .map(Heater::Hotend)
        .collect();
    if state.temps.bed.is_some() || profile.heated_bed {
        out.push(Heater::Bed);
    }
    if state.temps.chamber.is_some() || profile.heated_chamber {
        out.push(Heater::Chamber);
    }
    out
}

/// A heater for messages: "Hotend T0", "Bed", "Chamber".
pub fn heater_label(heater: Heater) -> String {
    match heater {
        Heater::Hotend(t) => format!("Hotend T{t}"),
        Heater::Bed => "Bed".to_string(),
        Heater::Chamber => "Chamber".to_string(),
    }
}

/// "Hotend T0 210 °C, Bed off": what setting `targets` does.
pub fn targets_text(targets: &[(Heater, f32)]) -> String {
    targets
        .iter()
        .map(|&(heater, celsius)| {
            let value = if celsius.is_finite() && celsius > 0.0 {
                format!("{} °C", crate::format::trimmed(f64::from(celsius), 1))
            } else {
                "off".to_string()
            };
            format!("{} {value}", heater_label(heater))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A `set` edit merged into the setting the firmware reported (same key),
/// so the parameters not given keep their values.
pub fn merged_setting(settings: &PrinterSettings, edit: &SettingCommand) -> SettingCommand {
    match settings.get(&edit.key()) {
        Some(current) => {
            let mut merged = current.clone();
            for &(letter, value) in &edit.params {
                merged.set(letter, value);
            }
            merged
        }
        None => edit.clone(),
    }
}

/// An 8.3 SD card name for a local file: up to eight letters and digits of
/// its stem, upper case, with a `.GCO` extension.
pub fn sd_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut name: String = stem
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    if name.is_empty() {
        name = "PRINT".to_string();
    }
    format!("{name}.GCO")
}

/// Print options for a loaded file: its analysis drives progress, ETA and
/// layer tracking.
pub fn print_options(loaded: &LoadedFile) -> PrintOptions {
    PrintOptions {
        toolpath: Some(Arc::clone(&loaded.toolpath)),
        ..PrintOptions::default()
    }
}

/// The reference entry of a raw line when it is marked dangerous (factory
/// reset, file deletion, firmware update, ...), so a UI can ask first.
/// Emergency commands (M112, M108, M410, M876) never ask.
pub fn dangerous_gcode(line: &str) -> Option<&'static GcodeDoc> {
    let doc = gcode_doc(line.trim())?;
    let emergency = ["M112", "M108", "M410", "M876"].contains(&doc.code);
    (doc.dangerous && !emergency).then_some(doc)
}

/// Why a print cannot start, if it cannot.
pub fn print_blocker(state: &PrinterState, loaded: Option<&LoadedFile>) -> Option<String> {
    if !state.is_connected() {
        return Some("Not connected to a printer".to_string());
    }
    if state.halted {
        return Some("The printer is halted; restart it (M999) first".to_string());
    }
    if state.job_active() {
        return Some("A job is already running".to_string());
    }
    if loaded.is_none() {
        return Some("No file loaded".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::sprint_proto::{FirmwareInfo, HeaterReading, TempReport};
    use sprint_host::sprint_sim::SimConfig;

    #[test]
    fn targets_resolve_with_the_profile_and_baud() {
        let profile = PrinterProfile::default();
        assert_eq!(
            resolve_target(None, None, &profile),
            Ok(ConnectTarget::Simulator(SimConfig::default()))
        );
        assert_eq!(
            resolve_target(Some("COM3"), Some(250_000), &profile),
            Ok(ConnectTarget::Serial {
                port: "COM3".into(),
                baud: 250_000
            })
        );
        assert_eq!(
            resolve_target(Some("COM3@115200"), Some(0), &profile),
            Ok(ConnectTarget::Serial {
                port: "COM3".into(),
                baud: 0
            })
        );
        assert!(
            resolve_target(Some("sim"), Some(115_200), &profile)
                .unwrap_err()
                .contains("only applies to serial ports")
        );
        assert!(resolve_target(Some("ftp://x"), None, &profile).is_err());
    }

    #[test]
    fn fan_percentages() {
        assert_eq!(fan_value(0.0), 0);
        assert_eq!(fan_value(50.0), 128);
        assert_eq!(fan_value(100.0), 255);
        assert_eq!(fan_value(400.0), 255);
        assert_eq!(fan_value(f32::NAN), 0);
    }

    #[test]
    fn presets_and_macros_match_by_name_or_prefix() {
        let profile = PrinterProfile::default();
        let presets = &profile.temp_presets;
        assert_eq!(find_preset(presets, "pla").map(|p| p.hotend), Ok(205.0));
        assert_eq!(find_preset(presets, " PE ").map(|p| p.bed), Ok(80.0));
        let err = find_preset(presets, "nylon").unwrap_err();
        assert!(err.contains("PLA, PETG, ABS, TPU"), "{err}");
        assert!(
            find_preset(&[], "pla")
                .unwrap_err()
                .contains("no temperature presets")
        );
        let macros = sprint_host::AppConfig::default().macros;
        assert_eq!(
            find_macro(&macros, "home").map(|m| m.gcode.as_str()),
            Ok("G28")
        );
        assert_eq!(
            find_macro(&macros, "bed").map(|m| m.name.as_str()),
            Ok("Bed mesh")
        );
        assert!(find_macro(&macros, "zzz").is_err());
    }

    #[test]
    fn heaters_follow_the_printer() {
        let profile = PrinterProfile::default();
        let state = PrinterState::default();
        assert_eq!(heaters(&state, &profile), [Heater::Hotend(0), Heater::Bed]);
        let state = PrinterState {
            firmware: Some(FirmwareInfo {
                extruder_count: Some(2),
                ..FirmwareInfo::default()
            }),
            temps: TempReport {
                chamber: Some(HeaterReading::default()),
                ..TempReport::default()
            },
            ..PrinterState::default()
        };
        assert_eq!(
            heaters(&state, &profile),
            [
                Heater::Hotend(0),
                Heater::Hotend(1),
                Heater::Bed,
                Heater::Chamber
            ]
        );
    }

    #[test]
    fn heater_texts() {
        assert_eq!(
            targets_text(&[(Heater::Hotend(1), 215.5), (Heater::Bed, 0.0)]),
            "Hotend T1 215.5 °C, Bed off"
        );
        assert_eq!(targets_text(&[(Heater::Chamber, f32::NAN)]), "Chamber off");
        assert_eq!(targets_text(&[]), "");
    }

    #[test]
    fn edits_merge_into_reported_settings() {
        let mut settings = PrinterSettings::new();
        settings.insert(SettingCommand::parse("M92 X80 Y80 Z400 E93").expect("M92"));
        let edit = SettingCommand::parse("M92 E415").expect("edit");
        assert_eq!(
            merged_setting(&settings, &edit).to_gcode(),
            "M92 X80 Y80 Z400 E415"
        );
        let fresh = SettingCommand::parse("M900 K0.05").expect("edit");
        assert_eq!(merged_setting(&settings, &fresh).to_gcode(), "M900 K0.05");
    }

    #[test]
    fn sd_names_are_8_3() {
        assert_eq!(sd_name(Path::new("calibration-cube.gcode")), "CALIBRAT.GCO");
        assert_eq!(sd_name(Path::new("dir/v2 part.gco")), "V2PART.GCO");
        assert_eq!(sd_name(Path::new("---.gcode")), "PRINT.GCO");
    }

    #[test]
    fn dangerous_codes_but_not_emergencies() {
        assert_eq!(dangerous_gcode("M502").map(|d| d.code), Some("M502"));
        assert_eq!(dangerous_gcode("m30 file.gco").map(|d| d.code), Some("M30"));
        assert!(dangerous_gcode("M112").is_none());
        assert!(dangerous_gcode("G28").is_none());
        assert!(dangerous_gcode("BED_MESH_CALIBRATE").is_none());
    }

    #[test]
    fn print_needs_a_connection_and_a_file() {
        let offline = PrinterState::default();
        assert_eq!(
            print_blocker(&offline, None).as_deref(),
            Some("Not connected to a printer")
        );
        let connected = PrinterState {
            status: sprint_host::ConnectionStatus::Connected,
            ..PrinterState::default()
        };
        assert_eq!(
            print_blocker(&connected, None).as_deref(),
            Some("No file loaded")
        );
    }
}
