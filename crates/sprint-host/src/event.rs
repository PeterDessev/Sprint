//! Change notifications. Subscribe with
//! [`Printer::subscribe`](crate::Printer::subscribe); every subscriber gets
//! every event.
//!
//! Events are notifications, the [`PrinterState`](crate::PrinterState) is
//! the truth: a subscriber that stops reading has at most
//! [`EVENT_QUEUE_LIMIT`] undelivered events queued, newer ones are dropped
//! for it until it catches up. Dropped receivers are forgotten.

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use sprint_proto::{BedMesh, FirmwareInfo, PositionReport, SdFile, StatsReport, TempReport};

use crate::console::ConsoleEntry;
use crate::job::JobStatus;
use crate::state::{ConnectionStatus, Heater, HostPrompt};

/// Undelivered events kept per subscriber.
pub const EVENT_QUEUE_LIMIT: usize = 65_536;

/// Something changed.
#[derive(Clone, Debug)]
pub enum Event {
    /// The connection status changed.
    Status(ConnectionStatus),
    /// A console line was added.
    Console(ConsoleEntry),
    /// New temperatures (merged into the state).
    Temperatures(TempReport),
    /// New position report.
    Position(PositionReport),
    /// M115 report.
    Firmware(FirmwareInfo),
    /// A complete M503 report replaced `state.settings`.
    SettingsUpdated,
    /// The job status changed.
    Job(JobStatus),
    /// Job progress (rate limited): `line` of `total` lines done.
    JobProgress {
        /// Lines processed (see [`JobInfo::current_line`](crate::JobInfo::current_line)).
        line: usize,
        /// Lines in the job.
        total: usize,
    },
    /// A complete SD card listing.
    SdFiles(Vec<SdFile>),
    /// A complete M119 report.
    Endstops(Vec<(String, bool)>),
    /// A bed leveling grid.
    Mesh(BedMesh),
    /// A complete M78 report.
    Stats(StatsReport),
    /// PID autotune succeeded.
    AutotuneFinished {
        /// Tuned heater.
        heater: Heater,
        /// Proportional gain.
        kp: f32,
        /// Integral gain.
        ki: f32,
        /// Derivative gain.
        kd: f32,
    },
    /// PID autotune failed.
    AutotuneFailed(String),
    /// A host prompt was shown (`Some`) or closed (`None`).
    Prompt(Option<HostPrompt>),
    /// A message from the firmware (`echo:`, `//action:notification`).
    FirmwareMessage(String),
    /// A firmware or host error (halts, failed jobs, lost links).
    Error(String),
}

/// Fan-out of events to subscribers.
#[derive(Default)]
pub struct EventBus {
    subscribers: Mutex<Vec<Sender<Event>>>,
}

impl EventBus {
    /// A new receiver of every future event.
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = crossbeam_channel::bounded(EVENT_QUEUE_LIMIT);
        self.subscribers.lock().push(tx);
        rx
    }

    /// Deliver `event` to every subscriber, forgetting dropped ones.
    pub fn emit(&self, event: Event) {
        let mut subs = self.subscribers.lock();
        if subs.is_empty() {
            return;
        }
        subs.retain(|tx| match tx.try_send(event.clone()) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Number of live subscribers (dropped ones are pruned on the next
    /// event).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.subscribers.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_out_and_pruning() {
        let bus = EventBus::default();
        bus.emit(Event::SettingsUpdated); // nobody listening
        let a = bus.subscribe();
        let b = bus.subscribe();
        bus.emit(Event::FirmwareMessage("hi".into()));
        assert!(matches!(a.try_recv(), Ok(Event::FirmwareMessage(m)) if m == "hi"));
        assert!(matches!(b.try_recv(), Ok(Event::FirmwareMessage(_))));
        drop(b);
        bus.emit(Event::SettingsUpdated);
        assert_eq!(bus.len(), 1);
        assert!(matches!(a.try_recv(), Ok(Event::SettingsUpdated)));
    }

    #[test]
    fn a_stuck_subscriber_is_bounded() {
        let bus = EventBus::default();
        let rx = bus.subscribe();
        for _ in 0..EVENT_QUEUE_LIMIT + 10 {
            bus.emit(Event::SettingsUpdated);
        }
        assert_eq!(rx.len(), EVENT_QUEUE_LIMIT);
        assert_eq!(bus.len(), 1);
    }
}
