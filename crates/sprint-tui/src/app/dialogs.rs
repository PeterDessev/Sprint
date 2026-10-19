//! The modal dialogs of the TUI: connect, open file, temperatures,
//! confirmation, reports and the printer's host prompts.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    BAUD_RATES, ConnectTarget, DEFAULT_BAUD, Heater, HostPrompt, PortInfo, PrinterProfile,
    TempPreset, list_ports,
};

use crate::format;
use crate::paths::{PathCandidate, common_completion, complete_path, resolve};
use crate::report::{ReportLine, tone_style};
use crate::theme::Theme;
use crate::widgets::{CommandInput, CommandLine, InputEvent, KeyHints};

/// What a dialog asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// Connect (asking first while a job runs).
    Connect(ConnectTarget),
    /// Disconnect (asking first while a job runs).
    Disconnect,
    /// Connect, stopping a running job (already confirmed).
    ConnectAnyway(ConnectTarget),
    /// Disconnect, stopping a running job (already confirmed).
    DisconnectAnyway,
    Open(PathBuf),
    /// Set heater targets (asking first when that turns a heater off
    /// while a job runs).
    SetTemps(Vec<(Heater, f32)>),
    /// Set heater targets (already confirmed).
    SetTempsAnyway(Vec<(Heater, f32)>),
    /// Print the loaded file, which must still be the one at this path
    /// (the question named it).
    StartPrint(PathBuf),
    CancelJob,
    Quit,
    EepromReset,
    Send(String),
    RunMacro(String),
    SdPrint(String),
    SdDelete(String),
    AnswerPrompt(usize),
}

/// The result of a key in a dialog.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    /// Stay open.
    Keep,
    /// Close without doing anything.
    Close,
    /// Close and run this.
    Run(Action),
    /// Stay open and show this error.
    Error(String),
}

/// An input line for dialogs: no completion popup.
pub(crate) fn plain_input() -> CommandInput {
    CommandInput::new().with_completer(|_| Vec::new())
}

