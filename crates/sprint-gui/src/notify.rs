//! Turns printer [`Event`]s into toasts: job results, errors, autotune
//! results, connection changes and (rate-limited) firmware messages.

use sprint_host::{ConnectionStatus, Event, Heater, JobStatus};

use crate::format;
use crate::widgets::{ToastKind, Toasts};

/// Minimum seconds between two firmware-message toasts.
pub const MESSAGE_INTERVAL: f64 = 4.0;
/// Minimum seconds between two identical error toasts.
pub const ERROR_INTERVAL: f64 = 3.0;

/// A toast to show.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// Severity.
    pub kind: ToastKind,
    /// Title line.
    pub title: String,
    /// Optional detail.
    pub message: Option<String>,
}

impl Notice {
    fn new(kind: ToastKind, title: impl Into<String>, message: Option<String>) -> Notice {
        Notice {
            kind,
            title: title.into(),
            message,
        }
    }

    /// Add it to `toasts`.
    pub fn show(self, toasts: &mut Toasts) {
        let toast = toasts.add(self.kind, self.title);
        if let Some(message) = self.message {
            toast.message(message);
        }
    }
}

/// Stateful event-to-toast mapping (remembers what it showed recently).
#[derive(Debug, Default)]
pub struct Notifier {
    last_message: Option<(f64, String)>,
    last_error: Option<(f64, String)>,
    was_connected: bool,
    last_job: Option<String>,
}

impl Notifier {
    /// The toast for `event` at time `now` (seconds, monotonic), if any.
    /// `job_name` is the name of the current or last job.
    pub fn notice(&mut self, event: &Event, now: f64, job_name: Option<&str>) -> Option<Notice> {
        if let Some(name) = job_name {
            self.last_job = Some(name.to_owned());
        }
        let job = self.last_job.as_deref().map(|n| format::sanitize(n, 60));
        match event {
            Event::Job(status) => match status {
                JobStatus::Completed => {
                    Some(Notice::new(ToastKind::Success, "Print finished", job))
                }
                JobStatus::Cancelled => Some(Notice::new(ToastKind::Info, "Print cancelled", job)),
                JobStatus::Failed(reason) => Some(Notice::new(
                    ToastKind::Error,
                    "Print failed",
                    Some(format::sanitize(reason, 200)),
                )),
                _ => None,
            },
            Event::Error(message) => {
                let message = format::sanitize(message, 200);
                let repeated = self
                    .last_error
                    .as_ref()
                    .is_some_and(|(t, m)| *m == message && now - *t < ERROR_INTERVAL && now >= *t);
                self.last_error = Some((now, message.clone()));
                (!repeated).then(|| Notice::new(ToastKind::Error, "Printer error", Some(message)))
            }
            Event::AutotuneFinished { heater, kp, ki, kd } => Some(Notice::new(
                ToastKind::Success,
                format!("PID autotune finished ({})", heater_name(*heater)),
                Some(format!("Kp {kp:.2} · Ki {ki:.2} · Kd {kd:.2}")),
            )),
            Event::AutotuneFailed(reason) => Some(Notice::new(
                ToastKind::Error,
                "PID autotune failed",
                Some(format::sanitize(reason, 200)),
            )),
            Event::FirmwareMessage(message) => {
                let message = format::sanitize(message.trim(), 160);
                if message.is_empty() {
                    return None;
                }
                let recent = self.last_message.as_ref().is_some_and(|(t, m)| {
                    (now - *t < MESSAGE_INTERVAL && now >= *t) || *m == message
                });
                if recent {
                    return None;
                }
                self.last_message = Some((now, message.clone()));
                Some(Notice::new(ToastKind::Info, "Printer says", Some(message)))
            }
            Event::Status(status) => self.status_notice(status),
            _ => None,
        }
    }

    fn status_notice(&mut self, status: &ConnectionStatus) -> Option<Notice> {
        match status {
            ConnectionStatus::Connected => {
                self.was_connected = true;
                None
            }
            ConnectionStatus::Disconnected if self.was_connected => {
                self.was_connected = false;
                Some(Notice::new(ToastKind::Info, "Disconnected", None))
            }
            ConnectionStatus::Failed(reason) => {
                self.was_connected = false;
                Some(Notice::new(
                    ToastKind::Error,
                    "Connection failed",
                    Some(format::sanitize(reason, 200)),
                ))
            }
            _ => None,
        }
    }
}

