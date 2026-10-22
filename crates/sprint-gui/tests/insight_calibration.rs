//! The Calibration tab: every wizard in snapshots, and the commands each
//! one sends to the simulator.

mod common;
mod insight_common;

use common::{harness_state, snapshot};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use insight_common::*;
use sprint_gui::panels::CalibrationTab;
use sprint_gui::theme::Theme;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{AutotuneState, Heater, PrinterState, TempSample};

type H<'a> = Harness<'a, Host<CalibrationTab>>;

fn tab(
    theme: Theme,
    size: (f32, f32),
    state: Option<PrinterState>,
    printer: Option<sprint_host::Printer>,
) -> H<'static> {
    let host = match (state, printer) {
        (Some(state), None) => Host::<CalibrationTab>::synthetic(state),
        (state, Some(printer)) => {
            let mut host = Host::<CalibrationTab>::live(printer);
            host.state = state;
            host
        }
        (None, None) => Host::<CalibrationTab>::synthetic(PrinterState::default()),
    };
    let mut h = harness_state(theme, size, host, |ui, host| host.frame(ui));
    h.run_ok();
    h
}

/// Type `text` into the `index`-th number field and confirm with Enter.
fn type_number(h: &mut H<'_>, index: usize, text: &str) {
    h.get_all_by_role(Role::SpinButton)
        .nth(index)
        .expect("number field")
        .click();
    h.run_ok();
    // Replace whatever the field showed.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_ok();
    h.key_press(egui::Key::Backspace);
    h.run_ok();
    h.get_all_by_role(Role::SpinButton)
        .nth(index)
        .expect("number field")
        .type_text(text);
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
}

fn pointer_away(h: &mut H<'_>) {
    h.event(egui::Event::PointerGone);
    h.run_ok();
}

/// A state with an autotune of the hotend in progress: 150 s of swings
/// around 210 °C.
fn autotune_running() -> PrinterState {
    let mut state = demo_state();
    let start = state.temp_history.back().map_or(0.0, |s| s.t);
    for i in 1..=150 {
        let t = start + f64::from(i);
        let x = i as f32;
        let temp = if i < 40 {
            30.0 + 180.0 * (x / 40.0)
        } else {
            210.0 + 4.0 * ((x - 40.0) / 22.0 * std::f32::consts::TAU).sin()
        };
        state.temp_history.push_back(TempSample {
            t,
            hotends: vec![(temp, 210.0)],
            bed: Some((60.0, 60.0)),
            hotend_power: vec![if temp < 210.0 { 1.0 } else { 0.0 }],
            bed_power: Some(0.35),
            ..TempSample::default()
        });
    }
    state.autotune = Some(AutotuneState {
        heater: Heater::Hotend(0),
        target: 210.0,
        cycles: 8,
        running: true,
        result: None,
        error: None,
    });
    state.temps.hotends[0].current = 212.4;
    state
}

#[test]
fn pid_wizard_snapshots() {
    // Running: progress, estimated cycles and the temperature trace.
    let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(autotune_running()), None);
    // The trace starts when the wizard started the autotune.
    if let Some(s) = h.state_mut().state.as_mut() {
        s.temp_history.retain(|x| x.t > 240.0);
    }
    h.run_ok();
    assert!(h.query_by_label("Abort").is_some());
    snapshot(&mut h, "insight_calibration_pid_running_dark");

    // Finished: the new gains against the old ones.
    let mut done = demo_state();
    done.autotune = Some(AutotuneState {
        heater: Heater::Hotend(0),
        target: 210.0,
        cycles: 8,
        running: false,
        result: Some((21.73, 1.54, 76.9)),
        error: None,
    });
    let mut h = tab(Theme::Light, (1100.0, 900.0), Some(done), None);
    assert!(h.query_by_label("Apply gains").is_some());
    snapshot(&mut h, "insight_calibration_pid_result_light");
}