/// Draw a centered dialog frame of (at most) `width` x `height` and return
/// its inner area.
pub(crate) fn frame_dialog(
    frame: &mut Frame,
    theme: &Theme,
    title: &str,
    width: u16,
    height: u16,
    danger: bool,
) -> Rect {
    let screen = frame.area();
    let w = width.min(screen.width);
    let h = height.min(screen.height);
    let area = Rect::new(
        screen.x + (screen.width - w) / 2,
        screen.y + (screen.height - h) / 2,
        w,
        h,
    );
    frame.render_widget(Clear, area);
    let (border, title_style) = if danger {
        (theme.error(), theme.error())
    } else {
        (theme.border(true), theme.title())
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .style(theme.surface())
        .title(Line::from(Span::styled(format!(" {title} "), title_style)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

/// Key hints on the last row of a dialog; returns the rows above it.
fn hints_row(frame: &mut Frame, theme: &Theme, inner: Rect, hints: &[(&str, &str)]) -> Rect {
    if inner.height < 2 {
        return inner;
    }
    let [body, footer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
    frame.render_widget(KeyHints::new(hints.iter().copied()).theme(theme), footer);
    body
}

/// A row with a selection marker.
/// The highlight spans `width` columns.
fn list_row(selected: bool, theme: &Theme, spans: Vec<Span<'static>>, width: u16) -> Line<'static> {
    if selected {
        // One style for the whole highlighted row: dim or colored pieces
        // would be unreadable on the highlight.
        let style = theme.selected();
        let mut all = vec![Span::styled("▸ ", style)];
        all.extend(spans.into_iter().map(|s| Span::styled(s.content, style)));
        let used: usize = all.iter().map(Span::width).sum();
        let pad = usize::from(width).saturating_sub(used);
        if pad > 0 {
            all.push(Span::styled(" ".repeat(pad), style));
        }
        Line::from(all).style(style)
    } else {
        let mut all = vec![Span::styled("  ", theme.accent())];
        all.extend(spans);
        Line::from(all)
    }
}

// ----- connect -----

/// One choice in the connect dialog.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConnectItem {
    Disconnect,
    Simulator,
    Port(PortInfo),
    Tcp,
}

/// F2: pick the virtual printer, a serial port (with baud rate) or a TCP
/// address.
pub(crate) struct ConnectDialog {
    pub items: Vec<ConnectItem>,
    pub selected: usize,
    /// Index into [`baud_choices`].
    pub baud: usize,
    pub tcp: CommandInput,
    sim: SimConfig,
}

/// Baud rates offered: auto-detect, then the usual rates.
pub(crate) fn baud_choices() -> Vec<u32> {
    std::iter::once(0)
        .chain(BAUD_RATES.iter().copied())
        .collect()
}

fn baud_label(baud: u32) -> String {
    if baud == 0 {
        "auto".to_string()
    } else {
        baud.to_string()
    }
}

impl ConnectDialog {
    /// A dialog listing `ports`; `open` adds a Disconnect entry.
    pub(crate) fn new(ports: Vec<PortInfo>, open: bool, profile: &PrinterProfile) -> ConnectDialog {
        let mut items = Vec::new();
        if open {
            items.push(ConnectItem::Disconnect);
        }
        items.push(ConnectItem::Simulator);
        items.extend(ports.into_iter().map(ConnectItem::Port));
        items.push(ConnectItem::Tcp);
        let (baud, tcp_text, sim) = match &profile.connection {
            ConnectTarget::Serial { baud, .. } => (*baud, String::new(), SimConfig::default()),
            ConnectTarget::Tcp { host, port } => {
                (DEFAULT_BAUD, format!("{host}:{port}"), SimConfig::default())
            }
            ConnectTarget::Simulator(config) => (DEFAULT_BAUD, String::new(), config.clone()),
        };
        let baud = baud_choices().iter().position(|b| *b == baud).unwrap_or(0);
        let mut tcp = plain_input();
        tcp.set_text(if tcp_text.is_empty() {
            "localhost:5000"
        } else {
            &tcp_text
        });
        // Preselect the profile's port, else the first likely printer, else
        // the virtual printer.
        let selected = items
            .iter()
            .position(|i| match (i, &profile.connection) {
                (ConnectItem::Port(p), ConnectTarget::Serial { port, .. }) => p.name == *port,
                _ => false,
            })
            .or_else(|| {
                items
                    .iter()
                    .position(|i| matches!(i, ConnectItem::Port(p) if p.likely_printer))
            })
            .or_else(|| items.iter().position(|i| *i == ConnectItem::Simulator))
            .unwrap_or(0);
        ConnectDialog {
            items,
            selected,
            baud,
            tcp,
            sim,
        }
    }

    /// Re-enumerate the serial ports (never opens them).
    fn refresh(&mut self) {
        let current = self.items.get(self.selected).cloned();
        let open = self.items.first() == Some(&ConnectItem::Disconnect);
        let mut items = Vec::new();
        if open {
            items.push(ConnectItem::Disconnect);
        }
        items.push(ConnectItem::Simulator);
        items.extend(list_ports().into_iter().map(ConnectItem::Port));
        items.push(ConnectItem::Tcp);
        self.selected = current
            .and_then(|c| items.iter().position(|i| *i == c))
            .unwrap_or(0);
        self.items = items;
    }

    fn selected_item(&self) -> Option<&ConnectItem> {
        self.items.get(self.selected)
    }

    /// Pasted text is a network address: the TCP entry gets it (and the
    /// selection, so the edit is seen).
    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(tcp) = self.items.iter().position(|i| *i == ConnectItem::Tcp) {
            self.selected = tcp;
        }
        self.tcp.insert_str(text);
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        let on_tcp = self.selected_item() == Some(&ConnectItem::Tcp);
        let count = self.items.len().max(1);
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Up | KeyCode::BackTab => self.selected = (self.selected + count - 1) % count,
            KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1) % count,
            KeyCode::F(2) | KeyCode::F(5) => self.refresh(),
            KeyCode::Left if !on_tcp => {
                let n = baud_choices().len();
                self.baud = (self.baud + n - 1) % n;
            }
            KeyCode::Right if !on_tcp => self.baud = (self.baud + 1) % baud_choices().len(),
            KeyCode::Enter => return self.confirm(),
            _ if on_tcp => {
                self.tcp.handle_key(key);
            }
            _ => {}
        }
        Outcome::Keep
    }

    fn confirm(&self) -> Outcome {
        match self.selected_item() {
            Some(ConnectItem::Disconnect) => Outcome::Run(Action::Disconnect),
            Some(ConnectItem::Simulator) => {
                Outcome::Run(Action::Connect(ConnectTarget::Simulator(self.sim.clone())))
            }
            Some(ConnectItem::Port(p)) => Outcome::Run(Action::Connect(ConnectTarget::Serial {
                port: p.name.clone(),
                baud: baud_choices().get(self.baud).copied().unwrap_or(0),
            })),
            Some(ConnectItem::Tcp) => {
                let text = self.tcp.text().trim();
                let full = if text.to_ascii_lowercase().starts_with("tcp://") {
                    text.to_string()
                } else {
                    format!("tcp://{text}")
                };
                match ConnectTarget::parse(&full) {
                    Ok(target @ ConnectTarget::Tcp { .. }) => Outcome::Run(Action::Connect(target)),
                    Ok(_) => Outcome::Error(format!("'{text}' is not host:port")),
                    Err(e) => Outcome::Error(e.to_string()),
                }
            }
            None => Outcome::Close,
        }
    }

    pub(crate) fn draw(&self, frame: &mut Frame, theme: &Theme) {
        let rows = u16::try_from(self.items.len()).unwrap_or(u16::MAX);
        let inner = frame_dialog(frame, theme, "Connect", 72, rows.saturating_add(8), false);
        let body = hints_row(
            frame,
            theme,
            inner,
            &[
                ("Enter", "Connect"),
                ("↑↓", "Choose"),
                ("←→", "Baud"),
                ("F2", "Rescan ports"),
                ("Esc", "Close"),
            ],
        );
        let width = usize::from(body.width);
        let mut lines: Vec<Line> = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            let selected = i == self.selected;
            let spans = match item {
                ConnectItem::Disconnect => vec![
                    Span::styled("Disconnect", theme.warning()),
                    Span::styled("   close the current connection", theme.dim()),
                ],
                ConnectItem::Simulator => vec![
                    Span::styled(format!("{:<12}", "Virtual"), theme.text()),
                    Span::styled("built-in simulated Marlin printer", theme.dim()),
                ],
                ConnectItem::Port(p) => {
                    let ids = match (p.vid, p.pid) {
                        (Some(v), Some(pid)) => format!("  {v:04X}:{pid:04X}"),
                        _ => String::new(),
                    };
                    let room = width.saturating_sub(16 + ids.len() + 12);
                    let desc: String = p.description.chars().take(room).collect();
                    let mut spans = vec![
                        Span::styled(format!("{:<12}", p.name), theme.text()),
                        Span::styled(desc, theme.dim()),
                        Span::styled(ids, theme.dim()),
                    ];
                    if p.likely_printer {
                        spans.push(Span::styled("  ● printer", theme.success()));
                    }
                    spans
                }
                ConnectItem::Tcp => vec![
                    Span::styled(format!("{:<12}", "Network"), theme.text()),
                    Span::styled(format!("tcp://{}", self.tcp.text()), theme.dim()),
                ],
            };
            lines.push(list_row(selected, theme, spans, body.width));
        }
        if !self.items.iter().any(|i| matches!(i, ConnectItem::Port(_))) {
            lines.push(Line::from(Span::styled(
                "  No serial ports found (F2 rescans).",
                theme.dim(),
            )));
        }
        lines.push(Line::default());
        let baud = baud_choices().get(self.baud).copied().unwrap_or(0);
        let serial = matches!(self.selected_item(), Some(ConnectItem::Port(_)));
        let baud_style = if serial { theme.accent() } else { theme.dim() };
        lines.push(Line::from(vec![
            Span::styled("  Baud rate  ", theme.dim()),
            Span::styled(format!("◂ {} ▸", baud_label(baud)), baud_style),
            Span::styled("   for serial ports", theme.dim()),
        ]));
        let [list, edit] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(body);
        frame.render_widget(Paragraph::new(lines).style(theme.surface()), list);
        if self.selected_item() == Some(&ConnectItem::Tcp) {
            let line = CommandLine::new(&self.tcp)
                .prompt("  Address  tcp://")
                .theme(theme);
            if let Some(pos) = line.cursor_position(edit) {
                frame.set_cursor_position(pos);
            }
            frame.render_widget(line, edit);
        }
    }
}

// ----- open file -----

/// An entry of the open dialog's list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OpenEntry {
    Path(PathCandidate),
    Recent(PathBuf),
}

