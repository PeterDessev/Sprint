//! The whole app driven headless against the built-in virtual printer:
//! connecting, loading files, printing (pause, resume, cancel, completion),
//! the console, dialogs, shortcuts, the halted banner and host prompts.
//! No serial port is ever opened: every profile here is the simulator.

mod app_support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use app_support::{WINDOW, app, expect, fast_sim, sample, settle, sim_options};
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sprint_gui::app::{AppOptions, DialogKind, SprintApp};
use sprint_gui::context::{AppAction, Tab};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{ConnectTarget, ConnectionStatus, JobInfo, JobKind, JobStatus, PrinterState};

/// Options with the fast simulator, slowed down to `time_scale` with real
/// move times (so a print lasts long enough to pause it), and one command
/// in flight as with real printers: a cancel script then follows the
/// command the printer works on (with more in flight, a heat-up and a
/// probing already sent would be carried out first, taking minutes).
fn slow_sim_options(time_scale: f32) -> AppOptions {
    let mut options = sim_options();
    options.config.profiles[0].connection = ConnectTarget::Simulator(SimConfig {
        instant_moves: false,
        time_scale,
        ..fast_sim()
    });
    options.config.profiles[0].options.max_in_flight = 1;
    options
}

fn with_file(mut options: AppOptions, path: PathBuf) -> AppOptions {
    options.file = Some(path);
    options
}

fn analyzed(app: &SprintApp) -> bool {
    app.file().is_some_and(|f| f.toolpath.is_some()) && !app.is_loading()
}

fn job_status(app: &SprintApp) -> Option<JobStatus> {
    app.state().job.as_ref().map(|j| j.status.clone())
}

fn connect(h: &mut Harness<'_, SprintApp>) {
    h.get_by_label("Connect").click();
    expect(h, "the virtual printer to connect", |app| {
        app.state().is_connected()
    });
}

fn toast_titles(app: &SprintApp) -> Vec<String> {
    app.toasts().iter().map(|t| t.title().to_owned()).collect()
}

fn received(app: &SprintApp) -> Vec<String> {
    app.printer()
        .sim_control()
        .map(|c| c.received_commands())
        .unwrap_or_default()
}

/// A private temporary directory.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sprint-gui-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn connects_prints_pauses_resumes_and_cancels() {
    let options = with_file(slow_sim_options(30.0), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    connect(&mut h);
    assert!(h.query_by_label("Disconnect").is_some());

    // Ctrl+P opens the start-print dialog; its button starts the print.
    h.key_press_modifiers(Modifiers::COMMAND, Key::P);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::StartPrint));
    h.get_by_role_and_label(Role::Button, "Start print").click();
    expect(&mut h, "the print to run", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.current_line > 10)
    });
    assert_eq!(h.state().dialog(), None);
    assert!(toast_titles(h.state()).contains(&"Print started".to_owned()));

    // Space pauses and resumes (no text field has focus).
    h.key_press(Key::Space);
    expect(&mut h, "the print to pause", |app| {
        job_status(app) == Some(JobStatus::Paused)
    });
    h.key_press(Key::Space);
    expect(&mut h, "the print to resume", |app| {
        job_status(app) == Some(JobStatus::Printing)
    });

    // Cancel asks first.
    h.get_by_label("Cancel the job…").click();
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::ConfirmCancel));
    h.get_by_label("Cancel print").click();
    expect(&mut h, "the print to be cancelled", |app| {
        job_status(app) == Some(JobStatus::Cancelled)
    });
    expect(&mut h, "the cancel toast", |app| {
        toast_titles(app).contains(&"Print cancelled".to_owned())
    });
    // One toast for one cancel.
    let titles = toast_titles(h.state());
    assert!(
        !titles.iter().any(|t| t.starts_with("Cancelling")),
        "{titles:?}"
    );
    // Commands of the cancel script reached the printer.
    assert!(received(h.state()).iter().any(|c| c.starts_with("M104 S0")));
}

