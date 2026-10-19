//! `sprint send`: connect, send each command and print the printer's
//! replies to it, until its `ok` (or a timeout).
//!
//! Each command-line argument is one command (quote commands with
//! parameters: `"G1 X10"`); an argument with several lines sends each
//! line. Replies go to stdout as the printer sent them, host warnings and
//! errors to stderr. The timeout is how long the printer may stay silent
//! about a command: `busy` messages (homing, probing) and, during a heat
//! wait (M109, M190, M191), temperature reports restart it, and a heat
//! wait shows its progress on stderr. Exit status: 0 when every command
//! was acknowledged, 1 after a timeout, a lost connection, a firmware
//! error reply or a command the firmware does not know, 130 on Ctrl+C.

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use sprint_host::{ConsoleKind, Event, Printer};
use sprint_tui::interrupt::{self, Interrupt};
use sprint_tui::report;
use sprint_tui::session::{self, WaitError, link_idle};

use crate::cli::SendArgs;
use crate::headless::{self, Connected, EXIT_INTERRUPTED};

/// The link must stay idle this long after a command's last `ok`.
const SETTLE: Duration = Duration::from_millis(100);
/// Commands the host handles itself (M112 stops at once) may never show up
/// as sent lines; an idle link this long also ends their wait.
const IDLE_GIVE_UP: Duration = Duration::from_secs(1);
/// How often a heat wait's progress is shown on a terminal.
const HEAT_REDRAW: Duration = Duration::from_secs(1);
/// How often a heat wait's progress is printed when stderr is not a
/// terminal.
const HEAT_LINE: Duration = Duration::from_secs(10);

/// Run `sprint send`.
pub fn run(args: &SendArgs) -> Result<ExitCode> {
    let (config, profile) = headless::load_profile(args.profile.as_deref())?;
    let target = args.connect.resolve(&profile);
    let printer = Printer::with_console_capacity(config.console_max_lines);
    printer.set_profile(profile.clone());
    let interrupt = interrupt::global();
    let label = target.label();
    // The replies to the setup queries are not ours: they are over once
    // this returns.
    let connected = headless::connect(&printer, target, profile.options.clone(), &interrupt)?;
    if connected == Connected::Interrupted {
        return Ok(ExitCode::from(EXIT_INTERRUPTED));
    }
    let events = printer.subscribe();
    let timeout = Duration::from_secs_f32(args.timeout);
    let mut out = io::stdout().lock();
    let mut heat = HeatProgress::new(io::stderr().is_terminal());
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
            Wait {
                expected,
                timeout,
                heat_wait: is_heat_wait(command),
            },
            &interrupt,
            &mut |line| match line {
                Line::Heating(text) => heat.show(&text),
                line => {
                    heat.finish();
                    reply.line(&mut out, line);
                }
            },
        );
        heat.finish();
        if reply.error {
            code = ExitCode::FAILURE;
        }
        match result {
            Ok(()) => {}
            Err(WaitError::TimedOut) => {
                eprintln!(
                    "sprint send: no ok for {} and no sign of progress within {}; \
                     the remaining commands were not sent",
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
    /// The printer reported an error, or did not know the command.
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
            Line::Host(text) | Line::Heating(text) => eprintln!("{text}"),
        }
    }
}

/// A line to show.
enum Line {
    /// From the printer.
    Reply(String),
    /// A host warning or error.
    Host(String),
    /// How a heat wait is going (temperatures and targets).
    Heating(String),
}

/// A heat wait's progress on stderr: redrawn in place on a terminal, a
/// line now and then otherwise.
struct HeatProgress {
    terminal: bool,
    last: Option<Instant>,
    /// Width of the line drawn in place.
    drawn: usize,
}

impl HeatProgress {
    fn new(terminal: bool) -> HeatProgress {
        HeatProgress {
            terminal,
            last: None,
            drawn: 0,
        }
    }

    fn show(&mut self, text: &str) {
        let every = if self.terminal {
            HEAT_REDRAW
        } else {
            HEAT_LINE
        };
        if self.last.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        self.last = Some(Instant::now());
        let mut err = io::stderr().lock();
        if self.terminal {
            let width = text.chars().count();
            let pad = " ".repeat(self.drawn.saturating_sub(width));
            let _ = write!(err, "\r{text}{pad}").and_then(|()| err.flush());
            self.drawn = width;
        } else {
            let _ = writeln!(err, "{text}");
        }
    }

    /// End the line drawn in place.
    fn finish(&mut self) {
        if self.drawn > 0 {
            let _ = writeln!(io::stderr());
            self.drawn = 0;
        }
        self.last = None;
    }
}

/// Commands in `text`: lines that are not blank or comments.
fn command_lines(text: &str) -> usize {
    command_codes(text).count()
}

/// The first word of each command line, upper-case, without a line
/// number (`N12 M109 S200` gives `M109`).
fn command_codes(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines()
        .map(|l| l.split(';').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut words = l.split_whitespace();
            let first = words.next().unwrap_or_default();
            let code = if first.starts_with(['N', 'n']) && first[1..].parse::<u64>().is_ok() {
                words.next().unwrap_or_default()
            } else {
                first
            };
            code.to_ascii_uppercase()
        })
}

/// Whether `text` waits for a temperature (M109, M190, M191): the
/// firmware reports temperatures meanwhile.
fn is_heat_wait(text: &str) -> bool {
    command_codes(text).any(|code| matches!(code.as_str(), "M109" | "M190" | "M191"))
}

