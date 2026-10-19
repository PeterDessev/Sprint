//! Snapshots of the controls sidebar at the narrow and wide ends of its
//! side panel, in both themes and in its main states (heating, printing,
//! cold, offline, several tools), plus the macro editor.
//!
//! Regenerate with
//! `UPDATE_SNAPSHOTS=1 cargo test -p sprint-gui --test controls_snapshots`.

mod controls_common;

use controls_common::{
    Rig, by_label, cold_state, heating_state, pointer_away, printing, settle, sidebar,
    sidebar_fixed, snapshot,
};
use egui_kittest::{Harness, kittest::Queryable as _};
use sprint_gui::theme::Theme;
use sprint_host::sprint_proto::HeaterReading;
use sprint_host::{Macro, PrinterState};

/// The harness width for a side panel `panel` points wide: the panel's own
/// 8 pt inner margins are replaced by the harness' 24 pt ones.
fn harness_width(panel: f32) -> f32 {
    panel - 16.0 + 48.0
}

fn name(base: &str, theme: Theme) -> String {
    format!("controls_{base}_{}", theme.name().to_lowercase())
}

/// A colorful macro list, as a user might have set it up.
fn macros() -> Vec<Macro> {
    let plain = |name: &str, gcode: &str| Macro {
        name: name.into(),
        gcode: gcode.into(),
        color: None,
        confirm: false,
    };
    let colored = |name: &str, gcode: &str, color: [u8; 3], confirm: bool| Macro {
        color: Some(color),
        confirm,
        ..plain(name, gcode)
    };
    vec![
        colored(
            "Preheat PLA",
            "M140 S60\nM104 S205",
            [0xFF, 0x7A, 0x1A],
            false,
        ),
        colored("Level corners", "G28\nG1 Z0.2", [0x4D, 0xA3, 0xFF], true),
        colored("Filament change", "M600", [0xB1, 0x8C, 0xFF], true),
        plain("Home", "G28"),
        plain("Report settings", "M503"),
        plain("Motors off", "M84"),
        colored("Bed mesh", "G28\nG29", [0x3F, 0xCF, 0x8E], true),
    ]
}

/// A synthetic rig showing `state` with the colorful macros.
fn rig(state: PrinterState) -> Rig {
    let mut rig = Rig::synthetic(state);
    rig.config.macros = macros();
    rig
}

/// A sidebar on `rig` for a `panel` points wide side panel.
fn panel(theme: Theme, panel: f32, rig: Rig) -> Harness<'static, Rig> {
    sidebar(theme, harness_width(panel), rig)
}

#[test]
fn sidebar_widths_and_themes() {
    for theme in Theme::ALL {
        for width in [300.0, 420.0] {
            let mut h = panel(theme, width, rig(heating_state()));
            snapshot(&mut h, &name(&format!("sidebar_{width}"), theme));
        }
    }
}

#[test]
fn sidebar_at_the_narrowest_panel() {
    let mut h = panel(Theme::Dark, 280.0, rig(heating_state()));
    // Nothing is clipped: the jog pad and the presets fit the width.
    let pad = h.get(by_label("XY jog")).rect();
    assert!(pad.width() >= 150.0, "jog pad {pad:?}");
    assert!(pad.max.x <= harness_width(280.0) - 24.0 + 0.5, "{pad:?}");
    snapshot(&mut h, &name("sidebar_280", Theme::Dark));
}

#[test]
fn sidebar_while_printing() {
    let mut state = heating_state();
    printing(&mut state);
    state.feedrate_pct = 120.0;
    state.flow_pct = vec![95.0];
    let mut h = panel(Theme::Dark, 300.0, rig(state));
    // Babystep a little so the offset readout shows.
    h.get(by_label("Raise")).click();
    settle(&mut h);
    h.get(by_label("Raise")).click();
    settle(&mut h);
    assert!(h.query(by_label("+0.100 mm")).is_some());
    pointer_away(&mut h);
    snapshot(&mut h, &name("sidebar_printing", Theme::Dark));
}

#[test]
fn sidebar_cold_and_keyboard_jog() {
    let mut h = panel(Theme::Light, 300.0, rig(cold_state()));
    h.get(by_label("Keyboard jog")).click();
    settle(&mut h);
    controls_common::fit_height(&mut h, harness_width(300.0));
    assert!(h.query(by_label("Heat to PLA · 205 °C")).is_some());
    pointer_away(&mut h);
    snapshot(&mut h, &name("sidebar_cold", Theme::Light));
}

#[test]
fn sidebar_offline() {
    for theme in Theme::ALL {
        let mut h = panel(theme, 300.0, Rig::offline());
        snapshot(&mut h, &name("sidebar_offline", theme));
    }
}

/// Two hotends, a chamber and two fans on a wide panel.
#[test]
fn sidebar_with_two_tools() {
    let mut state = heating_state();
    if let Some(fw) = state.firmware.as_mut() {
        fw.extruder_count = Some(2);
    }
    state.temps.hotends.push(HeaterReading {
        current: 182.0,
        target: Some(240.0),
        power: Some(1.0),
    });
    state.temps.chamber = Some(HeaterReading {
        current: 31.5,
        target: None,
        power: None,
    });
    for (i, s) in state.temp_history.iter_mut().enumerate() {
        let t = i as f32;
        s.hotends.push((240.0 - 215.0 * (-t / 90.0).exp(), 240.0));
        s.chamber = Some((24.0 + 7.5 * (1.0 - (-t / 120.0).exp()), 0.0));
    }
    state.flow_pct = vec![100.0, 104.0];
    state.fan_speeds = vec![255, 77];
    state.active_tool = 1;
    let mut rig = rig(state);
    rig.config.profiles[0].extruders = 2;
    let mut h = panel(Theme::Dark, 420.0, rig);
    assert!(h.query(by_label("Hotend T1")).is_some());
    // The row label and its slider.
    assert_eq!(h.query_all(by_label("Flow T1")).count(), 2);
    assert_eq!(h.query_all(by_label("Fan 1")).count(), 2);
    snapshot(&mut h, &name("sidebar_tools", Theme::Dark));
}

#[test]
fn macros_empty_offers_defaults() {
    let mut rig = Rig::synthetic(heating_state());
    rig.config.macros.clear();
    let mut h = panel(Theme::Dark, 300.0, rig);
    assert!(h.query(by_label("Add defaults")).is_some());
    snapshot(&mut h, &name("sidebar_no_macros", Theme::Dark));
}

#[test]
fn macro_editor_dialog() {
    for theme in Theme::ALL {
        // Tall enough for the macros card to be on screen, then the
        // window shrinks to frame the dialog.
        let mut h = sidebar_fixed(theme, (720.0, 2400.0), rig(heating_state()));
        settle(&mut h);
        h.get(by_label("New macro")).click();
        settle(&mut h);
        h.set_size(egui::vec2(720.0, 760.0));
        settle(&mut h);
        // The name field has the focus; Tab moves on to the G-code.
        h.event(egui::Event::Text("Purge line".into()));
        h.key_press(egui::Key::Tab);
        h.event(egui::Event::Paste(
            "G28 ; home first\nG1 Z0.3 F600\nG1 X5 Y20 F3000\nG1 Y180 E15 F1200\nM502".into(),
        ));
        settle(&mut h);
        h.get(by_label("Color #3CC6C6")).click();
        settle(&mut h);
        assert!(
            h.query_all_by_role(egui::accesskit::Role::Label)
                .any(|n| n.value().is_some_and(|v| v.contains("Contains M502")))
        );
        pointer_away(&mut h);
        snapshot(&mut h, &name("macro_editor", theme));
    }
}