fn heater_name(heater: Heater) -> String {
    match heater {
        Heater::Hotend(n) => format!("hotend {}", u16::from(n) + 1),
        Heater::Bed => "bed".to_owned(),
        Heater::Chamber => "chamber".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_results() {
        let mut n = Notifier::default();
        let done = n
            .notice(&Event::Job(JobStatus::Completed), 0.0, Some("cube.gcode"))
            .expect("toast");
        assert_eq!(done.kind, ToastKind::Success);
        assert_eq!(done.message.as_deref(), Some("cube.gcode"));
        assert!(
            n.notice(&Event::Job(JobStatus::Printing), 0.0, None)
                .is_none()
        );
        let failed = n
            .notice(
                &Event::Job(JobStatus::Failed("printer reset".into())),
                1.0,
                None,
            )
            .expect("toast");
        assert_eq!(failed.kind, ToastKind::Error);
        assert_eq!(failed.message.as_deref(), Some("printer reset"));
        let cancelled = n
            .notice(&Event::Job(JobStatus::Cancelled), 2.0, None)
            .expect("toast");
        // The last job name is remembered.
        assert_eq!(cancelled.message.as_deref(), Some("cube.gcode"));
    }

    #[test]
    fn firmware_messages_are_rate_limited() {
        let mut n = Notifier::default();
        let msg = |s: &str| Event::FirmwareMessage(s.into());
        assert!(n.notice(&msg("Heating..."), 0.0, None).is_some());
        assert!(n.notice(&msg("Other"), 1.0, None).is_none());
        assert!(
            n.notice(&msg("Heating..."), 10.0, None).is_none(),
            "same text"
        );
        assert!(n.notice(&msg("Printing"), 10.0, None).is_some());
        assert!(n.notice(&msg("   "), 30.0, None).is_none());
    }

    #[test]
    fn errors_merge_when_repeated_quickly() {
        let mut n = Notifier::default();
        let e = Event::Error("checksum".into());
        assert!(n.notice(&e, 0.0, None).is_some());
        assert!(n.notice(&e, 1.0, None).is_none());
        assert!(n.notice(&e, 5.0, None).is_some());
        assert!(n.notice(&Event::Error("other".into()), 5.1, None).is_some());
    }

    #[test]
    fn connection_changes() {
        let mut n = Notifier::default();
        assert!(
            n.notice(&Event::Status(ConnectionStatus::Disconnected), 0.0, None)
                .is_none(),
            "nothing to report before a connection"
        );
        assert!(
            n.notice(&Event::Status(ConnectionStatus::Connected), 0.0, None)
                .is_none()
        );
        let lost = n
            .notice(&Event::Status(ConnectionStatus::Disconnected), 1.0, None)
            .expect("toast");
        assert_eq!(lost.title, "Disconnected");
        let failed = n
            .notice(
                &Event::Status(ConnectionStatus::Failed("no port".into())),
                2.0,
                None,
            )
            .expect("toast");
        assert_eq!(failed.kind, ToastKind::Error);
    }

    #[test]
    fn autotune_results() {
        let mut n = Notifier::default();
        let ok = n
            .notice(
                &Event::AutotuneFinished {
                    heater: Heater::Hotend(0),
                    kp: 22.2,
                    ki: 1.08,
                    kd: 114.0,
                },
                0.0,
                None,
            )
            .expect("toast");
        assert!(ok.title.contains("hotend 1"));
        assert!(ok.message.as_deref().unwrap_or("").contains("22.20"));
        let failed = n
            .notice(&Event::AutotuneFailed("timeout".into()), 0.0, None)
            .expect("toast");
        assert_eq!(failed.kind, ToastKind::Error);
        let mut toasts = Toasts::new();
        failed.show(&mut toasts);
        assert_eq!(toasts.len(), 1);
    }
}
