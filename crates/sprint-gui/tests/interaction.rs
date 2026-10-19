//! Behavior tests: clicks, keyboard input and the values widgets report.

mod common;

use common::harness_state;
use egui_kittest::kittest::Queryable as _;
use sprint_gui::{theme::Theme, widgets::*};

#[test]
fn toggle_flips_on_click_and_reports_change() {
    let mut h = harness_state(
        Theme::Dark,
        (300.0, 120.0),
        (false, 0),
        |ui, (on, changes)| {
            if ui.add(Toggle::new(on).label("Autoscroll")).changed() {
                *changes += 1;
            }
        },
    );
    h.get_by_label("Autoscroll").click();
    h.run_ok();
    assert_eq!(*h.state(), (true, 1));
    h.get_by_label("Autoscroll").click();
    h.run_ok();
    assert_eq!(*h.state(), (false, 2));
}

#[test]
fn disabled_toggle_ignores_clicks() {
    let mut h = harness_state(Theme::Light, (300.0, 120.0), false, |ui, on| {
        ui.add_enabled(false, Toggle::new(on).label("Locked"));
    });
    h.get_by_label("Locked").click();
    h.run_ok();
    assert!(!*h.state());
}

#[test]
fn segmented_control_selects_clicked_segment() {
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum View {
        Iso,
        Layer,
        Code,
    }
    let options = [
        (View::Iso, "3D"),
        (View::Layer, "Layer"),
        (View::Code, "G-code"),
    ];
    let mut h = harness_state(
        Theme::Dark,
        (400.0, 120.0),
        (View::Iso, 0),
        move |ui, (view, changes)| {
            if ui.add(SegmentedControl::new(view, &options)).changed() {
                *changes += 1;
            }
        },
    );
    h.get_by_label("Layer").click();
    h.run_ok();
    assert_eq!(*h.state(), (View::Layer, 1));
    // Clicking the selected segment again is not a change.
    h.get_by_label("Layer").click();
    h.run_ok();
    assert_eq!(*h.state(), (View::Layer, 1));
    h.get_by_label("G-code").click();
    h.run_ok();
    assert_eq!(*h.state(), (View::Code, 2));
}

#[test]
fn segmented_control_tolerates_empty_and_unknown_values() {
    let empty: [(u8, &str); 0] = [];
    let mut h = harness_state(Theme::Dark, (300.0, 100.0), 7_u8, move |ui, value| {
        ui.add(SegmentedControl::new(value, &empty));
        // A value that matches no option shows no highlight.
        ui.add(SegmentedControl::new(value, &[(1, "One"), (2, "Two")]).id_salt("b"));
    });
    h.run_ok();
    assert_eq!(*h.state(), 7);
    h.get_by_label("Two").click();
    h.run_ok();
    assert_eq!(*h.state(), 2);
}
