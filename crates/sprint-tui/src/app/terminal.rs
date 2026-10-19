//! Running the TUI on the real terminal: raw mode and the alternate
//! screen (restored on exit, on errors and on panics) and the event loop.

use std::io::{self, IsTerminal, Stdout};
use std::panic::{self, PanicHookInfo};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use sprint_host::Printer;

use super::App;
use crate::TuiOptions;
use crate::theme::Theme;

/// Shortest time between two frames (about 20 per second).
const FRAME: Duration = Duration::from_millis(50);
/// How long to wait for input when nothing is pending.
const IDLE_POLL: Duration = Duration::from_millis(50);

type Backend = CrosstermBackend<Stdout>;

/// Put the terminal back: leave raw mode and the alternate screen, stop
/// mouse capture and bracketed paste, show the cursor. Errors are ignored:
/// this runs on the way out, possibly from a panic.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
}

/// The panic hook in force before the TUI started.
type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// Restores the terminal (and the previous panic hook) when dropped, so
/// every way out of the TUI leaves a usable shell behind.
struct TerminalGuard {
    previous: Option<Arc<Hook>>,
}

impl TerminalGuard {
    fn enter() -> Result<(TerminalGuard, Terminal<Backend>)> {
        let previous: Arc<Hook> = Arc::new(panic::take_hook());
        let chained = Arc::clone(&previous);
        panic::set_hook(Box::new(move |info| {
            restore_terminal();
            chained(info);
        }));
        let guard = TerminalGuard {
            previous: Some(previous),
        };
        enable_raw_mode().context("cannot switch the terminal to raw mode")?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )
        .context("cannot set up the terminal")?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
            .context("cannot start the terminal UI")?;
        Ok((guard, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
        if let Some(previous) = self.previous.take() {
            panic::set_hook(Box::new(move |info| previous(info)));
        }
    }
}

/// Run the full-screen UI until the user quits.
///
/// The terminal is switched to raw mode and the alternate screen, and put
/// back however the UI ends (quit, error or panic). `options.file` is
/// loaded in the background; the UI connects to `options.connect`, or to
/// the active profile's printer only when the profile enables
/// `auto_connect`.
pub fn run_tui(options: TuiOptions) -> Result<()> {
    // Before anything else (and before any connection): without a
    // terminal there is nothing to draw on.
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        anyhow::bail!(
            "the terminal UI needs an interactive terminal \
             (sprint shell reads commands from a pipe, sprint print prints without a UI)"
        );
    }
    let (config, config_path) = crate::config::load_config();
    let profile = config.active().clone();
    let printer = Printer::with_console_capacity(config.console_max_lines);
    let wake = Arc::new(AtomicBool::new(true));
    {
        let wake = Arc::clone(&wake);
        printer.set_waker(move || wake.store(true, Ordering::Release));
    }
    let mut app = App::new(printer.clone(), config, Some(config_path), Theme::detect());
    if let Some(file) = options.file {
        app.open_file(file);
    }
    // Connect only when asked to, or when the profile says so.
    if let Some(target) = crate::actions::startup_target(options.connect, &profile) {
        app.connect(target);
    }
    let result = {
        let (_guard, mut terminal) = TerminalGuard::enter()?;
        event_loop(&mut terminal, &mut app, &wake)
    };
    printer.disconnect();
    result
}

/// Read input, follow the printer and redraw (at most every [`FRAME`],
/// and only when something changed) until the app quits.
fn event_loop(terminal: &mut Terminal<Backend>, app: &mut App, wake: &AtomicBool) -> Result<()> {
    let mut last_draw: Option<Instant> = None;
    while !app.quit {
        let since = last_draw.map_or(FRAME, |t| t.elapsed());
        let timeout = if app.dirty {
            FRAME.saturating_sub(since)
        } else {
            IDLE_POLL
        };
        if event::poll(timeout).context("cannot read the terminal")? {
            loop {
                let ev = event::read().context("cannot read the terminal")?;
                app.handle_event(ev);
                if app.quit || !event::poll(Duration::ZERO).context("cannot read the terminal")? {
                    break;
                }
            }
        }
        if wake.swap(false, Ordering::AcqRel) {
            app.sync();
        }
        app.tick(Instant::now());
        let due = last_draw.is_none_or(|t| t.elapsed() >= FRAME);
        if app.dirty && due && !app.quit {
            terminal
                .draw(|frame| app.draw(frame))
                .context("cannot draw the terminal UI")?;
            last_draw = Some(Instant::now());
            app.dirty = false;
        }
    }
    Ok(())
}
