//! Running the TUI on the real terminal: raw mode and the alternate
//! screen (restored on exit, on errors and on panics) and the event loop.

use std::io::{self, IsTerminal, Stdout};
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ThreadId};
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

/// Panics of other threads while the UI runs, for the status line.
static BACKGROUND_PANICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The messages of panics on other threads since the last call.
fn take_background_panics() -> Vec<String> {
    std::mem::take(
        &mut *BACKGROUND_PANICS
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    )
}

/// Install a panic hook for a UI running on `ui`; returns the previous
/// hook.
///
/// A panic on the UI thread ends the UI: `restore` puts the terminal back
/// before the previous hook reports the panic. A panic on another thread
/// (a file load, the printer engine, which survive their own panics)
/// leaves the UI running: the terminal stays as it is, nothing is written
/// over the screen, and the message waits for the status line.
fn install_panic_hook(ui: ThreadId, restore: fn()) -> Arc<Hook> {
    let previous: Arc<Hook> = Arc::new(panic::take_hook());
    let chained = Arc::clone(&previous);
    panic::set_hook(Box::new(move |info| {
        let current = thread::current();
        if current.id() == ui {
            restore();
            chained(info);
            return;
        }
        let name = current.name().unwrap_or("a background thread");
        let what = info.payload_as_str().unwrap_or("panic");
        log::error!("{name} panicked: {info}");
        BACKGROUND_PANICS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("Internal error in {name}: {what}"));
    }));
    previous
}

/// Put `previous` (from [`install_panic_hook`]) back.
fn restore_panic_hook(previous: Arc<Hook>) {
    panic::set_hook(Box::new(move |info| previous(info)));
}

/// Restores the terminal (and the previous panic hook) when dropped, so
/// every way out of the TUI leaves a usable shell behind.
struct TerminalGuard {
    previous: Option<Arc<Hook>>,
}

impl TerminalGuard {
    fn enter() -> Result<(TerminalGuard, Terminal<Backend>)> {
        let previous = install_panic_hook(thread::current().id(), restore_terminal);
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
            restore_panic_hook(previous);
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
    run_then_close(&printer, || {
        let (_guard, mut terminal) = TerminalGuard::enter()?;
        event_loop(&mut terminal, &mut app, &wake)
    })
}

/// Run the UI, then close the link the safe way however the UI ended:
/// quitting during a print (after asking), an error, or a panic on the UI
/// thread. A running print is cancelled first, so its cancel script turns
/// the heaters off before the link closes; only then does a panic go on
/// (the terminal is already restored by then).
fn run_then_close<T>(printer: &Printer, ui: impl FnOnce() -> T) -> T {
    let ended = panic::catch_unwind(AssertUnwindSafe(ui));
    let _ = crate::session::cancel_before_closing(printer, crate::session::CANCEL_WAIT, |name| {
        eprintln!("Cancelling {name} before closing the connection…")
    });
    printer.disconnect();
    match ended {
        Ok(value) => value,
        Err(panic) => panic::resume_unwind(panic),
    }
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
        for message in take_background_panics() {
            app.error(message);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static RESTORES: AtomicUsize = AtomicUsize::new(0);

    fn count_restore() {
        RESTORES.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn a_panicking_ui_still_cancels_the_print_before_closing() {
        use crate::interrupt::Interrupt;
        use crate::session;
        use sprint_host::sprint_sim::SimConfig;
        use sprint_host::{ConnectOptions, ConnectTarget, JobStatus};

        let printer = Printer::new();
        let interrupt = Interrupt::new();
        let sim = ConnectTarget::Simulator(SimConfig {
            boot_delay_ms: 20,
            time_scale: 5.0,
            ..SimConfig::default()
        });
        session::connect_blocking(&printer, sim, ConnectOptions::default(), &interrupt)
            .expect("connect");
        session::wait_setup(&printer, Duration::from_secs(30), &interrupt).expect("setup");
        let mut gcode = String::from("M104 S200\nM140 S60\nG28\nG1 Z0.3 F600\n");
        for i in 0..400 {
            gcode.push_str(&format!("G1 X{} Y20 F600\n", 20 + (i % 2) * 5));
        }
        let file = Arc::new(sprint_host::sprint_gcode::GcodeFile::from_text(
            "slow.gcode",
            &gcode,
        ));
        printer
            .start_print(file, Default::default())
            .expect("start");
        session::wait_for(&printer, Duration::from_secs(30), &interrupt, true, |s| {
            s.job.as_ref().is_some_and(|j| j.current_line > 5)
        })
        .expect("printing");
        let control = printer.sim_control().expect("simulator");
        // The UI thread panics in the middle of the print.
        let ended = panic::catch_unwind(AssertUnwindSafe(|| {
            run_then_close(&printer, || -> Result<()> { panic!("the UI broke") })
        }));
        assert!(ended.is_err(), "the panic goes on");
        assert_eq!(
            printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
            Some(JobStatus::Cancelled)
        );
        assert!(!printer.with_state(|s| s.status.is_open()));
        // The cancel script turned the heaters off before the link closed.
        let got = control.received_commands();
        assert!(got.iter().any(|c| c == "M104 S0"), "{got:?}");
        assert!(got.iter().any(|c| c == "M140 S0"), "{got:?}");
        // A UI that ends normally hands its result back.
        assert_eq!(run_then_close(&printer, || 7), 7);
    }

    #[test]
    fn only_a_panic_on_the_ui_thread_restores_the_terminal() {
        let previous = install_panic_hook(thread::current().id(), count_restore);
        // A background thread panics: the UI keeps its terminal and gets
        // the message.
        let worker = thread::Builder::new()
            .name("sprint-test-worker".into())
            .spawn(|| panic!("worker gave up"))
            .expect("spawn");
        assert!(worker.join().is_err());
        let restored_by_worker = RESTORES.load(Ordering::SeqCst);
        // The UI thread panics: the terminal is restored.
        let ui = panic::catch_unwind(|| panic!("the UI gave up"));
        let restored_by_ui = RESTORES.load(Ordering::SeqCst);
        restore_panic_hook(previous);
        assert!(ui.is_err());
        assert_eq!(restored_by_worker, 0);
        assert_eq!(restored_by_ui, 1);
        let messages = take_background_panics();
        assert!(
            messages.contains(&"Internal error in sprint-test-worker: worker gave up".to_string()),
            "{messages:?}"
        );
        assert!(take_background_panics().is_empty());
    }
}