#[test]
fn a_print_runs_to_completion() {
    let options = with_file(sim_options(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    connect(&mut h);
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    let dialog = h.state_mut().start_print_dialog_mut().expect("dialog");
    dialog.set_emit_progress(true);
    h.get_by_role_and_label(Role::Button, "Start print").click();
    expect(&mut h, "the print to finish", |app| {
        job_status(app) == Some(JobStatus::Completed)
    });
    expect(&mut h, "the completion toast", |app| {
        toast_titles(app).contains(&"Print finished".to_owned())
    });
    let commands = received(h.state());
    assert!(commands.iter().any(|c| c.starts_with("M73 P")), "M73 sent");
    let file = h.state().file().expect("file").file.clone();
    let last = (0..file.len())
        .rev()
        .find_map(|i| file.command(i))
        .expect("a command");
    assert!(commands.iter().any(|c| *c == last), "the last line arrived");
}

#[test]
fn starting_from_a_layer_sends_the_preamble() {
    let options = with_file(sim_options(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    connect(&mut h);
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    let dialog = h.state_mut().start_print_dialog_mut().expect("dialog");
    dialog.set_mode(sprint_gui::dialogs::StartMode::Layer);
    dialog.set_layer(50);
    settle(&mut h);
    let preamble = h
        .state_mut()
        .start_print_dialog_mut()
        .expect("dialog")
        .preamble()
        .to_owned();
    assert!(preamble.contains("G28 X Y"), "{preamble}");
    h.get_by_role_and_label(Role::Button, "Start print").click();
    expect(&mut h, "the print to finish", |app| {
        job_status(app) == Some(JobStatus::Completed)
    });
    let commands = received(h.state());
    assert!(commands.iter().any(|c| c == "G28 X Y"), "preamble sent");
}

#[test]
fn the_console_sends_and_remembers_commands() {
    let mut h = app(sim_options(), WINDOW);
    connect(&mut h);
    h.state_mut().console_mut().set_input("M115");
    h.state_mut().console_mut().focus_input();
    settle(&mut h);
    h.key_press(Key::Enter);
    expect(&mut h, "M115 to reach the printer", |app| {
        received(app).iter().any(|c| c == "M115")
    });
    assert_eq!(
        h.state().console().history().last().map(String::as_str),
        Some("M115")
    );
    assert_eq!(h.state().console().input(), "");
    expect(&mut h, "the reply in the console", |app| {
        app.console()
            .visible_entries()
            .any(|e| e.text.contains("FIRMWARE_NAME"))
    });

    // A quick command chip.
    h.get_by_label("M114").click();
    expect(&mut h, "M114 to reach the printer", |app| {
        received(app).iter().any(|c| c == "M114")
    });

    // Dangerous commands ask first.
    h.state_mut().console_mut().set_input("M502");
    h.state_mut().console_mut().focus_input();
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(h.state().console().is_confirming());
    assert!(!received(h.state()).iter().any(|c| c == "M502"));
    h.get_by_label("Send anyway").click();
    expect(&mut h, "M502 after confirming", |app| {
        received(app).iter().any(|c| c == "M502")
    });
}

#[test]
fn sending_a_dangerous_line_from_the_gcode_asks_first() {
    let dir = temp_dir("dangerous-line");
    let path = dir.join("end.gcode");
    std::fs::write(&path, "G28\nM502 ; factory reset\nM105\n").expect("write");
    let mut h = app(with_file(sim_options(), path), WINDOW);
    expect(&mut h, "the file to load", analyzed);
    connect(&mut h);
    h.state_mut().push_action(AppAction::ShowTab(Tab::Gcode));
    settle(&mut h);
    // A harmless line goes straight out.
    h.get_by_label("Line 3: M105").click_secondary();
    settle(&mut h);
    h.get_by_label_contains("Send line to printer").click();
    expect(&mut h, "M105 to reach the printer", |app| {
        received(app).iter().any(|c| c == "M105")
    });
    // A dangerous one asks first.
    h.get_by_label("Line 2: M502 ; factory reset")
        .click_secondary();
    settle(&mut h);
    h.get_by_label_contains("Send line to printer").click();
    settle(&mut h);
    assert!(h.state().gcode_tab().is_confirming());
    assert!(!received(h.state()).iter().any(|c| c == "M502"));
    h.get_by_label("Send anyway").click();
    expect(&mut h, "M502 after confirming", |app| {
        received(app).iter().any(|c| c == "M502")
    });
    assert!(!h.state().gcode_tab().is_confirming());
    let _ = std::fs::remove_dir_all(dir);
}

/// A dropped file for the drag-and-drop test.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

#[test]
fn files_open_by_action_shortcut_and_drop() {
    let mut h = app(AppOptions::headless(), WINDOW);
    // Ctrl+O asks for the (here disabled) native dialog.
    h.key_press_modifiers(Modifiers::COMMAND, Key::O);
    settle(&mut h);
    assert_eq!(h.state().file_dialog_requests(), 1);

    // A missing file is reported, nothing is loaded.
    h.state_mut().push_action(AppAction::LoadFile(PathBuf::from(
        "Z:/missing/nothing.gcode",
    )));
    expect(&mut h, "the error toast", |app| {
        toast_titles(app)
            .iter()
            .any(|t| t.starts_with("Could not open"))
    });
    assert!(h.state().file().is_none());

    // Dropping a file loads it and remembers it.
    let cube = sample("calibration-cube.gcode");
    h.input_mut()
        .dropped_files
        .push(Arc::new(Dropped(cube.clone())));
    expect(&mut h, "the dropped file to load", analyzed);
    assert_eq!(h.state().config().recent_files.first(), Some(&cube));
    assert!(
        h.state()
            .file()
            .is_some_and(|f| f.file.name() == "calibration-cube.gcode")
    );

    // Another file replaces it; the recent list keeps both, newest first.
    let vase = sample("twisted-vase.gcode");
    h.state_mut().push_action(AppAction::LoadFile(vase.clone()));
    expect(&mut h, "the vase to load", |app| {
        analyzed(app)
            && app
                .file()
                .is_some_and(|f| f.file.name() == "twisted-vase.gcode")
    });
    assert_eq!(h.state().config().recent_files[..2], [vase, cube]);

    // Closing unloads it.
    h.state_mut().push_action(AppAction::CloseFile);
    settle(&mut h);
    assert!(h.state().file().is_none());
}

#[test]
fn a_changed_file_is_offered_for_reload() {
    let dir = temp_dir("reload");
    let path = dir.join("part.gcode");
    std::fs::write(&path, "G28\nG1 Z0.2 F600\nG1 X10 Y0 E1 F1200\n").expect("write");
    let mut h = app(with_file(AppOptions::headless(), path.clone()), WINDOW);
    expect(&mut h, "the part to load", analyzed);
    assert_eq!(h.state().file().map(|f| f.file.len()), Some(3));
    h.state_mut().push_action(AppAction::SelectLine(1));
    settle(&mut h);

    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(
        &path,
        "G28\nG1 Z0.2 F600\nG1 X10 Y0 E1 F1200\nG1 X10 Y10 E2\n",
    )
    .expect("rewrite");
    expect(
        &mut h,
        "the change to be noticed",
        SprintApp::file_changed_on_disk,
    );
    settle(&mut h);
    assert!(h.query_by_label("Keep this version").is_some());
    h.get_by_label("Reload").click();
    expect(&mut h, "the new version to load", |app| {
        analyzed(app) && app.file().is_some_and(|f| f.file.len() == 4)
    });
    // A reload keeps the selection.
    assert_eq!(h.state().selected_line(), Some(1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reload_that_lands_during_its_print_waits() {
    let dir = temp_dir("reload-print");
    let path = dir.join("part.gcode");
    std::fs::write(&path, "G28\nG1 Z0.2 F600\nG1 X10 Y0 E1 F1200\n").expect("write");
    let mut h = app(with_file(AppOptions::headless(), path.clone()), WINDOW);
    expect(&mut h, "the part to load", analyzed);
    let printed = h.state().file().expect("file").file.clone();
    std::fs::write(
        &path,
        "G28\nG1 Z0.2 F600\nG1 X10 Y0 E1 F1200\nG1 X10 Y10 E2\n",
    )
    .expect("rewrite");
    // F5, then the print of the file starts before the new version is in.
    h.state_mut().push_action(AppAction::ReloadFile);
    h.step();
    let mut state = printing_state(JobStatus::Printing);
    if let Some(job) = &mut state.job {
        job.name = "part.gcode".into();
        job.total_lines = 3;
        job.current_line = 1;
    }
    h.state_mut().set_state_override(Some(state));
    expect(&mut h, "the reload to end", |app| !app.is_loading());
    settle(&mut h);
    let shown = h.state().file().expect("file").file.clone();
    assert!(Arc::ptr_eq(&shown, &printed), "the printed version stays");
    assert!(toast_titles(h.state()).contains(&"The file is printing".to_owned()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn shortcuts_switch_tabs_and_open_dialogs() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);

    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    settle(&mut h);
    assert_eq!(h.state().tab(), Tab::Gcode);
    assert!(h.state().gcode_tab().find().open);

    // Ctrl+, ("Settings…") opens the preferences, "Printer profiles…" the
    // active profile.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::Settings));
    let page =
        |h: &mut Harness<'_, SprintApp>| h.state_mut().settings_dialog_mut().map(|d| d.page());
    assert_eq!(page(&mut h), Some(sprint_gui::settings::Page::Preferences));
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    assert_eq!(page(&mut h), Some(sprint_gui::settings::Page::Profile(0)));
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);

    // Ctrl+P without a printer explains instead of opening the dialog.
    h.key_press_modifiers(Modifiers::COMMAND, Key::P);
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    assert!(toast_titles(h.state()).contains(&"Cannot print right now".to_owned()));

    for (action, kind) in [
        (AppAction::OpenShortcuts, DialogKind::Shortcuts),
        (AppAction::OpenAbout, DialogKind::About),
    ] {
        h.state_mut().push_action(action);
        settle(&mut h);
        assert_eq!(h.state().dialog(), Some(kind));
        h.key_press(Key::Escape);
        settle(&mut h);
        assert_eq!(h.state().dialog(), None);
    }

    // Tabs by clicking their titles.
    h.get_by_label("Temperature").click();
    settle(&mut h);
    assert_eq!(h.state().tab(), Tab::Temperature);
    h.get_by_label("3D view").click();
    settle(&mut h);
    assert_eq!(h.state().tab(), Tab::View3D);

    // The window title names the file.
    assert_eq!(h.state().window_title(), "Sprint — calibration-cube.gcode");
}

#[test]
fn escape_closes_find_and_go_to_line() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    // Typing in the find field, then Escape.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    settle(&mut h);
    h.event(egui::Event::Text("G1 X1".into()));
    settle(&mut h);
    assert_eq!(h.state().gcode_tab().find().query, "G1 X1");
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(
        !h.state().gcode_tab().find().open,
        "Escape closes the find bar"
    );
    assert!(h.state().gcode_tab().find().query.is_empty());
    // The same for "go to line".
    h.key_press_modifiers(Modifiers::COMMAND, Key::G);
    settle(&mut h);
    assert!(h.state().gcode_tab().is_goto_open());
    h.event(egui::Event::Text("12".into()));
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!h.state().gcode_tab().is_goto_open());
}

#[test]
fn selecting_a_line_syncs_the_views() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    settle(&mut h);
    let toolpath = h
        .state()
        .file()
        .and_then(|f| f.toolpath.clone())
        .expect("toolpath");
    // Show only the first layers, then select a line far above them from
    // the G-code tab: the 3D view widens its range to show it.
    h.state_mut().viewer_tab_mut().set_layer_range(0, 2);
    h.state_mut().push_action(AppAction::ShowTab(Tab::Gcode));
    settle(&mut h);
    let line = toolpath.layers[60].lines.start + 3;
    h.state_mut().push_action(AppAction::SelectLine(line));
    settle(&mut h);
    assert_eq!(h.state().selected_line(), Some(line));
    if let Some((lo, hi)) = h.state().viewer_tab().layer_range() {
        assert_eq!((lo, hi), (0, 60), "the line's layer is on top");
    }
    // Out-of-range selections are clamped to the file.
    h.state_mut().push_action(AppAction::SelectLine(u32::MAX));
    settle(&mut h);
    let len = h.state().file().expect("file").file.len() as u32;
    assert_eq!(h.state().selected_line(), Some(len - 1));
}

#[test]
fn settings_apply_to_the_profile_and_are_saved() {
    let dir = temp_dir("settings");
    let config_path = dir.join("config.toml");
    let options = AppOptions {
        config_path: Some(config_path.clone()),
        ..sim_options()
    };
    let mut h = app(options, WINDOW);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    let dialog = h.state_mut().settings_dialog_mut().expect("settings");
    dialog.draft.profiles[0].bed_size = [300.0, 280.0];
    dialog.draft.console_max_lines = 2000;
    settle(&mut h);
    h.get_by_label("Save").click();
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    assert_eq!(h.state().config().active().bed_size, [300.0, 280.0]);
    assert_eq!(h.state().printer().profile().bed_size, [300.0, 280.0]);
    assert!(toast_titles(h.state()).contains(&"Settings saved".to_owned()));
    expect(&mut h, "the configuration file", |_| config_path.exists());
    let saved = sprint_host::AppConfig::load_from(&config_path).expect("saved config");
    assert_eq!(saved.active().bed_size, [300.0, 280.0]);
    assert_eq!(saved.console_max_lines, 2000);
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn saving_keeps_what_others_changed_in_the_file() {
    let dir = temp_dir("shared-config");
    let config_path = dir.join("config.toml");
    let options = sim_options();
    options.config.save_to(&config_path).expect("initial file");
    let options = AppOptions {
        config_path: Some(config_path.clone()),
        ..options
    };
    let mut h = app(options, WINDOW);
    // While the GUI runs, the TUI opens a file and a hand edit adds a
    // macro.
    let mut theirs = sprint_host::AppConfig::load_from(&config_path).expect("file");
    theirs.push_recent(PathBuf::from("from-the-tui.gcode"));
    theirs.macros.push(sprint_host::Macro {
        name: "Added by hand".into(),
        gcode: "M117 hi".into(),
        color: None,
        confirm: false,
    });
    theirs.save_to(&config_path).expect("their save");
    // The GUI opens a file, which it saves to the recent files.
    let cube = sample("calibration-cube.gcode");
    h.state_mut().push_action(AppAction::LoadFile(cube.clone()));
    expect(&mut h, "the cube to be analyzed", analyzed);
    let saved_recent = |path: &Path| {
        sprint_host::AppConfig::load_from(path).is_ok_and(|c| {
            c.recent_files
                .first()
                .is_some_and(|f| f.ends_with("calibration-cube.gcode"))
        })
    };
    expect(&mut h, "the save", |_| saved_recent(&config_path));
    let saved = sprint_host::AppConfig::load_from(&config_path).expect("saved config");
    assert_eq!(saved.recent_files[1], PathBuf::from("from-the-tui.gcode"));
    assert!(
        saved.macros.iter().any(|m| m.name == "Added by hand"),
        "{:?}",
        saved.macros
    );
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_estop_halts_and_the_banner_recovers() {
    let mut h = app(sim_options(), WINDOW);
    connect(&mut h);
    h.get_by_label("E-STOP").click();
    expect(&mut h, "the firmware to halt", |app| app.state().halted);
    settle(&mut h);
    assert!(h.query_by_label("Printer halted").is_some());
    assert!(received(h.state()).iter().any(|c| c == "M112"));
    assert!(toast_titles(h.state()).contains(&"Emergency stop sent".to_owned()));
    // M999 is offered (a killed Marlin ignores it, the board needs a reset).
    h.get_by_label("Restart firmware (M999)").click();
    settle(&mut h);
    assert!(toast_titles(h.state()).contains(&"Restarting the firmware (M999)".to_owned()));
    // Reconnecting resets the (virtual) board.
    h.get_by_label("Reconnect").click();
    expect(&mut h, "a fresh connection", |app| {
        !app.state().halted && app.state().is_connected()
    });
    settle(&mut h);
    assert!(h.query_by_label("Printer halted").is_none());
}

#[test]
fn a_halted_printer_is_offered_no_actions() {
    let options = with_file(sim_options(), sample("calibration-cube.gcode"));
    let mut h = app(options, egui::vec2(1600.0, 1000.0));
    expect(&mut h, "the cube to be analyzed", analyzed);
    connect(&mut h);
    expect(&mut h, "the settings", |app| app.state().settings_loaded);
    // An autotune runs when the firmware halts.
    h.state()
        .printer()
        .start_pid_autotune(sprint_host::Heater::Hotend(0), 210.0, 8);
    expect(&mut h, "the autotune", |app| {
        app.state().autotune.as_ref().is_some_and(|a| a.running)
    });
    h.state().printer().emergency_stop();
    expect(&mut h, "the firmware to halt", |app| app.state().halted);
    let control = h.state().printer().sim_control().expect("simulator");
    control.clear_received();
    let disabled = |h: &Harness<'_, SprintApp>, label: &str| {
        h.get_by_label(label).accesskit_node().is_disabled()
    };
    // The sidebar's heater presets (above the macro of that name).
    let cool_down = h
        .get_all_by_label("Cool down")
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("cool down");
    assert!(cool_down.accesskit_node().is_disabled());
    for (tab, labels) in [
        (Tab::Calibration, vec!["Start autotune at 210 °C"]),
        (Tab::BedMesh, vec!["Probe bed"]),
        (Tab::Tuning, vec!["Save to EEPROM", "Load from EEPROM"]),
        (Tab::SdCard, vec!["Upload", "Initialize"]),
    ] {
        h.state_mut().push_action(AppAction::ShowTab(tab));
        settle(&mut h);
        for label in labels {
            assert!(disabled(&h, label), "{tab:?}: {label}");
        }
    }
    // The autotune does not read as running for good.
    h.state_mut()
        .push_action(AppAction::ShowTab(Tab::Calibration));
    settle(&mut h);
    assert!(h.query_by_label_contains("Autotune failed").is_some());
    assert!(h.query_by_label("Abort").is_none());
    h.state_mut().push_action(AppAction::ShowTab(Tab::SdCard));
    settle(&mut h);
    // The SD card's empty state offers nothing to click.
    assert!(h.query_by_label("Upload loaded file").is_none());
    // Nor are the tabs' automatic queries sent to the halted firmware.
    std::thread::sleep(Duration::from_millis(200));
    settle(&mut h);
    let got = control.received_commands();
    for code in ["M420", "M20", "M21", "M503"] {
        assert!(!got.iter().any(|c| c.starts_with(code)), "{code}: {got:?}");
    }
}

#[test]
fn an_emergency_stop_typed_in_the_console_goes_out_at_once() {
    let mut h = app(sim_options(), WINDOW);
    connect(&mut h);
    type_command(&mut h, "M112");
    assert!(!h.state().console().is_confirming());
    expect(&mut h, "the firmware to halt", |app| app.state().halted);
    assert!(received(h.state()).iter().any(|c| c == "M112"));
}

#[test]
fn host_prompts_are_answered() {
    let mut h = app(sim_options(), WINDOW);
    connect(&mut h);
    let control = h.state().printer().sim_control().expect("simulator");
    control.inject_line("//action:prompt_begin Insert filament");
    control.inject_line("//action:prompt_button Continue");
    control.inject_line("//action:prompt_show");
    expect(&mut h, "the prompt", |app| app.state().prompt.is_some());
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Continue").click();
    expect(&mut h, "the answer", |app| {
        received(app).iter().any(|c| c == "M876 S0")
    });
}

#[test]
fn a_prompt_with_many_choices_stays_centered() {
    let mut h = app(AppOptions::headless(), WINDOW);
    let choices = [
        "Continue printing",
        "Purge more filament",
        "Disable the runout sensor",
        "Cancel the print",
    ];
    h.state_mut().set_state_override(Some(PrinterState {
        status: ConnectionStatus::Connected,
        prompt: Some(sprint_host::HostPrompt {
            message: "Filament runout detected. What now?".into(),
            buttons: choices.iter().map(|c| (*c).to_owned()).collect(),
        }),
        ..PrinterState::default()
    }));
    settle(&mut h);
    // The dialog is about 460 points wide around the middle; the choices
    // wrap inside it instead of widening it to one side.
    let middle = WINDOW.x / 2.0;
    for label in choices.into_iter().chain(["Hide"]) {
        let rect = h.get_by_label(label).rect();
        assert!(
            rect.left() >= middle - 240.0 && rect.right() <= middle + 240.0,
            "{label}: {rect:?}"
        );
    }
}

#[test]
fn reconnect_and_disconnect() {
    let mut h = app(sim_options(), WINDOW);
    connect(&mut h);
    h.state_mut().push_action(AppAction::Reconnect);
    expect(&mut h, "the connection to drop", |app| {
        !app.state().is_connected()
    });
    expect(&mut h, "the connection to come back", |app| {
        app.state().is_connected()
    });
    h.get_by_label("Disconnect").click();
    expect(&mut h, "the connection to close", |app| {
        !app.state().status.is_open()
    });
    assert!(h.query_by_label("Connect").is_some());
}

#[test]
fn cancelling_a_connect_is_not_an_error() {
    // Nothing listens on port 1 of the loopback interface: the connect
    // fails a moment later, after the user cancelled it.
    let mut options = sim_options();
    options.config.profiles[0].connection = ConnectTarget::Tcp {
        host: "127.0.0.1".into(),
        port: 1,
    };
    let mut h = app(options, WINDOW);
    h.state_mut().push_action(AppAction::Connect);
    h.state_mut().push_action(AppAction::Disconnect);
    h.step();
    expect(&mut h, "the connect to end", |app| !app.connection_busy());
    settle(&mut h);
    let errors: Vec<_> = h
        .state()
        .toasts()
        .iter()
        .filter(|t| t.kind() == sprint_gui::widgets::ToastKind::Error)
        .map(|t| t.title().to_owned())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn reconnecting_while_connecting_comes_back() {
    let mut h = app(sim_options(), WINDOW);
    // Reconnect before the first connect has reported.
    h.state_mut().push_action(AppAction::Connect);
    h.state_mut().push_action(AppAction::Reconnect);
    expect(&mut h, "the virtual printer to connect", |app| {
        app.state().is_connected() && !app.connection_busy()
    });
    let errors: Vec<_> = h
        .state()
        .toasts()
        .iter()
        .filter(|t| t.kind() == sprint_gui::widgets::ToastKind::Error)
        .map(|t| t.title().to_owned())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn closing_during_a_print_asks_first() {
    let options = with_file(slow_sim_options(5.0), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    connect(&mut h);
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Start print").click();
    expect(&mut h, "the print to start", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status.is_active())
    });
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::ConfirmQuit));
    h.get_by_label("Keep printing").click();
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    assert!(
        h.state()
            .state()
            .job
            .as_ref()
            .is_some_and(|j| j.status.is_active())
    );
}

#[test]
fn disconnecting_or_reconnecting_during_a_print_asks_first() {
    let options = with_file(slow_sim_options(5.0), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    connect(&mut h);
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Start print").click();
    let printing = |app: &SprintApp| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status.is_active())
    };
    expect(&mut h, "the print to start", printing);

    // Disconnect: "Keep printing" keeps it.
    h.get_by_label("Disconnect").click();
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::ConfirmDisconnect));
    h.get_by_label("Keep printing").click();
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    assert!(printing(h.state()) && h.state().state().is_connected());

    // Reconnect from the menu asks too; confirmed, it comes back idle.
    h.state_mut().push_action(AppAction::Reconnect);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::ConfirmDisconnect));
    h.get_by_role_and_label(Role::Button, "Cancel and reconnect")
        .click();
    expect(&mut h, "the reconnect", |app| {
        app.state().is_connected() && !app.connection_busy() && !printing(app)
    });

    // Disconnecting without a job does not ask.
    h.get_by_label("Disconnect").click();
    expect(&mut h, "the connection to close", |app| {
        !app.state().status.is_open()
    });
    assert_eq!(h.state().dialog(), None);
}

