//! The console: everything sent to and received from the printer, colored
//! by kind, with filters, search, timestamps, autoscroll, copy / clear /
//! export, and a command line with history, G-code autocomplete,
//! multi-line paste, quick commands and confirmation for dangerous
//! commands.
//!
//! The panel keeps its own bounded copy of the log, fetched incrementally
//! with [`Printer::console_since`] (see [`ConsolePanel::sync`]), and an
//! index of the entries that pass the filters, so drawing touches only the
//! visible rows.

use std::collections::VecDeque;
use std::path::PathBuf;

use crossbeam_channel::Receiver;
use egui::text::{CCursor, CCursorRange};
use egui::{Align, Align2, Color32, Id, Key, Layout, Margin, Modifiers, RichText, Sense, Ui, vec2};
use sprint_host::{ConsoleEntry, ConsoleKind, Printer};
use sprint_proto::GcodeDoc;

use crate::context::AppCtx;
use crate::prefs::{ConsolePrefs, HISTORY_LEN};
use crate::theme::{self, Palette, TextRole};
use crate::widgets::{Chip, ConfirmDialog, DialogResult, IconButton};
use crate::{format, icons, native_dialog};

/// Commands offered as one-click chips.
pub const QUICK_COMMANDS: &[&str] = &["M105", "M114", "M115", "M503", "M119", "G28"];
/// Most autocomplete suggestions shown.
const MAX_SUGGESTIONS: usize = 8;
/// Entries fetched from the printer per call.
const FETCH_BATCH: usize = 4096;

/// Which entries are shown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConsoleFilter {
    /// Hide automatic traffic.
    pub hide_auto: bool,
    /// Hide bare `ok` replies.
    pub hide_ok: bool,
    /// Only entries containing this text (case-insensitive); empty = all.
    pub search: String,
}

/// `ok`, optionally with `N`/`P`/`B` fields (ADVANCED_OK), and nothing else.
pub fn is_bare_ok(text: &str) -> bool {
    let mut words = text.split_whitespace();
    if !words.next().is_some_and(|w| w.eq_ignore_ascii_case("ok")) {
        return false;
    }
    words.all(|w| {
        let mut c = w.chars();
        matches!(c.next(), Some('N' | 'P' | 'B'))
            && c.clone().count() > 0
            && c.all(|d| d.is_ascii_digit())
    })
}

/// Whether `entry` passes `filter`. `search_lower` is the lowercased
/// search text.
pub fn is_visible(entry: &ConsoleEntry, filter: &ConsoleFilter, search_lower: &str) -> bool {
    if filter.hide_auto && entry.auto {
        return false;
    }
    if filter.hide_ok && entry.kind == ConsoleKind::Received && is_bare_ok(&entry.text) {
        return false;
    }
    if !search_lower.is_empty() && !entry.text.to_lowercase().contains(search_lower) {
        return false;
    }
    true
}

/// A console timestamp: `"12:34.5"` within the first hour, `"1:02:03"`
/// after; seconds since the session started.
pub fn console_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return format::UNKNOWN.to_owned();
    }
    let tenths = (seconds * 10.0).floor() as u64;
    let total = tenths / 10;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}.{}", tenths % 10)
    }
}

/// One exported line: `"[12:34.5] > G28"`.
pub fn export_line(entry: &ConsoleEntry) -> String {
    let marker = match entry.kind {
        ConsoleKind::Sent => ">",
        ConsoleKind::Received => "<",
        ConsoleKind::Info => "-",
        ConsoleKind::Warning => "!",
        ConsoleKind::Error => "E",
    };
    format!("[{}] {marker} {}", console_time(entry.time), entry.text)
}

/// Autocomplete suggestions for what is typed: the first word of a
/// single-line command.
pub fn suggestions(input: &str) -> Vec<&'static GcodeDoc> {
    let token = input.trim_start();
    if token.is_empty() || token.contains(char::is_whitespace) || token.len() > 24 {
        return Vec::new();
    }
    sprint_proto::search_gcode(token)
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .collect()
}

