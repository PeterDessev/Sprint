//! The SD card tab: snapshots, sorting and filtering, the upload prompt,
//! and listing, printing, cancelling, deleting, uploading and releasing
//! against the simulator.

mod common;
mod insight_common;

use std::sync::Arc;
use std::time::Duration;

use common::{harness_state, snapshot};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use insight_common::*;
use sprint_gui::context::LoadedFile;
use sprint_gui::panels::SdCardTab;
use sprint_gui::panels::sd_card::SortKey;
use sprint_gui::theme::Theme;
use sprint_host::sprint_gcode::GcodeFile;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{JobInfo, JobKind, JobStatus, PrinterState};

type H<'a> = Harness<'a, Host<SdCardTab>>;

fn tab(theme: Theme, size: (f32, f32), state: PrinterState) -> H<'static> {
    let mut h = harness_state(
        theme,
        size,
        Host::<SdCardTab>::synthetic(state),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    h
}

fn loaded(name: &str, text: &str) -> LoadedFile {
    LoadedFile {
        path: None,
        file: Arc::new(GcodeFile::from_text(name, text)),
        toolpath: None,
        analysis_progress: None,
    }
}

/// A small but real print: home, heat-free moves.
fn small_gcode() -> String {
    let mut text = String::from("; test cube\nG28\nG90\nM83\n");
    for i in 0..60 {
        let x = 50 + (i % 10) * 5;
        text.push_str(&format!("G1 X{x} Y{} E0.4 F1800\n", 50 + i / 10));
    }
    text
}

fn sd_state() -> PrinterState {
    let mut state = demo_state();
    state.sd.card_present = Some(true);
    state
}

fn printing_state() -> PrinterState {
    let mut state = sd_state();
    state.sd.selected = Some("BENCHY~1.GCO".to_owned());
    let mut job = JobInfo::new(JobKind::SdPrint, "BENCHY~1.GCO", 0, 1_843_271);
    job.status = JobStatus::Printing;
    job.bytes_done = 774_174;
    job.progress = 0.42;
    job.elapsed = Duration::from_secs(47 * 60 + 12);
    job.eta = Some(Duration::from_secs(64 * 60));
    state.job = Some(job);
    state.sd.printing = Some((774_174, 1_843_271));
    state
}

fn type_in_focused(h: &mut H<'_>, text: &str) {
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_ok();
    h.key_press(egui::Key::Backspace);
    h.run_ok();
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .find(|n| n.is_focused())
        .expect("a focused text field")
        .type_text(text);
    h.run_ok();
}

/// Focus the upload prompt's name field (drawn after the table's filter).
fn focus_name_field(h: &mut H<'_>) {
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .last()
        .expect("name field")
        .click();
    h.run_ok();
}

#[test]
fn sd_card_snapshots() {
    let mut h = tab(Theme::Dark, (1000.0, 760.0), printing_state());
    assert!(h.query_by_label("Pause").is_some());
    assert!(h.query_by_label("Cancel print").is_some());
    assert!(h.query_by_label("SELECTED").is_some());
    // Nothing else prints, and nothing is deleted, while the card prints.
    for label in [
        "Print calibration_cube.gcode",
        "Delete calibration_cube.gcode",
        // M21 would remount the card under the running print.
        "Initialize",
        "Release",
    ] {
        assert!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            "{label}"
        );
    }
    snapshot(&mut h, "insight_sd_card_dark");

    let mut h = tab(Theme::Light, (1000.0, 760.0), sd_state());
    h.state_mut().file = Some(loaded("calibration cube v2.gcode", &small_gcode()));
    h.run_ok();
    h.get_by_label("Size").click();
    h.run_ok();
    h.event(egui::Event::PointerGone);
    h.run_ok();
    snapshot(&mut h, "insight_sd_card_light");
}

