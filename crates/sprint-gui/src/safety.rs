//! What the user sends, checked with the shared command-safety classifier
//! ([`sprint_proto::classify`]): typed and pasted console lines, the
//! console's quick commands, a line sent from the G-code tab, macros and
//! raw settings. Every front end (GUI, TUI, shell) asks the same
//! classifier, so a line gets the same answer everywhere.
//!
//! [`assess`] reads a block of lines in the [`job_context`] of the frame
//! and says whether it may go out as it is, needs a confirmation (with the
//! reasons to show) or must not be sent. M112 is never asked about: an
//! emergency stop goes out at once.

use sprint_host::{JobKind, JobStatus, PrinterState};
use sprint_proto::{Decision, JobContext, SafetyReason, Verdict, classify};

/// What the job is doing, as the classifier reads it:
///
/// * a host print that is [`JobStatus::Paused`] is [`JobContext::Paused`]:
///   moves and extrusion are fine, the host restores the position, the
///   modes, the temperatures and the fan when it resumes;
/// * any other active job (printing, pausing, resuming, an SD print or an
///   upload), and a print the printer runs from its card, is
///   [`JobContext::Printing`]: a paused SD print resumes where the head
///   is, so it is treated as printing;
/// * otherwise [`JobContext::Idle`].
pub fn job_context(state: &PrinterState) -> JobContext {
    match state.job.as_ref().filter(|j| j.status.is_active()) {
        Some(job) if job.kind == JobKind::HostPrint && job.status == JobStatus::Paused => {
            JobContext::Paused
        }
        Some(_) => JobContext::Printing,
        None if crate::context::printer_busy(state) => JobContext::Printing,
        None => JobContext::Idle,
    }
}

/// The classifier's answer for a block of lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Assessment {
    /// The block holds an M112: send the emergency stop at once (the
    /// firmware halts, nothing else in the block matters any more).
    pub emergency_stop: bool,
    /// Why the block must not be sent ("M28 (Start SD write): …"), one
    /// sentence per command and reason.
    pub blocked: Vec<String>,
    /// Why the block needs a confirmation, one sentence per command and
    /// reason.
    pub confirm: Vec<String>,
    /// Some confirmation is for a dangerous command (settings wipe,
    /// firmware restart, forced position) rather than for the job.
    pub dangerous: bool,
    /// The lines that send something.
    pub commands: usize,
}

impl Assessment {
    /// Nothing stands in the way: send it.
    pub fn is_clear(&self) -> bool {
        !self.emergency_stop && self.blocked.is_empty() && self.confirm.is_empty()
    }

    /// The title of the confirmation for the block, sent while the job is
    /// in `ctx`.
    pub fn title(&self, ctx: JobContext) -> &'static str {
        if self.dangerous {
            "Send a dangerous command?"
        } else {
            match ctx {
                JobContext::Paused => "Send this while the print is paused?",
                JobContext::Printing => "Send this during the print?",
                JobContext::Idle => "Send this command?",
            }
        }
    }

    /// The confirmation's message: a line on the job, then the reasons.
    pub fn message(&self, ctx: JobContext) -> String {
        let mut lines = Vec::new();
        match ctx {
            JobContext::Paused => {
                lines.push("A print is paused, to go on from where it stopped:".to_owned())
            }
            JobContext::Printing => lines.push("A print is running:".to_owned()),
            JobContext::Idle => {}
        }
        lines.extend(self.confirm.iter().cloned());
        lines.join("\n")
    }
}

/// Reasons that are about the command itself, not about the job.
fn is_dangerous(reason: SafetyReason) -> bool {
    matches!(
        reason,
        SafetyReason::Dangerous | SafetyReason::Restarts | SafetyReason::ForcesPosition
    )
}

/// "M502 (Restore factory settings): it can wipe settings, …." for a
/// verdict with a reason.
pub fn reason_text(verdict: &Verdict) -> Option<String> {
    let reason = verdict.reason()?;
    let what = match (&verdict.command, verdict.doc) {
        (Some(code), Some(doc)) => format!("{code} ({})", doc.name),
        (Some(code), None) => code.clone(),
        // Parameter-only lines ("X10 Y5") move like the last G0/G1.
        (None, _) => "A move".to_owned(),
    };
    Some(format!("{what}: {}.", reason.message()))
}

