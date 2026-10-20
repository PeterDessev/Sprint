//! The settings editor's model: a draft of every reported setting, what
//! changed, import and export. Pure; the tab draws it.

use sprint_proto::{PrinterSettings, SettingCategory, SettingCommand, SettingDef, setting_def};

use crate::widgets::is_modified;

/// Decimals used to compare parameters the catalog does not describe.
const UNKNOWN_DECIMALS: usize = 6;

/// One setting in the editor.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// [`SettingCommand::key`] of the reported command.
    pub key: String,
    /// What the printer reported (`None` for a setting only staged, e.g.
    /// imported, that the printer did not report).
    pub original: Option<SettingCommand>,
    /// The edited command.
    pub draft: SettingCommand,
    /// Raw G-code being edited, for settings the catalog does not know.
    pub raw: Option<String>,
}

impl Entry {
    fn new(cmd: SettingCommand) -> Entry {
        let raw = setting_def(&cmd.code).is_none().then(|| cmd.to_gcode());
        Entry {
            key: cmd.key(),
            original: Some(cmd.clone()),
            draft: cmd,
            raw,
        }
    }

    /// The catalog definition.
    pub fn def(&self) -> Option<&'static SettingDef> {
        setting_def(&self.draft.code)
    }

    /// The category it is shown in ([`SettingCategory::Other`] for unknown
    /// commands).
    pub fn category(&self) -> SettingCategory {
        self.def().map_or(SettingCategory::Other, |d| d.category)
    }

    /// The command the draft stands for: the edited command, or the parsed
    /// raw text (`None` when it does not parse, or names another command:
    /// only a setting's values can be edited, so "Apply" never sends a
    /// factory reset, homing or an emergency stop typed over a setting).
    pub fn command(&self) -> Option<SettingCommand> {
        match &self.raw {
            Some(text) => SettingCommand::parse(text.trim()).filter(|c| c.code == self.draft.code),
            None => Some(self.draft.clone()),
        }
    }

    /// The raw text does not stand for this setting (see
    /// [`Entry::command`]).
    pub fn is_invalid(&self) -> bool {
        self.raw.is_some() && self.command().is_none()
    }

    /// The raw text parses, but as another command than this setting's.
    pub fn changes_code(&self) -> bool {
        self.raw
            .as_deref()
            .and_then(|text| SettingCommand::parse(text.trim()))
            .is_some_and(|c| c.code != self.draft.code)
    }

    /// The draft differs from what the printer reported.
    pub fn is_modified(&self) -> bool {
        let Some(original) = &self.original else {
            return true;
        };
        match &self.raw {
            Some(text) => normalize(text) != normalize(&original.to_gcode()),
            None => !param_changes(original, &self.draft).is_empty(),
        }
    }

    /// The parameter changes (empty for raw entries; see [`Entry::is_modified`]).
    pub fn param_changes(&self) -> Vec<ParamChange> {
        match (&self.original, &self.raw) {
            (_, Some(_)) => Vec::new(),
            (Some(original), None) => param_changes(original, &self.draft),
            (None, None) => param_changes(
                &SettingCommand {
                    code: self.draft.code.clone(),
                    params: Vec::new(),
                    raw: String::new(),
                },
                &self.draft,
            ),
        }
    }

    /// Back to the reported values.
    pub fn revert(&mut self) {
        if let Some(original) = &self.original {
            self.draft = original.clone();
            if self.raw.is_some() {
                self.raw = Some(original.to_gcode());
            }
        }
    }

    /// Mutable value of a parameter of the draft.
    pub fn value_mut(&mut self, letter: char) -> Option<&mut f64> {
        self.draft
            .params
            .iter_mut()
            .find(|(l, _)| *l == letter)
            .map(|(_, v)| v)
    }

    /// The reported value of a parameter.
    pub fn original_value(&self, letter: char) -> Option<f64> {
        self.original.as_ref()?.get(letter)
    }

    /// A short name for the instance: "Hotend 1", "Extruder 0", "X axis",
    /// "Stealth mode" (Prusa's stealth-mode limits), "Column 0, row 2" (a
    /// mesh point), or "" for settings without selectors. See
    /// [`Draft::instance_label`] for the name the editor shows.
    pub fn instance_label(&self) -> String {
        let Some(def) = self.def() else {
            return String::new();
        };
        if def.code == "G29"
            && let Some(label) = mesh_point_label(&self.draft)
        {
            return label;
        }
        let mut parts: Vec<String> = def
            .selectors
            .iter()
            .filter_map(|&sel| {
                let value = self.draft.get(sel)?;
                let label = def.param(sel).map_or("", |p| p.label);
                Some(if value.is_finite() {
                    format!("{label} {}", crate::insight::format::trimmed(value, 3))
                } else {
                    label.to_owned()
                })
            })
            .collect();
        if self.draft.is_stealth() {
            parts.push(STEALTH_MODE.to_owned());
        }
        parts.join(", ")
    }

    /// Whether the entry matches a search query (code, name, description,
    /// parameter labels, raw text), case-insensitively.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let mut haystack = vec![
            self.key.to_lowercase(),
            self.draft.to_gcode().to_lowercase(),
        ];
        if let Some(def) = self.def() {
            haystack.push(def.name.to_lowercase());
            haystack.push(def.description.to_lowercase());
            haystack.push(def.category.name().to_lowercase());
            haystack.extend(def.params.iter().map(|p| p.label.to_lowercase()));
        }
        if let Some(raw) = &self.raw {
            haystack.push(raw.to_lowercase());
        }
        query
            .split_whitespace()
            .all(|word| haystack.iter().any(|h| h.contains(word)))
    }
}

