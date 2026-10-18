//! Icons: the Phosphor icon font (regular variant) with semantic names.
//!
//! Every constant is a one-character `&'static str` that works inline in
//! any text once [`crate::fonts::install`] ran:
//!
//! ```
//! use sprint_gui::icons;
//! let label = format!("{} Home all", icons::HOME);
//! let same = icons::with_text(icons::HOME, "Home all");
//! assert_eq!(label, same);
//! ```
//!
//! Use the semantic names below in Sprint's UI so the iconography stays
//! consistent; the whole Phosphor set is available as [`phosphor`].

#![warn(missing_docs)]

/// The complete Phosphor regular icon set (1500+ icons).
pub use egui_phosphor::regular as phosphor;

use phosphor as ph;

// Machine control
/// Home axes.
pub const HOME: &str = ph::HOUSE;
/// Motors off / steppers disabled.
pub const MOTORS_OFF: &str = ph::LOCK_SIMPLE_OPEN;
/// Emergency stop.
pub const ESTOP: &str = ph::HAND_PALM;
/// Generic octagon (stop sign shape).
pub const OCTAGON: &str = ph::OCTAGON;
/// Jog / move.
pub const MOVE: &str = ph::ARROWS_OUT_CARDINAL;
/// Up arrow.
pub const ARROW_UP: &str = ph::ARROW_UP;
/// Down arrow.
pub const ARROW_DOWN: &str = ph::ARROW_DOWN;
/// Left arrow.
pub const ARROW_LEFT: &str = ph::ARROW_LEFT;
/// Right arrow.
pub const ARROW_RIGHT: &str = ph::ARROW_RIGHT;
/// Caret up (small chevron).
pub const CARET_UP: &str = ph::CARET_UP;
/// Caret down (small chevron), dropdowns.
pub const CARET_DOWN: &str = ph::CARET_DOWN;
/// Caret left.
pub const CARET_LEFT: &str = ph::CARET_LEFT;
/// Caret right, disclosure.
pub const CARET_RIGHT: &str = ph::CARET_RIGHT;
/// Extrude filament.
pub const EXTRUDE: &str = ph::ARROW_FAT_LINES_DOWN;
/// Retract filament.
pub const RETRACT: &str = ph::ARROW_FAT_LINES_UP;
/// Babystepping / fine Z.
pub const BABYSTEP: &str = ph::ARROWS_DOWN_UP;
/// Position / crosshair.
pub const POSITION: &str = ph::CROSSHAIR_SIMPLE;

// Thermal
/// Hotend heater.
pub const HOTEND: &str = ph::FIRE;
/// Heated bed.
pub const BED: &str = ph::SQUARES_FOUR;
/// Heated chamber.
pub const CHAMBER: &str = ph::CUBE_TRANSPARENT;
/// Generic temperature.
pub const THERMOMETER: &str = ph::THERMOMETER_SIMPLE;
/// Heating up.
pub const HEATING: &str = ph::THERMOMETER_HOT;
/// Cooling down.
pub const COOLING: &str = ph::SNOWFLAKE;
/// Part cooling fan.
pub const FAN: &str = ph::FAN;
/// Heater power.
pub const POWER: &str = ph::LIGHTNING;
/// Switch off.
pub const OFF: &str = ph::POWER;

// Jobs and files
/// Start / resume.
pub const PLAY: &str = ph::PLAY;
/// Pause.
pub const PAUSE: &str = ph::PAUSE;
/// Stop / cancel a job.
pub const STOP: &str = ph::STOP;
/// Open a file.
pub const OPEN: &str = ph::FOLDER_OPEN;
/// A G-code file.
pub const FILE: &str = ph::FILE_CODE;
/// SD card.
pub const SD_CARD: &str = ph::SIM_CARD;
/// Upload.
pub const UPLOAD: &str = ph::UPLOAD_SIMPLE;
/// Download.
pub const DOWNLOAD: &str = ph::DOWNLOAD_SIMPLE;
/// Save (EEPROM, config).
pub const SAVE: &str = ph::FLOPPY_DISK;
/// Layers.
pub const LAYERS: &str = ph::STACK_SIMPLE;
/// Elapsed / duration.
pub const CLOCK: &str = ph::CLOCK;
/// Remaining time.
pub const TIMER: &str = ph::TIMER;
/// Waiting.
pub const HOURGLASS: &str = ph::HOURGLASS_MEDIUM;
/// Filament spool usage.
pub const FILAMENT: &str = ph::CYLINDER;

