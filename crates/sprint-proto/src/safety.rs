//! Command safety: may a line the user typed (console, shell, macro button)
//! go to the printer as it is, does it need a confirmation first, or must it
//! not be sent, given what the print job is doing?
//!
//! Every front end asks [`classify`] before sending, so the GUI, the TUI and
//! the shell apply the same rules. The line is read the way the firmware
//! will read it after the host cleaned it: a leading `N<n>` line number, a
//! `*checksum`, `;` comments and leading `(...)` comments are ignored, codes
//! are case-insensitive (`g28`), parameters may be glued (`G1X10`) and
//! spaces between the letter and the number are skipped (`M 112`, as Marlin
//! does). Klipper extended commands (`BED_MESH_CALIBRATE`,
//! `FIRMWARE_RESTART`, ...) are known by name.
//!
//! ```
//! use sprint_proto::{Decision, JobContext, SafetyReason, classify};
//!
//! // A factory reset always asks, even pasted with a line number.
//! let v = classify("N10 M502*45", JobContext::Idle);
//! assert_eq!(v.decision, Decision::Confirm(SafetyReason::Dangerous));
//! assert_eq!(v.command.as_deref(), Some("M502"));
//!
//! // Moves are fine when idle or paused, but interleave with a print.
//! assert!(classify("G1 X10", JobContext::Paused).is_allowed());
//! assert_eq!(
//!     classify("G1X10", JobContext::Printing).reason(),
//!     Some(SafetyReason::InterleavesWithPrint)
//! );
//!
//! // M112 is never questioned and goes out at once.
//! let v = classify("m112", JobContext::Printing);
//! assert!(v.is_allowed() && v.emergency);
//! ```

use std::fmt;

use crate::reference::{GcodeDoc, gcode_doc};

/// What the print job is doing when a line is about to be sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum JobContext {
    /// No print is running.
    #[default]
    Idle,
    /// A print is running and not paused (the host streams a file or the
    /// printer prints from its card): a typed command runs between the
    /// print's own lines.
    Printing,
    /// The print is paused and parked. Moves and extrusion are fine (the
    /// host restores the position and modes on resume), anything that goes
    /// down to the bed or loses the position is not.
    Paused,
}

/// Hotend targets below this (°C) stop extrusion: Marlin's and Klipper's
/// default minimum extrusion temperature.
pub const MIN_EXTRUSION_TEMP: f64 = 170.0;

/// Bed and chamber targets below this (°C) switch the heater off in effect.
pub const HEATER_OFF_BELOW: f64 = 30.0;

/// Why [`classify`] asks for a confirmation or blocks a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SafetyReason {
    /// Marked dangerous in the G-code reference ([`GcodeDoc::dangerous`]):
    /// wipes settings, flashes firmware, powers off, restarts, deletes a
    /// file or drives raw pins.
    Dangerous,
    /// Restarts the firmware (Klipper `FIRMWARE_RESTART`, `RESTART`,
    /// `SAVE_CONFIG`).
    Restarts,
    /// Sets the position or moves a stepper without homing or limits
    /// (Klipper `SET_KINEMATIC_POSITION`, `FORCE_MOVE`).
    ForcesPosition,
    /// Moves, extrudes or retracts between the print's own lines.
    InterleavesWithPrint,
    /// Changes the positioning, extrusion or unit mode or the arc plane the
    /// print's lines are read in (G90/G91, M82/M83, G20/G21, G17-G19).
    ChangesModes,
    /// Shifts the coordinates the print continues in (G92, workspace and
    /// home offsets).
    ShiftsCoordinates,
    /// Homes or probes: the nozzle goes down to the bed, through the part.
    HomesOrProbes,
    /// Switches tools.
    ToolChange,
    /// Switches the motors off: the printer loses its position.
    MotorsOff,
    /// Switches a heater off or drops the hotend below
    /// [`MIN_EXTRUSION_TEMP`].
    HeaterOff,
    /// Waits for a temperature: the print stalls until it is reached.
    WaitsForTemperature,
    /// Runs a calibration routine (PID or MPC autotune, resonance test).
    Calibration,
    /// Switches bed leveling or its fade height.
    ChangesLeveling,
    /// Quick stop (M410): every queued move is dropped.
    QuickStop,
    /// Starts another print (from the SD card or a start macro).
    StartsAnotherPrint,
    /// Ends the print (end or cancel macros switch heaters and motors off).
    EndsPrint,
    /// M28 would write the print's next lines into a file on the SD card
    /// instead of printing them.
    WritesToSd,
}

impl SafetyReason {
    /// The reason as a sentence fragment for a confirmation dialog, e.g.
    /// "it would interleave with the print".
    pub fn message(self) -> &'static str {
        match self {
            SafetyReason::Dangerous => {
                "it can wipe settings, delete files, stop, restart or power off the printer, or change its firmware"
            }
            SafetyReason::Restarts => "it restarts the firmware: a running print is lost",
            SafetyReason::ForcesPosition => {
                "it sets or moves the position without homing or limits: a wrong value crashes the machine"
            }
            SafetyReason::InterleavesWithPrint => {
                "it would interleave with the print: the nozzle moves between the print's own lines"
            }
            SafetyReason::ChangesModes => {
                "it changes how the print's next lines are read (positioning, extrusion, units or arc plane)"
            }
            SafetyReason::ShiftsCoordinates => "it shifts the coordinates the print continues in",
            SafetyReason::HomesOrProbes => {
                "homing Z or probing lowers the nozzle onto the bed and can crash into the part"
            }
            SafetyReason::ToolChange => "it switches tools in the middle of the print",
            SafetyReason::MotorsOff => {
                "with the motors off the printer loses the position the print continues from"
            }
            SafetyReason::HeaterOff => {
                "it switches a heater off or drops the hotend below printing temperature"
            }
            SafetyReason::WaitsForTemperature => {
                "it waits for the temperature: the print stalls until it is reached"
            }
            SafetyReason::Calibration => {
                "it runs a calibration routine that takes over the printer"
            }
            SafetyReason::ChangesLeveling => "it changes bed leveling for the rest of the print",
            SafetyReason::QuickStop => {
                "it drops every queued move at once: the print loses its place"
            }
            SafetyReason::StartsAnotherPrint => "it starts another print while this one runs",
            SafetyReason::EndsPrint => "it ends the print and switches heaters and motors off",
            SafetyReason::WritesToSd => {
                "the print's next lines would be written to a file on the SD card instead of printed"
            }
        }
    }
}

