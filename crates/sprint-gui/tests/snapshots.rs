//! Visual snapshots of every widget family in both themes.
//!
//! Regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p sprint-gui --test snapshots`.

mod common;

use common::{fit, fit_state, hover, snapshot};
use egui_kittest::kittest::Queryable as _;
use sprint_gui::{
    icons,
    theme::{Theme, Tone},
    widgets::*,
};

fn name(base: &str, theme: Theme) -> String {
    format!("{base}_{}", theme.name().to_lowercase())
}

fn buttons(ui: &mut egui::Ui) {
    section_header(ui, "Icon buttons");
    for size in [ButtonSize::Small, ButtonSize::Regular, ButtonSize::Large] {
        ui.horizontal(|ui| {
            for kind in [
                ButtonKind::Ghost,
                ButtonKind::Normal,
                ButtonKind::Primary,
                ButtonKind::Danger,
            ] {
                ui.add(
                    IconButton::new(icons::HOME)
                        .kind(kind)
                        .size(size)
                        .tooltip("Home"),
                );
            }
            ui.add(IconButton::new(icons::VIEW_3D).size(size).selected(true));
            ui.add(
                IconButton::new(icons::FAN)
                    .ghost()
                    .size(size)
                    .selected(true),
            );
        });
    }
    ui.add_space(8.0);
    section_header(ui, "Action buttons");
    ui.horizontal(|ui| {
        ui.add(
            ActionButton::new("Start print")
                .icon(icons::PLAY)
                .primary()
                .large(),
        );
        ui.add(ActionButton::new("Pause").icon(icons::PAUSE).large());
        ui.add(
            ActionButton::new("Cancel")
                .icon(icons::STOP)
                .danger()
                .large(),
        );
    });
    ui.horizontal(|ui| {
        ui.add(ActionButton::new("Connect").icon(icons::CONNECT).primary());
        ui.add(ActionButton::new("Refresh").icon(icons::REFRESH));
        ui.add(ActionButton::new("Clear").icon(icons::CLEAR).ghost());
        ui.add(ActionButton::new("Selected").selected(true));
        ui.add(ActionButton::new("Small").small());
    });
    ui.add(
        ActionButton::new("Home all axes")
            .icon(icons::HOME)
            .fill_width(true),
    );
    ui.add_enabled(false, ActionButton::new("Disabled").icon(icons::LOCK));
    ui.add_space(8.0);
    section_header(ui, "Emergency stop");
    ui.horizontal(|ui| {
        ui.add(EStopButton::new());
        ui.add_space(16.0);
        ui.add(
            EStopButton::new()
                .label("STOP")
                .style(EStopStyle::Octagon(72.0)),
        );
    });
}

fn status(ui: &mut egui::Ui) {
    section_header(ui, "Status pills");
    ui.horizontal(|ui| {
        ui.add(StatusPill::new("Connected", Tone::Success));
        ui.add(StatusPill::new("Connecting", Tone::Warning));
        ui.add(StatusPill::new("Disconnected", Tone::Neutral));
        ui.add(StatusPill::new("Halted", Tone::Danger));
    });
    ui.horizontal(|ui| {
        ui.add(StatusPill::new("Printing", Tone::Accent));
        ui.add(StatusPill::new("Simulator", Tone::Info).icon(icons::SIMULATOR));
    });
    section_header(ui, "Badges");
    ui.horizontal(|ui| {
        for tone in Tone::ALL {
            ui.add(Badge::new(&format!("{tone:?}").to_uppercase()).tone(tone));
        }
    });
    ui.horizontal(|ui| {
        for tone in Tone::ALL {
            ui.add(Badge::new("12").tone(tone).filled(true));
        }
    });
    section_header(ui, "Chips");
    ui.horizontal(|ui| {
        ui.add(Chip::new("Travel").selected(true));
        ui.add(Chip::new("Retractions"));
        ui.add(Chip::new("Perimeter").swatch(egui::Color32::from_rgb(255, 140, 60)));
        ui.add(Chip::new("Filter").icon(icons::FILTER));
        status_dot(ui, Tone::Success, 8.0);
    });
}