/// A slow print of the cube, running, with its heaters switched on; the
/// virtual printer's control handle (it outlives the link, as a real
/// printer does).
fn heating_print() -> (
    Harness<'static, SprintApp>,
    sprint_host::sprint_sim::SimControl,
) {
    let options = with_file(slow_sim_options(5.0), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    connect(&mut h);
    let control = h.state().printer().sim_control().expect("simulator");
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Start print").click();
    let heating = control.clone();
    expect(&mut h, "the print to switch the heaters on", move |app| {
        let sim = heating.snapshot();
        app.state().job_active()
            && sim.bed_target > 0.0
            && sim.hotend_targets.iter().any(|t| *t > 0.0)
    });
    (h, control)
}

#[test]
fn the_sd_card_is_not_called_empty_before_it_was_read() {
    // The SD card tab opened for the first time during a print: the card
    // was not read (no listing while a job runs).
    let (mut h, _control) = heating_print();
    h.state_mut().push_action(AppAction::ShowTab(Tab::SdCard));
    settle(&mut h);
    assert!(h.query_by_label("Not read yet").is_some());
    assert!(h.query_by_label_contains("The card is empty").is_none());
    assert!(
        h.get_by_label("Read the card")
            .accesskit_node()
            .is_disabled()
    );
    assert!(h.query_by_label("Upload loaded file").is_none());
    assert!(h.get_by_label("Upload").accesskit_node().is_disabled());
    // Once the job is over, the card is read and its files listed.
    h.state().printer().cancel();
    expect(&mut h, "the card's files", |app| {
        !app.state().job_active() && !app.state().sd.files.is_empty()
    });
    settle(&mut h);
    assert!(h.query_by_label("Not read yet").is_none());
}

/// The cancel script ran: the heaters are off.
#[track_caller]
fn assert_heaters_off(control: &sprint_host::sprint_sim::SimControl) {
    let sim = control.snapshot();
    let got = control.received_commands();
    assert!(
        sim.hotend_targets.iter().all(|t| *t == 0.0) && sim.bed_target == 0.0,
        "hotends {:?}, bed {}, received {:?}",
        sim.hotend_targets,
        sim.bed_target,
        &got[got.len().saturating_sub(12)..]
    );
    assert!(got.iter().any(|c| c == "M104 S0"), "{got:?}");
}

#[test]
fn disconnecting_during_a_print_cancels_it_first() {
    let (mut h, control) = heating_print();
    h.get_by_label("Disconnect").click();
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::ConfirmDisconnect));
    assert!(
        h.query_by_label_contains("the cancel script turns the heaters off")
            .is_some()
    );
    h.get_by_label("Cancel and disconnect").click();
    expect(&mut h, "the connection to close", |app| {
        !app.state().status.is_open() && !app.connection_busy()
    });
    assert_heaters_off(&control);
    assert!(!h.state().printer().with_state(|s| s.job_active()));
}