/// F3: type a path (Tab completes), or pick a match or a recent file.
pub(crate) struct OpenDialog {
    pub input: CommandInput,
    pub entries: Vec<OpenEntry>,
    pub selected: Option<usize>,
    base: PathBuf,
    recent: Vec<PathBuf>,
}

impl OpenDialog {
    pub(crate) fn new(base: PathBuf, recent: Vec<PathBuf>) -> OpenDialog {
        let mut dialog = OpenDialog {
            input: plain_input(),
            entries: Vec::new(),
            selected: None,
            base,
            recent,
        };
        dialog.refresh();
        dialog
    }

    /// Recompute the list for the current input.
    fn refresh(&mut self) {
        let text = self.input.text();
        self.entries = if text.is_empty() && !self.recent.is_empty() {
            self.recent.iter().cloned().map(OpenEntry::Recent).collect()
        } else {
            complete_path(text, &self.base, true)
                .into_iter()
                .map(OpenEntry::Path)
                .collect()
        };
        self.selected = None;
    }

    /// Tab: extend the input to the matches' common prefix.
    fn complete(&mut self) {
        let candidates = complete_path(self.input.text(), &self.base, true);
        if let Some(common) = common_completion(&candidates)
            && common.len() > self.input.text().len()
        {
            self.input.set_text(&common);
        } else if candidates.len() > 1 {
            // Nothing to add: cycle through the matches.
            self.refresh();
            self.move_selection(1);
            return;
        }
        self.refresh();
    }

    fn move_selection(&mut self, delta: isize) {
        let n = self.entries.len();
        if n == 0 {
            self.selected = None;
            return;
        }
        let next = match self.selected {
            None if delta > 0 => 0,
            None => n - 1,
            Some(i) => (i as isize + delta).rem_euclid(n as isize) as usize,
        };
        self.selected = Some(next);
    }

    /// Pasted text goes into the path; the list follows it (and a choice
    /// made before no longer stands).
    pub(crate) fn paste(&mut self, text: &str) {
        self.input.insert_str(text);
        self.refresh();
    }

    fn path_of(&self, entry: &OpenEntry) -> PathBuf {
        match entry {
            OpenEntry::Recent(path) => path.clone(),
            OpenEntry::Path(c) => resolve(&c.completion, &self.base),
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Tab => self.complete(),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down | KeyCode::BackTab => self.move_selection(1),
            KeyCode::Enter => return self.confirm(),
            _ => {
                if matches!(self.input.handle_key(key), InputEvent::Handled) {
                    self.refresh();
                }
            }
        }
        Outcome::Keep
    }

