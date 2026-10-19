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

fn temperature_harness(target: f32) -> egui_kittest::Harness<'static, Vec<TemperatureAction>> {
    harness_state(
        Theme::Dark,
        (380.0, 220.0),
        Vec::new(),
        move |ui, actions| {
            let r = TemperatureCard::new("Hotend", sprint_gui::theme::HeaterKind::Hotend)
                .current(150.0)
                .target(target)
                .presets(&[("PLA", 210.0), ("Silly", 999.0)])
                .show(ui);
            if let Some(a) = r.action {
                actions.push(a);
            }
        },
    )
}

fn type_target(
    h: &mut egui_kittest::Harness<'_, Vec<TemperatureAction>>,
    text: &str,
    key: egui::Key,
) {
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_ok();
    // Replace whatever the field showed.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_ok();
    h.key_press(egui::Key::Backspace);
    h.run_ok();
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text(text);
    h.run_ok();
    h.key_press(key);
    h.run_ok();
}

#[test]
fn temperature_target_applies_on_enter_only() {
    let mut h = temperature_harness(200.0);
    type_target(&mut h, "215", egui::Key::Escape);
    assert!(h.state().is_empty(), "Escape must not apply");
    type_target(&mut h, "hot", egui::Key::Enter);
    assert!(h.state().is_empty(), "invalid text must not apply");
    type_target(&mut h, "215.5", egui::Key::Enter);
    assert_eq!(h.state().as_slice(), &[TemperatureAction::SetTarget(215.5)]);
    // The field shows the real target again afterwards.
    assert_eq!(
        h.get_by_role(egui::accesskit::Role::TextInput)
            .value()
            .as_deref(),
        Some("200")
    );
}

#[test]
fn temperature_target_is_clamped_and_empty_means_off() {
    let mut h = temperature_harness(200.0);
    type_target(&mut h, "5000", egui::Key::Enter);
    type_target(&mut h, "", egui::Key::Enter);
    type_target(&mut h, "-20", egui::Key::Enter);
    assert_eq!(
        h.state().as_slice(),
        &[
            TemperatureAction::SetTarget(300.0),
            TemperatureAction::SetTarget(0.0),
            TemperatureAction::SetTarget(0.0),
        ]
    );
}

#[test]
fn temperature_presets_and_off() {
    let mut h = temperature_harness(200.0);
    h.get_by_label("Presets").click();
    h.run_ok();
    h.get_by_label_contains("PLA").click();
    h.run_ok();
    h.get_by_label("Presets").click();
    h.run_ok();
    h.get_by_label_contains("Silly").click();
    h.run_ok();
    h.get_by_label("Turn hotend off").click();
    h.run_ok();
    assert_eq!(
        h.state().as_slice(),
        &[
            TemperatureAction::SetTarget(210.0),
            TemperatureAction::SetTarget(300.0),
            TemperatureAction::Off,
        ]
    );
}

#[test]
fn temperature_off_button_is_disabled_when_off() {
    let mut h = temperature_harness(0.0);
    h.get_by_label("Turn hotend off").click();
    h.run_ok();
    assert!(h.state().is_empty());
}

fn dialog_harness(danger: bool) -> egui_kittest::Harness<'static, Vec<DialogResult>> {
    harness_state(
        Theme::Dark,
        (520.0, 320.0),
        Vec::new(),
        move |ui, answers| {
            if answers.is_empty()
                && let Some(answer) = ConfirmDialog::new("q", "Proceed?")
                    .confirm_label("Go")
                    .danger(danger)
                    .show(ui.ctx())
            {
                answers.push(answer);
            }
        },
    )
}

#[test]
fn dialog_buttons_answer() {
    let mut h = dialog_harness(false);
    h.get_by_label("Go").click();
    h.run_ok();
    assert_eq!(h.state().as_slice(), &[DialogResult::Confirmed]);
    let mut h = dialog_harness(true);
    h.get_by_label("Cancel").click();
    h.run_ok();
    assert_eq!(h.state().as_slice(), &[DialogResult::Cancelled]);
}

