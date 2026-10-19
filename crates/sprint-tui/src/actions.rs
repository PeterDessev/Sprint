//! Small decisions shared by the shell and the TUI: resolving connection
//! targets, presets, macros, settings edits and print options.

use std::path::Path;
use std::sync::Arc;

use sprint_host::sprint_proto::{
    Decision, JobContext, PrinterSettings, SafetyReason, SettingCommand, TempReport, Verdict,
    classify,
};
use sprint_host::{
    Axes, Axis, ConnectTarget, Heater, JobInfo, JobKind, JobStatus, Macro, PrintOptions, Printer,
    PrinterProfile, PrinterState, TempPreset,
};

use sprint_host::sprint_gcode::{ResumeOptions, resume_preamble};

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
                needed = needed.max(index.saturating_add(1));
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

/// The G-code that sets `heater` to `celsius` ("M104 T1 S215", "M140 S0",
/// "M141 S40"), for the safety check.
pub fn target_gcode(heater: Heater, celsius: f32) -> String {
    let value = crate::format::trimmed(f64::from(safe_target(celsius)), 1);
    match heater {
        Heater::Hotend(t) => format!("M104 T{t} S{value}"),
        Heater::Bed => format!("M140 S{value}"),
        Heater::Chamber => format!("M141 S{value}"),
    }
}

/// The G-code of `targets` for the safety check, a line per target that
/// changes something: switching off a heater whose target in `temps` is
/// off already is left out (nothing to ask about).
pub fn targets_gcode(temps: &TempReport, targets: &[(Heater, f32)]) -> String {
    targets
        .iter()
        .filter(|&&(heater, celsius)| safe_target(celsius) > 0.0 || target_set(temps, heater))
        .map(|&(heater, celsius)| target_gcode(heater, celsius))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `heater` has a target above 0 °C in `temps`.
fn target_set(temps: &TempReport, heater: Heater) -> bool {
    let reading = match heater {
        Heater::Hotend(t) => temps.hotends.get(usize::from(t)),
        Heater::Bed => temps.bed.as_ref(),
        Heater::Chamber => temps.chamber.as_ref(),
    };
    reading
        .and_then(|r| r.target)
        .is_some_and(|t| t.is_finite() && t > 0.0)
}

/// The M303 a PID autotune of `heater` sends, for the safety check.
pub fn autotune_gcode(heater: Heater, celsius: f32, cycles: u32) -> String {
    format!(
        "M303 E{} S{} C{cycles}",
        heater.autotune_index(),
        crate::format::trimmed(f64::from(safe_target(celsius)), 1)
    )
}

/// The G-code of `off` ([`crate::session::everything_off`]) for the safety
/// check: these heaters off, the fan off, the motors off (M84).
pub fn everything_off_gcode(heaters: &[Heater]) -> String {
    let mut lines: Vec<String> = heaters.iter().map(|&h| target_gcode(h, 0.0)).collect();
    lines.push("M107".into());
    lines.push("M84".into());
    lines.join("\n")
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

/// Print options for a loaded file with `profile`: its analysis drives
/// progress, ETA and layer tracking, and the progress shows on the
/// printer's display (M73) when the profile's `emit_progress` says so.
/// The scripts and hooks come from the profile the printer has.
pub fn print_options(loaded: &LoadedFile, profile: &PrinterProfile) -> PrintOptions {
    PrintOptions {
        toolpath: Some(Arc::clone(&loaded.toolpath)),
        emit_progress: profile.emit_progress,
        ..PrintOptions::default()
    }
}

/// A `--pause-at-layer` point: the print pauses (with the pause script)
/// before `line`, the first line of `layer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PausePoint {
    /// 0-based file line the job pauses before
    /// ([`Printer::pause_at_lines`]).
    pub line: usize,
    /// The layer, 1-based as the user gave it.
    pub layer: u64,
}

/// The pause points for `layers` (1-based) of `loaded`, in file order and
/// without repeats: each layer must exist and start at or after the line
/// the print starts from (`start_line`, 0-based).
pub fn pause_points(
    loaded: &LoadedFile,
    layers: &[u64],
    start_line: usize,
) -> Result<Vec<PausePoint>, String> {
    let count = loaded.toolpath.layers.len();
    let mut points = Vec::new();
    for &layer in layers {
        let found = usize::try_from(layer)
            .ok()
            .and_then(|l| l.checked_sub(1))
            .and_then(|i| loaded.toolpath.layers.get(i));
        let Some(found) = found else {
            return Err(format!(
                "--pause-at-layer {layer}: {} has {count} layer{}",
                loaded.name(),
                if count == 1 { "" } else { "s" }
            ));
        };
        let line = usize::try_from(found.lines.start).unwrap_or(usize::MAX);
        if line < start_line {
            return Err(format!(
                "--pause-at-layer {layer} starts before the line the print starts from"
            ));
        }
        points.push(PausePoint { line, layer });
    }
    points.sort_by_key(|p| p.line);
    points.dedup();
    Ok(points)
}

/// The running host print whose objects can be excluded, if one runs.
pub fn excludable_job(state: &PrinterState) -> Option<&JobInfo> {
    state
        .job
        .as_ref()
        .filter(|j| j.kind == JobKind::HostPrint && j.status.is_active())
}

/// The label of the object of `job` that `name` stands for: its name
/// (exact, then without regard to case, then a unique prefix), or its
/// number in the `objects` list (1 is the first). Errors are messages for
/// the user.
pub fn find_object<'a>(job: &'a JobInfo, name: &str) -> Result<&'a str, String> {
    let name = name.trim();
    let objects = &job.objects;
    if objects.is_empty() {
        return Err(format!(
            "{} has no object labels (the slicer's cancel-object option adds them)",
            job.name
        ));
    }
    if let Some(found) = objects.iter().find(|o| o.as_str() == name) {
        return Ok(found);
    }
    if let Some(found) = objects.iter().find(|o| o.eq_ignore_ascii_case(name)) {
        return Ok(found);
    }
    if let Some(found) = name
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| objects.get(i))
    {
        return Ok(found);
    }
    let lower = name.to_lowercase();
    let matches: Vec<&String> = objects
        .iter()
        .filter(|o| !lower.is_empty() && o.to_lowercase().starts_with(&lower))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "{} has no object '{name}' (objects lists them)",
            job.name
        )),
        _ => Err(format!(
            "'{name}' matches several objects (objects lists them)"
        )),
    }
}

