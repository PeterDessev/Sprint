//! Text reports of printer state (status, settings, mesh, firmware, SD
//! card, ...) shared by the line shell, the TUI and the CLI.
//!
//! A report is a list of [`ReportLine`]s made of [`Piece`]s with a
//! semantic [`Tone`]. The shell prints them with ANSI colors (or plain),
//! the TUI turns the tones into theme styles. Text coming from files or
//! the printer is sanitized: control characters never reach the terminal.

use std::fmt::Write as _;

use ratatui::style::{Color, Modifier, Style};
use sprint_host::sprint_proto::{
    BedMesh, GcodeDoc, PrinterSettings, SettingCategory, StatsReport, capability_description,
    setting_def,
};
use sprint_host::{
    ConnectionStatus, JobInfo, JobKind, JobStatus, LinkStats, Macro, PortInfo, PrinterState,
    SdState, TempPreset,
};

use crate::commands::{COMMANDS, CommandSpec, Group};
use crate::format;
use crate::loader::LoadedFile;
use crate::theme::{ColorSupport, Theme};
use crate::widgets::util::sanitize;
use crate::widgets::{part_bounds, summary_rows};

/// The meaning of a piece of report text, mapped to a style by the
/// front end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tone {
    /// Normal text.
    Plain,
    /// Secondary text: units, hints, placeholders.
    Dim,
    /// Row labels.
    Label,
    /// Section headings and values that matter.
    Accent,
    /// Good news: connected, at target, open endstop, success.
    Good,
    /// Attention: heating, triggered, paused.
    Warn,
    /// Errors and failures.
    Bad,
    /// A heatmap value, 0 (low) ..= 1 (high).
    Heat(f32),
}

/// A run of text with one tone.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    /// The text (already sanitized).
    pub text: String,
    /// Its tone.
    pub tone: Tone,
}

/// One line of a report.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReportLine {
    /// The pieces, left to right.
    pub pieces: Vec<Piece>,
}

impl ReportLine {
    /// An empty line.
    pub fn new() -> ReportLine {
        ReportLine::default()
    }

    /// A line of plain text.
    pub fn plain(text: impl AsRef<str>) -> ReportLine {
        ReportLine::new().with(text, Tone::Plain)
    }

    /// A line of text in one tone.
    pub fn toned(text: impl AsRef<str>, tone: Tone) -> ReportLine {
        ReportLine::new().with(text, tone)
    }

    /// A heading.
    pub fn heading(text: impl AsRef<str>) -> ReportLine {
        ReportLine::toned(text, Tone::Accent)
    }

    /// A `label  value` row with the label padded to `width` columns.
    pub fn row(label: &str, width: usize, value: impl AsRef<str>, tone: Tone) -> ReportLine {
        ReportLine::new()
            .with(format!("{label:<width$}"), Tone::Label)
            .with(value, tone)
    }

    /// Append text (sanitized) in a tone.
    #[must_use]
    pub fn with(mut self, text: impl AsRef<str>, tone: Tone) -> ReportLine {
        self.push(text, tone);
        self
    }

    /// Append text (sanitized) in a tone.
    pub fn push(&mut self, text: impl AsRef<str>, tone: Tone) {
        let text = sanitize(text.as_ref()).into_owned();
        if text.is_empty() {
            return;
        }
        match self.pieces.last_mut() {
            Some(last) if last.tone == tone => last.text.push_str(&text),
            _ => self.pieces.push(Piece { text, tone }),
        }
    }

    /// The text without styles.
    pub fn text(&self) -> String {
        self.pieces.iter().map(|p| p.text.as_str()).collect()
    }

    /// The line with ANSI escape sequences for `support` (none for
    /// [`ColorSupport::None`]), without a line break.
    pub fn ansi(&self, support: ColorSupport) -> String {
        if !support.has_color() {
            return self.text();
        }
        let theme = Theme::for_support(support);
        let mut out = String::new();
        for piece in &self.pieces {
            let style = shell_style(&theme, piece.tone);
            let start = style_escape(style);
            if start.is_empty() {
                out.push_str(&piece.text);
            } else {
                let _ = write!(out, "{start}{}\x1b[0m", piece.text);
            }
        }
        out
    }
}

/// The colors to print reports with on standard output: none when it is
/// not a terminal (pipes, files) or `NO_COLOR` is set; on Windows the
/// console's escape-sequence processing is switched on first.
pub fn stdout_colors() -> ColorSupport {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return ColorSupport::None;
    }
    #[cfg(windows)]
    if !crossterm::ansi_support::supports_ansi() {
        return ColorSupport::None;
    }
    ColorSupport::detect()
}

/// Lines joined with newlines (ANSI-styled for `support`), with a final
/// newline when there are any lines.
pub fn render(lines: &[ReportLine], support: ColorSupport) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line.ansi(support).trim_end());
        out.push('\n');
    }
    out
}

/// The color of a heatmap value: blue (low), cyan, green, yellow, red
/// (high).
pub fn heat_rgb(t: f32) -> [u8; 3] {
    const STOPS: [[f32; 3]; 5] = [
        [59.0, 110.0, 230.0],
        [40.0, 190.0, 220.0],
        [70.0, 200.0, 110.0],
        [240.0, 200.0, 60.0],
        [235.0, 80.0, 60.0],
    ];
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let scaled = t * (STOPS.len() - 1) as f32;
    // In 0..=4 by the clamp above.
    let i = (scaled.floor() as usize).min(STOPS.len() - 2);
    let f = scaled - i as f32;
    let mix = |k: usize| {
        let v = STOPS[i][k] + (STOPS[i + 1][k] - STOPS[i][k]) * f;
        // In 0..=255 by construction.
        v.round().clamp(0.0, 255.0) as u8
    };
    [mix(0), mix(1), mix(2)]
}

/// The ANSI color nearest a heatmap value on 16-color terminals.
fn heat_ansi(t: f32) -> Color {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.5
    };
    match (t * 4.0).round() as u8 {
        0 => Color::Blue,
        1 => Color::Cyan,
        2 => Color::Green,
        3 => Color::Yellow,
        _ => Color::Red,
    }
}

/// The style of a tone in the TUI.
pub fn tone_style(theme: &Theme, tone: Tone) -> Style {
    match tone {
        Tone::Plain => theme.text(),
        Tone::Dim => theme.dim(),
        Tone::Label => theme.dim(),
        Tone::Accent => theme.accent(),
        Tone::Good => theme.success(),
        Tone::Warn => theme.warning(),
        Tone::Bad => theme.error(),
        Tone::Heat(t) => {
            if theme.has_color() {
                Style::default()
                    .fg(theme.support.color(heat_rgb(t), heat_ansi(t)))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            }
        }
    }
}