    fn confirm(&mut self) -> Outcome {
        if let Some(entry) = self.selected.and_then(|i| self.entries.get(i)).cloned() {
            if let OpenEntry::Path(c) = &entry
                && c.is_dir
            {
                self.input.set_text(&c.completion);
                self.refresh();
                return Outcome::Keep;
            }
            return Outcome::Run(Action::Open(self.path_of(&entry)));
        }
        let text = self.input.text().trim().trim_matches('"').to_string();
        if text.is_empty() {
            return Outcome::Error("Type a file name (Tab completes)".to_string());
        }
        let path = resolve(&text, &self.base);
        if path.is_dir() {
            let mut shown = text;
            if !shown.ends_with(['/', std::path::MAIN_SEPARATOR]) {
                shown.push(std::path::MAIN_SEPARATOR);
            }
            self.input.set_text(&shown);
            self.refresh();
            return Outcome::Keep;
        }
        Outcome::Run(Action::Open(path))
    }

    pub(crate) fn draw(&self, frame: &mut Frame, theme: &Theme) {
        let inner = frame_dialog(frame, theme, "Open G-code file", 76, 20, false);
        let body = hints_row(
            frame,
            theme,
            inner,
            &[
                ("Enter", "Open"),
                ("Tab", "Complete"),
                ("↑↓", "Choose"),
                ("Esc", "Close"),
            ],
        );
        let [edit, rule, list] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(body);
        let line = CommandLine::new(&self.input)
            .prompt(" File  ")
            .placeholder("path to a .gcode file")
            .theme(theme);
        if let Some(pos) = line.cursor_position(edit) {
            frame.set_cursor_position(pos);
        }
        frame.render_widget(line, edit);
        let heading = if self.input.is_empty() && !self.recent.is_empty() {
            " Recent files".to_string()
        } else {
            format!(" In {}", display_dir(self.input.text(), &self.base))
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(heading, theme.dim()))),
            rule,
        );
        let rows = usize::from(list.height);
        let first = self
            .selected
            .map_or(0, |s| s.saturating_sub(rows.saturating_sub(1)));
        let width = usize::from(list.width);
        let mut lines: Vec<Line> = Vec::new();
        for (i, entry) in self.entries.iter().enumerate().skip(first).take(rows) {
            let selected = self.selected == Some(i);
            let spans = match entry {
                OpenEntry::Recent(path) => vec![Span::styled(
                    shorten(&path.display().to_string(), width.saturating_sub(4)),
                    theme.text(),
                )],
                OpenEntry::Path(c) => {
                    let size = c.size.map(format::bytes).unwrap_or_default();
                    let room = width.saturating_sub(4 + 10);
                    let name = shorten(&c.name, room);
                    let pad = room.saturating_sub(name.chars().count());
                    vec![
                        Span::styled(
                            name,
                            if c.is_dir {
                                theme.accent()
                            } else {
                                theme.text()
                            },
                        ),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(format!("{size:>10}"), theme.dim()),
                    ]
                }
            };
            lines.push(list_row(selected, theme, spans, list.width));
        }
        if self.entries.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No G-code files or folders match.",
                theme.dim(),
            )));
        }
        frame.render_widget(Paragraph::new(lines).style(theme.surface()), list);
    }
}

/// The directory a partial path lists, for the heading.
fn display_dir(input: &str, base: &Path) -> String {
    let dir = match input.rfind(['/', '\\']) {
        Some(i) => resolve(&input[..=i], base),
        None => base.to_path_buf(),
    };
    shorten(&dir.display().to_string(), 60)
}

/// `text` cut to `max` characters, keeping its end behind a `…`.
fn shorten(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let tail: String = text.chars().skip(count - (max - 1)).collect();
    format!("…{tail}")
}

// ----- temperatures -----

/// One heater row of the temperature dialog.
pub(crate) struct TempField {
    pub heater: Heater,
    pub input: CommandInput,
    pub current: f32,
}

/// `t`: set targets per heater, or apply a preset.
pub(crate) struct TempDialog {
    pub fields: Vec<TempField>,
    pub presets: Vec<TempPreset>,
    /// Row: fields, then presets, then "All off".
    pub selected: usize,
}

impl TempDialog {
    /// One field per heater with its (current, target) reading.
    pub(crate) fn new(heaters: Vec<(Heater, f32, f32)>, presets: Vec<TempPreset>) -> TempDialog {
        let fields = heaters
            .into_iter()
            .map(|(heater, current, target)| {
                let mut input = plain_input();
                if target.is_finite() && target > 0.0 {
                    input.set_text(&format::trimmed(f64::from(target), 1));
                }
                TempField {
                    heater,
                    input,
                    current,
                }
            })
            .collect();
        TempDialog {
            fields,
            presets,
            selected: 0,
        }
    }

