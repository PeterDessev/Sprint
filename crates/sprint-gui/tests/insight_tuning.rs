//! The Tuning tab: snapshots and the settings workflow against the
//! simulator.

mod common;
mod insight_common;

use common::{harness_state, snapshot};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use insight_common::*;
use sprint_gui::panels::TuningTab;
use sprint_gui::theme::Theme;
use sprint_host::PrinterState;
use sprint_host::sprint_proto::SettingCommand;

fn cmd(text: &str) -> SettingCommand {
    SettingCommand::parse(text).expect("setting command")
}

#[test]
fn tuning_tab_snapshots() {
    for theme in Theme::ALL {
        let mut h = harness_state(
            theme,
            (1180.0, 900.0),
            Host::<TuningTab>::synthetic(demo_state()),
            |ui, host| host.frame(ui),
        );
        h.run_ok();
        h.state_mut().tab.stage(cmd("M92 X80 Y80 Z400 E95.4"));
        h.state_mut().tab.stage(cmd("M301 P21.73 I1.5 D78.68"));
        h.run_ok();
        snapshot(&mut h, &themed("insight_tuning", theme));
    }
}

#[test]
fn tuning_tab_narrow_with_change_list() {
    let mut h = harness_state(
        Theme::Dark,
        (560.0, 900.0),
        Host::<TuningTab>::synthetic(demo_state()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    h.state_mut().tab.stage(cmd("M92 X80 Y80 Z400 E95.4"));
    h.state_mut().tab.stage(cmd("M569 S1 X Y"));
    h.run_ok();
    h.get_by_label("Show changes").click();
    h.run_ok();
    assert!(
        h.query_by_label_contains("Extruder steps: 93.00 → 95.40")
            .is_some()
    );
    assert!(h.query_by_label("Z driver: on → off").is_some());
    h.event(egui::Event::PointerGone);
    h.run_ok();
    snapshot(&mut h, "insight_tuning_changes_dark");
    h.get_by_label("Revert").click();
    h.run_ok();
    assert_eq!(h.state().tab.pending_changes(), 0);
}

#[test]
fn tuning_tab_empty_states() {
    let mut h = harness_state(
        Theme::Light,
        (800.0, 400.0),
        Host::<TuningTab>::synthetic(PrinterState::default()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    assert!(h.query_by_label("Not connected").is_some());
    // Klipper keeps its configuration in printer.cfg.
    let mut klipper = demo_state();
    klipper.settings = Default::default();
    klipper.settings_loaded = false;
    if let Some(fw) = klipper.firmware.as_mut() {
        fw.flavor = sprint_host::sprint_proto::FirmwareFlavor::Klipper;
    }
    h.state_mut().state = Some(klipper);
    h.run_ok();
    assert!(h.query_by_label("No EEPROM settings").is_some());
}

#[test]
fn tuning_tab_search_and_categories_filter_cards() {
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 900.0),
        Host::<TuningTab>::synthetic(demo_state()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    assert!(h.query_by_label_contains("Steps per unit (M92)").is_some());
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .expect("search box")
        .click();
    h.run_ok();
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .expect("search box")
        .type_text("jerk");
    h.run_ok();
    assert!(
        h.query_by_label_contains("Advanced motion (M205)")
            .is_some()
    );
    assert!(h.query_by_label_contains("Steps per unit (M92)").is_none());
    h.get_by_label("Clear the search").click();
    h.run_ok();
    // Categories narrow the list.
    h.get_by_label("Thermal").click();
    h.run_ok();
    assert!(h.query_by_label_contains("Hotend PID (M301)").is_some());
    assert!(h.query_by_label_contains("Steps per unit (M92)").is_none());
    h.get_by_label("All settings").click();
    h.run_ok();
    assert!(h.query_by_label_contains("Steps per unit (M92)").is_some());
    // Flags are switches named after the setting.
    h.get_by_label("Drivers").click();
    h.run_ok();
    // The first M569 instance (the second is the extruder's, "T0 E").
    h.get_all_by_role_and_label(egui::accesskit::Role::CheckBox, "Z driver")
        .next()
        .expect("Z driver switch")
        .click();
    h.run_ok();
    assert_eq!(h.state().tab.pending_changes(), 1);
    assert_eq!(
        h.state()
            .tab
            .draft()
            .entry("M569")
            .map(|e| e.draft.to_gcode()),
        Some("M569 S1 X Y".to_owned())
    );
}

#[test]
fn raw_settings_cannot_become_other_commands() {
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 900.0),
        Host::<TuningTab>::synthetic(demo_state()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    // The demo's only raw entry is "G21" (units), under "Other".
    h.get_by_label("Other").click();
    h.run_ok();
    // The search box comes first, the raw field last.
    let raw_field = |h: &Harness<'_, Host<TuningTab>>| {
        h.get_all_by_role(egui::accesskit::Role::TextInput)
            .last()
            .expect("raw G-code field")
            .accesskit_node()
            .id()
    };
    for text in ["M502", "G28", "M112"] {
        let id = raw_field(&h);
        h.get_by(|n| n.id() == id).click();
        h.run_ok();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.run_ok();
        h.key_press(egui::Key::Backspace);
        h.run_ok();
        h.get_by(|n| n.id() == id).type_text(text);
        h.run_ok();
        // A factory reset, homing or an emergency stop would go out with
        // "Apply", without the confirmation they need.
        assert!(
            h.state().tab.draft().changes().is_empty(),
            "{text} must not be applied as the G21 setting"
        );
        assert!(
            h.query_by_label_contains("Keep the G21 command").is_some(),
            "{text}"
        );
        assert!(h.get_by_label("Apply").accesskit_node().is_disabled());
        assert!(
            h.query_by_label("1 raw line is not a valid setting and will not be applied.")
                .is_some()
        );
    }
    // Back to the reported command.
    h.get_by_label("Reset to G21").click();
    h.run_ok();
    assert_eq!(h.state().tab.pending_changes(), 0);
}

#[test]
fn reverting_a_staged_setting_drops_it() {
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 900.0),
        Host::<TuningTab>::synthetic(demo_state()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    // An imported setting the printer did not report, and an edit.
    h.state_mut().tab.stage(cmd("M302 S170"));
    h.state_mut().tab.stage(cmd("M92 X80 Y80 Z400 E95.4"));
    h.run_ok();
    assert_eq!(h.state().tab.pending_changes(), 2);
    h.get_by_label("Modified").click();
    h.run_ok();
    h.get_by_label("Revert Cold extrusion").click();
    h.run_ok();
    assert_eq!(
        h.state().tab.pending_changes(),
        1,
        "only the M92 edit is left"
    );
    assert!(h.state().tab.draft().entry("M302").is_none());
    assert!(h.query_by_label_contains("Cold extrusion (M302)").is_none());
}

#[test]
fn firmware_without_eeprom_offers_no_saving() {
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 900.0),
        Host::<TuningTab>::synthetic(without_eeprom(demo_state())),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    // M500 and M501 do nothing there: no "saved" toast that is not true.
    for label in ["Save to EEPROM", "Load from EEPROM"] {
        assert!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            "{label}"
        );
    }
    assert!(
        !h.get_by_label("Factory reset")
            .accesskit_node()
            .is_disabled()
    );
    assert!(h.query_by_label("No EEPROM").is_some());
    // Firmware with EEPROM keeps the buttons.
    h.state_mut().state = Some(demo_state());
    h.run_ok();
    assert!(
        !h.get_by_label("Save to EEPROM")
            .accesskit_node()
            .is_disabled()
    );
    assert!(h.query_by_label("No EEPROM").is_none());
}

#[test]
fn a_category_that_disappears_shows_everything_again() {
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 900.0),
        Host::<TuningTab>::synthetic(demo_state()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    h.get_by_label("Drivers").click();
    h.run_ok();
    assert!(h.query_by_label_contains("Steps per unit (M92)").is_none());
    // A new report without any driver settings (another board, a reset).
    if let Some(s) = h.state_mut().state.as_mut() {
        for key in [
            "M906", "M906 T0", "M913", "M913 T0", "M569", "M914", "M569 T0",
        ] {
            s.settings.remove(key);
        }
    }
    h.run_ok();
    assert!(h.query_by_label("Drivers").is_none());
    assert!(h.query_by_label("Nothing matches").is_none());
    assert!(h.query_by_label_contains("Steps per unit (M92)").is_some());
}

#[test]
fn tuning_tab_warns_during_a_job() {
    let mut state = demo_state();
    let mut job =
        sprint_host::JobInfo::new(sprint_host::JobKind::HostPrint, "cube.gcode", 100, 1000);
    job.status = sprint_host::JobStatus::Printing;
    state.job = Some(job);
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 700.0),
        Host::<TuningTab>::synthetic(state),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    assert!(h.query_by_label("A job is running").is_some());
}