/// The dangerous commands among `lines` (with their reference entries).
pub fn dangerous_commands(lines: &[String]) -> Vec<&'static GcodeDoc> {
    lines
        .iter()
        .filter_map(|l| sprint_proto::gcode_doc(l.trim()))
        .filter(|d| d.dangerous)
        .collect()
}

/// The commands in a submitted text: trimmed non-empty lines.
pub fn command_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[derive(Default)]
struct Completion {
    selected: usize,
    /// The user moved the selection (Enter then accepts it).
    navigated: bool,
    /// Suggestions are hidden for exactly this input.
    hidden_for: Option<String>,
}

/// The console panel.
pub struct ConsolePanel {
    entries: VecDeque<ConsoleEntry>,
    /// Absolute index of the entry after the last one.
    pushed: u64,
    last_seq: u64,
    capacity: usize,
    visible: VecDeque<u64>,
    filter: ConsoleFilter,
    search_lower: String,
    /// Persisted filters.
    pub prefs: ConsolePrefs,
    input: String,
    history: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
    completion: Completion,
    pending: Option<Vec<String>>,
    jump: bool,
    unseen: usize,
    at_bottom: bool,
    focus_input: bool,
    export: Option<Receiver<Result<Option<PathBuf>, String>>>,
}

impl Default for ConsolePanel {
    fn default() -> Self {
        ConsolePanel::new(
            sprint_host::DEFAULT_CONSOLE_LINES,
            ConsolePrefs::default(),
            Vec::new(),
        )
    }
}

impl ConsolePanel {
    /// A console keeping `capacity` lines with persisted filters and
    /// history.
    pub fn new(capacity: usize, prefs: ConsolePrefs, history: Vec<String>) -> ConsolePanel {
        let mut panel = ConsolePanel {
            entries: VecDeque::new(),
            pushed: 0,
            last_seq: 0,
            capacity: capacity.max(1),
            visible: VecDeque::new(),
            filter: ConsoleFilter::default(),
            search_lower: String::new(),
            prefs,
            input: String::new(),
            history,
            history_pos: None,
            draft: String::new(),
            completion: Completion::default(),
            pending: None,
            jump: false,
            unseen: 0,
            at_bottom: true,
            focus_input: false,
            export: None,
        };
        panel.apply_filter();
        panel
    }

