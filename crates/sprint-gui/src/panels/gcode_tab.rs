//! The G-code tab: a virtualized, syntax-colored view of the whole file
//! with line numbers, print progress, the selection shared with the 3D
//! view, reference tooltips, find (Ctrl+F), go to line (Ctrl+G), a
//! context menu (copy, start print from here, send to the printer) and
//! editing in an external editor (the file reloads when saved there).
//!
//! Only the visible rows are laid out each frame
//! ([`egui::ScrollArea::show_rows`]), so multi-million-line files scroll as
//! smoothly as small ones. Searching scans the file's text buffer on a
//! worker thread for large files and reports results as they come.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Receiver;
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Color32, FontId, Key, Layout, Margin, Modifiers, Rect, RichText, Sense, Stroke, Ui, vec2,
};
use sprint_gcode::GcodeFile;

use crate::context::{AppAction, AppCtx, Tab};
use crate::safety::{self, Assessment};
use crate::theme::{self, Palette};
use crate::widgets::{Badge, ConfirmDialog, DialogResult, IconButton};
use crate::{format, icons};

/// Files below this size are searched on the UI thread (instant results).
const SYNC_SEARCH_BYTES: usize = 1 << 20;
/// Most matches collected for one query.
pub const MAX_MATCHES: usize = 1_000_000;
/// Longest line shown in full (longer lines are cut for drawing).
const MAX_DRAWN_CHARS: usize = 400;
/// Below this width the toolbar wraps and the status line is shorter.
const COMPACT_TOOLBAR_W: f32 = 600.0;

/// What a piece of a G-code line is, for coloring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// Leading whitespace and separators.
    Plain,
    /// `N123` line number.
    LineNumber,
    /// A G command (`G1`, `G29.1`).
    GCode,
    /// An M command.
    MCode,
    /// A T (tool) or other single-letter command.
    OtherCode,
    /// An extended (Klipper / RRF macro) command name.
    Extended,
    /// A parameter letter or `KEY=` name.
    Param,
    /// A parameter value.
    Value,
    /// Free text argument (M117 message, file name, quoted string).
    Text,
    /// `; comment` or `(comment)`.
    Comment,
    /// `*42` checksum.
    Checksum,
}

/// Commands whose argument is free text (as Marlin reads them).
fn takes_text(letter: u8, number: u32) -> bool {
    letter == b'M' && matches!(number, 16 | 23 | 28 | 30 | 32 | 33 | 117 | 118 | 810..=819)
}

/// Split a line into colored tokens. Ranges are byte ranges into `line`,
/// cover it completely, are in order and fall on char boundaries.
pub fn tokenize(line: &str) -> Vec<(Range<usize>, TokenKind)> {
    let b = line.as_bytes();
    let n = b.len();
    let mut out: Vec<(Range<usize>, TokenKind)> = Vec::new();
    let mut push = |range: Range<usize>, kind: TokenKind| {
        if range.start >= range.end {
            return;
        }
        if let Some((last, last_kind)) = out.last_mut()
            && *last_kind == kind
            && last.end == range.start
        {
            last.end = range.end;
            return;
        }
        out.push((range, kind));
    };
    let mut i = 0;
    let mut command_seen = false;
    let mut text_mode = false;
    // Only whitespace so far (an N word must come first).
    let mut at_start = true;
    while i < n {
        let c = b[i];
        if !c.is_ascii_whitespace() && !c.is_ascii_alphabetic() {
            at_start = false;
        }
        if c == b';' {
            push(i..n, TokenKind::Comment);
            break;
        }
        if c == b'(' && !text_mode {
            let end = b[i..]
                .iter()
                .position(|&x| x == b')')
                .map_or(n, |p| i + p + 1);
            push(i..end, TokenKind::Comment);
            i = end;
            continue;
        }
        if c.is_ascii_whitespace() {
            let start = i;
            while i < n && b[i].is_ascii_whitespace() {
                i += 1;
            }
            push(start..i, TokenKind::Plain);
            continue;
        }
        if text_mode {
            // Up to a comment (a ';' starts a comment even in a message).
            let end = b[i..].iter().position(|&x| x == b';').map_or(n, |p| i + p);
            // Trailing whitespace before the comment is plain.
            let mut text_end = end;
            while text_end > i && b[text_end - 1].is_ascii_whitespace() {
                text_end -= 1;
            }
            push(i..text_end, TokenKind::Text);
            push(text_end..end, TokenKind::Plain);
            i = end;
            continue;
        }
        if c == b'*' {
            let start = i;
            i += 1;
            while i < n && b[i].is_ascii_digit() {
                i += 1;
            }
            push(start..i, TokenKind::Checksum);
            continue;
        }
        if c == b'"' {
            let end = b[i + 1..]
                .iter()
                .position(|&x| x == b'"')
                .map_or(n, |p| i + 1 + p + 1);
            push(i..end, TokenKind::Text);
            i = end;
            continue;
        }
        if c == b'{' {
            let end = b[i..]
                .iter()
                .position(|&x| x == b'}')
                .map_or(n, |p| i + p + 1);
            push(i..end, TokenKind::Text);
            i = end;
            continue;
        }
        if c.is_ascii_alphabetic() {
            // A word: letters, then maybe a number.
            let start = i;
            let mut j = i + 1;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') && !b[j].is_ascii_digit()
            {
                j += 1;
            }
            let letters = j - start;
            let upper = c.to_ascii_uppercase();
            if letters == 1 {
                // Letter + number: N word, command or parameter.
                let num_start = j;
                let mut k = j;
                if k < n && matches!(b[k], b'-' | b'+') {
                    k += 1;
                }
                while k < n && (b[k].is_ascii_digit() || b[k] == b'.') {
                    k += 1;
                }
                let number = &line[num_start..k];
                if !command_seen && upper == b'N' && !number.is_empty() && at_start {
                    at_start = false;
                    push(start..k, TokenKind::LineNumber);
                    i = k;
                    continue;
                }
                at_start = false;
                if !command_seen && !number.is_empty() && matches!(upper, b'G' | b'M' | b'T' | b'D')
                {
                    command_seen = true;
                    let kind = match upper {
                        b'G' => TokenKind::GCode,
                        b'M' => TokenKind::MCode,
                        _ => TokenKind::OtherCode,
                    };
                    push(start..k, kind);
                    let whole = number.split('.').next().unwrap_or("");
                    if let Ok(num) = whole.parse::<u32>()
                        && takes_text(upper, num)
                    {
                        text_mode = true;
                    }
                    i = k;
                    continue;
                }
                push(start..j, TokenKind::Param);
                push(num_start..k, TokenKind::Value);
                i = k;
                continue;
            }
            // A longer word: an extended command name or KEY=value.
            at_start = false;
            let mut k = j;
            while k < n && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                k += 1;
            }
            if k < n && b[k] == b'=' {
                push(start..k + 1, TokenKind::Param);
                let value_end = b[k + 1..]
                    .iter()
                    .position(|x| x.is_ascii_whitespace() || *x == b';')
                    .map_or(n, |p| k + 1 + p);
                push(k + 1..value_end, TokenKind::Value);
                i = value_end;
                continue;
            }
            if !command_seen {
                command_seen = true;
                push(start..k, TokenKind::Extended);
            } else {
                push(start..k, TokenKind::Text);
            }
            i = k;
            continue;
        }
        // Anything else (numbers without a letter, punctuation, UTF-8).
        let start = i;
        let mut k = i + 1;
        while k < n && !line.is_char_boundary(k) {
            k += 1;
        }
        push(start..k, TokenKind::Value);
        i = k;
    }
    out
}

