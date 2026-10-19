//! `sprint print`: stream a file to a printer without a UI.
//!
//! The file is loaded and analyzed first (so progress, ETA and layers are
//! known), then Sprint connects, waits for the printer's setup and starts
//! the job. A progress line (percent, layer, ETA, temperatures) goes to
//! stderr: redrawn in place on a terminal, a line per 10% otherwise. The
//! first Ctrl+C cancels the job (the cancel script runs); a second one
//! stops waiting. Exit status: 0 when the print completed, 1 when it
//! failed or was cancelled by the printer, 130 after Ctrl+C.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use sprint_gcode::{ResumeOptions, resume_preamble};
use sprint_host::{JobStatus, Printer, PrinterProfile};
use sprint_tui::format;
use sprint_tui::interrupt::{self, Interrupt};
use sprint_tui::loader::LoadedFile;
use sprint_tui::report;
use sprint_tui::session::{self, WaitError};

use crate::analyze;
use crate::cli::PrintArgs;

/// How long the setup queries after connecting may take.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the job is looked at.
const POLL: Duration = Duration::from_millis(100);
/// How often the progress line is redrawn on a terminal.
const REDRAW: Duration = Duration::from_millis(250);
/// How long a cancel may take before Sprint stops waiting.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(60);

/// Exit code after Ctrl+C.
const EXIT_INTERRUPTED: u8 = 130;

/// Run `sprint print`.
pub fn run(args: &PrintArgs) -> Result<ExitCode> {
    let (config, _) = sprint_tui::config::load_config();
    let profile = config.active().clone();
    let interrupt = interrupt::global();
    let stderr_tty = io::stderr().is_terminal();
    let show_progress = !args.no_progress;

    let loaded = load(
        &args.file,
        &profile,
        show_progress && stderr_tty,
        &interrupt,
    )?;
    let mut options = sprint_tui::actions::print_options(&loaded);
    if let Some(line) = args.start_line {
        let total = loaded.file.len();
        let index = usize::try_from(line - 1).unwrap_or(usize::MAX);
        if index >= total {
            bail!(
                "--start-line {line} is past the end of {} ({total} lines)",
                loaded.name()
            );
        }
        options.start_line = index;
        options.preamble = resume_preamble(&loaded.file, index, &ResumeOptions::default());
    }

    let printer = Printer::with_console_capacity(config.console_max_lines);
    printer.set_profile(profile.clone());
    let label = args.connect.label();
    if show_progress {
        eprintln!("Connecting to {label}…");
    }
    session::connect_blocking(
        &printer,
        args.connect.clone(),
        profile.options.clone(),
        &interrupt,
    )
    .map_err(|e| anyhow!("{e}"))?;
    match session::wait_setup(&printer, SETUP_TIMEOUT, &interrupt) {
        Err(WaitError::Interrupted) => {
            printer.disconnect();
            return Ok(ExitCode::from(EXIT_INTERRUPTED));
        }
        Err(WaitError::Disconnected(_)) => bail!("{label} closed the connection"),
        Ok(()) | Err(WaitError::TimedOut) => {}
    }
    printer
        .start_print(loaded.file.clone(), options)
        .map_err(|e| anyhow!("cannot print {}: {e}", loaded.name()))?;
    if show_progress {
        let stats = &loaded.toolpath.stats;
        eprintln!(
            "Printing {} on {label}: {} layers, {} estimated (Ctrl+C cancels)",
            loaded.name(),
            loaded.toolpath.layers.len(),
            format::duration(f64::from(stats.estimated_time))
        );
    }

    let mut progress = Progress::new(show_progress, stderr_tty);
    let outcome = follow(&printer, &interrupt, &mut |view| progress.show(view));
    progress.finish();
    printer.disconnect();

    let (status, elapsed, percent) = outcome.last;
    let name = loaded.name();
    match (status, outcome.by_user) {
        (JobStatus::Completed, _) => {
            let mut out = io::stdout().lock();
            let _ = writeln!(
                out,
                "Completed {name}: {percent} in {}",
                format::clock_of(elapsed)
            );
            Ok(ExitCode::SUCCESS)
        }
        (_, true) => {
            eprintln!("Cancelled {name} at {percent}");
            Ok(ExitCode::from(EXIT_INTERRUPTED))
        }
        (JobStatus::Failed(why), false) => Err(anyhow!("{name} failed at {percent}: {why}")),
        (JobStatus::Cancelled, false) => Err(anyhow!("the printer cancelled {name} at {percent}")),
        (other, false) => Err(anyhow!("{name} stopped at {percent} ({})", other.label())),
    }
}

