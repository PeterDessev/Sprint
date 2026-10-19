//! A single-line command editor with history, Emacs keys and Tab
//! completion from the G-code reference.
//!
//! [`CommandInput`] holds the text and handles crossterm key events;
//! [`CommandLine`] draws it and [`CompletionPopup`] draws the completion
//! candidates.

use std::borrow::Cow;
use std::cell::Cell;
use std::fmt;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::theme::{Theme, default_theme};
use crate::widgets::util::{self, char_width};

/// Most completion candidates offered at once.
pub const MAX_CANDIDATES: usize = 50;

/// Default number of history entries kept.
pub const DEFAULT_MAX_HISTORY: usize = 500;

/// One completion candidate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Candidate {
    /// Text that replaces the word being completed (`"M104"`).
    pub insert: String,
    /// Short description shown next to it (`"Set hotend temperature"`).
    pub description: String,
    /// The command should be confirmed before sending; the popup marks it.
    pub dangerous: bool,
}

/// Candidates from the G-code reference ([`sprint_proto::search_gcode`]):
/// codes starting with `word` (`"M10"` -> M10, M100, M104, ...) or, for
/// words, commands whose name matches (`"temp"` -> M104, M105, ...). At
/// most [`MAX_CANDIDATES`].
pub fn gcode_candidates(word: &str) -> Vec<Candidate> {
    sprint_proto::search_gcode(word)
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|doc| Candidate {
            insert: doc.code.to_string(),
            description: doc.name.to_string(),
            dangerous: doc.dangerous,
        })
        .collect()
}

/// A completion source: candidates for the first word of the line.
pub type Completer = Box<dyn Fn(&str) -> Vec<Candidate> + Send + Sync>;

/// What a key did to a [`CommandInput`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// The editor does not use this key; the application may.
    Ignored,
    /// The editor consumed the key (the text, cursor or completion popup
    /// may have changed).
    Handled,
    /// Enter on a non-empty line: the line, trimmed. It was added to the
    /// history and the editor was cleared.
    Submitted(String),
}

/// The open completion popup.
#[derive(Clone, Debug)]
struct Popup {
    candidates: Vec<Candidate>,
    selected: usize,
}

/// A single-line editor for commands.
///
/// Keys (presses and repeats; releases are ignored):
///
/// | Key | Action |
/// |---|---|
/// | printable characters | insert |
/// | Left/Right, Ctrl+B/F | move by character |
/// | Ctrl+Left/Right, Alt+B/F | move by word |
/// | Home/End, Ctrl+A/E | start/end of line |
/// | Backspace/Ctrl+H, Delete/Ctrl+D | delete before/at the cursor |
/// | Ctrl+W, Alt+Backspace | delete the word before the cursor |
/// | Alt+D | delete the word after the cursor |
/// | Ctrl+U / Ctrl+K | delete to the start / end of the line |
/// | Ctrl+Y | paste the last deleted text |
/// | Up/Down, Ctrl+P/N | older/newer history entry |
/// | Tab | complete the command (one match) or open the popup |
/// | Tab/Down, Shift+Tab/Up | (popup) next/previous candidate |
/// | Enter | (popup) accept the candidate; otherwise submit |
/// | Esc | (popup) close it |
///
/// Everything else (Esc without a popup, Ctrl+C, function keys, ...) is
/// [`InputEvent::Ignored`] so the application can bind it. Control
/// characters never enter the text (pasted text has them replaced by
/// spaces).
pub struct CommandInput {
    text: String,
    /// Byte offset of the cursor, always on a char boundary.
    cursor: usize,
    history: Vec<String>,
    /// Entry being shown while browsing the history.
    history_index: Option<usize>,
    /// The line being edited before browsing the history.
    draft: String,
    max_history: usize,
    kill_buffer: String,
    popup: Option<Popup>,
    completer: Completer,
    /// First text column shown (horizontal scroll), kept between renders.
    scroll: Cell<usize>,
}

impl fmt::Debug for CommandInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandInput")
            .field("text", &self.text)
            .field("cursor", &self.cursor)
            .field("history", &self.history.len())
            .field("history_index", &self.history_index)
            .field("popup", &self.popup)
            .finish_non_exhaustive()
    }
}

impl Default for CommandInput {
    fn default() -> Self {
        CommandInput::new()
    }
}

impl CommandInput {
    /// An empty editor completing from the G-code reference.
    pub fn new() -> CommandInput {
        CommandInput {
            text: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            max_history: DEFAULT_MAX_HISTORY,
            kill_buffer: String::new(),
            popup: None,
            completer: Box::new(gcode_candidates),
            scroll: Cell::new(0),
        }
    }

    /// Use another completion source (for example shell commands plus
    /// G-code).
    #[must_use]
    pub fn with_completer(
        mut self,
        completer: impl Fn(&str) -> Vec<Candidate> + Send + Sync + 'static,
    ) -> Self {
        self.completer = Box::new(completer);
        self
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor as a byte offset into [`text`](CommandInput::text).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Terminal columns before the cursor.
    pub fn cursor_column(&self) -> usize {
        util::width(self.text.get(..self.cursor).unwrap_or(""))
    }

    /// True when the line is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replace the text (control characters become spaces) and put the
    /// cursor at its end.
    pub fn set_text(&mut self, text: &str) {
        self.text = single_line(text);
        self.cursor = self.text.len();
        self.history_index = None;
        self.popup = None;
    }

    /// Clear the text.
    pub fn clear(&mut self) {
        self.set_text("");
    }

    /// Insert text at the cursor (for pastes); control characters,
    /// including line breaks, become spaces.
    pub fn insert_str(&mut self, text: &str) {
        let clean = single_line(text);
        if clean.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        self.edited();
    }

    /// Insert one character at the cursor (control characters are
    /// ignored).
    pub fn insert_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.edited();
    }