#[test]
fn pid_wizard_shows_failures() {
    let mut failed = demo_state();
    failed.autotune = Some(AutotuneState {
        heater: Heater::Bed,
        target: 60.0,
        cycles: 8,
        running: false,
        result: None,
        error: Some("PID Autotune failed! timeout".into()),
    });
    let h = tab(Theme::Dark, (1100.0, 700.0), Some(failed), None);
    assert!(
        h.query_by_label_contains("Autotune failed: PID Autotune failed! timeout")
            .is_some()
    );
}

#[test]
fn esteps_wizard_walks_through_the_steps() {
    let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(demo_state()), None);
    h.get_by_label("Extruder E-steps").click();
    h.run_ok();
    // The demo hotend sits at its target: ready.
    h.get_by_label("Next").click();
    h.run_ok();
    h.get_by_label("Marked").click();
    h.run_ok();
    h.get_by_label("Extrude 100 mm").click();
    h.run_ok();
    h.get_by_label("Measure").click();
    h.run_ok();
    // 120 mm mark, 22 mm left: 98 mm went in, 93 -> 94.90 steps/mm.
    type_number(&mut h, 0, "22");
    assert!(h.query_by_label("Apply 94.90 steps/mm").is_some());
    pointer_away(&mut h);
    snapshot(&mut h, "insight_calibration_esteps_dark");
    // A silly measurement is explained, not applied.
    type_number(&mut h, 0, "90");
    assert!(h.query_by_label_contains("Apply").is_none());
    assert!(h.query_by_label("Cannot compute E-steps").is_some());
}

/// Replace a setting in the synthetic state, as the printer's report would.
fn report_setting(h: &mut H<'_>, line: &str) {
    if let Some(s) = h.state_mut().state.as_mut() {
        s.settings
            .insert(sprint_host::sprint_proto::SettingCommand::parse(line).expect("setting"));
    }
    h.run_ok();
}

#[test]
fn esteps_result_stays_put_after_applying() {
    let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(demo_state()), None);
    h.get_by_label("Extruder E-steps").click();
    h.run_ok();
    for step in ["Next", "Marked", "Extrude 100 mm", "Measure"] {
        h.get_by_label(step).click();
        h.run_ok();
    }
    type_number(&mut h, 0, "22");
    h.get_by_label("Apply 94.90 steps/mm").click();
    h.run_ok();
    // The printer now reports the new steps/mm. The measurement was made
    // with 93: the result must not be corrected a second time.
    report_setting(&mut h, "M92 X80 Y80 Z400 E94.90");
    assert!(h.query_by_label("New steps/mm: 94.90").is_some());
    assert!(h.query_by_label("Tested with: 93.00").is_some());
    assert!(
        h.query_by_label_contains("Apply").is_none(),
        "already applied"
    );
    assert!(
        !h.get_by_label("Save to EEPROM")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn esteps_measurement_starts_from_the_mark() {
    let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(demo_state()), None);
    h.get_by_label("Extruder E-steps").click();
    h.run_ok();
    h.get_by_label("Next").click();
    h.run_ok();
    // The filament is marked 150 mm up instead of 120.
    type_number(&mut h, 0, "150");
    for step in ["Marked", "Extrude 100 mm", "Measure"] {
        h.get_by_label(step).click();
        h.run_ok();
    }
    // Nothing measured yet: the field shows what a perfect extruder
    // leaves, not a 23% correction computed from a stale 20 mm.
    assert!(h.query_by_label("Extruded: 100.0 mm").is_some());
    assert!(h.query_by_label("Change: 0.0%").is_some());
    type_number(&mut h, 0, "52");
    assert!(h.query_by_label("Extruded: 98.0 mm").is_some());
}