#[test]
fn dialog_keyboard() {
    let mut h = dialog_harness(false);
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert_eq!(h.state().as_slice(), &[DialogResult::Confirmed]);
    let mut h = dialog_harness(false);
    h.key_press(egui::Key::Escape);
    h.run_ok();
    assert_eq!(h.state().as_slice(), &[DialogResult::Cancelled]);
    // Enter never confirms a danger dialog (Cancel has the focus).
    let mut h = dialog_harness(true);
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert_ne!(h.state().first(), Some(&DialogResult::Confirmed));
}

#[test]
fn dialog_backdrop_click_cancels() {
    let mut h = dialog_harness(true);
    click_at(&mut h, egui::pos2(5.0, 5.0));
    h.run_ok();
    assert_eq!(h.state().as_slice(), &[DialogResult::Cancelled]);
}

#[test]
fn toasts_dismiss_and_expire() {
    let mut toasts = Toasts::new();
    toasts.info("Short").duration(1.0);
    toasts.error("Sticky").sticky();
    let mut h = harness_state(Theme::Light, (520.0, 320.0), toasts, |ui, toasts| {
        toasts.show(ui.ctx());
    });
    // The harness settles for many frames: the timed toast is gone.
    h.run_ok();
    assert_eq!(h.state().len(), 1);
    assert!(h.query_by_label("Short").is_none());
    h.get_by_label("Dismiss").click();
    h.run_ok();
    assert!(h.state().is_empty());
}

#[test]
fn heatmap_reports_clicked_cell_with_row_zero_at_bottom() {
    let grid = vec![vec![0.1, 0.2, 0.3], vec![-0.1, f32::NAN, -0.3]];
    let mut h = harness_state(
        Theme::Dark,
        (300.0, 300.0),
        Vec::new(),
        move |ui, clicks| {
            let r = Heatmap::new(&grid).legend(false).max_cell(40.0).show(ui);
            if let Some(c) = r.clicked {
                clicks.push((c.row, c.col, c.value));
            }
        },
    );
    let area = h.get_by_label("heatmap").rect();
    let cell = area.width() / 3.0;
    // Top-left on screen is the last row.
    click_at(&mut h, area.min + egui::vec2(0.5 * cell, 0.5 * cell));
    // Bottom-right is row 0, last column.
    click_at(&mut h, area.min + egui::vec2(2.5 * cell, 1.5 * cell));
    // The missing cell reports NaN.
    click_at(&mut h, area.min + egui::vec2(1.5 * cell, 0.5 * cell));
    let clicks = h.state();
    assert_eq!(clicks.len(), 3);
    assert_eq!((clicks[0].0, clicks[0].1, clicks[0].2), (1, 0, -0.1));
    assert_eq!((clicks[1].0, clicks[1].1, clicks[1].2), (0, 2, 0.3));
    assert_eq!((clicks[2].0, clicks[2].1), (1, 1));
    assert!(clicks[2].2.is_nan());
}

#[test]
fn widgets_in_sibling_columns_do_not_share_ids() {
    // `ui.columns` children share a stable id; widgets must still be
    // independent (a clash would make one click hit both copies).
    let mut h = harness_state(
        Theme::Dark,
        (720.0, 420.0),
        (Vec::new(), Vec::new()),
        |ui, (jogs, temps)| {
            ui.columns(2, |cols| {
                for (i, col) in cols.iter_mut().enumerate() {
                    if let Some(a) = ZJog::new().height(200.0).show(col).action {
                        jogs.push((i, a));
                    }
                    let r = TemperatureCard::new("Hotend", sprint_gui::theme::HeaterKind::Hotend)
                        .current(20.0)
                        .target(0.0)
                        .show(col);
                    if let Some(a) = r.action {
                        temps.push((i, a));
                    }
                }
            });
        },
    );
    let nodes: Vec<egui::Rect> = h
        .get_all_by_label("Move Z +10 mm")
        .map(|n| n.rect())
        .collect();
    assert_eq!(nodes.len(), 2);
    let right = nodes
        .iter()
        .copied()
        .max_by(|a, b| a.min.x.total_cmp(&b.min.x));
    if let Some(right) = right {
        click_at(&mut h, right.center());
    }
    assert_eq!(
        h.state().0,
        vec![(
            1,
            JogAction::Move {
                axis: Axis::Z,
                distance: 10.0
            }
        )]
    );
    // Typing into the left card's target only affects the left card.
    let edits: Vec<egui::Rect> = h
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .map(|n| n.rect())
        .collect();
    assert_eq!(edits.len(), 2);
    let left = edits
        .iter()
        .copied()
        .min_by(|a, b| a.min.x.total_cmp(&b.min.x));
    if let Some(left) = left {
        click_at(&mut h, left.center());
    }
    h.event(egui::Event::Text("200".to_owned()));
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert_eq!(h.state().1, vec![(0, TemperatureAction::SetTarget(200.0))]);
}

