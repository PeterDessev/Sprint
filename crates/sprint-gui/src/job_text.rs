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
}
