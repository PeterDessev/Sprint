//! [`TemperatureCard`]: one heater at a glance, with an editable target.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Key, Layout, Pos2, Rect, Sense, Stroke,
    TextEdit, Ui, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::icons;
use crate::theme::{self, HeaterKind, Palette, TextRole, Tone, with_alpha};

use super::{ActionResponse, Badge, IconButton, Sparkline, card_frame, icon_font, status_dot};

/// What a heater is doing, derived from its current and target temperature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HeaterState {
    /// No valid reading.
    Unknown,
    /// Target is off and the heater is near ambient.
    Off,
    /// Below a set target.
    Heating,
    /// Above the target, or off but still hot.
    Cooling,
    /// Within the tolerance of the target.
    AtTarget,
}

/// Above this a heater whose target is off still counts as "cooling".
pub const HOT_THRESHOLD: f32 = 45.0;

impl HeaterState {
    /// Classify a reading. `tolerance` is the ± band (°C) that counts as
    /// "at target"; NaN or negative tolerances count as 0.
    pub fn classify(current: f32, target: f32, tolerance: f32) -> HeaterState {
        if !current.is_finite() {
            return HeaterState::Unknown;
        }
        let tolerance = if tolerance.is_finite() {
            tolerance.max(0.0)
        } else {
            0.0
        };
        if !target.is_finite() || target <= 0.0 {
            return if current > HOT_THRESHOLD {
                HeaterState::Cooling
            } else {
                HeaterState::Off
            };
        }
        if (current - target).abs() <= tolerance {
            HeaterState::AtTarget
        } else if current < target {
            HeaterState::Heating
        } else {
            HeaterState::Cooling
        }
    }

    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            HeaterState::Unknown => "No reading",
            HeaterState::Off => "Off",
            HeaterState::Heating => "Heating",
            HeaterState::Cooling => "Cooling",
            HeaterState::AtTarget => "At target",
        }
    }

    /// Semantic color of the state.
    pub fn tone(self) -> Tone {
        match self {
            HeaterState::Unknown | HeaterState::Off => Tone::Neutral,
            HeaterState::Heating => Tone::Warning,
            HeaterState::Cooling => Tone::Info,
            HeaterState::AtTarget => Tone::Success,
        }
    }
}

/// What the user asked a [`TemperatureCard`] to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TemperatureAction {
    /// Set the target (°C, already clamped to `0..=max_target`; 0 = off).
    SetTarget(f32),
    /// Turn the heater off.
    Off,
}

/// Parse a typed target: "215", "215.5", "215 °C", "215c". Commas are
/// accepted as decimal separators. Returns `None` for anything else.
pub fn parse_temperature(text: &str) -> Option<f32> {
    let t = text.trim();
    let t = t
        .strip_suffix("°C")
        .or_else(|| t.strip_suffix("°c"))
        .or_else(|| t.strip_suffix('C'))
        .or_else(|| t.strip_suffix('c'))
        .or_else(|| t.strip_suffix('°'))
        .unwrap_or(t)
        .trim()
        .replace(',', ".");
    if t.eq_ignore_ascii_case("off") {
        return Some(0.0);
    }
    t.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// A temperature as text: "215.3" (one decimal), "—" when unknown.
pub fn format_temperature(value: f32) -> String {
    if value.is_finite() {
        let s = format!("{value:.1}");
        if s == "-0.0" { "0.0".to_owned() } else { s }
    } else {
        "—".to_owned()
    }
}

/// A target as text: "215" (decimals only when needed), "Off" for 0.
fn format_target(value: f32) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "Off".to_owned();
    }
    let s = format!("{value:.1}");
    s.strip_suffix(".0").map_or(s.clone(), str::to_owned)
}

/// Edit buffer of the target field, kept in egui temp memory.
#[derive(Clone, Debug, Default)]
struct TargetEdit {
    text: String,
}

/// A heater card: name and icon, the current temperature in large type
/// colored by state, an editable target (Enter applies), a preset menu, an
/// off button, a power bar and a sparkline of recent readings.
///
/// ```no_run
/// # use sprint_gui::{theme::HeaterKind, widgets::{TemperatureAction, TemperatureCard}};
/// # fn demo(ui: &mut egui::Ui, history: &[f32]) {
/// let r = TemperatureCard::new("Hotend", HeaterKind::Hotend)
///     .current(212.4)
///     .target(215.0)
///     .power(Some(0.64))
///     .history(history)
///     .presets(&[("PLA", 210.0), ("PETG", 240.0)])
///     .show(ui);
/// match r.action {
///     Some(TemperatureAction::SetTarget(t)) => { /* M104 S{t} */ }
///     Some(TemperatureAction::Off) => { /* M104 S0 */ }
///     None => {}
/// }
/// # }
/// ```
#[must_use = "call `.show(ui)`"]
pub struct TemperatureCard<'a> {
    name: &'a str,
    kind: HeaterKind,
    current: f32,
    target: f32,
    power: Option<f32>,
    history: &'a [f32],
    presets: &'a [(&'a str, f32)],
    max_target: f32,
    tolerance: f32,
    id_salt: Option<Id>,
}