impl fmt::Display for SafetyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// What to do with a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Decision {
    /// Send it.
    Allow,
    /// Ask the user first; send only after a yes.
    Confirm(SafetyReason),
    /// Do not send it; tell the user why.
    Block(SafetyReason),
}

/// The result of [`classify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// Send, ask first, or refuse.
    pub decision: Decision,
    /// An emergency command (M112, M108, M410, M876): once allowed (or
    /// confirmed) it goes out right away, bypassing the queue and flow
    /// control, as [`crate::is_emergency_command`] describes.
    pub emergency: bool,
    /// The command as classified: the canonical code (`"G28"`, `"M104"`,
    /// `"G29.1"`, `"T1"`) or the upper-case name of an extended command
    /// (`"BED_MESH_CALIBRATE"`). `None` for lines that send nothing
    /// (blank, comments) and for parameter-only lines (`"X10 Y5"`).
    pub command: Option<String>,
    /// The reference entry of `command`, when there is one (for the
    /// command's name in a confirmation).
    pub doc: Option<&'static GcodeDoc>,
}

impl Verdict {
    /// True when the line can be sent without asking.
    pub fn is_allowed(&self) -> bool {
        self.decision == Decision::Allow
    }

    /// True when the user must confirm the line first.
    pub fn needs_confirmation(&self) -> bool {
        matches!(self.decision, Decision::Confirm(_))
    }

    /// True when the line must not be sent.
    pub fn is_blocked(&self) -> bool {
        matches!(self.decision, Decision::Block(_))
    }

    /// Why the line needs a confirmation or is blocked.
    pub fn reason(&self) -> Option<SafetyReason> {
        match self.decision {
            Decision::Allow => None,
            Decision::Confirm(r) | Decision::Block(r) => Some(r),
        }
    }
}

/// Decide whether `line` may be sent while the job is in `ctx`.
///
/// * Dangerous commands ([`GcodeDoc::dangerous`]: settings wipes, firmware
///   flashing, power off, restart, file deletion, raw pins) and Klipper's
///   `FIRMWARE_RESTART`, `RESTART`, `SAVE_CONFIG`, `SET_KINEMATIC_POSITION`
///   and `FORCE_MOVE` always need a confirmation, except M112, which is an
///   emergency: never asked, sent at once.
/// * While [`JobContext::Printing`]: moves (G0-G3, G5, G6, firmware
///   retraction, parking, ...), homing and probing, positioning, extrusion
///   and unit modes, G92 and other coordinate shifts, tool changes, motors
///   off, heaters off or below [`MIN_EXTRUSION_TEMP`] (bed and chamber
///   below [`HEATER_OFF_BELOW`]), temperature waits, calibration routines,
///   leveling changes, M410, start/end/cancel macros and SD prints need a
///   confirmation; M28 (SD write) and starting an SD print are blocked.
///   Tuning (temperatures, fans, speed and flow, babysteps, filament
///   change, object exclusion) is allowed.
/// * While [`JobContext::Paused`]: moves, extrusion, mode changes and
///   heater changes are allowed (the host restores position and modes on
///   resume), but homing Z, probing, coordinate shifts other than `G92 E`,
///   tool changes, motors off, calibration, leveling changes and
///   start/end macros need a confirmation, and M28 is blocked.
/// * When [`JobContext::Idle`] everything else is allowed.
///
/// Unknown commands (including user macros) are allowed. Never panics.
pub fn classify(line: &str, ctx: JobContext) -> Verdict {
    let Some(cmd) = lex(line) else {
        return Verdict {
            decision: Decision::Allow,
            emergency: false,
            command: None,
            doc: None,
        };
    };
    let (command, doc, emergency) = match &cmd {
        Lexed::Classic {
            letter,
            number,
            sub,
            ..
        } => {
            let code = match sub {
                Some(s) => format!("{}{number}.{s}", char::from(*letter)),
                None => format!("{}{number}", char::from(*letter)),
            };
            let emergency =
                *letter == b'M' && sub.is_none() && matches!(number, 112 | 108 | 410 | 876);
            let doc = gcode_doc(&code);
            (Some(code), doc, emergency)
        }
        Lexed::Words(_) => (None, None, false),
        Lexed::Extended { name, .. } => (Some(name.to_ascii_uppercase()), None, false),
    };
    let decision = if matches!(
        &cmd,
        Lexed::Classic {
            letter: b'M',
            number: 112,
            sub: None,
            ..
        }
    ) {
        Decision::Allow
    } else if doc.is_some_and(|d| d.dangerous) {
        Decision::Confirm(SafetyReason::Dangerous)
    } else {
        rule(class_of(&cmd), ctx)
    };
    Verdict {
        decision,
        emergency,
        command,
        doc,
    }
}

