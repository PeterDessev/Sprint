//! Everything the host knows about the printer: connection status,
//! firmware, temperatures, position, settings, SD card, prompts, the
//! current job and link statistics. Read it with
//! [`Printer::with_state`](crate::Printer::with_state) or
//! [`Printer::snapshot`](crate::Printer::snapshot).

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use sprint_proto::{
    BedMesh, FirmwareInfo, PositionReport, PrinterSettings, SdFile, StatsReport, TempReport,
};

use crate::job::JobInfo;

/// Most temperature samples kept in [`PrinterState::temp_history`] (30
/// minutes at one sample per second).
pub const TEMP_HISTORY_LEN: usize = 1800;
/// Time span of [`PrinterState::temp_history`]: older samples are dropped
/// (seconds).
pub const TEMP_HISTORY_SECS: f64 = 1800.0;
/// Firmware messages kept in [`PrinterState::firmware_messages`].
pub const FIRMWARE_MESSAGES_LEN: usize = 50;

/// The state of the printer as seen by the host.
#[derive(Clone, Debug)]
pub struct PrinterState {
    /// Connection status.
    pub status: ConnectionStatus,
    /// [`ConnectTarget::label`](crate::ConnectTarget::label) of the current
    /// or last connection.
    pub target: Option<String>,
    /// M115 report.
    pub firmware: Option<FirmwareInfo>,
    /// `Cap:` lines of the M115 report, in report order.
    pub capabilities: Vec<(String, bool)>,
    /// Latest temperatures (fields missing from a report keep their last
    /// value).
    pub temps: TempReport,
    /// About one sample per second, last 30 minutes.
    pub temp_history: VecDeque<TempSample>,
    /// Last M114 / M154 report.
    pub position: Option<PositionReport>,
    /// Settings from the last complete M503 report (plus optimistic
    /// [`Printer::apply_setting`](crate::Printer::apply_setting) edits).
    pub settings: PrinterSettings,
    /// An M503 report has completed since connecting.
    pub settings_loaded: bool,
    /// The current or last job.
    pub job: Option<JobInfo>,
    /// SD card.
    pub sd: SdState,
    /// Last M119 report.
    pub endstops: Vec<(String, bool)>,
    /// Last bed leveling grid (M420 V, G29, BED_MESH_OUTPUT).
    pub mesh: Option<BedMesh>,
    /// Last M78 report.
    pub stats: Option<StatsReport>,
    /// Axes known to be homed. `None`: unknown, nothing since connecting
    /// says (the printer may have been homed from its display). `Some`:
    /// the axes homed by an acknowledged `G28` (all, or the listed ones;
    /// from any source, the job stream included) since homing was last
    /// lost; the others count as not homed. `M84`/`M18` (all or the listed
    /// axes), halts and board resets clear it to `Some` of no axes, a
    /// disconnect to `None`. The pause and cancel scripts never move an
    /// axis that is not known to be homed.
    pub homed: Option<Axes>,
    /// Z babystepping (`M290 Z`/`S`, from any source, the job stream
    /// included) acknowledged since Z was last homed (Marlin forgets
    /// babysteps when it homes), the printer reset or halted, or the
    /// connection opened; mm, negative = closer to the bed.
    pub babystep_z: f32,
    /// Active tool (`T<n>` / "Active Extruder").
    pub active_tool: u8,
    /// Feedrate override, last set via M220 (100 default).
    pub feedrate_pct: f32,
    /// Flow override per tool (100 default).
    pub flow_pct: Vec<f32>,
    /// Fan speeds 0..=255 per fan index.
    pub fan_speeds: Vec<u8>,
    /// Firmware busy message ("processing", "paused for user"), cleared on
    /// `ok`.
    pub busy: Option<String>,
    /// Active `//action:prompt`.
    pub prompt: Option<HostPrompt>,
    /// Last or running PID autotune.
    pub autotune: Option<AutotuneState>,
    /// Link statistics.
    pub link: LinkStats,
    /// Firmware halted/killed (M112, thermal runaway, ...): reset the board
    /// ([`Printer::reset_board`](crate::Printer::reset_board)), or restart
    /// it when [`PrinterState::restartable`].
    pub halted: bool,
    /// While [`PrinterState::halted`]: the firmware can be restarted in
    /// place ([`Printer::firmware_restart`](crate::Printer::firmware_restart):
    /// M999 after Marlin's "Printer stopped ... use M999", FIRMWARE_RESTART
    /// on Klipper). False after a kill (M112, thermal runaway, "Printer
    /// halted. kill() called!"): Marlin then ignores everything, M999
    /// included, until the board is reset or power-cycled, so UIs should
    /// offer [`Printer::reset_board`](crate::Printer::reset_board) first.
    pub restartable: bool,
    /// Last error reported by the firmware or the host.
    pub last_error: Option<String>,
    /// Recent `echo:` / action notification messages (last 50).
    pub firmware_messages: VecDeque<String>,
}