// Views and panels
/// 3D view.
pub const VIEW_3D: &str = ph::CUBE;
/// Layer / 2D view.
pub const VIEW_LAYER: &str = ph::SQUARE_HALF;
/// Console / terminal.
pub const CONSOLE: &str = ph::TERMINAL_WINDOW;
/// G-code text.
pub const CODE: &str = ph::CODE;
/// Temperature graph.
pub const CHART: &str = ph::CHART_LINE;
/// Printer / firmware info.
pub const PRINTER: &str = ph::PRINTER;
/// Firmware / board.
pub const FIRMWARE: &str = ph::CPU;
/// Tuning (settings editor).
pub const TUNING: &str = ph::SLIDERS_HORIZONTAL;
/// Calibration tools.
pub const CALIBRATION: &str = ph::GAUGE;
/// Bed mesh.
pub const MESH: &str = ph::GRID_FOUR;
/// Settings.
pub const SETTINGS: &str = ph::GEAR_SIX;
/// Speed / feedrate override.
pub const SPEED: &str = ph::SPEEDOMETER;
/// Flow override.
pub const FLOW: &str = ph::DROP;
/// Macros.
pub const MACRO: &str = ph::LIGHTNING_A;
/// Statistics.
pub const STATS: &str = ph::CHART_BAR;
/// Link / connection statistics.
pub const LINK: &str = ph::PULSE;
/// Endstops.
pub const ENDSTOP: &str = ph::TARGET;
/// Ruler / measurement.
pub const RULER: &str = ph::RULER;

// Connection
/// Connect.
pub const CONNECT: &str = ph::PLUGS_CONNECTED;
/// Disconnect / disconnected.
pub const DISCONNECT: &str = ph::PLUGS;
/// Serial / USB port.
pub const USB: &str = ph::USB;
/// Network (TCP) connection.
pub const NETWORK: &str = ph::GLOBE_SIMPLE;
/// Virtual printer (simulator).
pub const SIMULATOR: &str = ph::FLASK;
/// Refresh / rescan.
pub const REFRESH: &str = ph::ARROWS_CLOCKWISE;
/// Restart / reset.
pub const RESTART: &str = ph::ARROW_COUNTER_CLOCKWISE;

// Feedback
/// Information.
pub const INFO: &str = ph::INFO;
/// Success / done.
pub const SUCCESS: &str = ph::CHECK_CIRCLE;
/// Warning.
pub const WARNING: &str = ph::WARNING;
/// Error.
pub const ERROR: &str = ph::WARNING_OCTAGON;
/// Question / help.
pub const HELP: &str = ph::QUESTION;
/// Check mark.
pub const CHECK: &str = ph::CHECK;
/// Close / dismiss.
pub const CLOSE: &str = ph::X;
/// Notification bell.
pub const BELL: &str = ph::BELL_SIMPLE;

// Editing
/// Add.
pub const ADD: &str = ph::PLUS;
/// Remove / decrease.
pub const REMOVE: &str = ph::MINUS;
/// Delete.
pub const DELETE: &str = ph::TRASH_SIMPLE;
/// Edit.
pub const EDIT: &str = ph::PENCIL_SIMPLE;
/// Copy to clipboard.
pub const COPY: &str = ph::COPY_SIMPLE;
/// Search.
pub const SEARCH: &str = ph::MAGNIFYING_GLASS;
/// Filter.
pub const FILTER: &str = ph::FUNNEL_SIMPLE;
/// Send (console).
pub const SEND: &str = ph::PAPER_PLANE_RIGHT;
/// Clear / sweep.
pub const CLEAR: &str = ph::BROOM;
/// Reset to original value.
pub const RESET: &str = ph::ARROW_U_UP_LEFT;
/// Overflow menu.
pub const MORE: &str = ph::DOTS_THREE;
/// Empty tray (empty states).
pub const EMPTY: &str = ph::TRAY;
/// Light theme.
pub const SUN: &str = ph::SUN;
/// Dark theme.
pub const MOON: &str = ph::MOON;
/// Visibility on.
pub const EYE: &str = ph::EYE;
/// Visibility off.
pub const EYE_OFF: &str = ph::EYE_SLASH;
/// Locked.
pub const LOCK: &str = ph::LOCK_SIMPLE;

/// `"{icon} {text}"`: an icon followed by a label, with the spacing used
/// throughout Sprint (an empty `icon` yields just the text).
pub fn with_text(icon: &str, text: &str) -> String {
    if icon.is_empty() {
        text.to_owned()
    } else if text.is_empty() {
        icon.to_owned()
    } else {
        format!("{icon} {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_single_private_use_chars() {
        for icon in [
            HOME,
            MOTORS_OFF,
            ESTOP,
            HOTEND,
            BED,
            CHAMBER,
            FAN,
            PLAY,
            PAUSE,
            STOP,
            SD_CARD,
            CONSOLE,
            TUNING,
            CALIBRATION,
            MESH,
            SUCCESS,
            WARNING,
            ERROR,
            INFO,
            CLOSE,
            COPY,
            EXTRUDE,
            RETRACT,
            EMPTY,
            RESET,
        ] {
            let mut chars = icon.chars();
            let c = chars.next().expect("one char");
            assert!(chars.next().is_none());
            assert!(('\u{E000}'..='\u{F8FF}').contains(&c), "{c:?} not in PUA");
        }
    }

    #[test]
    fn with_text_joins() {
        assert_eq!(with_text(HOME, "Home"), format!("{HOME} Home"));
        assert_eq!(with_text("", "Home"), "Home");
        assert_eq!(with_text(HOME, ""), HOME);
    }
}
