//! Temperatures: one [`TemperatureCard`] per heater, material presets that
//! set hotend and bed in one click, and "Cool down".

use std::ops::RangeInclusive;

use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Pos2, Response, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, WidgetType,
    text::{LayoutJob, TextWrapping},
    vec2,
};
use sprint_host::sprint_proto::HeaterReading;
use sprint_host::{Heater, Printer, PrinterProfile, PrinterState, TempPreset};

use super::section;
use crate::context::AppCtx;
use crate::icons;
use crate::theme::{self, HeaterKind, TextRole, mix};
use crate::widgets::{
    ActionButton, ConfirmDialog, DialogResult, TemperatureAction, TemperatureCard,
    format_temperature,
};

/// The most hotends shown (guards against absurd EXTRUDER_COUNT reports).
pub(super) const MAX_HOTENDS: usize = 8;

/// Seconds of history drawn in each card's sparkline.
const SPARKLINE_SECS: f64 = 120.0;

/// Readings outside this range are sensor faults, not temperatures (°C):
/// they show as "no reading" instead of a number that breaks the layout.
const PLAUSIBLE_READING: RangeInclusive<f32> = -273.0..=2000.0;

/// The hottest target Sprint offers or sends from presets (°C); hobby
/// hotends top out well below.
pub(super) const MAX_TARGET: f32 = 500.0;

/// A reading as shown: NaN unless it is a plausible temperature.
pub(super) fn plausible_reading(celsius: f32) -> f32 {
    if PLAUSIBLE_READING.contains(&celsius) {
        celsius
    } else {
        f32::NAN
    }
}

/// A reported target as shown: 0 (off) unless it is a plausible, positive
/// temperature.
pub(super) fn plausible_target(target: Option<f32>) -> f32 {
    target
        .filter(|t| *t > 0.0 && *t <= *PLAUSIBLE_READING.end())
        .unwrap_or(0.0)
}

/// The highest target a heater card accepts: the card's usual limit for
/// its kind (300 / 120 / 80 °C), raised to the hottest of `presets` and the
/// current `target` so the card never cuts down a temperature the profile's
/// presets set, and never above [`MAX_TARGET`].
pub(super) fn card_max_target(kind: HeaterKind, presets: &[(&str, f32)], target: f32) -> f32 {
    let usual = match kind {
        HeaterKind::Hotend => 300.0,
        HeaterKind::Bed => 120.0,
        HeaterKind::Chamber => 80.0,
    };
    presets
        .iter()
        .map(|(_, t)| *t)
        .chain([target])
        .filter(|t| t.is_finite())
        .fold(usual, f32::max)
        .min(MAX_TARGET)
}

/// Whether a preset temperature can be sent: 0 (off) up to [`MAX_TARGET`].
pub(super) fn valid_preset_temp(celsius: f32) -> bool {
    (0.0..=MAX_TARGET).contains(&celsius)
}

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

/// Current temperature of a heater (NaN without a plausible reading).
pub(super) fn current_temp(state: &PrinterState, heater: Heater) -> f32 {
    reading(state, heater).map_or(f32::NAN, |r| plausible_reading(r.current))
}

/// Target of a heater (0 = off or unknown).
pub(super) fn target_temp(state: &PrinterState, heater: Heater) -> f32 {
    plausible_target(reading(state, heater).and_then(|r| r.target))
}