impl<'a> TemperatureCard<'a> {
    /// A card for a heater with no reading yet.
    pub fn new(name: &'a str, kind: HeaterKind) -> Self {
        TemperatureCard {
            name,
            kind,
            current: f32::NAN,
            target: 0.0,
            power: None,
            history: &[],
            presets: &[],
            max_target: match kind {
                HeaterKind::Hotend => 300.0,
                HeaterKind::Bed => 120.0,
                HeaterKind::Chamber => 80.0,
            },
            tolerance: 2.0,
            id_salt: None,
        }
    }

    /// Current temperature (°C; NaN = no reading).
    pub fn current(mut self, current: f32) -> Self {
        self.current = current;
        self
    }

    /// Target temperature (°C; 0 = off).
    pub fn target(mut self, target: f32) -> Self {
        self.target = target;
        self
    }

    /// Heater power 0..=1 (clamped), `None` hides the bar.
    pub fn power(mut self, power: Option<f32>) -> Self {
        self.power = power;
        self
    }

    /// Recent readings, oldest first (for the sparkline).
    pub fn history(mut self, history: &'a [f32]) -> Self {
        self.history = history;
        self
    }

    /// `(name, °C)` presets offered in the menu.
    pub fn presets(mut self, presets: &'a [(&'a str, f32)]) -> Self {
        self.presets = presets;
        self
    }

    /// Highest target the user may set (default by kind: 300 / 120 / 80).
    pub fn max_target(mut self, max: f32) -> Self {
        if max.is_finite() && max > 0.0 {
            self.max_target = max;
        }
        self
    }

    /// The ± band that counts as "at target" (default 2 °C).
    pub fn tolerance(mut self, tolerance: f32) -> Self {
        if tolerance.is_finite() && tolerance >= 0.0 {
            self.tolerance = tolerance;
        }
        self
    }

    /// Id salt (defaults to the name; set it when two heaters share one).
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(Id::new(salt));
        self
    }

    /// The state the card shows.
    pub fn state(&self) -> HeaterState {
        HeaterState::classify(self.current, self.target, self.tolerance)
    }

    /// Show the card.
    pub fn show(self, ui: &mut Ui) -> ActionResponse<TemperatureAction> {
        let p = theme::ui_palette(ui);
        let id = ui.make_persistent_id(
            self.id_salt
                .unwrap_or_else(|| Id::new(("sprint-temperature", self.name))),
        );
        let state = self.state();
        let heater = p.heater(self.kind);
        let mut action = None;
        let inner = card_frame(&p).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            // Below this width the target editor and the badge get a row of
            // their own instead of sharing rows with the name and readout.
            let wide = ui.available_width() >= 280.0;
            // Header: right-aligned controls first, then the name truncated
            // into whatever space is left.
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let off = ui.add_enabled(
                        self.target > 0.0,
                        IconButton::new(icons::OFF)
                            .ghost()
                            .small()
                            .tooltip(format!("Turn {} off", self.name.to_lowercase())),
                    );
                    if off.clicked() {
                        action = Some(TemperatureAction::Off);
                    }
                    if let Some(t) = self.preset_menu(ui) {
                        action = Some(TemperatureAction::SetTarget(t));
                    }
                    if wide {
                        ui.add_space(2.0);
                        ui.add(Badge::new(state.label()).tone(state.tone()));
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        heater_icon(ui, &p, self.kind, heater);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(self.name)
                                    .font(theme::font(ui, TextRole::Title))
                                    .color(p.text),
                            )
                            .truncate(),
                        );
                    });
                });
            });
            if wide {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(t) = self.target_editor(ui, &p, id, true) {
                            action = Some(TemperatureAction::SetTarget(t));
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            readout(ui, &p, self.current, state);
                        });
                    });
                });
            } else {
                readout(ui, &p, self.current, state);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(t) = self.target_editor(ui, &p, id, false) {
                            action = Some(TemperatureAction::SetTarget(t));
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let badge = Badge::new(state.label()).tone(state.tone());
                            if ui.available_width() >= badge.width(ui) {
                                ui.add(badge);
                            } else {
                                status_dot(ui, state.tone(), 8.0).on_hover_text(state.label());
                            }
                        });
                    });
                });
            }
            if let Some(power) = self.power {
                power_bar(ui, &p, power, heater);
            }
            if !self.history.is_empty() {
                ui.add(
                    Sparkline::new(self.history)
                        .color(heater)
                        .height(34.0)
                        .reference((self.target > 0.0).then_some(self.target))
                        .min_span(4.0),
                );
            }
        });
        let label = format!(
            "{} {} °C, target {}",
            self.name,
            format_temperature(self.current),
            format_target(self.target)
        );
        inner
            .response
            .widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, &label));
        ActionResponse::new(inner.response, action)
    }

    /// The preset dropdown button (if there are presets). Returns the
    /// chosen target.
    fn preset_menu(&self, ui: &mut Ui) -> Option<f32> {
        if self.presets.is_empty() {
            return None;
        }
        let button = ui.add(
            IconButton::new(icons::CARET_DOWN)
                .ghost()
                .small()
                .tooltip("Presets"),
        );
        let mut chosen = None;
        egui::Popup::menu(&button).show(|ui| {
            ui.set_min_width(150.0);
            for (name, temp) in self.presets {
                if !temp.is_finite() {
                    continue;
                }
                let temp = temp.clamp(0.0, self.max_target);
                let text = format!("{name}  ·  {} °C", format_target(temp));
                if ui.button(text).clicked() {
                    chosen = Some(temp);
                }
            }
        });
        chosen
    }

    /// The "Target [ 215 ] °C" editor. Returns a new target on Enter.
    fn target_editor(&self, ui: &mut Ui, p: &Palette, id: Id, caption: bool) -> Option<f32> {
        let edit_id = id.with("target");
        let mut edit: TargetEdit = ui.data(|d| d.get_temp(edit_id)).unwrap_or_default();
        let focused = ui.memory(|m| m.has_focus(edit_id));
        if !focused {
            edit.text = if self.target > 0.0 && self.target.is_finite() {
                format_target(self.target)
            } else {
                String::new()
            };
        }
        let mut result = None;
        ui.label(egui::RichText::new("°C").color(p.text_dim).size(13.0));
        let response = ui
            .scope(|ui| {
                let v = ui.visuals_mut();
                v.extreme_bg_color = p.input;
                v.text_edit_bg_color = Some(p.input);
                v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
                v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
                v.widgets.inactive.corner_radius = CornerRadius::same(theme::RADIUS);
                v.widgets.hovered.corner_radius = CornerRadius::same(theme::RADIUS);
                v.widgets.active.corner_radius = CornerRadius::same(theme::RADIUS);
                ui.add(
                    TextEdit::singleline(&mut edit.text)
                        .id(edit_id)
                        .desired_width(52.0)
                        .horizontal_align(Align::Max)
                        .hint_text("Off")
                        .font(FontId::proportional(15.0))
                        .margin(vec2(6.0, 4.0)),
                )
            })
            .inner
            .on_hover_text(format!(
                "Target temperature, 0 – {} °C. Type a value and press Enter.",
                format_target(self.max_target)
            ));
        if response.lost_focus() {
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                result = if edit.text.trim().is_empty() {
                    Some(0.0)
                } else {
                    parse_temperature(&edit.text).map(|t| t.clamp(0.0, self.max_target))
                };
            }
            // Applied or abandoned: show the real target again.
            edit.text = format_target(self.target);
        }
        let caption_text = egui::RichText::new("Target").color(p.text_dim).size(12.0);
        let label = if caption {
            ui.label(caption_text)
        } else {
            // Keep an accessible label without spending the space.
            ui.allocate_response(vec2(0.0, 0.0), Sense::hover())
        };
        let _ = response.labelled_by(label.id);
        ui.data_mut(|d| d.insert_temp(edit_id, edit));
        result
    }
}

