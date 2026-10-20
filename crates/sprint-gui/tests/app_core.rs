//! The app with the host's newer capabilities, driven headless against the
//! built-in virtual printer: connecting at startup to a given target,
//! resetting the board after an emergency stop, recovering a print from
//! the line it stopped at, skipping objects, refusing files that are not
//! G-code, the shared command-safety classifier and the lenient
//! configuration load. No serial port is ever opened: every target here is
//! the simulator.

mod app_support;

use std::path::PathBuf;
use std::time::Duration;

use app_support::{WINDOW, app, expect, fast_sim, settle, sim_options};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use sprint_gui::app::{AppOptions, DialogKind, SprintApp};
use sprint_gui::context::AppAction;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{ConnectTarget, JobStatus};

fn connect(h: &mut Harness<'_, SprintApp>) {
    h.get_by_label("Connect").click();
    expect(h, "the virtual printer to connect", |app| {
        app.state().is_connected()
    });
}

#[allow(dead_code)]
fn analyzed(app: &SprintApp) -> bool {
    app.file().is_some_and(|f| f.toolpath.is_some()) && !app.is_loading()
}

#[allow(dead_code)]
fn toast_titles(app: &SprintApp) -> Vec<String> {
    app.toasts().iter().map(|t| t.title().to_owned()).collect()
}

#[allow(dead_code)]
fn received(app: &SprintApp) -> Vec<String> {
    app.printer()
        .sim_control()
        .map(|c| c.received_commands())
        .unwrap_or_default()
}

/// A private temporary directory.
#[allow(dead_code)]
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sprint-gui-core-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn a_target_given_at_startup_is_connected() {
    // `sprint gui --connect T`: that printer, though the profile's
    // auto-connect is off (and its printer another one).
    let given = SimConfig {
        machine_name: "Given printer".into(),
        ..fast_sim()
    };
    let options = AppOptions {
        connect: Some(ConnectTarget::Simulator(given)),
        ..sim_options()
    };
    assert!(!options.config.active().auto_connect);
    let mut h = app(options, WINDOW);
    expect(&mut h, "the given printer to answer", |app| {
        app.state()
            .firmware
            .as_ref()
            .and_then(|f| f.machine_type.as_deref())
            == Some("Given printer")
    });
    // Without a target, the virtual printer or auto-connect, nothing opens.
    let mut idle = app(sim_options(), WINDOW);
    settle(&mut idle);
    std::thread::sleep(Duration::from_millis(100));
    settle(&mut idle);
    assert!(!idle.state().state().status.is_open());
    assert!(!idle.state().connection_busy());
    connect(&mut idle);
}

