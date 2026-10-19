//! The controls sidebar driving the virtual printer: every click must reach
//! the simulator as the expected G-code, and motion must stay locked while
//! a job runs. Only the simulator is ever connected.

mod controls_common;

use std::time::{Duration, Instant};

use controls_common::{
    Rig, click_at, contains_run, printing, received, sidebar_fixed, wait_for, wait_for_commands,
    wait_idle,
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};
use sprint_gui::{
    theme::Theme,
    widgets::{JogCorner, JogDirection, JogGeometry, JogRegion},
};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{Macro, Printer, PrinterState};

/// A sidebar on a connected simulator, tall enough that nothing scrolls.
fn connected() -> Harness<'static, Rig> {
    let mut h = sidebar_fixed(Theme::Dark, (420.0, 2600.0), Rig::simulator());
    controls_common::settle(&mut h);
    h
}

fn printer(h: &Harness<'_, Rig>) -> Printer {
    h.state().printer.clone()
}

/// Forget what the simulator received so far.
fn clear(h: &Harness<'_, Rig>) {
    let p = printer(h);
    wait_idle(&p);
    controls_common::sim(&p).clear_received();
}

/// Keep the UI running for a while (lets queued commands reach the printer).
fn run_for(h: &mut Harness<'_, Rig>, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Assert that nothing reached the printer after giving it time to.
fn assert_nothing_sent(h: &mut Harness<'_, Rig>, what: &str) {
    run_for(h, Duration::from_millis(300));
    let p = printer(h);
    wait_idle(&p);
    let got = received(&p);
    assert!(got.is_empty(), "{what}: expected no commands, got {got:?}");
}

/// Click the center of a jog pad region.
fn click_jog(h: &mut Harness<'_, Rig>, region: JogRegion) {
    let rect = h.get_by_label("XY jog").rect();
    let geo = JogGeometry::new(rect, true);
    let pos = match region {
        JogRegion::Center => geo.center,
        JogRegion::Corner(corner) => geo.corner_rect(corner).center(),
        JogRegion::Sector { direction, ring } => {
            let (r0, r1) = geo.ring_radii(ring);
            let r = (r0 + r1) / 2.0;
            let offset = match direction {
                JogDirection::Up => egui::vec2(0.0, -r),
                JogDirection::Down => egui::vec2(0.0, r),
                JogDirection::Left => egui::vec2(-r, 0.0),
                JogDirection::Right => egui::vec2(r, 0.0),
            };
            geo.center + offset
        }
    };
    click_at(h, pos);
}

/// Click a widget by its accessible label with separate press and release
/// frames (custom-painted widgets act on the press position). With several
/// matches the first in drawing order is clicked.
fn click(h: &mut Harness<'_, Rig>, label: &str) {
    let pos = h
        .get_all_by_label(label)
        .next()
        .unwrap_or_else(|| panic!("no widget labelled {label:?}"))
        .rect()
        .center();
    click_at(h, pos);
}

/// The rect of the slider labelled `label`.
fn slider(h: &Harness<'_, Rig>, label: &str) -> egui::Rect {
    h.get_all_by_role(Role::Slider)
        .find(|n| n.accesskit_node().label().as_deref() == Some(label))
        .unwrap_or_else(|| panic!("no slider {label:?}"))
        .rect()
}

#[test]
fn jog_pad_and_z_strip_send_relative_moves() {
    let mut h = connected();
    let p = printer(&h);
    click_jog(
        &mut h,
        JogRegion::Sector {
            direction: JogDirection::Up,
            ring: 2,
        },
    );
    wait_for_commands(&p, &["G91", "G1 Y10 F3000", "G90"]);
    click_jog(
        &mut h,
        JogRegion::Sector {
            direction: JogDirection::Left,
            ring: 0,
        },
    );
    wait_for_commands(&p, &["G91", "G1 X-0.1 F3000", "G90"]);
    click(&mut h, "Move Z +1 mm");
    wait_for_commands(&p, &["G91", "G1 Z1 F600", "G90"]);
    click(&mut h, "Move Z \u{2212}10 mm");
    wait_for_commands(&p, &["G91", "G1 Z-10 F600", "G90"]);
}

#[test]
fn homing_and_motors_off() {
    let mut h = connected();
    let p = printer(&h);
    click_jog(&mut h, JogRegion::Center);
    wait_for_commands(&p, &["G28 X Y"]);
    click_jog(&mut h, JogRegion::Corner(JogCorner::BottomLeft));
    wait_for_commands(&p, &["G28"]);
    click_jog(&mut h, JogRegion::Corner(JogCorner::TopLeft));
    wait_for_commands(&p, &["G28 X"]);
    click(&mut h, "Home Z (G28 Z)");
    wait_for_commands(&p, &["G28 Z"]);
    click_jog(&mut h, JogRegion::Corner(JogCorner::BottomRight));
    wait_for_commands(&p, &["M84"]);
}

#[test]
fn move_to_lifts_first_and_center_moves_to_the_bed_center() {
    let mut h = connected();
    let p = printer(&h);
    // The simulator starts at Z0; the default target is X110 Y110 Z10.
    click(&mut h, "Move");
    let got = wait_for_commands(&p, &["G90", "G1 Z10 F600", "G90", "G1 X110 Y110 F3000"]);
    assert!(!got.iter().any(|c| c.starts_with("G91")), "{got:?}");
    clear(&h);
    click(&mut h, "Center");
    wait_for_commands(&p, &["G90", "G1 X110 Y110 F3000"]);
    clear(&h);
    h.get_by_label("Refresh the position (M114)").click();
    h.run_ok();
    wait_for_commands(&p, &["M114"]);
}

/// Type `value` into the move-to field of `axis` (0 = X, 1 = Y, 2 = Z).
fn set_move_target(h: &mut Harness<'_, Rig>, axis: usize, value: &str) {
    let field = h
        .get_all_by_role(Role::SpinButton)
        .nth(axis)
        .expect("move-to field")
        .rect();
    click_at(h, field.center());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text(value.into()));
    h.step();
    h.key_press(egui::Key::Enter);
    h.run_ok();
}

