//! Temperatures: one [`TemperatureCard`] per heater, material presets that
//! set hotend and bed in one click, and "Cool down".

use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Pos2, Response, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, WidgetType, vec2,
};
use sprint_host::sprint_proto::HeaterReading;
use sprint_host::{Heater, Printer, PrinterProfile, PrinterState, TempPreset};

use super::section;
use crate::context::AppCtx;
use crate::icons;
use crate::theme::{self, HeaterKind, TextRole, mix};
use crate::widgets::{ActionButton, TemperatureAction, TemperatureCard, format_temperature};

/// The most hotends shown (guards against absurd EXTRUDER_COUNT reports).
pub(super) const MAX_HOTENDS: usize = 8;

/// Seconds of history drawn in each card's sparkline.
const SPARKLINE_SECS: f64 = 120.0;

/// Everything a heater card shows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct HeaterView {
    pub heater: Heater,
    pub name: String,
    pub kind: HeaterKind,
    /// °C, NaN without a reading.
    pub current: f32,
    /// °C, 0 = off.
    pub target: f32,
    /// 0..=1 when the firmware reports it.
    pub power: Option<f32>,
    /// Recent readings, oldest first.
    pub history: Vec<f32>,
}

/// How many hotends to show: the profile's extruder count, the firmware's
/// EXTRUDER_COUNT or the number of hotends in the temperature reports,
/// whichever is largest (1..=[`MAX_HOTENDS`]).
pub(super) fn hotend_count(state: &PrinterState, profile: &PrinterProfile) -> usize {
    let firmware = state
        .firmware
        .as_ref()
        .and_then(|f| f.extruder_count)
        .map_or(0, usize::from);
    usize::from(profile.extruders)
        .max(firmware)
        .max(state.temps.hotends.len())
        .clamp(1, MAX_HOTENDS)
}

/// Whether the bed gets a card: the profile says it is heated.
pub(super) fn has_bed(profile: &PrinterProfile) -> bool {
    profile.heated_bed
}

/// Whether the chamber gets a card: the profile says it is heated or the
/// firmware reports a chamber temperature.
pub(super) fn has_chamber(state: &PrinterState, profile: &PrinterProfile) -> bool {
    profile.heated_chamber || state.temps.chamber.is_some()
}

/// Display name of a heater ("Hotend", "Hotend T1", "Bed", "Chamber").
pub(super) fn heater_name(heater: Heater, hotends: usize) -> String {
    match heater {
        Heater::Hotend(0) if hotends <= 1 => "Hotend".to_owned(),
        Heater::Hotend(tool) => format!("Hotend T{tool}"),
        Heater::Bed => "Bed".to_owned(),
        Heater::Chamber => "Chamber".to_owned(),
    }
}

/// The latest reading of a heater.
pub(super) fn reading(state: &PrinterState, heater: Heater) -> Option<&HeaterReading> {
    match heater {
        Heater::Hotend(tool) => state.temps.hotends.get(usize::from(tool)),
        Heater::Bed => state.temps.bed.as_ref(),
        Heater::Chamber => state.temps.chamber.as_ref(),
    }
}

/// Finite readings of `heater` from the last `secs` seconds of history.
pub(super) fn heater_history(state: &PrinterState, heater: Heater, secs: f64) -> Vec<f32> {
    let Some(last) = state.temp_history.back() else {
        return Vec::new();
    };
    let since = last.t - secs;
    state
        .temp_history
        .iter()
        .filter(|s| s.t >= since)
        .filter_map(|s| match heater {
            Heater::Hotend(tool) => s.hotends.get(usize::from(tool)).map(|h| h.0),
            Heater::Bed => s.bed.map(|b| b.0),
            Heater::Chamber => s.chamber.map(|c| c.0),
        })
        .filter(|v| v.is_finite())
        .collect()
}

