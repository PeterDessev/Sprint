//! Status lines that wait for the printer to take a command: what
//! `fan 50`, `eeprom save` and the like say once the printer acknowledged
//! the command, or that it rejected it or will not take it.

use std::thread;
use std::time::Duration;

use crossbeam_channel::Sender;
use sprint_host::{Delivery, Tracked};

use super::output::Out;
use crate::session;

/// How a line that followed a command is printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    Dim,
    Good,
    Warn,
}

/// What to print once the printer took a command.
pub(crate) struct Report {
    pub tracked: Tracked,
    /// The line, and its style, once the printer acknowledged it.
    pub done: (Style, String),
    /// What did not happen ("Not saved to EEPROM"), which starts the line
    /// when the printer rejects the command or will not take it; `None`
    /// when another line says so (a print's own failure).
    pub failed: Option<String>,
}

impl Report {
    /// Print what `delivery` (not pending) means.
    pub(crate) fn print(self, out: &Out, delivery: Delivery) {
        match delivery {
            Delivery::Pending => {}
            Delivery::Acknowledged => match self.done {
                (Style::Dim, text) => out.dim(text),
                (Style::Good, text) => out.good(text),
                (Style::Warn, text) => out.warn(text),
            },
            Delivery::Rejected(answer) => {
                if let Some(failed) = self.failed {
                    out.error(format!("{failed}: {}", answer.trim()));
                }
            }
            Delivery::Lost => {
                if let Some(failed) = self.failed {
                    out.warn(format!("{failed}: the printer did not confirm it"));
                }
            }
        }
    }
}

/// Prints reports in the background, in the order they came (the printer
/// takes the commands in that order too).
pub(crate) struct Reports {
    tx: Sender<Report>,
}

impl Reports {
    pub(crate) fn start(out: Out) -> Reports {
        let (tx, rx) = crossbeam_channel::unbounded::<Report>();
        // Ends with the shell (the sender), once the reports it holds are
        // settled: a closed connection loses every pending command.
        thread::Builder::new()
            .name("sprint-shell-reports".into())
            .spawn(move || {
                for mut report in rx {
                    let delivery = loop {
                        match report.tracked.poll() {
                            Delivery::Pending => thread::sleep(session::POLL * 5),
                            delivery => break delivery,
                        }
                    };
                    report.print(&out, delivery);
                }
            })
            .expect("spawn the report thread");
        Reports { tx }
    }

    /// Print `report` once the printer has taken its command.
    pub(crate) fn push(&self, report: Report) {
        let _ = self.tx.send(report);
    }
}

/// At the prompt, how long a command may keep the shell waiting before
/// its line follows in the background.
pub(crate) const PROMPT_GRACE: Duration = Duration::from_millis(500);
