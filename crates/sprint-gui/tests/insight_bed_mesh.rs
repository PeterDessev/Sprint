//! The Bed mesh tab: snapshots, the screw helper, odd meshes, and probing,
//! leveling and fade height against the simulator.

mod common;
mod insight_common;

use common::{harness_state, snapshot};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use insight_common::*;
use sprint_gui::panels::BedMeshTab;
use sprint_gui::theme::Theme;
use sprint_host::PrinterState;
use sprint_host::sprint_proto::BedMesh;

type H<'a> = Harness<'a, Host<BedMeshTab>>;

fn tab(theme: Theme, size: (f32, f32), state: PrinterState) -> H<'static> {
    let mut h = harness_state(
        theme,
        size,
        Host::<BedMeshTab>::synthetic(state),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    h
}

fn pointer_away(h: &mut H<'_>) {
    h.event(egui::Event::PointerGone);
    h.run_ok();
}

#[test]
fn bed_mesh_snapshots() {
    for theme in Theme::ALL {
        let mut h = tab(theme, (1180.0, 1240.0), demo_state());
        assert!(h.query_by_label("heatmap").is_some());
        assert!(h.query_by_label("Surface preview").is_some());
        assert!(h.query_by_label("Bed plan").is_some());
        assert!(h.query_by_label("Leveling on").is_some());
        snapshot(&mut h, &themed("insight_bed_mesh", theme));
    }
}

#[test]
fn bed_mesh_narrow_layout() {
    let mut h = tab(Theme::Dark, (600.0, 2080.0), demo_state());
    assert!(h.query_by_label("Bed plan").is_some());
    assert!(h.query_by_label("Surface preview").is_some());
    snapshot(&mut h, "insight_bed_mesh_narrow_dark");
}

#[test]
fn a_narrow_header_keeps_its_title_readable() {
    let width = 380.0;
    let h = tab(Theme::Dark, (width, 2400.0), demo_state());
    // The header's buttons do not fit next to the title: they go below it
    // instead of over it.
    let title = h.get_by_label("Bed mesh").rect();
    for label in ["Probe bed", "Load", "Leveling on"] {
        let control = h.get_by_label(label).rect();
        assert!(
            control.min.y >= title.max.y,
            "{label} at {control:?} covers the title at {title:?}"
        );
    }
    // A wide tab keeps the buttons next to the title.
    let h = tab(Theme::Dark, (1180.0, 1240.0), demo_state());
    let title = h.get_by_label("Bed mesh").rect();
    assert!(h.get_by_label("Probe bed").rect().center().y < title.max.y);
}

#[test]
fn a_narrow_tab_keeps_the_screw_card_inside() {
    let width = 380.0;
    let mut h = tab(Theme::Dark, (width, 2400.0), demo_state());
    // The direction switch used to push the screw card (bed plan, list,
    // surface) past the tab's edge.
    assert_eq!(overflowing(&h, width - 24.0), Vec::<String>::new());
    h.get_by_label("Clockwise raises").click();
    h.run_ok();
    assert!(
        h.query_by_label_contains("Front right: Turn counter-clockwise")
            .is_some()
    );
}

