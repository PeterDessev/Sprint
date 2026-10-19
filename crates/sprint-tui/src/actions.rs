//! Small decisions shared by the shell and the TUI: resolving connection
//! targets, presets, macros, settings edits and print options.

use std::path::Path;
use std::sync::Arc;

use sprint_host::sprint_proto::{GcodeDoc, PrinterSettings, SettingCommand, gcode_doc};
use sprint_host::{
    ConnectTarget, Heater, JobInfo, JobKind, Macro, PrintOptions, PrinterProfile, PrinterState,
    TempPreset,
};

use crate::commands::TempTarget;
use crate::loader::LoadedFile;

/// What to connect to at startup: the target asked for, else the
/// profile's printer only when the profile turns `auto_connect` on (off by
/// default: a front end never opens a port by itself).
pub fn startup_target(
    asked: Option<ConnectTarget>,
    profile: &PrinterProfile,
) -> Option<ConnectTarget> {
    asked.or_else(|| profile.auto_connect.then(|| profile.connection.clone()))
}

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

/// The degrees a typed heater target stands for: its value, or the bed
/// (`bed`) or hotend temperature of the profile's preset it names.
pub fn resolve_temp(target: &TempTarget, presets: &[TempPreset], bed: bool) -> Result<f32, String> {
    match target {
        TempTarget::Celsius(celsius) => Ok(*celsius),
        TempTarget::Preset(name) => {
            find_preset(presets, name).map(|p| if bed { p.bed } else { p.hotend })
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

/// The index of the printer profile called `name` (case-insensitive; a
/// unique prefix works) or numbered `name` (1 is the first, as `profile`
/// lists them).
pub fn find_profile(profiles: &[PrinterProfile], name: &str) -> Result<usize, String> {
    let name = name.trim();
    if let Some(i) = profiles
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case(name))
    {
        return Ok(i);
    }
    if let Ok(n) = name.parse::<usize>() {
        return match n.checked_sub(1).filter(|&i| i < profiles.len()) {
            Some(i) => Ok(i),
            None => Err(format!(
                "there is no profile {n} (1 to {}; profile lists them)",
                profiles.len()
            )),
        };
    }
    let lower = name.to_lowercase();
    let matches: Vec<usize> = profiles
        .iter()
        .enumerate()
        .filter(|(_, p)| !lower.is_empty() && p.name.to_lowercase().starts_with(&lower))
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => {
            let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
            Err(format!(
                "no profile '{name}' (profiles: {})",
                names.join(", ")
            ))
        }
        _ => Err(format!("'{name}' matches several profiles")),
    }
}

/// The macro a `macro <name> [args…]` line runs, and its arguments. Macro
/// names may contain spaces ("Bed mesh"): the longest run of leading
/// words that names a macro wins, the words after it are the arguments.
/// Without such a name the whole text may be a unique prefix of one (no
/// arguments), and then its first word (the rest are arguments).
pub fn macro_call<'a>(macros: &'a [Macro], text: &str) -> Result<(&'a Macro, Vec<String>), String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    for take in (1..=words.len()).rev() {
        let name = words[..take].join(" ");
        if let Some(m) = macros.iter().find(|m| m.name.eq_ignore_ascii_case(&name)) {
            let args = words[take..].iter().map(|w| (*w).to_string()).collect();
            return Ok((m, args));
        }
    }
    if let Ok(m) = find_macro(macros, text) {
        return Ok((m, Vec::new()));
    }
    match words.split_first() {
        Some((first, rest)) if !rest.is_empty() => {
            find_macro(macros, first).map(|m| (m, rest.iter().map(|w| (*w).to_string()).collect()))
        }
        _ => find_macro(macros, text).map(|m| (m, Vec::new())),
    }
}

