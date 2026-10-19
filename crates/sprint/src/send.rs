//! `sprint send`: connect, send each command and print the printer's
//! replies to it, until its `ok` (or a timeout).
//!
//! Each command-line argument is one command (quote commands with
//! parameters: `"G1 X10"`); an argument with several lines sends each
//! line. Replies go to stdout as the printer sent them, host warnings and
//! errors to stderr. Exit status: 0 when every command was acknowledged, 1
//! after a timeout, a lost connection or a firmware error reply, 130 on
//! Ctrl+C.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use sprint_host::{ConsoleKind, Event, Printer};
use sprint_tui::interrupt::{self, Interrupt};
use sprint_tui::session::{self, WaitError, link_idle};

use crate::cli::SendArgs;
use crate::headless::{self, Connected, EXIT_INTERRUPTED};

/// The link must stay idle this long after a command's last `ok`.
const SETTLE: Duration = Duration::from_millis(100);
/// Commands the host handles itself (M112 stops at once) may never show up
/// as sent lines; an idle link this long also ends their wait.
const IDLE_GIVE_UP: Duration = Duration::from_secs(1);

/// Run `sprint send`.
pub fn run(args: &SendArgs) -> Result<ExitCode> {
    let (config, _) = sprint_tui::config::load_config();
    let profile = config.active().clone();
    let printer = Printer::with_console_capacity(config.console_max_lines);
    printer.set_profile(profile.clone());
    let interrupt = interrupt::global();
    let label = args.connect.label();
    // The replies to the setup queries are not ours: they are over once
    // this returns.
    let connected = headless::connect(
        &printer,
        args.connect.clone(),
        profile.options.clone(),
        &interrupt,
    )?;
    if connected == Connected::Interrupted {
        return Ok(ExitCode::from(EXIT_INTERRUPTED));
    }
    let events = printer.subscribe();
    let timeout = Duration::from_secs_f32(args.timeout);
    let mut out = io::stdout().lock();
    let mut code = ExitCode::SUCCESS;
    for command in &args.commands {
        let expected = command_lines(command);
        if expected == 0 {
            continue;
        }
        printer.send(command);
        let mut reply = Reply::default();
        let result = wait_reply(
            &printer,
            &events,
            expected,
            timeout,
            &interrupt,
            &mut |line| {
                reply.line(&mut out, line);
            },
        );
        if reply.error {
            code = ExitCode::FAILURE;
        }
        match result {
            Ok(()) => {}
            Err(WaitError::TimedOut) => {
                eprintln!(
                    "sprint send: no ok for {} within {}; the remaining commands were not sent",
                    command.trim(),
                    sprint_tui::format::duration(timeout.as_secs_f64())
                );
                code = ExitCode::FAILURE;
                break;
            }
            Err(WaitError::Interrupted) => {
                code = ExitCode::from(EXIT_INTERRUPTED);
                break;
            }
            Err(WaitError::Disconnected(why)) => {
                eprintln!("sprint send: {}", headless::closed(&label, &why));
                code = ExitCode::FAILURE;
                break;
            }
        }
    }
    printer.disconnect();
    Ok(code)
}

/// What came back for one command.
#[derive(Default)]
struct Reply {
    /// The printer reported an error.
    error: bool,
    /// Stdout is gone (`| head`): stop writing.
    closed: bool,
}

impl Reply {
    fn line(&mut self, out: &mut impl Write, line: Line) {
        match line {
            Line::Reply(text) => {
                if is_error_reply(&text) {
                    self.error = true;
                }
                if !self.closed && writeln!(out, "{text}").and_then(|()| out.flush()).is_err() {
                    self.closed = true;
                }
            }
            Line::Host(text) => eprintln!("{text}"),
        }
    }
}

/// A line to show.
enum Line {
    /// From the printer.
    Reply(String),
    /// A host warning or error.
    Host(String),
}

/// Commands in `text`: lines that are not blank or comments.
fn command_lines(text: &str) -> usize {
    text.lines()
        .map(|l| l.split(';').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .count()
}

/// Firmware errors (`Error:...`, `!! ...`).
fn is_error_reply(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("error") || lower.starts_with("!!")
}

/// Pass the replies to `show` until `expected` commands went out and the
/// link has been idle for a moment (every `ok` arrived).
fn wait_reply(
    printer: &Printer,
    events: &Receiver<Event>,
    expected: usize,
    timeout: Duration,
    interrupt: &Interrupt,
    show: &mut dyn FnMut(Line),
) -> Result<(), WaitError> {
    let deadline = Instant::now().checked_add(timeout);
    let mut sent = 0;
    let mut idle_since: Option<Instant> = None;
    loop {
        match events.recv_timeout(session::POLL) {
            Ok(Event::Console(entry)) if !entry.auto => match entry.kind {
                ConsoleKind::Sent => sent += 1,
                ConsoleKind::Received => show(Line::Reply(entry.text)),
                ConsoleKind::Warning | ConsoleKind::Error => show(Line::Host(entry.text)),
                ConsoleKind::Info => {}
            },
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(WaitError::Disconnected(String::new()));
            }
        }
        // Only decide once every reply so far has been shown.
        if events.is_empty() {
            let (idle, open) = printer.with_state(|s| (link_idle(s), s.status.is_open()));
            if !open {
                return Err(WaitError::Disconnected(String::new()));
            }
            if idle {
                let since = *idle_since.get_or_insert_with(Instant::now);
                let settled = since.elapsed();
                if (sent >= expected && settled >= SETTLE) || settled >= IDLE_GIVE_UP {
                    return Ok(());
                }
            } else {
                idle_since = None;
            }
        }
        if interrupt.take() {
            return Err(WaitError::Interrupted);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(WaitError::TimedOut);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_counted_per_line() {
        assert_eq!(command_lines("G28"), 1);
        assert_eq!(command_lines("G28 ; home\n\n; only a comment\nM105"), 2);
        assert_eq!(command_lines("   "), 0);
        assert_eq!(command_lines(";x"), 0);
    }

    #[test]
    fn firmware_errors_are_recognised() {
        assert!(is_error_reply("Error:Printer halted. kill() called!"));
        assert!(is_error_reply("!! Thermal runaway"));
        assert!(!is_error_reply("echo:Unknown command: \"M9999\""));
        assert!(!is_error_reply("ok"));
    }

    #[test]
    fn replies_are_written_and_errors_noted() {
        let mut out = Vec::new();
        let mut reply = Reply::default();
        reply.line(&mut out, Line::Reply("ok T:20".into()));
        reply.line(&mut out, Line::Reply("Error:Bad".into()));
        assert_eq!(String::from_utf8_lossy(&out), "ok T:20\nError:Bad\n");
        assert!(reply.error);
    }
}
