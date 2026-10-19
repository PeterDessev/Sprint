//! Console lines and the scrollable console view.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, StatefulWidget, Widget};

use crate::theme::{Theme, default_theme};
use crate::widgets::util::{self, sanitize};

/// What a console line is.
///
/// Mirrors the host's console kinds so applications can map them one to
/// one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ConsoleKind {
    /// A command sent to the printer.
    Sent,
    /// A line received from the printer.
    #[default]
    Received,
    /// A message from the host itself.
    Info,
    /// A host or firmware warning.
    Warning,
    /// A host or firmware error.
    Error,
}

impl ConsoleKind {
    /// Two-character tag shown before the text, so kinds stay
    /// distinguishable without colors.
    pub fn tag(self) -> &'static str {
        match self {
            ConsoleKind::Sent => "> ",
            ConsoleKind::Received => "< ",
            ConsoleKind::Info => "* ",
            ConsoleKind::Warning => "! ",
            ConsoleKind::Error => "!!",
        }
    }
}

/// One line of console output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsoleLine {
    /// What the line is.
    pub kind: ConsoleKind,
    /// The text, without line ending. Control characters are made visible
    /// when rendering.
    pub text: String,
    /// Automatic traffic (temperature polling, auto-reports and their
    /// `ok`s), hidden unless the view shows it.
    pub auto: bool,
    /// A line of a streamed print (the file's lines sent and the `ok`s
    /// answering them), hidden unless the view shows it.
    pub stream: bool,
}

impl ConsoleLine {
    /// A line of the given kind.
    pub fn new(kind: ConsoleKind, text: impl Into<String>) -> ConsoleLine {
        ConsoleLine {
            kind,
            text: text.into(),
            auto: false,
            stream: false,
        }
    }

    /// A command sent to the printer.
    pub fn sent(text: impl Into<String>) -> ConsoleLine {
        ConsoleLine::new(ConsoleKind::Sent, text)
    }

    /// A line received from the printer.
    pub fn received(text: impl Into<String>) -> ConsoleLine {
        ConsoleLine::new(ConsoleKind::Received, text)
    }

    /// A host message.
    pub fn info(text: impl Into<String>) -> ConsoleLine {
        ConsoleLine::new(ConsoleKind::Info, text)
    }

    /// A warning.
    pub fn warning(text: impl Into<String>) -> ConsoleLine {
        ConsoleLine::new(ConsoleKind::Warning, text)
    }

    /// An error.
    pub fn error(text: impl Into<String>) -> ConsoleLine {
        ConsoleLine::new(ConsoleKind::Error, text)
    }

    /// The same line marked as automatic traffic (or not).
    #[must_use]
    pub fn with_auto(mut self, auto: bool) -> ConsoleLine {
        self.auto = auto;
        self
    }

    /// The same line marked as part of a streamed print (or not).
    #[must_use]
    pub fn with_stream(mut self, stream: bool) -> ConsoleLine {
        self.stream = stream;
        self
    }
}

/// Lines a mouse wheel notch scrolls.
const WHEEL_LINES: usize = 3;

/// Scroll position of a [`ConsoleView`].
///
/// The view either *follows* the newest line (the default) or stays where
/// the user scrolled to: on the same lines while new ones arrive and when
/// the automatic-traffic filter changes. When the application drops old
/// lines from the front of its log, it calls
/// [`lines_removed`](ConsoleState::lines_removed) so the view stays put
/// then too. Scrolling back down to the newest line resumes following.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleState {
    /// Shown lines between the newest visible line and the newest line.
    offset: usize,
    follow: bool,
    /// Log index of the newest visible line at the last render, while
    /// scrolled back: the next render keeps that line at the bottom.
    anchor: Option<usize>,
    /// `offset` as of the last render; scrolling since then moves the view
    /// by the difference.
    rendered_offset: usize,
    /// Lines removed from the front of the log since the last render.
    removed: usize,
    /// Number of shown lines at the last render.
    last_count: Option<usize>,
    /// Text rows at the last render (page size).
    last_height: usize,
}

impl Default for ConsoleState {
    fn default() -> Self {
        ConsoleState::new()
    }
}