    /// Entries submitted so far, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Replace the history (for example with one loaded from disk); keeps
    /// the newest [`max_history`](CommandInput::set_max_history) entries.
    pub fn set_history(&mut self, entries: impl IntoIterator<Item = String>) {
        self.history = entries
            .into_iter()
            .map(|e| single_line(&e).trim().to_string())
            .filter(|e| !e.is_empty())
            .collect();
        self.trim_history();
        self.history_index = None;
    }

    /// Add an entry to the history (skipped when empty or equal to the
    /// newest entry).
    pub fn push_history(&mut self, entry: &str) {
        let entry = single_line(entry).trim().to_string();
        if entry.is_empty() || self.history.last() == Some(&entry) {
            return;
        }
        self.history.push(entry);
        self.trim_history();
    }

    /// Keep at most this many history entries (at least 1).
    pub fn set_max_history(&mut self, max: usize) {
        self.max_history = max.max(1);
        self.trim_history();
    }

    fn trim_history(&mut self) {
        let excess = self.history.len().saturating_sub(self.max_history);
        if excess > 0 {
            self.history.drain(..excess);
        }
    }

    /// The completion candidates and the selected index while the popup
    /// is open.
    pub fn completion(&self) -> Option<(&[Candidate], usize)> {
        self.popup
            .as_ref()
            .map(|p| (p.candidates.as_slice(), p.selected))
    }

    /// True while the completion popup is open.
    pub fn is_completing(&self) -> bool {
        self.popup.is_some()
    }

    /// Close the completion popup without accepting a candidate.
    pub fn cancel_completion(&mut self) {
        self.popup = None;
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputEvent {
        if key.kind == KeyEventKind::Release {
            return InputEvent::Ignored;
        }
        let mods = key.modifiers;
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        let alt = mods.contains(KeyModifiers::ALT);
        if self.popup.is_some()
            && let Some(event) = self.handle_popup_key(key.code, mods)
        {
            return event;
        }
        match key.code {
            // AltGr arrives as Ctrl+Alt on Windows: still a character.
            KeyCode::Char(c) if (!ctrl && !alt) || (ctrl && alt) => {
                self.insert_char(c);
                InputEvent::Handled
            }
            KeyCode::Char(c) if ctrl => self.handle_ctrl(c),
            KeyCode::Char(c) if alt => self.handle_alt(c),
            KeyCode::Enter => self.submit(),
            KeyCode::Tab => self.complete(),
            KeyCode::Backspace if alt || ctrl => {
                self.kill_word_before();
                InputEvent::Handled
            }
            KeyCode::Backspace => {
                self.delete_before();
                InputEvent::Handled
            }
            KeyCode::Delete => {
                self.delete_at();
                InputEvent::Handled
            }
            KeyCode::Left if ctrl || alt => self.moved(self.word_start_before(self.cursor)),
            KeyCode::Right if ctrl || alt => self.moved(self.word_end_after(self.cursor)),
            KeyCode::Left => self.moved(self.prev_boundary(self.cursor)),
            KeyCode::Right => self.moved(self.next_boundary(self.cursor)),
            KeyCode::Home => self.moved(0),
            KeyCode::End => self.moved(self.text.len()),
            KeyCode::Up => {
                self.history_older();
                InputEvent::Handled
            }
            KeyCode::Down => {
                self.history_newer();
                InputEvent::Handled
            }
            _ => InputEvent::Ignored,
        }
    }

    /// Keys with the popup open; `None` falls through to normal editing.
    fn handle_popup_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<InputEvent> {
        let popup = self.popup.as_mut()?;
        let count = popup.candidates.len().max(1);
        match code {
            KeyCode::Tab | KeyCode::Down => popup.selected = (popup.selected + 1) % count,
            KeyCode::BackTab | KeyCode::Up => popup.selected = (popup.selected + count - 1) % count,
            KeyCode::Char('n') if mods.contains(KeyModifiers::CONTROL) => {
                popup.selected = (popup.selected + 1) % count;
            }
            KeyCode::Char('p') if mods.contains(KeyModifiers::CONTROL) => {
                popup.selected = (popup.selected + count - 1) % count;
            }
            KeyCode::Enter => self.accept_completion(),
            KeyCode::Esc => self.popup = None,
            _ => return None,
        }
        Some(InputEvent::Handled)
    }

    fn handle_ctrl(&mut self, c: char) -> InputEvent {
        match c.to_ascii_lowercase() {
            'a' => return self.moved(0),
            'e' => return self.moved(self.text.len()),
            'b' => return self.moved(self.prev_boundary(self.cursor)),
            'f' => return self.moved(self.next_boundary(self.cursor)),
            'h' => self.delete_before(),
            'd' => self.delete_at(),
            'w' => self.kill_word_before(),
            'u' => self.kill(0..self.cursor),
            'k' => self.kill(self.cursor..self.text.len()),
            'y' => {
                let yank = self.kill_buffer.clone();
                self.insert_str(&yank);
            }
            'p' => self.history_older(),
            'n' => self.history_newer(),
            _ => return InputEvent::Ignored,
        }
        InputEvent::Handled
    }

    fn handle_alt(&mut self, c: char) -> InputEvent {
        match c.to_ascii_lowercase() {
            'b' => self.moved(self.word_start_before(self.cursor)),
            'f' => self.moved(self.word_end_after(self.cursor)),
            'd' => {
                self.kill(self.cursor..self.word_end_after(self.cursor));
                InputEvent::Handled
            }
            _ => InputEvent::Ignored,
        }
    }

    /// Enter: submit the trimmed line.
    fn submit(&mut self) -> InputEvent {
        let line = self.text.trim().to_string();
        if line.is_empty() {
            self.clear();
            return InputEvent::Handled;
        }
        self.push_history(&line);
        self.clear();
        self.draft.clear();
        InputEvent::Submitted(line)
    }

    /// Tab: complete the command word under the cursor. One candidate is
    /// inserted directly; several open the popup. Ignored when there is no
    /// command word at the cursor.
    pub fn complete(&mut self) -> InputEvent {
        let Some(word) = self.completion_word() else {
            return InputEvent::Ignored;
        };
        let query = self.text.get(word).unwrap_or("").to_string();
        let mut candidates = (self.completer)(&query);
        candidates.truncate(MAX_CANDIDATES);
        match candidates.len() {
            0 => self.popup = None,
            1 => {
                self.popup = Some(Popup {
                    candidates,
                    selected: 0,
                });
                self.accept_completion();
            }
            _ => {
                self.popup = Some(Popup {
                    candidates,
                    selected: 0,
                });
            }
        }
        InputEvent::Handled
    }

    /// Replace the command word with the selected candidate and close the
    /// popup.
    pub fn accept_completion(&mut self) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let Some(candidate) = popup.candidates.get(popup.selected) else {
            return;
        };
        let Some(word) = self.completion_word() else {
            return;
        };
        let insert = single_line(&candidate.insert);
        self.text.replace_range(word.clone(), &insert);
        let mut end = word.start + insert.len();
        if self.text[end..].starts_with(' ') {
            end += 1;
        } else {
            self.text.insert(end, ' ');
            end += 1;
        }
        self.cursor = end;
        self.history_index = None;
    }

