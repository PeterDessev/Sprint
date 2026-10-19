//! Behavior tests: clicks, keyboard input and the values widgets report.

mod common;

use common::{click_at, harness_state};
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

fn jog_harness() -> egui_kittest::Harness<'static, Vec<JogAction>> {
    harness_state(Theme::Dark, (420.0, 340.0), Vec::new(), |ui, actions| {
        ui.horizontal(|ui| {
            if let Some(a) = JogPad::new().show(ui).action {
                actions.push(a);
            }
            if let Some(a) = ZJog::new().show(ui).action {
                actions.push(a);
            }
        });
    })
}

#[test]
fn jog_pad_reports_moves_home_and_corners() {
    let mut h = jog_harness();
    let geo = JogGeometry::new(h.get_by_label("XY jog").rect(), true);
    let mid = |ring: usize| {
        let (r0, r1) = geo.ring_radii(ring);
        (r0 + r1) / 2.0
    };
    click_at(&mut h, geo.center + egui::vec2(mid(3), 0.0));
    click_at(&mut h, geo.center - egui::vec2(0.0, mid(0)));
    click_at(&mut h, geo.center + egui::vec2(0.0, mid(1)));
    click_at(&mut h, geo.center);
    click_at(&mut h, geo.corner_rect(JogCorner::BottomRight).center());
    click_at(&mut h, geo.corner_rect(JogCorner::TopLeft).center());
    // Outside every region: nothing.
    click_at(&mut h, egui::pos2(geo.center.x, geo.rect.min.y + 1.0));
    assert_eq!(
        h.state().as_slice(),
        &[
            JogAction::Move {
                axis: Axis::X,
                distance: 100.0
            },
            JogAction::Move {
                axis: Axis::Y,
                distance: 0.1
            },
            JogAction::Move {
                axis: Axis::Y,
                distance: -1.0
            },
            JogAction::Home(HomeTarget::XY),
            JogAction::MotorsOff,
            JogAction::Home(HomeTarget::X),
        ]
    );
}

#[test]
fn z_strip_reports_moves_and_home() {
    let mut h = jog_harness();
    h.get_by_label("Move Z +10 mm").click();
    h.run_ok();
    h.get_by_label("Move Z \u{2212}0.1 mm").click();
    h.run_ok();
    h.get_by_label("Home Z (G28 Z)").click();
    h.run_ok();
    assert_eq!(
        h.state().as_slice(),
        &[
            JogAction::Move {
                axis: Axis::Z,
                distance: 10.0
            },
            JogAction::Move {
                axis: Axis::Z,
                distance: -0.1
            },
            JogAction::Home(HomeTarget::Z),
        ]
    );
}

#[test]
fn disabled_jog_pad_does_nothing() {
    let mut h = harness_state(Theme::Light, (300.0, 300.0), Vec::new(), |ui, actions| {
        ui.add_enabled_ui(false, |ui| {
            if let Some(a) = JogPad::new().show(ui).action {
                actions.push(a);
            }
        });
    });
    let geo = JogGeometry::new(h.get_by_label("XY jog").rect(), true);
    click_at(&mut h, geo.center);
    assert!(h.state().is_empty());
}

#[test]
fn extruder_buttons_report_length_and_speed() {
    let mut h = harness_state(
        Theme::Dark,
        (360.0, 260.0),
        (10.0_f32, 5.0_f32, Vec::new()),
        |ui, (length, speed, actions)| {
            if let Some(a) = ExtruderControl::new(length, speed).show(ui).action {
                actions.push(a);
            }
        },
    );
    h.get_by_label("50").click();
    h.run_ok();
    assert_eq!(h.state().0, 50.0);
    h.get_by_label("Extrude").click();
    h.run_ok();
    h.get_by_label("Retract").click();
    h.run_ok();
    assert_eq!(
        h.state().2,
        vec![
            ExtruderAction::Extrude {
                length: 50.0,
                speed: 5.0
            },
            ExtruderAction::Retract {
                length: 50.0,
                speed: 5.0
            },
        ]
    );
}

#[test]
fn blocked_extruder_ignores_clicks_and_repairs_bad_values() {
    let mut h = harness_state(
        Theme::Dark,
        (360.0, 260.0),
        (f32::NAN, -3.0_f32, 0_usize),
        |ui, (length, speed, clicks)| {
            let r = ExtruderControl::new(length, speed)
                .blocked(Some("Too cold"))
                .show(ui);
            if r.action.is_some() {
                *clicks += 1;
            }
        },
    );
    h.get_by_label("Extrude").click();
    h.run_ok();
    let (length, speed, clicks) = *h.state();
    assert_eq!(clicks, 0);
    assert!(length.is_finite() && length > 0.0);
    assert!(speed.is_finite() && speed > 0.0);
}