/// Every heater the sidebar shows, in display order: hotends, bed, chamber.
/// Without a connection the readings are hidden (they would be stale).
pub(super) fn heaters(
    state: &PrinterState,
    profile: &PrinterProfile,
    connected: bool,
) -> Vec<HeaterView> {
    let hotends = hotend_count(state, profile);
    let mut list: Vec<(Heater, HeaterKind)> = (0..hotends)
        .filter_map(|i| u8::try_from(i).ok())
        .map(|i| (Heater::Hotend(i), HeaterKind::Hotend))
        .collect();
    if has_bed(profile) {
        list.push((Heater::Bed, HeaterKind::Bed));
    }
    if has_chamber(state, profile) {
        list.push((Heater::Chamber, HeaterKind::Chamber));
    }
    list.into_iter()
        .map(|(heater, kind)| {
            let reading = reading(state, heater).filter(|_| connected);
            HeaterView {
                heater,
                name: heater_name(heater, hotends),
                kind,
                current: reading.map_or(f32::NAN, |r| r.current),
                target: reading
                    .and_then(|r| r.target)
                    .filter(|t| t.is_finite() && *t > 0.0)
                    .unwrap_or(0.0),
                power: reading.and_then(|r| r.power).filter(|p| p.is_finite()),
                history: if connected {
                    heater_history(state, heater, SPARKLINE_SECS)
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

/// What one click on a preset sets: the hotend of `tool` and, when the
/// printer has one, the bed. Invalid (NaN, negative) values are skipped.
pub(super) fn preset_targets(
    preset: &TempPreset,
    tool: u8,
    heated_bed: bool,
) -> Vec<(Heater, f32)> {
    let valid = |t: f32| t.is_finite() && t >= 0.0;
    let mut targets = Vec::with_capacity(2);
    if valid(preset.hotend) {
        targets.push((Heater::Hotend(tool), preset.hotend));
    }
    if heated_bed && valid(preset.bed) {
        targets.push((Heater::Bed, preset.bed));
    }
    targets
}

/// "Cool down": every heater off.
pub(super) fn cool_down_targets(hotends: usize, bed: bool, chamber: bool) -> Vec<(Heater, f32)> {
    let mut targets: Vec<(Heater, f32)> = (0..hotends.min(MAX_HOTENDS))
        .filter_map(|i| u8::try_from(i).ok())
        .map(|i| (Heater::Hotend(i), 0.0))
        .collect();
    if bed {
        targets.push((Heater::Bed, 0.0));
    }
    if chamber {
        targets.push((Heater::Chamber, 0.0));
    }
    targets
}

/// A temperature for display in a preset: "205", "Off" for 0.
fn short_temp(value: f32) -> String {
    if !value.is_finite() || value <= 0.0 {
        "off".to_owned()
    } else {
        format!("{value:.0}")
    }
}

fn apply(printer: &Printer, targets: &[(Heater, f32)]) {
    for (heater, celsius) in targets {
        printer.set_temperature(*heater, *celsius);
    }
}

/// Draw the section.
pub(super) fn show(ui: &mut Ui, ctx: &mut AppCtx<'_>) {
    let printer = ctx.printer;
    let state = ctx.state;
    let connected = ctx.connected();
    let profile = ctx.config.active();
    let views = heaters(state, profile, connected);
    let hotends = hotend_count(state, profile);
    let bed = has_bed(profile);
    let chamber = has_chamber(state, profile);
    let tool = state
        .active_tool
        .min(u8::try_from(hotends - 1).unwrap_or(0));
    let presets = &profile.temp_presets;
    section(
        ui,
        icons::THERMOMETER,
        "Temperatures",
        |_| {},
        |ui| {
            ui.add_enabled_ui(connected, |ui| {
                for view in &views {
                    let menu: Vec<(&str, f32)> = presets
                        .iter()
                        .map(|p| {
                            let t = match view.kind {
                                HeaterKind::Bed => p.bed,
                                _ => p.hotend,
                            };
                            (p.name.as_str(), t)
                        })
                        .filter(|(_, t)| t.is_finite() && *t > 0.0)
                        .collect();
                    let menu = if view.kind == HeaterKind::Chamber {
                        Vec::new()
                    } else {
                        menu
                    };
                    let card = TemperatureCard::new(&view.name, view.kind)
                        .current(view.current)
                        .target(view.target)
                        .power(view.power)
                        .history(&view.history)
                        .presets(&menu)
                        .id_salt(("sprint-controls-heater", view.heater))
                        .show(ui);
                    match card.action {
                        Some(TemperatureAction::SetTarget(t)) => {
                            printer.set_temperature(view.heater, t);
                        }
                        Some(TemperatureAction::Off) => printer.set_temperature(view.heater, 0.0),
                        None => {}
                    }
                }
                ui.add_space(2.0);
                let buttons: Vec<_> = presets
                    .iter()
                    .filter_map(|preset| {
                        let targets = preset_targets(preset, tool, bed);
                        (!targets.is_empty()).then_some((preset, targets))
                    })
                    .collect();
                let gap = 6.0;
                let width = ui.available_width();
                let columns = if width >= 360.0 { 4 } else { 2 };
                let cell = ((width - gap * (columns - 1) as f32) / columns as f32).max(40.0);
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                for row in buttons.chunks(columns) {
                    ui.horizontal(|ui| {
                        for (preset, targets) in row {
                            let detail = if bed {
                                format!("{}/{}", short_temp(preset.hotend), short_temp(preset.bed))
                            } else {
                                short_temp(preset.hotend)
                            };
                            let tip = targets
                                .iter()
                                .map(|(h, t)| {
                                    format!(
                                        "{} {} °C",
                                        heater_name(*h, hotends),
                                        format_temperature(*t).trim_end_matches(".0")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            let r = preset_button(ui, &preset.name, &detail, cell)
                                .on_hover_text(format!("Preheat {}: {tip}", preset.name));
                            if r.clicked() {
                                apply(printer, targets);
                            }
                        }
                    });
                }
                let cool = ui.add(
                    ActionButton::new("Cool down")
                        .icon(icons::COOLING)
                        .fill_width(true)
                        .tooltip("Turn every heater off"),
                );
                if cool.clicked() {
                    apply(printer, &cool_down_targets(hotends, bed, chamber));
                }
            });
        },
    );
}

/// A button showing a preset's name and its temperatures ("PLA 205/60"),
/// `width` points wide.
fn preset_button(ui: &mut Ui, name: &str, detail: &str, width: f32) -> Response {
    let p = theme::ui_palette(ui);
    let name_font = FontId::new(13.0, theme::font(ui, TextRole::Title).family);
    let detail_font = FontId::proportional(12.0);
    let name_galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), name_font, Color32::PLACEHOLDER);
    let detail_galley =
        ui.painter()
            .layout_no_wrap(detail.to_owned(), detail_font, Color32::PLACEHOLDER);
    let gap = 5.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            ui.is_enabled(),
            format!("Preset {name}"),
        )
    });
    if ui.is_rect_visible(rect) {
        let hover = if response.hovered() && ui.is_enabled() {
            1.0
        } else {
            0.0
        };
        let pressed = response.is_pointer_button_down_on();
        let fill = if pressed {
            mix(p.raised, p.bg, 0.35)
        } else {
            mix(p.raised, p.raised_hover, hover)
        };
        let stroke = Stroke::new(1.0, mix(p.border, p.accent, hover * 0.7));
        let radius = CornerRadius::same(theme::RADIUS);
        let painter = ui.painter_at(rect);
        painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
        let y = rect.center().y;
        let content = name_galley.size().x + gap + detail_galley.size().x;
        let x = rect.center().x - content.min(rect.width() - 8.0) / 2.0;
        let name_pos = Pos2::new(x, y - name_galley.size().y / 2.0);
        let detail_pos = Pos2::new(
            name_pos.x + name_galley.size().x + gap,
            y - detail_galley.size().y / 2.0,
        );
        painter.galley(name_pos, name_galley, p.text);
        painter.galley(detail_pos, detail_galley, p.text_dim);
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::TempSample;
    use sprint_host::sprint_proto::{FirmwareInfo, TempReport};

    fn preset(hotend: f32, bed: f32) -> TempPreset {
        TempPreset {
            name: "PLA".into(),
            hotend,
            bed,
        }
    }

    #[test]
    fn preset_sets_active_hotend_and_bed() {
        assert_eq!(
            preset_targets(&preset(205.0, 60.0), 0, true),
            [(Heater::Hotend(0), 205.0), (Heater::Bed, 60.0)]
        );
        assert_eq!(
            preset_targets(&preset(240.0, 80.0), 1, true),
            [(Heater::Hotend(1), 240.0), (Heater::Bed, 80.0)]
        );
        // No heated bed: hotend only.
        assert_eq!(
            preset_targets(&preset(205.0, 60.0), 0, false),
            [(Heater::Hotend(0), 205.0)]
        );
        // Zero is a valid "off" target; garbage is skipped.
        assert_eq!(
            preset_targets(&preset(0.0, f32::NAN), 0, true),
            [(Heater::Hotend(0), 0.0)]
        );
        assert!(preset_targets(&preset(-5.0, f32::INFINITY), 0, true).is_empty());
    }

    #[test]
    fn cool_down_turns_everything_off() {
        assert_eq!(
            cool_down_targets(2, true, true),
            [
                (Heater::Hotend(0), 0.0),
                (Heater::Hotend(1), 0.0),
                (Heater::Bed, 0.0),
                (Heater::Chamber, 0.0)
            ]
        );
        assert_eq!(
            cool_down_targets(1, false, false),
            [(Heater::Hotend(0), 0.0)]
        );
        assert_eq!(cool_down_targets(1000, false, false).len(), MAX_HOTENDS);
    }

    #[test]
    fn hotend_count_takes_the_largest_source() {
        let mut state = PrinterState::default();
        let mut profile = PrinterProfile::default();
        assert_eq!(hotend_count(&state, &profile), 1);
        profile.extruders = 0;
        assert_eq!(hotend_count(&state, &profile), 1);
        state.firmware = Some(FirmwareInfo {
            extruder_count: Some(2),
            ..FirmwareInfo::default()
        });
        assert_eq!(hotend_count(&state, &profile), 2);
        state.temps.hotends = vec![HeaterReading::default(); 3];
        assert_eq!(hotend_count(&state, &profile), 3);
        profile.extruders = 200;
        assert_eq!(hotend_count(&state, &profile), MAX_HOTENDS);
    }

    #[test]
    fn heaters_follow_profile_and_reports() {
        let mut state = PrinterState::default();
        let mut profile = PrinterProfile::default();
        state.temps = TempReport {
            hotends: vec![HeaterReading {
                current: 200.0,
                target: Some(210.0),
                power: Some(0.5),
            }],
            bed: Some(HeaterReading {
                current: 59.0,
                target: Some(60.0),
                power: None,
            }),
            ..TempReport::default()
        };
        let views = heaters(&state, &profile, true);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].name, "Hotend");
        assert_eq!(views[0].target, 210.0);
        assert_eq!(views[0].power, Some(0.5));
        assert_eq!(views[1].heater, Heater::Bed);
        // A reported chamber appears even when the profile has none.
        state.temps.chamber = Some(HeaterReading {
            current: 30.0,
            target: None,
            power: None,
        });
        profile.heated_bed = false;
        let views = heaters(&state, &profile, true);
        let kinds: Vec<_> = views.iter().map(|v| v.heater).collect();
        assert_eq!(kinds, [Heater::Hotend(0), Heater::Chamber]);
        assert_eq!(views[1].target, 0.0);
        // Offline: no stale readings.
        let offline = heaters(&state, &profile, false);
        assert!(offline[0].current.is_nan());
        assert_eq!(offline[0].target, 0.0);
    }

    #[test]
    fn history_is_windowed_and_finite() {
        let mut state = PrinterState::default();
        for i in 0..300 {
            state.temp_history.push_back(TempSample {
                t: f64::from(i),
                hotends: vec![(if i == 250 { f32::NAN } else { i as f32 }, 0.0)],
                bed: (i % 2 == 0).then_some((50.0, 60.0)),
                ..TempSample::default()
            });
        }
        let h = heater_history(&state, Heater::Hotend(0), 120.0);
        assert_eq!(h.len(), 120); // 179..=299 minus the NaN
        assert_eq!(h.first(), Some(&179.0));
        assert_eq!(heater_history(&state, Heater::Bed, 10.0).len(), 5);
        assert!(heater_history(&state, Heater::Chamber, 10.0).is_empty());
        assert!(heater_history(&PrinterState::default(), Heater::Bed, 10.0).is_empty());
    }

    #[test]
    fn names_and_temps_format() {
        assert_eq!(heater_name(Heater::Hotend(0), 1), "Hotend");
        assert_eq!(heater_name(Heater::Hotend(0), 2), "Hotend T0");
        assert_eq!(heater_name(Heater::Hotend(1), 2), "Hotend T1");
        assert_eq!(heater_name(Heater::Bed, 2), "Bed");
        assert_eq!(short_temp(205.0), "205");
        assert_eq!(short_temp(0.0), "off");
        assert_eq!(short_temp(f32::NAN), "off");
    }
}