/// Absolute moves never travel close to the bed: "Center" and "Move" lift
/// to 5 mm first when the nozzle is lower.
#[test]
fn travel_near_the_bed_lifts_first() {
    let mut h = connected();
    let p = printer(&h);
    // The simulator starts at Z0.
    click(&mut h, "Center");
    wait_for_commands(&p, &["G90", "G1 Z5 F600", "G90", "G1 X110 Y110 F3000"]);
    // Straight down at the same spot: one move.
    set_move_target(&mut h, 2, "0.3");
    clear(&h);
    click(&mut h, "Move");
    wait_for_commands(&p, &["G90", "G1 X110 Y110 Z0.3 F3000"]);
    wait_for(&p, "the nozzle at Z0.3", |s| {
        s.position.as_ref().is_some_and(|pos| pos.z == 0.3)
    });
    // To another spot at the same low height: lift, travel, lower.
    set_move_target(&mut h, 0, "30");
    clear(&h);
    click(&mut h, "Move");
    wait_for_commands(
        &p,
        &[
            "G90",
            "G1 Z5 F600",
            "G90",
            "G1 X30 Y110 F3000",
            "G90",
            "G1 Z0.3 F600",
        ],
    );
}

/// Most firmware does not report the position by itself: the sidebar asks
/// for it after every move it makes (jogs, move-to, macros), so the readout
/// and the planning of absolute moves use where the nozzle really is.
#[test]
fn moves_refresh_the_position_without_auto_reports() {
    let config = SimConfig {
        auto_report_position: false,
        ..controls_common::fast_sim()
    };
    let mut rig = Rig::simulator_with(config);
    rig.config.macros.push(Macro {
        name: "Park".into(),
        gcode: "G1 X5 Y200 F3000".into(),
        color: None,
        confirm: false,
    });
    let mut h = sidebar_fixed(Theme::Dark, (420.0, 2600.0), rig);
    controls_common::settle(&mut h);
    let p = printer(&h);
    let at_z = |z: f32| move |s: &PrinterState| s.position.as_ref().is_some_and(|pos| pos.z == z);
    click(&mut h, "Move Z +10 mm");
    wait_for_commands(&p, &["G91", "G1 Z10 F600", "G90", "M114"]);
    wait_for(&p, "the position after the jog", at_z(10.0));
    run_for(&mut h, Duration::from_millis(50));
    assert!(
        h.query_by_label("Position X 0.00 Y 0.00 Z 10.00 E 0.00")
            .is_some(),
        "the readout shows the new height"
    );
    clear(&h);
    click(&mut h, "Center");
    wait_for_commands(&p, &["G90", "G1 X110 Y110 F3000", "M114"]);
    clear(&h);
    click(&mut h, "Park");
    wait_for_commands(&p, &["G1 X5 Y200 F3000", "M114"]);
    wait_for(&p, "the position after the macro", |s| {
        s.position.as_ref().is_some_and(|pos| pos.y == 200.0)
    });
    // Firmware that reports the position by itself is not asked.
    let mut h = connected();
    let p = printer(&h);
    click(&mut h, "Move Z +10 mm");
    wait_for_commands(&p, &["G91", "G1 Z10 F600", "G90"]);
    assert_nothing_more(&mut h, "M114");
}