/// What a command does, as far as safety is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Harmless or unknown.
    Other,
    /// Moves, extrudes or retracts.
    Move,
    /// G28; `z` when it homes Z (names Z, or neither X nor Y).
    Home {
        z: bool,
    },
    /// Probing, leveling and bed calibration routines.
    Probe,
    /// Positioning / extrusion / unit modes.
    Mode,
    /// The arc plane (G17-G19): read like a mode, but not restored on
    /// resume.
    Plane,
    /// Coordinate shift; `e_only` for `G92 E..`.
    Coordinates {
        e_only: bool,
    },
    Tool,
    MotorsOff,
    /// A heater target; `off` when it switches off / below printing temp.
    Heater {
        off: bool,
    },
    /// A heater target that waits; `off` as for `Heater`.
    HeaterWait {
        off: bool,
    },
    /// Waits for temperatures already set.
    Wait,
    HeatersOff,
    Calibration,
    Leveling,
    QuickStop,
    SdWrite,
    SdPrint,
    StartMacro,
    EndMacro,
    Restart,
    ForcePosition,
}

/// The decision for a class of command in a job context.
fn rule(class: Class, ctx: JobContext) -> Decision {
    use Decision::{Allow, Block, Confirm};
    use JobContext::{Idle, Paused, Printing};
    use SafetyReason as R;
    match (class, ctx) {
        (Class::Restart, _) => Confirm(R::Restarts),
        (Class::ForcePosition, _) => Confirm(R::ForcesPosition),
        (_, Idle) | (Class::Other, _) => Allow,
        (Class::Move, Printing) => Confirm(R::InterleavesWithPrint),
        (Class::Home { .. } | Class::Probe, Printing) => Confirm(R::HomesOrProbes),
        (Class::Home { z }, Paused) => {
            if z {
                Confirm(R::HomesOrProbes)
            } else {
                Allow
            }
        }
        (Class::Probe, Paused) => Confirm(R::HomesOrProbes),
        (Class::Mode, Printing) | (Class::Plane, _) => Confirm(R::ChangesModes),
        (Class::Coordinates { .. }, Printing) => Confirm(R::ShiftsCoordinates),
        (Class::Coordinates { e_only }, Paused) => {
            if e_only {
                Allow
            } else {
                Confirm(R::ShiftsCoordinates)
            }
        }
        (Class::Tool, _) => Confirm(R::ToolChange),
        (Class::MotorsOff, _) => Confirm(R::MotorsOff),
        (Class::Heater { off: true } | Class::HeaterWait { off: true }, Printing) => {
            Confirm(R::HeaterOff)
        }
        (Class::HeaterWait { off: false } | Class::Wait, Printing) => {
            Confirm(R::WaitsForTemperature)
        }
        (Class::Heater { off: false }, Printing) => Allow,
        (Class::HeatersOff, Printing) => Confirm(R::HeaterOff),
        (Class::Calibration, _) => Confirm(R::Calibration),
        (Class::Leveling, _) => Confirm(R::ChangesLeveling),
        (Class::QuickStop, Printing) => Confirm(R::QuickStop),
        (Class::SdWrite, _) => Block(R::WritesToSd),
        (Class::SdPrint, Printing) => Block(R::StartsAnotherPrint),
        (Class::SdPrint | Class::StartMacro, _) => Confirm(R::StartsAnotherPrint),
        (Class::EndMacro, _) => Confirm(R::EndsPrint),
        // Allowed while paused: moves, modes, heaters, waits, M410.
        (
            Class::Move
            | Class::Mode
            | Class::Heater { .. }
            | Class::HeaterWait { .. }
            | Class::Wait
            | Class::HeatersOff
            | Class::QuickStop,
            Paused,
        ) => Allow,
    }
}

/// Which heater a target is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Heater {
    Hotend,
    /// Bed or chamber.
    Enclosure,
}

/// True when a target switches the heater off in effect.
fn is_off(heater: Heater, target: f64) -> bool {
    match heater {
        Heater::Hotend => target < MIN_EXTRUSION_TEMP,
        Heater::Enclosure => target < HEATER_OFF_BELOW,
    }
}

fn class_of(cmd: &Lexed<'_>) -> Class {
    match *cmd {
        Lexed::Classic {
            letter,
            number,
            params,
            ..
        } => classic_class(letter, number, &words(params)),
        Lexed::Words(params) => {
            let words = words(params);
            // RepRapFirmware's `T-1` deselects every tool.
            if words.first().is_some_and(|w| w.0 == b'T') {
                return Class::Tool;
            }
            // A modal move on firmware that repeats the last motion mode.
            let moves = words
                .iter()
                .any(|w| matches!(w.0, b'X' | b'Y' | b'Z' | b'E' | b'A' | b'B' | b'C'));
            if moves { Class::Move } else { Class::Other }
        }
        Lexed::Extended { name, args } => extended_class(name, args),
    }
}

/// A parameter word: upper-case letter and its value (`None` without one).
type Word = (u8, Option<f64>);

fn has(words: &[Word], letter: u8) -> bool {
    words.iter().any(|w| w.0 == letter)
}

fn value(words: &[Word], letter: u8) -> Option<f64> {
    words.iter().find(|w| w.0 == letter).and_then(|w| w.1)
}