    /// The byte range of the first word, if the cursor is on it.
    fn completion_word(&self) -> Option<Range<usize>> {
        let start = self.text.len() - self.text.trim_start().len();
        let end = self.text[start..]
            .find(char::is_whitespace)
            .map_or(self.text.len(), |i| start + i);
        (start < end && (start..=end).contains(&self.cursor)).then_some(start..end)
    }

    /// After an edit: leave history browsing, refresh an open popup.
    fn edited(&mut self) {
        self.history_index = None;
        if self.popup.is_some() {
            let candidates = self
                .completion_word()
                .and_then(|w| self.text.get(w))
                .map(|q| (self.completer)(q))
                .unwrap_or_default();
            self.popup = (!candidates.is_empty()).then(|| Popup {
                candidates: candidates.into_iter().take(MAX_CANDIDATES).collect(),
                selected: 0,
            });
        }
    }

    /// Move the cursor (closes the popup).
    fn moved(&mut self, to: usize) -> InputEvent {
        self.cursor = to.min(self.text.len());
        self.popup = None;
        InputEvent::Handled
    }

    fn prev_boundary(&self, from: usize) -> usize {
        self.text[..from]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.text[from..]
            .chars()
            .next()
            .map_or(self.text.len(), |c| from + c.len_utf8())
    }