fn cards(ui: &mut egui::Ui) {
    card(
        ui,
        icons::FAN,
        "Part cooling fan",
        |ui| {
            ui.add(
                IconButton::new(icons::OFF)
                    .ghost()
                    .small()
                    .tooltip("Fan off"),
            );
            ui.add(Badge::new("M106").tone(Tone::Neutral));
        },
        |ui| {
            ui.label("Speed 100% (255)");
        },
    );
    ui.add_space(8.0);
    Card::new("Macros")
        .icon(icons::MACRO)
        .subtitle("3 defined")
        .collapsible(true)
        .show(ui, |ui| {
            ui.label("Level bed, Purge line, Park head");
        });
    ui.add_space(8.0);
    Card::new("Collapsed").collapsible(false).show(ui, |ui| {
        ui.label("hidden");
    });
    ui.add_space(8.0);
    card(
        ui,
        icons::FILE,
        "Job",
        |_| {},
        |ui| {
            EmptyState::new(icons::FILE, "No file loaded")
                .message("Open a G-code file or drop one onto the window to preview and print it.")
                .action("Open file…")
                .action_icon(icons::OPEN)
                .show(ui);
        },
    );
}

fn selection(ui: &mut egui::Ui) {
    section_header(ui, "Toggles");
    ui.horizontal(|ui| {
        ui.add(Toggle::new(&mut true).label("Autoscroll"));
        ui.add_space(12.0);
        ui.add(Toggle::new(&mut false).label("Show travel"));
        ui.add_space(12.0);
        ui.add_enabled(false, Toggle::new(&mut true).label("Locked"));
    });
    section_header(ui, "Segmented controls");
    let mut view = 0;
    ui.add(SegmentedControl::new(
        &mut view,
        &[(0, "3D"), (1, "Layer"), (2, "G-code")],
    ));
    let mut unit = 1;
    ui.add(
        SegmentedControl::new(&mut unit, &[(0, "0.1"), (1, "1"), (2, "10"), (3, "100")])
            .fill_width(true),
    );
}

/// A heater warming up towards `target` (first-order response), sampled
/// once per second.
fn heating_curve(start: f32, target: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| target - (target - start) * (-(i as f32) / 18.0).exp())
        .collect()
}

fn charts(ui: &mut egui::Ui) {
    let p = sprint_gui::theme::ui_palette(ui);
    section_header(ui, "Sparklines");
    let hot = heating_curve(24.0, 215.0, 90);
    let bed = heating_curve(22.0, 60.0, 90);
    let mut gappy = heating_curve(30.0, 45.0, 60);
    for v in &mut gappy[20..26] {
        *v = f32::NAN;
    }
    ui.add(Sparkline::new(&hot).color(p.hotend).reference(Some(215.0)));
    ui.add(
        Sparkline::new(&bed)
            .color(p.bed)
            .reference(Some(60.0))
            .height(28.0),
    );
    ui.horizontal(|ui| {
        ui.add(
            Sparkline::new(&gappy)
                .color(p.chamber)
                .width(160.0)
                .height(24.0),
        );
        ui.add(Sparkline::new(&[]).width(80.0).height(24.0));
        ui.add(Sparkline::new(&[f32::NAN, 1.0]).width(80.0).height(24.0));
    });
    ui.add_space(6.0);
    section_header(ui, "Mini chart");
    let temps: Vec<[f64; 2]> = hot
        .iter()
        .enumerate()
        .map(|(i, v)| [i as f64, f64::from(*v)])
        .collect();
    let targets = [[0.0, 215.0], [89.0, 215.0]];
    let beds: Vec<[f64; 2]> = bed
        .iter()
        .enumerate()
        .map(|(i, v)| [i as f64, f64::from(*v)])
        .collect();
    MiniChart::new()
        .series(ChartSeries::new("Hotend", &temps, p.hotend).fill(true))
        .series(ChartSeries::new("Target", &targets, p.hotend).dashed(true))
        .series(ChartSeries::new("Bed", &beds, p.bed))
        .unit("°C")
        .show(ui);
    MiniChart::new().height(48.0).show(ui);
    ui.add_space(6.0);
    section_header(ui, "Progress");
    ui.horizontal(|ui| {
        ui.add(ProgressRing::new(0.42).sublabel("1:23 left").animate(false));
        ui.add(
            ProgressRing::new(1.0)
                .tone(Tone::Success)
                .text("Done")
                .animate(false),
        );
        ui.add(ProgressRing::new(f32::NAN).size(64.0).animate(false));
        ui.add(ProgressRing::new(0.08).size(48.0).animate(false));
    });
    ui.add(
        LinearProgress::new(0.66)
            .label("Uploading to SD card")
            .show_percentage(true),
    );
    ui.add(LinearProgress::new(0.25).tone(Tone::Info).height(4.0));
    ui.add(LinearProgress::new(f32::NAN).tone(Tone::Warning));
}