#[test]
fn quitting_during_a_print_cancels_it_before_the_window_closes() {
    let (mut h, control) = heating_print();
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::ConfirmQuit));
    h.get_by_label("Cancel and quit").click();
    h.step();
    // The window waits for the printer to stop.
    assert!(!h.state().quit_requested());
    expect(&mut h, "the window to be asked to close", |app| {
        app.quit_requested()
    });
    assert!(!h.state().state().status.is_open());
    assert_eq!(h.state().dialog(), None);
    assert_heaters_off(&control);
}

#[test]
fn disconnecting_during_an_upload_closes_and_deletes_the_file_first() {
    let mut h = app(slow_sim_options(5.0), WINDOW);
    connect(&mut h);
    let control = h.state().printer().sim_control().expect("simulator");
    // Long enough to be still uploading however slowly the test runs.
    let text: String = (0..400_000)
        .map(|i| format!("G1 X{} Y10 F3000\n", i % 100))
        .collect();
    let file = Arc::new(sprint_host::sprint_gcode::GcodeFile::from_text(
        "cube.gcode",
        &text,
    ));
    h.state()
        .printer()
        .upload_to_sd(file, "CUBE.GCO")
        .expect("upload");
    expect(&mut h, "the upload to run", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.current_line > 50)
    });
    h.state_mut().push_action(AppAction::Disconnect);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::ConfirmDisconnect));
    assert!(
        h.query_by_label("Disconnect and cancel the upload?")
            .is_some()
    );
    h.get_by_label("Cancel and disconnect").click();
    expect(&mut h, "the connection to close", |app| {
        !app.state().status.is_open() && !app.connection_busy()
    });
    // The file was closed (M29) and the partial file deleted: nothing the
    // next connection sends ends up in it.
    let got = control.received_commands();
    let tail = &got[got.len().saturating_sub(3)..];
    assert!(tail.iter().any(|c| c == "M29"), "{tail:?}");
    assert!(tail.iter().any(|c| c == "M30 CUBE.GCO"), "{tail:?}");
}

#[test]
fn the_cancel_question_says_what_cancelling_the_job_does() {
    // A host print runs the cancel script.
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut()
        .set_state_override(Some(printing_state(JobStatus::Printing)));
    h.state_mut().push_action(AppAction::CancelJob);
    settle(&mut h);
    assert!(h.query_by_label("Cancel the print?").is_some());
    assert!(h.query_by_label_contains("cancel script").is_some());
    assert!(h.query_by_label("Cancel print").is_some());
    h.key_press(Key::Escape);
    settle(&mut h);
    // An upload deletes its partly written file: nothing about a print.
    let mut upload = printing_state(JobStatus::Printing);
    if let Some(job) = upload.job.as_mut() {
        job.kind = JobKind::SdUpload;
        job.name = "CUBE.GCO".into();
        job.progress = 0.52;
    }
    h.state_mut().set_state_override(Some(upload));
    h.state_mut().push_action(AppAction::CancelJob);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::ConfirmCancel));
    assert!(h.query_by_label("Cancel the upload?").is_some());
    assert!(
        h.query_by_label_contains("CUBE.GCO stops at 52% and the partly written file is deleted")
            .is_some()
    );
    assert!(h.query_by_label("Keep uploading").is_some());
    assert!(h.query_by_label("Cancel print").is_none());
}

#[test]
fn dropping_the_app_during_a_print_cancels_it() {
    // What also happens when the UI thread panics.
    let (h, control) = heating_print();
    drop(h);
    assert_heaters_off(&control);
}

/// An in-memory eframe store.
#[derive(Default)]
struct MemoryStore(std::collections::HashMap<String, String>);

impl eframe::Storage for MemoryStore {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

#[test]
fn ui_preferences_survive_a_restart() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut().push_action(AppAction::ShowTab(Tab::Gcode));
    h.state_mut()
        .push_action(AppAction::SetTheme(sprint_gui::theme::Theme::Light));
    h.state_mut().set_side_panels(false, true);
    // Typed commands are remembered even when nothing is connected.
    h.state_mut().console_mut().set_input("M105");
    h.state_mut().console_mut().focus_input();
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    let mut store = MemoryStore::default();
    eframe::App::save(h.state_mut(), &mut store);
    drop(h);

    let store: &'static MemoryStore = Box::leak(Box::new(store));
    let mut restored = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| {
            cc.storage = Some(store);
            SprintApp::new(
                cc,
                AppOptions {
                    restore_prefs: true,
                    ..AppOptions::headless()
                },
            )
        });
    restored.run_steps(3);
    let prefs = restored.state().ui_prefs();
    assert_eq!(restored.state().tab(), Tab::Gcode);
    assert_eq!(prefs.theme, sprint_gui::theme::Theme::Light);
    assert!(!prefs.show_sidebar);
    assert_eq!(prefs.history, ["M105"]);
    assert_eq!(restored.state().console().history(), ["M105"]);
}

#[test]
fn layer_keys_move_the_top_layer_under_the_pointer() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    if h.state().viewer_tab().viewer().is_none() {
        eprintln!("no GPU viewer on this machine: layer keys not tested");
        return;
    }
    h.state_mut().set_side_panels(false, false);
    settle(&mut h);
    h.state_mut().viewer_tab_mut().set_layer_range(10, 50);
    settle(&mut h);
    // Over the view (left of the cards and the slider).
    h.hover_at(egui::pos2(200.0, 600.0));
    settle(&mut h);
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert_eq!(h.state().viewer_tab().layer_range(), Some((10, 49)));
    h.key_press(Key::PageUp);
    settle(&mut h);
    assert_eq!(h.state().viewer_tab().layer_range(), Some((10, 59)));
    h.key_press_modifiers(Modifiers::SHIFT, Key::Home);
    settle(&mut h);
    assert_eq!(h.state().viewer_tab().layer_range(), Some((0, 59)));
    h.key_press(Key::End);
    settle(&mut h);
    assert_eq!(h.state().viewer_tab().layer_range(), Some((0, 99)));
}