#[test]
fn esteps_extrudes_once_per_mark() {
    let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(demo_state()), None);
    h.get_by_label("Extruder E-steps").click();
    h.run_ok();
    h.get_by_label("Next").click();
    h.run_ok();
    h.get_by_label("Marked").click();
    h.run_ok();
    let extrude = |h: &H<'_>| {
        h.get_by_label("Extrude 100 mm")
            .accesskit_node()
            .is_disabled()
    };
    assert!(!extrude(&h));
    h.get_by_label("Extrude 100 mm").click();
    h.run_ok();
    // A second click would extrude another 100 mm past the mark.
    assert!(extrude(&h), "one extrusion per mark");
    // Marking the filament again starts a new test.
    h.get_by_label("Back").click();
    h.run_ok();
    h.get_by_label("Marked").click();
    h.run_ok();
    assert!(!extrude(&h));
    assert!(h.get_by_label("Measure").accesskit_node().is_disabled());
}

#[test]
fn flow_result_stays_put_after_setting_it() {
    let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(demo_state()), None);
    h.get_by_label("Flow").click();
    h.run_ok();
    for (i, wall) in ["0.48", "0.47", "0.49", "0.48"].iter().enumerate() {
        type_number(&mut h, 2 + i, wall);
    }
    h.get_by_label("Set flow to 94 % (M221)").click();
    h.run_ok();
    // The printer reports the new flow; the walls were printed at 100%.
    if let Some(s) = h.state_mut().state.as_mut() {
        s.flow_pct = vec![94.0];
    }
    h.run_ok();
    assert!(h.query_by_label("New flow: 93.7 %").is_some());
    assert!(h.query_by_label("Set flow to 94 % (M221)").is_some());
}

#[test]
fn zoffset_and_flow_snapshots() {
    // Babystepped 0.05 mm closer since homing.
    let mut state = demo_state();
    state.babystep_z = -0.05;
    let mut h = tab(Theme::Light, (1100.0, 900.0), Some(state), None);
    h.get_by_label("Z offset").click();
    h.run_ok();
    assert!(
        h.query_by_label_contains("Probe Z offset: -1.520 → -1.570 mm")
            .is_some()
    );
    pointer_away(&mut h);
    snapshot(&mut h, "insight_calibration_zoffset_light");

    h.get_by_label("Flow").click();
    h.run_ok();
    for (i, wall) in ["0.48", "0.47", "0.49", "0.48"].iter().enumerate() {
        type_number(&mut h, 2 + i, wall);
    }
    assert!(h.query_by_label("Set flow to 94 % (M221)").is_some());
    pointer_away(&mut h);
    snapshot(&mut h, "insight_calibration_flow_light");
}

/// Fresh after connecting, whether Z is homed is not known: the wizard
/// says so before any babystep, with a way to home.
#[test]
fn zoffset_warns_while_z_is_not_known_to_be_homed() {
    let mut state = demo_state();
    state.homed = None;
    let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(state), None);
    h.get_by_label("Z offset").click();
    h.run_ok();
    assert!(h.query_by_label("Z is not known to be homed").is_some());
    assert!(h.query_by_label("Home all axes").is_some());
    pointer_away(&mut h);
    snapshot(&mut h, "insight_calibration_zoffset_unhomed_dark");
    // Homed X and Y only: Z is still in question.
    let mut state = demo_state();
    state.homed = Some(sprint_host::Axes::XY);
    let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(state), None);
    h.get_by_label("Z offset").click();
    h.run_ok();
    assert!(h.query_by_label("Z is not known to be homed").is_some());
}

#[test]
fn calibration_tab_narrow_and_disconnected() {
    let mut h = tab(
        Theme::Dark,
        (600.0, 800.0),
        Some(PrinterState::default()),
        None,
    );
    assert!(h.query_by_label("Not connected").is_some());
    // The wizard list becomes a segmented control.
    h.get_by_label("Flow").click();
    h.run_ok();
    assert!(h.query_by_label("Flow used").is_some());
    snapshot(&mut h, "insight_calibration_narrow_dark");
}

/// A printer homing on a Z endstop: no probe offset, a home offset.
fn without_probe_offset(state: &mut PrinterState) {
    state.settings.remove("M851");
}