#[test]
fn sorting_and_filtering_the_table() {
    let mut h = tab(Theme::Dark, (1000.0, 760.0), sd_state());
    // The rows' labels read "long name (8.3 name), size", in table order.
    let order = |h: &H<'_>| -> Vec<String> {
        h.root()
            .children_recursive()
            .filter_map(|n| {
                let node = n.accesskit_node();
                node.label().or_else(|| node.value())
            })
            .filter(|l| l.contains(".GCO), "))
            .map(|l| l.split(" (").next().unwrap_or_default().to_owned())
            .collect()
    };
    assert_eq!(
        order(&h),
        [
            "benchy_0.2mm_PLA.gcode",
            "calibration_cube.gcode",
            "first layer test.gcode",
            "VASE.GCO"
        ]
    );
    h.get_by_label("Size").click();
    h.run_ok();
    assert_eq!(h.state().tab.sort().key, SortKey::Size);
    assert_eq!(
        order(&h),
        [
            "benchy_0.2mm_PLA.gcode",
            "VASE.GCO",
            "calibration_cube.gcode",
            "first layer test.gcode"
        ]
    );
    h.get_by_label("Size").click();
    h.run_ok();
    assert_eq!(order(&h)[0], "first layer test.gcode");
    h.state_mut().tab.set_filter("cal");
    h.run_ok();
    assert_eq!(order(&h), ["calibration_cube.gcode"]);
    h.state_mut().tab.set_filter("zzz");
    h.run_ok();
    assert!(h.query_by_label_contains("No file matches").is_some());
}

#[test]
fn upload_prompt_validates_names() {
    let mut state = sd_state();
    state.sd.files.push(sprint_host::sprint_proto::SdFile {
        name: "CALIBR~1.GCO".to_owned(),
        long_name: None,
        size: Some(1),
    });
    let mut h = tab(Theme::Light, (1000.0, 760.0), state);
    h.state_mut().file = Some(loaded("calibration cube v2.gcode", &small_gcode()));
    h.run_ok();
    h.get_by_label("Upload").click();
    h.run_ok();
    assert!(h.state().tab.upload_open());
    // The suggestion avoids the names on the card.
    assert!(h.query_by_label("Upload as CALIBR~2.GCO").is_some());
    focus_name_field(&mut h);
    type_in_focused(&mut h, "my cube.gcode");
    assert!(h.query_by_label("Spaces are not allowed.").is_some());
    type_in_focused(&mut h, "calibr~1.gco");
    assert!(
        h.query_by_label_contains("is already on the card and will be replaced")
            .is_some()
    );
    type_in_focused(&mut h, "cube.gco");
    h.event(egui::Event::PointerGone);
    h.run_ok();
    snapshot(&mut h, "insight_sd_card_upload_light");
    h.get_by_label("Cancel").click();
    h.run_ok();
    assert!(!h.state().tab.upload_open());
}

#[test]
fn sd_card_empty_states() {
    let h = tab(Theme::Dark, (900.0, 600.0), PrinterState::default());
    assert!(h.query_by_label("Not connected").is_some());
    let mut state = sd_state();
    state.sd.card_present = Some(false);
    let h = tab(Theme::Dark, (900.0, 600.0), state);
    assert!(h.query_by_label("No SD card").is_some());
    let mut state = sd_state();
    state.sd.files.clear();
    let h = tab(Theme::Dark, (900.0, 600.0), state);
    assert!(h.query_by_label("No files").is_some());
    let mut state = sd_state();
    state.sd.files.clear();
    state.sd.listing = true;
    let h = tab(Theme::Dark, (900.0, 600.0), state);
    assert!(h.query_by_label_contains("Reading the card").is_some());
    // Printing from the printer's display, without a host job.
    let mut state = sd_state();
    state.sd.printing = Some((10, 40));
    state.sd.selected = Some("VASE.GCO".into());
    let h = tab(Theme::Dark, (900.0, 600.0), state);
    assert!(h.query_by_label_contains("10 B of 40 B").is_some());
}

#[test]
fn a_cancelled_sd_print_does_not_keep_the_card_busy() {
    // The host keeps the cancelled print's last progress report.
    let mut state = printing_state();
    if let Some(job) = state.job.as_mut() {
        job.status = JobStatus::Cancelled;
    }
    let mut h = tab(Theme::Dark, (1000.0, 760.0), state);
    let disabled = |h: &H<'_>, label: &str| h.get_by_label(label).accesskit_node().is_disabled();
    for label in ["Release", "Initialize", "Delete calibration_cube.gcode"] {
        assert!(!disabled(&h, label), "{label}");
    }
    // A later host print does not turn the old report into a print
    // started on the printer.
    if let Some(s) = h.state_mut().state.as_mut() {
        let mut job = JobInfo::new(JobKind::HostPrint, "cube.gcode", 10, 100);
        job.status = JobStatus::Completed;
        s.job = Some(job);
    }
    h.run_ok();
    assert!(
        h.query_by_label_contains("Started on the printer")
            .is_none()
    );
    assert!(!disabled(&h, "Release"));
    // A new report is a print started on the printer's display.
    if let Some(s) = h.state_mut().state.as_mut() {
        s.sd.printing = Some((800_000, 1_843_271));
    }
    h.run_ok();
    assert!(
        h.query_by_label_contains("Started on the printer")
            .is_some()
    );
    assert!(disabled(&h, "Release"));
}