#[test]
fn jog_pad_moves_by_the_ring_that_was_pressed() {
    // Press near the outer edge of the 10 mm ring and release a few points
    // further out (still a click): the move is 10 mm, not 100 mm.
    let mut h = jog_harness();
    let geo = JogGeometry::new(h.get_by_label("XY jog").rect(), true);
    let (_, edge) = geo.ring_radii(2);
    let slide = |h: &mut egui_kittest::Harness<'_, Vec<JogAction>>,
                 press: egui::Pos2,
                 release: egui::Pos2| {
        h.hover_at(press);
        h.step();
        h.event(egui::Event::PointerButton {
            pos: press,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.hover_at(release);
        h.step();
        h.event(egui::Event::PointerButton {
            pos: release,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    };
    slide(
        &mut h,
        geo.center + egui::vec2(edge - 2.0, 0.0),
        geo.center + egui::vec2(edge + 2.0, 0.0),
    );
    // A press in the dead zone outside the rings does nothing, even when
    // released over the outer ring.
    slide(
        &mut h,
        geo.center - egui::vec2(0.0, geo.outer_radius + 2.0),
        geo.center - egui::vec2(0.0, geo.outer_radius - 2.0),
    );
    assert_eq!(
        h.state().as_slice(),
        &[JogAction::Move {
            axis: Axis::X,
            distance: 10.0
        }]
    );
}

#[test]
fn widget_state_survives_layout_changes_above() {
    // A banner appearing above the parent `Ui` must not reset a collapsed
    // card or throw away a target that is being typed.
    let mut h = harness_state(
        Theme::Dark,
        (420.0, 420.0),
        (false, Vec::new()),
        |ui, (banner, actions)| {
            if *banner {
                ui.label("Banner");
            }
            ui.vertical(|ui| {
                Card::new("Macros").collapsible(true).show(ui, |ui| {
                    ui.label("Body text");
                });
                let r = TemperatureCard::new("Bed", sprint_gui::theme::HeaterKind::Bed)
                    .current(25.0)
                    .show(ui);
                if let Some(a) = r.action {
                    actions.push(a);
                }
            });
        },
    );
    assert!(h.query_by_label("Body text").is_some());
    h.get_by_label("Macros").click();
    h.run_ok();
    assert!(h.query_by_label("Body text").is_none(), "card collapsed");
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_ok();
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text("60");
    h.run_ok();
    h.state_mut().0 = true;
    h.run_ok();
    assert!(h.query_by_label("Banner").is_some());
    assert!(
        h.query_by_label("Body text").is_none(),
        "card stays collapsed"
    );
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert_eq!(h.state().1, vec![TemperatureAction::SetTarget(60.0)]);
}

#[test]
fn extruder_never_reports_beyond_its_limits() {
    let mut h = harness_state(
        Theme::Dark,
        (360.0, 260.0),
        (2000.0_f32, 0.01_f32, Vec::new()),
        |ui, (length, speed, actions)| {
            if let Some(a) = ExtruderControl::new(length, speed)
                .limits(100.0, 20.0)
                .show(ui)
                .action
            {
                actions.push(a);
            }
        },
    );
    h.get_by_label("Extrude").click();
    h.run_ok();
    assert_eq!(
        h.state().2,
        vec![ExtruderAction::Extrude {
            length: 100.0,
            speed: 0.1
        }]
    );
}

#[test]
fn key_value_custom_row_stays_in_the_value_column() {
    let mut h = harness_state(Theme::Dark, (520.0, 160.0), (), |ui, ()| {
        KeyValueGrid::new("kv").show(ui, |kv| {
            kv.row_ui("Extruders", |ui| {
                ui.label("Alpha");
                ui.label("Beta");
            });
            kv.row("Firmware", "A much longer firmware value");
        });
    });
    h.run_ok();
    let beta = h.get_by_label("Beta").rect();
    let long = h.get_by_label("A much longer firmware value").rect();
    assert!(
        beta.min.x < long.max.x,
        "Beta spilled into a third column: {beta:?} vs {long:?}"
    );
}

#[test]
fn long_card_title_is_cut_short_with_a_tooltip() {
    let title = "Part cooling fan and the auxiliary chamber fan";
    let mut h = harness_state(Theme::Dark, (240.0, 160.0), (), move |ui, ()| {
        card(
            ui,
            sprint_gui::icons::FAN,
            title,
            |ui| {
                ui.add(
                    IconButton::new(sprint_gui::icons::OFF)
                        .ghost()
                        .small()
                        .tooltip("Fan off"),
                );
            },
            |ui| {
                ui.label("100%");
            },
        );
    });
    assert!(h.query_by_label(title).is_none());
    // The header control stays where it was, inside the card.
    let off = h.get_by_label("Fan off").rect();
    assert!(off.max.x <= 240.0);
    // Hovering the cut title shows it in full.
    common::hover(&mut h, egui::pos2(off.min.x - 40.0, off.center().y));
    assert!(h.query_by_label(title).is_some(), "full title tooltip");
}

#[test]
fn estop_fires_once_on_press_even_when_the_pointer_slides() {
    let mut h = harness_state(Theme::Dark, (360.0, 160.0), 0_usize, |ui, fired| {
        ui.horizontal(|ui| {
            if ui.add(EStopButton::new()).changed() {
                *fired += 1;
            }
            if ui
                .add(
                    EStopButton::new()
                        .label("STOP")
                        .style(EStopStyle::Octagon(72.0)),
                )
                .changed()
            {
                *fired += 1;
            }
            ui.add_enabled(false, EStopButton::new().label("OFF"));
        });
    });
    let badge = h.get_by_label("E-STOP").rect();
    // A plain click fires once: on the press, not again on the release.
    click_at(&mut h, badge.center());
    assert_eq!(*h.state(), 1);
    // A hurried press that slides 30 pt before letting go is a drag to
    // egui, never a click, but it must still stop the printer.
    let start = badge.left_center() + egui::vec2(16.0, 0.0);
    h.hover_at(start);
    h.step();
    h.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    let end = start + egui::vec2(30.0, 0.0);
    h.hover_at(end);
    h.step();
    h.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    assert_eq!(*h.state(), 2);
    // Keyboard / accessibility activation, and the octagon style.
    h.get_by_label("E-STOP").click();
    h.run_ok();
    let octagon = h.get_by_label("STOP").rect();
    click_at(&mut h, octagon.center());
    assert_eq!(*h.state(), 4);
    // A disabled E-STOP stays quiet.
    let off = h.get_by_label("OFF").rect();
    click_at(&mut h, off.center());
    assert_eq!(*h.state(), 4);
}

#[test]
fn estop_above_modals_stays_clickable_under_a_dialog() {
    let mut h = harness_state(
        Theme::Light,
        (520.0, 320.0),
        (0_usize, 0_usize, false),
        |ui, (plain, above, disabled_fired)| {
            ui.horizontal(|ui| {
                if ui.add(EStopButton::new().label("PLAIN")).changed() {
                    *plain += 1;
                }
                if EStopButton::new().show_above_modals(ui).changed() {
                    *above += 1;
                }
                ui.add_enabled_ui(false, |ui| {
                    if EStopButton::new()
                        .label("LOCKED")
                        .show_above_modals(ui)
                        .changed()
                    {
                        *disabled_fired = true;
                    }
                });
            });
            let _ = ConfirmDialog::new("q", "Cancel the print?")
                .danger(true)
                .show(ui.ctx());
        },
    );
    // The modal swallows the click on the plain button...
    let plain = h.get_by_label("PLAIN").rect();
    click_at(&mut h, plain.center());
    // ...but not on the one drawn above it.
    let above = h.get_by_label("E-STOP").rect();
    // Same layout as `ui.add`: right after the plain button.
    assert!(above.min.x > plain.max.x && above.min.y == plain.min.y);
    click_at(&mut h, above.center());
    let locked = h.get_by_label("LOCKED").rect();
    click_at(&mut h, locked.center());
    assert_eq!(*h.state(), (0, 1, false));
}