/// The style of a tone on the user's own terminal background: plain
/// text keeps the terminal's colors.
fn shell_style(theme: &Theme, tone: Tone) -> Style {
    match tone {
        Tone::Plain => Style::default(),
        other => tone_style(theme, other),
    }
}

/// The SGR sequence that starts `style` (empty for the default style).
fn style_escape(style: Style) -> String {
    let mut codes: Vec<String> = Vec::new();
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("1".into());
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push("2".into());
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        codes.push("7".into());
    }
    if let Some(fg) = style.fg
        && let Some(code) = color_code(fg)
    {
        codes.push(code);
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// SGR foreground parameters for a color.
fn color_code(color: Color) -> Option<String> {
    let named = |n: u8| Some(n.to_string());
    match color {
        Color::Reset => None,
        Color::Black => named(30),
        Color::Red => named(31),
        Color::Green => named(32),
        Color::Yellow => named(33),
        Color::Blue => named(34),
        Color::Magenta => named(35),
        Color::Cyan => named(36),
        Color::Gray => named(37),
        Color::DarkGray => named(90),
        Color::LightRed => named(91),
        Color::LightGreen => named(92),
        Color::LightYellow => named(93),
        Color::LightBlue => named(94),
        Color::LightMagenta => named(95),
        Color::LightCyan => named(96),
        Color::White => named(97),
        Color::Indexed(i) => Some(format!("38;5;{i}")),
        Color::Rgb(r, g, b) => Some(format!("38;2;{r};{g};{b}")),
    }
}

/// Width of the label column of the status report.
const LABEL: usize = 10;

/// `current / target °C` of a heater reading (`--` for unknown values).
pub fn heater_text(current: f32, target: Option<f32>) -> String {
    let now = format::temperature(current);
    match target.filter(|t| t.is_finite() && *t > 0.0) {
        Some(t) => format!("{now} / {} °C", format::trimmed(f64::from(t), 1)),
        None => format!("{now} °C"),
    }
}

/// The tone of a connection status.
pub fn status_tone(status: &ConnectionStatus) -> Tone {
    match status {
        ConnectionStatus::Connected => Tone::Good,
        ConnectionStatus::Connecting | ConnectionStatus::Handshaking => Tone::Warn,
        ConnectionStatus::Failed(_) => Tone::Bad,
        ConnectionStatus::Disconnected => Tone::Dim,
    }
}

/// The tone of a job status.
pub fn job_tone(status: &JobStatus) -> Tone {
    match status {
        JobStatus::Starting | JobStatus::Printing | JobStatus::Resuming => Tone::Accent,
        JobStatus::Pausing | JobStatus::Paused | JobStatus::Cancelling => Tone::Warn,
        JobStatus::Completed => Tone::Good,
        JobStatus::Cancelled | JobStatus::Failed(_) => Tone::Bad,
    }
}

/// "Printing", "Failed: printer halted".
pub fn job_status_text(status: &JobStatus) -> String {
    match status {
        JobStatus::Failed(why) if !why.is_empty() => format!("Failed: {why}"),
        other => other.label().to_string(),
    }
}

/// "42.0%  layer 12/100  elapsed 12:00  ETA 20:00" for a job.
pub fn job_progress_text(job: &JobInfo) -> String {
    let mut out = format::percent(f64::from(job.progress));
    if let Some((layer, total)) = job.layer {
        let _ = write!(out, "  layer {}/{total}", (layer + 1).min(total.max(1)));
    }
    if job.kind == JobKind::SdUpload || job.layer.is_none() {
        let _ = write!(
            out,
            "  {}/{}",
            format::bytes(job.bytes_done),
            format::bytes(job.bytes_total)
        );
    }
    let _ = write!(out, "  elapsed {}", format::clock_of(job.elapsed));
    if let Some(eta) = job.eta {
        let _ = write!(out, "  ETA {}", format::clock_of(eta));
    }
    out
}

/// The `status` report.
pub fn status_lines(state: &PrinterState, file: Option<&LoadedFile>) -> Vec<ReportLine> {
    let mut lines = Vec::new();
    let mut printer = ReportLine::row(
        "Printer",
        LABEL,
        format!("● {}", state.status.label()),
        status_tone(&state.status),
    );
    if let Some(target) = &state.target {
        printer.push(format!("  {target}"), Tone::Plain);
    }
    if let Some(fw) = &state.firmware {
        printer.push(format!("  {}", fw.name), Tone::Dim);
    }
    if let ConnectionStatus::Failed(why) = &state.status {
        printer.push(format!("  {why}"), Tone::Bad);
    }
    if state.halted {
        printer.push("  HALTED (M999 or reconnect)", Tone::Bad);
    }
    lines.push(printer);
    if state.status.is_connected() {
        for (tool, reading) in state.temps.hotends.iter().enumerate() {
            if reading.current.is_nan() && reading.target.is_none() {
                continue;
            }
            let label = if state.temps.hotends.len() > 1 {
                format!("Hotend T{tool}")
            } else {
                "Hotend".to_string()
            };
            lines.push(heater_line(
                &label,
                reading.current,
                reading.target,
                reading.power,
            ));
        }
        if let Some(bed) = &state.temps.bed {
            lines.push(heater_line("Bed", bed.current, bed.target, bed.power));
        }
        if let Some(chamber) = &state.temps.chamber {
            lines.push(heater_line(
                "Chamber",
                chamber.current,
                chamber.target,
                chamber.power,
            ));
        }
        let position = match &state.position {
            Some(p) => format!(
                "X {}  Y {}  Z {}  E {}",
                format::fixed(f64::from(p.x), 2),
                format::fixed(f64::from(p.y), 2),
                format::fixed(f64::from(p.z), 2),
                format::fixed(f64::from(p.e), 2)
            ),
            None => format::MISSING.to_string(),
        };
        lines.push(ReportLine::row("Position", LABEL, position, Tone::Plain));
        lines.push(ReportLine::row(
            "Overrides",
            LABEL,
            overrides_text(state),
            Tone::Plain,
        ));
        if let Some(busy) = &state.busy {
            lines.push(ReportLine::row("Busy", LABEL, busy, Tone::Warn));
        }
    }
    if let Some(job) = &state.job {
        let mut line = ReportLine::row(
            "Job",
            LABEL,
            job_status_text(&job.status),
            job_tone(&job.status),
        );
        line.push(format!("  {}  ", job.name), Tone::Plain);
        line.push(job_progress_text(job), Tone::Dim);
        lines.push(line);
    }
    if let Some(prompt) = &state.prompt {
        let mut line = ReportLine::row("Prompt", LABEL, &prompt.message, Tone::Warn);
        if !prompt.buttons.is_empty() {
            line.push(format!("  [{}]", prompt.buttons.join("] [")), Tone::Dim);
        }
        lines.push(line);
    }
    if state.status.is_open() {
        let link = &state.link;
        lines.push(ReportLine::row(
            "Link",
            LABEL,
            format!(
                "{} lines  {} resends  {} errors  {} timeouts  rtt {} ms",
                format::count(usize::try_from(link.lines_sent).unwrap_or(usize::MAX)),
                link.resends,
                link.errors,
                link.timeouts,
                format::fixed(f64::from(link.rtt_avg_ms), 1)
            ),
            Tone::Dim,
        ));
    }
    if let Some(error) = &state.last_error {
        lines.push(ReportLine::row("Last error", LABEL, error, Tone::Bad));
    }
    match file {
        Some(f) => lines.push(ReportLine::row(
            "Loaded",
            LABEL,
            format!(
                "{}  {} layers  est. {}",
                f.name(),
                f.toolpath.layers.len(),
                format::duration(f64::from(f.toolpath.stats.estimated_time))
            ),
            Tone::Plain,
        )),
        None => lines.push(ReportLine::row(
            "Loaded",
            LABEL,
            "no file (load <file>)",
            Tone::Dim,
        )),
    }
    lines
}

/// "fan 100%  speed 100%  flow 100%".
pub fn overrides_text(state: &PrinterState) -> String {
    let fan = state.fan_speeds.first().copied().unwrap_or(0);
    let flow = state
        .flow_pct
        .get(usize::from(state.active_tool))
        .or(state.flow_pct.first())
        .copied()
        .unwrap_or(100.0);
    format!(
        "fan {}  speed {}%  flow {}%",
        format::percent(f64::from(fan) / 255.0).replace(".0%", "%"),
        format::trimmed(f64::from(state.feedrate_pct), 1),
        format::trimmed(f64::from(flow), 1)
    )
}

fn heater_line(label: &str, current: f32, target: Option<f32>, power: Option<f32>) -> ReportLine {
    let state = crate::HeaterState::classify(current, target);
    let tone = match state {
        crate::HeaterState::AtTarget => Tone::Good,
        crate::HeaterState::Heating => Tone::Warn,
        _ => Tone::Plain,
    };
    let mut line = ReportLine::row(label, LABEL, heater_text(current, target), tone);
    line.push(format!("  {}", state.name()), Tone::Dim);
    if let Some(p) = power.filter(|p| p.is_finite()) {
        line.push(
            format!(
                "  power {}",
                format::percent(f64::from(p)).replace(".0%", "%")
            ),
            Tone::Dim,
        );
    }
    line
}

/// The `settings` report: settings grouped by category, filtered by a code
/// (`M92`), a category (`motion`) or part of a name (`steps`).
pub fn settings_lines(settings: &PrinterSettings, filter: Option<&str>) -> Vec<ReportLine> {
    if settings.is_empty() {
        return vec![ReportLine::toned(
            "No settings reported yet (the firmware may not support M503).",
            Tone::Dim,
        )];
    }
    let filter = filter.map(|f| f.trim().to_ascii_lowercase());
    let matches = |code: &str, key: &str, name: &str, category: SettingCategory| {
        let Some(f) = filter.as_deref() else {
            return true;
        };
        code.eq_ignore_ascii_case(f)
            || key.eq_ignore_ascii_case(f)
            || category.name().eq_ignore_ascii_case(f)
            || name.to_ascii_lowercase().contains(f)
    };
    let mut lines = Vec::new();
    for category in SettingCategory::ALL {
        let rows: Vec<_> = settings
            .iter()
            .filter_map(|s| {
                let def = setting_def(&s.code);
                let cat = def.map_or(SettingCategory::Other, |d| d.category);
                let name = def.map_or("", |d| d.name);
                (cat == category && matches(&s.code, &s.key(), name, cat)).then_some((s, def))
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(ReportLine::new());
        }
        lines.push(ReportLine::heading(category.name()));
        for (setting, def) in rows {
            let key = setting.key();
            let params: Vec<String> = setting
                .params
                .iter()
                .filter(|(letter, _)| !def.is_some_and(|d| d.is_selector(*letter)))
                .map(|(letter, value)| {
                    if value.is_finite() {
                        format!("{letter}{}", format::trimmed(*value, 4))
                    } else {
                        letter.to_string()
                    }
                })
                .collect();
            let mut line = ReportLine::new().with(format!("  {key:<10}"), Tone::Accent);
            line.push(format!("{:<26}", def.map_or("", |d| d.name)), Tone::Label);
            line.push(params.join(" "), Tone::Plain);
            lines.push(line);
        }
    }
    if lines.is_empty() {
        let shown = filter.unwrap_or_default();
        lines.push(ReportLine::toned(
            format!("No setting matches '{shown}'."),
            Tone::Dim,
        ));
    } else {
        lines.push(ReportLine::new());
        lines.push(ReportLine::toned(
            "Change one with set <code> <params>; eeprom save keeps changes after a reboot.",
            Tone::Dim,
        ));
    }
    lines
}

/// Shades from low to high for colorless heatmaps.
const SHADES: &[u8] = b" .:-=+*#%@";

/// The shade character of a heatmap value.
fn shade(t: f32) -> char {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    // In 0..SHADES.len() by the clamp.
    let i = ((t * (SHADES.len() - 1) as f32).round() as usize).min(SHADES.len() - 1);
    char::from(SHADES[i])
}

/// The `mesh` report: the grid with the back row on top, each value colored
/// by its height, plus an ASCII shade map and the statistics.
pub fn mesh_lines(mesh: &BedMesh) -> Vec<ReportLine> {
    let rows = mesh.rows();
    let cols = mesh.cols();
    if rows == 0 || cols == 0 || mesh.count() == 0 {
        return vec![ReportLine::toned("The mesh has no points.", Tone::Dim)];
    }
    let (Some(min), Some(max)) = (mesh.min(), mesh.max()) else {
        return vec![ReportLine::toned("The mesh has no points.", Tone::Dim)];
    };
    let range = max - min;
    let level = |v: f32| if range > 0.0 { (v - min) / range } else { 0.5 };
    let signed = |v: f32| {
        if v.is_finite() {
            format!("{v:+.3}")
        } else {
            "  ---".to_string()
        }
    };
    let mut lines = vec![
        ReportLine::heading(format!(
            "Bed mesh: {} x {} points ({})",
            cols,
            rows,
            if mesh.kind.is_empty() {
                "unknown kind"
            } else {
                mesh.kind.as_str()
            }
        )),
        ReportLine::new()
            .with("range ", Tone::Label)
            .with(
                format!("{} mm", format::fixed(f64::from(range), 3)),
                Tone::Accent,
            )
            .with("   min ", Tone::Label)
            .with(signed(min), Tone::Plain)
            .with("   max ", Tone::Label)
            .with(signed(max), Tone::Plain)
            .with("   mean ", Tone::Label)
            .with(mesh.mean().map_or_else(|| "--".into(), signed), Tone::Plain)
            .with("   std dev ", Tone::Label)
            .with(
                mesh.std_dev()
                    .map_or_else(|| "--".into(), |s| format::fixed(f64::from(s), 3)),
                Tone::Plain,
            ),
        ReportLine::new(),
    ];
    // Column headings: X positions when known, else indices.
    let mut header = ReportLine::new().with(format!("{:>8}", ""), Tone::Dim);
    for col in 0..cols {
        let label = match mesh.x_positions.as_ref().and_then(|x| x.get(col)) {
            Some(x) if x.is_finite() => format!("X{}", format::fixed(f64::from(*x), 0)),
            _ => format!("#{col}"),
        };
        header.push(format!("{label:>8}"), Tone::Dim);
    }
    header.push("     ", Tone::Dim);
    lines.push(header);
    for row in (0..rows).rev() {
        let label = match mesh.y_positions.as_ref().and_then(|y| y.get(row)) {
            Some(y) if y.is_finite() => format!("Y{}", format::fixed(f64::from(*y), 0)),
            _ => format!("#{row}"),
        };
        let mut line = ReportLine::new().with(format!("{label:>8}"), Tone::Dim);
        let mut shades = String::new();
        for col in 0..cols {
            let v = mesh.get(row, col).unwrap_or(f32::NAN);
            if v.is_finite() {
                line.push(format!("{:>8}", signed(v)), Tone::Heat(level(v)));
                let c = shade(level(v));
                shades.push(c);
                shades.push(c);
            } else {
                line.push(format!("{:>8}", "---"), Tone::Dim);
                shades.push_str("  ");
            }
        }
        line.push(format!("     |{shades}|"), Tone::Dim);
        if row == rows - 1 {
            line.push("  back", Tone::Dim);
        } else if row == 0 {
            line.push("  front", Tone::Dim);
        }
        lines.push(line);
    }
    lines.push(ReportLine::new());
    let legend: String = SHADES.iter().map(|&b| char::from(b)).collect();
    lines.push(
        ReportLine::new()
            .with("low ", Tone::Dim)
            .with(format!("|{legend}|"), Tone::Dim)
            .with(" high", Tone::Dim),
    );
    lines
}

/// The `firmware` report.
pub fn firmware_lines(state: &PrinterState) -> Vec<ReportLine> {
    let Some(fw) = &state.firmware else {
        return vec![ReportLine::toned(
            "No firmware report yet (M115 was not answered).",
            Tone::Dim,
        )];
    };
    let width = 12;
    let mut lines = vec![ReportLine::row("Firmware", width, &fw.name, Tone::Accent)];
    let mut add = |label: &str, value: Option<&str>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            lines.push(ReportLine::row(label, width, v, Tone::Plain));
        }
    };
    add("Version", fw.version.as_deref());
    add("Flavor", Some(fw.flavor.name()));
    add("Machine", fw.machine_type.as_deref());
    let extruders = fw.extruder_count.map(|n| n.to_string());
    add("Extruders", extruders.as_deref());
    add("Protocol", fw.protocol_version.as_deref());
    add("Source", fw.source_url.as_deref());
    add("UUID", fw.uuid.as_deref());
    if !state.capabilities.is_empty() {
        lines.push(ReportLine::new());
        lines.push(ReportLine::heading(format!(
            "Capabilities ({} of {} enabled)",
            state.capabilities.iter().filter(|(_, on)| *on).count(),
            state.capabilities.len()
        )));
        let name_cols = state
            .capabilities
            .iter()
            .map(|(n, _)| n.len())
            .max()
            .unwrap_or(0)
            .min(28);
        for (name, on) in &state.capabilities {
            let (mark, tone) = if *on {
                ("✓ ", Tone::Good)
            } else {
                ("✗ ", Tone::Dim)
            };
            let mut line = ReportLine::new()
                .with("  ", Tone::Plain)
                .with(mark, tone)
                .with(
                    format!("{name:<name_cols$}"),
                    if *on { Tone::Plain } else { Tone::Dim },
                );
            if let Some(desc) = capability_description(name) {
                line.push(format!("  {desc}"), Tone::Dim);
            }
            lines.push(line);
        }
    }
    lines
}

