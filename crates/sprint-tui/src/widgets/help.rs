//! The key hint bar and the help overlay.

use std::borrow::Cow;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::theme::{Theme, default_theme};
use crate::widgets::util::{self, sanitize};

/// A key and what it does.
type Binding<'a> = (Cow<'a, str>, Cow<'a, str>);

/// Collect bindings from anything string-like.
fn bindings<'a, K, A>(items: impl IntoIterator<Item = (K, A)>) -> Vec<Binding<'a>>
where
    K: Into<Cow<'a, str>>,
    A: Into<Cow<'a, str>>,
{
    items
        .into_iter()
        .map(|(k, a)| (k.into(), a.into()))
        .collect()
}

/// A one-row bar of key hints: `q Quit  ? Help  Tab Complete`.
///
/// Keys are drawn in the key style, actions in normal text, on the
/// surface background. Hints that do not fit are left out from the end.
#[derive(Clone, Debug)]
pub struct KeyHints<'a> {
    hints: Vec<Binding<'a>>,
    theme: &'a Theme,
}

impl<'a> KeyHints<'a> {
    /// A bar of (key, action) pairs, in display order.
    pub fn new<K, A>(hints: impl IntoIterator<Item = (K, A)>) -> KeyHints<'a>
    where
        K: Into<Cow<'a, str>>,
        A: Into<Cow<'a, str>>,
    {
        KeyHints {
            hints: bindings(hints),
            theme: default_theme(),
        }
    }

    /// Draw with this theme.
    #[must_use]
    pub fn theme(mut self, theme: &'a Theme) -> Self {
        self.theme = theme;
        self
    }
}

impl Widget for KeyHints<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let theme = self.theme;
        let row = Rect::new(area.x, area.y, area.width, 1);
        buf.set_style(row, theme.surface());
        let width = usize::from(area.width);
        let mut spans = vec![Span::raw(" ")];
        let mut used = 1;
        for (key, action) in &self.hints {
            let key = sanitize(key);
            let action = sanitize(action);
            let sep = if used > 1 { 2 } else { 0 };
            let cols = sep + util::width(&key) + 1 + util::width(&action);
            if used + cols > width {
                break;
            }
            if sep > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(key.into_owned(), theme.key()));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(action.into_owned(), theme.text()));
            used += cols;
        }
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
    }
}

/// A titled group of key bindings in a [`HelpOverlay`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HelpSection<'a> {
    /// Section title ("Console", "Printer", ...).
    pub title: Cow<'a, str>,
    /// (key, description) pairs.
    pub entries: Vec<(Cow<'a, str>, Cow<'a, str>)>,
}

impl<'a> HelpSection<'a> {
    /// A section titled `title` with (key, description) entries.
    pub fn new<K, A>(
        title: impl Into<Cow<'a, str>>,
        entries: impl IntoIterator<Item = (K, A)>,
    ) -> HelpSection<'a>
    where
        K: Into<Cow<'a, str>>,
        A: Into<Cow<'a, str>>,
    {
        HelpSection {
            title: title.into(),
            entries: bindings(entries),
        }
    }
}

/// Narrowest key column.
const MIN_KEY_COLS: usize = 6;
/// Widest key column (longer keys are cut).
const MAX_KEY_COLS: usize = 20;
/// Widest overlay, in columns.
const MAX_WIDTH: usize = 80;

/// A centered, bordered help screen listing key bindings by section.
///
/// Render it over the whole screen area: it clears and fills only its own
/// centered box, sized to its content (at most 80 columns, and never larger
/// than the screen). Content taller than the box can be scrolled with
/// [`scroll`](HelpOverlay::scroll); the bottom border then says how many
/// lines are below.
#[derive(Clone, Debug)]
pub struct HelpOverlay<'a> {
    title: Cow<'a, str>,
    sections: &'a [HelpSection<'a>],
    footer: Option<Cow<'a, str>>,
    scroll: usize,
    theme: &'a Theme,
}