impl Default for PrinterState {
    fn default() -> Self {
        PrinterState {
            status: ConnectionStatus::Disconnected,
            target: None,
            firmware: None,
            capabilities: Vec::new(),
            temps: TempReport::default(),
            temp_history: VecDeque::new(),
            position: None,
            settings: PrinterSettings::default(),
            settings_loaded: false,
            job: None,
            sd: SdState::default(),
            endstops: Vec::new(),
            mesh: None,
            stats: None,
            homed: None,
            babystep_z: 0.0,
            active_tool: 0,
            feedrate_pct: 100.0,
            flow_pct: vec![100.0],
            fan_speeds: vec![0],
            busy: None,
            prompt: None,
            autotune: None,
            link: LinkStats::default(),
            halted: false,
            restartable: false,
            last_error: None,
            firmware_messages: VecDeque::new(),
        }
    }
}

impl PrinterState {
    /// The handshake has completed.
    pub fn is_connected(&self) -> bool {
        self.status == ConnectionStatus::Connected
    }

    /// Whether the firmware reported capability `name` as enabled
    /// (case-insensitive).
    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities
            .iter()
            .any(|(n, on)| *on && n.eq_ignore_ascii_case(name))
    }

    /// A job is running (or paused).
    pub fn job_active(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.status.is_active())
    }

    /// Append to [`PrinterState::firmware_messages`], keeping the last
    /// [`FIRMWARE_MESSAGES_LEN`].
    pub fn push_message(&mut self, message: String) {
        while self.firmware_messages.len() >= FIRMWARE_MESSAGES_LEN {
            self.firmware_messages.pop_front();
        }
        self.firmware_messages.push_back(message);
    }
}

/// Connection status.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ConnectionStatus {
    /// No connection.
    #[default]
    Disconnected,
    /// Opening the link.
    Connecting,
    /// Link open, waiting for the firmware to answer.
    Handshaking,
    /// Talking to the firmware.
    Connected,
    /// Opening, the handshake or the link failed.
    Failed(String),
}

impl ConnectionStatus {
    /// `Connected`.
    pub fn is_connected(&self) -> bool {
        *self == ConnectionStatus::Connected
    }

    /// A link is open or being opened (`Connecting`, `Handshaking`,
    /// `Connected`).
    pub fn is_open(&self) -> bool {
        matches!(
            self,
            ConnectionStatus::Connecting
                | ConnectionStatus::Handshaking
                | ConnectionStatus::Connected
        )
    }

    /// Short label for status bars.
    pub fn label(&self) -> &str {
        match self {
            ConnectionStatus::Disconnected => "Disconnected",
            ConnectionStatus::Connecting => "Connecting",
            ConnectionStatus::Handshaking => "Handshaking",
            ConnectionStatus::Connected => "Connected",
            ConnectionStatus::Failed(_) => "Failed",
        }
    }
}

/// One point of the temperature history.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TempSample {
    /// Seconds since the connection was opened.
    pub t: f64,
    /// (current, target) per hotend (target 0 when unknown).
    pub hotends: Vec<(f32, f32)>,
    /// (current, target) of the bed.
    pub bed: Option<(f32, f32)>,
    /// (current, target) of the chamber.
    pub chamber: Option<(f32, f32)>,
    /// Heater power 0..=1 per hotend (0 when unknown).
    pub hotend_power: Vec<f32>,
    /// Bed heater power 0..=1.
    pub bed_power: Option<f32>,
}