/// The instance name of a Prusa stealth-mode limit.
const STEALTH_MODE: &str = "Stealth mode";
/// The instance name of a normal-mode limit next to a stealth-mode one.
const NORMAL_MODE: &str = "Normal mode";

/// A mesh point's name: "Column 0, row 2" for `G29 S3 I0 J2` (or Marlin
/// 1.1's `G29 S3 X1 Y3`, numbered from 1 as reported) and a bilinear
/// `G29 W I0 J2`; "Z offset" for `G29 S4 Z`, the mesh's offset. `None`
/// for another form.
pub fn mesh_point_label(cmd: &SettingCommand) -> Option<String> {
    let index = |column: char, row: char| {
        let (i, j) = (cmd.get(column)?, cmd.get(row)?);
        (i.is_finite() && j.is_finite()).then_some((i, j))
    };
    if let Some((i, j)) = index('I', 'J').or_else(|| index('X', 'Y')) {
        let n = |v: f64| crate::insight::format::trimmed(v, 3);
        return Some(format!("Column {}, row {}", n(i), n(j)));
    }
    (cmd.get('S') == Some(4.0)).then(|| "Z offset".to_owned())
}

/// `cmd.raw` written from its values again, keeping the `; stealth`
/// marker of a Prusa stealth-mode limit: the marker is what keeps it
/// apart from the normal-mode limit of the same command (its key, and so
/// the host's update of the settings when it is applied).
fn refresh_raw(cmd: &mut SettingCommand) {
    let stealth = cmd.is_stealth();
    cmd.raw = cmd.to_gcode();
    if stealth {
        cmd.mark_stealth();
    }
}

/// A setting as a line of G-code for a file or the clipboard: its G-code,
/// with a `; stealth` comment for a Prusa stealth-mode limit so an import
/// keeps it apart from the normal-mode one.
pub fn gcode_line(cmd: &SettingCommand) -> String {
    if cmd.is_stealth() {
        format!("{} ; stealth", cmd.to_gcode())
    } else {
        cmd.to_gcode()
    }
}

/// Collapse whitespace and upper-case, for comparing raw G-code.
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}

/// One changed parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamChange {
    /// Parameter letter.
    pub letter: char,
    /// Label from the catalog (the letter when unknown).
    pub label: String,
    /// Unit from the catalog.
    pub unit: &'static str,
    /// Decimals to show.
    pub decimals: usize,
    /// Reported value (`None` = not reported; NaN = flag set).
    pub old: Option<f64>,
    /// New value (`None` = removed; NaN = flag set).
    pub new: Option<f64>,
    /// Whether it is a flag (bare letter).
    pub flag: bool,
}

impl ParamChange {
    /// "93.00 → 95.40 steps/mm", "off → on", "— → 0.25".
    pub fn describe(&self) -> String {
        let show = |v: Option<f64>| match v {
            None if self.flag => "off".to_owned(),
            None => "—".to_owned(),
            Some(_) if self.flag => "on".to_owned(),
            Some(v) => crate::widgets::format_number(v, self.decimals),
        };
        let unit = if self.flag || self.unit.is_empty() {
            String::new()
        } else {
            format!(" {}", self.unit)
        };
        format!("{} → {}{unit}", show(self.old), show(self.new))
    }
}

/// The parameters that differ between two commands (by the catalog's
/// decimals), in the draft's order followed by removed ones.
pub fn param_changes(old: &SettingCommand, new: &SettingCommand) -> Vec<ParamChange> {
    let def = setting_def(&new.code);
    let mut letters: Vec<char> = new.params.iter().map(|(l, _)| *l).collect();
    for (l, _) in &old.params {
        if !letters.contains(l) {
            letters.push(*l);
        }
    }
    letters
        .into_iter()
        .filter(|&l| !def.is_some_and(|d| d.is_selector(l)))
        .filter_map(|letter| {
            let param = def.and_then(|d| d.param(letter));
            let decimals = param.map_or(UNKNOWN_DECIMALS, |p| usize::from(p.decimals));
            let (o, n) = (old.get(letter), new.get(letter));
            let changed = match (o, n) {
                (Some(a), Some(b)) => is_modified(b, a, decimals),
                (None, None) => false,
                _ => true,
            };
            changed.then(|| ParamChange {
                letter,
                label: param.map_or_else(|| letter.to_string(), |p| p.label.to_owned()),
                unit: param.map_or("", |p| p.unit),
                decimals,
                old: o,
                new: n,
                flag: param.is_some_and(|p| p.flag),
            })
        })
        .collect()
}

/// A pending change, ready to apply.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Entry key.
    pub key: String,
    /// Setting name ("Steps per unit") or the code for unknown commands.
    pub name: String,
    /// Instance label ("Hotend 1"), may be empty.
    pub instance: String,
    /// The command to send.
    pub command: SettingCommand,
    /// What changed (empty for raw G-code edits).
    pub params: Vec<ParamChange>,
    /// The old raw G-code, for raw edits.
    pub old_raw: Option<String>,
    /// Commands sent after `command` to complete it (see
    /// [`switched_off_drivers`]).
    pub also: Vec<SettingCommand>,
}