fn forms(ui: &mut egui::Ui) {
    section_header(ui, "Numeric fields");
    let w = 120.0;
    ui.add(
        NumericField::new(&mut 80.0)
            .label("X steps")
            .unit("steps/mm")
            .range(1.0..=10_000.0)
            .original(80.0)
            .label_width(w),
    );
    ui.add(
        NumericField::new(&mut 415.5)
            .label("Z steps")
            .unit("steps/mm")
            .range(1.0..=10_000.0)
            .original(400.0)
            .label_width(w),
    );
    ui.add(
        NumericField::new(&mut 1500.0)
            .label("Max feedrate X")
            .unit("mm/s")
            .range(1.0..=1000.0)
            .decimals(0)
            .original(1500.0)
            .label_width(w),
    );
    ui.add(
        NumericField::new(&mut { f64::NAN })
            .label("Probe Z offset")
            .unit("mm")
            .label_width(w),
    );
    ui.add(
        NumericField::new(&mut 3_u8)
            .label("Fan index")
            .label_width(w),
    );
    ui.add_space(6.0);
    section_header(ui, "Key / value");
    KeyValueGrid::new("kv").key_width(110.0).show(ui, |kv| {
        kv.row("Firmware", "Marlin 2.1.2.4 (Sep 25 2024)");
        kv.row("Machine", "Prusa i3 MK3S+");
        kv.row_mono("UUID", "cede2a2f-41a2-4748-9b12-c55c62f367ff");
        kv.row_tone("Thermal", "Stable ±0.3 °C", Tone::Success);
        kv.row_tone("Endstop Z", "TRIGGERED", Tone::Warning);
        kv.row("Serial", "");
        kv.row_ui("Extruders", |ui| {
            ui.add(Badge::new("2").tone(Tone::Info));
        });
    });
}

fn jog(ui: &mut egui::Ui, (length, speed): &mut (f32, f32)) {
    card(
        ui,
        icons::MOVE,
        "Jog",
        |_| {},
        |ui| {
            ui.horizontal(|ui| {
                let _ = JogPad::new().size(240.0).show(ui);
                ui.add_space(8.0);
                let _ = ZJog::new().height(240.0).show(ui);
            });
        },
    );
    ui.add_space(12.0);
    card(
        ui,
        icons::EXTRUDE,
        "Extruder",
        |_| {},
        |ui| {
            let _ = ExtruderControl::new(length, speed).show(ui);
        },
    );
    ui.add_space(8.0);
    card(
        ui,
        icons::EXTRUDE,
        "Extruder (cold)",
        |_| {},
        |ui| {
            let _ = ExtruderControl::new(&mut 10.0, &mut 4.0)
                .presets(&[])
                .blocked(Some("Hotend below 170 °C: heat up to extrude"))
                .show(ui);
        },
    );
}

#[test]
fn buttons_snapshot() {
    for theme in Theme::ALL {
        let mut h = fit(theme, 560.0, buttons);
        snapshot(&mut h, &name("buttons", theme));
    }
}

#[test]
fn status_snapshot() {
    for theme in Theme::ALL {
        let mut h = fit(theme, 560.0, status);
        snapshot(&mut h, &name("status", theme));
    }
}

#[test]
fn cards_snapshot() {
    for theme in Theme::ALL {
        let mut h = fit(theme, 420.0, cards);
        snapshot(&mut h, &name("cards", theme));
    }
}

#[test]
fn selection_snapshot() {
    for theme in Theme::ALL {
        let mut h = fit(theme, 420.0, selection);
        snapshot(&mut h, &name("selection", theme));
    }
}

#[test]
fn charts_snapshot() {
    for theme in Theme::ALL {
        let mut h = fit(theme, 460.0, charts);
        snapshot(&mut h, &name("charts", theme));
    }
}

#[test]
fn forms_snapshot() {
    for theme in Theme::ALL {
        let mut h = fit(theme, 460.0, forms);
        snapshot(&mut h, &name("forms", theme));
    }
}