#[test]
fn a_partly_broken_configuration_loads_and_says_what_it_left_out() {
    let dir = temp_dir("config");
    let path = dir.join("config.toml");
    let mut config = sim_options().config;
    config.profiles.push(sprint_host::PrinterProfile {
        name: "Second printer".into(),
        ..sprint_host::PrinterProfile::default()
    });
    config.save_to(&path).expect("file");
    let text = std::fs::read_to_string(&path).expect("text");
    let broken = text.replacen("checksum = \"Always\"", "checksum = \"always\"", 1);
    assert_ne!(broken, text);
    std::fs::write(&path, &broken).expect("write");
    // What AppOptions::desktop does with the file AppConfig::path() names.
    let (config, report) = sprint_host::AppConfig::load_checked(&path);
    assert!(!report.is_clean());
    let options = AppOptions {
        config,
        config_report: Some(report),
        config_path: Some(path.clone()),
        ..sim_options()
    };
    let mut h = app(options, WINDOW);
    settle(&mut h);
    // Both profiles loaded; a toast says what happened.
    assert_eq!(h.state().config().profiles.len(), 2);
    assert!(toast_titles(h.state()).contains(&"Some settings could not be read".to_owned()));
    let message = h
        .state()
        .toasts()
        .iter()
        .find_map(|t| t.message_text().map(str::to_owned))
        .unwrap_or_default();
    assert!(message.contains("config.toml.bak"), "{message}");
    // The preferences list the problem and the backup.
    h.state_mut().push_action(AppAction::OpenPreferences);
    settle(&mut h);
    assert!(h.query_by_label_contains("checksum").is_some());
    // The toast and the settings say where the original is.
    assert!(
        h.query_all_by_label_contains("The file as it was is kept in")
            .count()
            >= 2
    );
    assert!(
        h.query_all_by_label_contains(&format!("Saved to {}", path.display()))
            .next()
            .is_some()
    );
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

/// A printer state printing a host job (for the console's classifier).
fn printing_state(status: sprint_host::JobStatus) -> sprint_host::PrinterState {
    let mut job = sprint_host::JobInfo::new(sprint_host::JobKind::HostPrint, "x.gcode", 100, 1000);
    job.status = status;
    sprint_host::PrinterState {
        status: sprint_host::ConnectionStatus::Connected,
        job: Some(job),
        ..sprint_host::PrinterState::default()
    }
}

fn type_command(h: &mut Harness<'_, SprintApp>, text: &str) {
    h.state_mut().console_mut().set_input(text);
    h.state_mut().console_mut().focus_input();
    settle(h);
    h.key_press(egui::Key::Enter);
    settle(h);
}

#[test]
fn the_console_refuses_what_the_classifier_blocks_and_keeps_the_text() {
    let mut h = app(AppOptions::headless(), WINDOW);
    h.state_mut()
        .set_state_override(Some(printing_state(sprint_host::JobStatus::Printing)));
    settle(&mut h);
    // Writing to the card would swallow the print's next lines.
    type_command(&mut h, "M28 notes.gco");
    assert!(!h.state().console().is_confirming());
    assert!(toast_titles(h.state()).contains(&"Not sent".to_owned()));
    assert_eq!(h.state().console().input(), "M28 notes.gco");
    // A pasted block asks once, with each reason.
    h.state_mut().console_mut().set_input("");
    type_command(&mut h, "M105");
    assert!(!h.state().console().is_confirming());
    type_command(&mut h, "G91\nG1 Z5\nM84\nM105");
    assert!(h.state().console().is_confirming());
    assert!(h.query_by_label("Send this during the print?").is_some());
    assert!(h.query_by_label_contains("M84 (").is_some());
    h.get_by_label("Cancel").click();
    settle(&mut h);
    // A dangerous command asks even without a job.
    h.state_mut()
        .set_state_override(Some(sprint_host::PrinterState {
            job: None,
            ..printing_state(sprint_host::JobStatus::Printing)
        }));
    settle(&mut h);
    type_command(&mut h, "M502");
    assert!(h.query_by_label("Send a dangerous command?").is_some());
    h.get_by_label("Cancel").click();
    settle(&mut h);
}

#[test]
fn an_emergency_stop_in_a_pasted_block_goes_out_alone_at_once() {
    let mut h = app(sim_options(), WINDOW);
    connect(&mut h);
    let control = h.state().printer().sim_control().expect("simulator");
    control.clear_received();
    type_command(&mut h, "G1 X10\nM112\nG28");
    assert!(!h.state().console().is_confirming());
    expect(&mut h, "the firmware to halt", |app| app.state().halted);
    let got = control.received_commands();
    assert!(got.iter().any(|c| c == "M112"), "{got:?}");
    assert!(!got.iter().any(|c| c.starts_with("G28")), "{got:?}");
    assert!(!got.iter().any(|c| c.starts_with("G1 ")), "{got:?}");
    assert!(toast_titles(h.state()).contains(&"Emergency stop sent".to_owned()));
}

/// Options for prints that last: real move times, one command in flight.
fn slow_print_options(time_scale: f32) -> AppOptions {
    let mut options = sim_options();
    options.config.profiles[0].connection = ConnectTarget::Simulator(SimConfig {
        instant_moves: false,
        time_scale,
        ..fast_sim()
    });
    options.config.profiles[0].options.max_in_flight = 1;
    options
}

/// A file of slow back-and-forth moves (a print that takes a while).
fn long_file(lines: usize) -> String {
    let mut text = String::from("G28\nG1 Z5 F600\n");
    for i in 0..lines {
        let x = if i % 2 == 0 { 150 } else { 50 };
        text.push_str(&format!("G1 X{x} Y100 F1200\n"));
    }
    text
}

fn start_print(h: &mut Harness<'_, SprintApp>) {
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(h);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Start print")
        .click();
    expect(h, "the print to run", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
}

/// The progress line the 3D view shows (`None` without a GPU viewer).
fn progress_line(app: &SprintApp) -> Option<Option<u32>> {
    app.viewer_tab().viewer().map(|v| v.options().progress_line)
}

#[test]
fn a_print_is_matched_to_the_loaded_file_by_path() {
    let dir = temp_dir("paths");
    for sub in ["a", "b"] {
        std::fs::create_dir_all(dir.join(sub)).expect("dir");
    }
    let printed = dir.join("a").join("part.gcode");
    let namesake = dir.join("b").join("part.gcode");
    std::fs::write(&printed, long_file(3000)).expect("write");
    std::fs::write(&namesake, long_file(20)).expect("write");
    let mut options = slow_print_options(40.0);
    options.file = Some(printed.clone());
    let mut h = app(options, WINDOW);
    expect(&mut h, "the file to be analyzed", analyzed);
    connect(&mut h);
    start_print(&mut h);
    expect(&mut h, "the 3D view to follow the print", |app| {
        progress_line(app).is_none_or(|line| line.is_some())
    });
    // The printed file itself is not reloaded under the print.
    h.state_mut()
        .push_action(AppAction::LoadFile(printed.clone()));
    settle(&mut h);
    assert!(toast_titles(h.state()).contains(&"The file is printing".to_owned()));
    // A file of the same name in another folder is another file: it loads,
    // and the print's progress is not drawn on it.
    h.state_mut()
        .push_action(AppAction::LoadFile(namesake.clone()));
    expect(&mut h, "the other file", |app| {
        analyzed(app)
            && app
                .file()
                .and_then(|f| f.path.as_deref())
                .is_some_and(|p| sprint_gui::loader::same_path(p, &namesake))
    });
    settle(&mut h);
    assert!(h.state().state().job_active(), "the print goes on");
    if let Some(line) = progress_line(h.state()) {
        assert_eq!(line, None, "progress drawn on another file");
    }
    // Back to the printed file: its progress shows again.
    h.state_mut()
        .push_action(AppAction::LoadFile(printed.clone()));
    expect(&mut h, "the printed file", |app| {
        analyzed(app)
            && app
                .file()
                .and_then(|f| f.path.as_deref())
                .is_some_and(|p| sprint_gui::loader::same_path(p, &printed))
    });
    settle(&mut h);
    if let Some(line) = progress_line(h.state()) {
        assert!(line.is_some(), "no progress on the printed file");
    }
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn files_that_are_not_gcode_are_never_printed() {
    let dir = temp_dir("content");
    let mut h = app(sim_options(), WINDOW);
    connect(&mut h);
    // A model saved under a G-code name: refused, saying what it is.
    let stl = dir.join("cube.gcode");
    std::fs::write(
        &stl,
        "solid cube\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 1 0\n  endloop\n endfacet\nendsolid cube\n",
    )
    .expect("write");
    h.state_mut().push_action(AppAction::LoadFile(stl));
    expect(&mut h, "the refusal", |app| {
        app.toasts().iter().any(|t| {
            t.message_text()
                .is_some_and(|m| m.contains("slice it first"))
        })
    });
    assert!(h.state().file().is_none());
    // Notes open with a warning, but nothing prints or uploads them.
    let notes = dir.join("notes.txt");
    std::fs::write(&notes, "Shopping list\nmilk\neggs and bread\n").expect("write");
    h.state_mut().push_action(AppAction::LoadFile(notes));
    expect(&mut h, "the notes", |app| {
        app.file().is_some() && !app.is_loading()
    });
    settle(&mut h);
    assert!(toast_titles(h.state()).contains(&"This is not G-code".to_owned()));
    let start = h.get_by_label("Start print… (Ctrl+P)");
    assert!(start.accesskit_node().is_disabled());
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    assert_eq!(h.state().dialog(), None);
    assert!(toast_titles(h.state()).contains(&"Cannot print right now".to_owned()));
    assert!(h.state().state().job.is_none());
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_prompt_is_shown_while_the_printer_is_halted() {
    // A halted firmware ignores M876: the engine drops its prompt, and the
    // app never shows one meanwhile.
    let mut h = app(AppOptions::headless(), WINDOW);
    let mut state = sprint_host::PrinterState {
        status: sprint_host::ConnectionStatus::Connected,
        halted: true,
        prompt: Some(sprint_host::HostPrompt {
            message: "Insert filament".into(),
            buttons: vec!["Continue".into()],
        }),
        ..sprint_host::PrinterState::default()
    };
    h.state_mut().set_state_override(Some(state.clone()));
    settle(&mut h);
    assert!(h.query_by_label("The printer is asking").is_none());
    assert!(h.query_by_label("Printer halted").is_some());
    state.halted = false;
    h.state_mut().set_state_override(Some(state));
    settle(&mut h);
    assert!(h.query_by_label("The printer is asking").is_some());
}

#[test]
fn a_print_pauses_before_the_layer_picked_in_the_start_dialog() {
    let mut options = sim_options();
    options.file = Some(app_support::sample("calibration-cube.gcode"));
    // The profile asks for M73 progress on the printer's display.
    options.config.profiles[0].emit_progress = true;
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    connect(&mut h);
    let layer_start = h
        .state()
        .file()
        .and_then(|f| f.toolpath.as_ref())
        .and_then(|tp| tp.layers.get(2))
        .map(|l| l.lines.start as usize)
        .expect("a third layer");
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    let dialog = h.state_mut().start_print_dialog_mut().expect("dialog");
    assert!(dialog.emit_progress(), "the profile's default");
    dialog.set_pause_layer(Some(2));
    settle(&mut h);
    assert!(h.query_by_label("Pause before a layer").is_some());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Start print")
        .click();
    expect(&mut h, "the pause before layer 3", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    let job = h.state().state().job.clone().expect("job");
    assert!(
        (layer_start.saturating_sub(1)..=layer_start).contains(&job.current_line),
        "paused at line {} for a layer starting at {layer_start}",
        job.current_line
    );
    assert!(
        received(h.state()).iter().any(|c| c.starts_with("M73 P")),
        "no M73 progress"
    );
}

#[test]
fn the_start_dialog_cannot_turn_off_m73_the_profile_sends() {
    const M73: &str = "Show progress on the printer's display (M73)";
    let file = app_support::sample("calibration-cube.gcode");
    for profile_sends in [true, false] {
        let mut options = sim_options();
        options.file = Some(file.clone());
        options.config.profiles[0].emit_progress = profile_sends;
        let mut h = app(options, WINDOW);
        expect(&mut h, "the cube to be analyzed", analyzed);
        connect(&mut h);
        h.state_mut()
            .push_action(AppAction::StartPrintDialog { start_line: 0 });
        settle(&mut h);
        h.get_by_label(M73).scroll_to_me();
        settle(&mut h);
        // The host sends M73 in every print of a profile that asks for it
        // (PrintOptions::emit_progress only adds it): the dialog says so
        // instead of offering a switch that would do nothing.
        assert_eq!(
            h.get_by_label(M73).accesskit_node().is_disabled(),
            profile_sends
        );
        h.get_by_label(M73).click();
        settle(&mut h);
        let dialog = h.state_mut().start_print_dialog_mut().expect("dialog");
        assert!(dialog.emit_progress(), "profile_sends: {profile_sends}");
        if profile_sends {
            dialog.set_emit_progress(false);
            assert!(dialog.emit_progress(), "still sent");
        }
    }
}

#[test]
fn a_cancelled_print_is_recovered_from_the_line_it_stopped_at() {
    let dir = temp_dir("recover");
    let path = dir.join("part.gcode");
    std::fs::write(&path, long_file(3000)).expect("write");
    let mut options = slow_print_options(40.0);
    options.file = Some(path);
    let mut h = app(options, WINDOW);
    expect(&mut h, "the file to be analyzed", analyzed);
    connect(&mut h);
    // No print yet: nothing to recover.
    assert!(h.query_by_label("Recover…").is_none());
    start_print(&mut h);
    expect(&mut h, "some progress", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.current_line > 30)
    });
    h.state().printer().cancel();
    expect(&mut h, "the cancel", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Cancelled)
    });
    settle(&mut h);
    let stopped = h
        .state()
        .state()
        .job
        .as_ref()
        .and_then(|j| j.stopped_at)
        .expect("stopped_at");
    // The job bar offers to go on from there.
    h.get_by_label("Recover…").click();
    settle(&mut h);
    assert_eq!(h.state().dialog(), Some(DialogKind::StartPrint));
    assert!(h.query_by_label("Recover print").is_some());
    assert!(h.query_by_label_contains("(it was cancelled)").is_some());
    let toolpath = h
        .state()
        .file()
        .and_then(|f| f.toolpath.clone())
        .expect("toolpath");
    let dialog = h.state_mut().start_print_dialog_mut().expect("dialog");
    assert!(dialog.is_recovering());
    assert_eq!(dialog.start_line(Some(&toolpath)), stopped);
    // The preamble is shown and can be edited before it goes out.
    assert!(
        dialog.preamble().contains("G28 X Y"),
        "{}",
        dialog.preamble()
    );
    let control = h.state().printer().sim_control().expect("simulator");
    control.clear_received();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Start print")
        .click();
    expect(&mut h, "the recovered print", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.current_line > stopped)
    });
    // The preamble went out before the file's lines.
    let got = control.received_commands();
    assert!(got.iter().any(|c| c == "G28 X Y"), "{got:?}");
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_settings_edit_print_events_and_m73() {
    let mut h = app(sim_options(), WINDOW);
    h.state_mut().push_action(AppAction::OpenSettings);
    settle(&mut h);
    let dialog = h.state_mut().settings_dialog_mut().expect("settings");
    dialog.draft.profiles[0].emit_progress = false;
    settle(&mut h);
    // The page lists the commands' environment.
    assert!(h.query_by_label_contains("SPRINT_REASON").is_some());
    let field = h.get_by_label("When a print finishes");
    field.scroll_to_me();
    settle(&mut h);
    h.get_by_label("When a print finishes").click();
    settle(&mut h);
    h.get_by_label("When a print finishes")
        .type_text("echo done \"$SPRINT_FILE\"");
    settle(&mut h);
    h.get_by_label("Show progress on the printer's display (M73)")
        .scroll_to_me();
    settle(&mut h);
    h.get_by_label("Show progress on the printer's display (M73)")
        .click();
    settle(&mut h);
    h.get_by_label("Save").click();
    settle(&mut h);
    let profile = h.state().config().active().clone();
    assert_eq!(profile.on_finish_command, "echo done \"$SPRINT_FILE\"");
    assert!(profile.emit_progress);
    // The printer gets the profile with its commands.
    assert_eq!(
        h.state().printer().profile().on_finish_command,
        profile.on_finish_command
    );
}