/// How to print a file again from a line: the line and the resume
/// preamble that comes first.
#[derive(Clone, Debug, PartialEq)]
pub struct Recovery {
    /// 0-based line the print starts from.
    pub line: usize,
    /// The resume preamble ([`resume_preamble`]): heat and wait, fan,
    /// units and modes, home X and Y (never Z), lift, travel back to where
    /// `line` starts, restore E and the feedrate.
    pub preamble: Vec<String>,
    /// Z is not known to be homed (a reset, a halt, a new connection): the
    /// preamble declares the nozzle's Z with `G92 Z` (it must still be
    /// where the print stopped) instead of trusting the firmware's 0.
    pub declares_z: bool,
}

/// What `recover [line]` does with the loaded file: start from `line`
/// (1-based) when given, else from where the last host print of this very
/// file (the same path) failed or was cancelled
/// ([`JobInfo::stopped_at`]). Errors are messages for the user.
pub fn recovery(
    state: &PrinterState,
    loaded: Option<&LoadedFile>,
    line: Option<u64>,
) -> Result<Recovery, String> {
    let Some(loaded) = loaded else {
        return Err("No file loaded: load the file the print stopped in".to_string());
    };
    let name = loaded.name();
    let index = match line {
        Some(n) => {
            let index = usize::try_from(n.saturating_sub(1)).unwrap_or(usize::MAX);
            if index >= loaded.file.len() {
                return Err(format!(
                    "line {n} is past the end of {name} ({} lines)",
                    loaded.file.len()
                ));
            }
            index
        }
        None => {
            let stopped = state.job.as_ref().filter(|j| {
                j.kind == JobKind::HostPrint
                    && matches!(j.status, JobStatus::Failed(_) | JobStatus::Cancelled)
            });
            let Some(job) = stopped else {
                return Err(format!(
                    "No failed or cancelled print to recover (recover <line> starts {name} \
                     from a line)"
                ));
            };
            if job.path.as_deref() != loaded.file.path() || job.path.is_none() {
                return Err(format!(
                    "The print that stopped was {} from another file: load it to recover it",
                    job.name
                ));
            }
            let Some(index) = job.stopped_at else {
                return Err(format!(
                    "{name} did not stop at a known line (recover <line> starts it from a line)"
                ));
            };
            index
        }
    };
    let declares_z = !state.homed.is_some_and(|axes| axes.z);
    let options = ResumeOptions {
        restore_z_position: declares_z,
        ..ResumeOptions::default()
    };
    Ok(Recovery {
        line: index,
        preamble: resume_preamble(&loaded.file, index, &options),
        declares_z,
    })
}

/// Print options for `recovery` of `loaded` (see [`print_options`]).
pub fn recovery_options(
    loaded: &LoadedFile,
    profile: &PrinterProfile,
    recovery: &Recovery,
) -> PrintOptions {
    PrintOptions {
        start_line: recovery.line,
        preamble: recovery.preamble.clone(),
        ..print_options(loaded, profile)
    }
}