fn heater_icon(ui: &mut Ui, p: &Palette, kind: HeaterKind, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().circle_filled(
            rect.center(),
            13.0,
            with_alpha(color, if p.dark { 0.18 } else { 0.14 }),
        );
        let icon = match kind {
            HeaterKind::Hotend => icons::HOTEND,
            HeaterKind::Bed => icons::BED,
            HeaterKind::Chamber => icons::CHAMBER,
        };
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon,
            icon_font(ui, 15.0),
            color,
        );
    }
}

/// The large current temperature colored by state, with its unit.
fn readout(ui: &mut Ui, p: &Palette, current: f32, state: HeaterState) {
    let color = match state {
        HeaterState::Unknown => p.text_faint,
        HeaterState::Off => p.text,
        HeaterState::Heating => p.warning,
        HeaterState::Cooling => p.info,
        HeaterState::AtTarget => p.success,
    };
    let big = theme::font(ui, TextRole::Display);
    let number = ui
        .painter()
        .layout_no_wrap(format_temperature(current), big, color);
    let unit = ui.painter().layout_no_wrap(
        "°C".to_owned(),
        FontId::new(15.0, theme::font(ui, TextRole::Title).family),
        p.text_dim,
    );
    let size = vec2(number.size().x + 4.0 + unit.size().x, number.size().y);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let baseline_gap = number.size().y - unit.size().y;
        ui.painter().galley(rect.min, number.clone(), color);
        ui.painter().galley(
            pos2(
                rect.min.x + number.size().x + 4.0,
                rect.min.y + baseline_gap * 0.78,
            ),
            unit,
            p.text_dim,
        );
    }
}