impl<'a> HelpOverlay<'a> {
    /// A help screen with these sections, titled "Help".
    pub fn new(sections: &'a [HelpSection<'a>]) -> HelpOverlay<'a> {
        HelpOverlay {
            title: Cow::Borrowed("Help"),
            sections,
            footer: Some(Cow::Borrowed("Esc close")),
            scroll: 0,
            theme: default_theme(),
        }
    }

    /// Replace the title.
    #[must_use]
    pub fn title(mut self, title: impl Into<Cow<'a, str>>) -> Self {
        self.title = title.into();
        self
    }

    /// Text in the bottom border (default "Esc close"); `None` hides it.
    #[must_use]
    pub fn footer(mut self, footer: Option<Cow<'a, str>>) -> Self {
        self.footer = footer;
        self
    }

    /// Skip this many content lines (clamped to the content).
    #[must_use]
    pub fn scroll(mut self, lines: usize) -> Self {
        self.scroll = lines;
        self
    }

    /// Draw with this theme.
    #[must_use]
    pub fn theme(mut self, theme: &'a Theme) -> Self {
        self.theme = theme;
        self
    }

    /// Width of the key column.
    fn key_cols(&self) -> usize {
        self.sections
            .iter()
            .flat_map(|s| &s.entries)
            .map(|(k, _)| util::width(&sanitize(k)))
            .max()
            .unwrap_or(0)
            .clamp(MIN_KEY_COLS, MAX_KEY_COLS)
    }

    /// Number of content lines (section titles, entries, blank lines).
    pub fn content_height(&self) -> usize {
        let entries: usize = self.sections.iter().map(|s| s.entries.len() + 1).sum();
        entries + self.sections.len().saturating_sub(1)
    }

    /// The content width wanted (without the border).
    fn content_width(&self) -> usize {
        let key_cols = self.key_cols();
        let entry = self
            .sections
            .iter()
            .flat_map(|s| &s.entries)
            .map(|(_, d)| 2 + key_cols + 2 + util::width(&sanitize(d)))
            .max()
            .unwrap_or(0);
        let titles = self
            .sections
            .iter()
            .map(|s| util::width(&sanitize(&s.title)))
            .max()
            .unwrap_or(0);
        let chrome =
            util::width(&self.title).max(self.footer.as_deref().map_or(0, util::width)) + 4;
        entry.max(titles).max(chrome) + 2
    }

    /// The box drawn on a screen of `screen`: centered, sized to the
    /// content and clamped to the screen.
    pub fn area(&self, screen: Rect) -> Rect {
        let want_w = (self.content_width() + 2).min(MAX_WIDTH);
        let want_h = self.content_height() + 2;
        let width = u16::try_from(want_w).unwrap_or(u16::MAX).min(screen.width);
        let height = u16::try_from(want_h).unwrap_or(u16::MAX).min(screen.height);
        let x = screen.x + (screen.width - width) / 2;
        let y = screen.y + (screen.height - height) / 2;
        Rect::new(x, y, width, height)
    }

    /// All content lines for an inner width of `width` columns.
    fn content(&self, width: usize) -> Vec<Line<'static>> {
        let theme = self.theme;
        let key_cols = self.key_cols();
        let mut out = Vec::with_capacity(self.content_height());
        for (i, section) in self.sections.iter().enumerate() {
            if i > 0 {
                out.push(Line::default());
            }
            let title =
                util::truncate(&sanitize(&section.title), width.saturating_sub(1)).into_owned();
            out.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(title, theme.accent()),
            ]));
            for (key, description) in &section.entries {
                let key = util::truncate(&sanitize(key), key_cols).into_owned();
                let pad = " ".repeat(key_cols.saturating_sub(util::width(&key)));
                let room = width.saturating_sub(2 + key_cols + 2 + 1);
                let description = util::truncate(&sanitize(description), room).into_owned();
                out.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(key, theme.key()),
                    Span::raw(format!("{pad}  ")),
                    Span::styled(description, theme.text()),
                ]));
            }
        }
        out
    }
}

