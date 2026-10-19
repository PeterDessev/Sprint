//! Macro arguments, as pronsole has them and the shell writes them: in a
//! macro's G-code `{0}`, `{1}`, … are its arguments in order, `{1=200}`
//! gives one a default, and `{args}` is all of them separated by spaces
//! (the same syntax as `sprint_tui::actions::macro_gcode`). Other
//! placeholders (`{x}`, `{heaters_off}`, …) are left for the host, which
//! fills them in with the machine state when the macro runs.
//!
//! The GUI asks for the arguments of such a macro in a small dialog
//! ([`MacroParams`] says which fields it needs) and sends the G-code that
//! [`fill`] makes of them, never the placeholders themselves.

use sprint_gcode::MachineState;
use sprint_host::{PrinterProfile, PrinterState, ScriptContext, render_script_with};

/// The arguments a macro takes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MacroParams {
    /// One entry per numbered argument `0..=highest`: its default when the
    /// G-code gives one (`{1=200}`; the first default written wins).
    pub numbered: Vec<Option<String>>,
    /// The G-code uses `{args}`.
    pub all: bool,
}

impl MacroParams {
    /// The macro takes arguments.
    pub fn any(&self) -> bool {
        self.all || !self.numbered.is_empty()
    }

    /// Number of argument fields to ask for: the numbered ones, or one
    /// free-text field for `{args}` alone.
    pub fn fields(&self) -> usize {
        if self.numbered.is_empty() {
            usize::from(self.all)
        } else {
            self.numbered.len()
        }
    }