#[test]
fn screw_helper_follows_the_reference_and_thread() {
    // Tall enough for the screw editor.
    let mut h = tab(Theme::Light, (1180.0, 1700.0), demo_state());
    // The demo bed is tilted up towards the right and the front.
    assert!(h.query_by_label_contains("Front left: Reference").is_some());
    assert!(
        h.query_by_label_contains("Front right: Turn clockwise")
            .is_some(),
        "front right is high"
    );
    // Choosing the front right as the reference flips the advice.
    h.get_by_label_contains("Front right:").click();
    h.run_ok();
    assert_eq!(h.state().tab.screws().reference(), 1);
    assert!(
        h.query_by_label_contains("Front right: Reference")
            .is_some()
    );
    assert!(
        h.query_by_label_contains("Front left: Turn counter-clockwise")
            .is_some()
    );
    // A coarser thread needs fewer turns for the same height.
    h.get_by_label("M5 · 0.8").click();
    h.run_ok();
    assert_eq!(h.state().tab.screws().pitch(), 0.8);
    // Clockwise raising the bed swaps every direction.
    h.get_by_label("Clockwise raises it").click();
    h.run_ok();
    assert!(
        h.query_by_label_contains("Front left: Turn clockwise")
            .is_some()
    );
    // The editor adds and removes screws, down to three.
    h.get_by_label("Edit screw positions").click();
    h.run_ok();
    h.get_by_label("Add screw").click();
    h.run_ok();
    assert_eq!(h.state().tab.screws().screws().len(), 5);
    for _ in 0..3 {
        if let Some(remove) = h.get_all_by_label("Remove this screw").last() {
            remove.click();
        }
        h.run_ok();
    }
    assert_eq!(h.state().tab.screws().screws().len(), 3);
    h.get_by_label("Four corners").click();
    h.run_ok();
    assert_eq!(h.state().tab.screws().screws().len(), 4);
    pointer_away(&mut h);
}

#[test]
fn the_screw_setup_is_remembered_per_printer() {
    let mut h = tab(Theme::Light, (1180.0, 1700.0), demo_state());
    h.get_by_label("M5 · 0.8").click();
    h.run_ok();
    h.get_by_label("Clockwise raises it").click();
    h.run_ok();
    h.get_by_label_contains("Back right:").click();
    h.run_ok();
    // The app restarts: a fresh tab finds the setup in egui's persisted
    // memory (saved with the window state).
    h.state_mut().tab = BedMeshTab::default();
    h.run_ok();
    let screws = h.state().tab.screws();
    assert_eq!(screws.pitch(), 0.8);
    assert_eq!(screws.reference(), 2);
    assert!(
        h.query_by_label_contains("Front left: Turn clockwise")
            .is_some(),
        "clockwise raises the bed"
    );
    // Another printer profile has its own setup.
    h.state_mut().config.active_mut().name = "Second printer".into();
    h.run_ok();
    assert_eq!(h.state().tab.screws().pitch(), 0.5);
    assert_eq!(h.state().tab.screws().reference(), 0);
}

#[test]
fn surface_turns_when_dragged() {
    let mut h = tab(Theme::Dark, (1180.0, 1060.0), demo_state());
    let before = h.state().tab.azimuth();
    let rect = h.get_by_label("Surface preview").rect();
    let start = rect.center();
    h.drag_at(start);
    h.run_ok();
    for step in 1..=5 {
        h.hover_at(start + egui::vec2(12.0 * step as f32, 0.0));
        h.run_ok();
    }
    h.drop_at(start + egui::vec2(60.0, 0.0));
    h.run_ok();
    assert!(
        (h.state().tab.azimuth() - before).abs() > 5.0,
        "{} -> {}",
        before,
        h.state().tab.azimuth()
    );
}

#[test]
fn bed_mesh_empty_states() {
    let h = tab(Theme::Dark, (900.0, 600.0), PrinterState::default());
    assert!(h.query_by_label("Not connected").is_some());
    let mut connected = demo_state();
    connected.mesh = None;
    let mut h = tab(Theme::Dark, (900.0, 600.0), connected);
    assert!(h.query_by_label("No bed mesh").is_some());
    // Probing asks first.
    h.get_all_by_label("Probe bed")
        .last()
        .expect("button")
        .click();
    h.run_ok();
    assert!(h.query_by_label("Probe the bed?").is_some());
    h.get_by_label("Cancel").click();
    h.run_ok();
    assert!(h.query_by_label("Probe the bed?").is_none());
    assert!(!h.state().tab.is_probing());
    // A firmware without AUTOLEVEL gets a hint.
    let mut manual = demo_state();
    manual.mesh = None;
    for (name, on) in &mut manual.capabilities {
        if name == "AUTOLEVEL" {
            *on = false;
        }
    }
    let h = tab(Theme::Dark, (900.0, 600.0), manual);
    assert!(
        h.query_by_label_contains("reports no automatic bed leveling")
            .is_some()
    );
}