    /// Commands typed so far, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Entries kept (all kinds).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is kept.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries that pass the filters, oldest first.
    pub fn visible_entries(&self) -> impl Iterator<Item = &ConsoleEntry> + '_ {
        self.visible.iter().filter_map(|abs| self.get(*abs))
    }

    /// The input line's text.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Replace the input line's text.
    pub fn set_input(&mut self, text: &str) {
        self.input = text.to_owned();
        self.history_pos = None;
    }

    /// Focus the input line on the next frame.
    pub fn focus_input(&mut self) {
        self.focus_input = true;
    }

    /// Set the search text.
    pub fn set_search(&mut self, text: &str) {
        self.filter.search = text.to_owned();
        self.apply_filter();
    }

    /// Keep at most `capacity` lines (drops the oldest).
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        self.trim();
    }

    fn get(&self, abs: u64) -> Option<&ConsoleEntry> {
        let front = self.pushed - self.entries.len() as u64;
        abs.checked_sub(front)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| self.entries.get(i))
    }

    /// Fetch what the printer logged since the last call.
    pub fn sync(&mut self, printer: &Printer) {
        loop {
            let batch = printer.console_since(self.last_seq, FETCH_BATCH);
            let n = batch.len();
            self.push_entries(batch);
            if n < FETCH_BATCH {
                break;
            }
        }
    }

    /// Append entries (in order; ones already seen are skipped).
    pub fn push_entries(&mut self, batch: Vec<ConsoleEntry>) {
        for entry in batch {
            if entry.seq <= self.last_seq {
                continue;
            }
            self.last_seq = entry.seq;
            let abs = self.pushed;
            self.pushed += 1;
            if is_visible(&entry, &self.filter, &self.search_lower) {
                self.visible.push_back(abs);
                if !self.at_bottom {
                    self.unseen += 1;
                }
            }
            self.entries.push_back(entry);
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
        let front = self.pushed - self.entries.len() as u64;
        while self.visible.front().is_some_and(|&a| a < front) {
            self.visible.pop_front();
        }
    }

    fn apply_filter(&mut self) {
        self.filter.hide_auto = self.prefs.hide_auto;
        self.filter.hide_ok = self.prefs.hide_ok;
        self.search_lower = self.filter.search.to_lowercase();
        let front = self.pushed - self.entries.len() as u64;
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| is_visible(e, &self.filter, &self.search_lower))
            .map(|(i, _)| front + i as u64)
            .collect();
    }

    /// Remove every line (here and in the printer's log).
    pub fn clear(&mut self, printer: &Printer) {
        printer.clear_console();
        self.entries.clear();
        self.visible.clear();
        self.unseen = 0;
    }

    /// Send `text` (one or more lines) as typed by the user: dangerous
    /// commands ask first. Returns true when it was sent right away.
    pub fn submit(&mut self, ctx: &mut AppCtx<'_>, text: &str) -> bool {
        let lines = command_lines(text);
        if lines.is_empty() {
            return false;
        }
        self.remember(text.trim());
        if !ctx.state.status.is_open() {
            ctx.toasts
                .warning("Not connected")
                .message("Connect to a printer to send commands.");
            return false;
        }
        if !dangerous_commands(&lines).is_empty() {
            self.pending = Some(lines);
            return false;
        }
        ctx.printer.send(&lines.join("\n"));
        self.jump = true;
        true
    }

    fn remember(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_owned());
        }
        if self.history.len() > HISTORY_LEN {
            let excess = self.history.len() - HISTORY_LEN;
            self.history.drain(..excess);
        }
        self.history_pos = None;
    }

    /// Step through the history (`older` = Up).
    fn history_step(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let pos = match (self.history_pos, older) {
            (None, true) => {
                self.draft = self.input.clone();
                Some(self.history.len() - 1)
            }
            (None, false) => return,
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => None,
        };
        self.history_pos = pos;
        self.input = match pos {
            Some(i) => self.history[i].clone(),
            None => std::mem::take(&mut self.draft),
        };
        // Recalled text is not being typed: no completion list for it, so
        // the arrows keep walking the history (a recalled "G28" is a known
        // code and would otherwise capture them). Typing brings it back.
        self.completion.hidden_for = Some(self.input.clone());
        self.completion.navigated = false;
    }

    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let p = theme::ui_palette(ui);
        self.poll_export(ctx);
        egui::Frame::new()
            .inner_margin(Margin {
                left: 12,
                right: 10,
                top: 10,
                bottom: 6,
            })
            .show(ui, |ui| {
                self.header(ui, ctx, &p);
            });
        let input_h = self.input_height(ui);
        let body = ui.available_rect_before_wrap();
        let log_rect =
            egui::Rect::from_min_max(body.min, egui::pos2(body.max.x, body.max.y - input_h));
        let input_rect = egui::Rect::from_min_max(egui::pos2(body.min.x, log_rect.max.y), body.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(log_rect), |ui| {
            self.log(ui, &p);
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(input_rect), |ui| {
            self.input_area(ui, ctx, &p);
        });
        ui.allocate_rect(body, Sense::hover());
        self.confirm_dangerous(ui.ctx(), ctx);
    }

    fn input_height(&self, ui: &Ui) -> f32 {
        let lines = self.input.lines().count().clamp(1, 5) as f32;
        let row = ui.text_style_height(&egui::TextStyle::Monospace);
        // chips + field + margins
        30.0 + lines * row + 34.0
    }

    fn header(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, p: &Palette) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::with_text(icons::CONSOLE, "Console"))
                    .font(theme::font(ui, TextRole::Title))
                    .color(p.text),
            );
            ui.label(
                RichText::new(format::count(self.visible.len() as u64))
                    .size(11.0)
                    .color(p.text_faint),
            )
            .on_hover_text(format!(
                "{} of {} lines shown",
                format::count(self.visible.len() as u64),
                format::count(self.entries.len() as u64)
            ));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if ui
                    .add(
                        IconButton::new(icons::CLEAR)
                            .ghost()
                            .small()
                            .tooltip("Clear the console"),
                    )
                    .clicked()
                {
                    self.clear(ctx.printer);
                }
                let exporting = self.export.is_some();
                if ui
                    .add_enabled(
                        !exporting && !self.visible.is_empty(),
                        IconButton::new(icons::DOWNLOAD)
                            .ghost()
                            .small()
                            .tooltip("Export the shown lines to a file…"),
                    )
                    .clicked()
                {
                    self.start_export(ui.ctx());
                }
                if ui
                    .add_enabled(
                        !self.visible.is_empty(),
                        IconButton::new(icons::COPY)
                            .ghost()
                            .small()
                            .tooltip("Copy the shown lines"),
                    )
                    .clicked()
                {
                    let text = self.export_text();
                    ui.ctx().copy_text(text);
                    ctx.toasts.info(format!(
                        "Copied {} lines",
                        format::count(self.visible.len() as u64)
                    ));
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Chips from the right; the search field takes the rest.
            let mut changed = false;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(Chip::new("Time").selected(self.prefs.timestamps))
                    .on_hover_text("Show timestamps")
                    .clicked()
                {
                    self.prefs.timestamps = !self.prefs.timestamps;
                }
                let show_ok = !self.prefs.hide_ok;
                if ui
                    .add(Chip::new("ok").selected(show_ok))
                    .on_hover_text("Show bare \"ok\" replies")
                    .clicked()
                {
                    self.prefs.hide_ok = show_ok;
                    changed = true;
                }
                let show_auto = !self.prefs.hide_auto;
                if ui
                    .add(Chip::new("Auto").selected(show_auto))
                    .on_hover_text("Show automatic temperature and position reports")
                    .clicked()
                {
                    self.prefs.hide_auto = show_auto;
                    changed = true;
                }
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.filter.search)
                        .id(Id::new("sprint-console-search"))
                        .hint_text(format!("{}  Filter", icons::FILTER))
                        .desired_width(f32::INFINITY),
                );
                if search.changed() {
                    changed = true;
                }
            });
            if changed {
                self.apply_filter();
            }
        });
    }

    fn log(&mut self, ui: &mut Ui, p: &Palette) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, p.bg);
        let font = theme::builtin_font(ui, egui::TextStyle::Monospace);
        let row_h = (ui.ctx().fonts_mut(|f| f.row_height(&font)) + 3.0).round();
        let n = self.visible.len();
        if n == 0 {
            ui.add_space(rect.height() * 0.3);
            ui.vertical_centered(|ui| {
                let text = if self.entries.is_empty() {
                    "Nothing here yet.\nConnect to a printer to see its traffic."
                } else {
                    "No lines match the filters."
                };
                ui.label(RichText::new(text).color(p.text_faint));
            });
            return;
        }
        let mut area = egui::ScrollArea::both()
            .id_salt("sprint-console-log")
            .auto_shrink([false, false])
            .stick_to_bottom(true);
        if self.jump {
            area = area.vertical_scroll_offset(n as f32 * row_h + rect.height());
            self.jump = false;
            self.unseen = 0;
        }
        let time_w = if self.prefs.timestamps {
            ui.ctx().fonts_mut(|f| f.glyph_width(&font, '0')) * 8.0 + 10.0
        } else {
            0.0
        };
        let timestamps = self.prefs.timestamps;
        let output = egui::Frame::new()
            .inner_margin(Margin::symmetric(10, 4))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                area.show_rows(ui, row_h, n, |ui, rows| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for i in rows {
                        let Some(entry) = self.visible.get(i).and_then(|&abs| self.get(abs)) else {
                            continue;
                        };
                        console_row(ui, entry, &font, row_h, time_w, timestamps, p);
                    }
                })
            })
            .inner;
        let max_offset = (output.content_size.y - output.inner_rect.height()).max(0.0);
        self.at_bottom = output.state.offset.y >= max_offset - row_h;
        if self.at_bottom {
            self.unseen = 0;
        } else {
            // "Jump to latest" pill.
            let label = if self.unseen > 0 {
                format!(
                    "{}  {} new",
                    icons::ARROW_DOWN,
                    format::count(self.unseen as u64)
                )
            } else {
                format!("{}  Latest", icons::ARROW_DOWN)
            };
            let pos = egui::pos2(rect.center().x, rect.bottom() - 14.0);
            let area = egui::Area::new(Id::new("sprint-console-jump"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .pivot(Align2::CENTER_BOTTOM)
                .constrain_to(rect);
            area.show(ui.ctx(), |ui| {
                let galley = ui.painter().layout_no_wrap(
                    label.clone(),
                    egui::FontId::proportional(13.0),
                    p.on_accent,
                );
                let size = galley.size() + vec2(24.0, 10.0);
                let (r, resp) = ui.allocate_exact_size(size, Sense::click());
                let fill = if resp.hovered() {
                    p.accent_hover
                } else {
                    p.accent
                };
                ui.painter()
                    .rect_filled(r, egui::CornerRadius::same(255), fill);
                ui.painter()
                    .galley(r.center() - galley.size() / 2.0, galley, p.on_accent);
                if resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.jump = true;
                }
            });
        }
    }

    fn input_area(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, p: &Palette) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, p.panel);
        ui.painter()
            .hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, p.border));
        let open = ctx.state.status.is_open();
        egui::Frame::new()
            .inner_margin(Margin {
                left: 10,
                right: 10,
                top: 6,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for &cmd in QUICK_COMMANDS {
                        let tip = sprint_proto::gcode_doc(cmd).map_or("", |d| d.name);
                        let chip = ui
                            .add_enabled(open, Chip::new(cmd))
                            .on_hover_text(tip)
                            .on_disabled_hover_text("Connect to a printer first");
                        if chip.clicked() {
                            self.submit(ctx, cmd);
                        }
                    }
                });
                ui.add_space(4.0);
                self.command_line(ui, ctx, p);
            });
    }

    fn command_line(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, p: &Palette) {
        let id = Id::new("sprint-console-input");
        let focused = ui.ctx().memory(|m| m.has_focus(id));
        let suggestions = if self.completion.hidden_for.as_deref() == Some(self.input.as_str()) {
            Vec::new()
        } else {
            suggestions(&self.input)
        };
        if self.completion.selected >= suggestions.len() {
            self.completion.selected = 0;
            self.completion.navigated = false;
        }
        let mut submit = false;
        let mut accept = None;
        if focused {
            ui.input_mut(|i| {
                if !suggestions.is_empty() {
                    if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                        self.completion.selected =
                            (self.completion.selected + 1) % suggestions.len();
                        self.completion.navigated = true;
                    }
                    if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                        self.completion.selected =
                            (self.completion.selected + suggestions.len() - 1) % suggestions.len();
                        self.completion.navigated = true;
                    }
                    if i.consume_key(Modifiers::NONE, Key::Tab) {
                        accept = Some(self.completion.selected);
                    }
                    if i.consume_key(Modifiers::NONE, Key::Escape) {
                        self.completion.hidden_for = Some(self.input.clone());
                    }
                }
                let single_line = !self.input.contains('\n');
                if accept.is_none() && i.consume_key(Modifiers::NONE, Key::Enter) {
                    if self.completion.navigated && !suggestions.is_empty() {
                        accept = Some(self.completion.selected);
                    } else {
                        submit = true;
                    }
                }
                if suggestions.is_empty() && single_line {
                    if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                        self.history_step(true);
                    }
                    if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                        self.history_step(false);
                    }
                }
            });
        }
        let mut field_rect = egui::Rect::NOTHING;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                let send = ui.add_enabled(
                    !self.input.trim().is_empty(),
                    IconButton::new(icons::SEND)
                        .primary()
                        .tooltip("Send (Enter)"),
                );
                if send.clicked() {
                    submit = true;
                }
                let edit = egui::TextEdit::multiline(&mut self.input)
                    .id(id)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(1)
                    .desired_width(f32::INFINITY)
                    .hint_text("Send G-code…")
                    .return_key(Some(egui::KeyboardShortcut::new(
                        Modifiers::SHIFT,
                        Key::Enter,
                    )));
                let response = ui.add(edit);
                field_rect = response.rect;
                if self.focus_input {
                    response.request_focus();
                    self.focus_input = false;
                }
                if response.changed() {
                    self.history_pos = None;
                    self.completion.navigated = false;
                    if self.completion.hidden_for.as_deref() != Some(self.input.as_str()) {
                        self.completion.hidden_for = None;
                    }
                }
            });
        });
        if let Some(index) = accept
            && let Some(doc) = suggestions.get(index)
        {
            self.input = format!("{} ", doc.code);
            self.completion.hidden_for = Some(self.input.clone());
            self.completion.navigated = false;
            set_cursor_end(ui.ctx(), id, &self.input);
        }
        if submit {
            let text = std::mem::take(&mut self.input);
            self.completion = Completion::default();
            if self.submit(ctx, &text) || self.pending.is_some() {
                // Sent (or waiting for confirmation): keep the field empty.
            } else if !ctx.state.status.is_open() {
                // Not sent: give the text back so nothing typed is lost.
                self.input = text;
            }
            ui.memory_mut(|m| m.request_focus(id));
        }
        // Suggestions popup above the field.
        if focused && !suggestions.is_empty() && field_rect.is_positive() && accept.is_none() {
            let clicked = suggestion_popup(
                ui.ctx(),
                field_rect,
                &suggestions,
                self.completion.selected,
                p,
            );
            if let Some(index) = clicked
                && let Some(doc) = suggestions.get(index)
            {
                self.input = format!("{} ", doc.code);
                self.completion.hidden_for = Some(self.input.clone());
                set_cursor_end(ui.ctx(), id, &self.input);
                ui.memory_mut(|m| m.request_focus(id));
            }
        }
    }

    fn confirm_dangerous(&mut self, egui_ctx: &egui::Context, ctx: &mut AppCtx<'_>) {
        let Some(lines) = &self.pending else {
            return;
        };
        let docs = dangerous_commands(lines);
        let message = docs
            .iter()
            .map(|d| format!("{} — {}: {}", d.code, d.name, d.description))
            .collect::<Vec<_>>()
            .join("\n");
        let details = lines.join("\n");
        let answer = ConfirmDialog::new("sprint-console-dangerous", "Send a dangerous command?")
            .message(&message)
            .details(&details)
            .confirm_label("Send anyway")
            .danger(true)
            .width(460.0)
            .show(egui_ctx);
        match answer {
            Some(DialogResult::Confirmed) => {
                if let Some(lines) = self.pending.take() {
                    ctx.printer.send(&lines.join("\n"));
                    self.jump = true;
                }
            }
            Some(DialogResult::Cancelled) => self.pending = None,
            None => {}
        }
    }

    /// True while the dangerous-command question is open.
    pub fn is_confirming(&self) -> bool {
        self.pending.is_some()
    }

    fn export_text(&self) -> String {
        let mut text = String::new();
        for entry in self.visible_entries() {
            text.push_str(&export_line(entry));
            text.push('\n');
        }
        text
    }

    fn start_export(&mut self, repaint: &egui::Context) {
        let text = self.export_text();
        self.export = Some(native_dialog::spawn(Some(repaint.clone()), move || {
            match native_dialog::save_text_file("Export console", "sprint-console.txt") {
                Some(path) => std::fs::write(&path, text)
                    .map(|()| Some(path))
                    .map_err(|e| e.to_string()),
                None => Ok(None),
            }
        }));
    }

    fn poll_export(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(rx) = &self.export else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(Some(path))) => {
                ctx.toasts
                    .success("Console exported")
                    .message(path.display().to_string());
                self.export = None;
            }
            Ok(Ok(None)) => self.export = None,
            Ok(Err(e)) => {
                ctx.toasts.error("Could not export the console").message(e);
                self.export = None;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => self.export = None,
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
    }
}

