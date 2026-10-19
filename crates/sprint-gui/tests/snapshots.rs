//! Visual snapshots of every widget family in both themes.
//!
//! Regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p sprint-gui --test snapshots`.

mod common;

use common::{fit, snapshot};
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