#[test]
fn a_finished_print_asks_for_the_users_attention() {
    let dir = temp_dir("attention");
    let path = dir.join("short.gcode");
    std::fs::write(&path, "G28\nG1 Z5 F600\nG1 X50 Y50 F3000\nG1 X60 Y60\n").expect("write");
    let mut options = sim_options();
    options.file = Some(path);
    let mut h = app(options, WINDOW);
    expect(&mut h, "the file to be analyzed", analyzed);
    connect(&mut h);
    assert_eq!(h.state().attention_requests(), 0);
    h.state_mut()
        .push_action(AppAction::StartPrintDialog { start_line: 0 });
    settle(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Start print")
        .click();
    expect(&mut h, "the print to finish", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    expect(&mut h, "the attention request", |app| {
        app.attention_requests() == 1
    });
    assert!(toast_titles(h.state()).contains(&"Print finished".to_owned()));
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sizes_are_the_parts_and_the_files_on_disk() {
    // The cube's part, not its purge line: about 20 mm on every side.
    let mut options = AppOptions::headless();
    options.file = Some(app_support::sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    // Room for the file card beside the legend.
    h.state_mut().set_side_panels(false, false);
    expect(&mut h, "the cube to be analyzed", analyzed);
    settle(&mut h);
    let toolpath = h
        .state()
        .file()
        .and_then(|f| f.toolpath.clone())
        .expect("toolpath");
    let [x, y, z] = toolpath.part_bounds.size();
    assert!(
        (19.0..22.0).contains(&x) && (19.0..22.0).contains(&y),
        "{x} {y}"
    );
    let fmt = |v: f32| sprint_gui::format::mm(v, 1);
    let size = format!("{} × {} × {} mm", fmt(x), fmt(y), fmt(z));
    if h.state().viewer_tab().viewer().is_some() {
        assert!(h.query_by_label(&size).is_some(), "no {size}");
    }
    // A file with a byte order mark: its size on disk, not the text's.
    let dir = temp_dir("sizes");
    let path = dir.join("bom.gcode");
    std::fs::write(&path, "\u{FEFF}G28\nG1 X10 Y10 F3000\n").expect("write");
    h.state_mut().push_action(AppAction::LoadFile(path));
    expect(&mut h, "the file", |app| {
        app.file().is_some_and(|f| f.file.name() == "bom.gcode") && !app.is_loading()
    });
    h.state_mut()
        .push_action(AppAction::ShowTab(sprint_gui::context::Tab::Gcode));
    settle(&mut h);
    let on_disk = h.state().file().expect("file").file.source_len();
    assert_eq!(on_disk, 24);
    assert!(h.query_by_label("24 B").is_some());
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_3d_view_frames_the_part_clear_of_its_cards() {
    let mut options = AppOptions::headless();
    options.file = Some(app_support::sample("calibration-cube.gcode"));
    let mut h = app(options, WINDOW);
    expect(&mut h, "the cube to be analyzed", analyzed);
    if h.state().viewer_tab().viewer().is_none() {
        eprintln!("no GPU viewer on this machine: not tested");
        return;
    }
    // Room for the legend and the file card side by side.
    h.state_mut().set_side_panels(false, false);
    let camera =
        |h: &Harness<'_, SprintApp>| *h.state().viewer_tab().viewer().expect("viewer").camera();
    let mut last = camera(&h);
    for _ in 0..20 {
        settle(&mut h);
        if camera(&h) == last {
            break;
        }
        last = camera(&h);
    }
    let (view, cards) = h
        .state()
        .viewer_tab()
        .view_and_cards()
        .map(|(v, c)| (v, c.to_vec()))
        .expect("drawn");
    assert_eq!(cards.len(), 2, "the legend and the file card");
    let toolpath = h
        .state()
        .file()
        .and_then(|f| f.toolpath.clone())
        .expect("toolpath");
    let b = toolpath.part_bounds;
    let size = [view.width(), view.height()];
    let viewer = h.state().viewer_tab().viewer().expect("viewer");
    for i in 0..8 {
        let corner = [
            if i & 1 == 0 { b.min[0] } else { b.max[0] },
            if i & 2 == 0 { b.min[1] } else { b.max[1] },
            if i & 4 == 0 { b.min[2] } else { b.max[2] },
        ];
        let [x, y] = viewer.project(corner, size).expect("in front");
        let on_screen = view.min + egui::vec2(x, y);
        assert!(view.contains(on_screen), "{corner:?} at {on_screen:?}");
        for card in &cards {
            assert!(
                !card.contains(on_screen),
                "{corner:?} at {on_screen:?} under the card {card:?}"
            );
        }
    }
}

/// A plate of two labelled objects (M486), `layers` layers high.
fn objects_file(layers: usize) -> String {
    let mut text = String::from("M486 T2\nM83\nG28\nG1 Z0.2 F600\n");
    for layer in 0..layers {
        let z = 0.2 + layer as f32 * 0.2;
        text.push_str(&format!("G1 Z{z:.2} F600\n"));
        for (id, name, x0) in [(0, "left", 60.0), (1, "right", 140.0)] {
            text.push_str(&format!("M486 S{id} A\"{name}\"\n"));
            for i in 0..10 {
                let y = 90.0 + i as f32 * 2.0;
                text.push_str(&format!(
                    "G1 X{x0} Y{y} F3000\nG1 X{} Y{y} E0.8 F1200\n",
                    x0 + 20.0
                ));
            }
            text.push_str("M486 S-1\n");
        }
    }
    text
}

#[test]
fn objects_are_listed_highlighted_and_skipped_while_printing() {
    let dir = temp_dir("objects");
    let path = dir.join("plate.gcode");
    std::fs::write(&path, objects_file(30)).expect("write");
    let mut options = slow_print_options(20.0);
    options.file = Some(path);
    let mut h = app(options, WINDOW);
    // Room for the cards beside each other.
    h.state_mut().set_side_panels(false, true);
    expect(&mut h, "the plate to be analyzed", analyzed);
    settle(&mut h);
    let viewer = h.state().viewer_tab().viewer().is_some();
    if !viewer {
        eprintln!("no GPU viewer on this machine: the 3D view's cards are not tested");
        return;
    }
    assert!(h.query_by_label("Objects · 2").is_some());
    // Hovering a row highlights its object in the view.
    let right = h.get_by_label("right").rect().center();
    h.hover_at(right);
    settle(&mut h);
    assert_eq!(
        h.state()
            .viewer_tab()
            .viewer()
            .and_then(|v| v.options().highlight_object),
        Some(1)
    );
    // No skipping before the print.
    assert!(
        h.query_by_label("Skip “right” for the rest of the print")
            .is_none()
    );
    connect(&mut h);
    start_print(&mut h);
    expect(&mut h, "the print to know its objects", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.objects == ["left", "right"])
    });
    settle(&mut h);
    h.get_by_label("Skip “right” for the rest of the print")
        .click();
    settle(&mut h);
    assert!(h.query_by_label("Skip “right”?").is_some());
    h.get_by_label("Skip object").click();
    expect(&mut h, "right to be skipped", |app| {
        app.state()
            .job
            .as_ref()
            .is_some_and(|j| j.excluded_objects == ["right"])
    });
    settle(&mut h);
    // The card and the job bar say so; the object can be printed again.
    assert!(h.query_all_by_label("1 SKIPPED").count() >= 2);
    assert!(h.query_by_label("Print “right” again").is_some());
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}
