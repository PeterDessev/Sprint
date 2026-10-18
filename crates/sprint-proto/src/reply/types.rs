//! Reply data types.

use serde::{Deserialize, Serialize};

use crate::nan::same_f32;
use crate::{BedMesh, FirmwareInfo, SettingCommand};

/// One logical reply from the printer. A received line may produce several
/// (`ok T:...` yields [`Reply::Ok`] then [`Reply::Temperatures`]), and lines
/// inside a multi-line block may produce none until the block completes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    /// "ok", "ok N12", "ok N12 P15 B3" (ADVANCED_OK), Repetier "ok 12".
    /// Temperatures or a position on the same line are emitted as a
    /// separate reply.
    Ok {
        /// Line number acknowledged (`N` or Repetier's bare number).
        line: Option<u32>,
        /// Free planner slots (`P`).
        planner_free: Option<u32>,
        /// Free command buffer slots (`B`).
        buffer_free: Option<u32>,
    },
    /// "Resend: 12", "rs N12", "rs 12".
    Resend(u32),
    /// Temperature report.
    Temperatures(TempReport),
    /// Position report.
    Position(PositionReport),
    /// "echo:busy: processing", "busy: paused for user" (message only).
    Busy(String),
    /// Marlin idle "wait".
    Wait,
    /// Firmware boot "start".
    Start,
    /// "Error:..." / "!! ..." (Klipper). `fatal` for halts, kills, thermal
    /// faults and "Printer stopped" (the firmware needs M999 or a restart).
    Error {
        /// Message without the `Error:` / `!!` prefix.
        message: String,
        /// The firmware stopped and will not print until restarted.
        fatal: bool,
    },
    /// `echo:Unknown command: "X"` (the command, quotes removed).
    UnknownCommand(String),
    /// M115 `FIRMWARE_NAME:` line.
    Firmware(FirmwareInfo),
    /// M115 `Cap:NAME:0|1` line.
    Capability {
        /// Capability name, e.g. "AUTOREPORT_TEMP".
        name: String,
        /// Whether the firmware has it enabled.
        enabled: bool,
    },
    /// One line of M503 output: "echo:  M92 X80.00 Y80.00 Z400.00 E93.00".
    Setting(SettingCommand),
    /// SD card messages.
    SdCard(SdReply),
    /// "x_min: open", "z_probe: TRIGGERED".
    Endstop {
        /// Endstop name as printed ("x_min", "z_probe", "filament").
        name: String,
        /// True when triggered / hit.
        triggered: bool,
    },
    /// Complete leveling grid (emitted once the multi-line block ends).
    Mesh(BedMesh),
    /// M303 result (Kp, Ki, Kd); emitted once per completed autotune.
    AutotuneResult {
        /// Proportional gain.
        kp: f32,
        /// Integral gain.
        ki: f32,
        /// Derivative gain.
        kd: f32,
    },
    /// "PID Autotune failed..." (the reason, e.g. "timeout").
    AutotuneFailed(String),
    /// M78 statistics, one reply per received line with the fields found
    /// on that line.
    Stats(StatsReport),
    /// "//action:pause", "//action:prompt_begin Filament runout", ...
    HostAction(HostAction),
    /// "echo:Active Extruder: 1"
    ActiveExtruder(u8),
    /// M31 "echo:Print time: 1h 2m 3s" (seconds).
    PrintTime(u64),
    /// Any other "echo:" message (prefix stripped).
    Echo(String),
    /// "// ..." Klipper / debug info lines (prefix stripped).
    Info(String),
    /// Anything unrecognised (raw line).
    Other(String),
}

/// One heater (or sensor) reading.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HeaterReading {
    /// Current temperature (°C). NaN for a placeholder entry (placeholders
    /// compare equal to each other).
    #[serde(deserialize_with = "crate::nan::de_f32")]
    pub current: f32,
    /// Target temperature, when reported.
    pub target: Option<f32>,
    /// Heater power 0..=1, when reported.
    pub power: Option<f32>,
}

impl PartialEq for HeaterReading {
    /// Field-wise equality where a NaN placeholder `current` equals another
    /// placeholder, so a report with gaps equals its own clone.
    fn eq(&self, other: &Self) -> bool {
        same_f32(self.current, other.current)
            && self.target == other.target
            && self.power == other.power
    }
}

