//! The controls sidebar must never panic, whatever the printer reports and
//! whatever the config holds: non-finite and absurd readings, too many
//! tools and fans, broken presets and macros, at every supported width.
//! The rigs here are synthetic (no printer is connected), so clicking
//! everything is harmless.

mod controls_common;

use controls_common::{Rig, heating_state, pointer_away, settle, sidebar};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};
use sprint_gui::theme::Theme;
use sprint_host::sprint_proto::{HeaterReading, PositionReport};
use sprint_host::{Macro, PrinterState, TempPreset, TempSample};

/// Everything a confused (or malicious) firmware could report.
fn nasty_state() -> PrinterState {
    let mut state = heating_state();
    state.position = Some(PositionReport {
        x: f32::NAN,
        y: f32::INFINITY,
        z: -1e30,
        e: f32::MAX,
        stepper_counts: None,
    });
    state.temps.hotends = (0..40)
        .map(|i| HeaterReading {
            current: [f32::NAN, f32::MAX, -300.0, 1e9][i % 4],
            target: [Some(f32::INFINITY), Some(-50.0), None, Some(f32::NAN)][i % 4],
            power: [Some(-3.0), Some(f32::NAN), Some(99.0), None][i % 4],
        })
        .collect();
    state.temps.bed = Some(HeaterReading {
        current: f32::NEG_INFINITY,
        target: Some(1e30),
        power: Some(f32::INFINITY),
    });
    state.temps.chamber = Some(HeaterReading {
        current: f32::NAN,
        target: None,
        power: None,
    });
    if let Some(fw) = state.firmware.as_mut() {
        fw.extruder_count = Some(255);
    }
    state.active_tool = 200;
    state.feedrate_pct = f32::NAN;
    state.flow_pct = vec![f32::INFINITY, -5.0, 1e9];
    state.fan_speeds = vec![255; 50];
    let nasty = [f32::NAN, f32::INFINITY, f32::MAX, -f32::MAX, 0.0];
    for (i, v) in nasty.iter().enumerate() {
        state.temp_history.push_back(TempSample {
            t: [f64::NAN, 1e300, -1.0, f64::INFINITY, 241.0][i],
            hotends: vec![(*v, *v); i * 7],
            bed: Some((*v, *v)),
            chamber: Some((*v, -*v)),
            hotend_power: vec![*v; i],
            bed_power: Some(*v),
        });
    }
    state
}

/// A config with broken presets, feedrates and macros.
fn nasty_rig(state: PrinterState) -> Rig {
    let mut rig = Rig::synthetic(state);
    let profile = &mut rig.config.profiles[0];
    profile.extruders = 0;
    profile.bed_size = [f32::NAN, -40.0];
    profile.max_z = f32::INFINITY;
    profile.heated_chamber = true;
    profile.jog_feedrate_xy = f32::NAN;
    profile.jog_feedrate_z = -1.0;
    profile.extrude_feedrate = f32::INFINITY;
    profile.temp_presets = vec![
        TempPreset {
            name: String::new(),
            hotend: f32::NAN,
            bed: f32::INFINITY,
        },
        TempPreset {
            name: "A preset with a very, very long name that never ends".into(),
            hotend: 1e9,
            bed: -10.0,
        },
        TempPreset {
            name: "🔥 Nylon ✨".into(),
            hotend: 260.0,
            bed: 90.0,
        },
    ];
    let long_gcode: String = (0..2000).map(|i| format!("G1 X{i}\n")).collect();
    rig.config.macros = vec![
        Macro {
            name: String::new(),
            gcode: String::new(),
            color: Some([0, 0, 0]),
            confirm: true,
        },
        Macro {
            name: "Ünïcødé 🚀 macro with an extremely long name".into(),
            gcode: long_gcode,
            color: Some([255, 255, 255]),
            confirm: false,
        },
        Macro {
            name: "   ".into(),
            gcode: "; only a comment".into(),
            color: None,
            confirm: false,
        },
    ];
    rig
}

/// Click every button, slider and toggle on screen once (closing any
/// dialog or menu it opens), then hover across the panel.
fn poke_everything(h: &mut Harness<'_, Rig>) {
    let targets: Vec<egui::Pos2> = h
        .query_all_by_role(Role::Button)
        .chain(h.query_all_by_role(Role::Slider))
        .chain(h.query_all_by_role(Role::CheckBox))
        .chain(h.query_all_by_role(Role::RadioButton))
        .map(|n| n.rect().center())
        .collect();
    assert!(targets.len() > 20, "only {} controls", targets.len());
    for pos in targets {
        controls_common::click_at(h, pos);
        h.key_press(egui::Key::Escape);
        h.step();
    }
    let size = h.ctx.content_rect();
    let mut y = size.min.y + 10.0;
    while y < size.max.y {
        h.hover_at(egui::pos2(size.center().x, y));
        h.step();
        y += 97.0;
    }
    pointer_away(h);
}

#[test]
fn absurd_reports_and_config_do_not_panic() {
    for (theme, width) in [(Theme::Dark, 280.0), (Theme::Light, 480.0)] {
        let mut h = sidebar(theme, width + 32.0, nasty_rig(nasty_state()));
        settle(&mut h);
        poke_everything(&mut h);
    }
}

#[test]
fn a_job_and_absurd_reports_do_not_panic() {
    let mut state = nasty_state();
    controls_common::printing(&mut state);
    let mut h = sidebar(Theme::Dark, 352.0, nasty_rig(state));
    poke_everything(&mut h);
}

#[test]
fn an_empty_state_and_config_do_not_panic() {
    let mut rig = Rig::synthetic(PrinterState {
        status: sprint_host::ConnectionStatus::Connected,
        ..PrinterState::default()
    });
    rig.config.macros.clear();
    rig.config.profiles[0].temp_presets.clear();
    let mut h = sidebar(Theme::Light, 332.0, rig);
    poke_everything(&mut h);
}