/// What a recovery will do, a sentence per line, then its preamble.
pub fn recovery_lines(recovery: &Recovery, name: &str) -> Vec<String> {
    let mut lines = vec![format!(
        "{name} prints again from line {}, after this preamble:",
        recovery.line + 1
    )];
    if recovery.declares_z {
        lines.push(
            "Z is not homed: the nozzle must still be where the print stopped (G92 Z declares it)."
                .to_string(),
        );
    }
    lines.extend(recovery.preamble.iter().map(|l| format!("  {l}")));
    lines
}

/// "2, 5, 10": the layers of `points`.
pub fn layers_text(points: &[PausePoint]) -> String {
    points
        .iter()
        .map(|p| p.layer.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// "Progress shows on the printer (M73)." when a print with `options`
/// sends it.
pub fn progress_note(options: &PrintOptions) -> Option<&'static str> {
    options
        .emit_progress
        .then_some("Progress shows on the printer's display (M73).")
}

/// What the job is doing, as the command-safety classifier
/// ([`classify`]) needs it: `Paused` only for a host print that is
/// paused (the host restores its position, modes and temperatures on
/// resume); any other active job, and a print from the SD card that was
/// started at the printer, is `Printing` (pausing, resuming and cancelling
/// still run the print's or a script's lines); otherwise `Idle`.
pub fn job_context(state: &PrinterState) -> JobContext {
    context_of(state.job.as_ref(), state.sd.printing.is_some())
}

/// [`job_context`] from the job record and whether the printer reports
/// printing from its SD card.
pub fn context_of(job: Option<&JobInfo>, sd_printing: bool) -> JobContext {
    match job.filter(|j| j.status.is_active()) {
        Some(job) if job.kind == JobKind::HostPrint && job.status == JobStatus::Paused => {
            JobContext::Paused
        }
        Some(_) => JobContext::Printing,
        None if sd_printing => JobContext::Printing,
        None => JobContext::Idle,
    }
}

/// A command line that [`classify`] does not let through as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flagged {
    /// The line as given.
    pub line: String,
    /// What the classifier said.
    pub verdict: Verdict,
}

impl Flagged {
    /// The command for messages: "G28 (Auto home)", the extended
    /// command's name, or the line itself.
    pub fn name(&self) -> String {
        match (self.verdict.doc, &self.verdict.command) {
            (Some(doc), _) => format!("{} ({})", doc.code, doc.name),
            (None, Some(command)) => command.clone(),
            (None, None) => self.line.trim().to_string(),
        }
    }

    /// The short command ("G28", "BED_MESH_CALIBRATE") or the line.
    pub fn code(&self) -> String {
        self.verdict
            .command
            .clone()
            .unwrap_or_else(|| self.line.trim().to_string())
    }

    /// Whether the reason is the running job (not the command itself).
    pub fn job_related(&self) -> bool {
        !matches!(
            self.verdict.reason(),
            None | Some(SafetyReason::Dangerous | SafetyReason::Restarts)
                | Some(SafetyReason::ForcesPosition)
        )
    }

    /// "G28 (Auto home): homing Z or probing lowers the nozzle …"; for a
    /// dangerous command its reference description says what it does.
    pub fn describe(&self) -> String {
        let why = match (self.verdict.reason(), self.verdict.doc) {
            (Some(SafetyReason::Dangerous), Some(doc)) => doc.description.to_string(),
            (Some(reason), _) => reason.message().to_string(),
            (None, _) => "it may be sent".to_string(),
        };
        format!("{}: {why}", self.name())
    }
}

/// What [`check_gcode`] found: the lines that need a confirmation and the
/// lines that must not be sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Safety {
    /// Lines to ask about first.
    pub confirm: Vec<Flagged>,
    /// Lines that must not be sent (the whole text is refused).
    pub blocked: Vec<Flagged>,
    /// The text has an emergency stop (M112): it goes out at once, never
    /// asked about.
    pub emergency_stop: bool,
}

impl Safety {
    /// Everything may be sent without asking.
    pub fn is_allowed(&self) -> bool {
        self.confirm.is_empty() && self.blocked.is_empty()
    }

    /// Some line needs a confirmation (and none is blocked).
    pub fn needs_confirmation(&self) -> bool {
        self.blocked.is_empty() && !self.confirm.is_empty()
    }

    /// Some line must not be sent.
    pub fn is_blocked(&self) -> bool {
        !self.blocked.is_empty()
    }

    /// Some line needs a yes because of the running job (not only because
    /// of what it is).
    pub fn job_related(&self) -> bool {
        self.confirm
            .iter()
            .chain(&self.blocked)
            .any(Flagged::job_related)
    }