#[test]
fn applying_the_home_offset_takes_the_babystep_back() {
    let printer = sim_printer(fast_sim());
    home(&printer);
    assert!(printer.with_state(|s| s.settings.value("M206", 'Z').is_some()));
    let mut host = Host::<CalibrationTab>::live(printer.clone());
    host.tweak = Some(without_probe_offset);
    let mut h = harness_state(Theme::Dark, (1100.0, 900.0), host, |ui, host| {
        host.frame(ui)
    });
    h.run_ok();
    h.get_by_label("Z offset").click();
    h.run_ok();
    clear_received(&printer);
    for _ in 0..2 {
        h.get_by_label("Lower the nozzle 0.025 mm").click();
        h.run_ok();
    }
    run_until(&mut h, "both babysteps", |s| {
        (s.babystep_z + 0.05).abs() < 1e-4
    });
    h.run_ok();
    assert!(
        h.query_by_label_contains("Home offset Z: 0.000 → 0.050 mm")
            .is_some()
    );
    h.get_by_label("Apply to home offset").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M206"), "M206 X0 Y0 Z0.05");
    // M206 moves the next moves 0.05 mm down at once; with the babystep
    // still active the nozzle would sit 0.1 mm closer until the next homing.
    let sim = printer.sim_control().expect("simulator");
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        let babysteps: Vec<String> = sim
            .received_commands()
            .into_iter()
            .filter(|c| c.starts_with("M290"))
            .collect();
        if babysteps.len() >= 3 {
            assert_eq!(babysteps, ["M290 Z-0.025", "M290 Z-0.025", "M290 Z0.05"]);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the babystep was not taken back: {babysteps:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        h.query_by_label_contains("applies to the next moves")
            .is_some()
    );
    // Taken back, the babystepping no longer counts: nothing more to apply.
    run_until(&mut h, "the take back", |s| s.babystep_z.abs() < 1e-4);
    h.run_ok();
    assert!(
        h.query_by_label_contains("Home offset Z: 0.050 → 0.050 mm")
            .is_some()
    );
    assert!(
        h.get_by_label("Apply to home offset")
            .accesskit_node()
            .is_disabled()
    );
    printer.disconnect();
}

/// The question before babystepping with Z not known to be homed goes
/// away when the printer homes while it is open, without the babystep.
#[test]
fn zoffset_question_goes_away_once_z_is_homed() {
    let printer = sim_printer(fast_sim());
    let mut h = tab(Theme::Dark, (1100.0, 900.0), None, Some(printer.clone()));
    h.get_by_label("Z offset").click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Lower the nozzle 0.025 mm").click();
    h.run_ok();
    assert!(h.query_by_label("Babystep without homing Z?").is_some());
    home(&printer);
    run_until(&mut h, "the homing", |s| {
        s.homed == Some(sprint_host::Axes::ALL)
    });
    h.run_ok();
    assert!(h.query_by_label("Babystep without homing Z?").is_none());
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!received(&printer, "M290"), "the question's babystep");
}