/// Nothing starting with `prefix` reached the printer.
fn assert_nothing_more(h: &mut Harness<'_, Rig>, prefix: &str) {
    run_for(h, Duration::from_millis(300));
    let p = printer(h);
    wait_idle(&p);
    let got = received(&p);
    assert!(
        !got.iter().any(|c| c.starts_with(prefix)),
        "unexpected {prefix}: {got:?}"
    );
}

#[test]
fn temperature_presets_cool_down_and_typed_targets() {
    let mut h = connected();
    let p = printer(&h);
    click(&mut h, "Preset PLA");
    wait_for_commands(&p, &["M104 T0 S205", "M140 S60"]);
    wait_for(&p, "the PLA targets", |s| {
        s.temps.hotends.first().and_then(|t| t.target) == Some(205.0)
            && s.temps.bed.as_ref().and_then(|b| b.target) == Some(60.0)
    });
    click(&mut h, "Cool down");
    wait_for_commands(&p, &["M104 T0 S0", "M140 S0"]);
    wait_for(&p, "the heaters to be off", |s| {
        s.temps.hotends.first().and_then(|t| t.target) == Some(0.0)
    });
    run_for(&mut h, Duration::from_millis(50));
    clear(&h);
    // Type a target into the hotend card and press Enter.
    let field = h
        .get_all_by_role(Role::TextInput)
        .next()
        .expect("target field")
        .rect();
    click_at(&mut h, field.center());
    h.get_all_by_role(Role::TextInput)
        .next()
        .expect("target field")
        .type_text("199");
    h.step();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    wait_for_commands(&p, &["M104 T0 S199"]);
}

#[test]
fn macros_run_directly_or_after_confirmation() {
    let mut h = connected();
    let p = printer(&h);
    click(&mut h, "Home");
    wait_for_commands(&p, &["G28"]);
    clear(&h);
    // "Bed mesh" asks first: nothing is sent until it is confirmed.
    click(&mut h, "Bed mesh…");
    h.run_ok();
    assert!(h.query_by_label("Run “Bed mesh”?").is_some(), "no dialog");
    h.get_by_label("Cancel").click();
    h.run_ok();
    assert!(h.query_by_label("Run “Bed mesh”?").is_none());
    assert_nothing_sent(&mut h, "a cancelled macro");
    click(&mut h, "Bed mesh…");
    h.run_ok();
    h.get_by_label("Run").click();
    h.run_ok();
    wait_for_commands(&p, &["G28", "G29", "M420 V"]);
}