    fn rows(&self) -> usize {
        self.fields.len() + self.presets.len() + 1
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        let rows = self.rows();
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Up | KeyCode::BackTab => self.selected = (self.selected + rows - 1) % rows,
            KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1) % rows,
            KeyCode::Enter => return self.confirm(),
            _ => {
                if let Some(field) = self.fields.get_mut(self.selected) {
                    // Only digits and a decimal point are typed; editing
                    // keys (Ctrl+...) still work.
                    let rejected = matches!(key.code, KeyCode::Char(c) if !(c.is_ascii_digit() || c == '.'))
                        && !key.modifiers.contains(KeyModifiers::CONTROL);
                    if !rejected {
                        field.input.handle_key(key);
                    }
                }
            }
        }
        Outcome::Keep
    }

    fn confirm(&self) -> Outcome {
        let preset_index = self.selected.checked_sub(self.fields.len());
        match preset_index {
            None => {
                let mut targets = Vec::new();
                for field in &self.fields {
                    let text = field.input.text().trim();
                    let value = if text.is_empty() {
                        0.0
                    } else {
                        match text.parse::<f32>() {
                            Ok(v) if v.is_finite() && (0.0..=500.0).contains(&v) => v,
                            _ => {
                                return Outcome::Error(format!(
                                    "{}: '{text}' is not a temperature (0 to 500 °C)",
                                    field.heater.name()
                                ));
                            }
                        }
                    };
                    targets.push((field.heater, value));
                }
                Outcome::Run(Action::SetTemps(targets))
            }
            Some(i) => match self.presets.get(i) {
                Some(preset) => {
                    let targets = self
                        .fields
                        .iter()
                        .filter_map(|f| match f.heater {
                            Heater::Hotend(_) => Some((f.heater, preset.hotend)),
                            Heater::Bed => Some((f.heater, preset.bed)),
                            Heater::Chamber => None,
                        })
                        .collect();
                    Outcome::Run(Action::SetTemps(targets))
                }
                None => Outcome::Run(Action::SetTemps(
                    self.fields.iter().map(|f| (f.heater, 0.0)).collect(),
                )),
            },
        }
    }

    pub(crate) fn draw(&self, frame: &mut Frame, theme: &Theme) {
        let rows = u16::try_from(self.rows()).unwrap_or(u16::MAX);
        let inner = frame_dialog(
            frame,
            theme,
            "Temperatures",
            52,
            rows.saturating_add(6),
            false,
        );
        let body = hints_row(
            frame,
            theme,
            inner,
            &[("Enter", "Apply"), ("↑↓", "Choose"), ("Esc", "Close")],
        );
        let mut y = body.y;
        let line_at = |y: u16| Rect::new(body.x, y, body.width, 1);
        for (i, field) in self.fields.iter().enumerate() {
            if y >= body.bottom() {
                return;
            }
            let selected = self.selected == i;
            let label = match field.heater {
                Heater::Hotend(t)
                    if self
                        .fields
                        .iter()
                        .filter(|f| matches!(f.heater, Heater::Hotend(_)))
                        .count()
                        > 1 =>
                {
                    format!("Hotend T{t}")
                }
                Heater::Hotend(_) => "Hotend".to_string(),
                other => other.name(),
            };
            let row = line_at(y);
            let marker = if selected { "▸ " } else { "  " };
            let prefix = format!("{marker}{label:<11}");
            let now = format!("now {}", format::temperature(field.current));
            let [left, edit, right] = Layout::horizontal([
                Constraint::Length(u16::try_from(prefix.chars().count()).unwrap_or(13)),
                Constraint::Length(12),
                Constraint::Fill(1),
            ])
            .areas(row);
            let style = if selected {
                theme.accent()
            } else {
                theme.text()
            };
            frame.render_widget(Paragraph::new(Span::styled(prefix, style)), left);
            let line = CommandLine::new(&field.input)
                .prompt("[")
                .placeholder("off")
                .focused(selected)
                .theme(theme);
            if selected && let Some(pos) = line.cursor_position(edit) {
                frame.set_cursor_position(pos);
            }
            frame.render_widget(line, edit);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("] °C   ", theme.dim()),
                    Span::styled(now, theme.dim()),
                ])),
                right,
            );
            y += 1;
        }
        y += 1;
        if y < body.bottom() {
            frame.render_widget(
                Paragraph::new(Span::styled("  Presets", theme.dim())),
                line_at(y),
            );
            y += 1;
        }
        let first_preset = self.fields.len();
        for (i, preset) in self.presets.iter().enumerate() {
            if y >= body.bottom() {
                return;
            }
            let selected = self.selected == first_preset + i;
            let spans = vec![
                Span::styled(format!("{:<9}", preset.name), theme.text()),
                Span::styled(
                    format!(
                        "hotend {} °C, bed {} °C",
                        format::trimmed(f64::from(preset.hotend), 1),
                        format::trimmed(f64::from(preset.bed), 1)
                    ),
                    theme.dim(),
                ),
            ];
            frame.render_widget(
                Paragraph::new(list_row(selected, theme, spans, body.width)),
                line_at(y),
            );
            y += 1;
        }
        if y < body.bottom() {
            let selected = self.selected == self.rows() - 1;
            let spans = vec![
                Span::styled(format!("{:<9}", "All off"), theme.text()),
                Span::styled("every heater off", theme.dim()),
            ];
            frame.render_widget(
                Paragraph::new(list_row(selected, theme, spans, body.width)),
                line_at(y),
            );
        }
    }
}

// ----- confirmation -----

/// A yes/no question guarding an action.
pub(crate) struct Confirm {
    pub title: String,
    pub message: Vec<String>,
    pub yes: String,
    pub no: String,
    pub danger: bool,
    pub action: Action,
    /// The highlighted button is "yes".
    pub on_yes: bool,
}