impl ConsoleState {
    /// A state that follows the newest line.
    pub fn new() -> ConsoleState {
        ConsoleState {
            offset: 0,
            follow: true,
            anchor: None,
            rendered_offset: 0,
            removed: 0,
            last_count: None,
            last_height: 0,
        }
    }

    /// True while the view sticks to the newest line.
    pub fn is_following(&self) -> bool {
        self.follow
    }

    /// Lines scrolled back from the newest line (0 while following).
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Jump to the newest line and follow new output.
    pub fn follow(&mut self) {
        self.offset = 0;
        self.follow = true;
    }

    /// Scroll `lines` towards older output. Stops following unless there
    /// is nothing to scroll back to.
    pub fn scroll_up(&mut self, lines: usize) {
        self.offset = self.offset.saturating_add(lines).min(self.max_offset());
        if self.offset > 0 {
            self.follow = false;
        }
    }

    /// Scroll `lines` towards newer output; reaching the newest line
    /// resumes following.
    pub fn scroll_down(&mut self, lines: usize) {
        self.offset = self.offset.saturating_sub(lines);
        if self.offset == 0 {
            self.follow = true;
        }
    }

    /// Scroll one page (the height of the last render, minus one line of
    /// context) towards older output.
    pub fn page_up(&mut self) {
        self.scroll_up(self.page());
    }

    /// Scroll one page towards newer output.
    pub fn page_down(&mut self) {
        self.scroll_down(self.page());
    }

    /// Tell the state that `count` lines were removed from the front of
    /// the log since the last render (a log trimmed at capacity), so a
    /// scrolled-back view stays on the same lines. Count every removed
    /// line, shown or filtered out.
    pub fn lines_removed(&mut self, count: usize) {
        self.removed = self.removed.saturating_add(count);
    }

    /// Jump to the oldest line.
    pub fn scroll_to_top(&mut self) {
        self.scroll_up(usize::MAX);
    }

    /// Default key bindings: PageUp/PageDown scroll a page, Shift+Up/Down
    /// a line, Ctrl+Home jumps to the oldest line and Ctrl+End (or
    /// Shift+End) back to following. Key releases are ignored. Returns
    /// true when the key was used.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::PageUp => self.page_up(),
            KeyCode::PageDown => self.page_down(),
            KeyCode::Up if shift => self.scroll_up(1),
            KeyCode::Down if shift => self.scroll_down(1),
            KeyCode::Home if ctrl => self.scroll_to_top(),
            KeyCode::End if ctrl || shift => self.follow(),
            _ => return false,
        }
        true
    }

    /// Mouse wheel scrolling. Returns true when the event was used.
    pub fn handle_mouse(&mut self, event: MouseEvent) -> bool {
        match event.kind {
            MouseEventKind::ScrollUp => self.scroll_up(WHEEL_LINES),
            MouseEventKind::ScrollDown => self.scroll_down(WHEEL_LINES),
            _ => return false,
        }
        true
    }

    fn page(&self) -> usize {
        self.last_height.saturating_sub(1).max(1)
    }

    /// Largest useful offset known from the last render (unbounded before
    /// the first render; the render clamps it).
    fn max_offset(&self) -> usize {
        match self.last_count {
            Some(count) => count.saturating_sub(self.last_height.max(1)),
            None => usize::MAX,
        }
    }

    /// Update for a render of the shown lines (their log indices, in
    /// order) on `height` rows; returns the offset to draw with.
    fn update(&mut self, shown: &[usize], height: usize) -> usize {
        let count = shown.len();
        if self.follow {
            self.offset = 0;
        } else if let Some(anchor) = self.anchor {
            // Shown lines after the anchored line, wherever it is now: new
            // lines, a changed filter and trimming leave the view in place.
            let below = match anchor.checked_sub(self.removed) {
                Some(index) => count - shown.partition_point(|&i| i <= index),
                // The anchored line itself was removed: the oldest lines.
                None => count,
            };
            // Scrolling since the last render moves on from there.
            self.offset = if self.offset >= self.rendered_offset {
                below.saturating_add(self.offset - self.rendered_offset)
            } else {
                below.saturating_sub(self.rendered_offset - self.offset)
            };
        }
        self.removed = 0;
        self.last_count = Some(count);
        self.last_height = height;
        self.offset = self.offset.min(self.max_offset());
        if self.offset == 0 {
            // Back at the newest line (or everything fits): follow again.
            self.follow = true;
        }
        self.anchor = if self.follow {
            None
        } else {
            count
                .checked_sub(self.offset + 1)
                .and_then(|i| shown.get(i))
                .copied()
        };
        self.rendered_offset = self.offset;
        self.offset
    }
}