    /// Why, a sentence per flagged line (blocked ones first), at most
    /// [`MAX_FLAGGED_SHOWN`] of them and then how many more there are.
    pub fn reasons(&self) -> Vec<String> {
        let all: Vec<&Flagged> = self.blocked.iter().chain(&self.confirm).collect();
        let mut out: Vec<String> = all
            .iter()
            .take(MAX_FLAGGED_SHOWN)
            .map(|f| f.describe())
            .collect();
        if all.len() > MAX_FLAGGED_SHOWN {
            out.push(format!("… and {} more", all.len() - MAX_FLAGGED_SHOWN));
        }
        out
    }
}

/// Flagged lines [`Safety::reasons`] lists.
pub const MAX_FLAGGED_SHOWN: usize = 4;

/// [`classify`] every line of `gcode` (a typed line, a pasted block, a
/// macro, a script) for a job in `ctx`.
pub fn check_gcode(gcode: &str, ctx: JobContext) -> Safety {
    let mut safety = Safety::default();
    for line in gcode.lines() {
        let verdict = classify(line, ctx);
        if verdict.emergency && verdict.command.as_deref() == Some("M112") {
            safety.emergency_stop = true;
        }
        let flagged = || Flagged {
            line: line.trim().to_string(),
            verdict: verdict.clone(),
        };
        match verdict.decision {
            Decision::Allow => {}
            Decision::Confirm(_) => safety.confirm.push(flagged()),
            Decision::Block(_) => safety.blocked.push(flagged()),
        }
    }
    safety
}

/// Why manual motion is refused while a job prints.
pub const NOT_WHILE_PRINTING: &str = "Not while a job runs (pause it first)";

/// A manual motion command (jog, move, extrude, home, tool change,
/// probing). They are refused while a job prints and checked with the
/// classifier's rules otherwise: while a host print is paused moves and
/// extrusion are fine (the host restores the position and modes on
/// resume), homing Z and probing ask first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// Move one axis by `mm` (E extrudes with the active tool).
    Jog {
        /// The axis.
        axis: Axis,
        /// Distance, signed.
        mm: f32,
        /// mm/min.
        feedrate: f32,
    },
    /// Extrude (negative: retract) with `tool`.
    Extrude {
        /// The tool.
        tool: u8,
        /// Length, signed.
        mm: f32,
        /// mm/min.
        feedrate: f32,
    },
    /// Home these axes; `zero_e` also sets the extruder position to 0.
    Home {
        /// Axes to home (none: only `zero_e`).
        axes: Axes,
        /// `G92 E0`.
        zero_e: bool,
    },
    /// Select a tool.
    Tool(u8),
    /// Probe the bed mesh (homing first unless homed).
    ProbeMesh,
}

impl Motion {
    /// The G-code it sends, for the safety check.
    pub fn gcode(&self) -> String {
        let number = |v: f32| sprint_host::script::format_number(f64::from(v), 3);
        match *self {
            Motion::Jog {
                axis: Axis::E,
                mm,
                feedrate,
            } => format!("M83\nG1 E{} F{}", number(mm), number(feedrate)),
            Motion::Jog { axis, mm, feedrate } => format!(
                "G91\nG1 {}{} F{}\nG90",
                axis.letter(),
                number(mm),
                number(feedrate)
            ),
            Motion::Extrude { mm, feedrate, .. } => {
                format!("M83\nG1 E{} F{}", number(mm), number(feedrate))
            }
            Motion::Home { axes, zero_e } => {
                let mut lines = Vec::new();
                if axes != Axes::default() {
                    lines.push(axes.home_command());
                }
                if zero_e {
                    lines.push("G92 E0".to_string());
                }
                lines.join("\n")
            }
            Motion::Tool(t) => format!("T{t}"),
            Motion::ProbeMesh => "G29".to_string(),
        }
    }

    /// The axes it moves, which should be homed first: a jog of X, Y or
    /// Z (not E, homing itself, a tool change or probing, which homes
    /// first).
    pub fn moves_axes(&self) -> Axes {
        match *self {
            Motion::Jog { axis: Axis::X, .. } => Axes::X,
            Motion::Jog { axis: Axis::Y, .. } => Axes::Y,
            Motion::Jog { axis: Axis::Z, .. } => Axes::Z,
            _ => Axes::NONE,
        }
    }

    /// Send it.
    pub fn run(&self, printer: &Printer) {
        match *self {
            Motion::Jog { axis, mm, feedrate } => printer.jog(axis, mm, feedrate),
            Motion::Extrude { tool, mm, feedrate } => printer.extrude(tool, mm, feedrate),
            Motion::Home { axes, zero_e } => {
                if axes != Axes::default() {
                    printer.home(axes);
                }
                if zero_e {
                    printer.send("G92 E0");
                }
            }
            Motion::Tool(t) => printer.select_tool(t),
            Motion::ProbeMesh => printer.probe_bed_mesh(),
        }
    }
}