impl Change {
    /// One line per parameter: "Extruder steps: 93.00 → 95.40 steps/mm",
    /// or "M149 C → M149 F" for raw edits.
    pub fn lines(&self) -> Vec<String> {
        if self.params.is_empty() {
            let old = self.old_raw.as_deref().unwrap_or("—");
            return vec![format!("{old} → {}", self.command.to_gcode())];
        }
        self.params
            .iter()
            .map(|p| format!("{}: {}", p.label, p.describe()))
            .collect()
    }
}

/// The editor's draft of every reported setting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    entries: Vec<Entry>,
    /// The report the draft was last based on.
    base: PrinterSettings,
}

impl Draft {
    /// A draft of `settings` without edits.
    pub fn new(settings: &PrinterSettings) -> Draft {
        Draft {
            entries: settings.iter().cloned().map(Entry::new).collect(),
            base: settings.clone(),
        }
    }

    /// The report this draft is based on.
    pub fn base(&self) -> &PrinterSettings {
        &self.base
    }

    /// Entries in report order (staged extras last).
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Mutable entries.
    pub fn entries_mut(&mut self) -> &mut [Entry] {
        &mut self.entries
    }

    /// Entry by key.
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// Mutable entry by key.
    pub fn entry_mut(&mut self, key: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.key == key)
    }

    /// The instance name the editor shows for `entry`:
    /// [`Entry::instance_label`], with "Normal mode" for a normal-mode
    /// limit the firmware reported next to a stealth-mode one (Prusa's
    /// M201 and M203).
    pub fn instance_label(&self, entry: &Entry) -> String {
        let label = entry.instance_label();
        let beside_stealth = !entry.draft.is_stealth()
            && self
                .entries
                .iter()
                .any(|e| e.draft.code == entry.draft.code && e.draft.is_stealth());
        match (beside_stealth, label.is_empty()) {
            (false, _) => label,
            (true, true) => NORMAL_MODE.to_owned(),
            (true, false) => format!("{label}, {NORMAL_MODE}"),
        }
    }

    /// Follow a new report: unedited entries take the new values, edited
    /// ones keep their edits against the new reported values (and stop
    /// being edits once the printer reports the edited value), staged
    /// entries the printer did not report stay, and entries that are gone
    /// from the report are dropped.
    pub fn rebase(&mut self, settings: &PrinterSettings) {
        let mut old: Vec<Entry> = std::mem::take(&mut self.entries);
        let mut entries = Vec::with_capacity(settings.len());
        for cmd in settings.iter() {
            let key = cmd.key();
            match old.iter().position(|e| e.key == key) {
                Some(i) => {
                    let mut e = old.remove(i);
                    if e.is_modified() {
                        e.original = Some(cmd.clone());
                        if !e.is_modified() {
                            e = Entry::new(cmd.clone());
                        }
                    } else {
                        e = Entry::new(cmd.clone());
                    }
                    entries.push(e);
                }
                None => entries.push(Entry::new(cmd.clone())),
            }
        }
        // Staged settings the printer does not report (yet).
        entries.extend(old.into_iter().filter(|e| e.original.is_none()));
        self.entries = entries;
        self.base = settings.clone();
    }

    /// Discard the edit of one setting: back to the reported values, or
    /// gone when it was only staged (imported, not reported).
    pub fn revert(&mut self, key: &str) {
        let Some(i) = self.entries.iter().position(|e| e.key == key) else {
            return;
        };
        if self.entries[i].original.is_some() {
            self.entries[i].revert();
        } else {
            self.entries.remove(i);
        }
    }

    /// Discard every edit.
    pub fn revert_all(&mut self) {
        self.entries.retain(|e| e.original.is_some());
        for e in &mut self.entries {
            e.revert();
        }
    }

    /// Stage `cmd` as an edit: replace the matching entry's draft, or add
    /// it.
    pub fn stage(&mut self, cmd: SettingCommand) {
        let key = cmd.key();
        match self.entry_mut(&key) {
            Some(e) => {
                if e.raw.is_some() {
                    e.raw = Some(cmd.to_gcode());
                }
                e.draft = cmd;
            }
            None => {
                let mut e = Entry::new(cmd);
                e.original = None;
                self.entries.push(e);
            }
        }
    }

    /// `cmd` merged into the current draft of its setting: imported
    /// parameters replace the drafted ones and the others are kept (a file
    /// line `M92 E415` changes only E). Settings with flags (`M569 S1 X Y`)
    /// and unknown commands are taken whole.
    pub fn merged(&self, cmd: &SettingCommand) -> SettingCommand {
        let Some(entry) = self.entry(&cmd.key()) else {
            return cmd.clone();
        };
        let has_flags = entry
            .def()
            .is_some_and(|d| d.params.iter().any(|p| p.flag && !d.is_selector(p.letter)));
        if entry.raw.is_some() || has_flags {
            return cmd.clone();
        }
        let mut merged = entry.draft.clone();
        for &(letter, value) in &cmd.params {
            merged.set(letter, value);
        }
        refresh_raw(&mut merged);
        merged
    }

    /// Stage an imported command ([`Draft::merged`] into the draft).
    pub fn stage_import(&mut self, cmd: &SettingCommand) {
        let merged = self.merged(cmd);
        self.stage(merged);
    }

    /// Number of modified entries.
    pub fn modified_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_modified()).count()
    }

    /// Modified raw entries whose text does not parse.
    pub fn invalid_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.is_modified() && e.is_invalid())
            .count()
    }

    /// The pending changes, in report order (invalid raw edits excluded).
    pub fn changes(&self) -> Vec<Change> {
        self.entries
            .iter()
            .filter(|e| e.is_modified())
            .filter_map(|e| {
                let mut command = e.command()?;
                refresh_raw(&mut command);
                Some(Change {
                    also: e
                        .original
                        .as_ref()
                        .and_then(|o| switched_off_drivers(o, &command))
                        .into_iter()
                        .collect(),
                    key: e.key.clone(),
                    name: e
                        .def()
                        .map_or_else(|| command.code.clone(), |d| d.name.to_owned()),
                    instance: self.instance_label(e),
                    params: e.param_changes(),
                    old_raw: e.raw.as_ref().map(|_| {
                        e.original
                            .as_ref()
                            .map(SettingCommand::to_gcode)
                            .unwrap_or_default()
                    }),
                    command,
                })
            })
            .collect()
    }

    /// Every setting as G-code, one per line (with edits; see
    /// [`gcode_line`]).
    pub fn to_gcode(&self) -> String {
        self.entries
            .iter()
            .filter_map(Entry::command)
            .map(|c| gcode_line(&c))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Categories that have entries, in display order, with their counts
    /// and modified counts.
    pub fn categories(&self) -> Vec<(SettingCategory, usize, usize)> {
        SettingCategory::ALL
            .iter()
            .filter_map(|&cat| {
                let in_cat: Vec<&Entry> = self
                    .entries
                    .iter()
                    .filter(|e| e.category() == cat)
                    .collect();
                (!in_cat.is_empty()).then(|| {
                    (
                        cat,
                        in_cat.len(),
                        in_cat.iter().filter(|e| e.is_modified()).count(),
                    )
                })
            })
            .collect()
    }
}