/// SD card state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SdState {
    /// Card inserted/initialised (unknown until reported).
    pub card_present: Option<bool>,
    /// Files from the last complete listing.
    pub files: Vec<SdFile>,
    /// A listing is being received.
    pub listing: bool,
    /// (bytes done, total bytes) of a running SD print.
    pub printing: Option<(u64, u64)>,
    /// File selected with M23.
    pub selected: Option<String>,
}

/// An active host prompt (`//action:prompt_begin` ... `prompt_show`).
#[derive(Clone, Debug, PartialEq)]
pub struct HostPrompt {
    /// Prompt text.
    pub message: String,
    /// Button labels; answer with
    /// [`Printer::answer_prompt`](crate::Printer::answer_prompt)(index).
    pub buttons: Vec<String>,
}

/// A PID autotune run.
#[derive(Clone, Debug, PartialEq)]
pub struct AutotuneState {
    /// Heater being tuned.
    pub heater: Heater,
    /// Target temperature (°C).
    pub target: f32,
    /// Number of cycles.
    pub cycles: u32,
    /// Still running.
    pub running: bool,
    /// (Kp, Ki, Kd) on success.
    pub result: Option<(f32, f32, f32)>,
    /// Failure message.
    pub error: Option<String>,
}

/// Link statistics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkStats {
    /// Seconds since the link was opened.
    pub connected_secs: f64,
    /// Lines written (including resent lines).
    pub lines_sent: u64,
    /// Bytes written.
    pub bytes_sent: u64,
    /// Bytes read.
    pub bytes_received: u64,
    /// `ok`s received.
    pub oks: u64,
    /// `Resend:` requests received.
    pub resends: u64,
    /// `Error:` lines received.
    pub errors: u64,
    /// `ok` timeouts recovered from.
    pub timeouts: u64,
    /// Round trip (send to `ok`) of the last acknowledged command, ms.
    /// Commands that block in the firmware by design (homing, probing,
    /// heat waits, dwells, `M400`, user waits, autotune) are left out of
    /// the round-trip statistics.
    pub rtt_last_ms: f32,
    /// Smoothed round trip, ms.
    pub rtt_avg_ms: f32,
    /// Largest round trip, ms.
    pub rtt_max_ms: f32,
    /// Commands waiting to be sent (manual queue, scripts, pending resends).
    pub queue_len: usize,
    /// Commands awaiting their `ok`.
    pub in_flight: usize,
    /// Commands acknowledged per second over the last 2 seconds.
    pub lines_per_sec: f32,
}

/// A heater.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Heater {
    /// Hotend of the given tool.
    Hotend(u8),
    /// Heated bed.
    Bed,
    /// Heated chamber.
    Chamber,
}

impl Heater {
    /// Display name ("Hotend 0", "Bed", "Chamber").
    pub fn name(&self) -> String {
        match self {
            Heater::Hotend(t) => format!("Hotend {t}"),
            Heater::Bed => "Bed".to_string(),
            Heater::Chamber => "Chamber".to_string(),
        }
    }

    /// The M303 `E` index: tool number, -1 for the bed, -2 for the chamber.
    pub fn autotune_index(&self) -> i32 {
        match self {
            Heater::Hotend(t) => i32::from(*t),
            Heater::Bed => -1,
            Heater::Chamber => -2,
        }
    }

    /// Inverse of [`Heater::autotune_index`].
    pub fn from_autotune_index(index: i64) -> Option<Heater> {
        match index {
            -1 => Some(Heater::Bed),
            -2 => Some(Heater::Chamber),
            i => u8::try_from(i).ok().map(Heater::Hotend),
        }
    }
}

/// A motion axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    /// X.
    X,
    /// Y.
    Y,
    /// Z.
    Z,
    /// Extruder.
    E,
}

impl Axis {
    /// The G-code letter.
    pub fn letter(&self) -> char {
        match self {
            Axis::X => 'X',
            Axis::Y => 'Y',
            Axis::Z => 'Z',
            Axis::E => 'E',
        }
    }
}

/// A set of axes for homing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Axes {
    /// X.
    pub x: bool,
    /// Y.
    pub y: bool,
    /// Z.
    pub z: bool,
}

impl Axes {
    /// X, Y and Z.
    pub const ALL: Axes = Axes {
        x: true,
        y: true,
        z: true,
    };
    /// X and Y.
    pub const XY: Axes = Axes {
        x: true,
        y: true,
        z: false,
    };
    /// X only.
    pub const X: Axes = Axes {
        x: true,
        y: false,
        z: false,
    };
    /// Y only.
    pub const Y: Axes = Axes {
        x: false,
        y: true,
        z: false,
    };
    /// Z only.
    pub const Z: Axes = Axes {
        x: false,
        y: false,
        z: true,
    };