/// Firmware errors (`Error:...`, `!! ...`) and commands the firmware does
/// not know (`echo:Unknown command: "M9999"`).
fn is_error_reply(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("error") || lower.starts_with("!!") || lower.contains("unknown command")
}

/// Whether a reply says the firmware is still working on a command
/// (`echo:busy: processing`).
fn is_busy(text: &str) -> bool {
    text.to_ascii_lowercase().contains("busy:")
}

/// What [`wait_reply`] waits for.
#[derive(Clone, Copy, Debug)]
struct Wait {
    /// Commands that must go out.
    expected: usize,
    /// How long the printer may stay silent about them.
    timeout: Duration,
    /// They wait for a temperature: temperature reports are progress.
    heat_wait: bool,
}

/// Pass the replies to `show` until `wait.expected` commands went out
/// and the link has been idle for a moment (every `ok` arrived), or the
/// printer said nothing about them for `wait.timeout` (busy messages and,
/// in a heat wait, temperature reports restart it).
fn wait_reply(
    printer: &Printer,
    events: &Receiver<Event>,
    wait: Wait,
    interrupt: &Interrupt,
    show: &mut dyn FnMut(Line),
) -> Result<(), WaitError> {
    let mut deadline = Instant::now().checked_add(wait.timeout);
    let mut sent = 0;
    let mut idle_since: Option<Instant> = None;
    loop {
        match events.recv_timeout(session::POLL) {
            Ok(Event::Console(entry)) => {
                if entry.kind == ConsoleKind::Received && is_busy(&entry.text) {
                    deadline = Instant::now().checked_add(wait.timeout);
                }
                if !entry.auto {
                    match entry.kind {
                        ConsoleKind::Sent => sent += 1,
                        ConsoleKind::Received => show(Line::Reply(entry.text)),
                        ConsoleKind::Warning | ConsoleKind::Error => show(Line::Host(entry.text)),
                        ConsoleKind::Info => {}
                    }
                }
            }
            Ok(Event::Temperatures(_)) if wait.heat_wait && sent > 0 => {
                deadline = Instant::now().checked_add(wait.timeout);
                let text = printer.with_state(report::monitor_text);
                show(Line::Heating(format!("Heating: {text}")));
            }
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
                if (sent >= wait.expected && settled >= SETTLE) || settled >= IDLE_GIVE_UP {
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
    use sprint_host::sprint_sim::SimConfig;
    use sprint_host::{ConnectOptions, ConnectTarget};

    #[test]
    fn commands_are_counted_per_line() {
        assert_eq!(command_lines("G28"), 1);
        assert_eq!(command_lines("G28 ; home\n\n; only a comment\nM105"), 2);
        assert_eq!(command_lines("   "), 0);
        assert_eq!(command_lines(";x"), 0);
    }

    #[test]
    fn heat_waits_are_recognised() {
        assert!(is_heat_wait("M190 S60"));
        assert!(is_heat_wait("m109 s200"));
        assert!(is_heat_wait("G28\nN5 M191 S40*12"));
        assert!(!is_heat_wait("M104 S200"));
        assert!(!is_heat_wait("M1090"));
        assert!(!is_heat_wait("; M109"));
        assert!(!is_heat_wait("N"));
    }

    #[test]
    fn firmware_errors_are_recognised() {
        assert!(is_error_reply("Error:Printer halted. kill() called!"));
        assert!(is_error_reply("!! Thermal runaway"));
        assert!(is_error_reply("echo:Unknown command: \"M9999\""));
        assert!(!is_error_reply("ok"));
        assert!(!is_error_reply("echo:busy: processing"));
        assert!(is_busy("echo:busy: processing"));
        assert!(!is_busy("ok T:20"));
    }

    #[test]
    fn replies_are_written_and_errors_noted() {
        let mut out = Vec::new();
        let mut reply = Reply::default();
        reply.line(&mut out, Line::Reply("ok T:20".into()));
        assert!(!reply.error);
        reply.line(&mut out, Line::Reply("Error:Bad".into()));
        assert_eq!(String::from_utf8_lossy(&out), "ok T:20\nError:Bad\n");
        assert!(reply.error);
    }

    /// A heat wait on a real-time printer lasts longer than the timeout
    /// but reports temperatures all along: it is waited for.
    #[test]
    fn a_heat_wait_that_reports_progress_is_not_a_timeout() {
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        session::connect_blocking(
            &printer,
            ConnectTarget::Simulator(SimConfig {
                boot_delay_ms: 20,
                time_scale: 10.0,
                ..SimConfig::default()
            }),
            ConnectOptions::default(),
            &interrupt,
        )
        .expect("connect to the simulator");
        session::wait_setup(&printer, Duration::from_secs(30), &interrupt).expect("setup");
        let events = printer.subscribe();
        // The bed from room temperature to 35 °C takes seconds (ten times
        // faster than real time).
        let started = Instant::now();
        printer.send("M190 S35");
        let mut heating = 0;
        let result = wait_reply(
            &printer,
            &events,
            Wait {
                expected: 1,
                timeout: Duration::from_millis(1000),
                heat_wait: true,
            },
            &interrupt,
            &mut |line| {
                if matches!(line, Line::Heating(_)) {
                    heating += 1;
                }
            },
        );
        assert_eq!(result, Ok(()));
        assert!(
            started.elapsed() > Duration::from_millis(1000),
            "{:?}",
            started.elapsed()
        );
        assert!(heating > 0);
        // (Without signs of progress the timeout still ends the wait: see
        // the send_gives_up_after_the_timeout test of the binary.)
        printer.disconnect();
    }
}