#[test]
fn a_print_started_on_the_printer_keeps_the_card_busy() {
    // Printing from the card without a host job: started on the display.
    let mut state = sd_state();
    state.sd.printing = Some((10_000, 734_120));
    state.sd.selected = Some("VASE.GCO".into());
    let mut h = tab(Theme::Dark, (1000.0, 760.0), state);
    h.state_mut().file = Some(loaded("cube.gcode", &small_gcode()));
    h.run_ok();
    assert!(
        h.query_by_label_contains("Started on the printer")
            .is_some()
    );
    // M23/M24 would swap the file under the running print; M28 would
    // write to the card it reads from.
    for label in [
        "Print calibration_cube.gcode",
        "Upload",
        "Delete calibration_cube.gcode",
        "Initialize",
        "Release",
    ] {
        assert!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            "{label}"
        );
    }
}

#[test]
fn a_released_card_offers_only_initializing() {
    let mut h = tab(Theme::Dark, (1000.0, 760.0), sd_state());
    h.state_mut().file = Some(loaded("cube.gcode", &small_gcode()));
    h.run_ok();
    h.get_by_label("Release").click();
    h.run_ok();
    assert!(h.query_by_label("Card released").is_some());
    // The firmware let go of the card: printing, deleting or writing to
    // it can only fail until it is initialized again.
    for label in [
        "Print calibration_cube.gcode",
        "Delete calibration_cube.gcode",
        "Upload",
        "Release",
    ] {
        assert!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            "{label}"
        );
    }
    assert!(!h.get_by_label("Initialize").accesskit_node().is_disabled());
    // Reading the list again mounts the card first (M21, then M20).
    h.get_by_label("Read the file list again (M20)").click();
    h.run_ok();
    assert!(h.query_by_label("Card released").is_none());
    assert!(
        !h.get_by_label("Print calibration_cube.gcode")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn the_cancel_question_goes_away_with_its_job() {
    let mut h = tab(Theme::Dark, (1000.0, 760.0), printing_state());
    h.get_by_label("Cancel print").click();
    h.run_ok();
    assert!(h.query_by_label("Cancel the SD print?").is_some());
    // The print ends (and another job may start) while the question is
    // open: "Cancel job" must not cancel whatever runs by then.
    if let Some(s) = h.state_mut().state.as_mut() {
        s.sd.printing = None;
        let mut job = JobInfo::new(JobKind::HostPrint, "cube.gcode", 10, 100);
        job.status = JobStatus::Printing;
        s.job = Some(job);
    }
    h.run_ok();
    assert!(h.query_by_label("Cancel the SD print?").is_none());
    assert!(h.query_by_label("Cancel job").is_none());
}

#[test]
fn odd_file_lists_never_panic() {
    let mut state = sd_state();
    state.sd.files = vec![
        sprint_host::sprint_proto::SdFile {
            name: String::new(),
            long_name: Some(String::new()),
            size: None,
        },
        sprint_host::sprint_proto::SdFile {
            name: "\u{1F600}\u{0}\t.GCO".to_owned(),
            long_name: Some("x".repeat(500)),
            size: Some(u64::MAX),
        },
    ];
    let mut job = JobInfo::new(JobKind::SdUpload, "", 0, 0);
    job.progress = f32::NAN;
    job.status = JobStatus::Failed("checksum storm".into());
    state.job = Some(job);
    let mut h = tab(Theme::Light, (420.0, 700.0), state);
    h.run_ok();
}

#[test]
fn listing_printing_and_cancelling_on_the_simulator() {
    // Timed moves so the SD print is still running when cancelled (the
    // card's file heats up first; M524 waits for that, as in Marlin).
    let printer = sim_printer(SimConfig {
        boot_delay_ms: 20,
        time_scale: 30.0,
        instant_moves: false,
        ..SimConfig::default()
    });
    clear_received(&printer);
    let mut h = harness_state(
        Theme::Dark,
        (1000.0, 760.0),
        Host::<SdCardTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    // The listing is read once by itself.
    wait_for_command(&printer, "M20");
    run_until(&mut h, "the file list", |s| s.sd.files.len() >= 3);
    assert!(h.query_by_label("Print calibration_cube.gcode").is_some());

    // Print, after confirming.
    h.get_by_label("Print calibration_cube.gcode").click();
    h.run_ok();
    assert!(h.query_by_label("Print calibration_cube.gcode?").is_some());
    clear_received(&printer);
    h.get_by_label("Start print").click();
    h.run_ok();
    assert!(wait_for_command(&printer, "M23").starts_with("M23 CALIBR"));
    wait_for_command(&printer, "M24");
    run_until(&mut h, "the SD print", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.kind == JobKind::SdPrint && j.status == JobStatus::Printing)
    });
    // Release is refused while printing from the card.
    assert!(h.get_by_label("Release").accesskit_node().is_disabled());
    h.get_by_label("Cancel print").click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Cancel job").click();
    h.run_ok();
    run_until(&mut h, "the end of the print", |s| {
        s.job.as_ref().is_some_and(|j| j.status.is_finished())
    });
    let status = printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone()));
    assert_eq!(status, Some(JobStatus::Cancelled));
    assert!(received(&printer, "M524"));
    // The card is free again.
    h.run_ok();
    assert!(!h.get_by_label("Release").accesskit_node().is_disabled());
    printer.disconnect();
}