impl Widget for HelpOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup = self.area(area);
        if popup.is_empty() {
            return;
        }
        let theme = self.theme;
        Clear.render(popup, buf);
        let inner_h = usize::from(popup.height.saturating_sub(2));
        let total = self.content_height();
        let scroll = self.scroll.min(total.saturating_sub(inner_h));
        let below = total.saturating_sub(scroll + inner_h);
        let mut block = Block::bordered()
            .border_style(theme.border(true))
            .style(theme.surface())
            .title(Line::from(Span::styled(
                format!(" {} ", sanitize(&self.title)),
                theme.title(),
            )));
        let bottom = match (&self.footer, below) {
            (_, n) if n > 0 => Some(format!(" ↓ {n} more ")),
            (Some(footer), _) => Some(format!(" {} ", sanitize(footer))),
            (None, _) => None,
        };
        if let Some(text) = bottom {
            block = block.title_bottom(Line::from(Span::styled(text, theme.dim())).right_aligned());
        }
        let inner = block.inner(popup);
        block.render(popup, buf);
        if inner.is_empty() {
            return;
        }
        let lines = self.content(usize::from(inner.width));
        for (row, line) in lines.iter().skip(scroll).take(inner_h).enumerate() {
            let Ok(dy) = u16::try_from(row) else { break };
            buf.set_line(inner.x, inner.y + dy, line, inner.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testutil::{lines, render};

    fn sections() -> Vec<HelpSection<'static>> {
        vec![
            HelpSection::new("General", [("q", "Quit"), ("?", "Toggle this help")]),
            HelpSection::new(
                "Console",
                [
                    ("PgUp/PgDn", "Scroll a page"),
                    ("Ctrl+End", "Follow new output"),
                ],
            ),
        ]
    }

    #[test]
    fn hints_fill_the_row_and_drop_what_does_not_fit() {
        let hints = KeyHints::new([("q", "Quit"), ("?", "Help"), ("Tab", "Complete")]);
        let buf = render(hints, 40, 1);
        assert_eq!(lines(&buf)[0].trim_end(), " q Quit  ? Help  Tab Complete");
        let hints = KeyHints::new([("q", "Quit"), ("?", "Help"), ("Tab", "Complete")]);
        let buf = render(hints, 18, 1);
        assert_eq!(lines(&buf)[0].trim_end(), " q Quit  ? Help");
        let theme = Theme::dark();
        let hints = KeyHints::new([("q".to_string(), "Quit".to_string())]).theme(&theme);
        let buf = render(hints, 10, 1);
        assert_eq!(buf[(1, 0)].fg, theme.accent);
        assert_eq!(buf[(3, 0)].fg, theme.text);
        // The whole row gets the bar background.
        assert_eq!(buf[(9, 0)].bg, theme.surface);
        let _ = render(KeyHints::new([("x", "y")]), 0, 0);
    }

    #[test]
    fn overlay_is_centered_and_sized_to_content() {
        let s = sections();
        let help = HelpOverlay::new(&s);
        assert_eq!(help.content_height(), 7);
        let area = help.area(Rect::new(0, 0, 80, 24));
        assert_eq!(area.height, 9);
        assert_eq!(area.x, (80 - area.width) / 2);
        assert_eq!(area.y, (24 - 9) / 2);
        let buf = render(HelpOverlay::new(&s), 80, 24);
        let rows = lines(&buf);
        let top = usize::from(area.y);
        assert!(rows[top].contains(" Help "), "{rows:?}");
        assert!(rows[top + 1].contains(" General"), "{rows:?}");
        assert!(rows[top + 2].contains("  q          Quit"), "{rows:?}");
        assert!(rows[top + 4].trim_matches(['│', ' ']).is_empty());
        assert!(
            rows[top + 6].contains("PgUp/PgDn  Scroll a page"),
            "{rows:?}"
        );
        assert!(rows[top + 8].contains("Esc close"), "{rows:?}");
        // Outside the box nothing is drawn.
        assert!(rows[0].trim().is_empty());
    }

    #[test]
    fn overlay_scrolls_when_the_screen_is_short() {
        let s = sections();
        let buf = render(HelpOverlay::new(&s), 60, 5);
        let rows = lines(&buf);
        assert!(rows[1].contains("General"));
        assert!(rows[4].contains("↓ 4 more"), "{rows:?}");
        let buf = render(HelpOverlay::new(&s).scroll(100), 60, 5);
        let rows = lines(&buf);
        assert!(rows[3].contains("Follow new output"), "{rows:?}");
        assert!(rows[4].contains("Esc close"), "{rows:?}");
        let buf = render(
            HelpOverlay::new(&s).footer(None).title("Keys").scroll(4),
            60,
            5,
        );
        let rows = lines(&buf);
        assert!(rows[0].contains("Keys"));
        assert!(!rows[4].contains("Esc"));
    }

    #[test]
    fn narrow_screens_cut_descriptions() {
        let s = vec![HelpSection::new(
            "Very long section title that goes on",
            [(
                "Ctrl+Shift+Alt+Super+X",
                "A description far too long for the box",
            )],
        )];
        let buf = render(HelpOverlay::new(&s), 30, 6);
        for row in lines(&buf) {
            assert!(util::width(&row) <= 30);
        }
        let text = lines(&buf).join("\n");
        assert!(text.contains('…'), "{text}");
        for (w, h) in [(0, 0), (1, 1), (3, 2), (10, 3)] {
            let _ = render(HelpOverlay::new(&s), w, h);
        }
        let empty: Vec<HelpSection<'static>> = Vec::new();
        assert_eq!(HelpOverlay::new(&empty).content_height(), 0);
        let _ = render(HelpOverlay::new(&empty), 20, 5);
    }
}