    /// No axis.
    pub const NONE: Axes = Axes {
        x: false,
        y: false,
        z: false,
    };

    /// Whether `axis` is in the set (never for [`Axis::E`]).
    pub fn has(&self, axis: Axis) -> bool {
        match axis {
            Axis::X => self.x,
            Axis::Y => self.y,
            Axis::Z => self.z,
            Axis::E => false,
        }
    }

    /// The axes in either set.
    pub fn union(self, other: Axes) -> Axes {
        Axes {
            x: self.x || other.x,
            y: self.y || other.y,
            z: self.z || other.z,
        }
    }

    /// The axes of `self` that are not in `other`.
    pub fn without(self, other: Axes) -> Axes {
        Axes {
            x: self.x && !other.x,
            y: self.y && !other.y,
            z: self.z && !other.z,
        }
    }

    /// No axis in the set.
    pub fn is_empty(&self) -> bool {
        *self == Axes::NONE
    }

    /// The axis letters, space separated ("X Y"; "" for none).
    pub fn letters(&self) -> String {
        [(self.x, "X"), (self.y, "Y"), (self.z, "Z")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, l)| *l)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The G28 command homing these axes (`G28` for all or none).
    pub fn home_command(&self) -> String {
        if *self == Axes::ALL || *self == Axes::default() {
            return "G28".to_string();
        }
        let mut s = String::from("G28");
        for (on, letter) in [(self.x, " X"), (self.y, " Y"), (self.z, " Z")] {
            if on {
                s.push_str(letter);
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let s = PrinterState::default();
        assert_eq!(s.status, ConnectionStatus::Disconnected);
        assert_eq!(s.feedrate_pct, 100.0);
        assert_eq!(s.flow_pct, [100.0]);
        assert!(!s.is_connected() && !s.job_active() && !s.halted);
    }

    #[test]
    fn home_commands() {
        assert_eq!(Axes::ALL.home_command(), "G28");
        assert_eq!(Axes::default().home_command(), "G28");
        assert_eq!(Axes::XY.home_command(), "G28 X Y");
        assert_eq!(Axes::Z.home_command(), "G28 Z");
        assert_eq!(
            Axes {
                x: true,
                y: false,
                z: true
            }
            .home_command(),
            "G28 X Z"
        );
    }

    #[test]
    fn axis_sets() {
        let xz = Axes {
            x: true,
            y: false,
            z: true,
        };
        assert!(xz.has(Axis::X) && !xz.has(Axis::Y) && !xz.has(Axis::E));
        assert_eq!(xz.union(Axes::Y), Axes::ALL);
        assert_eq!(Axes::ALL.without(xz), Axes::Y);
        assert!(Axes::NONE.is_empty() && !Axes::Z.is_empty());
        assert_eq!(xz.letters(), "X Z");
        assert_eq!(Axes::NONE.letters(), "");
    }

    #[test]
    fn heater_indices_round_trip() {
        for h in [
            Heater::Hotend(0),
            Heater::Hotend(3),
            Heater::Bed,
            Heater::Chamber,
        ] {
            assert_eq!(
                Heater::from_autotune_index(i64::from(h.autotune_index())),
                Some(h)
            );
        }
        assert_eq!(Heater::from_autotune_index(-7), None);
        assert_eq!(Heater::from_autotune_index(300), None);
    }

    #[test]
    fn messages_are_bounded() {
        let mut s = PrinterState::default();
        for i in 0..100 {
            s.push_message(i.to_string());
        }
        assert_eq!(s.firmware_messages.len(), FIRMWARE_MESSAGES_LEN);
        assert_eq!(s.firmware_messages.back().map(String::as_str), Some("99"));
    }

    #[test]
    fn status_helpers() {
        assert!(ConnectionStatus::Handshaking.is_open());
        assert!(!ConnectionStatus::Failed("x".into()).is_open());
        assert!(ConnectionStatus::Connected.is_connected());
        assert_eq!(ConnectionStatus::Failed("x".into()).label(), "Failed");
    }
}