fn power_bar(ui: &mut Ui, p: &Palette, power: f32, color: Color32) {
    let power = if power.is_finite() {
        power.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let font = FontId::proportional(11.5);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 14.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.text(
        Pos2::new(rect.min.x, rect.center().y),
        Align2::LEFT_CENTER,
        icons::with_text(icons::POWER, "Power"),
        font.clone(),
        p.text_dim,
    );
    painter.text(
        Pos2::new(rect.max.x, rect.center().y),
        Align2::RIGHT_CENTER,
        format!("{}%", (power * 100.0).round() as u32),
        font,
        p.text,
    );
    let bar = Rect::from_min_max(
        pos2(rect.min.x + 66.0, rect.center().y - 2.5),
        pos2(rect.max.x - 40.0, rect.center().y + 2.5),
    );
    if bar.width() > 4.0 {
        let radius = CornerRadius::same(255);
        painter.rect_filled(bar, radius, if p.dark { p.raised } else { p.raised_hover });
        if power > 0.0 {
            let fill = Rect::from_min_size(bar.min, vec2((bar.width() * power).max(5.0), 5.0));
            painter.rect_filled(fill, radius, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_states() {
        use HeaterState::*;
        assert_eq!(HeaterState::classify(f32::NAN, 200.0, 2.0), Unknown);
        assert_eq!(HeaterState::classify(f32::INFINITY, 200.0, 2.0), Unknown);
        assert_eq!(HeaterState::classify(24.0, 0.0, 2.0), Off);
        assert_eq!(HeaterState::classify(120.0, 0.0, 2.0), Cooling);
        assert_eq!(HeaterState::classify(120.0, f32::NAN, 2.0), Cooling);
        assert_eq!(HeaterState::classify(150.0, 215.0, 2.0), Heating);
        assert_eq!(HeaterState::classify(213.5, 215.0, 2.0), AtTarget);
        assert_eq!(HeaterState::classify(216.9, 215.0, 2.0), AtTarget);
        assert_eq!(HeaterState::classify(230.0, 215.0, 2.0), Cooling);
        assert_eq!(HeaterState::classify(214.0, 215.0, f32::NAN), Heating);
        assert_eq!(HeaterState::classify(215.0, 215.0, -5.0), AtTarget);
    }

    #[test]
    fn parse_accepts_common_spellings() {
        assert_eq!(parse_temperature("215"), Some(215.0));
        assert_eq!(parse_temperature(" 215.5 "), Some(215.5));
        assert_eq!(parse_temperature("215,5"), Some(215.5));
        assert_eq!(parse_temperature("60 °C"), Some(60.0));
        assert_eq!(parse_temperature("60c"), Some(60.0));
        assert_eq!(parse_temperature("60°"), Some(60.0));
        assert_eq!(parse_temperature("off"), Some(0.0));
        assert_eq!(parse_temperature("OFF"), Some(0.0));
        assert_eq!(parse_temperature("hot"), None);
        assert_eq!(parse_temperature(""), None);
        assert_eq!(parse_temperature("NaN"), None);
        assert_eq!(parse_temperature("inf"), None);
        assert_eq!(parse_temperature("1e3"), Some(1000.0));
    }

    #[test]
    fn formatting() {
        assert_eq!(format_temperature(215.34), "215.3");
        assert_eq!(format_temperature(-0.01), "0.0");
        assert_eq!(format_temperature(f32::NAN), "—");
        assert_eq!(format_target(215.0), "215");
        assert_eq!(format_target(215.5), "215.5");
        assert_eq!(format_target(0.0), "Off");
        assert_eq!(format_target(-3.0), "Off");
        assert_eq!(format_target(f32::NAN), "Off");
    }

    #[test]
    fn default_limits_by_kind() {
        assert_eq!(
            TemperatureCard::new("a", HeaterKind::Hotend).max_target,
            300.0
        );
        assert_eq!(TemperatureCard::new("b", HeaterKind::Bed).max_target, 120.0);
        let c = TemperatureCard::new("c", HeaterKind::Chamber).max_target(f32::NAN);
        assert_eq!(c.max_target, 80.0);
    }
}