#[test]
fn a_click_below_a_single_layer_lowers_the_bottom_handle() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    if h.state().viewer_tab().viewer().is_none() {
        eprintln!("no GPU viewer on this machine: not tested");
        return;
    }
    h.state_mut().set_side_panels(false, false);
    h.state_mut().viewer_tab_mut().set_layer_range(50, 50);
    settle(&mut h);
    // On the slider's track, well below the handle.
    let track = egui::pos2(WINDOW.x - 22.0, WINDOW.y - 150.0);
    h.hover_at(track);
    h.step();
    h.drag_at(track);
    h.step();
    h.drop_at(track);
    settle(&mut h);
    let (lo, hi) = h.state().viewer_tab().layer_range().expect("range");
    assert_eq!(hi, 50, "the top stays");
    assert!(lo < 50, "the bottom moved down: {lo}");
}

#[test]
fn the_cards_over_the_3d_view_keep_the_pointer_from_the_toolpath() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    if h.state().viewer_tab().viewer().is_none() {
        eprintln!("no GPU viewer on this machine: not tested");
        return;
    }
    h.state_mut().set_side_panels(false, false);
    settle(&mut h);
    let camera =
        |h: &Harness<'_, SprintApp>| *h.state().viewer_tab().viewer().expect("viewer").camera();
    let before = camera(&h);
    // A click on the file card's text (the toolpath's top layer lies
    // beneath it) selects nothing.
    let estimate = h.get_by_label("Estimate").rect().center();
    h.hover_at(estimate);
    h.step();
    h.drag_at(estimate);
    h.step();
    h.drop_at(estimate);
    settle(&mut h);
    assert_eq!(h.state().selected_line(), None);
    // A drag on the card does not orbit, the wheel over the legend does not
    // zoom.
    let to = estimate + egui::vec2(-60.0, 40.0);
    h.drag_at(estimate);
    h.step();
    h.hover_at(to);
    h.step();
    h.drop_at(to);
    settle(&mut h);
    let legend = h.get_by_label("Travel").rect().center() + egui::vec2(60.0, 0.0);
    h.hover_at(legend);
    h.step();
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, -3.0),
        phase: egui::TouchPhase::Move,
        modifiers: Modifiers::NONE,
    });
    settle(&mut h);
    assert_eq!(camera(&h), before);
}

#[test]
fn connecting_keeps_the_3d_view_of_the_reanalyzed_file() {
    let options = with_file(sim_options(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    if h.state().viewer_tab().viewer().is_none() {
        eprintln!("no GPU viewer on this machine: not tested");
        return;
    }
    h.state_mut().set_side_panels(false, false);
    settle(&mut h);
    h.state_mut().viewer_tab_mut().set_layer_range(10, 40);
    // Orbit the camera with a drag (between the legend and the file card).
    let (from, to) = (egui::pos2(640.0, 560.0), egui::pos2(700.0, 520.0));
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    h.hover_at(to);
    h.step();
    h.drop_at(to);
    settle(&mut h);
    let camera = *h.state().viewer_tab().viewer().expect("viewer").camera();
    let toolpath = |app: &SprintApp| {
        app.file()
            .and_then(|f| f.toolpath.as_ref())
            .map(|t| Arc::as_ptr(t) as usize)
    };
    let analyzed_offline = toolpath(h.state());
    // Connecting re-analyzes the file with the firmware's motion limits.
    connect(&mut h);
    expect(
        &mut h,
        "the re-analysis with the firmware's limits",
        |app| analyzed(app) && toolpath(app) != analyzed_offline,
    );
    settle(&mut h);
    assert_eq!(h.state().viewer_tab().layer_range(), Some((10, 40)));
    assert_eq!(
        *h.state().viewer_tab().viewer().expect("viewer").camera(),
        camera
    );
}

/// A real pointer click (press and release) at `pos`.
fn click_at(h: &mut Harness<'_, SprintApp>, pos: egui::Pos2) {
    h.hover_at(pos);
    h.step();
    h.drag_at(pos);
    h.step();
    h.drop_at(pos);
    h.step();
}

/// The jog moves (G91 / G1 / G90) the printer received.
fn jogs_received(app: &SprintApp) -> Vec<String> {
    received(app)
        .into_iter()
        .filter(|c| c.starts_with("G1 ") || c == "G91")
        .collect()
}

#[test]
fn keyboard_jog_leaves_the_layer_keys_to_the_3d_view() {
    let options = with_file(sim_options(), sample("calibration-cube.gcode"));
    let mut h = app(options, egui::vec2(1440.0, 1200.0));
    expect(&mut h, "the cube to be analyzed", analyzed);
    if h.state().viewer_tab().viewer().is_none() {
        eprintln!("no GPU viewer on this machine: not tested");
        return;
    }
    connect(&mut h);
    let control = h.state().printer().sim_control().expect("simulator");
    // Wait for the sidebar to stop changing (firmware and settings shown).
    expect(&mut h, "the settings", |app| {
        app.state().settings_loaded && analyzed(app)
    });
    settle(&mut h);
    // Keyboard jog on, by a click on its toggle: the keys jog.
    let toggle = h.get_by_label("Keyboard jog").rect().center();
    click_at(&mut h, toggle);
    settle(&mut h);
    assert!(
        h.query_by_label_contains("PgUp/PgDn Z").is_some(),
        "keyboard jog is on"
    );
    h.key_press(Key::ArrowDown);
    expect(&mut h, "the keyboard jog", |app| {
        jogs_received(app).iter().any(|c| c == "G1 Y-1 F3000")
    });
    // Shift never makes a move down ten times as far.
    control.clear_received();
    h.key_press_modifiers(Modifiers::SHIFT, Key::PageDown);
    expect(&mut h, "the Z jog", |app| {
        jogs_received(app).iter().any(|c| c.starts_with("G1 Z"))
    });
    assert!(
        jogs_received(h.state()).contains(&"G1 Z-1 F600".to_owned()),
        "{:?}",
        jogs_received(h.state())
    );
    // A click into the 3D view (between the legend and the file card,
    // below the part): the layer keys move the layers, not the printer.
    h.state_mut().viewer_tab_mut().set_layer_range(20, 60);
    settle(&mut h);
    let view = egui::pos2(660.0, 980.0);
    click_at(&mut h, view);
    settle(&mut h);
    control.clear_received();
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert_eq!(h.state().viewer_tab().layer_range(), Some((20, 59)));
    h.key_press_modifiers(Modifiers::SHIFT, Key::PageDown);
    settle(&mut h);
    assert_eq!(h.state().viewer_tab().layer_range(), Some((10, 59)));
    // Back in the controls, but with the pointer over the view: the view
    // still has the keys.
    // (On the sidebar's margin, beside the toggle.)
    click_at(&mut h, egui::pos2(4.0, toggle.y));
    h.hover_at(view);
    settle(&mut h);
    h.key_press(Key::ArrowUp);
    settle(&mut h);
    assert_eq!(h.state().viewer_tab().layer_range(), Some((10, 60)));
    std::thread::sleep(Duration::from_millis(200));
    settle(&mut h);
    assert_eq!(jogs_received(h.state()), Vec::<String>::new());
}

#[test]
fn the_menu_bar_runs_commands() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.get_by_label("Help").click();
    settle(&mut h);
    h.get_by_label_contains("About Sprint").click();
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::About));
    h.key_press(Key::Escape);
    settle(&mut h);

    h.get_by_label("View").click();
    settle(&mut h);
    h.get_by_label_contains("Light theme").click();
    settle(&mut h);
    assert_eq!(h.state().ui_prefs().theme, sprint_gui::theme::Theme::Light);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);

    h.get_by_label("View").click();
    settle(&mut h);
    h.get_by_label("Console").click();
    settle(&mut h);
    assert!(!h.state().ui_prefs().show_console);

    // Two items, two pages of the settings.
    for (item, page) in [
        ("Settings…", sprint_gui::settings::Page::Preferences),
        ("Printer profiles…", sprint_gui::settings::Page::Profile(0)),
    ] {
        h.get_by_label("Tools").click();
        settle(&mut h);
        h.get_by_label_contains(item).click();
        settle(&mut h);
        let shown = h.state_mut().settings_dialog_mut().map(|d| d.page());
        assert_eq!(shown, Some(page), "{item}");
        h.key_press(Key::Escape);
        settle(&mut h);
    }
}

/// Open the Printer menu (the menu bar's, not the tab of that name).
fn open_printer_menu(h: &mut Harness<'_, SprintApp>) {
    h.get_all_by_label("Printer")
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("the Printer menu")
        .click();
}