/// The axes of `axes` not known to be homed: all of them while homing
/// is unknown (since connecting), else those no acknowledged G28 homed
/// since a halt, a reset or the motors going off
/// ([`PrinterState::homed`]).
pub fn not_homed(state: &PrinterState, axes: Axes) -> Axes {
    match state.homed {
        None => axes,
        Some(homed) => axes.without(homed),
    }
}

/// The axes a line of G-code homes: those a `G28` names, or all of them
/// for a bare `G28`.
pub fn homed_by(gcode: &str) -> Axes {
    gcode
        .lines()
        .map(sprint_host::sprint_gcode::parse_line)
        .filter(|line| line.code.is_some_and(|c| c.is('G', 28)))
        .fold(Axes::NONE, |all, line| {
            let named = Axes {
                x: line.has('X'),
                y: line.has('Y'),
                z: line.has('Z'),
            };
            all.union(if named.is_empty() { Axes::ALL } else { named })
        })
}

/// Axes the user asked to home whose `G28` may not be acknowledged yet:
/// they count as homed while the link is busy (the move typed after
/// `home` runs after it), and are forgotten once it has been idle for a
/// moment ([`crate::session::IDLE_SETTLE`]: a command sent a moment ago
/// may not show as queued yet), when [`PrinterState::homed`] says what
/// happened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HomingRequests {
    axes: Axes,
    /// When the last homing was sent.
    since: Option<std::time::Instant>,
}

impl HomingRequests {
    /// `gcode` went to the printer: remember what it homes.
    pub fn sent(&mut self, gcode: &str) {
        let axes = homed_by(gcode);
        if !axes.is_empty() {
            self.axes = self.axes.union(axes);
            self.since = Some(std::time::Instant::now());
        }
    }

    /// The axes still on their way to being homed.
    pub fn pending(&mut self, state: &PrinterState) -> Axes {
        let settled = self
            .since
            .is_none_or(|t| t.elapsed() >= crate::session::IDLE_SETTLE);
        if settled && crate::session::link_idle(state) {
            *self = HomingRequests::default();
        }
        self.axes
    }
}

/// The warning for something that needs `axes` homed when some are not
/// (counting the `homing` ones as homed), such as "X Z not homed: the
/// move starts from where the printer believes it is (home x z first)".
pub fn homing_warning(
    state: &PrinterState,
    axes: Axes,
    homing: Axes,
    consequence: &str,
) -> Option<String> {
    let missing = not_homed(state, axes).without(homing);
    (!missing.is_empty()).then(|| {
        format!(
            "{} not homed: {consequence} (home {} first)",
            missing.letters(),
            missing.letters().to_lowercase()
        )
    })
}

/// What a move of axes that are not homed does.
pub const UNHOMED_MOVE: &str = "the move starts from where the printer believes it is";

/// What a babystep while Z is not homed does.
pub const UNHOMED_BABYSTEP: &str = "babysteps shift a Z the printer does not know";

/// "cube.gcode is printing." / "cube.gcode is paused." for messages about
/// the running job, or `None` when idle.
pub fn job_sentence(ctx: JobContext, job: Option<&str>) -> Option<String> {
    let name = job.unwrap_or("A job");
    match ctx {
        JobContext::Idle => None,
        JobContext::Printing => Some(format!("{name} is printing.")),
        JobContext::Paused => Some(format!("{name} is paused.")),
    }
}

/// The name of the active job, if any.
pub fn active_job_name(state: &PrinterState) -> Option<String> {
    state
        .job
        .as_ref()
        .filter(|j| j.status.is_active())
        .map(|j| j.name.clone())
}