fn classic_class(letter: u8, number: u32, w: &[Word]) -> Class {
    let has_any = |letters: &[u8]| letters.iter().any(|&l| has(w, l));
    match (letter, number) {
        (b'T', _) => Class::Tool,
        (b'G', 0..=3 | 5 | 6 | 11 | 12 | 27 | 42 | 61) => Class::Move,
        (b'G', 10) => {
            if has(w, b'L') || (has(w, b'P') && has_any(b"XYZUVW")) {
                // Workspace or tool offsets (RepRapFirmware).
                Class::Coordinates { e_only: false }
            } else if has(w, b'P') {
                // RepRapFirmware tool temperatures: S active, R standby.
                Class::Heater {
                    off: value(w, b'S').is_some_and(|t| is_off(Heater::Hotend, t)),
                }
            } else {
                // Firmware retraction.
                Class::Move
            }
        }
        (b'G', 28) => Class::Home {
            // Only X and Y keep Z still: without X/Y/Z, Marlin, Prusa
            // (`G28 W`, `G28 C`) and Klipper home every axis, and a 3-axis
            // printer reads other axis letters that way too.
            z: has(w, b'Z') || !has_any(b"XY"),
        },
        (b'G', 26 | 29..=30 | 32..=35 | 38 | 76 | 80) => Class::Probe,
        (b'G', 20 | 21 | 90 | 91) | (b'M', 82 | 83) => Class::Mode,
        (b'G', 17..=19) => Class::Plane,
        (b'G', 92) => Class::Coordinates {
            // `G92` alone resets every axis.
            e_only: has(w, b'E') && !has_any(b"XYZABCUVW"),
        },
        (b'G', 53..=59) | (b'M', 206 | 428) => Class::Coordinates { e_only: false },
        (b'M', 18 | 84) => {
            // `M84 S<seconds>` only sets the idle timeout.
            if has(w, b'S') && w.iter().all(|x| x.0 == b'S') {
                Class::Other
            } else {
                Class::MotorsOff
            }
        }
        (b'M', 104 | 109 | 140 | 141 | 190 | 191) => {
            let heater = if matches!(number, 104 | 109) {
                Heater::Hotend
            } else {
                Heater::Enclosure
            };
            // S sets the target; R (M109/M190) waits for it, cooling too.
            let off = b"SR"
                .iter()
                .copied()
                .filter_map(|l| value(w, l))
                .any(|t| is_off(heater, t));
            if matches!(number, 109 | 190 | 191) {
                Class::HeaterWait { off }
            } else {
                Class::Heater { off }
            }
        }
        // M192: wait for the probe temperature.
        (b'M', 116 | 192) => Class::Wait,
        // M915: Z stall alignment drives Z to the top; M916-M918 driver
        // tests and M863 encoder calibration jog the axes.
        (b'M', 303 | 863 | 915..=918) => Class::Calibration,
        // M306 T: MPC autotune (other M306 forms set the model).
        (b'M', 306) if has(w, b'T') => Class::Calibration,
        // M45: Prusa bed skew calibration; M401 deploys the probe.
        (b'M', 45 | 48 | 401) => Class::Probe,
        (b'M', 420) if has_any(b"SZ") => Class::Leveling,
        // RepRapFirmware: clear the bed transform (mesh off).
        (b'M', 561) => Class::Leveling,
        (b'M', 410) => Class::QuickStop,
        (b'M', 28) => Class::SdWrite,
        (b'M', 24 | 32) => Class::SdPrint,
        // M524: abort the SD print.
        (b'M', 524) => Class::EndMacro,
        // Park, load and unload filament.
        (b'M', 125 | 701 | 702) => Class::Move,
        _ => Class::Other,
    }
}

/// Klipper (and common macro) extended commands that are not harmless.
const EXTENDED: &[(&str, Class)] = &[
    ("FIRMWARE_RESTART", Class::Restart),
    ("RESTART", Class::Restart),
    ("SAVE_CONFIG", Class::Restart),
    ("SET_KINEMATIC_POSITION", Class::ForcePosition),
    ("FORCE_MOVE", Class::ForcePosition),
    ("BED_MESH_CALIBRATE", Class::Probe),
    ("PROBE", Class::Probe),
    ("PROBE_ACCURACY", Class::Probe),
    ("PROBE_CALIBRATE", Class::Probe),
    ("Z_TILT_ADJUST", Class::Probe),
    ("QUAD_GANTRY_LEVEL", Class::Probe),
    ("SCREWS_TILT_CALCULATE", Class::Probe),
    ("DELTA_CALIBRATE", Class::Probe),
    ("BED_TILT_CALIBRATE", Class::Probe),
    ("Z_ENDSTOP_CALIBRATE", Class::Probe),
    ("MANUAL_PROBE", Class::Probe),
    ("AXIS_TWIST_COMPENSATION_CALIBRATE", Class::Probe),
    ("PID_CALIBRATE", Class::Calibration),
    ("MPC_CALIBRATE", Class::Calibration),
    ("SHAPER_CALIBRATE", Class::Calibration),
    ("TEST_RESONANCES", Class::Calibration),
    ("TURN_OFF_HEATERS", Class::HeatersOff),
    ("TEMPERATURE_WAIT", Class::Wait),
    ("ACTIVATE_EXTRUDER", Class::Tool),
    // Prusa MMU: choose a filament on the printer, load to the nozzle.
    ("T?", Class::Tool),
    ("TX", Class::Tool),
    ("TC", Class::Tool),
    ("BED_MESH_CLEAR", Class::Leveling),
    ("STEPPER_BUZZ", Class::Move),
    ("MANUAL_STEPPER", Class::Move),
    ("SDCARD_PRINT_FILE", Class::SdPrint),
    ("PRINT_START", Class::StartMacro),
    ("START_PRINT", Class::StartMacro),
    ("PRINT_END", Class::EndMacro),
    ("END_PRINT", Class::EndMacro),
    ("CANCEL_PRINT", Class::EndMacro),
];

