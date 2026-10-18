//! Console lines and the scrollable console view.

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
}

impl ConsoleLine {
    /// A line of the given kind.
    pub fn new(kind: ConsoleKind, text: impl Into<String>) -> ConsoleLine {
        ConsoleLine {
            kind,
            text: text.into(),
            auto: false,
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
}
