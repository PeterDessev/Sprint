//! Shell output shared by the command thread and the echo thread.
//!
//! Every line goes through one lock, so printer output never tears a
//! report apart. On an interactive terminal the lines are handed to the
//! line editor's external printer, which prints them above the prompt
//! while a line is being edited; otherwise they are written to a stream.

use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::report::{ReportLine, Tone};
use crate::theme::ColorSupport;

/// Where finished text goes. `text` ends with a line break unless it is a
/// progress line (which starts with `\r`).
pub(crate) trait Sink: Send {
    fn write(&mut self, text: &str);
}

/// A sink over any writer (stdout, a test buffer). Write errors (a closed
/// pipe) are ignored: there is nobody left to tell.
pub(crate) struct WriterSink<W: Write + Send>(pub W);

impl<W: Write + Send> Sink for WriterSink<W> {
    fn write(&mut self, text: &str) {
        let _ = self.0.write_all(text.as_bytes());
        let _ = self.0.flush();
    }
}

struct Inner {
    sink: Box<dyn Sink>,
    support: ColorSupport,
    /// A progress line is on screen (no line break yet).
    progress: bool,
    /// Progress lines are drawn (a terminal); otherwise they are dropped.
    show_progress: bool,
}

/// Cloneable handle to the shell output.
#[derive(Clone)]
pub(crate) struct Out {
    inner: Arc<Mutex<Inner>>,
}

impl Out {
    /// Output to `sink` with `support` colors; `show_progress` draws
    /// progress lines (only sensible on a terminal).
    pub(crate) fn new(sink: Box<dyn Sink>, support: ColorSupport, show_progress: bool) -> Out {
        Out {
            inner: Arc::new(Mutex::new(Inner {
                sink,
                support,
                progress: false,
                show_progress,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while printing leaves nothing half-updated worth
        // refusing output for.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Print report lines.
    pub(crate) fn lines(&self, lines: &[ReportLine]) {
        if lines.is_empty() {
            return;
        }
        let mut inner = self.lock();
        let text = crate::report::render(lines, inner.support);
        inner.clear_progress();
        inner.sink.write(&text);
    }

    /// Print one report line.
    pub(crate) fn line(&self, line: ReportLine) {
        self.lines(std::slice::from_ref(&line));
    }

    /// Print text in one tone (one line per line of `text`).
    pub(crate) fn say(&self, tone: Tone, text: impl AsRef<str>) {
        let lines: Vec<ReportLine> = text
            .as_ref()
            .lines()
            .map(|l| ReportLine::toned(l, tone))
            .collect();
        self.lines(&lines);
    }

    /// Plain text.
    pub(crate) fn plain(&self, text: impl AsRef<str>) {
        self.say(Tone::Plain, text);
    }

    /// Secondary text.
    pub(crate) fn dim(&self, text: impl AsRef<str>) {
        self.say(Tone::Dim, text);
    }

    /// Good news.
    pub(crate) fn good(&self, text: impl AsRef<str>) {
        self.say(Tone::Good, text);
    }

    /// Something to notice.
    pub(crate) fn warn(&self, text: impl AsRef<str>) {
        self.say(Tone::Warn, text);
    }

    /// Something failed.
    pub(crate) fn error(&self, text: impl AsRef<str>) {
        self.say(Tone::Bad, text);
    }

    /// Show (or replace) the progress line.
    pub(crate) fn progress(&self, text: &str) {
        let mut inner = self.lock();
        if !inner.show_progress {
            return;
        }
        let line = ReportLine::toned(text, Tone::Dim).ansi(inner.support);
        let erase = if inner.support.has_color() {
            "\x1b[K"
        } else {
            ""
        };
        inner.sink.write(&format!("\r{line}{erase}"));
        inner.progress = true;
    }

    /// Remove the progress line.
    pub(crate) fn end_progress(&self) {
        self.lock().clear_progress();
    }
}

impl Inner {
    fn clear_progress(&mut self) {
        if self.progress {
            self.progress = false;
            let erase = if self.support.has_color() {
                "\r\x1b[K"
            } else {
                "\n"
            };
            self.sink.write(erase);
        }
    }
}

/// A shared in-memory buffer for tests.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct Buffer(pub Arc<Mutex<Vec<u8>>>);

#[cfg(test)]
impl Buffer {
    /// Everything written so far.
    pub(crate) fn text(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Forget what was written.
    pub(crate) fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

#[cfg(test)]
impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_tones_and_progress() {
        let buffer = Buffer::default();
        let out = Out::new(
            Box::new(WriterSink(buffer.clone())),
            ColorSupport::None,
            true,
        );
        out.plain("one\ntwo");
        out.progress("Loading 40%");
        out.error("bad\x07 news");
        out.progress("Loading 90%");
        out.end_progress();
        out.end_progress();
        assert_eq!(
            buffer.text(),
            "one\ntwo\n\rLoading 40%\nbad· news\n\rLoading 90%\n"
        );
        // Colors wrap the text; the progress line is erased in place.
        let colored = Buffer::default();
        let out = Out::new(
            Box::new(WriterSink(colored.clone())),
            ColorSupport::Ansi16,
            false,
        );
        out.good("ok");
        out.progress("hidden");
        let text = colored.text();
        assert!(text.contains("\x1b[") && text.contains("ok"), "{text:?}");
        assert!(!text.contains("hidden"));
    }
}