/// Byte ranges of the ASCII-case-insensitive matches of `query` in `text`
/// (non-overlapping, left to right). Empty for an empty query.
pub fn find_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle = query.as_bytes();
    let hay = text.as_bytes();
    let mut out = Vec::new();
    if needle.is_empty() || needle.len() > hay.len() {
        return out;
    }
    let mut i = 0;
    while let Some(window) = hay.get(i..i + needle.len()) {
        // ASCII folding never touches UTF-8 lead or continuation bytes, so
        // a match of a whole UTF-8 query starts and ends on char
        // boundaries; the check below is belt and braces.
        if window.eq_ignore_ascii_case(needle)
            && text.is_char_boundary(i)
            && text.is_char_boundary(i + needle.len())
        {
            out.push(i..i + needle.len());
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// A scrollable console: one row per line, newest at the bottom.
///
/// * Automatic traffic (`auto` lines: temperature polling and its `ok`s)
///   is hidden unless [`show_auto`](ConsoleView::show_auto) is set; shown
///   automatic lines are dimmed. So are the lines of a streamed print
///   (`stream`: thousands of file lines and their `ok`s), unless
///   [`show_stream`](ConsoleView::show_stream) is set.
/// * Lines are colored by kind; without colors (or with
///   [`show_tags`](ConsoleView::show_tags)) a two-character tag (`> `,
///   `< `, `* `, `! `, `!!`) marks the kind.
/// * Matches of the [`search`](ConsoleView::search) text are highlighted.
/// * Lines wider than the view are cut with `…`; control characters from
///   the printer are made visible instead of reaching the terminal.
/// * While scrolled back, the bottom row shows how many newer lines there
///   are.
///
/// Use it as a [`StatefulWidget`] with a [`ConsoleState`]; as a plain
/// [`Widget`] it always shows the newest lines.
#[derive(Clone, Debug)]
pub struct ConsoleView<'a> {
    lines: &'a [ConsoleLine],
    show_auto: bool,
    show_stream: bool,
    search: Option<&'a str>,
    show_tags: bool,
    block: Option<Block<'a>>,
    theme: &'a Theme,
}

impl<'a> ConsoleView<'a> {
    /// A view of `lines`, oldest first.
    pub fn new(lines: &'a [ConsoleLine]) -> ConsoleView<'a> {
        ConsoleView {
            lines,
            show_auto: false,
            show_stream: false,
            search: None,
            show_tags: false,
            block: None,
            theme: default_theme(),
        }
    }

    /// Show automatic traffic (default false).
    #[must_use]
    pub fn show_auto(mut self, show: bool) -> Self {
        self.show_auto = show;
        self
    }

    /// Show the lines of a streamed print (default false).
    #[must_use]
    pub fn show_stream(mut self, show: bool) -> Self {
        self.show_stream = show;
        self
    }

    /// Highlight matches of this text (ASCII case-insensitive); `None` or
    /// an empty string highlights nothing.
    #[must_use]
    pub fn search(mut self, query: Option<&'a str>) -> Self {
        self.search = query.filter(|q| !q.is_empty());
        self
    }

    /// Prefix lines with their kind tag even with colors (default false;
    /// tags are always shown without colors).
    #[must_use]
    pub fn show_tags(mut self, show: bool) -> Self {
        self.show_tags = show;
        self
    }

    /// Surround the console with a block.
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

    /// True when `line` passes the automatic-traffic and stream filters.
    fn shows(&self, line: &ConsoleLine) -> bool {
        (self.show_auto || !line.auto) && (self.show_stream || !line.stream)
    }

    /// Number of lines that pass the filter.
    pub fn visible_count(&self) -> usize {
        self.lines.iter().filter(|l| self.shows(l)).count()
    }

    /// Number of shown lines containing the search text.
    pub fn match_count(&self) -> usize {
        let Some(query) = self.search else { return 0 };
        self.lines
            .iter()
            .filter(|l| self.shows(l) && !find_matches(&sanitize(&l.text), query).is_empty())
            .count()
    }

    /// The spans of one line and its width in columns.
    fn line_spans(&self, line: &ConsoleLine) -> (Vec<Span<'static>>, usize) {
        let theme = self.theme;
        let mut style = theme.console(line.kind);
        if line.auto || line.stream {
            style = style.patch(theme.dim());
        }
        let mut spans = Vec::new();
        let mut cols = 0;
        if self.show_tags || !theme.has_color() {
            let tag = line.kind.tag();
            spans.push(Span::styled(format!("{tag} "), style));
            cols += util::width(tag) + 1;
        }
        let text = sanitize(&line.text);
        cols += util::width(&text);
        let matches = self
            .search
            .map(|q| find_matches(&text, q))
            .unwrap_or_default();
        let mut pos = 0;
        for m in matches {
            if let Some(before) = text.get(pos..m.start).filter(|s| !s.is_empty()) {
                spans.push(Span::styled(before.to_string(), style));
            }
            if let Some(hit) = text.get(m.clone()) {
                spans.push(Span::styled(hit.to_string(), theme.search_highlight()));
            }
            pos = m.end;
        }
        if let Some(rest) = text.get(pos..).filter(|s| !s.is_empty()) {
            spans.push(Span::styled(rest.to_string(), style));
        }
        (spans, cols)
    }

    fn render_message(&self, area: Rect, buf: &mut Buffer, text: &str) {
        let y = area.y + area.height / 2;
        Paragraph::new(Line::from(Span::styled(text.to_string(), self.theme.dim())))
            .alignment(Alignment::Center)
            .render(Rect::new(area.x, y, area.width, 1), buf);
    }
}

impl StatefulWidget for ConsoleView<'_> {
    type State = ConsoleState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut ConsoleState) {
        let inner = match &self.block {
            Some(block) => {
                block.render(area, buf);
                block.inner(area)
            }
            None => area,
        };
        if inner.is_empty() {
            return;
        }
        // Log indices of the lines that pass the filter.
        let shown: Vec<usize> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| self.shows(l))
            .map(|(i, _)| i)
            .collect();
        let height = usize::from(inner.height);
        let offset = state.update(&shown, height);
        if shown.is_empty() {
            let message = if self.lines.is_empty() {
                "No console output yet"
            } else if self.lines.iter().all(|l| l.auto) {
                "Only automatic traffic (hidden)"
            } else if self.lines.iter().all(|l| l.stream) {
                "Only the print's lines (hidden)"
            } else {
                "Only hidden traffic"
            };
            self.render_message(inner, buf, message);
            return;
        }
        let end = shown.len().saturating_sub(offset);
        let start = end.saturating_sub(height);
        let width = usize::from(inner.width);
        let visible = shown.get(start..end).unwrap_or(&[]);
        for (row, line) in visible
            .iter()
            .filter_map(|&i| self.lines.get(i))
            .enumerate()
        {
            let Ok(dy) = u16::try_from(row) else { break };
            let y = inner.y + dy;
            let (spans, cols) = self.line_spans(line);
            if cols > width {
                // Keep the last column free for the mark: written over the
                // hidden half of a wide character it would not show.
                buf.set_line(inner.x, y, &Line::from(spans), inner.width - 1);
                buf.set_string(inner.right() - 1, y, "…", self.theme.dim());
            } else {
                buf.set_line(inner.x, y, &Line::from(spans), inner.width);
            }
        }
        if offset > 0 {
            let note = format!(" ↓ {offset} newer ");
            let cols = u16::try_from(util::width(&note)).unwrap_or(u16::MAX);
            if cols <= inner.width {
                let x = inner.right() - cols;
                buf.set_string(x, inner.bottom() - 1, note, self.theme.selected());
            }
        }
    }
}