    /// The arguments typed into `fields`: each numbered field (its default
    /// when left empty), or, for `{args}` alone, the words of its one
    /// field.
    pub fn args(&self, fields: &[String]) -> Vec<String> {
        if self.numbered.is_empty() {
            return fields
                .first()
                .map(|f| f.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default();
        }
        self.numbered
            .iter()
            .enumerate()
            .map(|(i, default)| {
                let typed = fields.get(i).map(|f| f.trim()).unwrap_or_default();
                if typed.is_empty() {
                    default.clone().unwrap_or_default()
                } else {
                    typed.to_owned()
                }
            })
            .collect()
    }

    /// The fields a dialog starts with: the defaults (empty where there
    /// is none).
    pub fn initial_fields(&self) -> Vec<String> {
        if self.numbered.is_empty() {
            vec![String::new(); self.fields()]
        } else {
            self.numbered
                .iter()
                .map(|d| d.clone().unwrap_or_default())
                .collect()
        }
    }

    /// The numbered arguments still missing among `fields` (0-based),
    /// those without a default left empty.
    pub fn missing(&self, fields: &[String]) -> Vec<usize> {
        self.numbered
            .iter()
            .enumerate()
            .filter(|(i, default)| {
                default.is_none() && fields.get(*i).is_none_or(|f| f.trim().is_empty())
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// Most numbered arguments a macro may take (a `{1000000}` typed by
/// mistake must not open a dialog with a million fields).
pub const MAX_ARGS: usize = 32;

/// One `{...}` placeholder of a macro: a numbered argument (with its
/// default), `{args}`, or something else.
enum Placeholder<'a> {
    Numbered(usize, Option<&'a str>),
    All,
    Other,
}

fn placeholder(inner: &str) -> Placeholder<'_> {
    let (key, default) = match inner.split_once('=') {
        Some((key, default)) => (key.trim(), Some(default)),
        None => (inner.trim(), None),
    };
    if key == "args" {
        return Placeholder::All;
    }
    match key.parse::<usize>() {
        Ok(index) => Placeholder::Numbered(index, default),
        Err(_) => Placeholder::Other,
    }
}

/// Walk the `{...}` placeholders of `gcode`: `visit` gets the text before
/// each one and the placeholder with its full text (braces included).
fn walk<'a>(gcode: &'a str, mut visit: impl FnMut(&'a str, Option<(&'a str, Placeholder<'a>)>)) {
    let mut rest = gcode;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        visit(
            &rest[..open],
            Some((&rest[open..open + close + 2], placeholder(&after[..close]))),
        );
        rest = &after[close + 1..];
    }
    visit(rest, None);
}

/// The arguments `gcode` takes (see the module docs). Numbered arguments
/// beyond [`MAX_ARGS`] are not counted ([`fill`] then reports them).
pub fn params(gcode: &str) -> MacroParams {
    let mut out = MacroParams::default();
    walk(gcode, |_, placeholder| match placeholder {
        Some((_, Placeholder::All)) => out.all = true,
        Some((_, Placeholder::Numbered(index, default))) if index < MAX_ARGS => {
            if out.numbered.len() <= index {
                out.numbered.resize(index + 1, None);
            }
            if let (Some(slot @ None), Some(d)) = (out.numbered.get_mut(index), default) {
                *slot = Some(d.to_owned());
            }
        }
        _ => {}
    });
    out
}

/// `gcode` with its arguments filled in from `args`: `{n}` is `args[n]`
/// (its default when missing), `{args}` all of them. Fails, saying which,
/// when an argument without a default is missing (or numbered beyond
/// [`MAX_ARGS`]).
pub fn fill(gcode: &str, args: &[String]) -> Result<String, String> {
    let mut out = String::with_capacity(gcode.len());
    let mut missing: Vec<usize> = Vec::new();
    walk(gcode, |text, placeholder| {
        out.push_str(text);
        match placeholder {
            None => {}
            Some((_, Placeholder::All)) => out.push_str(&args.join(" ")),
            Some((_, Placeholder::Numbered(index, default))) => {
                match (args.get(index).filter(|a| !a.trim().is_empty()), default) {
                    (Some(arg), _) => out.push_str(arg.trim()),
                    (None, Some(default)) => out.push_str(default),
                    (None, None) => {
                        if !missing.contains(&index) {
                            missing.push(index);
                        }
                    }
                }
            }
            Some((raw, Placeholder::Other)) => out.push_str(raw),
        }
    });
    // An unclosed brace stays as written (walk hands it over as text).
    if missing.is_empty() {
        Ok(out)
    } else {
        missing.sort_unstable();
        let names: Vec<String> = missing.iter().map(|i| format!("{{{i}}}")).collect();
        Err(format!("the macro needs a value for {}", names.join(", ")))
    }
}

/// A machine state for previewing what a template sends: the position,
/// tool, targets and fan the printer reported (the host renders macros
/// with the same values when they run).
pub fn machine_state(state: &PrinterState) -> MachineState {
    let mut m = MachineState::new();
    if let Some(p) = &state.position {
        m.position = [p.x, p.y, p.z, p.e].map(f64::from);
    }
    m.tool = state.active_tool;
    m.hotend_targets = state
        .temps
        .hotends
        .iter()
        .map(|h| h.target.unwrap_or(0.0))
        .collect();
    if m.hotend_targets.is_empty() {
        m.hotend_targets.push(0.0);
    }
    m.bed_target = state
        .temps
        .bed
        .as_ref()
        .and_then(|b| b.target)
        .unwrap_or(0.0);
    m.fan = state.fan_speeds.first().copied().unwrap_or(0);
    m.feedrate_pct = state.feedrate_pct;
    m
}

/// The commands `template` (a macro's G-code with its arguments filled
/// in, a script) sends now: placeholders such as `{x}` and
/// `{heaters_off}` rendered with the printer's state and `profile`, the
/// way the host renders macros (`{heaters_off}` covers every heater the
/// printer reports, too).
pub fn rendered(template: &str, state: &PrinterState, profile: &PrinterProfile) -> Vec<String> {
    let machine = machine_state(state);
    let mut ctx: ScriptContext<'_> = profile.script_context(&machine);
    let known = state.temps.hotends.len().max(machine.hotend_targets.len());
    ctx.hotends = ctx
        .hotends
        .max(u8::try_from(known).unwrap_or(u8::MAX))
        .max(1);
    ctx.heated_bed |= state.temps.bed.is_some();
    ctx.heated_chamber |= state.temps.chamber.is_some();
    render_script_with(template, &ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_found_with_their_defaults() {
        let p = params("M104 S{0}\nM140 S{1=60}\nM117 {args} at {x}");
        assert_eq!(p.numbered, vec![None, Some("60".to_owned())]);
        assert!(p.all && p.any());
        assert_eq!(p.fields(), 2);
        assert_eq!(p.initial_fields(), ["", "60"]);
        assert_eq!(p.missing(&["".into(), "".into()]), [0]);
        assert!(p.missing(&["210".into(), "".into()]).is_empty());
        // Machine-state placeholders are not arguments.
        let plain = params("G1 Z{z}\n{heaters_off}\nG28 {unclosed");
        assert!(!plain.any());
        assert_eq!(plain.fields(), 0);
        // {args} alone: one free-text field, split into words.
        let all = params("M117 {args}");
        assert_eq!(all.fields(), 1);
        assert_eq!(all.args(&["  Hello   there ".into()]), ["Hello", "there"]);
        // Gaps count up to the highest number; huge numbers are ignored.
        assert_eq!(params("{2}").numbered.len(), 3);
        assert!(!params(&format!("{{{}}}", usize::MAX)).any());
        assert!(!params("{99}").any());
    }

    #[test]
    fn filling_matches_the_shell() {
        let heat = "M104 S{0}\nM140 S{1=60}\nM117 {args} at {x}";
        assert_eq!(
            fill(heat, &["210".into()]).as_deref(),
            Ok("M104 S210\nM140 S60\nM117 210 at {x}")
        );
        assert_eq!(
            fill(heat, &["200".into(), "70".into()]).as_deref(),
            Ok("M104 S200\nM140 S70\nM117 200 70 at {x}")
        );
        let missing = fill(heat, &[]).expect_err("missing");
        assert!(missing.contains("{0}"), "{missing}");
        assert_eq!(fill("G28 {unclosed", &[]).as_deref(), Ok("G28 {unclosed"));
        assert!(fill(&format!("M117 {{{}}}", usize::MAX), &[]).is_err());
        // Fields left empty take the default.
        let p = params(heat);
        assert_eq!(
            fill(heat, &p.args(&["205".into(), " ".into()])).as_deref(),
            Ok("M104 S205\nM140 S60\nM117 205 60 at {x}")
        );
    }

    #[test]
    fn previews_render_machine_placeholders() {
        let mut state = PrinterState {
            position: Some(sprint_host::sprint_proto::PositionReport {
                x: 12.5,
                y: 30.0,
                z: 0.2,
                e: 4.0,
                stepper_counts: None,
            }),
            ..PrinterState::default()
        };
        let profile = PrinterProfile::default();
        assert_eq!(
            rendered("G1 X{x} Y{y}\n{heaters_off}\nM107", &state, &profile),
            ["G1 X12.5 Y30", "M104 S0", "M140 S0", "M107"]
        );
        // A second hotend the printer reports is switched off too.
        state.temps.hotends = vec![Default::default(), Default::default()];
        let lines = rendered("{heaters_off}", &state, &profile);
        assert_eq!(lines, ["M104 T0 S0", "M104 T1 S0", "M140 S0"]);
    }
}
