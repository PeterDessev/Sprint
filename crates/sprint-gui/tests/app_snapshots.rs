//! Snapshots of the main window in its typical states (see `app_support`
//! for how they are compared).
//!
//! The printer-dependent states use a synthetic [`PrinterState`] (through
//! `SprintApp::set_state_override`) so the images are reproducible, and a
//! frozen clock for "finishes at". The controls sidebar is closed: it is
//! another panel's work and has its own snapshots.

mod app_support;

use std::time::Duration;

use app_support::{WINDOW, app, expect, settle, snapshot};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sprint_gui::app::{AppOptions, DialogKind, SprintApp};
use sprint_gui::context::{AppAction, Tab};
use sprint_gui::theme::Theme;
use sprint_host::sprint_proto::{HeaterReading, TempReport, parse_firmware_line};
use sprint_host::{
    ConnectionStatus, ConsoleEntry, ConsoleKind, HostPrompt, JobInfo, JobKind, JobStatus,
    LinkStats, PrinterState,
};

/// 2024-01-01 12:00 local time, for "finishes at".
const NOON: i64 = 1_704_110_400;

/// The app with the calibration cube loaded and analyzed.
fn with_cube(options: AppOptions) -> Harness<'static, SprintApp> {
    with_cube_at(options, WINDOW)
}

/// [`with_cube`] in a window of `size`.
fn with_cube_at(options: AppOptions, size: egui::Vec2) -> Harness<'static, SprintApp> {
    let options = AppOptions {
        file: Some(app_support::sample("calibration-cube.gcode")),
        ..options
    };
    let mut h = app(options, size);
    h.state_mut().set_side_panels(false, true);
    expect(&mut h, "the cube to be analyzed", |app| {
        app.file().is_some_and(|f| f.toolpath.is_some()) && !app.is_loading()
    });
    expect(&mut h, "the 3D view to be ready", |app| {
        app.viewer_tab().viewer().is_none_or(|v| !v.is_loading())
    });
    settle(&mut h);
    h.state_mut().toasts_mut().clear();
    settle(&mut h);
    h
}

fn connected_state() -> PrinterState {
    let firmware = parse_firmware_line(
        "FIRMWARE_NAME:Marlin 2.1.2.1 (Github) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin \
         PROTOCOL_VERSION:1.0 MACHINE_TYPE:Sprint Virtual Printer EXTRUDER_COUNT:1",
    );
    PrinterState {
        status: ConnectionStatus::Connected,
        target: Some("Virtual printer".into()),
        firmware,
        temps: TempReport {
            hotends: vec![HeaterReading {
                current: 214.6,
                target: Some(215.0),
                power: Some(0.42),
            }],
            bed: Some(HeaterReading {
                current: 59.8,
                target: Some(60.0),
                power: Some(0.18),
            }),
            ..TempReport::default()
        },
        link: LinkStats {
            connected_secs: 1325.0,
            lines_sent: 48_211,
            oks: 48_208,
            resends: 2,
            rtt_last_ms: 2.1,
            rtt_avg_ms: 2.4,
            rtt_max_ms: 9.8,
            lines_per_sec: 37.0,
            ..LinkStats::default()
        },
        feedrate_pct: 100.0,
        flow_pct: vec![100.0],
        ..PrinterState::default()
    }
}

fn printing_state(app: &SprintApp) -> PrinterState {
    let file = app.file().expect("file");
    let toolpath = file.toolpath.as_ref().expect("toolpath");
    let layers = toolpath.layers.len();
    let layer = (layers * 2 / 5).min(layers.saturating_sub(1));
    let current = toolpath.layers.get(layer).map_or(0, |l| {
        (l.lines.start + (l.lines.end - l.lines.start) / 2) as usize
    });
    let mut job = JobInfo::new(JobKind::HostPrint, file.file.name(), file.file.len(), 0);
    job.path = file.file.path().map(|p| p.to_path_buf());
    job.status = JobStatus::Printing;
    job.current_line = current;
    job.progress = 0.42;
    job.elapsed = Duration::from_secs(18 * 60 + 41);
    job.eta = Some(Duration::from_secs(24 * 60 + 5));
    job.estimated_total = Some(Duration::from_secs(43 * 60));
    job.layer = Some((layer, layers));
    job.filament_used_mm = 1834.0;
    let mut state = connected_state();
    state.job = Some(job);
    state
        .firmware_messages
        .push_back("Printing calibration-cube".into());
    state
}

fn entry(seq: u64, kind: ConsoleKind, text: &str) -> ConsoleEntry {
    ConsoleEntry {
        seq: 1_000_000 + seq,
        time: seq as f64 * 0.7,
        kind,
        text: text.into(),
        auto: false,
        line: None,
        stream: false,
    }
}