fn extended_class(name: &str, args: &str) -> Class {
    if name.eq_ignore_ascii_case("SET_HEATER_TEMPERATURE") {
        let heater = match ext_param(args, "HEATER") {
            Some(h) if h.to_ascii_lowercase().starts_with("extruder") => Heater::Hotend,
            _ => Heater::Enclosure,
        };
        // Klipper sets 0 (off) when TARGET is missing.
        let target = ext_param(args, "TARGET").map_or(Some(0.0), |t| t.parse::<f64>().ok());
        return Class::Heater {
            off: target.is_some_and(|t| is_off(heater, t)),
        };
    }
    if name.eq_ignore_ascii_case("SET_STEPPER_ENABLE") {
        let off = ext_param(args, "ENABLE").is_none_or(|v| v.parse::<f64>() == Ok(0.0));
        return if off { Class::MotorsOff } else { Class::Other };
    }
    if name.eq_ignore_ascii_case("SET_GCODE_OFFSET") {
        // Z / Z_ADJUST are babysteps; X and Y shift the print sideways.
        let shifts = ["X", "Y", "X_ADJUST", "Y_ADJUST"]
            .iter()
            .any(|k| ext_param(args, k).is_some());
        return if shifts {
            Class::Coordinates { e_only: false }
        } else {
            Class::Other
        };
    }
    if name.eq_ignore_ascii_case("BED_MESH_PROFILE") {
        // LOAD swaps the mesh the print is leveled with; SAVE and REMOVE
        // only manage stored profiles.
        return if ext_param(args, "LOAD").is_some() {
            Class::Leveling
        } else {
            Class::Other
        };
    }
    if let Some(&(_, class)) = EXTENDED.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return class;
    }
    // Every PROBE* command probes (PROBE_EDDY_CURRENT_CALIBRATE, ...).
    if name
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("PROBE"))
    {
        return Class::Probe;
    }
    Class::Other
}

/// Value of a Klipper `KEY=value` argument (key case-insensitive, quotes
/// removed).
fn ext_param<'a>(args: &'a str, key: &str) -> Option<&'a str> {
    args.split_whitespace().find_map(|token| {
        let (k, v) = token.split_once('=')?;
        k.eq_ignore_ascii_case(key)
            .then(|| v.trim_matches(|c| c == '"' || c == '\''))
    })
}

/// One command, read like the firmware reads it.
enum Lexed<'a> {
    /// A G, M or T code and the text of its parameters (comment and
    /// checksum removed).
    Classic {
        letter: u8,
        number: u32,
        sub: Option<u32>,
        params: &'a str,
    },
    /// Parameter words without a code (`X10 Y5`).
    Words(&'a str),
    /// Anything else: Klipper extended commands, macros, host commands.
    Extended { name: &'a str, args: &'a str },
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while b.get(i).copied().is_some_and(is_ws) {
        i += 1;
    }
    i
}

/// Digits starting at `i`: their value (`None` past `u32`) and the end.
fn digits_at(b: &[u8], i: usize) -> (Option<u32>, usize) {
    let mut j = i;
    let mut value: Option<u32> = Some(0);
    while let Some(&d) = b.get(j).filter(|d| d.is_ascii_digit()) {
        value = value
            .and_then(|v| v.checked_mul(10))
            .and_then(|v| v.checked_add(u32::from(d - b'0')));
        j += 1;
    }
    (value, j)
}

fn lex(line: &str) -> Option<Lexed<'_>> {
    let b = line.as_bytes();
    let mut i = 0;
    // Line numbers and leading paren comments, in any order.
    loop {
        i = skip_ws(b, i);
        match b.get(i)? {
            b'N' | b'n' if b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                i = digits_at(b, i + 1).1;
            }
            b'(' => {
                i = b[i..]
                    .iter()
                    .position(|&c| c == b')')
                    .map_or(b.len(), |p| i + p + 1);
            }
            b';' | b'*' | b'%' => return None,
            _ => break,
        }
    }
    // Only ASCII bytes were skipped, so `i` is a char boundary.
    let rest = line.get(i..)?;
    let rest = rest.split(';').next().unwrap_or("").trim_end();
    let rb = rest.as_bytes();
    let first = rb.first()?.to_ascii_uppercase();
    if matches!(first, b'G' | b'M' | b'T') {
        // Marlin skips blanks between the letter and the number ("M 112").
        let mut j = 1;
        while matches!(rb.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        let (number, mut end) = digits_at(rb, j);
        if end > j {
            let number = number?;
            let mut sub = None;
            if rb.get(end) == Some(&b'.') && rb.get(end + 1).is_some_and(u8::is_ascii_digit) {
                let (s, e) = digits_at(rb, end + 1);
                sub = Some(s?);
                end = e;
            }
            let params = rest.get(end..).unwrap_or("");
            let params = params.split('*').next().unwrap_or("");
            return Some(Lexed::Classic {
                letter: first,
                number,
                sub,
                params,
            });
        }
    }
    let second = rb.get(1).copied().unwrap_or(b' ');
    if first.is_ascii_alphabetic()
        && (second.is_ascii_digit() || matches!(second, b'-' | b'+' | b'.'))
    {
        return Some(Lexed::Words(rest.split('*').next().unwrap_or("")));
    }
    let end = rb.iter().position(|&c| is_ws(c)).unwrap_or(rb.len());
    let name = rest.get(..end)?;
    let args = rest.get(end..).unwrap_or("").trim();
    // A checksum after an extended command ("N3 RESTART*45").
    let args = match args.rfind('*') {
        Some(star) if args[star + 1..].bytes().all(|c| c.is_ascii_digit()) => args[..star].trim(),
        _ => args,
    };
    let name = match name.rfind('*') {
        Some(star) if name[star + 1..].bytes().all(|c| c.is_ascii_digit()) => &name[..star],
        _ => name,
    };
    Some(Lexed::Extended { name, args })
}