#[test]
fn the_menu_restarts_only_a_halted_firmware() {
    let mut h = app(sim_options(), WINDOW);
    connect(&mut h);
    let restart_item = |h: &mut Harness<'_, SprintApp>| {
        open_printer_menu(h);
        settle(h);
        let disabled = h
            .get_by_label_contains("Restart firmware (M999)")
            .accesskit_node()
            .is_disabled();
        h.key_press(Key::Escape);
        settle(h);
        disabled
    };
    // A running firmware answers M999 with a resend: not offered.
    assert!(restart_item(&mut h));
    h.state().printer().emergency_stop();
    expect(&mut h, "the firmware to halt", |app| app.state().halted);
    // Halted: the banner offers it too, so look at the menu's item.
    open_printer_menu(&mut h);
    settle(&mut h);
    let enabled = h
        .query_all_by_label_contains("Restart firmware (M999)")
        .filter(|n| !n.accesskit_node().is_disabled())
        .count();
    assert_eq!(enabled, 2, "the menu item and the banner's button");
}

/// Type `text` into the console's command line and press Enter.
fn type_command(h: &mut Harness<'_, SprintApp>, text: &str) {
    h.state_mut().console_mut().set_input(text);
    h.state_mut().console_mut().focus_input();
    settle(h);
    h.key_press(Key::Enter);
    settle(h);
}

#[test]
fn console_history_steps_through_single_word_commands() {
    let mut h = app(AppOptions::headless(), WINDOW);
    // Not connected: the commands are remembered (and given back).
    for command in ["M503", "G28"] {
        type_command(&mut h, command);
    }
    assert_eq!(h.state().console().history(), ["M503", "G28"]);
    h.state_mut().console_mut().set_input("");
    h.state_mut().console_mut().focus_input();
    settle(&mut h);
    // A recalled command is also a known G-code: the arrows must keep
    // walking the history instead of the completion list.
    for (key, expected) in [
        (Key::ArrowUp, "G28"),
        (Key::ArrowUp, "M503"),
        (Key::ArrowDown, "G28"),
        (Key::ArrowDown, ""),
    ] {
        h.key_press(key);
        settle(&mut h);
        assert_eq!(h.state().console().input(), expected, "after {key:?}");
    }
}

/// A synthetic state: connected and printing `cube.gcode`.
fn printing_state(status: JobStatus) -> PrinterState {
    let mut job = JobInfo::new(JobKind::HostPrint, "cube.gcode", 1000, 10_000);
    job.status = status;
    job.current_line = 400;
    PrinterState {
        status: ConnectionStatus::Connected,
        job: Some(job),
        ..PrinterState::default()
    }
}

#[test]
fn a_print_from_the_card_locks_motion_and_probing() {
    let mut h = app(AppOptions::headless(), egui::vec2(1600.0, 1000.0));
    // Started on the printer's display: no job, only its SD progress.
    let mut state = printing_state(JobStatus::Printing);
    state.job = None;
    state.sd.printing = Some((1_000, 50_000));
    h.state_mut().set_state_override(Some(state));
    settle(&mut h);
    assert!(h.get_by_label("XY jog").accesskit_node().is_disabled());
    assert!(h.get_by_label("G28").accesskit_node().is_disabled());
    for (tab, label) in [
        (Tab::Calibration, "Start autotune at 210 °C"),
        (Tab::BedMesh, "Probe bed"),
    ] {
        h.state_mut().push_action(AppAction::ShowTab(tab));
        settle(&mut h);
        assert!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            "{tab:?}: {label}"
        );
    }
    assert!(
        h.query_by_label("The printer prints from its card")
            .is_none()
    );
    h.state_mut()
        .push_action(AppAction::ShowTab(Tab::Calibration));
    settle(&mut h);
    assert!(
        h.query_by_label("The printer prints from its card")
            .is_some()
    );
    // A move typed in the console asks first.
    type_command(&mut h, "G1 X10");
    assert!(h.state().console().is_confirming());
}

#[test]
fn console_motion_is_guarded_while_a_job_runs() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut()
        .set_state_override(Some(printing_state(JobStatus::Printing)));
    settle(&mut h);
    // Homing would interleave with the print: its chip is off, the
    // queries stay available.
    assert!(h.get_by_label("G28").accesskit_node().is_disabled());
    assert!(!h.get_by_label("M105").accesskit_node().is_disabled());
    // Typed moves ask first; harmless commands go straight out.
    type_command(&mut h, "G1 X10 F3000");
    assert!(h.state().console().is_confirming());
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert!(!h.state().console().is_confirming());
    type_command(&mut h, "M106 S255");
    assert!(!h.state().console().is_confirming());
    // While paused nothing is streamed: moves are the user's business.
    h.state_mut()
        .set_state_override(Some(printing_state(JobStatus::Paused)));
    settle(&mut h);
    type_command(&mut h, "G1 X10 F3000");
    assert!(!h.state().console().is_confirming());
    // But homing Z and probing lower the nozzle onto the part: they ask,
    // paused or not.
    for line in ["G28", "G29", "G28 Z", "M84"] {
        type_command(&mut h, line);
        assert!(h.state().console().is_confirming(), "{line}");
        assert!(
            h.query_by_label("Send this while the print is paused?")
                .is_some()
        );
        h.get_by_label("Cancel").click();
        settle(&mut h);
    }
    // Homing X and Y parks the head: fine while paused.
    type_command(&mut h, "G28 X Y");
    assert!(!h.state().console().is_confirming());
    // Without a job the chip is back.
    let mut idle = printing_state(JobStatus::Completed);
    idle.job = None;
    h.state_mut().set_state_override(Some(idle));
    settle(&mut h);
    assert!(!h.get_by_label("G28").accesskit_node().is_disabled());
}

#[test]
fn an_upload_of_the_file_is_not_shown_as_a_print() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    let job_of = |kind| {
        let mut state = printing_state(JobStatus::Printing);
        if let Some(job) = &mut state.job {
            job.kind = kind;
            job.name = "calibration-cube.gcode".into();
        }
        state
    };
    let progress_line = |app: &SprintApp| {
        app.viewer_tab()
            .viewer()
            .and_then(|v| v.options().progress_line)
    };
    // Printing the file: its progress shows in the 3D view and the G-code.
    h.state_mut()
        .set_state_override(Some(job_of(JobKind::HostPrint)));
    settle(&mut h);
    if h.state().viewer_tab().viewer().is_some() {
        assert_eq!(progress_line(h.state()), Some(399));
    }
    h.state_mut().push_action(AppAction::ShowTab(Tab::Gcode));
    settle(&mut h);
    assert!(h.query_by_label("Follow print").is_some());
    // Uploading it to the SD card prints nothing.
    h.state_mut()
        .set_state_override(Some(job_of(JobKind::SdUpload)));
    settle(&mut h);
    assert!(h.query_by_label("Follow print").is_none());
    h.state_mut().push_action(AppAction::ShowTab(Tab::View3D));
    settle(&mut h);
    assert_eq!(progress_line(h.state()), None);
    // The job bar still reports the upload.
    assert!(h.query_by_label("UPLOADING").is_some());
}

#[test]
fn the_empty_console_does_not_ask_to_connect_when_connected() {
    let mut h = app(AppOptions::headless(), WINDOW);
    settle(&mut h);
    // The controls sidebar also mentions connecting, so match the console's
    // own empty-state text.
    assert!(h.query_by_label_contains("to see its traffic").is_some());
    h.state_mut().set_state_override(Some(PrinterState {
        status: ConnectionStatus::Connected,
        ..PrinterState::default()
    }));
    settle(&mut h);
    assert!(h.query_by_label_contains("to see its traffic").is_none());
    assert!(h.query_by_label_contains("replies show up here").is_some());
}

#[test]
fn shortcuts_wait_while_the_console_asks() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut()
        .set_state_override(Some(printing_state(JobStatus::Printing)));
    settle(&mut h);
    // A move typed during the print: the console asks first.
    type_command(&mut h, "G1 X10 F3000");
    assert!(h.state().console().is_confirming());
    // The window's shortcuts do not reach behind the question.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    h.key_press_modifiers(Modifiers::COMMAND, Key::O);
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    assert_eq!(h.state().file_dialog_requests(), 0);
    assert!(h.state().console().is_confirming());
    h.get_by_label("Cancel").click();
    settle(&mut h);
    // Answered: they work again.
    h.key_press_modifiers(Modifiers::COMMAND, Key::O);
    settle(&mut h);
    assert_eq!(h.state().file_dialog_requests(), 1);
}