#[test]
fn motion_and_extrusion_are_locked_during_a_job() {
    let mut rig = Rig::simulator();
    rig.tweak = Some(printing);
    let mut h = sidebar_fixed(Theme::Dark, (420.0, 2600.0), rig);
    controls_common::settle(&mut h);
    assert!(h.get_by_label("XY jog").accesskit_node().is_disabled());
    for label in [
        "Move Z +1 mm",
        "Move",
        "Center",
        "Load",
        "Unload",
        "Retract",
    ] {
        assert!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            "{label} is enabled during a job"
        );
    }
    assert_eq!(h.query_all_by_label("Locked while a job runs").count(), 2);
    click_jog(
        &mut h,
        JogRegion::Sector {
            direction: JogDirection::Up,
            ring: 1,
        },
    );
    click_jog(&mut h, JogRegion::Corner(JogCorner::BottomLeft));
    click(&mut h, "Move Z +1 mm");
    click(&mut h, "Center");
    click(&mut h, "Load");
    assert_nothing_sent(&mut h, "motion during a job");
    // Live tuning stays available.
    let p = printer(&h);
    assert!(!h.get_by_label("Raise").accesskit_node().is_disabled());
    click(&mut h, "Raise");
    wait_for_commands(&p, &["M290 Z0.05"]);
    // A preset changes the print's temperatures: it asks first.
    click(&mut h, "Preset PETG");
    h.run_ok();
    assert!(h.query_by_label("Set the PETG temperatures?").is_some());
    h.get_by_label("Set").click();
    h.run_ok();
    wait_for_commands(&p, &["M104 T0 S240", "M140 S80"]);
}

/// Turning heaters off during a job makes the print fail: "Cool down" and
/// a card's off button ask first; outside a job they act at once.
#[test]
fn heaters_off_during_a_job_asks_first() {
    let mut rig = Rig::simulator();
    rig.tweak = Some(printing);
    let mut h = sidebar_fixed(Theme::Dark, (420.0, 2600.0), rig);
    controls_common::settle(&mut h);
    let p = printer(&h);
    p.set_temperature(sprint_host::Heater::Hotend(0), 200.0);
    p.set_temperature(sprint_host::Heater::Bed, 60.0);
    wait_for(&p, "the targets", |s| {
        s.temps.bed.as_ref().and_then(|b| b.target) == Some(60.0)
            && s.temps.hotends.first().and_then(|t| t.target) == Some(200.0)
    });
    run_for(&mut h, Duration::from_millis(50));
    clear(&h);
    // "Cool down" is the button, not the macro of the same name (drawn
    // later).
    click(&mut h, "Cool down");
    h.run_ok();
    assert!(h.query_by_label("Turn every heater off?").is_some());
    h.get_by_label("Cancel").click();
    h.run_ok();
    assert!(h.query_by_label("Turn every heater off?").is_none());
    assert_nothing_sent(&mut h, "a cancelled cool down");
    // Enter picks the safe answer: the focused Cancel.
    click(&mut h, "Cool down");
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert!(h.query_by_label("Turn every heater off?").is_none());
    assert_nothing_sent(&mut h, "Enter in the cool down question");
    click(&mut h, "Cool down");
    h.run_ok();
    h.get_by_label("Turn off").click();
    h.run_ok();
    wait_for_commands(&p, &["M104 T0 S0", "M140 S0"]);
    wait_for(&p, "the heaters off", |s| {
        s.temps.bed.as_ref().and_then(|b| b.target) == Some(0.0)
    });
    run_for(&mut h, Duration::from_millis(50));
    clear(&h);
    // With every heater off there is nothing to ask.
    click(&mut h, "Cool down");
    h.run_ok();
    assert!(h.query_by_label("Turn every heater off?").is_none());
    wait_for_commands(&p, &["M104 T0 S0", "M140 S0"]);
}

#[test]
fn babystep_tracks_and_undoes_its_offset() {
    let mut h = connected();
    let p = printer(&h);
    click(&mut h, "0.1");
    click(&mut h, "Raise");
    wait_for_commands(&p, &["M290 Z0.1"]);
    click(&mut h, "0.01");
    click(&mut h, "Lower");
    wait_for_commands(&p, &["M290 Z-0.01"]);
    h.run_ok();
    assert!(h.query_by_label("+0.090 mm").is_some());
    clear(&h);
    click(&mut h, "Undo all babystepping (M290 Z-0.09)");
    wait_for_commands(&p, &["M290 Z-0.09"]);
}

