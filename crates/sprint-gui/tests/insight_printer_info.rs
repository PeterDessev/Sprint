//! The Printer tab: snapshots of a synthetic printer and behaviour against
//! the simulator.

mod common;
mod insight_common;

use common::{harness_state, snapshot};
use egui_kittest::kittest::Queryable as _;
use insight_common::*;
use sprint_gui::context::{AppAction, Tab};
use sprint_gui::panels::calibration::Wizard;
use sprint_gui::panels::{CalibrationTab, PrinterInfoTab};
use sprint_gui::theme::Theme;
use sprint_host::{Heater, PrinterState};

/// Frames that feed the tab's round-trip history with varying samples.
fn feed_link_history(h: &mut egui_kittest::Harness<'_, Host<PrinterInfoTab>>) {
    for i in 0..48 {
        if let Some(state) = h.state_mut().state.as_mut() {
            state.link.connected_secs += 1.0;
            let x = i as f32;
            state.link.rtt_last_ms = 2.6 + 0.6 * (x / 3.0).sin() + if i == 30 { 9.0 } else { 0.0 };
        }
        h.step();
    }
}

#[test]
fn printer_tab_snapshots() {
    for theme in Theme::ALL {
        let mut h = harness_state(
            theme,
            (1180.0, 1290.0),
            Host::<PrinterInfoTab>::synthetic(demo_state()),
            |ui, host| host.frame(ui),
        );
        feed_link_history(&mut h);
        h.run_ok();
        snapshot(&mut h, &themed("insight_printer", theme));
    }
}

#[test]
fn printer_tab_two_columns_with_findings() {
    let mut state = demo_state();
    // A bed swinging ±1.6 °C: the tab suggests a PID autotune.
    state.temp_history = demo_history(1.6);
    let mut h = harness_state(
        Theme::Dark,
        (760.0, 1500.0),
        Host::<PrinterInfoTab>::synthetic(state),
        |ui, host| host.frame(ui),
    );
    feed_link_history(&mut h);
    h.run_ok();
    snapshot(&mut h, "insight_printer_findings_dark");
    // The finding offers a shortcut to the calibration tab.
    h.get_by_label("Calibrate PID").click();
    h.run_ok();
    assert!(
        h.state()
            .actions
            .contains(&AppAction::ShowTab(Tab::Calibration))
    );
}

/// The Printer and Calibration tabs sharing the centre area, like the
/// shell shows them.
#[derive(Default)]
struct CentreArea {
    printer: PrinterInfoTab,
    calibration: CalibrationTab,
    tab: Option<Tab>,
}

impl Panel for CentreArea {
    fn show(&mut self, ui: &mut egui::Ui, ctx: &mut sprint_gui::context::AppCtx<'_>) {
        if self.tab == Some(Tab::Calibration) {
            self.calibration.ui(ui, ctx);
        } else {
            self.printer.ui(ui, ctx);
        }
    }
}