#[test]
fn switching_profiles_analyzes_the_file_again() {
    let mut options = with_file(sim_options(), sample("calibration-cube.gcode"));
    let other = sprint_host::PrinterProfile {
        name: "Big filament printer".into(),
        filament_diameter: 2.85,
        ..options.config.profiles[0].clone()
    };
    options.config.profiles.push(other);
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    let before = h
        .state()
        .file()
        .and_then(|f| f.toolpath.clone())
        .expect("toolpath");
    // Pick the other profile in the connection bar.
    h.query_all_by_role(Role::ComboBox)
        .find(|n| n.value().is_some_and(|v| v.contains("Test printer")))
        .expect("the profile combo")
        .click();
    settle(&mut h);
    h.get_by_label("Big filament printer").click();
    settle(&mut h);
    assert_eq!(h.state().config().active_profile, 1);
    expect(&mut h, "the analysis with the new filament", |app| {
        analyzed(app)
            && app
                .file()
                .and_then(|f| f.toolpath.as_ref())
                .is_some_and(|t| !Arc::ptr_eq(t, &before))
    });
    // Disconnected, nothing else changed: no further analysis.
    let after = h
        .state()
        .file()
        .and_then(|f| f.toolpath.clone())
        .expect("toolpath");
    settle(&mut h);
    assert!(!h.state().is_loading());
    assert!(
        h.state()
            .file()
            .and_then(|f| f.toolpath.as_ref())
            .is_some_and(|t| Arc::ptr_eq(t, &after))
    );
}

#[test]
fn an_unfinished_address_does_not_stick_to_another_profile() {
    let mut options = AppOptions::headless();
    let bridge = |name: &str, host: &str| sprint_host::PrinterProfile {
        name: name.into(),
        connection: ConnectTarget::Tcp {
            host: host.into(),
            port: 23,
        },
        ..sprint_host::PrinterProfile::default()
    };
    options.config.profiles = vec![bridge("Bridge A", "a.local"), bridge("Bridge B", "b.local")];
    let mut h = app(options, WINDOW);
    let address = |h: &Harness<'_, SprintApp>| {
        h.query_all_by_role(Role::TextInput)
            .filter_map(|n| n.value())
            .find(|v| v.ends_with(":23") || v.contains("nonsense"))
    };
    assert_eq!(address(&h).as_deref(), Some("a.local:23"));
    // Half-typed garbage: Connect is off.
    h.query_all_by_role(Role::TextInput)
        .find(|n| n.value().as_deref() == Some("a.local:23"))
        .expect("the address field")
        .click();
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(egui::Event::Text("nonsense".into()));
    settle(&mut h);
    assert_eq!(address(&h).as_deref(), Some("nonsense"));
    assert!(h.get_by_label("Connect").accesskit_node().is_disabled());
    // Another profile shows its own address, and can connect.
    h.query_all_by_role(Role::ComboBox)
        .find(|n| n.value().is_some_and(|v| v.contains("Bridge A")))
        .expect("the profile combo")
        .click();
    settle(&mut h);
    h.get_by_label("Bridge B").click();
    settle(&mut h);
    assert_eq!(h.state().config().active_profile, 1);
    assert_eq!(address(&h).as_deref(), Some("b.local:23"));
    assert!(!h.get_by_label("Connect").accesskit_node().is_disabled());
}

#[test]
fn the_settings_show_each_profile_its_own_address() {
    let mut options = AppOptions::headless();
    let bridge = |name: &str, host: &str| sprint_host::PrinterProfile {
        name: name.into(),
        connection: ConnectTarget::Tcp {
            host: host.into(),
            port: 23,
        },
        ..sprint_host::PrinterProfile::default()
    };
    options.config.profiles = vec![bridge("Bridge A", "a.local"), bridge("Bridge B", "b.local")];
    let mut h = app(options, WINDOW);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    let field = |h: &Harness<'_, SprintApp>, value: &str| {
        h.query_all_by_role(Role::TextInput)
            .any(|n| n.value().as_deref() == Some(value))
    };
    // Garbage typed into the first profile's address...
    h.query_all_by_role(Role::TextInput)
        .rfind(|n| n.value().as_deref() == Some("a.local:23"))
        .expect("the address field (the dialog's, after the bar's)")
        .click();
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(egui::Event::Text("nonsense".into()));
    settle(&mut h);
    assert!(field(&h, "nonsense"));
    // ...is not shown as the second profile's.
    h.state_mut()
        .settings_dialog_mut()
        .expect("settings")
        .set_page(sprint_gui::settings::Page::Profile(1));
    settle(&mut h);
    assert!(field(&h, "b.local:23"));
    assert!(!field(&h, "nonsense"));
    // Both profiles keep their addresses.
    h.get_by_label("Save").click();
    settle(&mut h);
    let hosts: Vec<String> = h
        .state()
        .config()
        .profiles
        .iter()
        .map(|p| p.connection.label())
        .collect();
    assert_eq!(hosts, ["tcp://a.local:23", "tcp://b.local:23"]);
}

#[test]
fn firmware_limits_analysis_survives_a_disconnect() {
    let options = with_file(sim_options(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    let first = h
        .state()
        .file()
        .and_then(|f| f.toolpath.clone())
        .expect("toolpath");
    connect(&mut h);
    // The virtual printer's M503 limits differ from the defaults.
    expect(&mut h, "the analysis with the firmware's limits", |app| {
        analyzed(app)
            && app
                .file()
                .and_then(|f| f.toolpath.as_ref())
                .is_some_and(|t| !Arc::ptr_eq(t, &first))
    });
    let with_firmware = h
        .state()
        .file()
        .and_then(|f| f.toolpath.clone())
        .expect("toolpath");
    h.get_by_label("Disconnect").click();
    expect(&mut h, "the connection to close", |app| {
        !app.state().status.is_open()
    });
    settle(&mut h);
    assert!(!h.state().is_loading());
    assert!(
        h.state()
            .file()
            .and_then(|f| f.toolpath.as_ref())
            .is_some_and(|t| Arc::ptr_eq(t, &with_firmware))
    );
}

#[test]
fn saving_settings_keeps_files_opened_meanwhile() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    // A file dropped while the dialog is open loads and is remembered...
    let cube = sample("calibration-cube.gcode");
    h.input_mut()
        .dropped_files
        .push(Arc::new(Dropped(cube.clone())));
    expect(&mut h, "the dropped file to load", analyzed);
    assert_eq!(h.state().config().recent_files.first(), Some(&cube));
    // ...and saving the settings does not forget it.
    h.state_mut()
        .settings_dialog_mut()
        .expect("settings")
        .draft
        .profiles[0]
        .max_z = 300.0;
    h.get_by_label("Save").click();
    settle(&mut h);
    assert_eq!(h.state().config().active().max_z, 300.0);
    assert_eq!(h.state().config().recent_files.first(), Some(&cube));
}

#[test]
fn the_console_completes_gcode() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut().console_mut().focus_input();
    settle(&mut h);
    h.event(egui::Event::Text("M10".into()));
    settle(&mut h);
    assert_eq!(h.state().console().input(), "M10");
    // Tab accepts the first suggestion from the G-code reference.
    h.key_press(Key::Tab);
    settle(&mut h);
    let first = sprint_gui::panels::console::suggestions("M10")[0].code;
    assert_eq!(h.state().console().input(), format!("{first} "));
}

#[test]
fn long_texts_are_cut_before_the_buttons() {
    let size = egui::Vec2::from(sprint_gui::MIN_WINDOW_SIZE);
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, size);
    expect(&mut h, "the cube to be analyzed", analyzed);
    let right_of = |a: egui::Rect, b: egui::Rect| a.right() <= b.left() + 0.5;

    // A long job name stops before the job statistics.
    let mut state = printing_state(JobStatus::Printing);
    if let Some(job) = &mut state.job {
        job.name = "Benchy_0.2mm_PLA_MK3S_1h23m_with_supports_and_a_long_name.gcode".into();
        // Every statistic shown, as in a real print.
        job.elapsed = Duration::from_secs(18 * 60 + 41);
        job.eta = Some(Duration::from_secs(24 * 60 + 5));
        job.layer = Some((40, 100));
        job.filament_used_mm = 1834.0;
    }
    h.state_mut().set_state_override(Some(state));
    settle(&mut h);
    let name = h.get_by_label_contains("Benchy_0.2mm").rect();
    let stats = h.get_by_label_contains("Elapsed").rect();
    assert!(right_of(name, stats), "{name:?} runs into {stats:?}");

    // A long halt reason stops before the banner's buttons, a long prompt
    // before its notice's button.
    let mut state = printing_state(JobStatus::Failed("halted".into()));
    state.halted = true;
    state.last_error = Some(
        "Heating failed, system stopped! Heater_ID: 0 - Printer halted. kill() called! \
         Thermal runaway protection triggered on the hotend"
            .into(),
    );
    state.prompt = Some(sprint_host::HostPrompt {
        message: "Filament runout detected on extruder 1: insert new filament, then press \
                  Continue on the printer or here to purge and resume printing"
            .into(),
        buttons: vec!["Continue".into()],
    });
    h.state_mut().set_state_override(Some(state));
    settle(&mut h);
    h.get_by_label("Hide").click();
    settle(&mut h);
    let reason = h.get_by_label_contains("Heating failed").rect();
    let restart = h.get_by_label("Restart firmware (M999)").rect();
    assert!(
        right_of(reason, restart),
        "{reason:?} runs into {restart:?}"
    );
    let waiting = h.get_by_label_contains("The printer is waiting").rect();
    let answer = h.get_by_label("Answer").rect();
    assert!(
        right_of(waiting, answer),
        "{waiting:?} runs into {answer:?}"
    );
    assert!(answer.right() <= size.x);
}