/// The `endstops` report.
pub fn endstop_lines(endstops: &[(String, bool)]) -> Vec<ReportLine> {
    if endstops.is_empty() {
        return vec![ReportLine::toned("No endstop report yet.", Tone::Dim)];
    }
    let width = endstops.iter().map(|(n, _)| n.len()).max().unwrap_or(0) + 2;
    endstops
        .iter()
        .map(|(name, triggered)| {
            let (text, tone) = if *triggered {
                ("TRIGGERED", Tone::Warn)
            } else {
                ("open", Tone::Good)
            };
            ReportLine::row(name, width, text, tone)
        })
        .collect()
}

/// The `sd ls` report.
pub fn sd_lines(sd: &SdState) -> Vec<ReportLine> {
    let mut lines = Vec::new();
    if sd.card_present == Some(false) {
        return vec![ReportLine::toned("No SD card inserted.", Tone::Warn)];
    }
    if sd.files.is_empty() {
        lines.push(ReportLine::toned("The card holds no files.", Tone::Dim));
    } else {
        lines.push(ReportLine::heading(format!(
            "{} file{} on the SD card",
            sd.files.len(),
            if sd.files.len() == 1 { "" } else { "s" }
        )));
        let width = sd
            .files
            .iter()
            .map(|f| f.name.len())
            .max()
            .unwrap_or(0)
            .min(40)
            + 2;
        for file in &sd.files {
            let mut line = ReportLine::new().with(format!("  {:<width$}", file.name), Tone::Plain);
            line.push(
                format!(
                    "{:>9}",
                    file.size.map_or_else(|| "--".to_string(), format::bytes)
                ),
                Tone::Dim,
            );
            if let Some(long) = file.long_name.as_deref().filter(|l| *l != file.name) {
                line.push(format!("  {long}"), Tone::Dim);
            }
            lines.push(line);
        }
    }
    if let Some((done, total)) = sd.printing {
        let fraction = if total > 0 {
            done as f64 / total as f64
        } else {
            0.0
        };
        lines.push(ReportLine::row(
            "Printing",
            10,
            format!(
                "{}  {}",
                sd.selected.as_deref().unwrap_or("?"),
                format::percent(fraction)
            ),
            Tone::Accent,
        ));
    }
    lines
}

