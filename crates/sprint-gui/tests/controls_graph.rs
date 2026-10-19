//! The temperature graph tab: snapshots of the chart from synthetic
//! histories and its controls driven through the real UI.
//!
//! Regenerate the snapshots with
//! `UPDATE_SNAPSHOTS=1 cargo test -p sprint-gui --test controls_graph`.

mod controls_common;

use std::time::{Duration, Instant};

use controls_common::{Rig, graph, graph_state, hover, settle, snapshot, wait_for};
use egui::accesskit::Toggled;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};
use sprint_gui::theme::Theme;
use sprint_host::{ConnectionStatus, Heater, PrinterState, TempSample};

fn name(base: &str, theme: Theme) -> String {
    format!("controls_{base}_{}", theme.name().to_lowercase())
}

#[test]
fn graph_snapshots() {
    for theme in Theme::ALL {
        let mut h = graph(
            theme,
            (1000.0, 760.0),
            Rig::synthetic(graph_state(false, false)),
        );
        settle(&mut h);
        snapshot(&mut h, &name("graph", theme));
    }
}

#[test]
fn graph_multi_heater_snapshot() {
    let mut h = graph(
        Theme::Dark,
        (1000.0, 820.0),
        Rig::synthetic(graph_state(true, true)),
    );
    settle(&mut h);
    snapshot(&mut h, &name("graph_multi", Theme::Dark));
}

/// A narrow center tab: the view controls move to a second toolbar row
/// and the statistics columns tighten instead of overflowing.
#[test]
fn graph_narrow_snapshot() {
    let mut h = graph(
        Theme::Dark,
        (640.0, 720.0),
        Rig::synthetic(graph_state(true, false)),
    );
    settle(&mut h);
    let window = h.get_by_label("10 min").rect();
    let export = h.get_by_label("Export CSV").rect();
    assert!(window.min.y > export.max.y, "controls below the title row");
    for label in ["STATUS", "Export CSV", "Power"] {
        let rect = h.get_by_label(label).rect();
        assert!(rect.max.x <= 640.0 - 16.0, "{label} overflows: {rect:?}");
    }
    snapshot(&mut h, &name("graph_narrow", Theme::Dark));
}

#[test]
fn graph_empty_snapshots() {
    let mut h = graph(Theme::Dark, (800.0, 480.0), Rig::offline());
    settle(&mut h);
    snapshot(&mut h, &name("graph_empty", Theme::Dark));
}

/// The graph tab on ten minutes of synthetic history.
fn synthetic_graph() -> Harness<'static, Rig> {
    let mut h = graph(
        Theme::Dark,
        (1000.0, 760.0),
        Rig::synthetic(graph_state(false, false)),
    );
    settle(&mut h);
    h
}

fn follow_toggled(h: &Harness<'_, Rig>) -> Option<Toggled> {
    h.get_by_label("Follow").accesskit_node().toggled()
}