#[test]
fn the_gcode_toolbar_fits_a_narrow_tab() {
    // The smallest window with both side panels open leaves the tabs about
    // 350 points.
    let size = egui::Vec2::from(sprint_gui::MIN_WINDOW_SIZE);
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, size);
    expect(&mut h, "the cube to be analyzed", analyzed);
    let mut state = printing_state(JobStatus::Printing);
    if let Some(job) = &mut state.job {
        job.name = "calibration-cube.gcode".into();
    }
    h.state_mut().set_state_override(Some(state));
    h.state_mut().push_action(AppAction::FindInGcode);
    settle(&mut h);
    h.event(egui::Event::Text("G1 X1".into()));
    h.state_mut().push_action(AppAction::GotoLine);
    settle(&mut h);
    // Everything stays left of the console (its first quick command).
    let console = h.get_by_label("M105").rect().left() - 10.0;
    let mut controls: Vec<egui::Rect> = h
        .query_all_by_label("Close (Esc)")
        .map(|n| n.rect())
        .collect();
    assert_eq!(controls.len(), 2, "find and go-to close buttons");
    for label in ["Previous (Shift+Enter)", "Next (Enter)", "Follow print"] {
        controls.push(h.get_by_label(label).rect());
    }
    for rect in controls {
        assert!(rect.right() <= console, "{rect:?} is cut off at {console}");
    }
}

#[test]
fn nothing_runs_under_the_estop_in_a_narrow_window() {
    let size = egui::Vec2::from(sprint_gui::MIN_WINDOW_SIZE);
    let mut options = AppOptions::headless();
    let profile = &mut options.config.profiles[0];
    profile.name = "Workshop printer with a long name".into();
    profile.connection = ConnectTarget::Tcp {
        host: "octopi-workshop-printer.local".into(),
        port: 8080,
    };
    let mut h = app(options, size);
    h.state_mut().set_state_override(Some(PrinterState {
        status: ConnectionStatus::Connected,
        target: Some("tcp://octopi-workshop-printer.local:8080".into()),
        firmware: sprint_host::sprint_proto::parse_firmware_line(
            "FIRMWARE_NAME:Marlin 2.1.2.1 (Github) PROTOCOL_VERSION:1.0 EXTRUDER_COUNT:1",
        ),
        ..PrinterState::default()
    }));
    settle(&mut h);
    let estop = h.get_by_label("E-STOP").rect();
    let mut left: Vec<egui::Rect> = h
        .query_all_by_label_contains("Marlin")
        .map(|n| n.rect())
        .collect();
    left.push(h.get_by_label("Connected").rect());
    left.push(h.get_by_label("Disconnect").rect());
    for rect in left {
        assert!(
            rect.right() <= estop.left(),
            "{rect:?} runs under the E-STOP at {estop:?}"
        );
    }
}

#[test]
fn a_stray_click_beside_the_settings_keeps_the_edits() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    h.state_mut()
        .settings_dialog_mut()
        .expect("settings")
        .draft
        .profiles[0]
        .max_z = 321.0;
    // A click on the dimmed window around the dialog.
    let beside = egui::pos2(40.0, WINDOW.y / 2.0);
    h.hover_at(beside);
    h.step();
    h.drag_at(beside);
    h.step();
    h.drop_at(beside);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::Settings));
    let dialog = h.state_mut().settings_dialog_mut().expect("still open");
    assert_eq!(dialog.draft.profiles[0].max_z, 321.0);
    // Escape still cancels.
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    assert_ne!(h.state().config().active().max_z, 321.0);
}

#[test]
fn reconnect_reopens_the_same_connection() {
    // Started on the virtual printer while the profile names another
    // target (here an unused TCP port; in real life often a serial port):
    // Reconnect must come back to the virtual printer, not open the
    // profile's target.
    let mut options = sim_options();
    options.simulator = Some(fast_sim());
    options.config.profiles[0].connection = ConnectTarget::Tcp {
        host: "127.0.0.1".into(),
        port: 1,
    };
    let mut h = app(options, WINDOW);
    expect(&mut h, "the virtual printer to connect", |app| {
        app.state().is_connected()
    });
    assert_eq!(h.state().state().target.as_deref(), Some("Virtual printer"));
    // The (locked) connection bar shows what is connected, not the profile.
    settle(&mut h);
    let shown: Vec<String> = h
        .query_all_by_role(Role::ComboBox)
        .filter_map(|n| n.value())
        .collect();
    assert!(
        shown.iter().any(|v| v.contains("Virtual printer")),
        "{shown:?}"
    );
    assert!(!shown.iter().any(|v| v.contains("Network")), "{shown:?}");
    h.state_mut().push_action(AppAction::Reconnect);
    expect(&mut h, "the connection to drop", |app| {
        !app.state().is_connected()
    });
    expect(&mut h, "the virtual printer to come back", |app| {
        app.state().is_connected() || matches!(app.state().status, ConnectionStatus::Failed(_))
    });
    assert_eq!(h.state().state().target.as_deref(), Some("Virtual printer"));
    assert!(h.state().state().is_connected());
}

#[test]
fn the_start_dialog_closes_when_another_file_loads() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    h.state_mut().set_state_override(Some(PrinterState {
        status: ConnectionStatus::Connected,
        ..PrinterState::default()
    }));
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 8000 });
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::StartPrint));
    // A three-line file replaces the cube: line 8,001 means nothing there.
    let dir = temp_dir("replace");
    let small = dir.join("small.gcode");
    std::fs::write(&small, "G28\nG1 Z0.2 F600\nG1 X10 Y0 E1 F1200\n").expect("write");
    h.state_mut().push_action(AppAction::LoadFile(small));
    expect(&mut h, "the small file to load", |app| {
        analyzed(app) && app.file().is_some_and(|f| f.file.len() == 3)
    });
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_start_dialog_fits_the_smallest_window() {
    let size = egui::Vec2::from(sprint_gui::MIN_WINDOW_SIZE);
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, size);
    expect(&mut h, "the cube to be analyzed", analyzed);
    h.state_mut().set_state_override(Some(PrinterState {
        status: ConnectionStatus::Connected,
        ..PrinterState::default()
    }));
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    let dialog = h.state_mut().start_print_dialog_mut().expect("dialog");
    dialog.set_mode(sprint_gui::dialogs::StartMode::Layer);
    dialog.set_layer(12);
    settle(&mut h);
    // Its warning makes the dialog taller still.
    h.get_by_label("Home Z").click();
    settle(&mut h);
    // The whole frame (20 points of margin around the content) is on
    // screen: the title at the top, the buttons at the bottom.
    let rects: Vec<egui::Rect> = h
        .query_all_by_label("Start print")
        .map(|n| n.rect())
        .collect();
    let top = rects.iter().map(|r| r.top()).fold(f32::INFINITY, f32::min);
    let bottom = rects.iter().map(|r| r.bottom()).fold(0.0, f32::max);
    assert!(top >= 20.0, "the title is cut off: {rects:?}");
    assert!(
        bottom <= size.y - 20.0,
        "the buttons are cut off: {rects:?}"
    );
}

#[test]
fn the_3d_view_ignores_keys_and_wheel_behind_a_dialog() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    if h.state().viewer_tab().viewer().is_none() {
        eprintln!("no GPU viewer on this machine: not tested");
        return;
    }
    h.state_mut().set_side_panels(false, true);
    settle(&mut h);
    h.state_mut().viewer_tab_mut().set_layer_range(10, 50);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    // Over the dialog, which covers the view and its layer slider.
    for pos in [egui::pos2(500.0, 400.0), egui::pos2(875.0, 400.0)] {
        h.hover_at(pos);
        settle(&mut h);
        h.key_press(Key::ArrowDown);
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -3.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        });
        settle(&mut h);
        assert_eq!(
            h.state().viewer_tab().layer_range(),
            Some((10, 50)),
            "at {pos:?}"
        );
    }
    assert_eq!(h.state().dialog(), Some(DialogKind::Settings));
}

#[test]
fn the_printer_cannot_be_switched_while_connected() {
    let mut options = sim_options();
    let other = sprint_host::PrinterProfile {
        name: "Other printer".into(),
        bed_size: [400.0, 400.0],
        ..options.config.profiles[0].clone()
    };
    options.config.profiles.push(other);
    let mut h = app(options, WINDOW);
    connect(&mut h);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    // The profile of the open connection stays active: its bed size and
    // parking position drive the scripts of the printer that is attached.
    let dialog = h.state_mut().settings_dialog_mut().expect("settings");
    dialog.set_page(sprint_gui::settings::Page::Profile(1));
    settle(&mut h);
    let switch = h.get_by_label("Use this printer");
    assert!(switch.accesskit_node().is_disabled());
    switch.click();
    settle(&mut h);
    h.state_mut()
        .settings_dialog_mut()
        .expect("settings")
        .set_page(sprint_gui::settings::Page::Profile(0));
    settle(&mut h);
    assert!(
        h.get_by_label("Delete this profile")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label("Save").click();
    settle(&mut h);
    assert_eq!(h.state().config().active_profile, 0);
    assert_eq!(h.state().printer().profile().bed_size, [220.0, 220.0]);
}
