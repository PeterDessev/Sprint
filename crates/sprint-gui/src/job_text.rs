//! What the confirmations about a running job say, in one place, worded
//! after what actually happens for each [`JobKind`] (a host print runs the
//! cancel script, an upload deletes its partly written file).

use sprint_host::JobKind;

use crate::format;

/// A yes / no question about a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobQuestion {
    /// The dialog's title.
    pub title: String,
    /// What happens when the user confirms.
    pub message: String,
    /// The confirm button.
    pub confirm: &'static str,
    /// The button that keeps the job running.
    pub keep: &'static str,
}

/// The question before cancelling the job `kind` named `name`, `percent`
/// done (see [`format::progress_percent`]). `cancel_script` is the printer
/// profile's cancel script (host prints run it; empty: none is set). The
/// job bar, the Printer menu and the SD card tab all ask this.
pub fn cancel_question(
    kind: JobKind,
    name: &str,
    percent: u32,
    cancel_script: &str,
) -> JobQuestion {
    let name = format::sanitize(name, 60);
    match kind {
        JobKind::HostPrint => {
            let script = if cancel_script.trim().is_empty() {
                "No cancel script is set in the printer profile: the heaters stay as they are."
            } else {
                "The printer profile's cancel script then runs (by default it parks the head \
                 and turns the heaters off)."
            };
            JobQuestion {
                title: "Cancel the print?".to_owned(),
                message: format!(
                    "{name} stops at {percent}%. {script} The rest can be printed later by \
                     starting from a line."
                ),
                confirm: "Cancel print",
                keep: "Keep printing",
            }
        }
        JobKind::SdPrint => JobQuestion {
            title: "Stop the SD print?".to_owned(),
            message: format!(
                "{name} stops at {percent}%: the printer stops reading it from its card \
                 (M524). What it does then, parking or heaters, is up to its firmware."
            ),
            confirm: "Stop print",
            keep: "Keep printing",
        },
        JobKind::SdUpload => JobQuestion {
            title: "Cancel the upload?".to_owned(),
            message: format!(
                "The upload of {name} stops at {percent}% and the partly written file is \
                 deleted from the card (M29, M30)."
            ),
            confirm: "Cancel upload",
            keep: "Keep uploading",
        },
    }
}

/// What closes the link while a job runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closing {
    /// The connection is closed.
    Disconnect,
    /// The connection is closed and opened again.
    Reconnect,
    /// Sprint quits.
    Quit,
}

/// The question before `closing` the link while the host runs the job
/// `kind` named `name` (a host print or an SD upload; an SD print goes on
/// by itself and is not asked about). The job is cancelled before the link
/// closes (see [`crate::connection::close_link`]).
pub fn close_question(kind: JobKind, name: &str, closing: Closing) -> JobQuestion {
    let name = format::sanitize(name, 60);
    let upload = kind == JobKind::SdUpload;
    let (verb, confirm, then) = match closing {
        Closing::Disconnect => (
            "Disconnect",
            "Cancel and disconnect",
            "The connection closes once the printer has stopped.",
        ),
        Closing::Reconnect => (
            "Reconnect",
            "Cancel and reconnect",
            "Sprint reconnects once the printer has stopped.",
        ),
        Closing::Quit => (
            "Quit",
            "Cancel and quit",
            "Sprint quits once the printer has stopped.",
        ),
    };
    let (what, first, keep) = if upload {
        (
            "upload",
            format!(
                "The upload of {name} is cancelled first and the partly written file is \
                 deleted from the card."
            ),
            "Keep uploading",
        )
    } else {
        (
            "print",
            format!(
                "{name} is cancelled first: the cancel script turns the heaters off and \
                 parks the head."
            ),
            "Keep printing",
        )
    };
    JobQuestion {
        title: format!("{verb} and cancel the {what}?"),
        message: format!("{first} {then}"),
        confirm,
        keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_questions_say_the_job_is_cancelled_first() {
        let q = close_question(JobKind::HostPrint, "cube.gcode", Closing::Quit);
        assert_eq!(q.title, "Quit and cancel the print?");
        assert!(q.message.contains("heaters off"), "{q:?}");
        assert_eq!(q.confirm, "Cancel and quit");
        assert_eq!(q.keep, "Keep printing");
        let q = close_question(JobKind::SdUpload, "CUBE.GCO", Closing::Disconnect);
        assert_eq!(q.title, "Disconnect and cancel the upload?");
        assert!(q.message.contains("deleted from the card"), "{q:?}");
        assert_eq!(q.keep, "Keep uploading");
        let q = close_question(JobKind::HostPrint, "x\u{1b}[2J", Closing::Reconnect);
        assert_eq!(q.confirm, "Cancel and reconnect");
        assert!(!q.message.contains('\u{1b}'), "{q:?}");
    }

    #[test]
    fn cancel_questions_say_what_each_job_kind_does() {
        let q = cancel_question(JobKind::HostPrint, "cube.gcode", 42, "M104 S0");
        assert_eq!(q.title, "Cancel the print?");
        assert!(q.message.starts_with("cube.gcode stops at 42%."), "{q:?}");
        assert!(q.message.contains("cancel script"), "{q:?}");
        assert!(q.message.contains("starting from a line"), "{q:?}");
        assert!(!q.message.contains("cannot be resumed"), "{q:?}");
        let q = cancel_question(JobKind::HostPrint, "cube.gcode", 42, "  ");
        assert!(q.message.contains("No cancel script"), "{q:?}");
        let q = cancel_question(JobKind::SdPrint, "CUBE.GCO", 10, "M104 S0");
        assert_eq!(q.title, "Stop the SD print?");
        assert!(q.message.contains("M524"), "{q:?}");
        assert!(!q.message.contains("cancel script"), "{q:?}");
        let q = cancel_question(JobKind::SdUpload, "CUBE.GCO", 52, "M104 S0");
        assert_eq!(q.title, "Cancel the upload?");
        assert!(q.message.contains("deleted from the card"), "{q:?}");
        assert_eq!((q.confirm, q.keep), ("Cancel upload", "Keep uploading"));
    }
}