/// Classify every line of `lines` (raw text: comments, line numbers and
/// checksums are read as the firmware reads them) while the job is in
/// `ctx`.
pub fn assess<S: AsRef<str>>(lines: &[S], ctx: JobContext) -> Assessment {
    let mut out = Assessment::default();
    for line in lines {
        let verdict = classify(line.as_ref(), ctx);
        if verdict.command.is_some() || verdict.reason().is_some() {
            out.commands += 1;
        }
        if verdict.emergency && verdict.command.as_deref() == Some("M112") {
            out.emergency_stop = true;
            continue;
        }
        let Some(text) = reason_text(&verdict) else {
            continue;
        };
        match verdict.decision {
            Decision::Allow => {}
            Decision::Confirm(reason) => {
                out.dangerous |= is_dangerous(reason);
                if !out.confirm.contains(&text) {
                    out.confirm.push(text);
                }
            }
            Decision::Block(_) => {
                if !out.blocked.contains(&text) {
                    out.blocked.push(text);
                }
            }
        }
    }
    out
}

/// [`assess`] in the job context of `state`.
pub fn assess_now<S: AsRef<str>>(lines: &[S], state: &PrinterState) -> Assessment {
    assess(lines, job_context(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::JobInfo;

    fn state(kind: JobKind, status: JobStatus) -> PrinterState {
        let mut job = JobInfo::new(kind, "x", 10, 10);
        job.status = status;
        PrinterState {
            job: Some(job),
            ..PrinterState::default()
        }
    }

    #[test]
    fn job_contexts() {
        assert_eq!(job_context(&PrinterState::default()), JobContext::Idle);
        assert_eq!(
            job_context(&state(JobKind::HostPrint, JobStatus::Printing)),
            JobContext::Printing
        );
        assert_eq!(
            job_context(&state(JobKind::HostPrint, JobStatus::Paused)),
            JobContext::Paused
        );
        // Pausing and resuming are still moving.
        for status in [JobStatus::Pausing, JobStatus::Resuming, JobStatus::Starting] {
            assert_eq!(
                job_context(&state(JobKind::HostPrint, status)),
                JobContext::Printing
            );
        }
        // An SD print resumes where the head is: never "paused" for moves.
        assert_eq!(
            job_context(&state(JobKind::SdPrint, JobStatus::Paused)),
            JobContext::Printing
        );
        assert_eq!(
            job_context(&state(JobKind::SdUpload, JobStatus::Printing)),
            JobContext::Printing
        );
        // Finished jobs are history.
        assert_eq!(
            job_context(&state(JobKind::HostPrint, JobStatus::Cancelled)),
            JobContext::Idle
        );
        // A print started on the printer's display.
        let card = PrinterState {
            sd: sprint_host::SdState {
                printing: Some((10, 100)),
                ..Default::default()
            },
            ..PrinterState::default()
        };
        assert_eq!(job_context(&card), JobContext::Printing);
    }

    #[test]
    fn blocks_are_assessed_line_by_line() {
        let idle = assess(&["G28", "M105", "; note", ""], JobContext::Idle);
        assert!(idle.is_clear());
        assert_eq!(idle.commands, 2);

        let printing = assess(
            &["M105", "G28 X", "g28 x", "M106 S255"],
            JobContext::Printing,
        );
        assert!(!printing.is_clear());
        assert_eq!(printing.confirm.len(), 1, "{:?}", printing.confirm);
        assert!(
            printing.confirm[0].starts_with("G28 ("),
            "{:?}",
            printing.confirm
        );
        assert!(!printing.dangerous);
        assert_eq!(
            printing.title(JobContext::Printing),
            "Send this during the print?"
        );
        assert!(printing.message(JobContext::Printing).contains("running"));

        // Paused: moves are fine, homing Z asks.
        assert!(assess(&["G91", "G1 Z5", "G1 E-2"], JobContext::Paused).is_clear());
        let home = assess(&["G28"], JobContext::Paused);
        assert_eq!(home.confirm.len(), 1);
        assert!(home.message(JobContext::Paused).contains("paused"));

        // Dangerous commands ask even when idle; M28 is refused mid-print.
        let reset = assess(&["N10 M502*45"], JobContext::Idle);
        assert!(reset.dangerous);
        assert_eq!(reset.title(JobContext::Idle), "Send a dangerous command?");
        let sd = assess(&["M28 x.gco"], JobContext::Printing);
        assert_eq!(sd.blocked.len(), 1);
        assert!(sd.confirm.is_empty());

        // M112 is never asked about, whatever comes with it.
        let stop = assess(&["G28", "m112"], JobContext::Printing);
        assert!(stop.emergency_stop);
        let quick = assess(&["M410"], JobContext::Printing);
        assert!(!quick.emergency_stop && quick.confirm.len() == 1);
        assert!(assess(&["M108", "M876 S0"], JobContext::Printing).is_clear());

        // Parameter-only lines read as moves.
        let words = assess(&["X10 Y5"], JobContext::Printing);
        assert!(
            words.confirm[0].starts_with("A move"),
            "{:?}",
            words.confirm
        );
    }
}