impl Confirm {
    /// A question; dangerous ones start on the safe answer.
    pub(crate) fn new(
        title: impl Into<String>,
        message: Vec<String>,
        yes: impl Into<String>,
        no: impl Into<String>,
        danger: bool,
        action: Action,
    ) -> Confirm {
        Confirm {
            title: title.into(),
            message,
            yes: yes.into(),
            no: no.into(),
            danger,
            action,
            on_yes: !danger,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n' | 'N') => Outcome::Close,
            KeyCode::Char('y' | 'Y') => Outcome::Run(self.action.clone()),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                self.on_yes = !self.on_yes;
                Outcome::Keep
            }
            KeyCode::Enter if self.on_yes => Outcome::Run(self.action.clone()),
            KeyCode::Enter => Outcome::Close,
            _ => Outcome::Keep,
        }
    }

    pub(crate) fn draw(&self, frame: &mut Frame, theme: &Theme) {
        let width = self
            .message
            .iter()
            .map(|m| m.chars().count())
            .max()
            .unwrap_or(0)
            .max(self.title.chars().count() + 4)
            .clamp(36, 70);
        let height = u16::try_from(self.message.len())
            .unwrap_or(10)
            .saturating_add(5);
        let inner = frame_dialog(
            frame,
            theme,
            &self.title,
            u16::try_from(width + 4).unwrap_or(74),
            height,
            self.danger,
        );
        let [text, _, buttons] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        let lines: Vec<Line> = self
            .message
            .iter()
            .map(|m| Line::from(Span::styled(format!(" {m}"), theme.text())))
            .collect();
        frame.render_widget(Paragraph::new(lines), text);
        let button = |label: &str, active: bool, danger: bool| {
            let style = match (active, danger) {
                (true, true) => Style::default()
                    .fg(theme.background_or_black())
                    .bg(theme.error)
                    .add_modifier(ratatui::style::Modifier::BOLD),
                (true, false) => theme.selected(),
                (false, _) => theme.dim(),
            };
            Span::styled(format!("  {label}  "), style)
        };
        let row = Line::from(vec![
            button(&self.yes, self.on_yes, self.danger),
            Span::raw("   "),
            button(&self.no, !self.on_yes, false),
            Span::styled("     y / n", theme.dim()),
        ])
        .centered();
        frame.render_widget(Paragraph::new(row), buttons);
    }
}

// ----- reports -----

/// The printer state a report shows; the report is redrawn from it when
/// the printer sends news.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ReportSource {
    /// `settings [filter]`.
    Settings(Option<String>),
    Firmware,
    Endstops,
    Mesh,
    Stats,
    SdFiles,
}

/// A report waiting for the printer's answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pending {
    /// When the request was sent.
    pub since: Instant,
    /// Give up after this long.
    pub timeout: Duration,
    /// The answer may carry no news (no mesh, no SD card): an idle link
    /// after the request also ends the wait.
    pub idle_completes: bool,
    /// Since when the link has been idle.
    pub idle_since: Option<Instant>,
}

impl Pending {
    pub(crate) fn new(timeout: Duration, idle_completes: bool) -> Pending {
        Pending {
            since: Instant::now(),
            timeout,
            idle_completes,
            idle_since: None,
        }
    }
}

/// A scrollable text report (status, settings, mesh, ...), possibly still
/// waiting for the printer's answer.
pub(crate) struct ReportView {
    pub title: String,
    pub lines: Vec<ReportLine>,
    pub scroll: usize,
    /// Where the lines come from, for live updates.
    pub source: Option<ReportSource>,
    pub pending: Option<Pending>,
}

impl ReportView {
    pub(crate) fn new(title: impl Into<String>, lines: Vec<ReportLine>) -> ReportView {
        ReportView {
            title: title.into(),
            lines,
            scroll: 0,
            source: None,
            pending: None,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll += 1,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll += 10,
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = usize::MAX,
            _ => {}
        }
        Outcome::Keep
    }

    pub(crate) fn draw(&mut self, frame: &mut Frame, theme: &Theme) {
        let width = self
            .lines
            .iter()
            .map(|l| unicode_width::UnicodeWidthStr::width(l.text().as_str()))
            .max()
            .unwrap_or(0)
            .max(self.title.chars().count() + 6)
            .clamp(40, 110);
        let wanted = self.lines.len() + 3 + usize::from(self.pending.is_some());
        let inner = frame_dialog(
            frame,
            theme,
            &self.title,
            u16::try_from(width + 4).unwrap_or(114),
            u16::try_from(wanted + 2).unwrap_or(u16::MAX),
            false,
        );
        let body = hints_row(
            frame,
            theme,
            inner,
            &[("↑↓ PgUp PgDn", "Scroll"), ("Esc", "Close")],
        );
        let rows = usize::from(body.height);
        let mut shown: Vec<Line> = Vec::new();
        if self.pending.is_some() {
            shown.push(Line::from(Span::styled(
                " Waiting for the printer…",
                theme.warning(),
            )));
        }
        let room = rows.saturating_sub(shown.len());
        let max_scroll = self.lines.len().saturating_sub(room);
        self.scroll = self.scroll.min(max_scroll);
        for line in self.lines.iter().skip(self.scroll).take(room) {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(
                line.pieces
                    .iter()
                    .map(|p| Span::styled(p.text.clone(), tone_style(theme, p.tone))),
            );
            shown.push(Line::from(spans));
        }
        frame.render_widget(Paragraph::new(shown).style(theme.surface()), body);
        if max_scroll > 0 && body.width > 12 {
            let note = format!(
                " {}/{} ",
                self.scroll + room.min(self.lines.len()),
                self.lines.len()
            );
            let x = body
                .right()
                .saturating_sub(u16::try_from(note.len()).unwrap_or(0));
            frame.buffer_mut().set_string(
                x,
                body.bottom().saturating_sub(1).max(body.y),
                note,
                theme.dim(),
            );
        }
    }
}