/// Plausible readings of `heater` from the last `secs` seconds of history.
pub(super) fn heater_history(state: &PrinterState, heater: Heater, secs: f64) -> Vec<f32> {
    // The newest sample with a usable time sets the window.
    let Some(last) = state
        .temp_history
        .iter()
        .rev()
        .map(|s| s.t)
        .find(|t| t.is_finite())
    else {
        return Vec::new();
    };
    let since = last - secs;
    state
        .temp_history
        .iter()
        .filter(|s| s.t >= since && s.t <= last)
        .filter_map(|s| match heater {
            Heater::Hotend(tool) => s.hotends.get(usize::from(tool)).map(|h| h.0),
            Heater::Bed => s.bed.map(|b| b.0),
            Heater::Chamber => s.chamber.map(|c| c.0),
        })
        .filter(|v| PLAUSIBLE_READING.contains(v))
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
                current: reading.map_or(f32::NAN, |r| plausible_reading(r.current)),
                target: plausible_target(reading.and_then(|r| r.target)),
                power: reading
                    .and_then(|r| r.power)
                    .filter(|p| p.is_finite())
                    .map(|p| p.clamp(0.0, 1.0)),
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
/// printer has one, the bed. Invalid values (NaN, negative, above
/// [`MAX_TARGET`]) are skipped.
pub(super) fn preset_targets(
    preset: &TempPreset,
    tool: u8,
    heated_bed: bool,
) -> Vec<(Heater, f32)> {
    let valid = valid_preset_temp;
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

/// A temperature for display in a preset: "205", "off" for 0 (or a value
/// that is not sent).
fn short_temp(value: f32) -> String {
    if !valid_preset_temp(value) || value <= 0.0 {
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

/// Where a temperature change came from.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Change {
    /// A heater card (a typed target, its preset menu or its off button).
    Card,
    /// A preset button (named).
    Preset(String),
    /// "Cool down".
    CoolDown,
}

/// Whether a change must be confirmed while a print runs: turning off a
/// heater that is heating makes the print fail, and a preset button
/// changes several heaters with one click. Typed targets and targets that
/// stay the same go through.
pub(super) fn needs_confirmation(
    change: &Change,
    targets: &[(Heater, f32)],
    state: &PrinterState,
) -> bool {
    let current = |heater: Heater| target_temp(state, heater);
    match change {
        Change::Preset(_) => targets
            .iter()
            .any(|(heater, t)| (t - current(*heater)).abs() >= 0.5),
        Change::Card | Change::CoolDown => targets
            .iter()
            .any(|(heater, t)| *t <= 0.0 && current(*heater) > 0.0),
    }
}

/// A target for a confirmation: "215 °C" or "off".
fn target_label(celsius: f32) -> String {
    if celsius > 0.0 && celsius.is_finite() {
        format!("{} °C", format_temperature(celsius).trim_end_matches(".0"))
    } else {
        "off".to_owned()
    }
}

/// A temperature change waiting for confirmation because a job runs.
#[derive(Clone, Debug, PartialEq)]
struct Pending {
    title: String,
    /// One line per heater: "Hotend: 215 °C → off".
    details: String,
    /// Turns a heater off (red dialog, Enter does not confirm).
    danger: bool,
    targets: Vec<(Heater, f32)>,
}

impl Pending {
    fn new(
        change: &Change,
        targets: Vec<(Heater, f32)>,
        state: &PrinterState,
        hotends: usize,
    ) -> Self {
        let details = targets
            .iter()
            .map(|(heater, t)| {
                format!(
                    "{}: {} \u{2192} {}",
                    heater_name(*heater, hotends),
                    target_label(target_temp(state, *heater)),
                    target_label(*t)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let danger = targets.iter().any(|(_, t)| *t <= 0.0);
        let title = match change {
            Change::CoolDown => "Turn every heater off?".to_owned(),
            Change::Preset(name) => format!("Set the {name} temperatures?"),
            Change::Card => match targets.as_slice() {
                [(heater, _)] if danger => {
                    format!("Turn the {} off?", heater_name(*heater, hotends))
                }
                _ => "Change the temperatures?".to_owned(),
            },
        };
        Pending {
            title,
            details,
            danger,
            targets,
        }
    }
}

/// UI state of the temperatures section.
#[derive(Debug, Default)]
pub(super) struct Temperatures {
    /// A change waiting for "Are you sure?" while a print runs.
    pending: Option<Pending>,
}

impl Temperatures {
    /// Draw the section.
    pub(super) fn show(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let state = ctx.state;
        let hotends = hotend_count(state, ctx.config.active());
        if let Some((change, targets)) = show(ui, ctx) {
            // Also a print the printer runs from its card: it fails
            // without heat just the same.
            if ctx.printer_busy() && needs_confirmation(&change, &targets, state) {
                self.pending = Some(Pending::new(&change, targets, state, hotends));
            } else {
                apply(ctx.printer, &targets);
            }
        }
    }

    /// The confirmation of a change during a print, while one is open.
    pub(super) fn dialogs(&mut self, ui: &Ui, ctx: &mut AppCtx<'_>) {
        let Some(pending) = &self.pending else {
            return;
        };
        let (message, confirm) = if pending.danger {
            ("A print is running: without heat it fails.", "Turn off")
        } else {
            (
                "A print is running: the new targets apply to it right away.",
                "Set",
            )
        };
        let answer = ConfirmDialog::new("sprint-controls-temperatures", &pending.title)
            .message(message)
            .details(&pending.details)
            .confirm_label(confirm)
            .icon(icons::THERMOMETER)
            .danger(pending.danger)
            .show(ui.ctx());
        match answer {
            Some(DialogResult::Confirmed) => {
                if ctx.command_blocker().is_none() {
                    apply(ctx.printer, &pending.targets);
                }
                self.pending = None;
            }
            Some(DialogResult::Cancelled) => self.pending = None,
            None => {}
        }
    }
}

/// Draw the section; returns the change a click asked for.
fn show(ui: &mut Ui, ctx: &mut AppCtx<'_>) -> Option<(Change, Vec<(Heater, f32)>)> {
    let state = ctx.state;
    let connected = ctx.connected();
    // A halted firmware ignores heater commands (it switched them off).
    let ready = ctx.command_blocker().is_none();
    let profile = ctx.config.active();
    let views = heaters(state, profile, connected);
    let hotends = hotend_count(state, profile);
    let bed = has_bed(profile);
    let chamber = has_chamber(state, profile);
    let tool = state
        .active_tool
        .min(u8::try_from(hotends - 1).unwrap_or(0));
    let presets = &profile.temp_presets;
    let mut change = None;
    section(
        ui,
        icons::THERMOMETER,
        "Temperatures",
        |_| {},
        |ui| {
            ui.add_enabled_ui(ready, |ui| {
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
                        .filter(|(_, t)| *t > 0.0 && valid_preset_temp(*t))
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
                        .max_target(card_max_target(view.kind, &menu, view.target))
                        .id_salt(("sprint-controls-heater", view.heater))
                        .show(ui);
                    match card.action {
                        Some(TemperatureAction::SetTarget(t)) => {
                            change = Some((Change::Card, vec![(view.heater, t)]));
                        }
                        Some(TemperatureAction::Off) => {
                            change = Some((Change::Card, vec![(view.heater, 0.0)]));
                        }
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
                                change =
                                    Some((Change::Preset(preset.name.clone()), targets.clone()));
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
                    change = Some((Change::CoolDown, cool_down_targets(hotends, bed, chamber)));
                }
            });
        },
    );
    change
}

/// Horizontal padding inside a preset button (each side).
const PRESET_PADDING: f32 = 8.0;
/// Gap between a preset's name and its temperatures.
const PRESET_GAP: f32 = 5.0;
/// A preset name never gets narrower than this to make room for the
/// temperatures (they are dropped instead; the tooltip has them).
const PRESET_MIN_NAME: f32 = 28.0;

/// How a preset button fits its text into `inner` points: the width the
/// name is cut to (`None`: it fits) and whether the temperatures show.
/// The temperatures stay visible; a long name is cut to make room for
/// them, down to [`PRESET_MIN_NAME`].
pub(super) fn preset_fit(name: f32, detail: f32, inner: f32) -> (Option<f32>, bool) {
    if name + PRESET_GAP + detail <= inner {
        return (None, true);
    }
    let room = inner - PRESET_GAP - detail;
    if room >= PRESET_MIN_NAME {
        (Some(room), true)
    } else {
        ((name > inner).then_some(inner.max(0.0)), false)
    }
}

/// A button showing a preset's name and its temperatures ("PLA 205/60"),
/// `width` points wide.
fn preset_button(ui: &mut Ui, name: &str, detail: &str, width: f32) -> Response {
    let p = theme::ui_palette(ui);
    let name_font = FontId::new(13.0, theme::font(ui, TextRole::Title).family);
    let detail_font = FontId::proportional(12.0);
    let mut name_galley =
        ui.painter()
            .layout_no_wrap(name.to_owned(), name_font.clone(), Color32::PLACEHOLDER);
    let mut detail_galley =
        ui.painter()
            .layout_no_wrap(detail.to_owned(), detail_font, Color32::PLACEHOLDER);
    let (cut, show_detail) = preset_fit(
        name_galley.size().x,
        detail_galley.size().x,
        width - 2.0 * PRESET_PADDING,
    );
    if let Some(max) = cut {
        let mut job =
            LayoutJob::simple_singleline(name.to_owned(), name_font, Color32::PLACEHOLDER);
        job.wrap = TextWrapping::truncate_at_width(max);
        name_galley = ui.painter().layout_job(job);
    }
    if !show_detail {
        detail_galley = ui.painter().layout_no_wrap(
            String::new(),
            FontId::proportional(12.0),
            Color32::PLACEHOLDER,
        );
    }
    let gap = if show_detail { PRESET_GAP } else { 0.0 };
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
        assert!(preset_targets(&preset(1e9, 501.0), 0, true).is_empty());
        assert_eq!(
            preset_targets(&preset(MAX_TARGET, 0.0), 0, true),
            [(Heater::Hotend(0), MAX_TARGET), (Heater::Bed, 0.0)]
        );
    }

    #[test]
    fn implausible_reports_are_hidden() {
        assert_eq!(plausible_reading(215.0), 215.0);
        assert_eq!(plausible_reading(-15.0), -15.0);
        for bad in [f32::NAN, f32::INFINITY, f32::MAX, -300.0, 1e9] {
            assert!(plausible_reading(bad).is_nan(), "{bad}");
        }
        assert_eq!(plausible_target(Some(210.0)), 210.0);
        for bad in [None, Some(0.0), Some(-5.0), Some(f32::NAN), Some(1e30)] {
            assert_eq!(plausible_target(bad), 0.0, "{bad:?}");
        }
        let mut state = PrinterState::default();
        state.temps.hotends = vec![HeaterReading {
            current: f32::MAX,
            target: Some(f32::INFINITY),
            power: Some(7.0),
        }];
        state.temps.bed = Some(HeaterReading {
            current: 60.0,
            target: Some(60.0),
            power: Some(f32::NAN),
        });
        let views = heaters(&state, &PrinterProfile::default(), true);
        assert!(views[0].current.is_nan());
        assert_eq!(views[0].target, 0.0);
        assert_eq!(views[0].power, Some(1.0));
        assert_eq!(views[1].power, None);
        assert!(current_temp(&state, Heater::Hotend(0)).is_nan());
        assert_eq!(target_temp(&state, Heater::Hotend(0)), 0.0);
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
        // A corrupt newest time does not empty the sparkline.
        state.temp_history.push_back(TempSample {
            t: f64::NAN,
            hotends: vec![(1.0, 0.0)],
            ..TempSample::default()
        });
        assert_eq!(heater_history(&state, Heater::Hotend(0), 120.0).len(), 120);
    }

    #[test]
    fn changes_during_a_job_that_need_confirmation() {
        let mut state = PrinterState::default();
        state.temps.hotends = vec![HeaterReading {
            current: 210.0,
            target: Some(215.0),
            power: None,
        }];
        state.temps.bed = Some(HeaterReading {
            current: 22.0,
            target: Some(0.0),
            power: None,
        });
        let hot_off = [(Heater::Hotend(0), 0.0)];
        let bed_off = [(Heater::Bed, 0.0)];
        // Turning off a heater that heats: yes; one that is off: no.
        assert!(needs_confirmation(&Change::Card, &hot_off, &state));
        assert!(!needs_confirmation(&Change::Card, &bed_off, &state));
        assert!(needs_confirmation(
            &Change::CoolDown,
            &cool_down_targets(1, true, false),
            &state
        ));
        // A typed target goes through.
        assert!(!needs_confirmation(
            &Change::Card,
            &[(Heater::Hotend(0), 220.0)],
            &state
        ));
        // A preset asks when it changes a target.
        let pla = Change::Preset("PLA".into());
        let same = [(Heater::Hotend(0), 215.0), (Heater::Bed, 0.0)];
        assert!(!needs_confirmation(&pla, &same, &state));
        let hotter = [(Heater::Hotend(0), 215.0), (Heater::Bed, 60.0)];
        assert!(needs_confirmation(&pla, &hotter, &state));
        // Nothing heats: cooling down needs no question.
        let cold = PrinterState::default();
        assert!(!needs_confirmation(
            &Change::CoolDown,
            &cool_down_targets(2, true, true),
            &cold
        ));

        let pending = Pending::new(&Change::Card, hot_off.to_vec(), &state, 1);
        assert_eq!(pending.title, "Turn the Hotend off?");
        assert_eq!(pending.details, "Hotend: 215 °C \u{2192} off");
        assert!(pending.danger);
        let pending = Pending::new(&pla, hotter.to_vec(), &state, 2);
        assert_eq!(pending.title, "Set the PLA temperatures?");
        assert_eq!(
            pending.details,
            "Hotend T0: 215 °C \u{2192} 215 °C\nBed: off \u{2192} 60 °C"
        );
        assert!(!pending.danger);
        let pending = Pending::new(
            &Change::CoolDown,
            cool_down_targets(1, true, false),
            &state,
            1,
        );
        assert_eq!(pending.title, "Turn every heater off?");
        assert!(pending.danger);
    }

    #[test]
    fn cards_accept_what_the_presets_set() {
        let usual = card_max_target(HeaterKind::Hotend, &[("PLA", 205.0)], 0.0);
        assert_eq!(usual, 300.0);
        assert_eq!(card_max_target(HeaterKind::Bed, &[], 0.0), 120.0);
        assert_eq!(card_max_target(HeaterKind::Chamber, &[], f32::NAN), 80.0);
        // A hotter preset or target raises the limit ...
        let pc = [("PLA", 205.0), ("PC", 320.0)];
        assert_eq!(card_max_target(HeaterKind::Hotend, &pc, 0.0), 320.0);
        assert_eq!(
            card_max_target(HeaterKind::Bed, &[("PC", 130.0)], 0.0),
            130.0
        );
        assert_eq!(card_max_target(HeaterKind::Chamber, &[], 95.0), 95.0);
        // ... up to the most Sprint ever sends.
        assert_eq!(
            card_max_target(HeaterKind::Hotend, &[("Lava", 9000.0)], f32::INFINITY),
            MAX_TARGET
        );
    }

    #[test]
    fn long_preset_names_make_room_for_the_temperatures() {
        // Fits: nothing is cut.
        assert_eq!(preset_fit(30.0, 40.0, 100.0), (None, true));
        // A long name is cut so the temperatures stay.
        assert_eq!(preset_fit(150.0, 40.0, 100.0), (Some(55.0), true));
        // No room for both: the name alone, cut to the button.
        assert_eq!(preset_fit(150.0, 80.0, 100.0), (Some(100.0), false));
        assert_eq!(preset_fit(90.0, 80.0, 100.0), (None, false));
        assert_eq!(preset_fit(10.0, 10.0, -5.0), (Some(0.0), false));
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
        assert_eq!(short_temp(1e9), "off");
    }
}