/// `M569 S1 X Y Z` lists the drivers in StealthChop, and Marlin only
/// changes the drivers a command names: dropping Z from the list leaves Z
/// as it is. The command that switches the dropped drivers to SpreadCycle
/// (`M569 S0 Z`, with the same `T`/`I` selectors), if any were dropped.
pub fn switched_off_drivers(
    original: &SettingCommand,
    edited: &SettingCommand,
) -> Option<SettingCommand> {
    if edited.code != "M569" || !edited.get('S').is_some_and(|s| s >= 0.5) {
        return None;
    }
    let def = setting_def("M569")?;
    let dropped: Vec<char> = original
        .params
        .iter()
        .map(|(l, _)| *l)
        .filter(|&l| !def.is_selector(l) && def.param(l).is_some_and(|p| p.flag))
        .filter(|&l| !edited.has(l))
        .collect();
    if dropped.is_empty() {
        return None;
    }
    let mut params = vec![('S', 0.0)];
    params.extend(
        def.selectors
            .iter()
            .filter_map(|&sel| edited.get(sel).map(|v| (sel, v))),
    );
    params.extend(dropped.iter().map(|&l| (l, f64::NAN)));
    let mut cmd = SettingCommand::new("M569", &params);
    cmd.raw = cmd.to_gcode();
    Some(cmd)
}

/// A text file of settings: what will be exported.
pub fn export_text(draft: &Draft, firmware: Option<&str>, machine: Option<&str>) -> String {
    let mut out = String::from("; Sprint settings export\n");
    if let Some(fw) = firmware.filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("; Firmware: {}\n", one_line(fw)));
    }
    if let Some(m) = machine.filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("; Machine: {}\n", one_line(m)));
    }
    out.push_str("; Apply with Tuning > Import, or send the lines and M500 to keep them.\n");
    let gcode = draft.to_gcode();
    if !gcode.is_empty() {
        out.push_str(&gcode);
        out.push('\n');
    }
    out
}

fn one_line(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// A line of an imported file that was not used.
#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    /// 1-based line number.
    pub line: usize,
    /// The text.
    pub text: String,
    /// Why.
    pub reason: &'static str,
}

/// What an import contains.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Import {
    /// Setting commands in the order their settings first appear (a later
    /// line for the same key replaces an earlier one); at most
    /// [`MAX_IMPORT_SETTINGS`].
    pub commands: Vec<SettingCommand>,
    /// Lines that were not used.
    pub skipped: Vec<Skipped>,
}

/// Commands that must never come from a settings file.
const NOT_SETTINGS: [&str; 4] = ["M500", "M501", "M502", "M503"];

/// Most distinct settings taken from one file (a firmware reports well
/// under a hundred); more are skipped, so a wrong file cannot stall the
/// UI with a preview of thousands of rows.
pub const MAX_IMPORT_SETTINGS: usize = 1000;