// ----- host prompt -----

/// Keys pressed this soon after a host prompt appeared were meant for what
/// was on screen before (a G-code line being typed): they do not answer or
/// hide it. An accidental "Continue" during a filament change would resume
/// the print before the filament is loaded.
pub(crate) const PROMPT_GRACE: Duration = Duration::from_millis(800);

/// A question from the firmware (`//action:prompt`, M600 filament change).
pub(crate) struct PromptDialog {
    pub prompt: HostPrompt,
    pub selected: usize,
    /// When the dialog appeared.
    pub shown: Instant,
}

impl PromptDialog {
    /// A dialog for `prompt`, appearing now.
    pub(crate) fn new(prompt: HostPrompt) -> PromptDialog {
        PromptDialog {
            prompt,
            selected: 0,
            shown: Instant::now(),
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        let n = self.prompt.buttons.len();
        let settled = self.shown.elapsed() >= PROMPT_GRACE;
        if !settled && matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char(_)) {
            return Outcome::Keep;
        }
        match key.code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Left | KeyCode::BackTab if n > 0 => {
                self.selected = (self.selected + n - 1) % n;
                Outcome::Keep
            }
            KeyCode::Right | KeyCode::Tab if n > 0 => {
                self.selected = (self.selected + 1) % n;
                Outcome::Keep
            }
            KeyCode::Char(c) => match c.to_digit(10).and_then(|d| usize::try_from(d).ok()) {
                Some(d) if d >= 1 && d <= n => Outcome::Run(Action::AnswerPrompt(d - 1)),
                _ => Outcome::Keep,
            },
            // A prompt without buttons is answered with "continue" (0).
            KeyCode::Enter => {
                Outcome::Run(Action::AnswerPrompt(self.selected.min(n.saturating_sub(1))))
            }
            _ => Outcome::Keep,
        }
    }

    pub(crate) fn draw(&self, frame: &mut Frame, theme: &Theme) {
        let message = format!(" {}", self.prompt.message);
        let width = message.chars().count().clamp(40, 70);
        let inner = frame_dialog(
            frame,
            theme,
            "The printer asks",
            u16::try_from(width + 4).unwrap_or(74),
            7,
            false,
        );
        let [text, _, buttons] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        frame.render_widget(Paragraph::new(Span::styled(message, theme.text())), text);
        let mut spans = Vec::new();
        let labels: Vec<&str> = if self.prompt.buttons.is_empty() {
            vec!["Continue"]
        } else {
            self.prompt.buttons.iter().map(String::as_str).collect()
        };
        for (i, label) in labels.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let style = if i == self.selected {
                theme.selected()
            } else {
                theme.dim()
            };
            spans.push(Span::styled(format!(" {} {label} ", i + 1), style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans).centered()), buttons);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn port(name: &str, likely: bool) -> PortInfo {
        PortInfo {
            name: name.into(),
            description: "USB serial".into(),
            vid: Some(0x1A86),
            pid: Some(0x7523),
            manufacturer: None,
            serial_number: None,
            likely_printer: likely,
        }
    }

    #[test]
    fn connect_dialog_picks_targets() {
        let profile = PrinterProfile::default();
        let mut d = ConnectDialog::new(
            vec![port("COM1", false), port("COM7", true)],
            false,
            &profile,
        );
        // The likely printer is preselected.
        assert_eq!(
            d.selected_item(),
            Some(&ConnectItem::Port(port("COM7", true)))
        );
        // Baud cycles; auto is first.
        let start = d.baud;
        d.handle_key(key(KeyCode::Right));
        assert_eq!(d.baud, (start + 1) % baud_choices().len());
        d.baud = 0;
        assert_eq!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::Connect(ConnectTarget::Serial {
                port: "COM7".into(),
                baud: 0
            }))
        );
        d.selected = 0;
        assert_eq!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::Connect(ConnectTarget::Simulator(
                SimConfig::default()
            )))
        );
        // TCP: typing edits the address.
        d.selected = d.items.len() - 1;
        d.tcp.clear();
        for c in "127.0.0.1:2323".chars() {
            d.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::Connect(ConnectTarget::Tcp {
                host: "127.0.0.1".into(),
                port: 2323
            }))
        );
        d.tcp.set_text("nonsense");
        assert!(matches!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Error(_)
        ));
        assert_eq!(d.handle_key(key(KeyCode::Esc)), Outcome::Close);
        // With a connection open, Disconnect comes first.
        let d = ConnectDialog::new(Vec::new(), true, &profile);
        assert_eq!(d.items[0], ConnectItem::Disconnect);
        assert_eq!(d.selected_item(), Some(&ConnectItem::Simulator));
    }

    #[test]
    fn temp_dialog_applies_fields_presets_and_off() {
        let presets = PrinterProfile::default().temp_presets;
        let mut d = TempDialog::new(
            vec![(Heater::Hotend(0), 25.0, 0.0), (Heater::Bed, 24.0, 60.0)],
            presets,
        );
        assert_eq!(d.fields[1].input.text(), "60");
        for c in "215x".chars() {
            d.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(d.fields[0].input.text(), "215", "letters are ignored");
        assert_eq!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::SetTemps(vec![
                (Heater::Hotend(0), 215.0),
                (Heater::Bed, 60.0)
            ]))
        );
        d.fields[0].input.set_text("999");
        assert!(matches!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Error(_)
        ));
        // Third row: the first preset (PLA).
        d.handle_key(key(KeyCode::Down));
        d.handle_key(key(KeyCode::Down));
        assert_eq!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::SetTemps(vec![
                (Heater::Hotend(0), 205.0),
                (Heater::Bed, 60.0)
            ]))
        );
        // Last row: all off.
        d.handle_key(key(KeyCode::Up));
        d.handle_key(key(KeyCode::Up));
        d.handle_key(key(KeyCode::Up));
        assert_eq!(d.selected, d.rows() - 1);
        assert_eq!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::SetTemps(vec![
                (Heater::Hotend(0), 0.0),
                (Heater::Bed, 0.0)
            ]))
        );
    }

    #[test]
    fn confirm_defaults_to_the_safe_answer_when_dangerous() {
        let mut c = Confirm::new(
            "Cancel",
            vec![],
            "Cancel print",
            "Keep",
            true,
            Action::CancelJob,
        );
        assert_eq!(c.handle_key(key(KeyCode::Enter)), Outcome::Close);
        c.handle_key(key(KeyCode::Left));
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::CancelJob)
        );
        let mut c = Confirm::new(
            "Print",
            vec![],
            "Print",
            "Not now",
            false,
            Action::StartPrint(PathBuf::from("a.gcode")),
        );
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::StartPrint(PathBuf::from("a.gcode")))
        );
        assert_eq!(
            c.handle_key(key(KeyCode::Char('y'))),
            Outcome::Run(Action::StartPrint(PathBuf::from("a.gcode")))
        );
        assert_eq!(c.handle_key(key(KeyCode::Char('n'))), Outcome::Close);
    }

    #[test]
    fn prompt_buttons_answer_by_index() {
        let mut p = PromptDialog::new(HostPrompt {
            message: "Filament runout".into(),
            buttons: vec!["Continue".into(), "Purge more".into()],
        });
        // Keys typed as it appears do not answer it.
        assert_eq!(p.handle_key(key(KeyCode::Char('1'))), Outcome::Keep);
        assert_eq!(p.handle_key(key(KeyCode::Enter)), Outcome::Keep);
        assert_eq!(p.handle_key(key(KeyCode::Esc)), Outcome::Keep);
        p.shown -= PROMPT_GRACE;
        p.handle_key(key(KeyCode::Right));
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::AnswerPrompt(1))
        );
        assert_eq!(
            p.handle_key(key(KeyCode::Char('1'))),
            Outcome::Run(Action::AnswerPrompt(0))
        );
        assert_eq!(p.handle_key(key(KeyCode::Char('9'))), Outcome::Keep);
        let mut bare = PromptDialog::new(HostPrompt {
            message: "Paused".into(),
            buttons: vec![],
        });
        bare.shown -= PROMPT_GRACE;
        assert_eq!(
            bare.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::AnswerPrompt(0))
        );
    }

    #[test]
    fn pasting_replaces_the_choice() {
        let samples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples");
        let arcs = samples.join("arcs-test.gcode");
        let cube = samples.join("calibration-cube.gcode");
        let mut d = OpenDialog::new(samples.clone(), vec![arcs]);
        d.handle_key(key(KeyCode::Down));
        assert_eq!(d.selected, Some(0));
        d.paste(&cube.display().to_string());
        assert_eq!(d.selected, None);
        assert!(
            d.entries
                .iter()
                .any(|e| matches!(e, OpenEntry::Path(c) if c.name == "calibration-cube.gcode")),
            "the list follows the pasted path"
        );
        assert_eq!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::Open(cube))
        );
        // An address pasted into the connect dialog lands in (and selects)
        // the TCP entry.
        let mut c = ConnectDialog::new(Vec::new(), false, &PrinterProfile::default());
        c.tcp.clear();
        c.paste("127.0.0.1:2323");
        assert_eq!(c.selected_item(), Some(&ConnectItem::Tcp));
        assert_eq!(
            c.handle_key(key(KeyCode::Enter)),
            Outcome::Run(Action::Connect(ConnectTarget::Tcp {
                host: "127.0.0.1".into(),
                port: 2323
            }))
        );
    }

    #[test]
    fn shorten_keeps_the_end() {
        assert_eq!(shorten("abcdef", 10), "abcdef");
        assert_eq!(shorten("abcdef", 4), "…def");
        assert_eq!(shorten("abcdef", 0), "");
    }
}
