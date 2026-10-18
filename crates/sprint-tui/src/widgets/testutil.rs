//! Rendering helpers for widget unit tests (TestBackend based).

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::widgets::Widget;

/// Render a widget into a `width` x `height` test terminal.
pub(crate) fn render<W: Widget>(widget: W, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| frame.render_widget(widget, frame.area()))
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// The rows of a buffer as strings (wide characters appear once).
pub(crate) fn lines(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            let mut row = String::new();
            let mut skip = 0;
            for x in area.left()..area.right() {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buf[(x, y)].symbol();
                skip = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1);
                row.push_str(symbol);
            }
            row
        })
        .collect()
}
