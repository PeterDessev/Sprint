//! The controls sidebar on a two-tool simulator with a heated chamber: a
//! card per heater, presets for the active tool, the tool selector, per
//! tool flow and "Cool down" for everything. Only the simulator is ever
//! connected.

mod controls_common;

use std::time::Duration;

use controls_common::{
    Rig, by_label, click_at, received, settle, sidebar_fixed, wait_for, wait_for_commands,
    wait_idle,
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};
use sprint_gui::theme::Theme;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{Heater, Printer};

fn two_tools() -> Harness<'static, Rig> {
    let config = SimConfig {
        extruders: 2,
        heated_chamber: true,
        ..controls_common::fast_sim()
    };
    let rig = Rig::simulator_with(config);
    wait_for(&rig.printer, "both hotends and the chamber", |s| {
        s.temps.hotends.len() == 2 && s.temps.chamber.is_some()
    });
    let mut h = sidebar_fixed(Theme::Dark, (420.0, 3000.0), rig);
    settle(&mut h);
    h
}

fn printer(h: &Harness<'_, Rig>) -> Printer {
    h.state().printer.clone()
}

fn clear(h: &Harness<'_, Rig>) {
    let p = printer(h);
    wait_idle(&p);
    controls_common::sim(&p).clear_received();
}

/// Click the first widget named `label` in drawing order (the default
/// macros repeat some names further down).
fn click(h: &mut Harness<'_, Rig>, label: &str) {
    let pos = h
        .query_all(by_label(label))
        .next()
        .unwrap_or_else(|| panic!("no widget {label:?}"))
        .rect()
        .center();
    click_at(h, pos);
    settle(h);
}

#[test]
fn every_heater_gets_a_card() {
    let h = two_tools();
    for name in ["Hotend T0", "Hotend T1", "Bed", "Chamber"] {
        assert!(
            h.query_all(by_label(name)).count() >= 1,
            "no card for {name}"
        );
    }
}

#[test]
fn presets_heat_the_active_tool_and_cool_down_covers_all() {
    let mut h = two_tools();
    let p = printer(&h);
    click(&mut h, "Preset PLA");
    wait_for_commands(&p, &["M104 T0 S205", "M140 S60"]);
    clear(&h);
    click(&mut h, "Cool down");
    wait_for_commands(&p, &["M104 T0 S0", "M104 T1 S0", "M140 S0", "M141 S0"]);
}

#[test]
fn the_tool_selector_picks_the_extruder() {
    let mut h = two_tools();
    let p = printer(&h);
    p.set_temperature(Heater::Hotend(1), 200.0);
    wait_for(&p, "T1 to pass 170 °C", |s| {
        s.temps.hotends.get(1).is_some_and(|t| t.current >= 175.0)
    });
    click(&mut h, "T1");
    // T0 is still cold, T1 hot: extrusion is allowed for T1.
    for _ in 0..5 {
        h.step();
    }
    assert!(!h.get(by_label("Extrude")).accesskit_node().is_disabled());
    clear(&h);
    click(&mut h, "Extrude");
    wait_for_commands(&p, &["T1", "M83", "G1 E10 F300"]);
    // Back to T0: cold again, so blocked.
    click(&mut h, "T0");
    assert!(h.get(by_label("Extrude")).accesskit_node().is_disabled());
}

#[test]
fn flow_is_set_per_tool() {
    let mut h = two_tools();
    let p = printer(&h);
    let flow = h
        .get_all_by_role(Role::Slider)
        .find(|n| n.accesskit_node().label().as_deref() == Some("Flow T1"))
        .expect("flow slider of T1")
        .rect();
    click_at(&mut h, egui::pos2(flow.max.x - 2.0, flow.center().y));
    wait_for_commands(&p, &["M221 T1 S200"]);
    // Nothing else was touched.
    for _ in 0..10 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    wait_idle(&p);
    let flows: Vec<_> = received(&p)
        .into_iter()
        .filter(|c| c.starts_with("M221"))
        .collect();
    assert_eq!(flows, ["M221 T1 S200"]);
}