    /// Start of the word before `from` (skipping spaces first).
    fn word_start_before(&self, from: usize) -> usize {
        let before = &self.text[..from];
        let trimmed = before.trim_end();
        trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8())
    }

    /// End of the word after `from` (skipping spaces first).
    fn word_end_after(&self, from: usize) -> usize {
        let after = &self.text[from..];
        let skip = after.len() - after.trim_start().len();
        let rest = &after[skip..];
        let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
        from + skip + word
    }

    fn delete_before(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.prev_boundary(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.edited();
    }

    fn delete_at(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let end = self.next_boundary(self.cursor);
        self.text.replace_range(self.cursor..end, "");
        self.edited();
    }

    fn kill_word_before(&mut self) {
        let start = self.word_start_before(self.cursor);
        self.kill(start..self.cursor);
    }

    /// Delete a range into the kill buffer.
    fn kill(&mut self, range: Range<usize>) {
        if range.is_empty() || range.end > self.text.len() {
            return;
        }
        let Some(killed) = self.text.get(range.clone()) else {
            return;
        };
        self.kill_buffer = killed.to_string();
        self.text.replace_range(range.clone(), "");
        if self.cursor >= range.end {
            self.cursor -= range.len();
        } else if self.cursor > range.start {
            self.cursor = range.start;
        }
        self.edited();
    }

    fn history_older(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => {
                self.draft = self.text.clone();
                self.history.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.show_history(Some(index));
    }

    fn history_newer(&mut self) {
        let Some(i) = self.history_index else {
            return;
        };
        if i + 1 < self.history.len() {
            self.show_history(Some(i + 1));
        } else {
            self.show_history(None);
        }
    }

    fn show_history(&mut self, index: Option<usize>) {
        self.text = match index.and_then(|i| self.history.get(i)) {
            Some(entry) => entry.clone(),
            None => std::mem::take(&mut self.draft),
        };
        self.cursor = self.text.len();
        self.history_index = index;
        self.popup = None;
    }

    /// The first column shown and the cursor column within `cols` columns
    /// of text, keeping the cursor visible with as little scrolling as
    /// possible.
    fn view(&self, cols: usize) -> (usize, usize) {
        let cursor = self.cursor_column();
        let total = util::width(&self.text);
        let mut scroll = self.scroll.get();
        if cols == 0 {
            return (scroll, cursor);
        }
        if total < cols {
            scroll = 0;
        } else if cursor < scroll {
            scroll = cursor;
        } else if cursor >= scroll + cols {
            scroll = cursor + 1 - cols;
        }
        // Do not leave empty space at the end when the text could fill it.
        scroll = scroll.min((total + 1).saturating_sub(cols));
        self.scroll.set(scroll);
        (scroll, cursor)
    }
}

/// `text` on one line: control characters (line breaks, tabs, escapes)
/// become spaces.
fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Draws a [`CommandInput`]: prompt, text (scrolled horizontally to keep
/// the cursor visible) and a placeholder while empty. Put the terminal
/// cursor at [`cursor_position`](CommandLine::cursor_position).
#[derive(Clone, Debug)]
pub struct CommandLine<'a> {
    input: &'a CommandInput,
    prompt: Cow<'a, str>,
    placeholder: Option<Cow<'a, str>>,
    focused: bool,
    block: Option<Block<'a>>,
    theme: &'a Theme,
}

impl<'a> CommandLine<'a> {
    /// The editor `input` with the prompt `"> "`.
    pub fn new(input: &'a CommandInput) -> CommandLine<'a> {
        CommandLine {
            input,
            prompt: Cow::Borrowed("> "),
            placeholder: None,
            focused: true,
            block: None,
            theme: default_theme(),
        }
    }

    /// The prompt in front of the text.
    #[must_use]
    pub fn prompt(mut self, prompt: impl Into<Cow<'a, str>>) -> Self {
        self.prompt = prompt.into();
        self
    }

    /// Dim text shown while the line is empty.
    #[must_use]
    pub fn placeholder(mut self, placeholder: impl Into<Cow<'a, str>>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    /// Whether the line has keyboard focus (accent prompt and a cursor;
    /// default true).
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Surround the line with a block.
    #[must_use]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Draw with this theme.
    #[must_use]
    pub fn theme(mut self, theme: &'a Theme) -> Self {
        self.theme = theme;
        self
    }

    fn inner(&self, area: Rect) -> Rect {
        match &self.block {
            Some(block) => block.inner(area),
            None => area,
        }
    }

    /// The prompt as drawn (sanitized and cut to half the row).
    fn prompt_text(&self, width: usize) -> String {
        let prompt = single_line(&self.prompt);
        util::truncate(&prompt, width / 2).into_owned()
    }

    /// Where the terminal cursor belongs when the line is drawn in `area`;
    /// `None` when unfocused or there is no room.
    pub fn cursor_position(&self, area: Rect) -> Option<Position> {
        if !self.focused {
            return None;
        }
        let inner = self.inner(area);
        if inner.is_empty() {
            return None;
        }
        let width = usize::from(inner.width);
        let prompt_cols = util::width(&self.prompt_text(width));
        let cols = width.checked_sub(prompt_cols).filter(|c| *c > 0)?;
        let (scroll, cursor) = self.input.view(cols);
        let x = prompt_cols + cursor.saturating_sub(scroll).min(cols - 1);
        Some(Position::new(inner.x + u16::try_from(x).ok()?, inner.y))
    }
}

impl Widget for CommandLine<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if let Some(block) = &self.block {
            block.render(area, buf);
        }
        let inner = self.inner(area);
        if inner.is_empty() {
            return;
        }
        let theme = self.theme;
        let width = usize::from(inner.width);
        let prompt = self.prompt_text(width);
        let prompt_cols = util::width(&prompt);
        let cols = width.saturating_sub(prompt_cols);
        let prompt_style = if self.focused {
            theme.accent()
        } else {
            theme.dim()
        };
        let mut spans = vec![Span::styled(prompt, prompt_style)];
        let text = self.input.text();
        if text.is_empty() {
            if let Some(placeholder) = &self.placeholder {
                let shown = util::truncate(&single_line(placeholder), cols).into_owned();
                spans.push(Span::styled(shown, theme.dim()));
            }
        } else {
            let (scroll, _) = self.input.view(cols);
            spans.push(Span::styled(
                visible_slice(text, scroll, cols),
                theme.text(),
            ));
        }
        buf.set_line(inner.x, inner.y, &Line::from(spans), inner.width);
    }
}

/// The part of `text` from column `skip` that fits in `cols` columns.
/// Wide characters cut by either edge are left out; one cut by the left
/// edge leaves blanks, so the rest of the text stays in its columns (where
/// the cursor position expects it).
fn visible_slice(text: &str, skip: usize, cols: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if col < skip {
            col += w;
            if col > skip {
                let blank = (col - skip).min(cols);
                out.extend(std::iter::repeat_n(' ', blank));
                used += blank;
            }
            continue;
        }
        if used + w > cols {
            break;
        }
        out.push(c);
        used += w;
        col += w;
    }
    out
}

/// Default rows of candidates shown at once.
const POPUP_ROWS: u16 = 8;
/// Widest popup, in columns.
const POPUP_MAX_WIDTH: u16 = 64;

/// The completion candidates of a [`CommandInput`] as a bordered list
/// (`code  name`, dangerous commands marked with `!`), scrolled to keep the
/// selection visible. Draws nothing while no popup is open. Place it with
/// [`area`](CompletionPopup::area).
#[derive(Clone, Debug)]
pub struct CompletionPopup<'a> {
    input: &'a CommandInput,
    max_rows: u16,
    theme: &'a Theme,
}