/// The `ports` report (and `sprint ports`): name, description, VID:PID and
/// a marker on likely printers.
pub fn ports_lines(ports: &[PortInfo]) -> Vec<ReportLine> {
    if ports.is_empty() {
        return vec![
            ReportLine::toned("No serial ports found.", Tone::Dim),
            ReportLine::toned(
                "Use connect sim for the built-in virtual printer.",
                Tone::Dim,
            ),
        ];
    }
    let name_w = ports
        .iter()
        .map(|p| p.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let desc_w = ports
        .iter()
        .map(|p| p.description.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(11, 40);
    let mut lines = vec![ReportLine::toned(
        format!(
            "{:<name_w$}  {:<desc_w$}  {:<9}  ",
            "PORT", "DESCRIPTION", "VID:PID"
        ),
        Tone::Label,
    )];
    for p in ports {
        let ids = match (p.vid, p.pid) {
            (Some(v), Some(pid)) => format!("{v:04X}:{pid:04X}"),
            _ => "--".to_string(),
        };
        let desc = crate::widgets::util::truncate(&p.description, desc_w);
        let mut line = ReportLine::new()
            .with(
                format!("{:<name_w$}  ", p.name),
                if p.likely_printer {
                    Tone::Accent
                } else {
                    Tone::Plain
                },
            )
            .with(format!("{desc:<desc_w$}  "), Tone::Plain)
            .with(format!("{ids:<9}  "), Tone::Dim);
        if p.likely_printer {
            line.push("● likely printer", Tone::Good);
        }
        lines.push(line);
    }
    lines
}

/// The `stats` report: the firmware's print statistics and the link's.
pub fn stats_lines(stats: Option<&StatsReport>, link: &LinkStats) -> Vec<ReportLine> {
    let width = 14;
    let mut lines = vec![ReportLine::heading("Printer statistics (M78)")];
    match stats {
        Some(s) => {
            let mut add = |label: &str, value: Option<String>| {
                if let Some(v) = value {
                    lines.push(ReportLine::row(label, width, v, Tone::Plain));
                }
            };
            add("Prints", s.prints.map(|n| n.to_string()));
            add("Finished", s.finished.map(|n| n.to_string()));
            add("Failed", s.failed.map(|n| n.to_string()));
            add(
                "Total time",
                s.total_time.map(|t| format::duration(t as f64)),
            );
            add(
                "Longest job",
                s.longest_job.map(|t| format::duration(t as f64)),
            );
            add("Filament used", s.filament_used_mm.map(format::length_mm));
        }
        None => lines.push(ReportLine::toned(
            "  not reported (the firmware may lack PRINTCOUNTER)",
            Tone::Dim,
        )),
    }
    lines.push(ReportLine::new());
    lines.push(ReportLine::heading("Link"));
    let count = |n: u64| format::count(usize::try_from(n).unwrap_or(usize::MAX));
    for (label, value) in [
        ("Connected", format::duration(link.connected_secs)),
        ("Lines sent", count(link.lines_sent)),
        (
            "Traffic",
            format!(
                "{} sent, {} received",
                format::bytes(link.bytes_sent),
                format::bytes(link.bytes_received)
            ),
        ),
        ("Acknowledged", count(link.oks)),
        (
            "Resends",
            format!(
                "{}  errors {}  timeouts {}",
                link.resends, link.errors, link.timeouts
            ),
        ),
        (
            "Round trip",
            format!(
                "{} ms avg, {} ms max",
                format::fixed(f64::from(link.rtt_avg_ms), 1),
                format::fixed(f64::from(link.rtt_max_ms), 1)
            ),
        ),
        (
            "Throughput",
            format!(
                "{} lines/s",
                format::fixed(f64::from(link.lines_per_sec), 0)
            ),
        ),
    ] {
        lines.push(ReportLine::row(label, width, value, Tone::Plain));
    }
    lines
}

/// The `info` report: statistics of the loaded file.
pub fn file_lines(loaded: &LoadedFile) -> Vec<ReportLine> {
    let file = &loaded.file;
    let toolpath = &loaded.toolpath;
    let mut lines = vec![
        ReportLine::heading(file.name()),
        ReportLine::toned(format!("  {}", loaded.path.display()), Tone::Dim),
        ReportLine::toned(
            format!(
                "  {}, {} lines ({} commands), analyzed in {}",
                format::bytes(u64::try_from(file.byte_len()).unwrap_or(u64::MAX)),
                format::count(file.len()),
                format::count(file.command_count()),
                format_elapsed(loaded.load_time)
            ),
            Tone::Dim,
        ),
        ReportLine::new(),
    ];
    let bounds = part_bounds(toolpath);
    let rows = summary_rows(&toolpath.stats, &toolpath.metadata, Some(&bounds));
    let width = rows.iter().map(|r| r.label.len()).max().unwrap_or(0) + 2;
    for row in rows {
        let mut line = ReportLine::row(row.label, width, &row.value, Tone::Plain);
        if let Some(note) = &row.note {
            line.push(format!("  ({note})"), Tone::Dim);
        }
        lines.push(line);
    }
    lines
}

/// "35 ms", "1.2 s".
fn format_elapsed(elapsed: std::time::Duration) -> String {
    let ms = elapsed.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{} ms", format::fixed(ms, 0))
    } else {
        format!("{} s", format::fixed(ms / 1000.0, 1))
    }
}

/// The command list for `help`.
pub fn help_lines() -> Vec<ReportLine> {
    /// Usage column width; longer usages get the summary on the next line.
    const USAGE: usize = 30;
    let mut lines = Vec::new();
    for group in Group::ALL {
        if !lines.is_empty() {
            lines.push(ReportLine::new());
        }
        lines.push(ReportLine::heading(group.title()));
        for spec in COMMANDS.iter().filter(|c| c.group == group) {
            if spec.usage.chars().count() > USAGE {
                lines.push(ReportLine::plain(format!("  {}", spec.usage)));
                lines.push(ReportLine::toned(
                    format!("  {:USAGE$}  {}", "", spec.summary),
                    Tone::Dim,
                ));
            } else {
                lines.push(
                    ReportLine::new()
                        .with(format!("  {:<USAGE$}  ", spec.usage), Tone::Plain)
                        .with(spec.summary, Tone::Dim),
                );
            }
        }
    }
    lines.push(ReportLine::new());
    lines.push(ReportLine::toned(
        "Anything else that looks like G-code (G28, M105, BED_MESH_CALIBRATE) is sent as typed;",
        Tone::Dim,
    ));
    lines.push(ReportLine::toned(
        "prefix a line with ! to send it raw. help <command> or help <G-code> explains one.",
        Tone::Dim,
    ));
    lines
}

/// `help <command>`.
pub fn command_help(spec: &CommandSpec) -> Vec<ReportLine> {
    let mut lines = vec![
        ReportLine::new()
            .with("usage: ", Tone::Label)
            .with(spec.usage, Tone::Accent),
        ReportLine::plain(format!("  {}", spec.summary)),
    ];
    if !spec.aliases.is_empty() {
        lines.push(ReportLine::toned(
            format!("  also: {}", spec.aliases.join(", ")),
            Tone::Dim,
        ));
    }
    lines
}

/// `help <G-code>`: the reference entry.
pub fn gcode_help(doc: &GcodeDoc) -> Vec<ReportLine> {
    let mut title = ReportLine::new()
        .with(doc.code, Tone::Accent)
        .with(format!("  {}", doc.name), Tone::Plain)
        .with(format!("  ({})", doc.category.name()), Tone::Dim);
    if doc.dangerous {
        title.push("  use with care", Tone::Bad);
    }
    vec![
        title,
        ReportLine::new()
            .with("usage: ", Tone::Label)
            .with(doc.usage, Tone::Plain),
        ReportLine::toned(format!("  {}", doc.description), Tone::Dim),
    ]
}

/// The `macro` list.
pub fn macro_lines(macros: &[Macro]) -> Vec<ReportLine> {
    if macros.is_empty() {
        return vec![ReportLine::toned(
            "No macros defined (add them to the configuration file).",
            Tone::Dim,
        )];
    }
    let width = macros
        .iter()
        .map(|m| m.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(24)
        + 2;
    macros
        .iter()
        .map(|m| {
            let gcode: Vec<&str> = m
                .gcode
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let mut line = ReportLine::new()
                .with(format!("  {:<width$}", m.name), Tone::Accent)
                .with(gcode.join(" | "), Tone::Dim);
            if m.confirm {
                line.push("  (asks first)", Tone::Dim);
            }
            line
        })
        .collect()
}

/// The preset list (`preheat` without a name).
pub fn preset_lines(presets: &[TempPreset]) -> Vec<ReportLine> {
    if presets.is_empty() {
        return vec![ReportLine::toned("The profile has no presets.", Tone::Dim)];
    }
    let width = presets
        .iter()
        .map(|p| p.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    presets
        .iter()
        .map(|p| {
            ReportLine::new()
                .with(format!("  {:<width$}", p.name), Tone::Accent)
                .with(
                    format!(
                        "hotend {} °C, bed {} °C",
                        format::trimmed(f64::from(p.hotend), 1),
                        format::trimmed(f64::from(p.bed), 1)
                    ),
                    Tone::Plain,
                )
        })
        .collect()
}

/// A one-line live status for `monitor` and `sprint print`:
/// `"Printing 42.0%  layer 12/100  ETA 20:00  T0 205/210  B 60/60"`.
pub fn monitor_text(state: &PrinterState) -> String {
    let mut out = String::new();
    if !state.status.is_connected() {
        out.push_str(state.status.label());
    }
    if let Some(job) = state.job.as_ref() {
        if !out.is_empty() {
            out.push_str("  ");
        }
        let _ = write!(
            out,
            "{} {}",
            job.status.label(),
            format::percent(f64::from(job.progress))
        );
        if let Some((layer, total)) = job.layer {
            let _ = write!(out, "  layer {}/{total}", (layer + 1).min(total.max(1)));
        }
        if let Some(eta) = job.eta.filter(|_| job.status.is_active()) {
            let _ = write!(out, "  ETA {}", format::clock_of(eta));
        }
    }
    let temp = |current: f32, target: Option<f32>| {
        let now = format::fixed(f64::from(current), 0);
        match target.filter(|t| t.is_finite() && *t > 0.0) {
            Some(t) => format!("{now}/{}", format::fixed(f64::from(t), 0)),
            None => now,
        }
    };
    for (tool, h) in state.temps.hotends.iter().enumerate() {
        if h.current.is_finite() {
            let _ = write!(out, "  T{tool} {}", temp(h.current, h.target));
        }
    }
    if let Some(bed) = state.temps.bed.as_ref().filter(|b| b.current.is_finite()) {
        let _ = write!(out, "  B {}", temp(bed.current, bed.target));
    }
    if let Some(p) = &state.position {
        let _ = write!(out, "  Z {}", format::fixed(f64::from(p.z), 2));
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::sprint_proto::{
        FirmwareInfo, HeaterReading, PositionReport, SdFile, SettingCommand, TempReport,
    };
    use sprint_host::{HostPrompt, JobInfo};

    fn texts(lines: &[ReportLine]) -> Vec<String> {
        lines.iter().map(ReportLine::text).collect()
    }

    fn joined(lines: &[ReportLine]) -> String {
        texts(lines).join("\n")
    }

    fn connected_state() -> PrinterState {
        let mut job = JobInfo::new(JobKind::HostPrint, "cube.gcode", 1000, 50_000);
        job.status = JobStatus::Printing;
        job.progress = 0.42;
        job.layer = Some((11, 100));
        job.elapsed = std::time::Duration::from_secs(720);
        job.eta = Some(std::time::Duration::from_secs(1200));
        PrinterState {
            status: ConnectionStatus::Connected,
            target: Some("Virtual printer".into()),
            firmware: Some(FirmwareInfo {
                name: "Marlin 2.1.2.1".into(),
                version: Some("2.1.2.1".into()),
                machine_type: Some("Sprint Virtual Printer".into()),
                extruder_count: Some(1),
                ..FirmwareInfo::default()
            }),
            capabilities: vec![("AUTOREPORT_TEMP".into(), true), ("EEPROM".into(), false)],
            temps: TempReport {
                hotends: vec![HeaterReading {
                    current: 205.3,
                    target: Some(210.0),
                    power: Some(0.64),
                }],
                bed: Some(HeaterReading {
                    current: 60.0,
                    target: Some(60.0),
                    power: Some(0.3),
                }),
                ..TempReport::default()
            },
            position: Some(PositionReport {
                x: 110.0,
                y: 110.5,
                z: 0.2,
                e: 3.25,
                stepper_counts: None,
            }),
            job: Some(job),
            fan_speeds: vec![255],
            ..PrinterState::default()
        }
    }

    #[test]
    fn status_reports_everything_at_a_glance() {
        let text = joined(&status_lines(&connected_state(), None));
        for fact in [
            "Printer   ● Connected  Virtual printer  Marlin 2.1.2.1",
            "Hotend    205.3 / 210 °C  heating  power 64%",
            "Bed       60.0 / 60 °C  at target  power 30%",
            "Position  X 110.00  Y 110.50  Z 0.20  E 3.25",
            "Overrides fan 100%  speed 100%  flow 100%",
            "Job       Printing  cube.gcode  42.0%  layer 12/100  elapsed 12:00  ETA 20:00",
            "Link      0 lines",
            "Loaded    no file (load <file>)",
        ] {
            assert!(text.contains(fact), "missing {fact:?} in\n{text}");
        }
        let offline = joined(&status_lines(&PrinterState::default(), None));
        assert!(offline.starts_with("Printer   ● Disconnected"), "{offline}");
        assert!(!offline.contains("Position"), "{offline}");
        let mut halted = connected_state();
        halted.halted = true;
        halted.prompt = Some(HostPrompt {
            message: "Filament runout".into(),
            buttons: vec!["Continue".into(), "Purge".into()],
        });
        let text = joined(&status_lines(&halted, None));
        assert!(text.contains("HALTED"), "{text}");
        assert!(
            text.contains("Prompt    Filament runout  [Continue] [Purge]"),
            "{text}"
        );
    }

    #[test]
    fn printer_text_is_sanitized() {
        let mut state = connected_state();
        state.target = Some("evil\x1b[2J\x07".into());
        let lines = status_lines(&state, None);
        for line in &lines {
            assert!(!line.text().contains('\x1b'), "{line:?}");
            assert!(!line.text().contains('\x07'), "{line:?}");
        }
    }

    #[test]
    fn ansi_colors_follow_the_support_level() {
        let line = ReportLine::new()
            .with("ok ", Tone::Plain)
            .with("hot", Tone::Bad);
        assert_eq!(line.ansi(ColorSupport::None), "ok hot");
        let colored = line.ansi(ColorSupport::TrueColor);
        assert!(colored.starts_with("ok \x1b["), "{colored:?}");
        assert!(colored.contains("38;2;"), "{colored:?}");
        assert!(colored.ends_with("hot\x1b[0m"), "{colored:?}");
        let ansi16 = line.ansi(ColorSupport::Ansi16);
        assert!(ansi16.contains("\x1b[1;31m"), "{ansi16:?}");
        assert_eq!(
            render(&[line.clone(), ReportLine::new()], ColorSupport::None),
            "ok hot\n\n"
        );
        // Pieces of the same tone merge.
        assert_eq!(
            ReportLine::new()
                .with("a", Tone::Dim)
                .with("b", Tone::Dim)
                .pieces
                .len(),
            1
        );
    }

    #[test]
    fn settings_are_grouped_and_filterable() {
        let mut settings = PrinterSettings::new();
        for line in [
            "M92 X80 Y80 Z400 E93",
            "M203 X500 Y500 Z12 E120",
            "M301 P22.2 I1.08 D114",
            "M851 X-40 Y-10 Z-1.5",
            "M999 S1",
        ] {
            settings.insert(SettingCommand::parse(line).expect("setting"));
        }
        let text = joined(&settings_lines(&settings, None));
        assert!(
            text.starts_with("Motion\n  M92       Steps per unit"),
            "{text}"
        );
        assert!(text.contains("X80 Y80 Z400 E93"), "{text}");
        assert!(text.contains("Thermal\n  M301"), "{text}");
        assert!(text.contains("P22.2 I1.08 D114"), "{text}");
        assert!(text.contains("Probing"), "{text}");
        assert!(text.contains("Other\n  M999"), "{text}");
        assert!(text.contains("eeprom save"), "{text}");
        let motion = joined(&settings_lines(&settings, Some("motion")));
        assert!(
            motion.contains("M92") && motion.contains("M203"),
            "{motion}"
        );
        assert!(!motion.contains("M301"), "{motion}");
        let code = joined(&settings_lines(&settings, Some("m301")));
        assert!(code.contains("M301") && !code.contains("M92 "), "{code}");
        let none = joined(&settings_lines(&settings, Some("nothing-like-this")));
        assert_eq!(none, "No setting matches 'nothing-like-this'.");
        let empty = joined(&settings_lines(&PrinterSettings::new(), None));
        assert!(empty.starts_with("No settings reported"), "{empty}");
    }

    #[test]
    fn mesh_heatmap_puts_the_back_row_on_top() {
        let mut mesh = BedMesh::from_rows(
            vec![
                vec![-0.1, 0.0, 0.1],
                vec![0.0, f32::NAN, 0.05],
                vec![0.2, 0.15, 0.1],
            ],
            "bilinear",
        );
        mesh.set_bounds(10.0, 210.0, 10.0, 210.0);
        let lines = mesh_lines(&mesh);
        let text = joined(&lines);
        assert!(
            text.starts_with("Bed mesh: 3 x 3 points (bilinear)"),
            "{text}"
        );
        assert!(text.contains("range 0.300 mm"), "{text}");
        let rows: Vec<&str> = text.lines().filter(|l| l.contains('|')).collect();
        assert_eq!(rows.len(), 4, "{text}");
        assert!(rows[0].trim_start().starts_with("Y210"), "{text}");
        assert!(
            rows[0].contains("+0.200") && rows[0].ends_with("back"),
            "{text}"
        );
        assert!(
            rows[2].trim_start().starts_with("Y10") && rows[2].ends_with("front"),
            "{text}"
        );
        assert!(rows[1].contains("---"), "{text}");
        assert!(text.contains("X10") && text.contains("X210"), "{text}");
        // The highest point gets the densest shade and the hottest tone.
        assert!(rows[0].contains("|@@"), "{text}");
        let hottest = lines
            .iter()
            .skip(3)
            .flat_map(|l| &l.pieces)
            .find(|p| p.text.contains("+0.200"))
            .map(|p| p.tone);
        assert_eq!(hottest, Some(Tone::Heat(1.0)));
        assert_eq!(
            joined(&mesh_lines(&BedMesh::default())),
            "The mesh has no points."
        );
        // A flat mesh does not divide by zero.
        let flat = BedMesh::from_rows(vec![vec![0.0, 0.0]], "mbl");
        assert!(joined(&mesh_lines(&flat)).contains("range 0.000 mm"));
    }

    #[test]
    fn heat_colors_run_blue_to_red() {
        assert_eq!(heat_rgb(0.0), [59, 110, 230]);
        assert_eq!(heat_rgb(1.0), [235, 80, 60]);
        assert_eq!(heat_rgb(f32::NAN), heat_rgb(0.5));
        assert_eq!(heat_rgb(7.0), heat_rgb(1.0));
        assert_eq!(shade(0.0), ' ');
        assert_eq!(shade(1.0), '@');
    }

    #[test]
    fn firmware_and_capabilities() {
        let text = joined(&firmware_lines(&connected_state()));
        assert!(text.starts_with("Firmware    Marlin 2.1.2.1"), "{text}");
        assert!(
            text.contains("Machine     Sprint Virtual Printer"),
            "{text}"
        );
        assert!(text.contains("Capabilities (1 of 2 enabled)"), "{text}");
        assert!(text.contains("✓ AUTOREPORT_TEMP"), "{text}");
        assert!(text.contains("✗ EEPROM"), "{text}");
        assert!(joined(&firmware_lines(&PrinterState::default())).starts_with("No firmware"));
    }

    #[test]
    fn endstops_sd_ports_and_stats() {
        let text = joined(&endstop_lines(&[
            ("x_min".into(), false),
            ("z_probe".into(), true),
        ]));
        assert_eq!(text, "x_min    open\nz_probe  TRIGGERED");
        assert_eq!(joined(&endstop_lines(&[])), "No endstop report yet.");

        let sd = SdState {
            card_present: Some(true),
            files: vec![
                SdFile {
                    name: "CUBE.GCO".into(),
                    long_name: Some("calibration-cube.gcode".into()),
                    size: Some(123_456),
                },
                SdFile {
                    name: "VASE.GCO".into(),
                    long_name: None,
                    size: None,
                },
            ],
            ..SdState::default()
        };
        let text = joined(&sd_lines(&sd));
        assert!(text.starts_with("2 files on the SD card"), "{text}");
        assert!(
            text.contains("CUBE.GCO") && text.contains("calibration-cube.gcode"),
            "{text}"
        );
        let no_card = SdState {
            card_present: Some(false),
            ..SdState::default()
        };
        assert_eq!(joined(&sd_lines(&no_card)), "No SD card inserted.");

        let ports = vec![
            PortInfo {
                name: "COM3".into(),
                description: "USB-SERIAL CH340".into(),
                vid: Some(0x1A86),
                pid: Some(0x7523),
                manufacturer: None,
                serial_number: None,
                likely_printer: true,
            },
            PortInfo {
                name: "COM1".into(),
                description: "Communications Port".into(),
                vid: None,
                pid: None,
                manufacturer: None,
                serial_number: None,
                likely_printer: false,
            },
        ];
        let text = joined(&ports_lines(&ports));
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].starts_with("PORT  DESCRIPTION"), "{text}");
        assert!(rows[1].starts_with("COM3  USB-SERIAL CH340") && rows[1].contains("1A86:7523"));
        assert!(rows[1].ends_with("● likely printer"), "{text}");
        assert!(rows[2].contains("--") && !rows[2].contains('★'), "{text}");
        assert!(joined(&ports_lines(&[])).starts_with("No serial ports found."));

        let stats = StatsReport {
            prints: Some(12),
            finished: Some(10),
            failed: Some(2),
            total_time: Some(3600 * 30),
            longest_job: Some(3600 * 5),
            filament_used_mm: Some(123_456.0),
        };
        let text = joined(&stats_lines(Some(&stats), &LinkStats::default()));
        assert!(text.contains("Prints        12"), "{text}");
        assert!(text.contains("Total time    1d 6h 00m"), "{text}");
        assert!(text.contains("Round trip"), "{text}");
        assert!(joined(&stats_lines(None, &LinkStats::default())).contains("not reported"));
    }

    #[test]
    fn help_lists_every_command() {
        let text = joined(&help_lines());
        for spec in COMMANDS {
            assert!(text.contains(spec.usage), "{} missing", spec.name);
        }
        for group in Group::ALL {
            assert!(text.contains(group.title()));
        }
        let spec = crate::commands::command_spec("temp").expect("temp");
        let text = joined(&command_help(spec));
        assert!(
            text.starts_with("usage: temp <celsius|off> [tool]"),
            "{text}"
        );
        assert!(text.contains("also: settemp"), "{text}");
        let doc = sprint_host::sprint_proto::gcode_doc("M502").expect("M502");
        let text = joined(&gcode_help(doc));
        assert!(text.starts_with("M502"), "{text}");
        assert!(text.contains("use with care"), "{text}");
    }

    #[test]
    fn macros_presets_and_monitor() {
        let macros = vec![Macro {
            name: "Bed mesh".into(),
            gcode: "G28\nG29\nM420 V".into(),
            color: None,
            confirm: true,
        }];
        assert_eq!(
            joined(&macro_lines(&macros)),
            "  Bed mesh  G28 | G29 | M420 V  (asks first)"
        );
        assert!(joined(&macro_lines(&[])).starts_with("No macros"));
        let presets = vec![TempPreset {
            name: "PLA".into(),
            hotend: 205.0,
            bed: 60.0,
        }];
        assert_eq!(
            joined(&preset_lines(&presets)),
            "  PLA  hotend 205 °C, bed 60 °C"
        );
        assert_eq!(
            monitor_text(&connected_state()),
            "Printing 42.0%  layer 12/100  ETA 20:00  T0 205/210  B 60/60  Z 0.20"
        );
        assert_eq!(monitor_text(&PrinterState::default()), "Disconnected");
    }
}