/// Parse a file of setting commands (an export, an M503 log, a list of
/// `M92 E415` lines). Comments and blank lines are ignored; `echo:`
/// prefixes are accepted. Only settings known to the catalog or reported
/// by the printer (`known_codes`) are taken, never a
/// [report-only](SettingCommand::is_report_only) one; anything else is
/// skipped with a reason. Prusa's stealth-mode limits stay apart from the
/// normal-mode ones: a `; stealth` comment (as exported) or, in an M503
/// log, a "... - stealth ..." heading above them marks them, as the reply
/// parser does.
pub fn parse_import(text: &str, known_codes: &[String]) -> Import {
    let mut import = Import::default();
    // Where each key is in `import.commands`.
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // Below a stealth-mode heading of an M503 log.
    let mut stealth = false;
    for (i, line) in text.lines().enumerate().take(100_000) {
        let trimmed = line.trim().trim_start_matches('\u{feff}');
        let trimmed = trimmed.strip_prefix("echo:").map_or(trimmed, str::trim);
        let body = trimmed.split(';').next().unwrap_or("").trim();
        if body.is_empty() {
            continue;
        }
        let skip = |reason| Skipped {
            line: i + 1,
            text: one_line(trimmed).chars().take(120).collect(),
            reason,
        };
        // Parsed with its comment, which may be the stealth marker.
        let Some(mut cmd) = SettingCommand::parse(trimmed) else {
            // A heading ("Maximum feedrates - stealth (mm/s):") or other
            // text.
            stealth = body.to_ascii_lowercase().contains("stealth");
            import.skipped.push(skip("not a setting command"));
            continue;
        };
        if stealth {
            cmd.mark_stealth();
        }
        if NOT_SETTINGS.contains(&cmd.code.as_str()) {
            import
                .skipped
                .push(skip("EEPROM commands are not imported"));
            continue;
        }
        // Marlin's M503 reports Autotemp as `M104 S B F`, but `M104 S` sets
        // the hotend temperature.
        if cmd.is_report_only() {
            import
                .skipped
                .push(skip("would heat or move the printer, not change a setting"));
            continue;
        }
        let known = setting_def(&cmd.code).is_some() || known_codes.contains(&cmd.code);
        if !known {
            import.skipped.push(skip("not a known setting"));
            continue;
        }
        // A later line for the same setting replaces the earlier one.
        match index.get(&cmd.key()) {
            Some(&at) => import.commands[at] = cmd,
            None if import.commands.len() >= MAX_IMPORT_SETTINGS => {
                import.skipped.push(skip("too many settings in one file"));
            }
            None => {
                index.insert(cmd.key(), import.commands.len());
                import.commands.push(cmd);
            }
        }
    }
    import
}

/// How an imported command compares with the draft.
#[derive(Clone, Debug, PartialEq)]
pub enum ImportDiff {
    /// Same values as the draft.
    Unchanged,
    /// Changes these parameters of an existing setting.
    Changed(Vec<ParamChange>),
    /// The printer did not report this setting.
    New,
}