fn set_cursor_end(ctx: &egui::Context, id: Id, text: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let end = CCursor::new(text.chars().count());
        state.cursor.set_char_range(Some(CCursorRange::one(end)));
        state.store(ctx, id);
    }
}

/// The color of a console line.
fn kind_color(entry: &ConsoleEntry, p: &Palette) -> Color32 {
    match entry.kind {
        ConsoleKind::Sent => p.info,
        ConsoleKind::Received if entry.auto || is_bare_ok(&entry.text) => p.text_dim,
        ConsoleKind::Received => p.text,
        ConsoleKind::Info => p.text_faint,
        ConsoleKind::Warning => p.warning,
        ConsoleKind::Error => p.danger,
    }
}

fn console_row(
    ui: &mut Ui,
    entry: &ConsoleEntry,
    font: &egui::FontId,
    row_h: f32,
    time_w: f32,
    timestamps: bool,
    p: &Palette,
) {
    let text = format::sanitize(&entry.text, 2000);
    let marker = match entry.kind {
        ConsoleKind::Sent => "› ",
        ConsoleKind::Received => "  ",
        ConsoleKind::Info => "· ",
        ConsoleKind::Warning => "! ",
        ConsoleKind::Error => "✕ ",
    };
    let mut job = egui::text::LayoutJob::default();
    let fmt = |color: Color32| egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let color = kind_color(entry, p);
    job.append(marker, 0.0, fmt(theme::with_alpha(color, 0.8)));
    job.append(&text, 0.0, fmt(color));
    let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
    let width = (time_w + galley.size().x + 8.0).max(ui.available_width());
    let (rect, response) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand2(vec2(6.0, 0.0)),
            2.0,
            theme::with_alpha(p.text, 0.04),
        );
    }
    if timestamps {
        ui.painter().text(
            egui::pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            console_time(entry.time),
            font.clone(),
            p.text_faint,
        );
    }
    ui.painter().galley(
        egui::pos2(
            rect.left() + time_w,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
    response.context_menu(|ui| {
        if ui
            .button(icons::with_text(icons::COPY, "Copy line"))
            .clicked()
        {
            ui.ctx().copy_text(entry.text.clone());
            ui.close();
        }
        if let Some(line) = entry.line {
            ui.label(RichText::new(format!("Sent as N{line}")).size(12.0).weak());
        }
    });
}

/// Draw the autocomplete list above `field`; returns the clicked index.
fn suggestion_popup(
    ctx: &egui::Context,
    field: egui::Rect,
    items: &[&'static GcodeDoc],
    selected: usize,
    p: &Palette,
) -> Option<usize> {
    let mut clicked = None;
    egui::Area::new(Id::new("sprint-console-suggestions"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(field.left(), field.top() - 4.0))
        .pivot(Align2::LEFT_BOTTOM)
        .show(ctx, |ui| {
            egui::Frame::popup(&ctx.global_style())
                .inner_margin(Margin::same(4))
                .show(ui, |ui| {
                    ui.set_width(field.width().clamp(240.0, 420.0));
                    for (i, doc) in items.iter().enumerate() {
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
                        if i == selected {
                            ui.painter().rect_filled(rect, 4.0, p.accent_soft);
                        } else if resp.hovered() {
                            ui.painter()
                                .rect_filled(rect, 4.0, theme::with_alpha(p.text, 0.05));
                        }
                        let mono = egui::FontId::monospace(13.0);
                        ui.painter().text(
                            rect.left_top() + vec2(8.0, 5.0),
                            Align2::LEFT_TOP,
                            doc.code,
                            mono,
                            p.accent,
                        );
                        ui.painter().text(
                            rect.left_top() + vec2(70.0, 5.0),
                            Align2::LEFT_TOP,
                            format::sanitize(doc.name, 48),
                            egui::FontId::proportional(13.0),
                            p.text,
                        );
                        ui.painter().text(
                            rect.left_top() + vec2(8.0, 22.0),
                            Align2::LEFT_TOP,
                            format::sanitize(doc.usage, 60),
                            egui::FontId::monospace(11.0),
                            p.text_dim,
                        );
                        if resp.clicked() {
                            clicked = Some(i);
                        }
                    }
                });
        });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, kind: ConsoleKind, text: &str, auto: bool) -> ConsoleEntry {
        ConsoleEntry {
            seq,
            time: seq as f64,
            kind,
            text: text.into(),
            auto,
            line: None,
        }
    }

    #[test]
    fn bare_ok_detection() {
        assert!(is_bare_ok("ok"));
        assert!(is_bare_ok("ok N12 P15 B3"));
        assert!(is_bare_ok("OK"));
        assert!(!is_bare_ok("ok T:20.0 /0.0"));
        assert!(!is_bare_ok("okay"));
        assert!(!is_bare_ok("ok N"));
        assert!(!is_bare_ok(""));
    }

    #[test]
    fn filtering() {
        let f = ConsoleFilter {
            hide_auto: true,
            hide_ok: true,
            search: String::new(),
        };
        assert!(!is_visible(
            &entry(1, ConsoleKind::Received, "T:20", true),
            &f,
            ""
        ));
        assert!(!is_visible(
            &entry(2, ConsoleKind::Received, "ok", false),
            &f,
            ""
        ));
        assert!(
            is_visible(&entry(3, ConsoleKind::Sent, "ok", false), &f, ""),
            "only received oks"
        );
        assert!(is_visible(
            &entry(4, ConsoleKind::Received, "echo:busy", false),
            &f,
            ""
        ));
        assert!(is_visible(
            &entry(5, ConsoleKind::Sent, "G28 X", false),
            &f,
            "g28"
        ));
        assert!(!is_visible(
            &entry(6, ConsoleKind::Sent, "M105", false),
            &f,
            "g28"
        ));
    }

    #[test]
    fn ring_trims_and_filter_index_follows() {
        let mut c = ConsolePanel::new(5, ConsolePrefs::default(), Vec::new());
        let batch: Vec<_> = (1..=8)
            .map(|i| entry(i, ConsoleKind::Received, &format!("line {i}"), i % 2 == 0))
            .collect();
        c.push_entries(batch);
        assert_eq!(c.len(), 5);
        // hide_auto is on by default: odd lines only.
        let shown: Vec<_> = c.visible_entries().map(|e| e.seq).collect();
        assert_eq!(shown, vec![5, 7]);
        // Duplicates (already seen) are skipped.
        c.push_entries(vec![entry(8, ConsoleKind::Received, "dup", false)]);
        assert_eq!(c.len(), 5);
        c.prefs.hide_auto = false;
        c.apply_filter();
        let shown: Vec<_> = c.visible_entries().map(|e| e.seq).collect();
        assert_eq!(shown, vec![4, 5, 6, 7, 8]);
        c.set_search("LINE 6");
        let shown: Vec<_> = c.visible_entries().map(|e| e.seq).collect();
        assert_eq!(shown, vec![6]);
        c.set_search("");
        c.set_capacity(2);
        let shown: Vec<_> = c.visible_entries().map(|e| e.seq).collect();
        assert_eq!(shown, vec![7, 8]);
    }

    #[test]
    fn history_navigation() {
        let mut c = ConsolePanel::default();
        c.remember("G28");
        c.remember("M105");
        c.remember("M105");
        assert_eq!(c.history(), &["G28", "M105"]);
        c.set_input("draft");
        c.history_step(true);
        assert_eq!(c.input(), "M105");
        c.history_step(true);
        assert_eq!(c.input(), "G28");
        c.history_step(true);
        assert_eq!(c.input(), "G28");
        c.history_step(false);
        assert_eq!(c.input(), "M105");
        c.history_step(false);
        assert_eq!(c.input(), "draft");
        c.history_step(false);
        assert_eq!(c.input(), "draft");
        for i in 0..(HISTORY_LEN + 10) {
            c.remember(&format!("M{i}"));
        }
        assert_eq!(c.history().len(), HISTORY_LEN);
    }

    #[test]
    fn helpers() {
        assert_eq!(console_time(0.0), "00:00.0");
        assert_eq!(console_time(75.36), "01:15.3");
        assert_eq!(console_time(3723.0), "1:02:03");
        assert_eq!(console_time(f64::NAN), format::UNKNOWN);
        assert_eq!(
            export_line(&entry(1, ConsoleKind::Sent, "G28", false)),
            "[00:01.0] > G28"
        );
        assert_eq!(command_lines(" G28 \n\n M105\r\n"), vec!["G28", "M105"]);
        let s = suggestions("M10");
        assert!(!s.is_empty() && s.len() <= MAX_SUGGESTIONS);
        assert!(s.iter().all(|d| d.code.starts_with("M10")));
        assert!(suggestions("").is_empty());
        assert!(suggestions("G1 X").is_empty());
        let danger = dangerous_commands(&["G28".into(), "M502".into(), "m112".into()]);
        let codes: Vec<_> = danger.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec!["M502", "M112"]);
    }
}