/// Load and analyze the file (progress on stderr when `progress`).
fn load(
    path: &Path,
    profile: &PrinterProfile,
    progress: bool,
    interrupt: &Interrupt,
) -> Result<LoadedFile> {
    let file = analyze::load(path)?;
    let options = sprint_tui::loader::analyze_options(profile);
    let started = Instant::now();
    let stderr = io::stderr();
    let mut bar = analyze::Progress::new(progress.then_some(stderr), file.name());
    let toolpath = sprint_gcode::analyze(&file, &options, &mut |fraction| {
        bar.update(fraction);
        !interrupt.is_raised()
    });
    bar.finish();
    let toolpath = toolpath.context("the analysis was cancelled")?;
    Ok(LoadedFile::new(
        path.to_path_buf(),
        file,
        toolpath,
        started.elapsed(),
    ))
}

/// What the job looked like at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub status: JobStatus,
    /// 0..=1.
    pub progress: f32,
    /// [`report::monitor_text`] of the printer.
    pub text: String,
}

/// How following a job ended.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The last status, the elapsed time and the progress ("42.0%").
    pub last: (JobStatus, Duration, String),
    /// Ctrl+C cancelled (or abandoned) the job.
    pub by_user: bool,
}

/// Follow the job until it ends: the first Ctrl+C cancels it, the second
/// stops waiting (as does a cancel that takes too long).
pub fn follow(printer: &Printer, interrupt: &Interrupt, show: &mut dyn FnMut(&JobView)) -> Outcome {
    let mut cancelled_at: Option<Instant> = None;
    loop {
        let (view, elapsed) = printer.with_state(|s| {
            let job = s.job.as_ref();
            (
                JobView {
                    status: job.map_or(JobStatus::Failed("no job".into()), |j| j.status.clone()),
                    progress: job.map_or(0.0, |j| j.progress),
                    text: report::monitor_text(s),
                },
                job.map(|j| j.elapsed).unwrap_or_default(),
            )
        });
        show(&view);
        let percent = format::percent(f64::from(view.progress));
        let by_user = cancelled_at.is_some();
        if view.status.is_finished() {
            return Outcome {
                last: (view.status, elapsed, percent),
                by_user,
            };
        }
        let presses = interrupt.count();
        let gave_up = cancelled_at.is_some_and(|t| t.elapsed() >= CANCEL_TIMEOUT);
        if presses >= 2 || gave_up {
            return Outcome {
                last: (view.status, elapsed, percent),
                by_user: true,
            };
        }
        if presses == 1 && cancelled_at.is_none() {
            printer.cancel();
            cancelled_at = Some(Instant::now());
            eprintln!("\nCancelling (Ctrl+C again stops waiting)…");
        }
        std::thread::sleep(POLL);
    }
}

/// The progress line on stderr.
struct Progress {
    enabled: bool,
    terminal: bool,
    last_draw: Option<Instant>,
    /// Width of the line on screen (terminal).
    drawn: usize,
    /// Last 10% step printed (not a terminal).
    last_step: Option<u32>,
    last_status: Option<JobStatus>,
}

impl Progress {
    fn new(enabled: bool, terminal: bool) -> Progress {
        Progress {
            enabled,
            terminal,
            last_draw: None,
            drawn: 0,
            last_step: None,
            last_status: None,
        }
    }

    fn show(&mut self, view: &JobView) {
        if !self.enabled {
            return;
        }
        let status_changed = self.last_status.as_ref() != Some(&view.status);
        self.last_status = Some(view.status.clone());
        let mut err = io::stderr().lock();
        if self.terminal {
            let due = self.last_draw.is_none_or(|t| t.elapsed() >= REDRAW);
            if !(due || status_changed || view.status.is_finished()) {
                return;
            }
            self.last_draw = Some(Instant::now());
            let text = fit(&view.text, terminal_width().saturating_sub(1));
            let width = unicode_width::UnicodeWidthStr::width(text.as_str());
            let pad = " ".repeat(self.drawn.saturating_sub(width));
            let _ = write!(err, "\r{text}{pad}").and_then(|()| err.flush());
            self.drawn = width;
        } else {
            // In 0..=10 for progress in 0..=1.
            let step = (view.progress.clamp(0.0, 1.0) * 10.0).floor() as u32;
            if self.last_step != Some(step) || status_changed || view.status.is_finished() {
                self.last_step = Some(step);
                let _ = writeln!(err, "{}", view.text);
            }
        }
    }