/// The color of a token kind.
pub fn token_color(kind: TokenKind, p: &Palette) -> Color32 {
    match kind {
        TokenKind::Plain => p.text,
        TokenKind::LineNumber | TokenKind::Checksum => p.text_faint,
        TokenKind::GCode => p.accent,
        TokenKind::MCode => p.info,
        TokenKind::OtherCode | TokenKind::Extended => p.plot_color(3),
        TokenKind::Param => p.plot_color(6),
        TokenKind::Value => p.text,
        TokenKind::Text => p.warning,
        TokenKind::Comment => p.text_faint,
    }
}

/// Byte ranges of `needle` in `hay` (ASCII case-insensitive unless
/// `case_sensitive`), non-overlapping.
pub fn match_ranges(hay: &str, needle: &str, case_sensitive: bool) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if needle.is_empty() || needle.len() > hay.len() {
        return out;
    }
    let h = hay.as_bytes();
    let nb = needle.as_bytes();
    let mut i = 0;
    while i + nb.len() <= h.len() {
        let hit = if case_sensitive {
            &h[i..i + nb.len()] == nb
        } else {
            h[i..i + nb.len()].eq_ignore_ascii_case(nb)
        };
        if hit && hay.is_char_boundary(i) && hay.is_char_boundary(i + nb.len()) {
            out.push(i..i + nb.len());
            i += nb.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Lines of `file` containing `query`, in order, at most `limit`; `None`
/// when cancelled. `report` gets partial results as the scan advances.
pub fn find_lines(
    file: &GcodeFile,
    query: &str,
    case_sensitive: bool,
    cancel: &AtomicBool,
    limit: usize,
    report: &mut dyn FnMut(&[u32]),
) -> Option<(Vec<u32>, bool)> {
    let mut lines: Vec<u32> = Vec::new();
    if query.is_empty() || query.contains(['\n', '\r']) {
        return Some((lines, false));
    }
    let text = file.text().as_bytes();
    let needle = query.as_bytes();
    let first = needle[0];
    let (lo, hi) = if case_sensitive {
        (first, first)
    } else {
        (first.to_ascii_lowercase(), first.to_ascii_uppercase())
    };
    let mut i = 0usize;
    let mut reported = 0usize;
    let mut next_check = 0usize;
    while i + needle.len() <= text.len() {
        if i >= next_check {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            if lines.len() > reported {
                report(&lines[reported..]);
                reported = lines.len();
            }
            next_check = i + (4 << 20);
        }
        // Find the next candidate first byte.
        let Some(offset) = text[i..text.len() - needle.len() + 1]
            .iter()
            .position(|&c| c == lo || c == hi)
        else {
            break;
        };
        let at = i + offset;
        let window = &text[at..at + needle.len()];
        let hit = if case_sensitive {
            window == needle
        } else {
            window.eq_ignore_ascii_case(needle)
        };
        if hit {
            let line = file.line_at_offset(at) as u32;
            if lines.last() != Some(&line) {
                lines.push(line);
                if lines.len() >= limit {
                    report(&lines[reported..]);
                    return Some((lines, true));
                }
            }
            // Continue at the start of the next line.
            let next = file.byte_offset(line as usize + 1);
            i = next.max(at + 1);
        } else {
            i = at + 1;
        }
    }
    if lines.len() > reported {
        report(&lines[reported..]);
    }
    Some((lines, false))
}

/// Strip an `N` word so the reference lookup sees the command.
fn command_part(line: &str) -> &str {
    let t = line.trim_start();
    let b = t.as_bytes();
    if b.first().is_some_and(|c| c.eq_ignore_ascii_case(&b'N'))
        && b.get(1).is_some_and(u8::is_ascii_digit)
    {
        let end = b
            .iter()
            .position(|c| c.is_ascii_whitespace())
            .unwrap_or(b.len());
        return t[end..].trim_start();
    }
    t
}

enum SearchMsg {
    Partial(Vec<u32>),
    Done(bool),
}

/// A running or finished search.
struct Search {
    query: String,
    case_sensitive: bool,
    file: usize,
    matches: Vec<u32>,
    truncated: bool,
    done: bool,
    rx: Option<Receiver<SearchMsg>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The find bar's state.
#[derive(Default)]
pub struct FindBar {
    /// The bar is shown.
    pub open: bool,
    /// The query.
    pub query: String,
    /// Match case.
    pub case_sensitive: bool,
    focus: bool,
    search: Option<Search>,
    current: Option<usize>,
}

impl FindBar {
    /// Matching lines found so far.
    pub fn matches(&self) -> &[u32] {
        self.search.as_ref().map_or(&[], |s| s.matches.as_slice())
    }

    /// True while a background search runs.
    pub fn searching(&self) -> bool {
        self.search.as_ref().is_some_and(|s| !s.done)
    }

    /// Restart the search when the query, case or file changed.
    fn update(&mut self, file: &Arc<GcodeFile>, repaint: &egui::Context) {
        let key = Arc::as_ptr(file) as usize;
        if let Some(s) = &mut self.search {
            if let Some(rx) = &s.rx {
                for msg in rx.try_iter() {
                    match msg {
                        SearchMsg::Partial(lines) => s.matches.extend(lines),
                        SearchMsg::Done(truncated) => {
                            s.truncated = truncated;
                            s.done = true;
                        }
                    }
                }
                if s.done {
                    s.rx = None;
                }
            }
            if s.query == self.query && s.case_sensitive == self.case_sensitive && s.file == key {
                return;
            }
        }
        self.current = None;
        if self.query.is_empty() {
            self.search = None;
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let mut search = Search {
            query: self.query.clone(),
            case_sensitive: self.case_sensitive,
            file: key,
            matches: Vec::new(),
            truncated: false,
            done: false,
            rx: None,
            cancel: Arc::clone(&cancel),
        };
        if file.byte_len() < SYNC_SEARCH_BYTES {
            if let Some((lines, truncated)) = find_lines(
                file,
                &self.query,
                self.case_sensitive,
                &cancel,
                MAX_MATCHES,
                &mut |_| {},
            ) {
                search.matches = lines;
                search.truncated = truncated;
            }
            search.done = true;
        } else {
            let (tx, rx) = crossbeam_channel::unbounded();
            let file = Arc::clone(file);
            let query = self.query.clone();
            let case = self.case_sensitive;
            let ctx = repaint.clone();
            let spawned = std::thread::Builder::new()
                .name("sprint-gui-find".to_owned())
                .spawn(move || {
                    let result =
                        find_lines(&file, &query, case, &cancel, MAX_MATCHES, &mut |part| {
                            let _ = tx.send(SearchMsg::Partial(part.to_vec()));
                            ctx.request_repaint();
                        });
                    if let Some((_, truncated)) = result {
                        let _ = tx.send(SearchMsg::Done(truncated));
                    }
                    ctx.request_repaint();
                });
            if spawned.is_ok() {
                search.rx = Some(rx);
            } else {
                search.done = true;
            }
        }
        self.search = Some(search);
    }

    /// The match to go to from `from` (the selected line) in `direction`
    /// (+1 next, -1 previous), wrapping around.
    fn step(&mut self, from: Option<u32>, forward: bool) -> Option<u32> {
        let index = {
            let matches = self.matches();
            if matches.is_empty() {
                return None;
            }
            let n = matches.len();
            match (self.current, from) {
                (Some(c), Some(line)) if matches.get(c) == Some(&line) => {
                    if forward {
                        (c + 1) % n
                    } else {
                        (c + n - 1) % n
                    }
                }
                (_, Some(line)) => {
                    if forward {
                        matches.partition_point(|&m| m <= line) % n
                    } else {
                        (matches.partition_point(|&m| m < line) + n - 1) % n
                    }
                }
                (_, None) => {
                    if forward {
                        0
                    } else {
                        n - 1
                    }
                }
            }
        };
        self.current = Some(index);
        self.matches().get(index).copied()
    }
}

/// Go-to-line field state.
struct Goto {
    text: String,
    focus: bool,
    error: bool,
}

/// Parse a "go to line" entry (1-based) into a 0-based line, clamped to
/// the file.
pub fn parse_goto(text: &str, lines: usize) -> Option<u32> {
    let n: u64 = text.trim().replace([',', '_', ' '], "").parse().ok()?;
    if lines == 0 || n == 0 {
        return None;
    }
    Some((n.min(lines as u64) - 1) as u32)
}

/// The G-code tab.
pub struct GcodeTab {
    find: FindBar,
    goto: Option<Goto>,
    scroll_to: Option<u32>,
    file_key: usize,
    follow_print: bool,
    last_followed: Option<usize>,
    hovered_line: Option<u32>,
    /// A line waiting for "Send anyway", with why it asks and what the
    /// job was doing.
    confirm_send: Option<(String, Assessment, sprint_proto::JobContext)>,
}

impl Default for GcodeTab {
    fn default() -> Self {
        GcodeTab {
            find: FindBar::default(),
            goto: None,
            scroll_to: None,
            file_key: 0,
            follow_print: true,
            last_followed: None,
            hovered_line: None,
            confirm_send: None,
        }
    }
}

impl GcodeTab {
    /// Scroll to a 0-based line on the next frame.
    pub fn scroll_to(&mut self, line: u32) {
        self.scroll_to = Some(line);
    }

    /// Open the find bar and focus its field.
    pub fn open_find(&mut self) {
        self.find.open = true;
        self.find.focus = true;
        self.goto = None;
    }

    /// Open the "go to line" field.
    pub fn open_goto(&mut self) {
        self.goto = Some(Goto {
            text: String::new(),
            focus: true,
            error: false,
        });
    }

    /// The find bar (queries and results).
    pub fn find(&self) -> &FindBar {
        &self.find
    }

    /// True while the "go to line" field is shown.
    pub fn is_goto_open(&self) -> bool {
        self.goto.is_some()
    }

    /// Set the search query (tests and scripting).
    pub fn set_query(&mut self, query: &str) {
        self.find.open = true;
        self.find.query = query.to_owned();
    }

    /// The source line under the pointer.
    pub fn hovered_line(&self) -> Option<u32> {
        self.hovered_line
    }

    /// Draw the tab.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let Some(loaded) = ctx.file else {
            ui.add_space(ui.available_height() * 0.25);
            let r = crate::widgets::EmptyState::new(icons::CODE, "No G-code loaded")
                .message("Open a file to read it here: line numbers, syntax colors and the G-code reference on hover.")
                .action("Open G-code…")
                .action_icon(icons::OPEN)
                .show(ui);
            if r.action_clicked() {
                ctx.actions.push(AppAction::OpenFileDialog);
            }
            return;
        };
        let file = Arc::clone(&loaded.file);
        let key = Arc::as_ptr(&file) as usize;
        if key != self.file_key {
            self.file_key = key;
            self.last_followed = None;
            self.find.current = None;
        }
        self.find.update(&file, ui.ctx());
        let p = theme::ui_palette(ui);

        // Progress of a host print of this file (1-based lines processed).
        let progress = ctx
            .state
            .job
            .as_ref()
            .filter(|j| j.status.is_active() && format::job_prints_file(j, &file))
            .map(|j| j.current_line);
        if let Some(done) = progress
            && self.follow_print
            && self.last_followed != Some(done)
        {
            self.last_followed = Some(done);
            if self.hovered_line.is_none() {
                self.scroll_to = Some(done.min(file.len()) as u32);
            }
        }

        egui::Frame::new()
            .fill(p.panel)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.toolbar(ui, ctx, &file, progress.is_some(), &p);
            });
        let status_h = 24.0;
        let body = ui.available_rect_before_wrap();
        let text_rect = Rect::from_min_max(body.min, egui::pos2(body.max.x, body.max.y - status_h));
        let status_rect = Rect::from_min_max(egui::pos2(body.min.x, text_rect.max.y), body.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
            self.text(ui, ctx, &file, progress, &p);
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(status_rect), |ui| {
            self.status(ui, ctx, &file, progress, &p);
        });
        ui.allocate_rect(body, Sense::hover());
        self.confirm_dangerous(ui.ctx(), ctx);
    }

    fn toolbar(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        file: &Arc<GcodeFile>,
        printing: bool,
        p: &Palette,
    ) {
        // Narrow tabs wrap the find bar, the go-to field and the follow
        // toggle onto more rows instead of cutting them off.
        let compact = ui.available_width() < COMPACT_TOOLBAR_W;
        if compact {
            ui.horizontal_wrapped(|ui| self.toolbar_items(ui, ctx, file, printing, compact, p));
        } else {
            ui.horizontal(|ui| self.toolbar_items(ui, ctx, file, printing, compact, p));
        }
    }

    fn toolbar_items(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        file: &Arc<GcodeFile>,
        printing: bool,
        compact: bool,
        p: &Palette,
    ) {
        ui.spacing_mut().item_spacing.x = 6.0;
        if self.find.open {
            self.find_bar(ui, ctx, file, compact, p);
        } else if ui
            .add(
                IconButton::new(icons::SEARCH)
                    .ghost()
                    .tooltip("Find (Ctrl+F)"),
            )
            .clicked()
        {
            self.open_find();
        }
        ui.separator();
        match &mut self.goto {
            Some(goto) => {
                ui.label(RichText::new("Go to line").color(p.text_dim));
                let id = egui::Id::new("sprint-gcode-goto");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut goto.text)
                        .id(id)
                        .desired_width(90.0)
                        .hint_text(format!("1–{}", format::count(file.len() as u64)))
                        .text_color_opt(goto.error.then_some(p.danger)),
                );
                if goto.focus {
                    edit.request_focus();
                    goto.focus = false;
                }
                if edit.changed() {
                    goto.error = false;
                }
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let escape = ui.input(|i| i.key_pressed(Key::Escape))
                    && (edit.has_focus() || edit.lost_focus());
                let mut close = escape;
                if enter {
                    match parse_goto(&goto.text, file.len()) {
                        Some(line) => {
                            ctx.actions.push(AppAction::SelectLine(line));
                            self.scroll_to = Some(line);
                            close = true;
                        }
                        None => {
                            goto.error = true;
                            goto.focus = true;
                        }
                    }
                }
                if ui
                    .add(
                        IconButton::new(icons::CLOSE)
                            .ghost()
                            .small()
                            .tooltip("Close (Esc)"),
                    )
                    .clicked()
                {
                    close = true;
                }
                if close {
                    self.goto = None;
                }
            }
            None => {
                if ui
                    .add(
                        IconButton::new(icons::ARROW_RIGHT)
                            .ghost()
                            .tooltip("Go to line (Ctrl+G)"),
                    )
                    .clicked()
                {
                    self.open_goto();
                }
            }
        }
        if ctx.file.is_some_and(|f| f.path.is_some())
            && ui
                .add(
                    IconButton::new(icons::EDIT)
                        .ghost()
                        .tooltip("Edit in an external editor (reloads when saved)"),
                )
                .clicked()
        {
            ctx.actions.push(AppAction::EditFile);
        }
        if printing && compact {
            ui.add(crate::widgets::Toggle::new(&mut self.follow_print).label("Follow print"))
                .on_hover_text("Keep the line being printed in view");
        } else if printing {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(crate::widgets::Toggle::new(&mut self.follow_print).label("Follow print"))
                    .on_hover_text("Keep the line being printed in view");
            });
        }
    }

    fn find_bar(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        file: &Arc<GcodeFile>,
        compact: bool,
        p: &Palette,
    ) {
        let id = egui::Id::new("sprint-gcode-find");
        // Enter / Shift+Enter in the field step through matches.
        let field_focused = ui.ctx().memory(|m| m.has_focus(id));
        let mut step = None;
        if field_focused {
            ui.input_mut(|i| {
                if i.consume_key(Modifiers::SHIFT, Key::Enter) {
                    step = Some(false);
                } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                    step = Some(true);
                }
            });
        }
        ui.label(RichText::new(icons::SEARCH).color(p.text_dim));
        let edit = ui.add(
            egui::TextEdit::singleline(&mut self.find.query)
                .id(id)
                .desired_width(if compact { 140.0 } else { 200.0 })
                .hint_text("Find in file"),
        );
        if self.find.focus {
            edit.request_focus();
            self.find.focus = false;
        }
        // Escape takes the focus away before the field sees it (egui clears
        // it at the start of the frame), hence `lost_focus` too.
        let escape =
            (edit.has_focus() || edit.lost_focus()) && ui.input(|i| i.key_pressed(Key::Escape));
        ui.add(egui::Button::selectable(self.find.case_sensitive, "Aa").small())
            .on_hover_text("Match case")
            .clicked()
            .then(|| self.find.case_sensitive = !self.find.case_sensitive);
        let count = self.find.matches().len();
        let label = if self.find.query.is_empty() {
            String::new()
        } else if self.find.searching() {
            format!("{}…", format::count(count as u64))
        } else if count == 0 {
            "No matches".to_owned()
        } else {
            let current = self.find.current.map_or(0, |c| c + 1);
            let more = if self.find.search.as_ref().is_some_and(|s| s.truncated) {
                "+"
            } else {
                ""
            };
            if current > 0 {
                format!(
                    "{} of {}{more}",
                    format::count(current as u64),
                    format::count(count as u64)
                )
            } else {
                format!("{}{more} lines", format::count(count as u64))
            }
        };
        if self.find.searching() {
            ui.add(egui::Spinner::new().size(12.0));
        }
        ui.label(RichText::new(label).color(
            if count == 0 && !self.find.query.is_empty() && !self.find.searching() {
                p.danger
            } else {
                p.text_dim
            },
        ));
        if ui
            .add_enabled(
                count > 0,
                IconButton::new(icons::CARET_UP)
                    .ghost()
                    .small()
                    .tooltip("Previous (Shift+Enter)"),
            )
            .clicked()
        {
            step = Some(false);
        }
        if ui
            .add_enabled(
                count > 0,
                IconButton::new(icons::CARET_DOWN)
                    .ghost()
                    .small()
                    .tooltip("Next (Enter)"),
            )
            .clicked()
        {
            step = Some(true);
        }
        if let Some(forward) = step {
            // Results may still be arriving; use what is there.
            self.find.update(file, ui.ctx());
            if let Some(line) = self.find.step(ctx.selected_line, forward) {
                ctx.actions.push(AppAction::SelectLine(line));
                self.scroll_to = Some(line);
            }
            ui.memory_mut(|m| m.request_focus(id));
        }
        if ui
            .add(
                IconButton::new(icons::CLOSE)
                    .ghost()
                    .small()
                    .tooltip("Close (Esc)"),
            )
            .clicked()
            || escape
        {
            self.find.open = false;
            self.find.query.clear();
            self.find.search = None;
        }
    }

    fn text(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        file: &Arc<GcodeFile>,
        progress: Option<usize>,
        p: &Palette,
    ) {
        let font = theme::builtin_font(ui, egui::TextStyle::Monospace);
        let row_h = (ui.ctx().fonts_mut(|f| f.row_height(&font)) + 5.0)
            .round()
            .max(14.0);
        let n = file.len();
        let digits = format::count(n as u64).len().max(4);
        let char_w = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '0')).max(4.0);
        let gutter = digits as f32 * char_w + 20.0;
        let selected = ctx.selected_line;
        let query =
            (self.find.open && !self.find.query.is_empty()).then(|| self.find.query.clone());
        let case = self.find.case_sensitive;
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("sprint-gcode-text")
            .auto_shrink([false, false]);
        let viewport_h = ui.available_height();
        if let Some(line) = self.scroll_to.take() {
            let line = (line as usize).min(n.saturating_sub(1));
            let target = (line as f32 * row_h - viewport_h * 0.35).max(0.0);
            scroll = scroll.vertical_scroll_offset(target);
        }
        let mut hovered = None;
        let bg = p.bg;
        egui::Frame::new().fill(bg).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            scroll.show_rows(ui, row_h, n, |ui, rows| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let width = ui.available_width();
                for index in rows {
                    let line = file.line(index);
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    let painter = ui.painter_at(rect);
                    let i32_index = index as u32;
                    let is_selected = selected == Some(i32_index);
                    let printed = progress.is_some_and(|done| index < done);
                    let current = progress.is_some_and(|done| index == done);
                    let is_match =
                        query.is_some() && self.find.matches().binary_search(&i32_index).is_ok();
                    // Backgrounds.
                    if is_selected {
                        painter.rect_filled(rect, 0.0, p.accent_soft);
                    } else if current {
                        painter.rect_filled(rect, 0.0, theme::with_alpha(p.accent, 0.10));
                    } else if response.hovered() {
                        painter.rect_filled(rect, 0.0, theme::with_alpha(p.text, 0.04));
                    }
                    // Gutter: progress strip and line number.
                    let gutter_rect =
                        Rect::from_min_max(rect.min, egui::pos2(rect.min.x + gutter, rect.max.y));
                    painter.rect_filled(gutter_rect, 0.0, p.panel);
                    if printed {
                        painter.rect_filled(
                            Rect::from_min_max(
                                gutter_rect.min,
                                egui::pos2(gutter_rect.min.x + 3.0, gutter_rect.max.y),
                            ),
                            0.0,
                            theme::with_alpha(p.success, 0.7),
                        );
                    }
                    if current {
                        painter.rect_filled(
                            Rect::from_min_max(
                                gutter_rect.min,
                                egui::pos2(gutter_rect.min.x + 3.0, gutter_rect.max.y),
                            ),
                            0.0,
                            p.accent,
                        );
                    }
                    if is_match {
                        painter.circle_filled(
                            egui::pos2(gutter_rect.min.x + 9.0, rect.center().y),
                            2.5,
                            p.warning,
                        );
                    }
                    let number_color = if is_selected || current {
                        p.text
                    } else {
                        p.text_faint
                    };
                    painter.text(
                        egui::pos2(gutter_rect.max.x - 10.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        (index + 1).to_string(),
                        font.clone(),
                        number_color,
                    );
                    // The line itself.
                    let shown = cut(line, MAX_DRAWN_CHARS);
                    let job = line_job(
                        shown,
                        &font,
                        p,
                        query.as_deref(),
                        case,
                        printed && !is_selected,
                    );
                    let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                    painter.galley(
                        egui::pos2(
                            gutter_rect.max.x + 8.0,
                            rect.center().y - galley.size().y / 2.0,
                        ),
                        galley,
                        p.text,
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Label,
                            true,
                            format!("Line {}: {}", index + 1, shown),
                        )
                    });
                    if response.hovered() {
                        hovered = Some(i32_index);
                    }
                    if response.clicked() {
                        ctx.actions.push(AppAction::SelectLine(i32_index));
                    }
                    let response = if response.hovered() {
                        doc_tooltip(response, line, p)
                    } else {
                        response
                    };
                    response.context_menu(|ui| {
                        self.context_menu(ui, ctx, file, index);
                    });
                }
            });
        });
        self.hovered_line = hovered;
    }

    fn context_menu(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, file: &GcodeFile, index: usize) {
        let line = file.line(index);
        ui.label(
            RichText::new(format!("Line {}", format::count(index as u64 + 1)))
                .size(12.0)
                .color(theme::ui_palette(ui).text_dim),
        );
        if ui
            .button(icons::with_text(icons::COPY, "Copy line"))
            .clicked()
        {
            ui.ctx().copy_text(line.to_owned());
            ui.close();
        }
        if ui
            .button(icons::with_text(icons::VIEW_3D, "Show in 3D view"))
            .clicked()
        {
            ctx.actions.push(AppAction::SelectLine(index as u32));
            ctx.actions.push(AppAction::ShowTab(Tab::View3D));
            ui.close();
        }
        ui.separator();
        let can_print = crate::panels::job_bar::print_blocker(ctx.state, Some(file));
        if ui
            .add_enabled(
                can_print.is_none(),
                egui::Button::new(icons::with_text(icons::PLAY, "Start print from here…")),
            )
            .on_disabled_hover_text(can_print.unwrap_or(""))
            .clicked()
        {
            ctx.actions
                .push(AppAction::StartPrintDialog { start_line: index });
            ui.close();
        }
        let command = file.command(index).map(|c| c.into_owned());
        // Like the console: the shared classifier decides, in what the job
        // does (also a print from the card: the line would come between
        // its own).
        let job = safety::job_context(ctx.state);
        let check = command
            .as_ref()
            .map(|c| safety::assess(std::slice::from_ref(c), job));
        let send_blocker = if command.is_none() {
            Some("This line has no command".to_owned())
        } else if let Some(reason) = ctx.command_blocker() {
            Some(reason.to_owned())
        } else {
            check
                .as_ref()
                .filter(|c| !c.blocked.is_empty())
                .map(|c| format!("Not now: {}", c.blocked.join(" ")))
        };
        if ui
            .add_enabled(
                send_blocker.is_none(),
                egui::Button::new(icons::with_text(icons::SEND, "Send line to printer")),
            )
            .on_disabled_hover_text(send_blocker.unwrap_or_default())
            .clicked()
        {
            if let (Some(command), Some(check)) = (command, check) {
                if check.emergency_stop {
                    ctx.printer.emergency_stop();
                    ctx.toasts
                        .warning("Emergency stop sent")
                        .message(crate::job_text::ESTOP_MESSAGE);
                } else if check.is_clear() {
                    send_line(ctx, &command);
                } else {
                    self.confirm_send = Some((command, check, job));
                }
            }
            ui.close();
        }
    }

    /// The "send this line?" question, while one waits.
    fn confirm_dangerous(&mut self, egui_ctx: &egui::Context, ctx: &mut AppCtx<'_>) {
        let Some((command, check, job)) = &self.confirm_send else {
            return;
        };
        let answer = ConfirmDialog::new("sprint-gcode-dangerous", check.title(*job))
            .message(&check.message(*job))
            .details(command)
            .confirm_label("Send anyway")
            .danger(true)
            .width(460.0)
            .show(egui_ctx);
        match answer {
            Some(DialogResult::Confirmed) => {
                if let Some((command, _, asked_in)) = self.confirm_send.take() {
                    // What the user agreed to must still be all there is.
                    let job = safety::job_context(ctx.state);
                    let check = safety::assess(std::slice::from_ref(&command), job);
                    let ready = ctx.command_blocker().is_none() && check.blocked.is_empty();
                    if ready && (job == asked_in || check.is_clear()) {
                        send_line(ctx, &command);
                    } else {
                        ctx.toasts
                            .warning("Not sent")
                            .message("The printer is not ready for it any more.");
                    }
                }
            }
            Some(DialogResult::Cancelled) => self.confirm_send = None,
            None => {}
        }
    }

    /// True while a line waits for confirmation.
    pub fn is_confirming(&self) -> bool {
        self.confirm_send.is_some()
    }

    fn status(
        &mut self,
        ui: &mut Ui,
        ctx: &AppCtx<'_>,
        file: &GcodeFile,
        progress: Option<usize>,
        p: &Palette,
    ) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, p.panel);
        ui.painter()
            .hline(rect.x_range(), rect.top(), Stroke::new(1.0, p.border));
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 0.0))),
            |ui| {
                // Narrow tabs keep the essentials (the rest is on hover).
                let compact = ui.available_width() < COMPACT_TOOLBAR_W;
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let small = |t: String| RichText::new(t).size(12.0).color(p.text_dim);
                    let lines = format::count(file.len() as u64);
                    let commands =
                        format!("{} commands", format::count(file.command_count() as u64));
                    let size = format::bytes(file.byte_len() as u64);
                    let first = match ctx.selected_line {
                        Some(line) => {
                            format!("Line {} of {lines}", format::count(u64::from(line) + 1))
                        }
                        None => format!("{lines} lines"),
                    };
                    let first = ui.label(small(first));
                    if compact {
                        first.on_hover_text(format!("{commands} · {size}"));
                    } else {
                        ui.label(small(commands));
                        ui.label(small(size));
                    }
                    if let Some(done) = progress {
                        // As the job bar says it: PAUSED, CANCELLING, ...
                        let (badge, tone) = ctx.state.job.as_ref().map_or_else(
                            || ("PRINTING".to_owned(), theme::Tone::Accent),
                            crate::panels::job_bar::job_badge,
                        );
                        ui.add(Badge::new(&badge).tone(tone));
                        let done = format::count(done as u64);
                        ui.label(small(if compact {
                            format!("{done} done")
                        } else {
                            format!("{done} of {lines} lines done")
                        }));
                    }
                });
            },
        );
    }
}