/// Drag with the primary button from `from` by `delta`, in small steps.
fn drag(h: &mut Harness<'_, Rig>, from: egui::Pos2, delta: egui::Vec2) {
    h.hover_at(from);
    h.step();
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for i in 1..=10 {
        h.hover_at(from + delta * (i as f32 / 10.0));
        h.step();
    }
    h.event(egui::Event::PointerButton {
        pos: from + delta,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    settle(h);
}

#[test]
fn window_choice_changes_the_statistics() {
    let mut h = synthetic_graph();
    // Ten minutes include the cold start of the hotend ...
    assert!(h.query_by_label("23.9").is_some());
    assert_eq!(h.query_all_by_label("Stable").count(), 2);
    h.get_by_label("2 min").click();
    settle(&mut h);
    // ... the last two minutes only the plateau at 230 °C.
    assert!(h.query_by_label("23.9").is_none());
    assert!(
        h.query_all_by_label_contains("229.").count()
            + h.query_all_by_label_contains("230.").count()
            >= 3,
        "min/max/mean of the hotend near 230"
    );
    h.get_by_label("All").click();
    settle(&mut h);
    assert!(h.query_by_label("23.9").is_some());
}

#[test]
fn dragging_stops_following_and_follow_returns() {
    let mut h = synthetic_graph();
    assert_eq!(follow_toggled(&h), Some(Toggled::True));
    h.get_by_label("2 min").click();
    settle(&mut h);
    assert!(h.query_by_label("23.9").is_none());
    // Drag the chart to the right: earlier readings come into view.
    drag(&mut h, egui::pos2(500.0, 250.0), egui::vec2(700.0, 0.0));
    assert_eq!(follow_toggled(&h), Some(Toggled::False));
    // The view stays where the user left it.
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(follow_toggled(&h), Some(Toggled::False));
    h.get_by_label("Follow").click();
    settle(&mut h);
    assert_eq!(follow_toggled(&h), Some(Toggled::True));
    assert!(h.query_by_label("23.9").is_none());
}

#[test]
fn hovering_reads_out_every_heater() {
    let mut h = synthetic_graph();
    hover(&mut h, egui::pos2(500.0, 250.0));
    let readouts = h.query_all_by_label_contains(" °C").count();
    assert!(
        readouts >= 2,
        "expected a hotend and a bed readout, got {readouts}"
    );
    assert!(h.query_by_label("→ 215").is_some(), "the hotend target");
    snapshot(&mut h, &name("graph_hover", Theme::Dark));
}

#[test]
fn power_plot_can_be_hidden() {
    let mut h = synthetic_graph();
    assert!(h.query_by_label("Heater power").is_some());
    h.get_by_label("Power").click();
    settle(&mut h);
    assert!(h.query_by_label("Heater power").is_none());
}

#[test]
fn empty_states_explain_what_is_missing() {
    let mut h = graph(Theme::Dark, (800.0, 480.0), Rig::offline());
    settle(&mut h);
    assert!(h.query_by_label("No temperature history").is_some());
    let connected = PrinterState {
        status: ConnectionStatus::Connected,
        ..PrinterState::default()
    };
    let mut h = graph(Theme::Light, (800.0, 480.0), Rig::synthetic(connected));
    settle(&mut h);
    assert!(
        h.query_by_label("Waiting for temperature reports")
            .is_some()
    );
}

#[test]
fn live_history_from_the_simulator() {
    let mut h = graph(Theme::Dark, (1000.0, 760.0), Rig::simulator());
    let printer = h.state().printer.clone();
    printer.set_temperature(Heater::Bed, 60.0);
    wait_for(&printer, "a few seconds of history", |s| {
        s.temp_history.len() >= 3
    });
    let end = Instant::now() + Duration::from_millis(200);
    while Instant::now() < end {
        h.step();
    }
    settle(&mut h);
    assert!(h.query_by_label("Live").is_some());
    // Each heater has a legend entry and a statistics row.
    assert_eq!(h.query_all_by_label("Hotend").count(), 2);
    assert_eq!(h.query_all_by_label("Bed").count(), 2);
    assert!(h.query_by_label("Export CSV").is_some());
}

#[test]
fn malformed_history_does_not_panic() {
    let mut state = graph_state(false, false);
    let nasty = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        -f32::MAX,
        0.0,
    ];
    for (i, v) in nasty.iter().enumerate() {
        state.temp_history.push_back(TempSample {
            t: 600.0 + i as f64,
            hotends: vec![(*v, *v); 1 + i * 5],
            bed: Some((*v, -*v)),
            chamber: (i % 2 == 0).then_some((*v, 40.0)),
            hotend_power: vec![*v; i],
            bed_power: Some(*v),
        });
    }
    // NaN, infinite and absurd times.
    for t in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300] {
        state.temp_history.push_back(TempSample {
            t,
            hotends: vec![(200.0, 210.0)],
            ..TempSample::default()
        });
    }
    let mut h = graph(Theme::Light, (900.0, 700.0), Rig::synthetic(state.clone()));
    settle(&mut h);
    hover(&mut h, egui::pos2(450.0, 250.0));
    drag(&mut h, egui::pos2(450.0, 250.0), egui::vec2(-200.0, 40.0));
    for label in ["2 min", "All", "Follow", "Power"] {
        h.get_by_label(label).click();
        settle(&mut h);
    }
    // Zooming far out keeps the grid finite.
    for _ in 0..40 {
        h.hover_at(egui::pos2(450.0, 250.0));
        h.event(egui::Event::Zoom(0.1));
        h.step();
    }
    settle(&mut h);
    assert!(h.query_by_label("Follow").is_some());
    // Time running backwards: the chart restarts from the newest run.
    for t in [10.0, 11.0, 12.0] {
        state.temp_history.push_back(TempSample {
            t,
            hotends: vec![(200.0, 210.0)],
            ..TempSample::default()
        });
    }
    let mut h = graph(Theme::Dark, (900.0, 700.0), Rig::synthetic(state));
    settle(&mut h);
    hover(&mut h, egui::pos2(450.0, 250.0));
    assert!(h.query_by_label("12 s recorded").is_none());
    assert!(h.query_by_label("2 s recorded").is_some());
}