#[test]
fn a_probe_without_a_new_mesh_ends() {
    let mut h = tab(Theme::Dark, (1180.0, 1060.0), demo_state());
    h.get_by_label("Probe bed").click();
    h.run_ok();
    h.get_by_label("Home and probe").click();
    h.run_ok();
    assert!(h.state().tab.is_probing());
    // G28, G29 and M420 V go out and are answered (the demo link has a
    // command in flight)...
    std::thread::sleep(std::time::Duration::from_millis(1100));
    if let Some(s) = h.state_mut().state.as_mut() {
        // The connection's clock runs on.
        s.link.connected_secs += 1.1;
    }
    h.run_ok();
    assert!(h.state().tab.is_probing(), "still waiting for the answers");
    // ...but G29 failed: the same mesh comes back. Probing must not block
    // the tab for the 15-minute timeout.
    if let Some(s) = h.state_mut().state.as_mut() {
        s.link.in_flight = 0;
        s.link.queue_len = 0;
    }
    h.run_ok();
    assert!(!h.state().tab.is_probing());
    assert!(h.state().toasts.iter().any(|t| t.title() == "No new mesh"));
    assert!(!h.get_by_label("Probe bed").accesskit_node().is_disabled());
}

#[test]
fn leveling_stays_put_during_a_job() {
    let mut state = demo_state();
    let mut job = sprint_host::JobInfo::new(sprint_host::JobKind::HostPrint, "cube.gcode", 10, 100);
    job.status = sprint_host::JobStatus::Printing;
    state.job = Some(job);
    let mut h = tab(Theme::Dark, (1180.0, 1060.0), state);
    let disabled = |h: &H<'_>, label: &str| h.get_by_label(label).accesskit_node().is_disabled();
    assert!(disabled(&h, "Apply the mesh while printing"));
    assert!(disabled(&h, "Probe bed"));
    // A new fade height would toggle leveling under the print too.
    h.get_by_role(egui::accesskit::Role::SpinButton).click();
    h.run_ok();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_ok();
    h.key_press(egui::Key::Backspace);
    h.run_ok();
    h.get_by_role(egui::accesskit::Role::SpinButton)
        .type_text("5");
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert!(disabled(&h, "Set"));
}

