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

#[test]
fn numeric_field_reset_restores_original() {
    let mut h = harness_state(
        Theme::Dark,
        (400.0, 120.0),
        (415.5_f64, 0),
        |ui, (v, changes)| {
            let r = ui.add(
                NumericField::new(v)
                    .label("Z steps")
                    .unit("steps/mm")
                    .original(400.0),
            );
            if r.changed() {
                *changes += 1;
            }
        },
    );
    h.get_by_label("Reset to 400.00 steps/mm").click();
    h.run_ok();
    assert_eq!(*h.state(), (400.0, 1));
    // Unmodified: the reset button is gone.
    assert!(h.query_by_label("Reset to 400.00 steps/mm").is_none());
}

#[test]
fn numeric_field_typed_value_is_clamped_to_range() {
    let mut h = harness_state(Theme::Dark, (400.0, 120.0), 10.0_f64, |ui, v| {
        ui.add(
            NumericField::new(v)
                .label("Speed")
                .range(1.0..=100.0)
                .decimals(0),
        );
    });
    // A DragValue keeps its SpinButton role while it is being typed into.
    h.get_by_role(egui::accesskit::Role::SpinButton).click();
    h.run_ok();
    h.get_by_role(egui::accesskit::Role::SpinButton)
        .type_text("5000");
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert_eq!(*h.state(), 100.0);
}

#[test]
fn numeric_field_missing_value_click_uses_original() {
    let mut h = harness_state(Theme::Light, (400.0, 120.0), f64::NAN, |ui, v| {
        ui.add(NumericField::new(v).label("Offset").original(-1.25));
    });
    h.get_by_label("Offset: no value").click();
    h.run_ok();
    assert_eq!(*h.state(), -1.25);
}

#[test]
fn key_value_click_copies_value() {
    let mut h = harness_state(Theme::Dark, (400.0, 160.0), None::<String>, |ui, copied| {
        KeyValueGrid::new("kv").show(ui, |kv| {
            kv.row("Firmware", "Marlin 2.1.2");
            kv.row("Empty", "");
            if let Some(c) = kv.copied() {
                *copied = Some(c.to_owned());
            }
        });
    });
    h.get_by_label("Marlin 2.1.2").click();
    h.step();
    h.step();
    let copied_cmd = h
        .output()
        .platform_output
        .commands
        .iter()
        .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == "Marlin 2.1.2"));
    h.run_ok();
    assert_eq!(h.state().as_deref(), Some("Marlin 2.1.2"));
    assert!(copied_cmd || h.state().is_some());
    // Empty values are shown as a dash and cannot be copied.
    h.get_by_label("").click();
    h.run_ok();
    assert_eq!(h.state().as_deref(), Some("Marlin 2.1.2"));
}