/// Compare an import with the draft, command by command.
pub fn diff_import(draft: &Draft, import: &Import) -> Vec<(SettingCommand, ImportDiff)> {
    import
        .commands
        .iter()
        .map(|cmd| {
            let diff = match draft.entry(&cmd.key()) {
                None => ImportDiff::New,
                Some(e) => match &e.raw {
                    Some(raw) if normalize(raw) == normalize(&cmd.to_gcode()) => {
                        ImportDiff::Unchanged
                    }
                    Some(_) => ImportDiff::Changed(Vec::new()),
                    None => {
                        let changes = param_changes(&e.draft, &draft.merged(cmd));
                        if changes.is_empty() {
                            ImportDiff::Unchanged
                        } else {
                            ImportDiff::Changed(changes)
                        }
                    }
                },
            };
            (cmd.clone(), diff)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(lines: &[&str]) -> PrinterSettings {
        lines
            .iter()
            .filter_map(|l| SettingCommand::parse(l))
            .collect()
    }

    fn report() -> PrinterSettings {
        settings(&[
            "G21",
            "M92 X80.00 Y80.00 Z400.00 E93.00",
            "M301 E0 P22.20 I1.08 D114.00",
            "M301 E1 P20.00 I1.00 D100.00",
            "M569 S1 X Y Z",
            "M149 C",
        ])
    }

    #[test]
    fn a_fresh_draft_has_no_changes() {
        let d = Draft::new(&report());
        assert_eq!(d.entries().len(), 6);
        assert_eq!(d.modified_count(), 0);
        assert!(d.changes().is_empty());
        // Unknown commands are edited as raw text.
        assert_eq!(
            d.entry("G21").and_then(|e| e.raw.clone()).as_deref(),
            Some("G21")
        );
        assert!(d.entry("M92").is_some_and(|e| e.raw.is_none()));
        assert_eq!(
            d.entry("M301 E1").map(Entry::instance_label).as_deref(),
            Some("Hotend 1")
        );
        assert_eq!(
            d.entry("M92").map(Entry::instance_label).as_deref(),
            Some("")
        );
    }

    #[test]
    fn edits_produce_changes_by_catalog_decimals() {
        let mut d = Draft::new(&report());
        // Below the shown precision: not a change.
        *d.entry_mut("M92")
            .and_then(|e| e.value_mut('E'))
            .expect("E") = 93.001;
        assert_eq!(d.modified_count(), 0);
        *d.entry_mut("M92")
            .and_then(|e| e.value_mut('E'))
            .expect("E") = 95.4;
        let changes = d.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].command.to_gcode(), "M92 X80 Y80 Z400 E95.4");
        assert_eq!(changes[0].name, "Steps per unit");
        assert_eq!(
            changes[0].lines(),
            vec!["Extruder steps: 93.00 → 95.40 steps/mm"]
        );
        // Reverting one entry.
        d.entry_mut("M92").expect("M92").revert();
        assert_eq!(d.modified_count(), 0);
    }

    #[test]
    fn flags_toggle_as_changes() {
        let mut d = Draft::new(&report());
        let e = d.entry_mut("M569").expect("M569");
        e.draft.remove('Z');
        let changes = d.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].command.to_gcode(), "M569 S1 X Y");
        assert_eq!(changes[0].lines(), vec!["Z driver: on → off"]);
    }

    #[test]
    fn dropped_drivers_are_switched_to_spreadcycle() {
        let mut d = Draft::new(&settings(&["M569 S1 X Y Z", "M569 S1 T0 E"]));
        d.entry_mut("M569").expect("M569").draft.remove('Z');
        let changes = d.changes();
        assert_eq!(changes[0].command.to_gcode(), "M569 S1 X Y");
        let also: Vec<String> = changes[0].also.iter().map(|c| c.to_gcode()).collect();
        assert_eq!(also, ["M569 S0 Z"]);
        // Every driver off: the S1 line names none, the S0 line all.
        let e = d.entry_mut("M569").expect("M569");
        e.draft.remove('X');
        e.draft.remove('Y');
        assert_eq!(d.changes()[0].also[0].to_gcode(), "M569 S0 X Y Z");
        // Selectors carry over; switching S itself needs nothing extra.
        let e = d.entry_mut("M569 T0").expect("M569 T0");
        e.draft.remove('E');
        let t0 = d
            .changes()
            .into_iter()
            .find(|c| c.key == "M569 T0")
            .expect("T0");
        assert_eq!(t0.also[0].to_gcode(), "M569 S0 T0 E");
        d.revert_all();
        d.entry_mut("M569").expect("M569").draft.set('S', 0.0);
        assert!(d.changes()[0].also.is_empty());
        // Nothing for other settings.
        let m92 = SettingCommand::parse("M92 E93").expect("parse");
        assert_eq!(switched_off_drivers(&m92, &m92), None);
    }

    #[test]
    fn raw_edits_validate() {
        let mut d = Draft::new(&report());
        d.entry_mut("G21").expect("G21").raw = Some("  g21 ".into());
        assert_eq!(d.modified_count(), 0, "case and spacing do not matter");
        d.entry_mut("G21").expect("G21").raw = Some("hello".into());
        assert_eq!(d.invalid_count(), 1);
        assert!(d.changes().is_empty(), "invalid raw edits are not applied");
        d.revert_all();
        assert_eq!(d.modified_count(), 0);
        assert_eq!(
            d.entry("G21").and_then(|e| e.raw.clone()).as_deref(),
            Some("G21")
        );
        // Values of an unknown setting can change...
        let mut d = Draft::new(&settings(&["M575 P0 B250"]));
        d.entry_mut("M575").expect("M575").raw = Some("m575 p0 b115".into());
        assert_eq!(d.changes()[0].lines(), vec!["M575 P0 B250 → M575 P0 B115"]);
        // ...but not the command: that would send something else entirely.
        for other in ["M502", "G28", "M112", "M576 P0 B115"] {
            let e = d.entry_mut("M575").expect("M575");
            e.raw = Some(other.into());
            assert!(e.is_invalid() && e.changes_code(), "{other}");
            assert!(d.changes().is_empty(), "{other}");
        }
        let e = d.entry_mut("M575").expect("M575");
        e.raw = Some("M575 P0 Bx".into());
        assert!(e.is_invalid() && !e.changes_code());
    }

    #[test]
    fn rebase_keeps_edits_and_settles_applied_ones() {
        let mut d = Draft::new(&report());
        *d.entry_mut("M92")
            .and_then(|e| e.value_mut('E'))
            .expect("E") = 95.4;
        *d.entry_mut("M301 E1")
            .and_then(|e| e.value_mut('P'))
            .expect("P") = 25.0;
        // The printer applied the M92 edit only.
        let mut next = report();
        next.insert(SettingCommand::parse("M92 X80 Y80 Z400 E95.40").expect("parse"));
        next.remove("M149");
        next.insert(SettingCommand::parse("M900 K0.22").expect("parse"));
        d.rebase(&next);
        assert_eq!(d.modified_count(), 1);
        assert!(d.entry("M92").is_some_and(|e| !e.is_modified()));
        assert!(d.entry("M301 E1").is_some_and(Entry::is_modified));
        assert!(d.entry("M149").is_none());
        assert!(d.entry("M900").is_some());
        assert_eq!(d.base(), &next);
    }

    #[test]
    fn staging_imports_and_new_settings() {
        let mut d = Draft::new(&report());
        d.stage(SettingCommand::parse("M92 E415").expect("parse"));
        d.stage(SettingCommand::parse("M851 Z-1.2").expect("parse"));
        d.stage(SettingCommand::parse("G20").expect("parse"));
        assert_eq!(d.modified_count(), 3);
        let changes = d.changes();
        assert!(changes.iter().any(|c| c.key == "M851"));
        assert_eq!(
            changes
                .iter()
                .find(|c| c.key == "M92")
                .map(|c| c.command.to_gcode()),
            Some("M92 E415".to_owned())
        );
        // A rebase keeps staged new settings.
        d.rebase(&report());
        assert!(d.entry("M851").is_some());
        // Reverting one: a staged setting goes, an edited one is reset.
        d.revert("M851");
        assert!(d.entry("M851").is_none());
        d.revert("M92");
        assert!(d.entry("M92").is_some_and(|e| !e.is_modified()));
        d.revert("M9999");
        assert_eq!(d.modified_count(), 1, "only G20 is left");
        d.stage(SettingCommand::parse("M851 Z-1.2").expect("parse"));
        d.revert_all();
        assert!(d.entry("M851").is_none());
        assert_eq!(d.modified_count(), 0);
    }

    #[test]
    fn search_matches_names_codes_and_labels() {
        let d = Draft::new(&report());
        let m92 = d.entry("M92").expect("M92");
        assert!(m92.matches("m92"));
        assert!(m92.matches("steps"));
        assert!(m92.matches("extruder steps"));
        assert!(m92.matches(""));
        assert!(!m92.matches("pid"));
        assert!(d.entry("M301 E0").is_some_and(|e| e.matches("hotend pid")));
        assert!(d.entry("G21").is_some_and(|e| e.matches("g21")));
    }

    #[test]
    fn categories_count_entries() {
        let mut d = Draft::new(&report());
        *d.entry_mut("M92")
            .and_then(|e| e.value_mut('X'))
            .expect("X") = 81.0;
        let cats = d.categories();
        assert!(cats.contains(&(SettingCategory::Motion, 1, 1)));
        assert!(cats.contains(&(SettingCategory::Thermal, 3, 0)));
        assert!(
            cats.iter()
                .any(|(c, n, _)| *c == SettingCategory::Other && *n == 1)
        );
    }

    #[test]
    fn export_and_import_round_trip() {
        let mut d = Draft::new(&report());
        *d.entry_mut("M92")
            .and_then(|e| e.value_mut('E'))
            .expect("E") = 95.4;
        let text = export_text(&d, Some("Marlin 2.1.2.1\n"), Some("Ender"));
        assert!(text.starts_with("; Sprint settings export\n; Firmware: Marlin 2.1.2.1\n"));
        assert!(text.contains("M92 X80 Y80 Z400 E95.4\n"));
        let import = parse_import(&text, &["G21".to_owned()]);
        assert_eq!(import.commands.len(), 6);
        assert!(import.skipped.is_empty(), "{:?}", import.skipped);
        // Against the unedited draft only M92 differs.
        let fresh = Draft::new(&report());
        let diffs = diff_import(&fresh, &import);
        let changed: Vec<_> = diffs
            .iter()
            .filter(|(_, d)| !matches!(d, ImportDiff::Unchanged))
            .collect();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0.key(), "M92");
    }

    #[test]
    fn import_skips_what_is_not_a_setting() {
        let text = "\u{feff}; header\n\nM500\nG28\nM104 S200\nhello world\necho:  M92 E415 ; comment\nM92 E420\nM851 Z-1.5\n";
        // Even from a printer that reports M104 (Marlin's Autotemp).
        let import = parse_import(text, &["M104".to_owned()]);
        assert_eq!(
            import
                .commands
                .iter()
                .map(|c| c.to_gcode())
                .collect::<Vec<_>>(),
            vec!["M92 E420", "M851 Z-1.5"]
        );
        let reasons: Vec<_> = import.skipped.iter().map(|s| (s.line, s.reason)).collect();
        assert_eq!(
            reasons,
            vec![
                (3, "EEPROM commands are not imported"),
                (4, "would heat or move the printer, not change a setting"),
                (5, "would heat or move the printer, not change a setting"),
                (6, "not a setting command"),
            ]
        );
        let d = Draft::new(&report());
        let diffs = diff_import(&d, &import);
        assert!(matches!(diffs[0].1, ImportDiff::Changed(ref p) if p.len() == 1));
        assert_eq!(diffs[1].1, ImportDiff::New);
        // Staging merges: the other axes stay.
        let mut d = d;
        for cmd in &import.commands {
            d.stage_import(cmd);
        }
        assert_eq!(
            d.entry("M92").map(|e| e.draft.to_gcode()).as_deref(),
            Some("M92 X80 Y80 Z400 E420")
        );
        // Flag settings are replaced whole.
        d.stage_import(&SettingCommand::parse("M569 S1 X").expect("parse"));
        assert_eq!(
            d.entry("M569").map(|e| e.draft.to_gcode()).as_deref(),
            Some("M569 S1 X")
        );
    }

    #[test]
    fn huge_imports_stay_fast_and_bounded() {
        // A sliced print picked by mistake: hundreds of thousands of lines,
        // the same few settings over and over.
        let mut print = String::new();
        for i in 0..100_000 {
            print.push_str(&format!("G1 X{i} E0.1\nM204 S{}\n", i % 3000));
        }
        let start = std::time::Instant::now();
        let import = parse_import(&print, &[]);
        assert_eq!(import.commands.len(), 1);
        assert_eq!(import.commands[0].to_gcode(), "M204 S1999");
        // Distinct settings by the thousand (every hotend index).
        let many: String = (0..20_000)
            .map(|i| format!("M301 E{i} P22 I1 D100\n"))
            .collect();
        let import = parse_import(&many, &[]);
        assert_eq!(import.commands.len(), MAX_IMPORT_SETTINGS);
        assert_eq!(
            import.skipped.len(),
            20_000 - MAX_IMPORT_SETTINGS,
            "the rest is skipped"
        );
        assert_eq!(import.skipped[0].reason, "too many settings in one file");
        // Both used to take minutes (every line rescanned every command).
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }

    /// A Prusa MK3 report: normal-mode and stealth-mode limits.
    fn prusa() -> PrinterSettings {
        let mut s = settings(&["M203 X200 Y200 Z12 E120", "M201 X1000 Y1000 Z200 E5000"]);
        for line in ["M203 X100 Y100 Z12 E120", "M201 X960 Y960 Z200 E5000"] {
            let mut cmd = SettingCommand::parse(line).expect("parse");
            cmd.mark_stealth();
            s.insert(cmd);
        }
        s
    }

    #[test]
    fn stealth_limits_keep_their_marker_through_edits() {
        let mut d = Draft::new(&prusa());
        assert_eq!(d.entries().len(), 4);
        let stealth = d.entry("M203 stealth").expect("stealth M203");
        assert_eq!(stealth.instance_label(), "Stealth mode");
        assert_eq!(d.instance_label(stealth), "Stealth mode");
        let normal = d.entry("M203").expect("normal M203");
        assert_eq!(normal.instance_label(), "");
        assert_eq!(d.instance_label(normal), "Normal mode");
        assert!(stealth.matches("stealth feedrate"));
        // Editing the stealth limit changes it, not the normal one.
        *d.entry_mut("M203 stealth")
            .and_then(|e| e.value_mut('X'))
            .expect("X") = 90.0;
        let changes = d.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].key, "M203 stealth");
        assert_eq!(changes[0].command.key(), "M203 stealth");
        assert_eq!(changes[0].command.to_gcode(), "M203 X90 Y100 Z12 E120");
        assert_eq!(changes[0].instance, "Stealth mode");
        // The host updates its copy of the settings by the command's key.
        let mut applied = prusa();
        applied.insert(changes[0].command.clone());
        assert_eq!(applied.max_feedrate(), Some([200.0, 200.0, 12.0, 120.0]));
        assert_eq!(
            applied.max_feedrate_stealth(),
            Some([90.0, 100.0, 12.0, 120.0])
        );
        // A normal-mode edit says which mode it is.
        d.revert_all();
        *d.entry_mut("M201")
            .and_then(|e| e.value_mut('X'))
            .expect("X") = 1250.0;
        let changes = d.changes();
        assert_eq!(
            (changes[0].key.as_str(), changes[0].instance.as_str()),
            ("M201", "Normal mode")
        );
        assert!(!changes[0].command.is_stealth());
        // Without stealth-mode limits no mode is named.
        let plain = Draft::new(&settings(&["M203 X200 Y200 Z12 E120"]));
        assert_eq!(plain.instance_label(plain.entry("M203").expect("M203")), "");
    }

    #[test]
    fn stealth_limits_survive_export_and_import() {
        let d = Draft::new(&prusa());
        let text = export_text(&d, None, None);
        assert!(
            text.contains("\nM203 X100 Y100 Z12 E120 ; stealth\n"),
            "{text}"
        );
        assert!(text.contains("\nM203 X200 Y200 Z12 E120\n"), "{text}");
        let import = parse_import(&text, &[]);
        let keys: Vec<String> = import.commands.iter().map(SettingCommand::key).collect();
        assert_eq!(keys, ["M203", "M201", "M203 stealth", "M201 stealth"]);
        assert!(
            diff_import(&d, &import)
                .iter()
                .all(|(_, diff)| *diff == ImportDiff::Unchanged)
        );
        // A changed stealth line is staged onto the stealth limit only.
        let mut d = d;
        for cmd in &parse_import("M203 X80 Y80 Z12 E120 ; stealth\n", &[]).commands {
            d.stage_import(cmd);
        }
        let changes = d.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].key, "M203 stealth");
        assert_eq!(changes[0].command.key(), "M203 stealth");
        assert_eq!(d.entry("M203").and_then(|e| e.draft.get('X')), Some(200.0));
    }

    #[test]
    fn an_m503_log_marks_the_limits_under_stealth_headings() {
        let log = "echo:Maximum feedrates - normal (mm/s):\n\
                   echo:  M203 X200.00 Y200.00 Z12.00 E120.00\n\
                   echo:Maximum feedrates - stealth (mm/s):\n\
                   echo:  M203 X100.00 Y100.00 Z12.00 E120.00\n\
                   echo:Maximum acceleration - normal (mm/s2):\n\
                   echo:  M201 X1000 Y1000 Z200 E5000\n\
                   echo:Maximum acceleration - stealth (mm/s2):\n\
                   echo:  M201 X960 Y960 Z200 E5000\n\
                   echo:Steps per unit:\n\
                   echo:  M92 X100.00 Y100.00 Z400.00 E280.00\n\
                   ok\n";
        let import = parse_import(log, &[]);
        let keys: Vec<String> = import.commands.iter().map(SettingCommand::key).collect();
        assert_eq!(
            keys,
            ["M203", "M203 stealth", "M201", "M201 stealth", "M92"]
        );
        assert_eq!(import.skipped.len(), 6, "the headings and the ok");
    }

    #[test]
    fn mesh_points_are_named_by_column_and_row() {
        let d = Draft::new(&settings(&[
            "G29 S3 I0 J2 Z0.0125",
            "G29 S3 X1 Y3 Z-0.02",
            "G29 W I1 J0 Z0.1",
            "G29 S4 Z0.05",
        ]));
        let labels: Vec<String> = d.entries().iter().map(|e| d.instance_label(e)).collect();
        assert_eq!(
            labels,
            [
                "Column 0, row 2",
                "Column 1, row 3",
                "Column 1, row 0",
                "Z offset"
            ]
        );
    }

    #[test]
    fn change_descriptions() {
        let c = ParamChange {
            letter: 'Z',
            label: "Z offset".into(),
            unit: "mm",
            decimals: 2,
            old: None,
            new: Some(-1.5),
            flag: false,
        };
        assert_eq!(c.describe(), "— → -1.50 mm");
    }
}