    fn finish(&mut self) {
        if self.enabled && self.terminal && self.drawn > 0 {
            let _ = writeln!(io::stderr());
            self.drawn = 0;
        }
    }
}

/// The terminal's width in columns (80 when unknown).
fn terminal_width() -> usize {
    crossterm::terminal::size().map_or(80, |(w, _)| usize::from(w).max(20))
}

/// `text` cut to `max` columns.
fn fit(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > max {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::sprint_sim::SimConfig;
    use sprint_host::{ConnectOptions, ConnectTarget};
    use std::sync::Arc;

    fn connect(sim: SimConfig) -> Printer {
        let printer = Printer::new();
        let interrupt = Interrupt::new();
        session::connect_blocking(
            &printer,
            ConnectTarget::Simulator(sim),
            ConnectOptions::default(),
            &interrupt,
        )
        .expect("connect to the simulator");
        session::wait_setup(&printer, Duration::from_secs(30), &interrupt).expect("setup");
        printer
    }

    /// Many short real-time moves: still printing while Ctrl+C arrives.
    fn slow_job() -> Arc<sprint_gcode::GcodeFile> {
        let mut text = String::from("G28\nG90\nM83\nG1 Z0.3 F600\n");
        for i in 0..400 {
            text.push_str(&format!("G1 X{} Y20 E0.2 F600\n", 20 + (i % 2) * 5));
        }
        Arc::new(sprint_gcode::GcodeFile::from_text("slow.gcode", &text))
    }

    #[test]
    fn a_print_is_followed_to_completion() {
        let printer = connect(SimConfig {
            boot_delay_ms: 20,
            time_scale: 100.0,
            instant_moves: true,
            ..SimConfig::default()
        });
        printer
            .start_print(slow_job(), Default::default())
            .expect("start");
        let mut views = Vec::new();
        let outcome = follow(&printer, &Interrupt::new(), &mut |v| views.push(v.clone()));
        assert_eq!(outcome.last.0, JobStatus::Completed);
        assert_eq!(outcome.last.2, "100.0%");
        assert!(!outcome.by_user);
        assert!(
            views
                .last()
                .is_some_and(|v| v.text.starts_with("Completed 100.0%"))
        );
        printer.disconnect();
    }

    #[test]
    fn ctrl_c_cancels_then_stops_waiting() {
        let printer = connect(SimConfig {
            boot_delay_ms: 20,
            time_scale: 5.0,
            instant_moves: false,
            ..SimConfig::default()
        });
        printer
            .start_print(slow_job(), Default::default())
            .expect("start");
        let interrupt = Interrupt::new();
        let raiser = interrupt.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            raiser.raise();
        });
        let outcome = follow(&printer, &interrupt, &mut |_| {});
        let _ = thread.join();
        assert!(outcome.by_user);
        assert_eq!(outcome.last.0, JobStatus::Cancelled);
        // A second press while the cancel runs stops waiting at once.
        printer
            .start_print(slow_job(), Default::default())
            .expect("start again");
        interrupt.reset();
        interrupt.raise();
        interrupt.raise();
        let outcome = follow(&printer, &interrupt, &mut |_| {});
        assert!(outcome.by_user);
        printer.disconnect();
    }

    #[test]
    fn progress_lines_without_a_terminal_come_per_ten_percent() {
        let mut progress = Progress::new(true, false);
        let view = |p: f32| JobView {
            status: JobStatus::Printing,
            progress: p,
            text: String::new(),
        };
        progress.show(&view(0.0));
        assert_eq!(progress.last_step, Some(0));
        progress.show(&view(0.05));
        assert_eq!(progress.last_step, Some(0));
        progress.show(&view(0.31));
        assert_eq!(progress.last_step, Some(3));
        assert_eq!(fit("Printing 42.0%", 8), "Printing");
        assert_eq!(fit("ab", 10), "ab");
    }
}