#[test]
fn sliders_send_fan_speed_and_overrides_on_release() {
    let mut h = connected();
    let p = printer(&h);
    // Click the far right of the fan slider: full speed.
    let fan = slider(&h, "Fan");
    click_at(&mut h, egui::pos2(fan.max.x - 2.0, fan.center().y));
    wait_for_commands(&p, &["M106 S255"]);
    wait_for(&p, "the fan report", |s| s.fan_speeds.first() == Some(&255));
    h.run_ok();
    click(&mut h, "Turn the fan off (M107)");
    wait_for_commands(&p, &["M107"]);
    // The speed override's reset appears once it differs from 100%.
    p.send("M220 S150");
    wait_for(&p, "the speed override", |s| s.feedrate_pct == 150.0);
    run_for(&mut h, Duration::from_millis(100));
    clear(&h);
    click(&mut h, "Reset to 100% (M220 S100)");
    wait_for_commands(&p, &["M220 S100"]);
    // Dragging only sends once, on release.
    clear(&h);
    let speed = slider(&h, "Speed");
    let start = egui::pos2(speed.min.x + 40.0, speed.center().y);
    h.hover_at(start);
    h.step();
    h.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    for i in 1..=8 {
        h.hover_at(start + egui::vec2(i as f32 * 10.0, 0.0));
        h.step();
    }
    assert_nothing_sent(&mut h, "a slider being dragged");
    let end = start + egui::vec2(80.0, 0.0);
    h.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    run_for(&mut h, Duration::from_millis(300));
    wait_idle(&p);
    let got: Vec<_> = received(&p)
        .into_iter()
        .filter(|c| c.starts_with("M220"))
        .collect();
    assert_eq!(got.len(), 1, "one M220 per drag: {got:?}");
    assert_ne!(got[0], "M220 S100", "the drag changed the value");
}