/// A script or macro's G-code as it will be sent, its `{x}`, `{max_z}`,
/// `{heaters_off}` ... placeholders rendered with the profile (the host
/// renders them again with the printer's position when it sends them), so
/// previews and the safety check see real commands.
pub fn rendered_gcode(gcode: &str, profile: &PrinterProfile) -> String {
    let state = sprint_host::sprint_gcode::MachineState::new();
    let ctx = profile.script_context(&state);
    sprint_host::script::render_script_with(gcode, &ctx).join("\n")
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

/// The profile's cancel script with its placeholders rendered (for
/// example `{heaters_off}` becomes one `M104` per hotend plus `M140 S0`),
/// ready for [`cancel_question`].
pub fn rendered_cancel_script(profile: &PrinterProfile) -> String {
    rendered_gcode(&profile.cancel_script, profile)
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

/// How to bring a halted printer back, naming the reset command as the
/// front end spells it (`reset`, `:reset`): after a Marlin stop() (or a
/// Klipper shutdown) M999 restarts the firmware; after M112, a thermal
/// runaway or another kill() only a board reset does.
pub fn halt_recovery(restartable: bool, reset: &str) -> String {
    if restartable {
        format!("M999 restarts the firmware ({reset} resets the board)")
    } else {
        format!("{reset} resets the board (M999 alone does not recover from M112 or a kill)")
    }
}

/// Why a print cannot start, if it cannot.
pub fn print_blocker(state: &PrinterState, loaded: Option<&LoadedFile>) -> Option<String> {
    if !state.is_connected() {
        return Some("Not connected to a printer".to_string());
    }
    if state.halted {
        return Some(format!(
            "The printer is halted: {}",
            halt_recovery(state.restartable, "reset")
        ));
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
        // Any placeholder, however large its number, is an error at most.
        let huge = named("huge", &format!("M117 {{{}}}", usize::MAX));
        assert!(macro_gcode(&huge, &[]).is_err());
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

    fn loaded_from_disk(dir: &Path, text: &str) -> LoadedFile {
        let path = dir.join("part.gcode");
        std::fs::write(&path, text).expect("write");
        let file = sprint_host::sprint_gcode::GcodeFile::load(&path).expect("load");
        let toolpath = sprint_host::sprint_gcode::analyze_simple(
            &file,
            &sprint_host::sprint_gcode::AnalyzeOptions::default(),
        );
        LoadedFile::new(path, file, toolpath, Default::default())
    }

    #[test]
    fn objects_are_found_by_name_prefix_or_number() {
        let mut job = JobInfo::new(JobKind::HostPrint, "plate.gcode", 10, 100);
        let err = find_object(&job, "x").expect_err("no labels");
        assert!(err.contains("plate.gcode has no object labels"), "{err}");
        job.objects = vec![
            "Cube".into(),
            "Cylinder".into(),
            "cube copy".into(),
            "2".into(),
        ];
        assert_eq!(find_object(&job, " Cube "), Ok("Cube"));
        assert_eq!(find_object(&job, "cylinder"), Ok("Cylinder"));
        assert_eq!(find_object(&job, "cyl"), Ok("Cylinder"));
        // A name that is a number wins over the position.
        assert_eq!(find_object(&job, "2"), Ok("2"));
        assert_eq!(find_object(&job, "3"), Ok("cube copy"));
        assert!(
            find_object(&job, "cu")
                .unwrap_err()
                .contains("several objects")
        );
        assert!(
            find_object(&job, "sphere")
                .unwrap_err()
                .contains("no object 'sphere'")
        );
        assert!(find_object(&job, "9").is_err());
        // Only a running host print can have objects excluded.
        let mut state = PrinterState {
            job: Some(job.clone()),
            ..PrinterState::default()
        };
        assert!(excludable_job(&state).is_some());
        job.status = JobStatus::Completed;
        state.job = Some(job.clone());
        assert!(excludable_job(&state).is_none());
        job.status = JobStatus::Printing;
        job.kind = JobKind::SdPrint;
        state.job = Some(job);
        assert!(excludable_job(&state).is_none());
    }

    #[test]
    fn moves_of_axes_not_homed_are_pointed_out() {
        let jog = |axis| Motion::Jog {
            axis,
            mm: 1.0,
            feedrate: 600.0,
        };
        assert_eq!(jog(Axis::Z).moves_axes(), Axes::Z);
        assert_eq!(jog(Axis::E).moves_axes(), Axes::NONE);
        assert_eq!(Motion::ProbeMesh.moves_axes(), Axes::NONE);
        // Unknown since connecting: every axis.
        let mut state = PrinterState::default();
        assert_eq!(not_homed(&state, Axes::XY), Axes::XY);
        assert_eq!(
            homing_warning(&state, Axes::Z, Axes::NONE, UNHOMED_BABYSTEP).as_deref(),
            Some("Z not homed: babysteps shift a Z the printer does not know (home z first)")
        );
        state.homed = Some(Axes::XY);
        assert_eq!(
            homing_warning(&state, Axes::XY, Axes::NONE, UNHOMED_MOVE),
            None
        );
        assert_eq!(not_homed(&state, Axes::ALL), Axes::Z);
        // A G28 Z on its way counts while the link is busy.
        assert_eq!(homing_warning(&state, Axes::Z, Axes::Z, UNHOMED_MOVE), None);
        let mut requests = HomingRequests::default();
        requests.sent("M105\ng28 z ; home Z\n");
        // Sent a moment ago: not queued yet, still counted.
        assert_eq!(requests.pending(&state), Axes::Z);
        state.link.queue_len = 1;
        std::thread::sleep(crate::session::IDLE_SETTLE);
        assert_eq!(requests.pending(&state), Axes::Z);
        // Done (idle, and long enough ago): the state says.
        state.link.queue_len = 0;
        assert_eq!(requests.pending(&state), Axes::NONE);
        assert_eq!(homed_by("G28"), Axes::ALL);
        assert_eq!(homed_by("G28 X Y\nG1 X1"), Axes::XY);
        assert_eq!(homed_by("G1 X1"), Axes::NONE);
        state.homed = Some(Axes::NONE);
        assert!(
            homing_warning(&state, Axes::ALL, Axes::NONE, UNHOMED_MOVE).is_some_and(|w| w
                .starts_with("X Y Z not homed: ")
                && w.ends_with("(home x y z first)"))
        );
    }

    #[test]
    fn a_stopped_print_is_recovered_from_its_line() {
        let dir = std::env::temp_dir().join(format!("sprint-tui-recover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let loaded = loaded_from_disk(
            &dir,
            "M104 S200\nM140 S60\nG28\nG90\nM83\nG1 Z0.2 F600\nG1 X10 Y10 E1\nG1 Z0.4\n\
             G1 X20 Y20 E1\nG1 X30 Y20 E1\n",
        );
        let mut state = PrinterState::default();
        assert!(
            recovery(&state, None, None)
                .unwrap_err()
                .starts_with("No file loaded")
        );
        let none = recovery(&state, Some(&loaded), None).expect_err("nothing stopped");
        assert!(none.contains("No failed or cancelled print"), "{none}");
        // The print of this very file failed before line 9 (0-based 8).
        let mut job = JobInfo::new(JobKind::HostPrint, "part.gcode", 10, 100);
        job.path = loaded.file.path().map(Path::to_path_buf);
        job.status = JobStatus::Failed("printer halted".into());
        job.stopped_at = Some(8);
        state.job = Some(job.clone());
        let plan = recovery(&state, Some(&loaded), None).expect("recovery");
        assert_eq!(plan.line, 8);
        // Z unknown (a halt, a reset): declared where the print stopped.
        assert!(plan.declares_z);
        assert!(
            plan.preamble.iter().any(|l| l == "G92 Z0.4"),
            "{:?}",
            plan.preamble
        );
        assert!(
            plan.preamble.iter().any(|l| l == "G28 X Y"),
            "{:?}",
            plan.preamble
        );
        let lines = recovery_lines(&plan, loaded.name());
        assert_eq!(
            lines[0],
            "part.gcode prints again from line 9, after this preamble:"
        );
        assert!(lines[1].starts_with("Z is not homed"), "{lines:?}");
        let options = recovery_options(&loaded, &PrinterProfile::default(), &plan);
        assert_eq!(options.start_line, 8);
        assert_eq!(options.preamble, plan.preamble);
        // Z homed: the firmware's Z is trusted.
        state.homed = Some(sprint_host::Axes::ALL);
        let plan = recovery(&state, Some(&loaded), None).expect("recovery");
        assert!(!plan.declares_z);
        assert!(!plan.preamble.iter().any(|l| l.starts_with("G92 Z")));
        // A line given; past the end is refused.
        assert_eq!(
            recovery(&state, Some(&loaded), Some(3)).map(|r| r.line),
            Ok(2)
        );
        let past = recovery(&state, Some(&loaded), Some(99)).expect_err("past the end");
        assert!(
            past.contains("past the end of part.gcode (10 lines)"),
            "{past}"
        );
        // The print that stopped was another file of the same name.
        job.path = Some(dir.join("elsewhere").join("part.gcode"));
        state.job = Some(job.clone());
        let other = recovery(&state, Some(&loaded), None).expect_err("another file");
        assert!(other.contains("from another file"), "{other}");
        // A completed print has nothing to recover.
        job.path = loaded.file.path().map(Path::to_path_buf);
        job.status = JobStatus::Completed;
        state.job = Some(job);
        assert!(recovery(&state, Some(&loaded), None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn print_options_follow_the_profile() {
        let file = sprint_host::sprint_gcode::GcodeFile::from_text(
            "a.gcode",
            "G28
G1 X1
",
        );
        let toolpath = sprint_host::sprint_gcode::analyze_simple(
            &file,
            &sprint_host::sprint_gcode::AnalyzeOptions::default(),
        );
        let loaded = LoadedFile::new("a.gcode".into(), file, toolpath, Default::default());
        let mut profile = PrinterProfile::default();
        let options = print_options(&loaded, &profile);
        assert!(options.toolpath.is_some() && !options.emit_progress);
        assert_eq!(progress_note(&options), None);
        profile.emit_progress = true;
        let options = print_options(&loaded, &profile);
        assert!(options.emit_progress);
        assert!(progress_note(&options).is_some_and(|n| n.contains("M73")));
    }

    #[test]
    fn sd_names_are_8_3() {
        assert_eq!(sd_name(Path::new("calibration-cube.gcode")), "CALIBRAT.GCO");
        assert_eq!(sd_name(Path::new("dir/v2 part.gco")), "V2PART.GCO");
        assert_eq!(sd_name(Path::new("---.gcode")), "PRINT.GCO");
    }

    fn job_state(kind: JobKind, status: JobStatus) -> PrinterState {
        let mut job = JobInfo::new(kind, "cube.gcode", 100, 1000);
        job.status = status;
        PrinterState {
            job: Some(job),
            ..PrinterState::default()
        }
    }

    #[test]
    fn the_job_context_comes_from_the_job() {
        assert_eq!(job_context(&PrinterState::default()), JobContext::Idle);
        let host = |status| job_context(&job_state(JobKind::HostPrint, status));
        assert_eq!(host(JobStatus::Printing), JobContext::Printing);
        assert_eq!(host(JobStatus::Starting), JobContext::Printing);
        // Pausing still streams (or waits for) the print's lines.
        assert_eq!(host(JobStatus::Pausing), JobContext::Printing);
        assert_eq!(host(JobStatus::Paused), JobContext::Paused);
        assert_eq!(host(JobStatus::Resuming), JobContext::Printing);
        assert_eq!(host(JobStatus::Completed), JobContext::Idle);
        assert_eq!(host(JobStatus::Failed("x".into())), JobContext::Idle);
        // The host does not restore an SD print's position on resume.
        let sd = job_state(JobKind::SdPrint, JobStatus::Paused);
        assert_eq!(job_context(&sd), JobContext::Printing);
        // A card print started at the printer.
        let mut card = PrinterState::default();
        card.sd.printing = Some((10, 100));
        assert_eq!(job_context(&card), JobContext::Printing);
    }

    #[test]
    fn lines_are_checked_with_the_shared_classifier() {
        // Idle: only what is dangerous in itself asks.
        let idle = check_gcode("G28\nG1 X10\nM502\n; note\n", JobContext::Idle);
        assert!(idle.needs_confirmation() && !idle.job_related());
        assert_eq!(idle.confirm.len(), 1);
        assert_eq!(idle.confirm[0].code(), "M502");
        assert_eq!(idle.confirm[0].name(), "M502 (Factory reset)");
        let why = idle.confirm[0].describe();
        assert!(why.starts_with("M502 (Factory reset): "), "{why}");
        // Printing: moves and homing ask, an SD print is refused.
        let printing = check_gcode("G28\nM104 S215\nM24", JobContext::Printing);
        assert!(printing.is_blocked() && !printing.needs_confirmation());
        assert!(printing.job_related());
        assert_eq!(printing.blocked[0].code(), "M24");
        assert_eq!(printing.confirm[0].code(), "G28");
        let reasons = printing.reasons();
        assert_eq!(reasons.len(), 2);
        assert!(reasons[0].starts_with("M24 "), "{reasons:?}");
        // Paused: moves are fine, homing Z still asks.
        assert!(check_gcode("G91\nG1 Z5\nG90", JobContext::Paused).is_allowed());
        assert!(check_gcode("G28 X Y", JobContext::Paused).is_allowed());
        assert!(check_gcode("G28", JobContext::Paused).needs_confirmation());
        // M112 is never asked about, and is noticed.
        let stop = check_gcode("G28\nM112", JobContext::Idle);
        assert!(stop.is_allowed() && stop.emergency_stop);
        // Many flagged lines are summed up.
        let many = check_gcode(&"G1 X1\n".repeat(9), JobContext::Printing);
        let reasons = many.reasons();
        assert_eq!(reasons.len(), MAX_FLAGGED_SHOWN + 1);
        assert_eq!(reasons.last().map(String::as_str), Some("… and 5 more"));
        assert_eq!(
            job_sentence(JobContext::Paused, Some("a.gcode")).as_deref(),
            Some("a.gcode is paused.")
        );
        assert_eq!(job_sentence(JobContext::Idle, Some("a.gcode")), None);
    }

    #[test]
    fn scripts_are_rendered_for_previews() {
        let profile = PrinterProfile {
            extruders: 2,
            ..PrinterProfile::default()
        };
        let text = rendered_gcode("{heaters_off}\nM107\nG1 Z{max_z}", &profile);
        assert_eq!(text, "M104 T0 S0\nM104 T1 S0\nM140 S0\nM107\nG1 Z250");
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
        // A halted printer: how to bring it back.
        let mut halted = PrinterState {
            halted: true,
            ..connected
        };
        let why = print_blocker(&halted, None).expect("halted");
        assert_eq!(
            why,
            "The printer is halted: reset resets the board \
             (M999 alone does not recover from M112 or a kill)"
        );
        halted.restartable = true;
        assert_eq!(
            halt_recovery(halted.restartable, ":reset"),
            "M999 restarts the firmware (:reset resets the board)"
        );
    }
}