/// A temperature report.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TempReport {
    /// Hotends by tool index; single-extruder "T:" -> `hotends[0]`.
    pub hotends: Vec<HeaterReading>,
    /// Heated bed (`B:`).
    pub bed: Option<HeaterReading>,
    /// Chamber (`C:`).
    pub chamber: Option<HeaterReading>,
    /// Probe / PINDA temperature (`P:`).
    pub probe: Option<f32>,
    /// Board / ambient temperature (Marlin `M:`, Prusa `A:`).
    pub board: Option<f32>,
    /// Redundant hotend sensor (`R:`).
    pub redundant: Option<f32>,
    /// Laser cooler (`L:`).
    pub cooler: Option<HeaterReading>,
}

/// A position report (M114 / auto-report).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PositionReport {
    /// X (mm).
    pub x: f32,
    /// Y (mm).
    pub y: f32,
    /// Z (mm).
    pub z: f32,
    /// E (mm).
    pub e: f32,
    /// Raw stepper counts after `Count`, when reported as integers.
    pub stepper_counts: Option<Vec<i64>>,
}

/// SD card / media replies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SdReply {
    /// "Begin file list".
    ListBegin,
    /// One entry of a file list.
    File(SdFile),
    /// "End file list".
    ListEnd,
    /// "SD card ok".
    CardOk,
    /// "SD init fail" and similar (the message).
    CardFailed(String),
    /// "No SD card" / "No media".
    NoCard,
    /// "File opened: x.gco Size: 12345" (also "Now fresh file:" / "Now doing
    /// file:", which carry no size: 0).
    Opened {
        /// File name.
        name: String,
        /// Size in bytes (0 when not reported).
        size: u64,
    },
    /// "File selected".
    Selected,
    /// "SD printing byte 1234/5678".
    Progress {
        /// Bytes printed.
        done: u64,
        /// File size.
        total: u64,
    },
    /// "Not SD printing".
    NotPrinting,
    /// "Done printing file".
    DonePrinting,
    /// "Writing to file: x.gco".
    WritingTo(String),
    /// "Done saving file."
    Saved,
    /// "File deleted:x.gco".
    Deleted(String),
    /// "Deletion failed, File: x.gco."
    DeleteFailed(String),
}

/// One SD card file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SdFile {
    /// DOS 8.3 name (possibly with a `/DIR/` path), as used by M23.
    pub name: String,
    /// Long file name (M20 L), when reported.
    pub long_name: Option<String>,
    /// Size in bytes, when reported.
    pub size: Option<u64>,
}

/// M78 print statistics. Each reply carries the fields of one line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StatsReport {
    /// Prints started.
    pub prints: Option<u32>,
    /// Prints finished.
    pub finished: Option<u32>,
    /// Prints failed / cancelled.
    pub failed: Option<u32>,
    /// Total print time (s).
    pub total_time: Option<u64>,
    /// Longest job (s).
    pub longest_job: Option<u64>,
    /// Filament used (mm).
    pub filament_used_mm: Option<f64>,
}

impl StatsReport {
    /// Copy every field that `other` has into `self` (merging line-by-line
    /// replies into one report).
    pub fn merge(&mut self, other: &StatsReport) {
        self.prints = other.prints.or(self.prints);
        self.finished = other.finished.or(self.finished);
        self.failed = other.failed.or(self.failed);
        self.total_time = other.total_time.or(self.total_time);
        self.longest_job = other.longest_job.or(self.longest_job);
        self.filament_used_mm = other.filament_used_mm.or(self.filament_used_mm);
    }
}

/// `//action:` host action commands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostAction {
    /// Host should pause the print.
    Pause,
    /// Firmware has paused.
    Paused,
    /// Host should resume.
    Resume,
    /// Firmware has resumed.
    Resumed,
    /// Host should cancel the print.
    Cancel,
    /// Host should disconnect.
    Disconnect,
    /// Host should start the print.
    Start,
    /// Filament ran out (optionally which tool, e.g. "T0").
    OutOfFilament(Option<String>),
    /// Notification text for the user.
    Notification(String),
    /// Start a prompt with this message.
    PromptBegin(String),
    /// Add a choice to the prompt (legacy form of a button).
    PromptChoice(String),
    /// Add a button to the prompt.
    PromptButton(String),
    /// Show the prompt.
    PromptShow,
    /// Close the prompt.
    PromptEnd,
    /// Any other action (text after `action:`).
    Other(String),
}