#[test]
fn cold_hotend_blocks_extrusion_until_heated() {
    let mut h = connected();
    let p = printer(&h);
    assert!(h.get_by_label("Extrude").accesskit_node().is_disabled());
    click(&mut h, "Heat to PLA · 205 °C");
    wait_for_commands(&p, &["M104 T0 S205"]);
    wait_for(&p, "the hotend to pass 170 °C", |s| {
        s.temps.hotends.first().is_some_and(|t| t.current >= 175.0)
    });
    run_for(&mut h, Duration::from_millis(50));
    assert!(!h.get_by_label("Extrude").accesskit_node().is_disabled());
    clear(&h);
    click(&mut h, "Extrude");
    wait_for_commands(&p, &["M83", "G1 E10 F300"]);
    clear(&h);
    click(&mut h, "Retract");
    wait_for_commands(&p, &["M83", "G1 E-10 F300"]);
    clear(&h);
    // The simulator reports M603 (ADVANCED_PAUSE_FEATURE) but has no M701:
    // the sidebar notices the rejection and runs its scripted load.
    click(&mut h, "Load");
    wait_for_commands(&p, &["M109 S205", "M701"]);
    let deadline = Instant::now() + controls_common::PATIENCE;
    while !contains_run(&received(&p), &["G1 E30 F150"]) {
        assert!(
            Instant::now() < deadline,
            "no scripted load: {:?}",
            received(&p)
        );
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    let got = received(&p);
    assert!(contains_run(&got, &["M83", "G1 E60 F480"]), "{got:?}");
    assert!(
        h.state()
            .toasts
            .iter()
            .any(|t| t.title() == "The printer has no M701"),
        "no toast about the fallback"
    );
    // From now on the scripted sequence runs directly.
    clear(&h);
    click(&mut h, "Unload");
    let got = wait_for_commands(&p, &["G1 E-80 F1800"]);
    assert!(!got.iter().any(|c| c.starts_with("M702")), "{got:?}");
    assert!(contains_run(&got, &["M109 S205"]), "{got:?}");
}

#[test]
fn keyboard_jogging_needs_the_toggle_and_a_free_keyboard() {
    let mut h = connected();
    let p = printer(&h);
    h.key_press(egui::Key::ArrowUp);
    h.run_ok();
    assert_nothing_sent(&mut h, "arrows with keyboard jog off");
    h.get_by_label("Keyboard jog").click();
    h.run_ok();
    h.key_press(egui::Key::ArrowUp);
    h.run_ok();
    wait_for_commands(&p, &["G91", "G1 Y1 F3000", "G90"]);
    h.key_press(egui::Key::PageDown);
    h.run_ok();
    wait_for_commands(&p, &["G91", "G1 Z-1 F600", "G90"]);
    clear(&h);
    // Typing in a field keeps the arrows for the field.
    let field = h
        .get_all_by_role(Role::TextInput)
        .next()
        .expect("field")
        .rect();
    click_at(&mut h, field.center());
    h.key_press(egui::Key::ArrowLeft);
    h.run_ok();
    assert_nothing_sent(&mut h, "arrows while typing");
}

#[test]
fn keyboard_jogging_leaves_the_arrows_to_an_open_menu() {
    let mut h = connected();
    let p = printer(&h);
    h.get_by_label("Keyboard jog").click();
    h.run_ok();
    // The context menu of the "Home" macro (the lowest "Home" on screen).
    h.get_all_by_label("Home")
        .max_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .expect("the Home macro")
        .click_secondary();
    h.run_ok();
    assert!(egui::Popup::is_any_open(&h.ctx), "the menu opened");
    h.key_press(egui::Key::ArrowDown);
    h.run_ok();
    assert_nothing_sent(&mut h, "arrows in an open menu");
    // Closed again, the arrows jog.
    h.key_press(egui::Key::Escape);
    h.run_ok();
    assert!(!egui::Popup::is_any_open(&h.ctx), "the menu closed");
    h.key_press(egui::Key::ArrowDown);
    h.run_ok();
    wait_for_commands(&p, &["G91", "G1 Y-1 F3000", "G90"]);
}

#[test]
fn keyboard_jogging_pauses_while_the_motion_card_is_collapsed() {
    let mut h = connected();
    let p = printer(&h);
    h.get_by_label("Keyboard jog").click();
    h.run_ok();
    // Collapse the card by its header.
    click(&mut h, "Motion");
    h.run_ok();
    assert!(h.query_by_label("Keyboard jog").is_none(), "card collapsed");
    h.key_press(egui::Key::ArrowUp);
    h.run_ok();
    assert_nothing_sent(&mut h, "arrows with the motion card collapsed");
    // Open again: the toggle is still on and the arrows jog.
    click(&mut h, "Motion");
    h.run_ok();
    h.key_press(egui::Key::ArrowUp);
    h.run_ok();
    wait_for_commands(&p, &["G91", "G1 Y1 F3000", "G90"]);
}

/// After an emergency stop the printer ignores commands until its firmware
/// restarts: the sidebar says so and locks motion and extrusion.
#[test]
fn a_halted_printer_locks_motion_and_says_why() {
    let mut h = sidebar_fixed(Theme::Dark, (420.0, 2600.0), Rig::simulator());
    controls_common::settle(&mut h);
    let p = printer(&h);
    assert!(!h.get_by_label("XY jog").accesskit_node().is_disabled());
    p.emergency_stop();
    wait_for(&p, "the halt", |s| s.halted);
    run_for(&mut h, Duration::from_millis(50));
    h.run_ok();
    assert!(
        h.query_all_by_label_contains("The printer is halted")
            .next()
            .is_some(),
        "no halt notice"
    );
    assert_eq!(
        h.query_all_by_label("Locked while the printer is halted")
            .count(),
        2
    );
    for label in ["XY jog", "Move", "Center", "Load", "Retract"] {
        assert!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            "{label} is enabled while halted"
        );
    }
}

#[test]
fn offline_sidebar_sends_nothing_and_does_not_panic() {
    let mut h = sidebar_fixed(Theme::Light, (420.0, 2600.0), Rig::offline());
    controls_common::settle(&mut h);
    assert!(h.get_by_label("XY jog").accesskit_node().is_disabled());
    assert!(h.get_by_label("Preset PLA").accesskit_node().is_disabled());
    click(&mut h, "Preset PLA");
    click_jog(&mut h, JogRegion::Center);
    click(&mut h, "Home");
    h.run_ok();
    assert!(h.state().printer.sim_control().is_none());
    // Macros are not runnable but still editable offline.
    assert!(h.query_by_label("Run “Home”?").is_none());
}
