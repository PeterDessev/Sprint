//! What the confirmations about a running job say, in one place, worded
//! after what actually happens for each [`JobKind`] (a host print runs the
//! cancel script, an upload deletes its partly written file).

use sprint_host::JobKind;

use crate::format;

/// What the toast after an emergency stop says: how to go on.
pub const ESTOP_MESSAGE: &str = "M112: the firmware halted. Reset the board from the red \
     banner to go on: after an emergency stop the firmware ignores M999 until its board \
     restarts.";

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
            let script = script_effect(cancel_script);
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

/// What the cancel script `cancel_script` (the printer profile's; empty:
/// none is set) does after a host print stops, in one sentence. A custom
/// script may do anything, so only the default's effect is named.
fn script_effect(cancel_script: &str) -> &'static str {
    if cancel_script.trim().is_empty() {
        "No cancel script is set in the printer profile: the heaters stay as they are."
    } else {
        "The printer profile's cancel script then runs (by default it parks the head and \
         turns the heaters off)."
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
/// closes (see [`crate::connection::close_link`]); `cancel_script` is the
/// printer profile's, which a cancelled host print runs (empty: none).
pub fn close_question(
    kind: JobKind,
    name: &str,
    closing: Closing,
    cancel_script: &str,
) -> JobQuestion {
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
                "{name} is cancelled first. {}",
                script_effect(cancel_script)
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

/// What the "Stopping before Sprint quits" dialog says while the job
/// `kind` named `name` is cancelled (see [`close_question`]).
pub fn quitting_message(kind: JobKind, name: &str, cancel_script: &str) -> String {
    let name = format::sanitize(name, 60);
    match kind {
        JobKind::SdUpload => format!(
            "Cancelling the upload of {name}: the partly written file is deleted from the \
             card. Sprint quits once that is done."
        ),
        JobKind::HostPrint | JobKind::SdPrint => format!(
            "Cancelling {name}. {} Sprint quits once the printer has stopped.",
            script_effect(cancel_script)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_questions_say_the_job_is_cancelled_first() {
        let script = "G91\nG1 Z10\nM104 S0";
        let q = close_question(JobKind::HostPrint, "cube.gcode", Closing::Quit, script);
        assert_eq!(q.title, "Quit and cancel the print?");
        assert!(q.message.contains("cancel script then runs"), "{q:?}");
        assert!(q.message.contains("heaters off"), "{q:?}");
        assert_eq!(q.confirm, "Cancel and quit");
        assert_eq!(q.keep, "Keep printing");
        // Without a cancel script nothing switches the heaters off.
        let q = close_question(JobKind::HostPrint, "cube.gcode", Closing::Quit, " ");
        assert!(q.message.contains("No cancel script"), "{q:?}");
        assert!(!q.message.contains("heaters off"), "{q:?}");
        let q = close_question(JobKind::SdUpload, "CUBE.GCO", Closing::Disconnect, "");
        assert_eq!(q.title, "Disconnect and cancel the upload?");
        assert!(q.message.contains("deleted from the card"), "{q:?}");
        assert_eq!(q.keep, "Keep uploading");
        let q = close_question(JobKind::HostPrint, "x\u{1b}[2J", Closing::Reconnect, script);
        assert_eq!(q.confirm, "Cancel and reconnect");
        assert!(!q.message.contains('\u{1b}'), "{q:?}");
    }

    #[test]
    fn the_quitting_message_says_what_the_cancel_does() {
        let m = quitting_message(JobKind::HostPrint, "cube.gcode", "M104 S0");
        assert!(m.starts_with("Cancelling cube.gcode."), "{m}");
        assert!(m.contains("heaters off"), "{m}");
        let m = quitting_message(JobKind::HostPrint, "cube.gcode", "");
        assert!(m.contains("No cancel script"), "{m}");
        assert!(!m.contains("heaters off"), "{m}");
        // An upload runs no cancel script.
        let m = quitting_message(JobKind::SdUpload, "CUBE.GCO", "M104 S0");
        assert!(m.contains("deleted from the card"), "{m}");
        assert!(!m.contains("cancel script"), "{m}");
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