/// The wizard counts the printer's babystepping (from anywhere) since its
/// counter was reset; homing Z makes the printer forget it. Z not known to
/// be homed is said, and babystepping then asks first.
#[test]
fn zoffset_counts_the_printers_babystepping() {
    let printer = sim_printer(fast_sim());
    let mut h = tab(Theme::Dark, (1100.0, 900.0), None, Some(printer.clone()));
    h.get_by_label("Z offset").click();
    h.run_ok();
    // Not homed: a warning with a way to home, and a question first.
    assert!(h.query_by_label("Z is not known to be homed").is_some());
    clear_received(&printer);
    h.get_by_label("Lower the nozzle 0.025 mm").click();
    h.run_ok();
    assert!(h.query_by_label("Babystep without homing Z?").is_some());
    h.get_by_label("Cancel").click();
    h.run_ok();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!received(&printer, "M290"), "a cancelled babystep");
    h.get_by_label("Home all axes").click();
    h.run_ok();
    run_until(&mut h, "the homing", |s| {
        s.homed == Some(sprint_host::Axes::ALL)
    });
    h.run_ok();
    assert!(h.query_by_label("Z is not known to be homed").is_none());
    // Homed: babysteps go out at once, and one from the console counts too.
    h.get_by_label("Lower the nozzle 0.025 mm").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M290"), "M290 Z-0.025");
    printer.send("M290 Z-0.05");
    run_until(&mut h, "both babysteps", |s| {
        (s.babystep_z + 0.075).abs() < 1e-4
    });
    h.run_ok();
    // In the wizard and in its tile of the list.
    assert_eq!(h.query_all_by_label("\u{2212}0.075 mm").count(), 2);
    // The counter reset starts the count over without moving the nozzle.
    clear_received(&printer);
    h.get_by_label("Reset counter").click();
    h.run_ok();
    assert!(h.query_by_label("no adjustment yet").is_some());
    h.get_by_label("Raise the nozzle 0.025 mm").click();
    h.run_ok();
    run_until(&mut h, "the babystep", |s| {
        (s.babystep_z + 0.05).abs() < 1e-4
    });
    h.run_ok();
    assert_eq!(h.query_all_by_label("+0.025 mm").count(), 2);
    // Homing Z drops the babysteps on the printer: none count.
    printer.home(sprint_host::Axes::Z);
    run_until(&mut h, "the Z homing", |s| s.babystep_z == 0.0);
    h.run_ok();
    assert!(h.query_by_label("no adjustment yet").is_some());
    printer.disconnect();
}

#[test]
fn wizards_do_not_save_without_eeprom() {
    let save_disabled = |h: &H<'_>| {
        h.get_by_label("Save to EEPROM")
            .accesskit_node()
            .is_disabled()
    };
    for (state, disabled) in [(without_eeprom(demo_state()), true), (demo_state(), false)] {
        let mut h = tab(Theme::Dark, (1100.0, 900.0), Some(state.clone()), None);
        h.get_by_label("Z offset").click();
        h.run_ok();
        assert_eq!(save_disabled(&h), disabled, "Z offset");
        // A finished autotune whose gains are in use.
        let mut tuned = state;
        tuned.autotune = Some(AutotuneState {
            heater: Heater::Hotend(0),
            target: 210.0,
            cycles: 8,
            running: false,
            result: Some((22.2, 1.08, 114.0)),
            error: None,
        });
        let h = tab(Theme::Dark, (1100.0, 900.0), Some(tuned), None);
        assert!(h.query_by_label("ACTIVE").is_some());
        assert_eq!(save_disabled(&h), disabled, "PID");
    }
}