impl<'a> CompletionPopup<'a> {
    /// The popup of `input`.
    pub fn new(input: &'a CommandInput) -> CompletionPopup<'a> {
        CompletionPopup {
            input,
            max_rows: POPUP_ROWS,
            theme: default_theme(),
        }
    }

    /// Show at most this many candidates at once (default 8).
    #[must_use]
    pub fn max_rows(mut self, rows: u16) -> Self {
        self.max_rows = rows.max(1);
        self
    }

    /// Draw with this theme.
    #[must_use]
    pub fn theme(mut self, theme: &'a Theme) -> Self {
        self.theme = theme;
        self
    }

    /// Columns of the widest code.
    fn code_cols(candidates: &[Candidate]) -> usize {
        candidates
            .iter()
            .map(|c| util::width(&single_line(&c.insert)))
            .max()
            .unwrap_or(0)
    }

    /// Where to draw the popup for an input line at `input_area`: above
    /// it when there is room (else below, else on the larger side, cut),
    /// left-aligned with it and inside `bounds`. `None` while no popup is
    /// open or there is no room.
    pub fn area(&self, input_area: Rect, bounds: Rect) -> Option<Rect> {
        let (candidates, _) = self.input.completion()?;
        let code_cols = Self::code_cols(candidates);
        let desc_cols = candidates
            .iter()
            .map(|c| util::width(&single_line(&c.description)))
            .max()
            .unwrap_or(0);
        // Border, space, mark, code, two spaces, description, space, border.
        let want_w = 2 + 1 + 2 + code_cols + 2 + desc_cols + 1;
        let width = u16::try_from(want_w)
            .unwrap_or(u16::MAX)
            .min(POPUP_MAX_WIDTH)
            .min(bounds.width);
        let rows = u16::try_from(candidates.len())
            .unwrap_or(u16::MAX)
            .min(self.max_rows);
        let want_h = rows.saturating_add(2);
        let above = input_area.y.saturating_sub(bounds.y);
        let below = bounds.bottom().saturating_sub(input_area.bottom());
        let (y, height) = if above >= want_h {
            (input_area.y - want_h, want_h)
        } else if below >= want_h {
            (input_area.bottom(), want_h)
        } else if above >= below {
            (bounds.y.max(input_area.y.saturating_sub(above)), above)
        } else {
            (input_area.bottom(), below)
        };
        if height < 3 || width < 8 {
            return None;
        }
        let x = input_area
            .x
            .clamp(bounds.x, bounds.right().saturating_sub(width));
        Some(Rect::new(x, y, width, height))
    }
}

impl Widget for CompletionPopup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some((candidates, selected)) = self.input.completion() else {
            return;
        };
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        Clear.render(area, buf);
        let count = candidates.len();
        let block = Block::bordered()
            .border_style(theme.border(true))
            .style(theme.surface())
            .title_bottom(
                Line::from(Span::styled(
                    format!(" {}/{count} ", selected + 1),
                    theme.dim(),
                ))
                .right_aligned(),
            );
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.is_empty() {
            return;
        }
        let rows = usize::from(inner.height);
        // Scroll so the selection is visible.
        let first = selected
            .saturating_sub(rows - 1)
            .min(count.saturating_sub(rows));
        let code_cols = Self::code_cols(candidates);
        let width = usize::from(inner.width);
        for (row, (index, candidate)) in candidates
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .enumerate()
        {
            let Ok(dy) = u16::try_from(row) else { break };
            let y = inner.y + dy;
            let is_selected = index == selected;
            let base = if is_selected {
                theme.selected()
            } else {
                theme.surface()
            };
            let code = single_line(&candidate.insert);
            let pad = " ".repeat(code_cols.saturating_sub(util::width(&code)));
            let mark = if candidate.dangerous { "! " } else { "  " };
            let mark_style = if candidate.dangerous && !is_selected {
                theme.error()
            } else {
                base
            };
            let used = 1 + 2 + code_cols + 2;
            let desc = util::truncate(
                &single_line(&candidate.description),
                width.saturating_sub(used + 1),
            )
            .into_owned();
            let desc_style = if is_selected { base } else { theme.dim() };
            let text_cols = used + util::width(&desc);
            let line = Line::from(vec![
                Span::styled(" ", base),
                Span::styled(mark, mark_style),
                Span::styled(format!("{code}{pad}  "), base),
                Span::styled(desc, desc_style),
                Span::styled(" ".repeat(width.saturating_sub(text_cols)), base),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testutil::lines;
    use crossterm::event::KeyEventState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn typed(text: &str) -> CommandInput {
        let mut input = CommandInput::new();
        for c in text.chars() {
            assert_eq!(input.handle_key(key(KeyCode::Char(c))), InputEvent::Handled);
        }
        input
    }

    #[test]
    fn typing_and_submitting() {
        let mut input = typed("  G28 X ");
        assert_eq!(input.text(), "  G28 X ");
        assert_eq!(
            input.handle_key(key(KeyCode::Enter)),
            InputEvent::Submitted("G28 X".to_string())
        );
        assert!(input.is_empty());
        assert_eq!(input.history(), ["G28 X"]);
        // Enter on a blank line submits nothing.
        let mut blank = typed("   ");
        assert_eq!(blank.handle_key(key(KeyCode::Enter)), InputEvent::Handled);
        assert!(blank.is_empty());
        assert!(blank.history().is_empty());
    }

    #[test]
    fn releases_and_unbound_keys_are_ignored() {
        let mut input = typed("M105");
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
            ..key(KeyCode::Char('x'))
        };
        assert_eq!(input.handle_key(release), InputEvent::Ignored);
        assert_eq!(input.handle_key(ctrl('c')), InputEvent::Ignored);
        assert_eq!(input.handle_key(key(KeyCode::Esc)), InputEvent::Ignored);
        assert_eq!(input.handle_key(key(KeyCode::F(1))), InputEvent::Ignored);
        assert_eq!(input.handle_key(alt('x')), InputEvent::Ignored);
        assert_eq!(input.text(), "M105");
        // AltGr (Ctrl+Alt) still types.
        let altgr = KeyEvent::new(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(input.handle_key(altgr), InputEvent::Handled);
        assert_eq!(input.text(), "M105@");
        // Shifted characters type normally.
        let shifted = KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT);
        input.handle_key(shifted);
        assert_eq!(input.text(), "M105@X");
    }

    #[test]
    fn cursor_movement_and_editing() {
        let mut input = typed("G1 X10");
        input.handle_key(key(KeyCode::Home));
        assert_eq!(input.cursor(), 0);
        input.handle_key(key(KeyCode::Delete));
        assert_eq!(input.text(), "1 X10");
        input.handle_key(key(KeyCode::Char('G')));
        input.handle_key(ctrl('e'));
        assert_eq!(input.cursor(), input.text().len());
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.text(), "G1 X1");
        input.handle_key(ctrl('a'));
        input.handle_key(ctrl('f'));
        input.handle_key(key(KeyCode::Right));
        assert_eq!(input.cursor(), 2);
        input.handle_key(ctrl('b'));
        input.handle_key(key(KeyCode::Left));
        assert_eq!(input.cursor(), 0);
        // Moving past the ends is harmless.
        input.handle_key(key(KeyCode::Left));
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.cursor(), 0);
        input.handle_key(key(KeyCode::End));
        input.handle_key(key(KeyCode::Right));
        input.handle_key(ctrl('d'));
        assert_eq!(input.text(), "G1 X1");
        input.handle_key(ctrl('h'));
        assert_eq!(input.text(), "G1 X");
    }