/// Send one line of the file to the printer, with a toast.
fn send_line(ctx: &mut AppCtx<'_>, command: &str) {
    ctx.printer.send(command);
    ctx.toasts
        .info("Sent")
        .message(format::sanitize(command, 80));
}

/// `line` cut to at most `max` chars.
fn cut(line: &str, max: usize) -> &str {
    match line.char_indices().nth(max) {
        Some((i, _)) => &line[..i],
        None => line,
    }
}

/// A syntax-colored layout job for one line, with search matches
/// highlighted. `dim` fades it (already printed).
fn line_job(
    line: &str,
    font: &FontId,
    p: &Palette,
    query: Option<&str>,
    case: bool,
    dim: bool,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_rows = 1;
    let tokens = tokenize(line);
    let matches = query
        .map(|q| match_ranges(line, q, case))
        .unwrap_or_default();
    // Cut points: token and match boundaries.
    let mut cuts: Vec<usize> = Vec::with_capacity(tokens.len() * 2 + matches.len() * 2 + 2);
    cuts.push(0);
    cuts.push(line.len());
    for (r, _) in &tokens {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    for r in &matches {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut token_index = 0;
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b || !line.is_char_boundary(a) || !line.is_char_boundary(b) {
            continue;
        }
        while token_index + 1 < tokens.len() && tokens[token_index].0.end <= a {
            token_index += 1;
        }
        let kind = tokens
            .get(token_index)
            .filter(|(r, _)| r.start <= a && a < r.end)
            .map_or(TokenKind::Plain, |(_, k)| *k);
        let mut color = token_color(kind, p);
        if dim {
            color = theme::mix(color, p.bg, 0.35);
        }
        let highlighted = matches.iter().any(|m| m.start <= a && a < m.end);
        let format = TextFormat {
            font_id: font.clone(),
            color: if highlighted { p.on_accent } else { color },
            background: if highlighted {
                theme::with_alpha(p.warning, 0.85)
            } else {
                Color32::TRANSPARENT
            },
            italics: kind == TokenKind::Comment,
            ..Default::default()
        };
        job.append(&line[a..b], 0.0, format);
    }
    job
}

/// Show the G-code reference for the line's command on hover.
fn doc_tooltip(response: egui::Response, line: &str, p: &Palette) -> egui::Response {
    let command = command_part(line);
    let Some(doc) = sprint_proto::gcode_doc(command) else {
        return response;
    };
    let p = *p;
    response.on_hover_ui(move |ui| {
        ui.set_max_width(360.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(doc.code).monospace().strong().color(p.accent));
            ui.label(RichText::new(doc.name).strong());
            if doc.dangerous {
                ui.add(Badge::new("CAUTION").tone(theme::Tone::Danger));
            }
        });
        ui.label(
            RichText::new(doc.usage)
                .monospace()
                .size(12.0)
                .color(p.text_dim),
        );
        ui.add_space(2.0);
        ui.label(doc.description);
        ui.label(
            RichText::new(doc.category.name())
                .size(11.0)
                .color(p.text_faint),
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<(&str, TokenKind)> {
        tokenize(line)
            .into_iter()
            .filter(|(_, k)| *k != TokenKind::Plain)
            .map(|(r, k)| (&line[r], k))
            .collect()
    }

    fn covers(line: &str) {
        let tokens = tokenize(line);
        let mut at = 0;
        for (r, _) in &tokens {
            assert_eq!(r.start, at, "{line:?}: {tokens:?}");
            assert!(line.is_char_boundary(r.start) && line.is_char_boundary(r.end));
            at = r.end;
        }
        assert_eq!(at, line.len(), "{line:?}: {tokens:?}");
    }

    #[test]
    fn tokenizes_classic_lines() {
        use TokenKind::*;
        assert_eq!(
            kinds("G1 X10.5 Y-2 E0.03 F1800 ; perimeter"),
            vec![
                ("G1", GCode),
                ("X", Param),
                ("10.5", Value),
                ("Y", Param),
                ("-2", Value),
                ("E", Param),
                ("0.03", Value),
                ("F", Param),
                ("1800", Value),
                ("; perimeter", Comment),
            ]
        );
        assert_eq!(
            kinds("N12 M104 S200*45"),
            vec![
                ("N12", LineNumber),
                ("M104", MCode),
                ("S", Param),
                ("200", Value),
                ("*45", Checksum)
            ]
        );
        assert_eq!(
            kinds("g1x10"),
            vec![("g1", GCode), ("x", Param), ("10", Value)]
        );
        assert_eq!(kinds("T1"), vec![("T1", OtherCode)]);
        assert_eq!(kinds("G29.1"), vec![("G29.1", GCode)]);
        assert_eq!(
            kinds("(old style) G28"),
            vec![("(old style)", Comment), ("G28", GCode)]
        );
    }

    #[test]
    fn tokenizes_text_and_extended_commands() {
        use TokenKind::*;
        assert_eq!(
            kinds("M117 Layer 3 (of 9) ; msg"),
            vec![
                ("M117", MCode),
                ("Layer 3 (of 9)", Text),
                ("; msg", Comment)
            ]
        );
        assert_eq!(
            kinds("M23 cube.gco"),
            vec![("M23", MCode), ("cube.gco", Text)]
        );
        assert_eq!(
            kinds("SET_FAN_SPEED FAN=part SPEED=0.5"),
            vec![
                ("SET_FAN_SPEED", Extended),
                ("FAN=", Param),
                ("part", Value),
                ("SPEED=", Param),
                ("0.5", Value)
            ]
        );
        assert_eq!(
            kinds("M98 P\"homeall.g\""),
            vec![("M98", MCode), ("P", Param), ("\"homeall.g\"", Text)]
        );
    }

    #[test]
    fn tokens_cover_every_line() {
        for line in [
            "",
            "   ",
            ";",
            "G1 X1",
            "M117 ünïcödé ✓",
            "(unterminated",
            "\"unterminated",
            "{expr",
            "G1 X{1+2} Y\"a\" Z3 *",
            "123 456",
            "ÄÖÜ G1",
            "N N1 NN",
            "M118 E1 hello ;c",
            "X=",
            "_ab=cd",
        ] {
            covers(line);
        }
        // Arbitrary bytes never panic.
        let mut seed = 7u32;
        for _ in 0..500 {
            let line: String = (0..24)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    let set = b"GMNT XYZEF0123456789.-;()\"{}*= _a\xc3";
                    char::from(set[(seed >> 16) as usize % (set.len() - 1)])
                })
                .collect();
            covers(&line);
        }
    }

    #[test]
    fn matches_within_a_line() {
        assert_eq!(match_ranges("G1 X1 x2", "x", false), vec![3..4, 6..7]);
        assert_eq!(match_ranges("G1 X1 x2", "x", true), vec![6..7]);
        assert_eq!(match_ranges("aaaa", "aa", true), vec![0..2, 2..4]);
        assert!(match_ranges("abc", "", false).is_empty());
        assert!(match_ranges("ab", "abc", false).is_empty());
        assert_eq!(match_ranges("é é", "é", false), vec![0..2, 3..5]);
    }

    #[test]
    fn finds_lines_in_a_file() {
        let file = GcodeFile::from_text(
            "t.gcode",
            "G28\nG1 X10 ; first\nM104 S200\ng1 x5 g1\n\nG1 Y1",
        );
        let never = AtomicBool::new(false);
        let find = |q: &str, case: bool| {
            find_lines(&file, q, case, &never, MAX_MATCHES, &mut |_| {})
                .expect("not cancelled")
                .0
        };
        assert_eq!(find("G1", true), vec![1, 5]);
        assert_eq!(find("g1", false), vec![1, 3, 5]);
        assert_eq!(find("first", false), vec![1]);
        assert!(find("nothing", false).is_empty());
        assert!(find("", false).is_empty());
        assert!(find("a\nb", false).is_empty());
        assert_eq!(find("Y1", true), vec![5], "match at the very end");
        let limited = find_lines(&file, "1", false, &never, 2, &mut |_| {}).expect("ok");
        assert_eq!(limited, (vec![1, 2], true));
        let cancelled = AtomicBool::new(true);
        assert!(find_lines(&file, "G", false, &cancelled, 10, &mut |_| {}).is_none());
    }

    #[test]
    fn find_steps_wrap_around() {
        let file = Arc::new(GcodeFile::from_text("t", "G1\nX\nG1\nX\nG1\n"));
        let ctx = egui::Context::default();
        let mut find = FindBar {
            query: "G1".into(),
            ..FindBar::default()
        };
        find.update(&file, &ctx);
        assert_eq!(find.matches(), &[0, 2, 4]);
        assert_eq!(find.step(None, true), Some(0));
        assert_eq!(find.step(Some(0), true), Some(2));
        assert_eq!(find.step(Some(2), true), Some(4));
        assert_eq!(find.step(Some(4), true), Some(0));
        assert_eq!(find.step(Some(0), false), Some(4));
        // From a selection between matches.
        find.current = None;
        assert_eq!(find.step(Some(1), true), Some(2));
        find.current = None;
        assert_eq!(find.step(Some(3), false), Some(2));
        // A new query restarts.
        find.query = "X".into();
        find.update(&file, &ctx);
        assert_eq!(find.matches(), &[1, 3]);
        assert_eq!(find.current, None);
        find.query.clear();
        find.update(&file, &ctx);
        assert!(find.matches().is_empty());
        assert_eq!(find.step(None, true), None);
    }

    #[test]
    fn large_files_are_searched_in_the_background() {
        let text: String = (0..120_000).map(|i| format!("G1 X{i} Y{i}\n")).collect();
        let file = Arc::new(GcodeFile::from_text("big", text));
        assert!(file.byte_len() >= SYNC_SEARCH_BYTES);
        let ctx = egui::Context::default();
        let mut find = FindBar {
            query: "X1199".into(),
            ..FindBar::default()
        };
        find.update(&file, &ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while find.searching() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
            find.update(&file, &ctx);
        }
        // X1199, X11990..X11999, X119900..X119999
        assert_eq!(find.matches().len(), 1 + 10 + 100);
        assert!(find.matches().windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn goto_parsing() {
        assert_eq!(parse_goto("1", 10), Some(0));
        assert_eq!(parse_goto(" 1,234 ", 5000), Some(1233));
        assert_eq!(parse_goto("99999", 10), Some(9));
        assert_eq!(parse_goto("0", 10), None);
        assert_eq!(parse_goto("abc", 10), None);
        assert_eq!(parse_goto("5", 0), None);
        assert_eq!(command_part("N12 G1 X1*3"), "G1 X1*3");
        assert_eq!(command_part("  M104 S1"), "M104 S1");
        assert_eq!(command_part("Nope"), "Nope");
        assert_eq!(cut("abcdef", 3), "abc");
        assert_eq!(cut("äöü", 5), "äöü");
    }
}
