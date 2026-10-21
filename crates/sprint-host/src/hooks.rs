//! Job hooks: external commands run when a print starts, finishes, fails
//! or is cancelled ([`PrinterProfile::on_start_command`] and friends),
//! for notifications, relays or logging. They run on their own thread, never blocking the engine;
//! their exit status and first lines of output go to the console (output
//! is read for a second after the hook ends, not for as long as a program
//! it started in the background keeps it open).
//!
//! [`PrinterProfile::on_start_command`]: crate::PrinterProfile::on_start_command

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::console::ConsoleKind;
use crate::engine::Shared;

/// A hook still running after this long is stopped.
pub(crate) const HOOK_TIMEOUT: Duration = Duration::from_secs(120);
/// Output lines of a hook copied to the console.
const OUTPUT_LINES: usize = 5;
/// Output bytes of a hook kept per stream.
const OUTPUT_BYTES: usize = 16 * 1024;
/// Longest output line copied to the console (characters).
const LINE_CHARS: usize = 200;
/// After the hook ends, its output is read this much longer: a program it
/// started in the background (a notifier) may keep the pipes open for
/// hours, and the report must not wait for it.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// One hook run: `command` (a shell command line) with `env` added to the
/// environment; `event` names it in the console ("finish").
#[derive(Clone, Debug)]
pub(crate) struct Hook {
    pub(crate) event: &'static str,
    pub(crate) command: String,
    pub(crate) env: Vec<(&'static str, String)>,
}

impl Hook {
    /// Run on a helper thread; the console gets the result.
    pub(crate) fn spawn(self, shared: Arc<Shared>) {
        let event = self.event;
        let spawned = thread::Builder::new()
            .name(format!("sprint-hook-{event}"))
            .spawn(move || {
                let (kind, lines) = self.run(HOOK_TIMEOUT);
                for line in lines {
                    shared.log(kind, line, false, None);
                }
                shared.wake_now();
            });
        if let Err(e) = spawned {
            log::warn!("could not start the {event} hook thread: {e}");
        }
    }

    /// Run and wait (at most `timeout`); returns what to log.
    pub(crate) fn run(&self, timeout: Duration) -> (ConsoleKind, Vec<String>) {
        let mut child = match self.command().spawn() {
            Ok(c) => c,
            Err(e) => {
                return (
                    ConsoleKind::Warning,
                    vec![format!("The {} hook could not run: {e}", self.event)],
                );
            }
        };
        let out = child.stdout.take().map(collect);
        let err = child.stderr.take().map(collect);
        let status = wait(&mut child, timeout);
        let mut output: Vec<String> = Vec::new();
        let grace = Instant::now() + OUTPUT_GRACE;
        for pipe in [out, err].into_iter().flatten() {
            // Readers still blocked after the grace are left behind; they
            // end when the last program holding the pipe does.
            while !pipe.reader.is_finished() && Instant::now() < grace {
                thread::sleep(Duration::from_millis(10));
            }
            let text = String::from_utf8_lossy(&pipe.kept.lock()).into_owned();
            output.extend(
                text.lines()
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .map(|l| l.chars().take(LINE_CHARS).collect::<String>()),
            );
        }
        let more = output.len().saturating_sub(OUTPUT_LINES);
        output.truncate(OUTPUT_LINES);
        if more > 0 {
            output.push(format!("({more} more line(s) of output)"));
        }
        let (kind, head) = match status {
            Some(s) if s.success() => (
                ConsoleKind::Info,
                format!("The {} hook ran: {}", self.event, self.command),
            ),
            Some(s) => (
                ConsoleKind::Warning,
                format!(
                    "The {} hook failed ({}): {}",
                    self.event,
                    describe(s),
                    self.command
                ),
            ),
            None => (
                ConsoleKind::Warning,
                format!(
                    "The {} hook was stopped after {} s: {}",
                    self.event,
                    timeout.as_secs(),
                    self.command
                ),
            ),
        };
        let mut lines = vec![head];
        lines.extend(output.into_iter().map(|l| format!("  {l}")));
        (kind, lines)
    }

    /// The shell command: `cmd /C` on Windows, `sh -c` elsewhere.
    fn command(&self) -> Command {
        #[cfg(windows)]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            /// No console window flashes up when the GUI runs a hook.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let mut cmd = Command::new("cmd");
            cmd.arg("/D")
                .arg("/C")
                .raw_arg(&self.command)
                .creation_flags(CREATE_NO_WINDOW);
            cmd
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(&self.command);
            cmd
        };
        cmd.envs(self.env.iter().map(|(k, v)| (*k, v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

/// A pipe being read on its own thread.
struct Output {
    /// The first bytes read so far.
    kept: Arc<Mutex<Vec<u8>>>,
    reader: thread::JoinHandle<()>,
}

/// Read a pipe to its end on a thread, keeping the first bytes.
fn collect(mut pipe: impl Read + Send + 'static) -> Output {
    let kept = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&kept);
    let reader = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut kept = sink.lock();
                    let room = OUTPUT_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
    });
    Output { kept, reader }
}

/// Wait for `child` at most `timeout`; kill it after that (`None`).
fn wait(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn describe(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => "killed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(command: &str) -> Hook {
        Hook {
            event: "test",
            command: command.to_string(),
            env: vec![("SPRINT_STATUS", "completed".to_string())],
        }
    }

    #[test]
    fn output_and_environment_reach_the_console() {
        let command = if cfg!(windows) {
            "echo hello %SPRINT_STATUS%"
        } else {
            "echo hello $SPRINT_STATUS"
        };
        let (kind, lines) = hook(command).run(Duration::from_secs(30));
        assert_eq!(kind, ConsoleKind::Info, "{lines:?}");
        assert_eq!(lines[1], "  hello completed", "{lines:?}");
    }

    #[test]
    fn failures_are_warnings() {
        let (kind, lines) = hook("exit 3").run(Duration::from_secs(30));
        assert_eq!(kind, ConsoleKind::Warning);
        assert!(lines[0].contains("exit status 3"), "{lines:?}");
    }

    #[test]
    fn a_program_left_running_does_not_hold_the_report() {
        // A hook that starts a notifier in the background ends at once,
        // but the notifier keeps the output pipes open.
        let command = if cfg!(windows) {
            "echo started & start /B ping -n 8 127.0.0.1"
        } else {
            "echo started; sleep 8 &"
        };
        let begun = Instant::now();
        let (kind, lines) = hook(command).run(Duration::from_secs(30));
        assert!(begun.elapsed() < Duration::from_secs(4), "{lines:?}");
        assert_eq!(kind, ConsoleKind::Info, "{lines:?}");
        assert_eq!(lines.get(1).map(String::as_str), Some("  started"));
    }

    #[test]
    fn long_output_is_cut() {
        let command = if cfg!(windows) {
            "for /L %i in (1,1,20) do @echo line %i"
        } else {
            "for i in $(seq 1 20); do echo line $i; done"
        };
        let (_, lines) = hook(command).run(Duration::from_secs(30));
        assert_eq!(lines.len(), 1 + OUTPUT_LINES + 1, "{lines:?}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some("  (15 more line(s) of output)")
        );
    }
}