/// Define macro `name` with `gcode` in `macros`, replacing one with the
/// same name (case-insensitive; its colour and confirmation are kept).
/// Returns whether one was replaced.
pub fn define_macro(macros: &mut Vec<Macro>, name: &str, gcode: &str) -> bool {
    match macros
        .iter_mut()
        .find(|m| m.name.eq_ignore_ascii_case(name))
    {
        Some(existing) => {
            existing.gcode = gcode.to_string();
            true
        }
        None => {
            macros.push(Macro {
                name: name.to_string(),
                gcode: gcode.to_string(),
                color: None,
                confirm: false,
            });
            false
        }
    }
}

/// Remove the macro called `name` (case-insensitive, the whole name) from
/// `macros`; returns it.
pub fn remove_macro(macros: &mut Vec<Macro>, name: &str) -> Option<Macro> {
    let index = macros
        .iter()
        .position(|m| m.name.eq_ignore_ascii_case(name.trim()))?;
    Some(macros.remove(index))
}

/// A macro's G-code with its arguments filled in, as pronsole does:
/// `{0}`, `{1}`, … are the arguments in order, `{1=200}` gives a default,
/// and `{args}` is all of them. Other placeholders (`{x}`, `{max_z}`) are
/// left for the machine-state ones the host fills in. Fails when an
/// argument without a default is missing, or arguments are given to a
/// macro that takes none.
pub fn macro_gcode(m: &Macro, args: &[String]) -> Result<String, String> {
    let mut out = String::with_capacity(m.gcode.len());
    let mut rest = m.gcode.as_str();
    let mut takes_args = false;
    // Arguments up to the last one without a default.
    let mut needed = 0usize;
    let mut missing = false;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let inner = &after[..close];
        let (key, default) = match inner.split_once('=') {
            Some((key, default)) => (key.trim(), Some(default)),
            None => (inner.trim(), None),
        };
        if key == "args" {
            takes_args = true;
            out.push_str(&args.join(" "));
        } else if let Ok(index) = key.parse::<usize>() {
            takes_args = true;
            if default.is_none() {
                needed = needed.max(index + 1);
            }
            match (args.get(index), default) {
                (Some(arg), _) => out.push_str(arg),
                (None, Some(default)) => out.push_str(default),
                (None, None) => missing = true,
            }
        } else {
            out.push_str(&rest[open..open + close + 2]);
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    if missing {
        return Err(format!(
            "macro {} needs {needed} argument{} (macro {} <args…>)",
            m.name,
            if needed == 1 { "" } else { "s" },
            m.name
        ));
    }
    if !takes_args && !args.is_empty() {
        return Err(format!(
            "macro {} takes no arguments (its G-code has no {{0}} or {{args}})",
            m.name
        ));
    }
    Ok(out)
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

/// A heater target safe to send: 0..=500 °C; anything else (NaN or a
/// negative value from a broken preset) is off.
pub fn safe_target(celsius: f32) -> f32 {
    if celsius.is_finite() {
        celsius.clamp(0.0, 500.0)
    } else {
        0.0
    }
}

/// `targets` with [`safe_target`] values.
pub fn safe_targets(targets: &[(Heater, f32)]) -> Vec<(Heater, f32)> {
    targets
        .iter()
        .map(|&(heater, celsius)| (heater, safe_target(celsius)))
        .collect()
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

/// Codes that home Z or probe: the nozzle goes down onto the part.
const PROBING: &[&str] = &[
    "G29", "G30", "G32", "G33", "G34", "G35", "G38.2", "G38.3", "G38.4", "G38.5", "G76", "M48",
];
/// Codes that switch the motors off: the printer loses its position.
const MOTORS_OFF: &[&str] = &["M18", "M84"];
/// Codes that move the nozzle or change how moves are read.
const MOVES: &[&str] = &[
    "G0", "G1", "G2", "G3", "G5", "G6", "G27", "G28", "G42", "G60", "G61", "G80", "G90", "G91",
    "G92", "M82", "M83",
];

/// Why typed `line` would ruin the running job, if it would, with its
/// reference entry: homing Z, probing and switching the motors off (even
/// while the job is `paused`), and while it prints, moves and changes to
/// positioning or extrusion modes, which run between the print's lines.
/// What a print is tuned with (temperatures, fan, speed, flow, babysteps,
/// a filament change) never conflicts.
pub fn job_conflict(line: &str, paused: bool) -> Option<(&'static GcodeDoc, &'static str)> {
    let doc = gcode_doc(line.trim())?;
    let code = doc.code;
    let why = if PROBING.contains(&code) || (code == "G28" && homes_z(line)) {
        "homing Z and probing lower the nozzle onto the part"
    } else if MOTORS_OFF.contains(&code) {
        "with the motors off the printer loses the position the print goes on from"
    } else if MOVES.contains(&code) && !paused {
        "it runs between the print's lines, moving the nozzle or changing how they move"
    } else {
        return None;
    };
    Some((doc, why))
}

/// Whether a G28 line homes Z: when it names Z, or no axis at all.
fn homes_z(line: &str) -> bool {
    let command = line.split(';').next().unwrap_or_default().trim();
    // The parameters follow the code ("G28 X Y", "G28X").
    let params = command.get(1..).map_or("", |rest| {
        rest.trim_start_matches(|c: char| c.is_ascii_digit())
    });
    let axes: Vec<char> = params
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| matches!(c, 'X' | 'Y' | 'Z'))
        .collect();
    axes.is_empty() || axes.contains(&'Z')
}

/// Whether the firmware waits for the user without a host prompt to
/// answer (`busy: paused for user` after M0/M1 or a filament change on
/// firmware without host prompt support): M108 lets it go on.
pub fn waiting_for_user(state: &PrinterState) -> bool {
    state.prompt.is_none()
        && state.busy.as_deref().is_some_and(|busy| {
            let lower = busy.to_ascii_lowercase();
            lower.contains("paused for user") || lower.contains("wait for user")
        })
}

/// The question before cancelling a job, worded for what cancelling it
/// does (the same words in the shell and the TUI).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelQuestion {
    /// "Cancel the print?"
    pub title: String,
    /// What happens, a sentence per line.
    pub message: Vec<String>,
    /// The answer that cancels.
    pub yes: &'static str,
    /// The answer that goes on.
    pub no: &'static str,
}

impl CancelQuestion {
    /// The question on one line (the shell): what happens, then the
    /// question.
    pub fn line(&self) -> String {
        format!("{} {}", self.message.join(" "), self.title)
    }
}

/// What cancelling `job` does: a host print stops and the profile's
/// `cancel_script` runs (it can be printed again from a line later); an
/// SD print is aborted by the printer (M524) without a script; an upload
/// stops and the partial file is deleted from the card.
pub fn cancel_question(job: &JobInfo, cancel_script: &str) -> CancelQuestion {
    let name = &job.name;
    let at = crate::format::percent(f64::from(job.progress));
    match job.kind {
        JobKind::HostPrint => CancelQuestion {
            title: "Cancel the print?".into(),
            message: vec![
                format!("{name} stops at {at}."),
                match script_summary(cancel_script) {
                    Some(script) => format!("The cancel script runs: {script}."),
                    None => "The profile has no cancel script: the heaters stay on.".into(),
                },
                "It can be printed again from a line later.".into(),
            ],
            yes: "Cancel print",
            no: "Keep printing",
        },
        JobKind::SdPrint => CancelQuestion {
            title: "Stop the SD print?".into(),
            message: vec![
                format!("{name} on the printer's SD card stops at {at} (M524)."),
                "No cancel script runs: the printer ends the print itself.".into(),
            ],
            yes: "Stop SD print",
            no: "Keep printing",
        },
        JobKind::SdUpload => CancelQuestion {
            title: "Cancel the upload?".into(),
            message: vec![
                format!("Writing {name} to the SD card stops at {at}."),
                format!("The partial file {name} is deleted from the card."),
            ],
            yes: "Cancel upload",
            no: "Keep uploading",
        },
    }
}

/// A script's first commands: "M104 S0, M140 S0, M107, G91, … (8
/// commands)", or `None` when it has none.
fn script_summary(script: &str) -> Option<String> {
    /// Commands shown.
    const SHOWN: usize = 4;
    let lines: Vec<&str> = script
        .lines()
        .map(|l| l.split(';').next().unwrap_or_default().trim())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let mut text = lines[..lines.len().min(SHOWN)].join(", ");
    if lines.len() > SHOWN {
        text.push_str(&format!(", … ({} commands)", lines.len()));
    }
    Some(text)
}

/// Why no file may be deleted from the SD card now, if none may: while
/// the printer prints from the card (deleting the open file can corrupt
/// the card or abort the print), while a file is being written to it, or
/// while a listing comes in. The GUI's SD tab refuses in the same cases.
pub fn sd_delete_blocker(state: &PrinterState) -> Option<String> {
    let job = state.job.as_ref().filter(|j| j.status.is_active());
    match job.map(|j| (j.kind, j.name.as_str())) {
        Some((JobKind::SdPrint, name)) => {
            return Some(format!(
                "{name} is printing from the SD card: cancel it before deleting files"
            ));
        }
        Some((JobKind::SdUpload, name)) => {
            return Some(format!(
                "{name} is being written to the SD card: wait for it (or cancel it) first"
            ));
        }
        _ => {}
    }
    if state.sd.printing.is_some() {
        return Some(
            "The printer is printing from the SD card: cancel it before deleting files".into(),
        );
    }
    state
        .sd
        .listing
        .then(|| "The SD card is being listed; try again in a moment".to_string())
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
    fn startup_connects_only_when_asked_or_allowed() {
        let mut profile = PrinterProfile {
            connection: ConnectTarget::Serial {
                port: "COM9".into(),
                baud: 115_200,
            },
            ..PrinterProfile::default()
        };
        // The default profile never connects by itself.
        assert!(!PrinterProfile::default().auto_connect);
        assert_eq!(startup_target(None, &profile), None);
        let sim = ConnectTarget::Simulator(SimConfig::default());
        assert_eq!(
            startup_target(Some(sim.clone()), &profile),
            Some(sim.clone())
        );
        profile.auto_connect = true;
        assert_eq!(
            startup_target(None, &profile),
            Some(profile.connection.clone())
        );
        assert_eq!(startup_target(Some(sim.clone()), &profile), Some(sim));
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

    fn named(name: &str, gcode: &str) -> Macro {
        Macro {
            name: name.into(),
            gcode: gcode.into(),
            color: None,
            confirm: false,
        }
    }

    #[test]
    fn macros_take_arguments_like_pronsole() {
        let heat = named("heat", "M104 S{0}\nM140 S{1=60}\nM117 {args} at {x}");
        assert_eq!(
            macro_gcode(&heat, &["210".into()]).as_deref(),
            Ok("M104 S210\nM140 S60\nM117 210 at {x}")
        );
        assert_eq!(
            macro_gcode(&heat, &["200".into(), "70".into()]).as_deref(),
            Ok("M104 S200\nM140 S70\nM117 200 70 at {x}")
        );
        let missing = macro_gcode(&heat, &[]).expect_err("missing");
        assert!(missing.contains("needs 1 argument"), "{missing}");
        let home = named("Home", "G28 {unclosed");
        assert_eq!(macro_gcode(&home, &[]).as_deref(), Ok("G28 {unclosed"));
        let extra = macro_gcode(&home, &["5".into()]).expect_err("no arguments");
        assert!(extra.contains("takes no arguments"), "{extra}");
        // Names with spaces, then the arguments.
        let macros = [
            named("Bed mesh", "G29"),
            heat.clone(),
            named("Bed", "M140 S{0}"),
        ];
        let (m, args) = macro_call(&macros, "bed mesh").expect("bed mesh");
        assert_eq!((m.name.as_str(), args.len()), ("Bed mesh", 0));
        let (m, args) = macro_call(&macros, "Bed 55").expect("bed");
        assert_eq!((m.name.as_str(), args), ("Bed", vec!["55".to_string()]));
        let (m, args) = macro_call(&macros, "hea 1 2").expect("prefix");
        assert_eq!((m.name.as_str(), args.len()), ("heat", 2));
        assert!(macro_call(&macros, "nothing here").is_err());
        assert!(macro_call(&macros, "").is_err());
        // Defining replaces by name; removing takes the whole name.
        let mut list = macros.to_vec();
        assert!(define_macro(&mut list, "HEAT", "M104 S{0}"));
        assert_eq!(list[1].gcode, "M104 S{0}");
        assert!(!define_macro(&mut list, "park", "G91"));
        assert_eq!(list.len(), 4);
        assert_eq!(
            remove_macro(&mut list, "bed").map(|m| m.name),
            Some("Bed".into())
        );
        assert!(remove_macro(&mut list, "bed").is_none());
    }

    #[test]
    fn sd_files_are_not_deleted_while_the_card_is_busy() {
        let mut state = PrinterState::default();
        assert_eq!(sd_delete_blocker(&state), None);
        let mut job = JobInfo::new(JobKind::SdPrint, "ABSE.GCO", 0, 1000);
        job.status = sprint_host::JobStatus::Printing;
        state.job = Some(job.clone());
        let why = sd_delete_blocker(&state).expect("printing");
        assert!(
            why.contains("ABSE.GCO is printing from the SD card"),
            "{why}"
        );
        job.kind = JobKind::SdUpload;
        state.job = Some(job.clone());
        assert!(sd_delete_blocker(&state).is_some_and(|w| w.contains("being written")));
        // A host print does not use the card.
        job.kind = JobKind::HostPrint;
        state.job = Some(job.clone());
        assert_eq!(sd_delete_blocker(&state), None);
        // Started at the printer: no job, but the card is printing.
        state.sd.printing = Some((10, 100));
        assert!(sd_delete_blocker(&state).is_some());
        state.sd.printing = None;
        state.sd.listing = true;
        assert!(sd_delete_blocker(&state).is_some_and(|w| w.contains("listed")));
        // A finished SD print no longer blocks.
        state.sd.listing = false;
        job.kind = JobKind::SdPrint;
        job.status = sprint_host::JobStatus::Completed;
        state.job = Some(job);
        assert_eq!(sd_delete_blocker(&state), None);
    }

    #[test]
    fn cancelling_is_described_for_the_kind_of_job() {
        let job = |kind: JobKind| {
            let mut job = JobInfo::new(kind, "cube.gco", 100, 1000);
            job.progress = 0.52;
            job
        };
        let script = "M104 S0\nM140 S0 ; heaters\n\nM107\nG91\nG1 Z10\nG90\nM84\n";
        let print = cancel_question(&job(JobKind::HostPrint), script);
        assert_eq!(print.title, "Cancel the print?");
        assert_eq!((print.yes, print.no), ("Cancel print", "Keep printing"));
        assert_eq!(
            print.line(),
            "cube.gco stops at 52.0%. The cancel script runs: M104 S0, M140 S0, M107, G91, \
             … (7 commands). It can be printed again from a line later. Cancel the print?"
        );
        let bare = cancel_question(&job(JobKind::HostPrint), " ; nothing\n");
        assert!(bare.message[1].contains("no cancel script"), "{bare:?}");
        let sd = cancel_question(&job(JobKind::SdPrint), script);
        assert_eq!(sd.title, "Stop the SD print?");
        assert!(sd.message[0].contains("(M524)"), "{sd:?}");
        assert!(sd.message[1].contains("No cancel script runs"), "{sd:?}");
        let upload = cancel_question(&job(JobKind::SdUpload), script);
        assert_eq!(upload.title, "Cancel the upload?");
        assert_eq!((upload.yes, upload.no), ("Cancel upload", "Keep uploading"));
        assert!(
            upload.message[1].contains("partial file cube.gco is deleted"),
            "{upload:?}"
        );
        for q in [print, sd, upload] {
            assert!(!q.line().contains("cannot be resumed"), "{q:?}");
        }
    }

    #[test]
    fn profiles_are_found_by_name_prefix_or_number() {
        let named = |name: &str| PrinterProfile {
            name: name.into(),
            ..PrinterProfile::default()
        };
        let profiles = [named("Prusa MK3"), named("Ender 3"), named("Ender 5")];
        assert_eq!(find_profile(&profiles, "ender 3"), Ok(1));
        assert_eq!(find_profile(&profiles, " prusa "), Ok(0));
        assert_eq!(find_profile(&profiles, "3"), Ok(2));
        let several = find_profile(&profiles, "end").expect_err("ambiguous");
        assert!(several.contains("several"), "{several}");
        let none = find_profile(&profiles, "voron").expect_err("missing");
        assert!(none.contains("Prusa MK3, Ender 3, Ender 5"), "{none}");
        assert!(find_profile(&profiles, "4").is_err());
        assert!(find_profile(&profiles, "0").is_err());
        assert!(find_profile(&profiles, "").is_err());
        assert!(find_profile(&[], "x").is_err());
    }

    #[test]
    fn targets_are_made_safe() {
        assert_eq!(safe_target(210.0), 210.0);
        assert_eq!(safe_target(-5.0), 0.0);
        assert_eq!(safe_target(9999.0), 500.0);
        assert_eq!(safe_target(f32::NAN), 0.0);
        assert_eq!(
            safe_targets(&[(Heater::Bed, f32::INFINITY)]),
            [(Heater::Bed, 0.0)]
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
    fn typed_gcode_that_conflicts_with_a_job() {
        // (line, paused) -> conflicts
        let cases: &[(&str, bool, bool)] = &[
            // Homing Z and probing put the nozzle on the part, paused or not.
            ("G28", false, true),
            ("G28", true, true),
            ("g28 z", true, true),
            ("G28 O", true, true),
            ("G28 X Y", true, false),
            ("G28 X Y", false, true),
            ("G29", true, true),
            ("G38.2 Z-10", true, true),
            ("M48", true, true),
            // Motors off loses the position the print continues from.
            ("M84", true, true),
            ("M18 X", false, true),
            // Moves and mode changes go between the print's lines.
            ("G1 X10", false, true),
            ("G01X10", false, true),
            ("G91", false, true),
            ("G92 E0", false, true),
            ("M83", false, true),
            ("G1 Z5", true, false),
            ("G91", true, false),
            // What a print is tuned with.
            ("M220 S120", false, false),
            ("M221 S95", false, false),
            ("M290 Z0.05", false, false),
            ("M104 S215", false, false),
            ("M106 S255", false, false),
            ("M600", false, false),
            ("M105", false, false),
            ("BED_MESH_CALIBRATE", false, false),
            ("", false, false),
        ];
        for &(line, paused, conflicts) in cases {
            assert_eq!(
                job_conflict(line, paused).is_some(),
                conflicts,
                "{line:?} paused={paused}"
            );
        }
        let (doc, why) = job_conflict("G28", true).expect("G28");
        assert_eq!(doc.code, "G28");
        assert!(why.contains("nozzle"), "{why}");
    }

    #[test]
    fn waits_for_the_user_without_a_prompt() {
        let busy = |text: &str| PrinterState {
            busy: Some(text.to_string()),
            ..PrinterState::default()
        };
        assert!(waiting_for_user(&busy("paused for user")));
        assert!(waiting_for_user(&busy("Wait for user")));
        assert!(!waiting_for_user(&busy("processing")));
        assert!(!waiting_for_user(&PrinterState::default()));
        // With a prompt the prompt is answered instead.
        let mut prompted = busy("paused for user");
        prompted.prompt = Some(sprint_host::HostPrompt {
            message: "M0 Stop".into(),
            buttons: vec!["Continue".into()],
        });
        assert!(!waiting_for_user(&prompted));
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