/// Parameter words of a classic command; `(...)` comments, quoted strings
/// and `{...}` expressions are skipped (a word with one has no value).
fn words(params: &str) -> Vec<Word> {
    let b = params.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'(' {
            i = b[i..]
                .iter()
                .position(|&c| c == b')')
                .map_or(b.len(), |p| i + p + 1);
        } else if c == b'"' {
            i = b[i + 1..]
                .iter()
                .position(|&c| c == b'"')
                .map_or(b.len(), |p| i + p + 2);
        } else if c == b'{' {
            i = b[i..]
                .iter()
                .position(|&c| c == b'}')
                .map_or(b.len(), |p| i + p + 1);
        } else if c.is_ascii_alphabetic() {
            let start = i + 1;
            let mut j = start;
            while b
                .get(j)
                .is_some_and(|d| d.is_ascii_digit() || matches!(d, b'.' | b'-' | b'+'))
            {
                j += 1;
            }
            let number = params
                .get(start..j)
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite());
            out.push((c.to_ascii_uppercase(), number));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::{Allow, Block, Confirm};
    use JobContext::{Idle, Paused, Printing};
    use SafetyReason as R;

    /// `(line, idle, printing, paused)`.
    type Row = (&'static str, Decision, Decision, Decision);

    fn check(rows: &[Row]) {
        for &(line, idle, printing, paused) in rows {
            for (ctx, expected) in [(Idle, idle), (Printing, printing), (Paused, paused)] {
                assert_eq!(
                    classify(line, ctx).decision,
                    expected,
                    "{line:?} while {ctx:?}"
                );
            }
        }
    }

    #[test]
    fn dangerous_commands_always_ask() {
        let d = Confirm(R::Dangerous);
        let restart = Confirm(R::Restarts);
        let force = Confirm(R::ForcesPosition);
        check(&[
            ("M502", d, d, d),
            ("M997", d, d, d),
            ("M999", d, d, d),
            ("M81", d, d, d),
            ("M42 P13 S255", d, d, d),
            ("M30 /old.gco", d, d, d),
            ("FIRMWARE_RESTART", restart, restart, restart),
            ("restart", restart, restart, restart),
            ("SAVE_CONFIG", restart, restart, restart),
            ("SET_KINEMATIC_POSITION Z=10", force, force, force),
            (
                "FORCE_MOVE STEPPER=stepper_z DISTANCE=5 VELOCITY=5",
                force,
                force,
                force,
            ),
        ]);
    }

    #[test]
    fn cleaning_like_the_host() {
        // Audit A50: numbered lines, checksums, comments, case and glued or
        // spaced codes are read as the firmware reads them.
        let d = Confirm(R::Dangerous);
        check(&[
            ("N10 M502*45", d, d, d),
            ("n10 m502", d, d, d),
            ("  M502 ; reset", d, d, d),
            ("(why) M502", d, d, d),
            ("N5 (why) N6 M502*12", d, d, d),
            ("M 502", d, d, d),
            ("M0502", d, d, d),
            ("M502;reset", d, d, d),
        ]);
        let home = Confirm(R::HomesOrProbes);
        check(&[
            ("N7 G28*55", Allow, home, home),
            ("g28 z", Allow, home, home),
            ("G 28", Allow, home, home),
            ("G28X", Allow, home, Allow),
        ]);
        // Nothing to send.
        for line in ["", "   ", "; G28", "(G28)", "*12", "N5", "%"] {
            let v = classify(line, Printing);
            assert!(v.is_allowed() && v.command.is_none(), "{line:?}");
        }
        // The command and its reference entry.
        let v = classify("n10 m502*45", Idle);
        assert_eq!(v.command.as_deref(), Some("M502"));
        assert_eq!(v.doc.map(|d| d.code), Some("M502"));
        let v = classify("g29.1 s1", Idle);
        assert_eq!(v.command.as_deref(), Some("G29.1"));
        let v = classify("bed_mesh_calibrate PROFILE=x", Idle);
        assert_eq!(v.command.as_deref(), Some("BED_MESH_CALIBRATE"));
        assert_eq!(v.doc, None);
    }

    #[test]
    fn emergencies() {
        for ctx in [Idle, Printing, Paused] {
            for line in ["M112", "m112", "N5 M112*1", "M 112", "M112 ; stop"] {
                let v = classify(line, ctx);
                assert!(v.is_allowed() && v.emergency, "{line:?} {ctx:?}");
            }
            for line in ["M108", "M876 S1", "m876s0"] {
                let v = classify(line, ctx);
                assert!(v.is_allowed() && v.emergency, "{line:?} {ctx:?}");
            }
        }
        let v = classify("M410", Printing);
        assert_eq!(v.decision, Confirm(R::QuickStop));
        assert!(v.emergency);
        assert!(classify("M410", Idle).is_allowed());
        assert!(classify("M410", Paused).is_allowed());
        for line in ["M112.1", "M1120", "G112", "M117 M112"] {
            assert!(!classify(line, Idle).emergency, "{line:?}");
        }
    }

    #[test]
    fn moves_and_modes() {
        let moves = Confirm(R::InterleavesWithPrint);
        let modes = Confirm(R::ChangesModes);
        let shift = Confirm(R::ShiftsCoordinates);
        check(&[
            ("G1 X10", Allow, moves, Allow),
            ("G1X10Y10F3000", Allow, moves, Allow),
            ("g0 z5", Allow, moves, Allow),
            ("G1 E5 F100", Allow, moves, Allow),
            ("G2 X10 Y10 I5 J0", Allow, moves, Allow),
            ("G10", Allow, moves, Allow),
            ("G11", Allow, moves, Allow),
            ("G27", Allow, moves, Allow),
            ("G12", Allow, moves, Allow),
            ("M125", Allow, moves, Allow),
            ("X10 Y5", Allow, moves, Allow),
            ("STEPPER_BUZZ STEPPER=stepper_x", Allow, moves, Allow),
            ("G90", Allow, modes, Allow),
            ("G91", Allow, modes, Allow),
            ("G20", Allow, modes, Allow),
            ("G21", Allow, modes, Allow),
            ("M82", Allow, modes, Allow),
            ("M83", Allow, modes, Allow),
            ("G92 E0", Allow, shift, Allow),
            ("G92 X0 Y0", Allow, shift, shift),
            ("G92", Allow, shift, shift),
            ("G92.1", Allow, shift, shift),
            ("G54", Allow, shift, shift),
            ("M206 Z-0.2", Allow, shift, shift),
            ("G10 L2 P1 X5", Allow, shift, shift),
            ("SET_GCODE_OFFSET X=1", Allow, shift, shift),
            ("SET_GCODE_OFFSET Z_ADJUST=0.02 MOVE=1", Allow, Allow, Allow),
            // Loading and unloading filament drive the extruder (and park).
            ("M701", Allow, moves, Allow),
            ("M702 T0", Allow, moves, Allow),
            // The arc plane decides how the print's G2/G3 are read, and the
            // host does not restore it on resume.
            ("G17", Allow, modes, modes),
            ("G18", Allow, modes, modes),
            ("g19", Allow, modes, modes),
        ]);
    }

    #[test]
    fn homing_probing_and_calibration() {
        let home = Confirm(R::HomesOrProbes);
        let cal = Confirm(R::Calibration);
        let level = Confirm(R::ChangesLeveling);
        check(&[
            ("G28", Allow, home, home),
            ("G28 Z", Allow, home, home),
            ("G28 X Y", Allow, home, Allow),
            // Firmware homes every axis when no X/Y/Z is named: Prusa's
            // `G28 W` (without mesh leveling) and `G28 C` (calibrate),
            // Marlin's `G28 O` / `G28 R5`, and letters a 3-axis Marlin or
            // Klipper does not know.
            ("G28 W", Allow, home, home),
            ("g28 c", Allow, home, home),
            ("G28 O", Allow, home, home),
            ("G28 R5", Allow, home, home),
            ("G28 A", Allow, home, home),
            ("G28 U V", Allow, home, home),
            ("G28 X W", Allow, home, Allow),
            ("G28 Y0", Allow, home, Allow),
            ("G28 X Z", Allow, home, home),
            ("G29", Allow, home, home),
            ("G29 P1", Allow, home, home),
            ("G30", Allow, home, home),
            ("G32", Allow, home, home),
            ("G33", Allow, home, home),
            ("G34", Allow, home, home),
            ("G35", Allow, home, home),
            ("G38.2 Z-10", Allow, home, home),
            ("G76", Allow, home, home),
            ("G80", Allow, home, home),
            ("G26", Allow, home, home),
            ("M48 P10", Allow, home, home),
            ("BED_MESH_CALIBRATE", Allow, home, home),
            ("PROBE", Allow, home, home),
            ("PROBE_CALIBRATE", Allow, home, home),
            ("probe_accuracy", Allow, home, home),
            ("Z_TILT_ADJUST", Allow, home, home),
            ("QUAD_GANTRY_LEVEL", Allow, home, home),
            ("SCREWS_TILT_CALCULATE", Allow, home, home),
            // Every Klipper PROBE* command probes (eddy-current probes,
            // plug-in probes); QUERY_PROBE only reads the probe.
            (
                "PROBE_EDDY_CURRENT_CALIBRATE CHIP=btt_eddy",
                Allow,
                home,
                home,
            ),
            ("probe_z_accuracy", Allow, home, home),
            ("QUERY_PROBE", Allow, Allow, Allow),
            // Prusa bed skew calibration probes the bed.
            ("M45", Allow, home, home),
            ("M303 E0 S210 C8", Allow, cal, cal),
            ("M306 T", Allow, cal, cal),
            ("M306 E0 P40", Allow, Allow, Allow),
            ("PID_CALIBRATE HEATER=extruder TARGET=210", Allow, cal, cal),
            ("SHAPER_CALIBRATE", Allow, cal, cal),
            // Routines that drive the axes: Z stall alignment, driver and
            // encoder tests.
            ("M915 S30 Z5", Allow, cal, cal),
            ("M916 X", Allow, cal, cal),
            ("M863 X", Allow, cal, cal),
            // Deploying the probe drops its pin onto the part.
            ("M401", Allow, home, home),
            ("M402", Allow, Allow, Allow),
            ("M420 S0", Allow, level, level),
            ("M420 Z10", Allow, level, level),
            ("M420 V", Allow, Allow, Allow),
            // Klipper and RepRapFirmware ways to drop or swap the mesh.
            ("BED_MESH_CLEAR", Allow, level, level),
            ("BED_MESH_PROFILE LOAD=default", Allow, level, level),
            ("bed_mesh_profile load=\"pei\"", Allow, level, level),
            ("BED_MESH_PROFILE SAVE=pei", Allow, Allow, Allow),
            ("M561", Allow, level, level),
        ]);
    }

    #[test]
    fn tools_motors_and_heaters() {
        let tool = Confirm(R::ToolChange);
        let motors = Confirm(R::MotorsOff);
        let off = Confirm(R::HeaterOff);
        let wait = Confirm(R::WaitsForTemperature);
        check(&[
            ("T1", Allow, tool, tool),
            ("t0", Allow, tool, tool),
            // RepRapFirmware deselects every tool with T-1; Prusa's MMU
            // asks for a filament with T? and loads with Tx / Tc.
            ("T-1", Allow, tool, tool),
            ("T?", Allow, tool, tool),
            ("Tx", Allow, tool, tool),
            ("TC", Allow, tool, tool),
            ("TEMPERATURE_WAIT SENSOR=bed MAXIMUM=40", Allow, wait, Allow),
            ("ACTIVATE_EXTRUDER EXTRUDER=extruder1", Allow, tool, tool),
            ("M84", Allow, motors, motors),
            ("M18 X Y", Allow, motors, motors),
            ("M84 E", Allow, motors, motors),
            ("M84 S600", Allow, Allow, Allow),
            (
                "SET_STEPPER_ENABLE STEPPER=stepper_x ENABLE=0",
                Allow,
                motors,
                motors,
            ),
            (
                "SET_STEPPER_ENABLE STEPPER=stepper_x ENABLE=1",
                Allow,
                Allow,
                Allow,
            ),
            ("M104 S215", Allow, Allow, Allow),
            ("M104 S0", Allow, off, Allow),
            ("M104 T1 S150", Allow, off, Allow),
            ("M104", Allow, Allow, Allow),
            ("M140 S55", Allow, Allow, Allow),
            ("M140 S0", Allow, off, Allow),
            ("M141 S0", Allow, off, Allow),
            ("M109 S220", Allow, wait, Allow),
            ("M109 R150", Allow, off, Allow),
            ("M190 S60", Allow, wait, Allow),
            ("M116", Allow, wait, Allow),
            ("M192 R40", Allow, wait, Allow),
            (
                "TEMPERATURE_WAIT SENSOR=extruder MINIMUM=200",
                Allow,
                wait,
                Allow,
            ),
            ("TURN_OFF_HEATERS", Allow, off, Allow),
            (
                "SET_HEATER_TEMPERATURE HEATER=extruder TARGET=215",
                Allow,
                Allow,
                Allow,
            ),
            (
                "SET_HEATER_TEMPERATURE HEATER=extruder TARGET=0",
                Allow,
                off,
                Allow,
            ),
            (
                "SET_HEATER_TEMPERATURE HEATER=extruder1 TARGET=100",
                Allow,
                off,
                Allow,
            ),
            (
                "SET_HEATER_TEMPERATURE HEATER=heater_bed TARGET=60",
                Allow,
                Allow,
                Allow,
            ),
            (
                "SET_HEATER_TEMPERATURE HEATER=heater_bed",
                Allow,
                off,
                Allow,
            ),
            ("G10 P0 S0 R0", Allow, off, Allow),
            ("G10 P0 S210 R170", Allow, Allow, Allow),
        ]);
    }

    #[test]
    fn jobs_and_sd() {
        let sd = Block(R::WritesToSd);
        let start = Confirm(R::StartsAnotherPrint);
        let end = Confirm(R::EndsPrint);
        check(&[
            ("M28 part.gco", Allow, sd, sd),
            ("M29", Allow, Allow, Allow),
            ("M24", Allow, Block(R::StartsAnotherPrint), start),
            ("M32 /part.gco", Allow, Block(R::StartsAnotherPrint), start),
            (
                "SDCARD_PRINT_FILE FILENAME=x.gcode",
                Allow,
                Block(R::StartsAnotherPrint),
                start,
            ),
            ("PRINT_START BED=60", Allow, start, start),
            ("END_PRINT", Allow, end, end),
            ("CANCEL_PRINT", Allow, end, end),
            ("M524", Allow, end, end),
        ]);
    }

    #[test]
    fn tuning_and_unknown_commands_are_allowed() {
        for line in [
            "M106 S255",
            "M107",
            "M220 S120",
            "M221 S95",
            "M290 Z0.02",
            "M600",
            "M117 G28 is next",
            "M0 Insert the magnets",
            "M105",
            "M114",
            "M486 P1",
            "EXCLUDE_OBJECT NAME=part_1",
            "SET_FAN_SPEED FAN=aux SPEED=0.5",
            "SET_PRESSURE_ADVANCE ADVANCE=0.04",
            "SET_VELOCITY_LIMIT ACCEL=3000",
            "MY_MACRO",
            "@pause",
            "M110 N0",
            "G4 P500",
            "S100",
            "é",
            "M99999999999",
            "G1.99999999999",
        ] {
            for ctx in [Idle, Printing, Paused] {
                let v = classify(line, ctx);
                assert!(v.is_allowed(), "{line:?} while {ctx:?}: {v:?}");
                assert!(!v.emergency, "{line:?}");
            }
        }
    }

    #[test]
    fn verdict_helpers_and_messages() {
        let v = classify("G28", Printing);
        assert!(v.needs_confirmation() && !v.is_allowed() && !v.is_blocked());
        assert_eq!(v.reason(), Some(R::HomesOrProbes));
        assert!(v.reason().is_some_and(|r| r.to_string().contains("crash")));
        let v = classify("M28 x.gco", Printing);
        assert!(v.is_blocked());
        assert_eq!(
            R::InterleavesWithPrint.message(),
            "it would interleave with the print: the nozzle moves between the print's own lines"
        );
        assert_eq!(classify("M105", Idle).reason(), None);
        assert_eq!(JobContext::default(), Idle);
    }

    #[test]
    fn odd_input_never_panics() {
        for line in [
            "N",
            "N99999999999999999999 M502",
            "G",
            "M",
            "T",
            "(",
            "(((",
            "\"",
            "G1 X\"",
            "G1 X{",
            "M104 S",
            "M104 S-",
            "M104 S1e999",
            "G28 (",
            "ééé",
            "\u{0}",
            "*",
            "M 1 2",
            "G1 X1.2.3",
            "SET_HEATER_TEMPERATURE TARGET=abc",
            "=",
            "N1 *",
        ] {
            for ctx in [Idle, Printing, Paused] {
                let _ = classify(line, ctx);
            }
        }
        // An overflowing line number still finds the command.
        assert_eq!(
            classify("N99999999999999999999 M502", Idle).decision,
            Confirm(R::Dangerous)
        );
    }
}