#[test]
fn jog_snapshot() {
    for theme in Theme::ALL {
        let mut h = fit_state(theme, 400.0, (10.0_f32, 5.0_f32), jog);
        snapshot(&mut h, &name("jog", theme));
        // Hover the 10 mm ring towards Y+.
        let pad = h.get_by_label("XY jog").rect();
        let geo = JogGeometry::new(pad, true);
        let (r0, r1) = geo.ring_radii(2);
        hover(&mut h, geo.center - egui::vec2(0.0, (r0 + r1) / 2.0));
        snapshot(&mut h, &name("jog_hover", theme));
    }
}

fn temperatures(ui: &mut egui::Ui) {
    use sprint_gui::theme::HeaterKind;
    let hot = heating_curve(24.0, 215.0, 40);
    let bed = heating_curve(22.0, 60.0, 120);
    let presets = [("PLA", 210.0), ("PETG", 240.0), ("ABS", 250.0)];
    let _ = TemperatureCard::new("Hotend", HeaterKind::Hotend)
        .current(*hot.last().unwrap_or(&0.0))
        .target(215.0)
        .power(Some(0.82))
        .history(&hot)
        .presets(&presets)
        .show(ui);
    ui.add_space(8.0);
    let _ = TemperatureCard::new("Bed", HeaterKind::Bed)
        .current(59.6)
        .target(60.0)
        .power(Some(0.31))
        .history(&bed)
        .presets(&[("PLA", 60.0), ("PETG", 80.0)])
        .show(ui);
    ui.add_space(8.0);
    ui.columns(2, |cols| {
        let _ = TemperatureCard::new("Chamber", HeaterKind::Chamber)
            .current(31.0)
            .target(0.0)
            .show(&mut cols[0]);
        let _ = TemperatureCard::new("Hotend 2", HeaterKind::Hotend)
            .current(f32::NAN)
            .target(0.0)
            .show(&mut cols[1]);
    });
    ui.add_space(8.0);
    let cooling: Vec<f32> = (0..60).map(|i| 180.0 - i as f32 * 1.5).collect();
    let _ = TemperatureCard::new("Hotend", HeaterKind::Hotend)
        .id_salt("cooling")
        .current(91.0)
        .target(0.0)
        .power(Some(0.0))
        .history(&cooling)
        .show(ui);
}

#[test]
fn temperatures_snapshot() {
    for theme in Theme::ALL {
        let mut h = fit(theme, 400.0, temperatures);
        snapshot(&mut h, &name("temperatures", theme));
    }
}

fn toasts_state() -> Toasts {
    let mut toasts = Toasts::new();
    toasts.info("Connected to virtual printer").duration(1000.0);
    toasts
        .warning("Resend requested")
        .message("Line 1234 had a checksum error; the link may be noisy.")
        .sticky();
    toasts.success("Print finished").sticky();
    toasts.success("Print finished").sticky();
    toasts
        .error("Thermal runaway")
        .message("Heater 0 stopped heating. The printer halted (M112).")
        .sticky();
    toasts
}

#[test]
fn toasts_snapshot() {
    for theme in Theme::ALL {
        let mut h = common::harness_state(theme, (560.0, 460.0), toasts_state(), |ui, toasts| {
            section_header(ui, "Toasts (top right)");
            ui.label("Background content");
            toasts.show(ui.ctx());
        });
        snapshot(&mut h, &name("toasts", theme));
    }
}

#[test]
fn dialogs_snapshot() {
    for (danger, base) in [(true, "dialog_danger"), (false, "dialog")] {
        for theme in Theme::ALL {
            let mut h = common::harness(theme, (560.0, 340.0), move |ui| {
                section_header(ui, "Behind the dialog");
                ui.add(ActionButton::new("Reset EEPROM").danger());
                let dialog = if danger {
                    ConfirmDialog::new("reset", "Reset all settings to defaults?")
                        .message(
                            "This sends M502, which restores the firmware defaults. Tuned \
                             values are lost unless they were saved elsewhere.",
                        )
                        .details("M502 ; factory reset")
                        .confirm_label("Reset")
                        .danger(true)
                } else {
                    ConfirmDialog::new("print", "Start printing benchy.gcode?")
                        .message("Estimated 1 h 42 min, 11.3 m of filament.")
                        .confirm_label("Start print")
                        .icon(icons::PLAY)
                };
                let _ = dialog.show(ui.ctx());
            });
            snapshot(&mut h, &name(base, theme));
        }
    }
}
