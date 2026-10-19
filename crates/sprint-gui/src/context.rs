//! Shared per-frame context handed to every panel.
//!
//! The app shell owns the [`sprint_host::Printer`], the configuration, the
//! loaded file and the toasts. Once per frame it takes a [`PrinterState`]
//! snapshot and lends everything to the panels through an [`AppCtx`].
//! Panels talk to the printer directly (`ctx.printer.send(..)`,
//! `ctx.printer.home(..)`, ...) and ask the shell for app-level things
//! (opening files, switching tabs, selecting a line) by pushing an
//! [`AppAction`].
//!
//! Contract for the panel modules: fields and variants here may be added,
//! never renamed or removed. Panels must not add [`AppAction`] variants;
//! anything else they need they handle locally.

use std::path::PathBuf;
use std::sync::Arc;

use sprint_gcode::{GcodeFile, Toolpath};
use sprint_host::{AppConfig, Printer, PrinterState};

use crate::widgets::Toasts;

/// A G-code file loaded into the app.
#[derive(Clone, Debug)]
pub struct LoadedFile {
    /// Where it was loaded from (None for files created in memory).
    pub path: Option<PathBuf>,
    /// The file's text and line index.
    pub file: Arc<GcodeFile>,
    /// Analysis result; `None` while the background analysis runs or after
    /// it failed or was cancelled.
    pub toolpath: Option<Arc<Toolpath>>,
    /// Background analysis progress in `0.0..=1.0` while it runs.
    pub analysis_progress: Option<f32>,
}

/// The center tabs.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Tab {
    /// 3D / layer toolpath view.
    #[default]
    View3D,
    /// G-code text view.
    Gcode,
    /// Temperature graph.
    Temperature,
    /// Firmware, capabilities, statistics, link and thermal health.
    Printer,
    /// EEPROM settings editor.
    Tuning,
    /// Calibration wizards.
    Calibration,
    /// Bed mesh heatmap and screw adjustment.
    BedMesh,
    /// SD card manager.
    SdCard,
}

impl Tab {
    /// Every tab in display order.
    pub const ALL: [Tab; 8] = [
        Tab::View3D,
        Tab::Gcode,
        Tab::Temperature,
        Tab::Printer,
        Tab::Tuning,
        Tab::Calibration,
        Tab::BedMesh,
        Tab::SdCard,
    ];

    /// Tab title.
    pub fn title(self) -> &'static str {
        match self {
            Tab::View3D => "3D view",
            Tab::Gcode => "G-code",
            Tab::Temperature => "Temperature",
            Tab::Printer => "Printer",
            Tab::Tuning => "Tuning",
            Tab::Calibration => "Calibration",
            Tab::BedMesh => "Bed mesh",
            Tab::SdCard => "SD card",
        }
    }

    /// The tab's icon (see [`crate::icons`]).
    pub fn icon(self) -> &'static str {
        match self {
            Tab::View3D => crate::icons::VIEW_3D,
            Tab::Gcode => crate::icons::CODE,
            Tab::Temperature => crate::icons::CHART,
            Tab::Printer => crate::icons::PRINTER,
            Tab::Tuning => crate::icons::TUNING,
            Tab::Calibration => crate::icons::CALIBRATION,
            Tab::BedMesh => crate::icons::MESH,
            Tab::SdCard => crate::icons::SD_CARD,
        }
    }
}

/// Requests from panels to the app shell, handled after the frame's panels
/// have been drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum AppAction {
    /// Show the "open G-code" file dialog.
    OpenFileDialog,
    /// Load (and analyze) a G-code file.
    LoadFile(PathBuf),
    /// Switch the center area to a tab.
    ShowTab(Tab),
    /// Select a source line: highlight it in the viewer and scroll the
    /// G-code view to it.
    SelectLine(u32),
    /// Open the start-print dialog, starting at the given 0-based line.
    StartPrintDialog {
        /// First line to stream (0 = beginning).
        start_line: usize,
    },
    /// Open the settings dialog on the active printer profile.
    OpenSettings,
    /// Load the current file again from disk (refused while it prints).
    ReloadFile,
    /// Unload the current file.
    CloseFile,
    /// Connect with the active profile's connection target.
    Connect,
    /// Close the printer connection.
    Disconnect,
    /// Close the connection and open it again.
    Reconnect,
    /// Pause the running job.
    PauseJob,
    /// Resume the paused job.
    ResumeJob,
    /// Ask whether to cancel the running job (and cancel it on "yes").
    CancelJob,
    /// Open the G-code tab's find bar.
    FindInGcode,
    /// Open the G-code tab's "go to line" field.
    GotoLine,
    /// Show the keyboard shortcuts.
    OpenShortcuts,
    /// Show the about dialog.
    OpenAbout,
    /// Switch between the dark and light theme.
    SetTheme(crate::theme::Theme),
    /// Stop loading or analyzing the file.
    CancelLoading,
    /// Analyze the loaded file again (current profile and printer limits).
    Reanalyze,
    /// Open the settings dialog on the app preferences.
    OpenPreferences,
    /// Open the loaded file in an external text editor; Sprint reloads it
    /// when it is saved there.
    EditFile,
    /// Reset the printer's board (the way out of an emergency stop:
    /// `Printer::reset_board`), off the UI thread.
    ResetBoard,
    /// Open the start-print dialog to go on with the last failed or
    /// cancelled print of the loaded file from the line it stopped at
    /// (see `job_bar::recover_point`).
    RecoverPrint,
}

