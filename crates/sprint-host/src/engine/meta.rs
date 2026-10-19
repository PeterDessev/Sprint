//! What the engine remembers about every command it sends.

use std::sync::Arc;

use sprint_gcode::{Code, parse_line};

/// Where a command came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// `send`, convenience controls, macros.
    User,
    /// A query of a convenience method (`request_bed_mesh`, ...), often
    /// sent by a UI on its own: its `echo:` replies are not messages for
    /// the user.
    Query,
    /// Handshake and post-handshake queries.
    Setup,
    /// Periodic polls.
    Poll(PollKind),
    /// The `M105` sent after a timeout.
    Recovery,
    /// Heaters off (or the SD file closed) after the engine gave up on a
    /// job; written at once if the connection is closed before they were
    /// sent.
    SafeStop,
    /// Part of the current job.
    Job(JobLine),
}

/// Kind of periodic poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PollKind {
    /// M105.
    Temperature,
    /// M27.
    SdStatus,
}

/// Role of a job command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobLine {
    /// File line (0-based index).
    Stream(usize),
    /// [`PrintOptions::preamble`](crate::PrintOptions::preamble) line.
    Preamble,
    /// Pause / resume / cancel script line, or an SD control command.
    Script,
    /// `M73` progress report.
    Progress,
    /// Travel (and `G92 E`) back to the file's position after the moves of
    /// an excluded object were left out.
    Resync,
}

/// A command the engine sent or will send.
#[derive(Debug)]
pub(crate) struct CmdMeta {
    /// The clean command text.
    pub text: String,
    /// First command word (`None` for extended commands).
    pub code: Option<Code>,
    /// Where it came from.
    pub source: Source,
    /// Automatic traffic (console `auto` flag).
    pub auto: bool,
    /// Known to run long (homing, probing, heat waits, dwells, ...).
    pub long: bool,
}

pub(crate) type Meta = Arc<CmdMeta>;

impl CmdMeta {
    /// Classify `text`.
    pub(crate) fn new(text: impl Into<String>, source: Source) -> Meta {
        let text = text.into();
        let code = parse_line(&text).code;
        let long = match code {
            Some(c) => is_long(c),
            // Klipper extended commands (BED_MESH_CALIBRATE, ...).
            None => true,
        };
        let auto = matches!(source, Source::Poll(_));
        Arc::new(CmdMeta {
            text,
            code,
            source,
            auto,
            long,
        })
    }

    /// Whether the command is `letter` `number` (e.g. `'M', 503`).
    pub(crate) fn is(&self, letter: char, number: u16) -> bool {
        self.code.is_some_and(|c| c.is(letter, number))
    }

    /// M109 / M190 / M191: the firmware blocks until a temperature is
    /// reached (and reports temperatures meanwhile).
    pub(crate) fn is_heat_wait(&self) -> bool {
        self.is('M', 109) || self.is('M', 190) || self.is('M', 191)
    }

    /// Commands during which temperature polling is pointless (the
    /// firmware reports temperatures itself and the poll would only queue
    /// up behind them).
    pub(crate) fn blocks_polling(&self) -> bool {
        self.is_heat_wait() || self.is('M', 303)
    }

    /// Commands print-time estimates count as (nearly) zero although they
    /// take real time: heat waits, homing, probing and leveling, waits for
    /// the user, filament changes, autotune, extended (Klipper) commands.
    /// Dwells (`G4`) and `M400` are estimated, so they do not count.
    pub(crate) fn unestimated(&self) -> bool {
        self.long && !self.is('G', 4) && !self.is('M', 400)
    }

    /// Commands that wait for the user or a temperature, which `M108`
    /// releases.
    pub(crate) fn is_wait(&self) -> bool {
        self.is_heat_wait() || self.is('M', 0) || self.is('M', 1) || self.is('M', 600)
    }

    /// The job line role, if any.
    pub(crate) fn job_line(&self) -> Option<JobLine> {
        match self.source {
            Source::Job(l) => Some(l),
            _ => None,
        }
    }
}

fn is_long(c: Code) -> bool {
    match c.letter {
        b'G' => matches!(c.number, 4 | 28 | 29 | 30 | 32 | 33 | 34 | 35 | 76 | 425),
        b'M' => matches!(
            c.number,
            0 | 1 | 48 | 109 | 190 | 191 | 303 | 400 | 600 | 701 | 702 | 1001
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        let m = CmdMeta::new("G28 X", Source::User);
        assert!(m.long && m.is('G', 28) && !m.auto);
        let m = CmdMeta::new("M109 S200", Source::Job(JobLine::Stream(3)));
        assert!(m.is_heat_wait() && m.blocks_polling() && m.is_wait());
        assert_eq!(m.job_line(), Some(JobLine::Stream(3)));
        let m = CmdMeta::new("M105", Source::Poll(PollKind::Temperature));
        assert!(m.auto && !m.long);
        let m = CmdMeta::new("G1 X10", Source::User);
        assert!(!m.long && !m.is_wait() && m.job_line().is_none());
        let m = CmdMeta::new("BED_MESH_CALIBRATE", Source::User);
        assert!(m.code.is_none() && m.long);
        assert!(CmdMeta::new("M600", Source::User).is_wait());
        for unestimated in [
            "G28",
            "G29",
            "M109 S200",
            "M190 S60",
            "M0",
            "M600",
            "START_PRINT",
        ] {
            assert!(
                CmdMeta::new(unestimated, Source::User).unestimated(),
                "{unestimated}"
            );
        }
        for estimated in ["G4 P500", "M400", "G1 X10", "M104 S200"] {
            assert!(
                !CmdMeta::new(estimated, Source::User).unestimated(),
                "{estimated}"
            );
        }
    }
}