#[test]
fn every_wizard_fits_a_narrow_tab() {
    if !common::system_has_reference_fonts() {
        eprintln!("skipped: these widths are tuned to the reference fonts");
        return;
    }
    let width = 420.0;
    let mut h = tab(Theme::Dark, (width, 1100.0), Some(demo_state()), None);
    // The switcher takes the short titles rather than widening the tab.
    for (switch, wizard) in [
        ("PID", "Start autotune at 210 °C"),
        ("E-steps", "Heat to 205 °C"),
        ("Z offset", "Lower the nozzle 0.025 mm"),
        ("Flow", "Flow used"),
    ] {
        h.get_by_label(switch).click();
        h.run_ok();
        assert!(h.query_by_label_contains(wizard).is_some(), "{wizard}");
        assert_eq!(
            overflowing(&h, width - 24.0),
            Vec::<String>::new(),
            "{wizard}"
        );
    }
    // Home and the counter reset do not sit on the step sizes (shown
    // without their unit, which moved into the caption).
    h.get_by_label("Z offset").click();
    h.run_ok();
    assert!(h.query_by_label("Step (mm)").is_some());
    let controls = ["0.01", "0.025", "0.05", "0.1", "Home", "Reset counter"];
    for label in controls {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    assert_eq!(overlapping(&h, &controls), Vec::<String>::new());
    pointer_away(&mut h);
    snapshot(&mut h, "insight_calibration_zoffset_narrow_dark");
    // Narrower still, a drop-down picks the wizard.
    let mut h = tab(Theme::Dark, (300.0, 900.0), Some(demo_state()), None);
    assert!(h.query_by_label("PID").is_none());
    assert_eq!(overflowing(&h, 300.0 - 24.0), Vec::<String>::new());
    h.get_by_role(Role::ComboBox).click();
    h.run_ok();
    h.get_by_label("Flow").click();
    h.run_ok();
    assert_eq!(
        h.state().tab.wizard(),
        sprint_gui::panels::calibration::Wizard::Flow
    );
}

#[test]
fn pid_autotune_runs_on_the_simulator() {
    let printer = sim_printer(SimConfig {
        time_scale: 400.0,
        ..fast_sim()
    });
    let mut h = tab(Theme::Dark, (1100.0, 900.0), None, Some(printer.clone()));
    clear_received(&printer);
    h.get_by_label("Start autotune at 210 °C").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M303"), "M303 E0 S210 C8 U1");
    run_until(&mut h, "the autotune result", |s| {
        s.autotune
            .as_ref()
            .is_some_and(|a| !a.running && (a.result.is_some() || a.error.is_some()))
    });
    run_until(&mut h, "the refreshed settings", |s| {
        let (Some(a), Some(pid)) = (s.autotune.as_ref(), s.settings.hotend_pid(0)) else {
            return false;
        };
        a.result.is_some_and(|r| (r.0 - pid.0).abs() < 0.01)
    });
    h.run_ok();
    // M303 U1 already made the firmware use the gains: save them.
    assert!(h.query_by_label("ACTIVE").is_some());
    clear_received(&printer);
    h.get_by_label("Save to EEPROM").click();
    h.run_ok();
    wait_for_command(&printer, "M500");
    printer.disconnect();
}

#[test]
fn esteps_zoffset_and_flow_reach_the_simulator() {
    let printer = sim_printer(SimConfig {
        time_scale: 200.0,
        ..fast_sim()
    });
    let mut h = tab(Theme::Dark, (1100.0, 900.0), None, Some(printer.clone()));
    h.get_by_label("Extruder E-steps").click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Heat to 205 °C").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M104"), "M104 T0 S205");
    run_until(&mut h, "the hotend at temperature", |s| {
        s.temps
            .hotends
            .first()
            .is_some_and(|r| (r.current - 205.0).abs() <= 2.0)
    });
    // The simulated hotend swings around its target: "Next" is enabled
    // only while it is within the tolerance.
    let deadline = std::time::Instant::now() + PATIENCE;
    while h.query_by_label("Marked").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the heat step never let go"
        );
        let next = h.get_by_label("Next");
        if !next.accesskit_node().is_disabled() {
            next.click();
        }
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.get_by_label("Marked").click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Extrude 100 mm").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "G1 E"), "G1 E100 F50");
    h.get_by_label("Measure").click();
    h.run_ok();
    type_number(&mut h, 0, "22");
    clear_received(&printer);
    h.get_by_label("Apply 94.90 steps/mm").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M92"), "M92 X80 Y80 Z400 E94.9");

    // Z offset: babystep and store it in the probe offset.
    home(&printer);
    h.get_by_label("Z offset").click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Lower the nozzle 0.025 mm").click();
    h.run_ok();
    h.get_by_label("Lower the nozzle 0.025 mm").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M290"), "M290 Z-0.025");
    run_until(&mut h, "both babysteps", |s| {
        (s.babystep_z + 0.05).abs() < 1e-4
    });
    h.run_ok();
    h.get_by_label("Apply to probe offset").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M851"), "M851 X10 Y10 Z-0.05");
    run_until(&mut h, "the new probe offset", |s| {
        s.settings.value("M851", 'Z') == Some(-0.05)
    });

    // Flow: M221 with the corrected percentage.
    h.get_by_label("Flow").click();
    h.run_ok();
    for (i, wall) in ["0.48", "0.48"].iter().enumerate() {
        type_number(&mut h, 2 + i, wall);
    }
    clear_received(&printer);
    h.get_by_label("Set flow to 94 % (M221)").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M221"), "M221 T0 S94");
    printer.disconnect();
}