/// Everything a panel may use during one frame.
pub struct AppCtx<'a> {
    /// The printer handle (cheap to clone; all calls are non-blocking except
    /// `start_*`/`upload_to_sd`, which wait briefly for the engine).
    pub printer: &'a Printer,
    /// Snapshot of the printer state taken at the start of this frame.
    pub state: &'a PrinterState,
    /// Application configuration (profiles, macros, recent files). Set
    /// `*config_dirty = true` after changing it so the shell saves it.
    pub config: &'a mut AppConfig,
    /// Set to true when `config` was modified.
    pub config_dirty: &'a mut bool,
    /// The loaded G-code file, if any.
    pub file: Option<&'a LoadedFile>,
    /// Toast notifications.
    pub toasts: &'a mut Toasts,
    /// Requests for the shell (see [`AppAction`]).
    pub actions: &'a mut Vec<AppAction>,
    /// The currently selected source line (viewer / G-code view selection).
    pub selected_line: Option<u32>,
}

impl AppCtx<'_> {
    /// True when the printer is connected and the handshake is done.
    pub fn connected(&self) -> bool {
        self.state.is_connected()
    }

    /// True while a job (host print, SD print or SD upload) is active.
    pub fn job_active(&self) -> bool {
        self.state
            .job
            .as_ref()
            .is_some_and(|job| job.status.is_active())
    }

    /// Why the printer takes no commands now (see [`command_blocker`]).
    pub fn command_blocker(&self) -> Option<&'static str> {
        command_blocker(self.state)
    }

    /// True while the printer is busy printing or uploading (see
    /// [`printer_busy`]).
    pub fn printer_busy(&self) -> bool {
        printer_busy(self.state)
    }

    /// Why the machine must not be moved, homed, probed or calibrated now
    /// (see [`motion_blocker`]).
    pub fn motion_blocker(&self) -> Option<&'static str> {
        motion_blocker(self.state)
    }
}

/// Why the printer takes no commands now: it is not connected, or its
/// firmware is halted (it ignores everything until its board is reset, or
/// after some errors until M999 restarts it).
/// Panels disable their actions with this as the hover text, and skip
/// their automatic queries.
pub fn command_blocker(state: &PrinterState) -> Option<&'static str> {
    if !state.is_connected() {
        Some("Connect to a printer first")
    } else if state.halted {
        Some("The printer is halted: reset its board first (the red banner offers it)")
    } else {
        None
    }
}

/// True while the printer is busy printing or uploading: a job Sprint runs
/// (host print, SD print or SD upload), or a print from the card started
/// on the printer itself (its SD progress reports). The last report of an
/// SD print Sprint ran and that has ended is not a print.
pub fn printer_busy(state: &PrinterState) -> bool {
    if state.job_active() {
        return true;
    }
    let sd_print_ended = state
        .job
        .as_ref()
        .is_some_and(|j| j.kind == sprint_host::JobKind::SdPrint && j.status.is_finished());
    state.sd.printing.is_some() && !sd_print_ended
}

/// Why the machine must not be moved, homed, probed or calibrated now:
/// [`command_blocker`], or a print or upload running ([`printer_busy`]),
/// whose moves the commands would interleave with.
pub fn motion_blocker(state: &PrinterState) -> Option<&'static str> {
    command_blocker(state).or_else(|| {
        if state.job_active() {
            Some("Not while a job runs")
        } else if printer_busy(state) {
            Some("Not while the printer prints from its card")
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tab_has_a_distinct_title_and_icon() {
        let titles: std::collections::HashSet<_> = Tab::ALL.iter().map(|t| t.title()).collect();
        let icons: std::collections::HashSet<_> = Tab::ALL.iter().map(|t| t.icon()).collect();
        assert_eq!(titles.len(), Tab::ALL.len());
        assert_eq!(icons.len(), Tab::ALL.len());
        assert_eq!(Tab::default(), Tab::View3D);
    }

    #[test]
    fn blockers_cover_offline_halted_jobs_and_card_prints() {
        use sprint_host::{ConnectionStatus, JobInfo, JobKind, JobStatus};
        let mut state = PrinterState::default();
        assert!(command_blocker(&state).is_some());
        state.status = ConnectionStatus::Connected;
        assert_eq!(command_blocker(&state), None);
        assert_eq!(motion_blocker(&state), None);
        state.halted = true;
        assert!(command_blocker(&state).is_some_and(|r| r.contains("halted")));
        state.halted = false;
        // A print started on the printer: only its SD progress tells.
        state.sd.printing = Some((10, 100));
        assert!(printer_busy(&state));
        assert!(motion_blocker(&state).is_some_and(|r| r.contains("card")));
        assert_eq!(command_blocker(&state), None);
        // The last report of an SD print Sprint ran that has ended is not.
        let mut job = JobInfo::new(JobKind::SdPrint, "A.GCO", 0, 100);
        job.status = JobStatus::Cancelled;
        state.job = Some(job.clone());
        assert!(!printer_busy(&state));
        job.status = JobStatus::Printing;
        state.job = Some(job);
        state.sd.printing = None;
        assert!(printer_busy(&state));
        assert!(motion_blocker(&state).is_some_and(|r| r.contains("job")));
    }
}