#[test]
fn tuning_tab_tolerates_odd_settings() {
    let mut state = demo_state();
    for line in [
        "M92 X-5 Y1e30",
        "M301 E9 P",
        "M593 X",
        "M999 Q1",
        "M205 J0.5 X100000",
    ] {
        if let Some(c) = SettingCommand::parse(line) {
            state.settings.insert(c);
        }
    }
    let mut h = harness_state(
        Theme::Dark,
        (300.0, 600.0),
        Host::<TuningTab>::synthetic(state),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
}

#[test]
fn tuning_tab_loads_applies_and_saves_on_the_simulator() {
    let printer = sim_printer(fast_sim());
    // A state that has not loaded the settings yet: the tab asks for them.
    let mut fresh = printer.snapshot();
    fresh.settings_loaded = false;
    fresh.settings = Default::default();
    clear_received(&printer);
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 900.0),
        Host::<TuningTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    h.state_mut().state = Some(fresh);
    h.run_ok();
    wait_for_command(&printer, "M503");
    // From now on the tab follows the live printer.
    h.state_mut().state = None;
    h.run_ok();
    assert!(h.query_by_label_contains("Steps per unit (M92)").is_some());

    // Apply an edit.
    clear_received(&printer);
    h.state_mut().tab.stage(cmd("M92 X80 Y80 Z400 E95.4"));
    h.run_ok();
    h.get_by_label("Apply change").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M92 "), "M92 X80 Y80 Z400 E95.4");
    wait_for_command(&printer, "M503");
    run_until(&mut h, "the applied steps", |s| {
        s.settings.value("M92", 'E') == Some(95.4)
    });
    assert_eq!(h.state().tab.pending_changes(), 0);

    // Save to EEPROM, after confirming.
    clear_received(&printer);
    h.get_by_label("Save to EEPROM").click();
    h.run_ok();
    assert!(
        !received(&printer, "M500"),
        "nothing is sent before confirming"
    );
    h.get_all_by_label("Save to EEPROM")
        .last()
        .expect("confirm button")
        .click();
    h.run_ok();
    wait_for_command(&printer, "M500");

    // Load from EEPROM.
    clear_received(&printer);
    h.get_by_label("Load from EEPROM").click();
    h.run_ok();
    h.get_all_by_label("Load from EEPROM")
        .last()
        .expect("confirm button")
        .click();
    h.run_ok();
    wait_for_command(&printer, "M501");

    // Factory reset asks first, and Cancel sends nothing.
    clear_received(&printer);
    h.get_by_label("Factory reset").click();
    h.run_ok();
    h.get_by_label("Cancel").click();
    h.run_ok();
    assert!(!received(&printer, "M502"));
    h.get_by_label("Factory reset").click();
    h.run_ok();
    h.get_by_label("Reset to defaults").click();
    h.run_ok();
    wait_for_command(&printer, "M502");
    run_until(&mut h, "the defaults", |s| {
        s.settings.value("M92", 'E') == Some(93.0)
    });
    printer.disconnect();
}