#[test]
fn saving_needs_an_eeprom() {
    let h = tab(Theme::Dark, (1180.0, 1060.0), without_eeprom(demo_state()));
    assert!(
        h.get_by_label("Save to EEPROM")
            .accesskit_node()
            .is_disabled()
    );
    let h = tab(Theme::Dark, (1180.0, 1060.0), demo_state());
    assert!(
        !h.get_by_label("Save to EEPROM")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn odd_meshes_never_panic() {
    let meshes = vec![
        BedMesh::from_rows(vec![vec![0.1]], "bilinear"),
        BedMesh::from_rows(vec![vec![f32::NAN; 4]; 4], "bilinear"),
        BedMesh::from_rows(
            vec![
                vec![0.1, -0.2, 0.3],
                vec![0.05],
                vec![],
                vec![0.0, f32::NAN, 0.2],
            ],
            "mbl",
        ),
        BedMesh::from_rows(vec![vec![1e30, -1e30], vec![f32::INFINITY, 0.0]], "ubl"),
        BedMesh::from_rows(vec![vec![0.0; 2]; 2], ""),
        BedMesh::from_rows(
            (0..30)
                .map(|r| (0..30).map(|c| ((r * c) as f32).sin() * 0.2).collect())
                .collect(),
            "klipper_mesh",
        ),
        {
            let mut m = BedMesh::from_rows(vec![vec![0.0, 0.1], vec![0.2, 0.3]], "bilinear");
            m.x_positions = Some(vec![200.0, 10.0]);
            m.y_positions = Some(vec![f32::NAN, 5.0]);
            m
        },
    ];
    for mesh in meshes {
        let mut state = demo_state();
        state.mesh = Some(mesh);
        let mut h = tab(Theme::Dark, (1000.0, 900.0), state);
        h.run_ok();
    }
    // A zero-sized bed in the profile falls back to sane screws.
    let mut h = tab(Theme::Light, (700.0, 900.0), demo_state());
    h.state_mut().config.active_mut().bed_size = [0.0, f32::NAN];
    h.run_ok();
    assert_eq!(h.state().tab.screws().screws().len(), 4);
}

#[test]
fn probing_and_leveling_on_the_simulator() {
    let printer = sim_printer(fast_sim());
    clear_received(&printer);
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 1060.0),
        Host::<BedMeshTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    // The stored mesh is requested once; the simulator has none yet.
    wait_for_command(&printer, "M420 V");
    h.run_ok();
    assert!(h.query_by_label("No bed mesh").is_some());

    // Probe, after confirming.
    h.get_all_by_label("Probe bed")
        .last()
        .expect("button")
        .click();
    h.run_ok();
    clear_received(&printer);
    h.get_by_label("Home and probe").click();
    h.run_ok();
    wait_for_command(&printer, "G29");
    run_until(&mut h, "the probed mesh", |s| s.mesh.is_some());
    h.run_ok();
    assert!(!h.state().tab.is_probing());
    assert!(h.state().toasts.iter().any(|t| t.title() == "Bed probed"));
    assert!(h.query_by_label("heatmap").is_some());
    assert!(h.query_by_label_contains("Front left: Reference").is_some());

    // Leveling is on after G29; switch it off.
    run_until(&mut h, "the leveling state", |s| {
        s.settings.value("M420", 'S') == Some(1.0)
    });
    clear_received(&printer);
    h.get_by_label("Apply the mesh while printing").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M420 S"), "M420 S0 Z10");
    run_until(&mut h, "leveling off", |s| {
        s.settings.value("M420", 'S') == Some(0.0)
    });

    // A new fade height.
    clear_received(&printer);
    // The fade height is the only number field while the screw editor is
    // closed.
    h.get_by_role(egui::accesskit::Role::SpinButton).click();
    h.run_ok();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_ok();
    h.key_press(egui::Key::Backspace);
    h.run_ok();
    h.get_by_role(egui::accesskit::Role::SpinButton)
        .type_text("5");
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    h.get_by_label("Set").click();
    h.run_ok();
    assert_eq!(wait_for_command(&printer, "M420 S"), "M420 S0 Z5");
    run_until(&mut h, "the new fade height", |s| {
        s.settings.value("M420", 'Z') == Some(5.0)
    });

    // Save and reload.
    clear_received(&printer);
    h.get_by_label("Save to EEPROM").click();
    h.run_ok();
    wait_for_command(&printer, "M500");
    clear_received(&printer);
    h.get_by_label("Load").click();
    h.run_ok();
    wait_for_command(&printer, "M420 V");
    printer.disconnect();
}

#[test]
fn the_stored_mesh_loads_again_after_a_hidden_reconnect() {
    let printer = sim_printer(fast_sim());
    let mut h = harness_state(
        Theme::Dark,
        (1180.0, 1060.0),
        Host::<BedMeshTab>::live(printer.clone()),
        |ui, host| host.frame(ui),
    );
    h.run_ok();
    wait_for_command(&printer, "M420 V");
    // Another tab is shown while the printer reconnects...
    h.state_mut().hidden = true;
    h.run_ok();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    printer.disconnect();
    let options = sprint_host::ConnectOptions {
        ok_timeout_secs: 3.0,
        ..sprint_host::ConnectOptions::default()
    };
    printer
        .connect(sprint_host::ConnectTarget::Simulator(fast_sim()), options)
        .expect("reconnect to the simulator");
    wait_for(&printer, "the new connection", |s| {
        s.is_connected() && s.settings_loaded
    });
    // ...and the new connection's clock has run for a while (longer than
    // the old one had when the tab last saw it).
    std::thread::sleep(std::time::Duration::from_millis(1500));
    // Coming back to the tab loads the new connection's mesh.
    h.state_mut().hidden = false;
    step_until_command(&mut h, "M420 V");
    printer.disconnect();
}