    #[test]
    fn emacs_kills_and_yank() {
        let mut input = typed("G1 X10 Y20");
        input.handle_key(ctrl('w'));
        assert_eq!(input.text(), "G1 X10 ");
        input.handle_key(ctrl('w'));
        assert_eq!(input.text(), "G1 ");
        input.handle_key(ctrl('y'));
        assert_eq!(input.text(), "G1 X10 ");
        input.handle_key(ctrl('a'));
        input.handle_key(alt('f'));
        assert_eq!(input.cursor(), 2);
        input.handle_key(ctrl('k'));
        assert_eq!(input.text(), "G1");
        input.handle_key(ctrl('y'));
        assert_eq!(input.text(), "G1 X10 ");
        input.handle_key(alt('b'));
        assert_eq!(input.cursor(), 3);
        input.handle_key(ctrl('u'));
        assert_eq!(input.text(), "X10 ");
        assert_eq!(input.cursor(), 0);
        input.handle_key(alt('d'));
        assert_eq!(input.text(), " ");
        let mut input = typed("M104 S200");
        input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(input.text(), "M104 ");
        input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(input.cursor(), 0);
        input.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn multibyte_text_is_edited_by_character() {
        let mut input = typed("M117 héllo 日本");
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.text(), "M117 héllo 日");
        for _ in 0..6 {
            input.handle_key(key(KeyCode::Left));
        }
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.text(), "M117 éllo 日");
        assert_eq!(input.cursor_column(), 5);
        input.handle_key(key(KeyCode::Delete));
        assert_eq!(input.text(), "M117 llo 日");
    }

    #[test]
    fn history_navigation_keeps_the_draft() {
        let mut input = CommandInput::new();
        for cmd in ["G28", "M105", "M105", "G1 X10"] {
            input.set_text(cmd);
            input.handle_key(key(KeyCode::Enter));
        }
        // Consecutive duplicates are stored once.
        assert_eq!(input.history(), ["G28", "M105", "G1 X10"]);
        input.set_text("draft");
        input.handle_key(key(KeyCode::Up));
        assert_eq!(input.text(), "G1 X10");
        input.handle_key(ctrl('p'));
        input.handle_key(key(KeyCode::Up));
        assert_eq!(input.text(), "G28");
        input.handle_key(key(KeyCode::Up));
        assert_eq!(input.text(), "G28");
        input.handle_key(key(KeyCode::Down));
        assert_eq!(input.text(), "M105");
        input.handle_key(ctrl('n'));
        input.handle_key(key(KeyCode::Down));
        assert_eq!(input.text(), "draft");
        input.handle_key(key(KeyCode::Down));
        assert_eq!(input.text(), "draft");
        // Empty history: arrows do nothing.
        let mut empty = typed("x");
        empty.handle_key(key(KeyCode::Up));
        assert_eq!(empty.text(), "x");
    }

    #[test]
    fn history_limits_and_loading() {
        let mut input = CommandInput::new();
        input.set_max_history(2);
        for cmd in ["a", "b", "c"] {
            input.push_history(cmd);
        }
        assert_eq!(input.history(), ["b", "c"]);
        input.set_history(["  x\n", "", "y"].map(String::from));
        assert_eq!(input.history(), ["x", "y"]);
        input.set_max_history(0);
        assert_eq!(input.history(), ["y"]);
    }

    #[test]
    fn pasted_control_characters_become_spaces() {
        let mut input = typed("M117 ");
        input.insert_str("two\r\nlines\t\u{1b}x");
        assert_eq!(input.text(), "M117 two  lines  x");
        input.insert_str("");
        input.insert_char('\u{7}');
        assert_eq!(input.text(), "M117 two  lines  x");
        input.set_text("a\nb");
        assert_eq!(input.text(), "a b");
    }

    #[test]
    fn tab_with_one_match_completes_directly() {
        let mut input = typed("m999");
        assert_eq!(sprint_proto::search_gcode("m999").len(), 1);
        assert_eq!(input.handle_key(key(KeyCode::Tab)), InputEvent::Handled);
        assert_eq!(input.text(), "M999 ");
        assert!(!input.is_completing());
        // The word under the cursor is replaced, the rest of the line kept.
        let mut input = CommandInput::new().with_completer(|w| {
            vec![Candidate {
                insert: format!("{}OK", w.to_uppercase()),
                description: "d".into(),
                dangerous: false,
            }]
        });
        input.set_text("ab X1");
        input.handle_key(key(KeyCode::Home));
        input.handle_key(key(KeyCode::Right));
        input.handle_key(key(KeyCode::Tab));
        assert_eq!(input.text(), "ABOK X1");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn tab_with_several_matches_opens_the_popup() {
        let mut input = typed("M10");
        assert_eq!(input.handle_key(key(KeyCode::Tab)), InputEvent::Handled);
        let (candidates, selected) = input.completion().expect("popup");
        assert!(candidates.len() > 1);
        assert!(candidates.iter().all(|c| c.insert.starts_with("M10")));
        assert!(candidates.iter().any(|c| c.insert == "M104"));
        assert_eq!(selected, 0);
        input.handle_key(key(KeyCode::Tab));
        input.handle_key(key(KeyCode::Down));
        assert_eq!(input.completion().map(|c| c.1), Some(2));
        input.handle_key(key(KeyCode::BackTab));
        input.handle_key(key(KeyCode::Up));
        input.handle_key(key(KeyCode::Up));
        // Wraps around.
        assert_eq!(
            input.completion().map(|c| c.1),
            Some(candidates_len(&input) - 1)
        );
        input.handle_key(key(KeyCode::Down));
        input.handle_key(key(KeyCode::Down));
        let chosen = input.completion().map(|(c, i)| c[i].insert.clone());
        assert_eq!(input.handle_key(key(KeyCode::Enter)), InputEvent::Handled);
        assert!(!input.is_completing());
        let chosen = chosen.expect("candidate");
        assert_eq!(input.text(), format!("{chosen} "));
        // Esc closes without changes.
        let mut input = typed("G2");
        input.handle_key(key(KeyCode::Tab));
        assert!(input.is_completing());
        assert_eq!(input.handle_key(key(KeyCode::Esc)), InputEvent::Handled);
        assert!(!input.is_completing());
        assert_eq!(input.text(), "G2");
    }

    fn candidates_len(input: &CommandInput) -> usize {
        input.completion().map_or(0, |(c, _)| c.len())
    }

    #[test]
    fn typing_refreshes_or_closes_the_popup() {
        let mut input = typed("M1");
        input.handle_key(key(KeyCode::Tab));
        let before = candidates_len(&input);
        input.handle_key(key(KeyCode::Char('0')));
        assert!(input.is_completing());
        assert!(candidates_len(&input) < before);
        assert!(
            input
                .completion()
                .is_some_and(|(c, _)| c.iter().all(|c| c.insert.starts_with("M10")))
        );
        // No candidates left: closed.
        input.handle_key(key(KeyCode::Char('Q')));
        assert!(!input.is_completing());
        // Cursor movement closes it too.
        let mut input = typed("G1");
        input.handle_key(key(KeyCode::Tab));
        assert!(input.is_completing());
        input.handle_key(key(KeyCode::Left));
        assert!(!input.is_completing());
    }

    #[test]
    fn word_queries_complete_by_name() {
        let mut input = typed("temp");
        input.handle_key(key(KeyCode::Tab));
        let (candidates, _) = input.completion().expect("popup");
        assert!(
            candidates.iter().any(|c| c.insert == "M104"),
            "{candidates:?}"
        );
    }

    #[test]
    fn tab_outside_the_command_word_is_ignored() {
        let mut empty = CommandInput::new();
        assert_eq!(empty.handle_key(key(KeyCode::Tab)), InputEvent::Ignored);
        let mut input = typed("G1 X");
        assert_eq!(input.handle_key(key(KeyCode::Tab)), InputEvent::Ignored);
        let mut nothing = typed("Q99999");
        assert_eq!(nothing.handle_key(key(KeyCode::Tab)), InputEvent::Handled);
        assert!(!nothing.is_completing());
        // Leading spaces are skipped; accepting keeps the rest of the line.
        let mut input = CommandInput::new().with_completer(|_| {
            vec![
                Candidate {
                    insert: "M104".into(),
                    ..Default::default()
                },
                Candidate {
                    insert: "M109".into(),
                    ..Default::default()
                },
            ]
        });
        input.set_text("  m10 S200");
        input.handle_key(key(KeyCode::Home));
        input.handle_key(key(KeyCode::Right));
        input.handle_key(key(KeyCode::Right));
        input.handle_key(key(KeyCode::Right));
        input.handle_key(key(KeyCode::Tab));
        input.handle_key(key(KeyCode::Tab));
        input.handle_key(key(KeyCode::Enter));
        assert_eq!(input.text(), "  M109 S200");
        assert_eq!(input.cursor(), 7);
    }

    fn draw(line: CommandLine<'_>, width: u16, height: u16) -> (Vec<String>, Option<Position>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut cursor = None;
        terminal
            .draw(|frame| {
                cursor = line.cursor_position(frame.area());
                frame.render_widget(line, frame.area());
            })
            .expect("draw");
        (lines(terminal.backend().buffer()), cursor)
    }

    #[test]
    fn command_line_renders_prompt_text_and_cursor() {
        let input = typed("G28");
        let (rows, cursor) = draw(CommandLine::new(&input), 12, 1);
        assert_eq!(rows, vec!["> G28       "]);
        assert_eq!(cursor, Some(Position::new(5, 0)));
        let empty = CommandInput::new();
        let (rows, cursor) = draw(CommandLine::new(&empty).placeholder("Type G-code"), 20, 1);
        assert_eq!(rows[0].trim_end(), "> Type G-code");
        assert_eq!(cursor, Some(Position::new(2, 0)));
        let (_, cursor) = draw(CommandLine::new(&input).focused(false), 12, 1);
        assert_eq!(cursor, None);
        let (rows, cursor) = draw(
            CommandLine::new(&input)
                .prompt("sprint> ")
                .block(Block::bordered()),
            20,
            3,
        );
        assert_eq!(rows[1], "│sprint> G28       │");
        assert_eq!(cursor, Some(Position::new(12, 1)));
    }

    #[test]
    fn long_text_scrolls_to_keep_the_cursor_visible() {
        let input = typed("G1 X100 Y100 Z0.3 E12.5 F3000");
        let (rows, cursor) = draw(CommandLine::new(&input), 12, 1);
        // 10 text columns; the cursor sits after the last character.
        assert_eq!(rows[0], "> 2.5 F3000 ");
        assert_eq!(cursor, Some(Position::new(11, 0)));
        let mut input = input;
        input.handle_key(key(KeyCode::Home));
        let (rows, cursor) = draw(CommandLine::new(&input), 12, 1);
        assert_eq!(rows[0], "> G1 X100 Y1");
        assert_eq!(cursor, Some(Position::new(2, 0)));
        // Moving within the view does not scroll.
        for _ in 0..3 {
            input.handle_key(key(KeyCode::Right));
        }
        let (rows, cursor) = draw(CommandLine::new(&input), 12, 1);
        assert_eq!(rows[0], "> G1 X100 Y1");
        assert_eq!(cursor, Some(Position::new(5, 0)));
        // Moving past the right edge scrolls by as little as needed.
        for _ in 0..8 {
            input.handle_key(key(KeyCode::Right));
        }
        let (rows, cursor) = draw(CommandLine::new(&input), 12, 1);
        assert_eq!(rows[0], ">  X100 Y100");
        assert_eq!(cursor, Some(Position::new(11, 0)));
    }

    #[test]
    fn wide_characters_cut_at_the_left_edge_keep_the_text_in_place() {
        let input = typed("M117 日本語日本語日本語");
        let (rows, cursor) = draw(CommandLine::new(&input), 12, 1);
        // The visible half of the cut character is blank, so the text ends
        // right before the cursor.
        assert_eq!(rows[0], ">  語日本語 ");
        assert_eq!(cursor, Some(Position::new(11, 0)));
        assert_eq!(visible_slice("日本", 1, 1), " ");
        assert_eq!(visible_slice("日本", 1, 3), " 本");
        assert_eq!(visible_slice("日本", 0, 3), "日");
    }

    #[test]
    fn tiny_command_lines_do_not_panic() {
        let input = typed("M105");
        for (w, h) in [(0, 0), (1, 1), (2, 1), (3, 1)] {
            let _ = draw(CommandLine::new(&input), w, h);
        }
        let (_, cursor) = draw(CommandLine::new(&input), 2, 1);
        assert!(cursor.is_some_and(|c| c.x < 2));
    }

    #[test]
    fn popup_area_prefers_above_then_below() {
        let mut input = typed("M1");
        let bounds = Rect::new(0, 0, 80, 24);
        assert_eq!(
            CompletionPopup::new(&input).area(Rect::new(0, 23, 80, 1), bounds),
            None
        );
        input.handle_key(key(KeyCode::Tab));
        let popup = CompletionPopup::new(&input);
        let above = popup.area(Rect::new(0, 23, 80, 1), bounds).expect("area");
        assert_eq!(above.bottom(), 23);
        assert_eq!(above.height, 10);
        let below = popup.area(Rect::new(0, 0, 80, 1), bounds).expect("area");
        assert_eq!(below.y, 1);
        // Squeezed: uses the larger side, cut.
        let squeezed = popup
            .area(Rect::new(0, 5, 80, 1), Rect::new(0, 0, 80, 10))
            .expect("area");
        assert_eq!((squeezed.y, squeezed.height), (0, 5));
        assert!(
            popup
                .area(Rect::new(0, 1, 80, 1), Rect::new(0, 0, 80, 3))
                .is_none()
        );
        // Kept inside narrow bounds.
        let narrow = popup
            .area(Rect::new(70, 23, 10, 1), Rect::new(0, 0, 40, 24))
            .expect("area");
        assert!(narrow.right() <= 40);
    }

    #[test]
    fn popup_lists_codes_and_names() {
        let theme = Theme::dark();
        let mut input = CommandInput::new().with_completer(|_| {
            vec![
                Candidate {
                    insert: "M500".into(),
                    description: "Save settings".into(),
                    dangerous: false,
                },
                Candidate {
                    insert: "M502".into(),
                    description: "Factory reset".into(),
                    dangerous: true,
                },
                Candidate {
                    insert: "M5".into(),
                    description: "Spindle off".into(),
                    dangerous: false,
                },
            ]
        });
        input.set_text("M5");
        input.handle_key(key(KeyCode::Tab));
        input.handle_key(key(KeyCode::Tab));
        let popup = CompletionPopup::new(&input).theme(&theme);
        let area = popup
            .area(Rect::new(0, 9, 40, 1), Rect::new(0, 0, 40, 10))
            .expect("area");
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget(popup, area))
            .expect("draw");
        let buf = terminal.backend().buffer();
        let rows = lines(buf);
        let top = usize::from(area.y);
        assert!(rows[top + 1].contains("  M500  Save settings"), "{rows:?}");
        assert!(rows[top + 2].contains("! M502  Factory reset"), "{rows:?}");
        assert!(rows[top + 3].contains("  M5    Spindle off"), "{rows:?}");
        assert!(rows[top + 4].contains("2/3"), "{rows:?}");
        // The selected row is highlighted.
        let y = area.y + 2;
        assert_eq!(buf[(area.x + 4, y)].bg, theme.accent);
        assert_ne!(buf[(area.x + 4, y - 1)].bg, theme.accent);
    }

    #[test]
    fn popup_scrolls_to_the_selection() {
        let mut input = CommandInput::new().with_completer(|_| {
            (0..20)
                .map(|i| Candidate {
                    insert: format!("C{i}"),
                    description: String::new(),
                    dangerous: false,
                })
                .collect()
        });
        input.set_text("C");
        input.handle_key(key(KeyCode::Tab));
        for _ in 0..12 {
            input.handle_key(key(KeyCode::Down));
        }
        let popup = CompletionPopup::new(&input).max_rows(4);
        let area = popup
            .area(Rect::new(0, 10, 30, 1), Rect::new(0, 0, 30, 11))
            .expect("area");
        assert_eq!(area.height, 6);
        let mut terminal = Terminal::new(TestBackend::new(30, 11)).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget(popup, area))
            .expect("draw");
        let text = lines(terminal.backend().buffer()).join("\n");
        assert!(text.contains("C12"), "{text}");
        assert!(!text.contains("C8 "), "{text}");
        assert!(text.contains("13/20"), "{text}");
        // Rendering without an open popup draws nothing.
        let closed = CommandInput::new();
        let mut terminal = Terminal::new(TestBackend::new(10, 4)).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget(CompletionPopup::new(&closed), frame.area()))
            .expect("draw");
        assert!(
            lines(terminal.backend().buffer())
                .iter()
                .all(|r| r.trim().is_empty())
        );
    }
}