/// A plausible console after connecting.
fn console_traffic(app: &mut SprintApp) {
    let lines = [
        (ConsoleKind::Info, "Connecting to Virtual printer"),
        (ConsoleKind::Received, "start"),
        (ConsoleKind::Received, "echo:Marlin 2.1.2.1"),
        (ConsoleKind::Sent, "M110 N0"),
        (ConsoleKind::Received, "ok"),
        (ConsoleKind::Sent, "M115"),
        (
            ConsoleKind::Received,
            "FIRMWARE_NAME:Marlin 2.1.2.1 (Github) PROTOCOL_VERSION:1.0 MACHINE_TYPE:Sprint Virtual Printer EXTRUDER_COUNT:1",
        ),
        (ConsoleKind::Received, "Cap:AUTOREPORT_TEMP:1"),
        (ConsoleKind::Received, "Cap:EEPROM:1"),
        (ConsoleKind::Received, "ok"),
        (ConsoleKind::Sent, "M155 S2"),
        (ConsoleKind::Received, "ok"),
        (ConsoleKind::Info, "Connected: Marlin 2.1.2.1"),
        (ConsoleKind::Sent, "G28"),
        (ConsoleKind::Received, "echo:busy: processing"),
        (
            ConsoleKind::Received,
            "X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0",
        ),
        (ConsoleKind::Received, "ok"),
        (ConsoleKind::Sent, "M104 S215"),
        (ConsoleKind::Received, "ok"),
        (ConsoleKind::Warning, "Line 1532: resend requested (N1532)"),
        (ConsoleKind::Sent, "M117 Printing calibration-cube"),
        (ConsoleKind::Received, "echo:Printing calibration-cube"),
        (ConsoleKind::Received, "ok"),
    ];
    let entries = lines
        .iter()
        .enumerate()
        .map(|(i, (kind, text))| entry(i as u64, *kind, text))
        .collect();
    app.console_mut().push_entries(entries);
}

#[test]
fn empty_window() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut().set_side_panels(false, true);
    settle(&mut h);
    snapshot(&mut h, "app_empty");
}

#[test]
fn file_loaded() {
    let mut h = with_cube(AppOptions::headless());
    assert!(h.query_by_label("3D view").is_some());
    snapshot(&mut h, "app_file_loaded");
}

#[test]
fn printing() {
    sprint_gui::format::freeze_clock(Some(NOON));
    let mut h = with_cube(AppOptions::headless());
    let state = printing_state(h.state());
    h.state_mut().set_state_override(Some(state));
    console_traffic(h.state_mut());
    settle(&mut h);
    snapshot(&mut h, "app_printing");

    // The G-code tab follows the print.
    h.state_mut().push_action(AppAction::ShowTab(Tab::Gcode));
    settle(&mut h);
    snapshot(&mut h, "app_printing_gcode");
}

#[test]
fn start_print_dialog() {
    let mut h = with_cube(AppOptions::headless());
    h.state_mut().set_state_override(Some(connected_state()));
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::StartPrint));
    let dialog = h.state_mut().start_print_dialog_mut().expect("dialog");
    dialog.set_mode(sprint_gui::dialogs::StartMode::Layer);
    dialog.set_layer(12);
    // A pause to swap the filament further up.
    dialog.set_pause_layer(Some(40));
    settle(&mut h);
    snapshot(&mut h, "app_start_print");
}

#[test]
fn settings_dialog() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut().set_side_panels(false, true);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::Settings));
    snapshot(&mut h, "app_settings");
}

#[test]
fn halted_with_a_host_prompt() {
    let mut h = with_cube(AppOptions::headless());
    let mut state = connected_state();
    state.halted = true;
    state.last_error = Some("Printer halted. kill() called!".into());
    h.state_mut().set_state_override(Some(state.clone()));
    settle(&mut h);
    snapshot(&mut h, "app_halted");

    state.halted = false;
    state.last_error = None;
    state.prompt = Some(HostPrompt {
        message: "Filament runout: insert new filament".into(),
        buttons: vec!["Continue".into(), "Purge more".into()],
    });
    h.state_mut().set_state_override(Some(state));
    settle(&mut h);
    snapshot(&mut h, "app_prompt");
}

#[test]
fn light_theme() {
    let mut h = with_cube(AppOptions::headless());
    h.state_mut().push_action(AppAction::SetTheme(Theme::Light));
    h.state_mut().set_state_override(Some(connected_state()));
    console_traffic(h.state_mut());
    settle(&mut h);
    snapshot(&mut h, "app_light");
}

#[test]
fn smallest_window() {
    sprint_gui::format::freeze_clock(Some(NOON));
    let size = egui::Vec2::from(sprint_gui::MIN_WINDOW_SIZE);
    let mut h = with_cube_at(AppOptions::headless(), size);
    let state = printing_state(h.state());
    h.state_mut().set_state_override(Some(state));
    settle(&mut h);
    // The toolbar is compact and the cards share the narrow view.
    assert!(
        h.query_by_label("Follow print: move the top layer with the print")
            .is_some()
    );
    snapshot(&mut h, "app_smallest");
}

#[test]
fn smallest_window_halted() {
    // The banner leaves the 3D view little height: the file card scrolls
    // instead of running under the scrub bar. The firmware's prompt is not
    // shown while it is halted (it ignores the answer).
    let size = egui::Vec2::from(sprint_gui::MIN_WINDOW_SIZE);
    let mut h = with_cube_at(AppOptions::headless(), size);
    let mut state = connected_state();
    state.halted = true;
    state.last_error = Some("Printer halted. kill() called!".into());
    state.prompt = Some(HostPrompt {
        message: "Insert filament and press Continue".into(),
        buttons: vec!["Continue".into()],
    });
    h.state_mut().set_state_override(Some(state));
    h.state_mut().viewer_tab_mut().prefs.show_legend = false;
    settle(&mut h);
    assert!(h.query_by_label("Hide").is_none());
    assert!(h.query_by_label("Reset board").is_some());
    h.remove_cursor();
    settle(&mut h);
    snapshot(&mut h, "app_smallest_halted");
}

#[test]
fn console_with_a_long_paste() {
    // Twenty pasted lines: the field shows five and scrolls, inside the
    // panel.
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut().set_side_panels(false, true);
    let text: String = (0..20)
        .map(|i| {
            format!(
                "G1 X{i} Y{i} F3000
"
            )
        })
        .collect();
    h.state_mut().console_mut().set_input(&text);
    h.state_mut().console_mut().focus_input();
    settle(&mut h);
    snapshot(&mut h, "app_console_paste");
}
