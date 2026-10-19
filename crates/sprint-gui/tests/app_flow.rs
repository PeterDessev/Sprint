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
use egui_kittest::kittest::Queryable;
use sprint_gui::app::{AppOptions, DialogKind, SprintApp};
use sprint_gui::context::{AppAction, Tab};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{ConnectTarget, JobStatus};

/// Options with the fast simulator, slowed down to `time_scale` with real
/// move times (so a print lasts long enough to pause it).
fn slow_sim_options(time_scale: f32) -> AppOptions {
    let mut options = sim_options();
    options.config.profiles[0].connection = ConnectTarget::Simulator(SimConfig {
        instant_moves: false,
        time_scale,
        ..fast_sim()
    });
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
fn shortcuts_switch_tabs_and_open_dialogs() {
    let options = with_file(AppOptions::headless(), sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);

    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    settle(&mut h);
    assert_eq!(h.state().tab(), Tab::Gcode);
    assert!(h.state().gcode_tab().find().open);

    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::Settings));
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
        assert!(lo <= 60 && hi >= 60, "range {lo}..={hi}");
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