impl Widget for ConsoleView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        StatefulWidget::render(self, area, buf, &mut ConsoleState::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testutil::{lines, render, render_stateful};

    fn numbered(n: usize) -> Vec<ConsoleLine> {
        (1..=n)
            .map(|i| ConsoleLine::received(format!("line {i}")))
            .collect()
    }

    #[test]
    fn shows_newest_lines_while_following() {
        let log = numbered(10);
        let buf = render(ConsoleView::new(&log), 12, 3);
        assert_eq!(
            lines(&buf),
            vec!["line 8      ", "line 9      ", "line 10     "]
        );
    }

    #[test]
    fn few_lines_start_at_the_top() {
        let log = numbered(2);
        let buf = render(ConsoleView::new(&log), 8, 4);
        assert_eq!(lines(&buf)[0], "line 1  ");
        assert_eq!(lines(&buf)[1], "line 2  ");
        assert_eq!(lines(&buf)[3], "        ");
    }

    #[test]
    fn auto_traffic_is_filtered() {
        let log = vec![
            ConsoleLine::sent("M105").with_auto(true),
            ConsoleLine::received("ok T:20.0 /0.0").with_auto(true),
            ConsoleLine::sent("G28"),
            ConsoleLine::received("ok"),
        ];
        let view = ConsoleView::new(&log);
        assert_eq!(view.visible_count(), 2);
        let buf = render(view, 20, 4);
        assert_eq!(lines(&buf)[0].trim_end(), "G28");
        assert_eq!(lines(&buf)[1].trim_end(), "ok");
        let theme = Theme::dark();
        let buf = render(ConsoleView::new(&log).show_auto(true).theme(&theme), 20, 4);
        assert_eq!(lines(&buf)[0].trim_end(), "M105");
        // Shown automatic lines are dimmed.
        assert_eq!(buf[(0, 0)].fg, theme.text_dim);
        assert_eq!(buf[(0, 2)].fg, theme.sent);
    }

    #[test]
    fn stream_lines_are_filtered() {
        let log = vec![
            ConsoleLine::sent("G1 X10 E0.2").with_stream(true),
            ConsoleLine::received("ok").with_stream(true),
            ConsoleLine::sent("M220 S110"),
            ConsoleLine::received("ok"),
            ConsoleLine::sent("M105").with_auto(true),
        ];
        let view = ConsoleView::new(&log);
        assert_eq!(view.visible_count(), 2);
        let buf = render(view, 20, 4);
        assert_eq!(lines(&buf)[0].trim_end(), "M220 S110");
        let theme = Theme::dark();
        let view = ConsoleView::new(&log).show_stream(true).theme(&theme);
        assert_eq!(view.visible_count(), 4);
        let buf = render(view, 20, 4);
        assert_eq!(lines(&buf)[0].trim_end(), "G1 X10 E0.2");
        // Shown stream lines are dimmed, like automatic traffic.
        assert_eq!(buf[(0, 0)].fg, theme.text_dim);
        assert_eq!(buf[(0, 2)].fg, theme.sent);
        // Both filters are independent.
        let all = ConsoleView::new(&log).show_stream(true).show_auto(true);
        assert_eq!(all.visible_count(), 5);
        let only_stream = vec![ConsoleLine::sent("G1 X1").with_stream(true)];
        let buf = render(ConsoleView::new(&only_stream), 34, 3);
        assert!(lines(&buf)[1].contains("Only the print's lines"));
        let mixed = vec![
            ConsoleLine::sent("G1 X1").with_stream(true),
            ConsoleLine::sent("M105").with_auto(true),
        ];
        let buf = render(ConsoleView::new(&mixed), 34, 3);
        assert!(lines(&buf)[1].contains("Only hidden traffic"));
    }

    #[test]
    fn empty_and_all_hidden_messages() {
        let buf = render(ConsoleView::new(&[]), 30, 3);
        assert!(lines(&buf)[1].contains("No console output yet"));
        let log = vec![ConsoleLine::received("ok").with_auto(true)];
        let buf = render(ConsoleView::new(&log), 34, 3);
        assert!(lines(&buf)[1].contains("Only automatic traffic"));
    }

    #[test]
    fn kind_colors_and_tags() {
        let theme = Theme::dark();
        let log = vec![
            ConsoleLine::sent("G28"),
            ConsoleLine::error("Error:Printer halted"),
            ConsoleLine::warning("timeout"),
            ConsoleLine::info("Connected"),
        ];
        let buf = render(ConsoleView::new(&log).theme(&theme), 24, 4);
        assert_eq!(buf[(0, 0)].fg, theme.sent);
        assert_eq!(buf[(0, 1)].fg, theme.error);
        assert_eq!(buf[(0, 2)].fg, theme.warning);
        assert_eq!(buf[(0, 3)].fg, theme.info);
        let buf = render(ConsoleView::new(&log).theme(&theme).show_tags(true), 24, 4);
        assert_eq!(lines(&buf)[0].trim_end(), ">  G28");
        assert_eq!(lines(&buf)[1].trim_end(), "!! Error:Printer halted");
        // Monochrome always tags.
        let mono = Theme::monochrome();
        let buf = render(ConsoleView::new(&log).theme(&mono), 24, 4);
        assert_eq!(lines(&buf)[2].trim_end(), "!  timeout");
    }

    #[test]
    fn search_highlights_every_match() {
        let theme = Theme::dark();
        let log = vec![
            ConsoleLine::received("ok T:20 t:30"),
            ConsoleLine::sent("G1"),
        ];
        let view = ConsoleView::new(&log).search(Some("T:")).theme(&theme);
        assert_eq!(view.match_count(), 1);
        let buf = render(view, 20, 2);
        let hl = theme.search_highlight().bg.expect("highlight background");
        assert_eq!(buf[(3, 0)].bg, hl);
        assert_eq!(buf[(4, 0)].bg, hl);
        assert_ne!(buf[(5, 0)].bg, hl);
        assert_eq!(buf[(8, 0)].bg, hl);
        assert_ne!(buf[(0, 1)].bg, hl);
        // An empty query highlights nothing.
        let view = ConsoleView::new(&log).search(Some(""));
        assert_eq!(view.match_count(), 0);
    }

    #[test]
    fn matching_is_ascii_case_insensitive_and_utf8_safe() {
        assert_eq!(find_matches("ok T:20 t:30", "t:"), vec![3..5, 8..10]);
        assert_eq!(find_matches("aaaa", "aa"), vec![0..2, 2..4]);
        assert!(find_matches("abc", "").is_empty());
        assert!(find_matches("ab", "abc").is_empty());
        assert_eq!(find_matches("20°C 30°C", "°c"), vec![2..5, 8..11]);
        assert_eq!(find_matches("日本語", "本"), vec![3..6]);
    }

    #[test]
    fn long_lines_are_cut_and_controls_sanitized() {
        let log = vec![ConsoleLine::received(
            "echo:\u{1b}[2Jhello world, a long line",
        )];
        let buf = render(ConsoleView::new(&log), 16, 1);
        assert_eq!(lines(&buf)[0], "echo:·[2Jhello …");
    }

    #[test]
    fn the_cut_mark_shows_after_wide_characters() {
        let log = vec![ConsoleLine::received("ab日本語日本語")];
        // 本 would end exactly at the edge and hide the mark.
        let buf = render(ConsoleView::new(&log), 6, 1);
        assert_eq!(lines(&buf)[0], "ab日 …");
        let buf = render(ConsoleView::new(&log), 7, 1);
        assert_eq!(lines(&buf)[0], "ab日本…");
        let buf = render(ConsoleView::new(&log), 1, 1);
        assert_eq!(lines(&buf)[0], "…");
    }

    #[test]
    fn scrolling_back_keeps_the_view_still() {
        let mut log = numbered(10);
        let mut state = ConsoleState::new();
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[2].trim_end(), "line 10");
        state.scroll_up(2);
        assert!(!state.is_following());
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[0].trim_end(), "line 6");
        assert!(lines(&buf)[2].contains("↓ 2 newer"), "{:?}", lines(&buf));
        // New output arrives: the same lines stay in view.
        log.extend((1..=3).map(|i| ConsoleLine::info(format!("new {i}"))));
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[0].trim_end(), "line 6");
        assert_eq!(state.offset(), 5);
        // Scrolling down to the bottom resumes following.
        state.scroll_down(100);
        assert!(state.is_following());
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[2].trim_end(), "new 3");
    }

    /// 60 lines, every other one automatic traffic.
    fn half_auto() -> Vec<ConsoleLine> {
        (0..60)
            .map(|i| {
                if i % 2 == 0 {
                    ConsoleLine::received(format!("line {i}"))
                } else {
                    ConsoleLine::received(format!("auto {i}")).with_auto(true)
                }
            })
            .collect()
    }

    #[test]
    fn toggling_the_auto_filter_keeps_the_view_in_place() {
        let log = half_auto();
        let mut state = ConsoleState::new();
        let _ = render_stateful(ConsoleView::new(&log), 20, 4, &mut state);
        state.scroll_up(5);
        let buf = render_stateful(ConsoleView::new(&log), 20, 4, &mut state);
        assert!(lines(&buf)[3].starts_with("line 48 "), "{:?}", lines(&buf));
        // Showing automatic traffic keeps the same line at the bottom.
        let buf = render_stateful(ConsoleView::new(&log).show_auto(true), 20, 4, &mut state);
        assert!(lines(&buf)[3].starts_with("line 48 "), "{:?}", lines(&buf));
        assert_eq!(state.offset(), 11);
        // Scrolling now counts automatic lines too.
        state.scroll_up(1);
        let buf = render_stateful(ConsoleView::new(&log).show_auto(true), 20, 4, &mut state);
        assert!(lines(&buf)[3].starts_with("auto 47 "), "{:?}", lines(&buf));
        // Hiding it again: the nearest shown line above stays at the bottom.
        let buf = render_stateful(ConsoleView::new(&log), 20, 4, &mut state);
        assert!(lines(&buf)[3].starts_with("line 46 "), "{:?}", lines(&buf));
        assert_eq!(state.offset(), 6);
    }

    #[test]
    fn trimmed_logs_keep_the_view_in_place() {
        let mut log = numbered(20);
        let mut state = ConsoleState::new();
        let _ = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        state.scroll_up(4);
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[0].trim_end(), "line 14");
        // A full log: three new lines push the three oldest out.
        log.drain(..3);
        log.extend((1..=3).map(|i| ConsoleLine::info(format!("new {i}"))));
        state.lines_removed(3);
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[0].trim_end(), "line 14");
        assert_eq!(state.offset(), 7);
        // Scrolling between renders moves on from there.
        log.push(ConsoleLine::info("new 4"));
        state.scroll_down(2);
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[0].trim_end(), "line 16");
        assert_eq!(state.offset(), 6);
        // The lines in view are trimmed away: the view shows the oldest.
        log.drain(..16);
        state.lines_removed(16);
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[0].trim_end(), "line 20");
        assert!(!state.is_following());
        // Removals while following change nothing.
        state.follow();
        state.lines_removed(1000);
        let buf = render_stateful(ConsoleView::new(&log), 20, 3, &mut state);
        assert_eq!(lines(&buf)[2].trim_end(), "new 4");
    }

    #[test]
    fn scroll_limits_and_pages() {
        let log = numbered(10);
        let mut state = ConsoleState::new();
        let _ = render_stateful(ConsoleView::new(&log), 20, 4, &mut state);
        state.scroll_to_top();
        assert_eq!(state.offset(), 6);
        let buf = render_stateful(ConsoleView::new(&log), 20, 4, &mut state);
        assert_eq!(lines(&buf)[0].trim_end(), "line 1");
        state.page_down();
        assert_eq!(state.offset(), 3);
        state.page_up();
        assert_eq!(state.offset(), 6);
        state.follow();
        assert!(state.is_following());
        assert_eq!(state.offset(), 0);
        // Nothing to scroll when everything fits.
        let short = numbered(2);
        let mut state = ConsoleState::new();
        let _ = render_stateful(ConsoleView::new(&short), 20, 4, &mut state);
        state.scroll_up(3);
        assert_eq!(state.offset(), 0);
        assert!(state.is_following());
    }

    #[test]
    fn clearing_the_log_clamps_the_offset() {
        let log = numbered(50);
        let mut state = ConsoleState::new();
        let _ = render_stateful(ConsoleView::new(&log), 20, 5, &mut state);
        state.scroll_up(30);
        let short = numbered(3);
        let buf = render_stateful(ConsoleView::new(&short), 20, 5, &mut state);
        assert_eq!(lines(&buf)[0].trim_end(), "line 1");
        assert!(state.is_following());
    }

    #[test]
    fn key_and_mouse_bindings() {
        use crossterm::event::KeyEventState;
        let log = numbered(40);
        let mut state = ConsoleState::new();
        let _ = render_stateful(ConsoleView::new(&log), 20, 10, &mut state);
        assert!(state.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)));
        assert_eq!(state.offset(), 9);
        assert!(state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)));
        assert_eq!(state.offset(), 10);
        assert!(!state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
            ..KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)
        };
        assert!(!state.handle_key(release));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL)));
        assert_eq!(state.offset(), 30);
        assert!(state.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL)));
        assert!(state.is_following());
        let wheel = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert!(state.handle_mouse(wheel(MouseEventKind::ScrollUp)));
        assert_eq!(state.offset(), 3);
        assert!(state.handle_mouse(wheel(MouseEventKind::ScrollDown)));
        assert!(state.is_following());
        assert!(!state.handle_mouse(wheel(MouseEventKind::Moved)));
    }

    #[test]
    fn tiny_areas_and_blocks() {
        let log = numbered(5);
        let buf = render(ConsoleView::new(&log).block(Block::bordered()), 2, 2);
        assert_eq!(lines(&buf), vec!["┌┐", "└┘"]);
        let mut state = ConsoleState::new();
        let _ = render_stateful(ConsoleView::new(&log), 4, 1, &mut state);
        state.scroll_up(2);
        let buf = render_stateful(ConsoleView::new(&log), 4, 1, &mut state);
        // The newer-lines note does not fit and is left out.
        assert_eq!(lines(&buf)[0], "lin…");
    }

    #[test]
    fn random_traffic_keeps_a_scrolled_view_on_its_line() {
        // Deterministic xorshift, so failures reproduce.
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            usize::try_from(seed % u64::try_from(n.max(1)).unwrap_or(1)).unwrap_or(0)
        };
        const HEIGHT: usize = 4;
        let mut log: Vec<ConsoleLine> = Vec::new();
        let mut state = ConsoleState::new();
        let (mut serial, mut show_auto) = (0, false);
        // The newest visible line while scrolled back.
        let mut bottom: Option<String> = None;
        for step in 0..4000 {
            // Whether this step must leave `bottom` where it is.
            let mut keeps = false;
            match next(9) {
                0..=2 => {
                    for _ in 0..next(4) {
                        serial += 1;
                        let line = ConsoleLine::received(format!("l{serial}"));
                        log.push(line.with_auto(next(2) == 0));
                    }
                    keeps = true;
                }
                3 => state.scroll_up(next(10)),
                4 => state.scroll_down(next(10)),
                5 => show_auto = !show_auto,
                6 | 7 => {
                    let n = next(log.len() / 2 + 2).min(log.len());
                    keeps = bottom
                        .as_ref()
                        .is_some_and(|b| !log[..n].iter().any(|l| &l.text == b));
                    log.drain(..n);
                    state.lines_removed(n);
                }
                _ => state.follow(),
            }
            let view = ConsoleView::new(&log).show_auto(show_auto);
            let _ = render_stateful(view, 12, 4, &mut state);
            let shown: Vec<&ConsoleLine> = log.iter().filter(|l| show_auto || !l.auto).collect();
            let max = shown.len().saturating_sub(HEIGHT);
            assert!(state.offset() <= max, "step {step}");
            assert_eq!(state.is_following(), state.offset() == 0, "step {step}");
            let now = (!state.is_following())
                .then(|| shown[shown.len() - 1 - state.offset()].text.clone());
            // Unless pushed against the oldest line, the view stays put.
            if keeps
                && state.offset() < max
                && let (Some(before), Some(after)) = (&bottom, &now)
            {
                assert_eq!(before, after, "step {step}");
            }
            bottom = now;
        }
    }
}