#[test]
fn tuning_tab_imports_with_a_preview() {
    let printer = sim_printer(fast_sim());
    let mut h = harness_state(
        Theme::Light,
        (1180.0, 900.0),
        Host::<TuningTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    h.state_mut().tab.import_text(
        "backup.gcode",
        "; my backup\nM92 E415\nG28\nM301 P22.20 I1.08 D114.00\nM500\n",
    );
    h.run_ok();
    assert!(h.query_by_label("Import settings").is_some());
    assert!(h.query_by_label_contains("backup.gcode").is_some());
    snapshot(&mut h, "insight_tuning_import_light");
    h.get_by_label("Stage 1 change").click();
    h.run_ok();
    assert_eq!(h.state().tab.pending_changes(), 1);
    assert!(h.query_by_label("Import settings").is_none());
    clear_received(&printer);
    h.get_by_label("Apply change").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M92 "), "M92 X80 Y80 Z400 E415");
    // Copy as G-code confirms with a toast.
    h.get_by_label("Copy all as G-code").click();
    h.run_ok();
    assert!(
        h.state()
            .toasts
            .iter()
            .any(|t| t.title() == "Settings copied")
    );
    // The export text carries the firmware and every setting.
    run_until(&mut h, "the applied steps", |s| {
        s.settings.value("M92", 'E') == Some(415.0)
    });
    let state = printer.snapshot();
    let mut config = sprint_host::AppConfig::default();
    let mut dirty = false;
    let mut toasts = sprint_gui::widgets::Toasts::default();
    let mut actions = Vec::new();
    let ctx = sprint_gui::context::AppCtx {
        printer: &printer,
        state: &state,
        config: &mut config,
        config_dirty: &mut dirty,
        file: None,
        toasts: &mut toasts,
        actions: &mut actions,
        selected_line: None,
    };
    let text = h.state().tab.export_text(&ctx);
    assert!(text.contains("; Firmware: Marlin 2.1.2.1 (Sprint Virtual)"));
    assert!(text.contains("\nM92 X80 Y80 Z400 E415\n"), "{text}");
    printer.disconnect();
}

#[test]
fn switching_a_driver_off_reaches_the_simulator() {
    let printer = sim_printer(fast_sim());
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 900.0),
        Host::<TuningTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    h.get_by_label("Drivers").click();
    h.run_ok();
    // The simulator runs every driver in StealthChop: "M569 S1 X Y Z".
    // The first M569 instance (the second is the extruder's, "T0 E").
    h.get_all_by_role_and_label(egui::accesskit::Role::CheckBox, "Z driver")
        .next()
        .expect("Z driver switch")
        .click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Apply change").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M569 S1"), "M569 S1 X Y");
    // Leaving Z out of the list does not change it: it is switched off.
    assert_eq!(wait_for_command(&printer, "M569 S0"), "M569 S0 Z");
    printer.disconnect();
}