#[test]
fn calibrate_pid_opens_the_wizard_for_that_heater() {
    let mut state = demo_state();
    // The bed swings ±1.6 °C; the hotend is fine.
    state.temp_history = demo_history(1.6);
    let mut h = harness_state(
        Theme::Dark,
        (760.0, 1500.0),
        Host::<CentreArea>::synthetic(state),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    h.get_by_label("Calibrate PID").click();
    h.run_ok();
    // The shell follows the action...
    let shown = h.state().actions.iter().find_map(|a| match a {
        AppAction::ShowTab(tab) => Some(*tab),
        _ => None,
    });
    assert_eq!(shown, Some(Tab::Calibration));
    h.state_mut().tab.tab = shown;
    h.run_ok();
    // ...and the PID wizard is ready to tune the bed, not the hotend.
    let calibration = &h.state().tab.calibration;
    assert_eq!(calibration.wizard(), Wizard::Pid);
    assert_eq!(calibration.pid_heater(), Heater::Bed);
    assert!(h.query_by_label("Start autotune at 60 °C").is_some());
}

#[test]
fn printer_tab_shows_empty_state_when_disconnected() {
    let mut h = harness_state(
        Theme::Light,
        (800.0, 400.0),
        Host::<PrinterInfoTab>::synthetic(PrinterState::default()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    assert!(h.query_by_label("Not connected").is_some());
    snapshot(&mut h, "insight_printer_disconnected_light");
    // A failed connection explains why.
    h.state_mut().state = Some(PrinterState {
        status: sprint_host::ConnectionStatus::Failed("port busy".into()),
        ..PrinterState::default()
    });
    h.run_ok();
    assert!(
        h.query_by_label_contains("port busy").is_some(),
        "the failure reason is shown"
    );
}

#[test]
fn printer_tab_shortcuts_and_copy() {
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 1060.0),
        Host::<PrinterInfoTab>::synthetic(demo_state()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    h.get_by_label("Edit in the Tuning tab").click();
    h.run_ok();
    assert!(h.state().actions.contains(&AppAction::ShowTab(Tab::Tuning)));
    h.get_by_label("Copy UUID").click();
    h.run_ok();
    assert!(
        h.state().toasts.iter().any(|t| t.title() == "UUID copied"),
        "copying the UUID confirms it"
    );
    // The capabilities chip reveals what the firmware lacks.
    assert!(h.query_by_label("MULTI_VOLUME").is_none());
    h.get_by_label("Unsupported").click();
    h.run_ok();
    assert!(h.query_by_label("MULTI_VOLUME").is_some());
}

#[test]
fn printer_tab_tolerates_garbage_state() {
    let mut state = demo_state();
    state.link.connected_secs = f64::NAN;
    state.link.rtt_last_ms = f32::INFINITY;
    state.link.lines_per_sec = f32::NAN;
    if let Some(p) = state.position.as_mut() {
        p.x = f32::NAN;
        p.stepper_counts = Some(vec![]);
    }
    if let Some(fw) = state.firmware.as_mut() {
        fw.source_url = Some("javascript:alert(1)".into());
        fw.machine_type = Some(String::new());
        fw.extruder_count = Some(0);
    }
    state
        .temp_history
        .iter_mut()
        .for_each(|s| s.hotends.clear());
    state.stats = Some(Default::default());
    state.endstops = vec![(String::new(), true)];
    state
        .firmware_messages
        .push_back("\u{1b}[31mweird\u{0}".into());
    let mut h = harness_state(
        Theme::Dark,
        (400.0, 900.0),
        Host::<PrinterInfoTab>::synthetic(state),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
}

#[test]
fn statistics_survive_huge_counters() {
    // Uninitialised EEPROM statistics read as 0xFFFFFFFF.
    let mut state = demo_state();
    state.stats = Some(sprint_host::sprint_proto::StatsReport {
        prints: Some(u32::MAX),
        finished: Some(u32::MAX),
        failed: Some(u32::MAX),
        total_time: Some(u64::MAX),
        longest_job: Some(u64::MAX),
        filament_used_mm: Some(f64::MAX),
    });
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 1060.0),
        Host::<PrinterInfoTab>::synthetic(state),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    assert!(h.query_by_label("Finished: 4,294,967,295").is_some());
    // Half of the jobs that ended were finished.
    assert!(h.query_by_label_contains("50%").is_some());
}

#[test]
fn printer_tab_queries_the_simulator() {
    let printer = sim_printer(fast_sim());
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 1060.0),
        Host::<PrinterInfoTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    // The firmware card shows what M115 reported.
    assert!(
        h.query_by_label("Marlin 2.1.2.1 (Sprint Virtual)")
            .is_some()
    );

    clear_received(&printer);
    h.get_by_label("Load the print statistics (M78)").click();
    h.run_ok();
    wait_for_command(&printer, "M78");
    run_until(&mut h, "the statistics", |s| s.stats.is_some());

    h.get_by_label("Query the endstops (M119)").click();
    h.run_ok();
    wait_for_command(&printer, "M119");
    run_until(&mut h, "the endstops", |s| !s.endstops.is_empty());
    h.run_ok();
    assert!(h.query_by_label("X min").is_some());

    clear_received(&printer);
    h.get_by_label("Report the position (M114)").click();
    h.run_ok();
    wait_for_command(&printer, "M114");

    clear_received(&printer);
    h.get_by_label("Refresh everything (M115, M78, M119, M114, M503)")
        .click();
    h.run_ok();
    for cmd in ["M115", "M78", "M119", "M114", "M503"] {
        wait_for_command(&printer, cmd);
    }

    // Live endstops poll M119 while the tab is shown.
    clear_received(&printer);
    h.get_by_label("Live").click();
    h.run_ok();
    // The poll goes out from the next frames.
    step_until_command(&mut h, "M119");
    printer.disconnect();
}

#[test]
fn live_endstops_do_not_pile_up_behind_a_long_command() {
    let printer = sim_printer(sprint_host::sprint_sim::SimConfig {
        time_scale: 1.0,
        ..fast_sim()
    });
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 1060.0),
        Host::<PrinterInfoTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    // A 5 s dwell holds the queue; ten seconds of UI time pass meanwhile.
    printer.send("G4 S5");
    wait_for(&printer, "the dwell", |s| s.link.in_flight > 0);
    h.get_by_label("Live").click();
    h.run_ok();
    let mut most = 0;
    for _ in 0..40 {
        h.step();
        most = most.max(printer.with_state(|s| s.link.queue_len));
        // Longer than the host's 20 ms stats tick, as between real frames.
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    assert!(most <= 1, "{most} commands queued behind the dwell");
    printer.disconnect();
}