#[test]
fn deleting_uploading_and_releasing_on_the_simulator() {
    let printer = sim_printer(fast_sim());
    let mut h = harness_state(
        Theme::Light,
        (1000.0, 900.0),
        Host::<SdCardTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    run_until(&mut h, "the file list", |s| s.sd.files.len() >= 3);

    // Delete asks first; Cancel keeps the file.
    h.get_by_label("Delete first layer test.gcode").click();
    h.run_ok();
    h.get_by_label("Cancel").click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Delete first layer test.gcode").click();
    h.run_ok();
    h.get_by_label("Delete").click();
    h.run_ok();
    assert!(wait_for_command(&printer, "M30").starts_with("M30 FIRSTL"));
    run_until(&mut h, "the file to go", |s| {
        !s.sd
            .files
            .iter()
            .any(|f| f.long_name.as_deref() == Some("first layer test.gcode"))
    });

    // Upload the loaded file and print it when done.
    h.state_mut().file = Some(loaded("my test cube.gcode", &small_gcode()));
    h.run_ok();
    h.get_by_label("Upload").click();
    h.run_ok();
    focus_name_field(&mut h);
    type_in_focused(&mut h, "test.gco");
    h.get_by_label("Print it when the upload finishes").click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Upload as TEST.GCO").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M28"), "M28 TEST.GCO");
    run_until(&mut h, "the upload", |s| {
        s.sd.files.iter().any(|f| f.name == "TEST.GCO")
    });
    // ...and the print starts by itself.
    assert_eq!(wait_for_command(&printer, "M23"), "M23 TEST.GCO");
    run_until(&mut h, "the end of the print", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.kind == JobKind::SdPrint && j.status.is_finished())
    });

    // "Print when done" only prints while the tab watches the upload end:
    // coming back to the tab later must not start a print out of the blue.
    h.get_by_label("Upload").click();
    h.run_ok();
    focus_name_field(&mut h);
    type_in_focused(&mut h, "later.gco");
    h.get_by_label("Print it when the upload finishes").click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Upload as LATER.GCO").click();
    // The click's frame starts the upload; then another tab is shown.
    h.step();
    h.state_mut().hidden = true;
    assert_eq!(wait_for_command(&printer, "M28"), "M28 LATER.GCO");
    run_until(&mut h, "the hidden upload", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.name == "LATER.GCO" && j.status == JobStatus::Completed)
    });
    for _ in 0..40 {
        h.step();
    }
    h.state_mut().hidden = false;
    h.run_ok();
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!received(&printer, "M23"), "no print started");
    assert!(
        h.state()
            .toasts
            .iter()
            .any(|t| t.title() == "LATER.GCO is on the card")
    );

    // Release and initialize the card.
    clear_received(&printer);
    h.get_by_label("Release").click();
    h.run_ok();
    wait_for_command(&printer, "M22");
    assert!(h.query_by_label("Card released").is_some());
    clear_received(&printer);
    h.get_by_label("Initialize card").click();
    h.run_ok();
    wait_for_command(&printer, "M21");
    wait_for_command(&printer, "M20");
    assert!(h.query_by_label("Card released").is_none());
    printer.disconnect();
}
