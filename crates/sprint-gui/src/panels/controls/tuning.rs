//! Live tuning, usable during prints: speed override (M220), flow per tool
//! (M221), fans (M106 / M107) and Z babystepping (M290).
//!
//! Sliders send their value when released (or on a keyboard change), never
//! while dragging, so a drag does not flood the printer with commands.

use std::collections::HashMap;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, EventFilter, FontId, Key, Modifiers, Pos2, Rect,
    Response, RichText, Sense, Stroke, Ui, WidgetInfo, vec2,
};
use sprint_host::{Printer, PrinterState};

use super::temperatures::{MAX_HOTENDS, hotend_count};
use super::{caption, section, split_row};
use crate::context::AppCtx;
use crate::icons;
use crate::theme::{self, TextRole, mix};
use crate::widgets::{IconButton, SegmentedControl, section_header, step_label};

/// Speed override range (%).
pub(super) const SPEED_RANGE: (f32, f32) = (10.0, 300.0);
/// Flow override range (%).
pub(super) const FLOW_RANGE: (f32, f32) = (50.0, 200.0);
/// Babystep sizes (mm).
pub(super) const BABYSTEPS: [f32; 3] = [0.01, 0.05, 0.1];
/// How long a sent value is shown before the printer's report wins (s).
const PENDING_SECS: f64 = 2.0;
/// The most fans offered.
const MAX_FANS: usize = 8;
/// Width of one babystep size segment.
const STEP_SEGMENT: f32 = 40.0;
/// The narrowest value readout (keeps "100%" and "5%" aligned).
const VALUE_MIN_WIDTH: f32 = 36.0;
/// Size of a small icon button (the resets).
const UNDO_SIZE: f32 = 24.0;

/// Fan percentage (0..=100) to the 0..=255 PWM value of M106.
pub(super) fn fan_pwm(percent: f32) -> u8 {
    if !percent.is_finite() {
        return 0;
    }
    (percent.clamp(0.0, 100.0) * 255.0 / 100.0).round() as u8
}

/// A 0..=255 fan PWM value as a percentage.
pub(super) fn fan_percent(pwm: u8) -> f32 {
    (f32::from(pwm) * 100.0 / 255.0).round()
}

/// A slider being dragged, or a value sent and not yet reported back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Knob {
    Speed,
    Flow(u8),
    Fan(u8),
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    value: f32,
    /// When it changed last (egui input time).
    at: f64,
    /// Still being edited (pointer down): not sent yet.
    live: bool,
}

/// UI state of the live tuning section.
#[derive(Debug)]
pub(super) struct Tuning {
    edits: HashMap<Knob, Pending>,
    /// Babystep size (mm).
    step: f32,
    /// Babystepping applied since connecting (mm).
    offset: f32,
    was_connected: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            edits: HashMap::new(),
            step: 0.05,
            offset: 0.0,
            was_connected: false,
        }
    }
}

/// Add `step` to a babystep offset, rounded to the micron so repeated
/// steps do not accumulate float noise.
pub(super) fn add_offset(offset: f32, step: f32) -> f32 {
    if !step.is_finite() {
        return offset;
    }
    ((offset + step) * 1000.0).round() / 1000.0
}

/// A signed offset: "+0.150 mm", "−0.050 mm", "0.000 mm".
pub(super) fn offset_label(offset: f32) -> String {
    if !offset.is_finite() || offset.abs() < 0.0005 {
        "0.000 mm".to_owned()
    } else if offset > 0.0 {
        format!("+{offset:.3} mm")
    } else {
        format!("\u{2212}{:.3} mm", offset.abs())
    }
}

/// Whether the firmware explicitly says it cannot babystep.
fn babystep_unsupported(state: &PrinterState) -> bool {
    state
        .capabilities
        .iter()
        .any(|(name, on)| !on && name.eq_ignore_ascii_case("BABYSTEPPING"))
}

/// Everything one slider row needs.
struct KnobSpec<'a> {
    knob: Knob,
    icon: &'a str,
    label: String,
    reported: f32,
    range: (f32, f32),
    /// The reset value and its tooltip.
    reset: (f32, &'a str),
    tooltip: String,
}

impl Tuning {
    /// Draw the section.
    pub(super) fn show(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let printer = ctx.printer;
        let state = ctx.state;
        let connected = ctx.connected();
        if self.was_connected && !connected {
            // A new connection starts without babystepping.
            self.offset = 0.0;
            self.edits.clear();
        }
        self.was_connected = connected;
        let hotends = hotend_count(state, ctx.config.active()).min(MAX_HOTENDS);
        let fans = state.fan_speeds.len().clamp(1, MAX_FANS);
        let now = ui.input(|i| i.time);
        section(
            ui,
            icons::TUNING,
            "Live tuning",
            |_| {},
            |ui| {
                ui.add_enabled_ui(connected, |ui| {
                    let speed = KnobSpec {
                        knob: Knob::Speed,
                        icon: icons::SPEED,
                        label: "Speed".to_owned(),
                        reported: state.feedrate_pct,
                        range: SPEED_RANGE,
                        reset: (100.0, "Reset to 100% (M220 S100)"),
                        tooltip: "Feedrate override (M220): scales every move".to_owned(),
                    };
                    if let Some(v) = self.knob_row(ui, &speed, now) {
                        printer.set_feedrate_override(v.round());
                    }
                    for tool in (0..hotends).filter_map(|t| u8::try_from(t).ok()) {
                        let spec = KnobSpec {
                            knob: Knob::Flow(tool),
                            icon: icons::FLOW,
                            label: if hotends > 1 {
                                format!("Flow T{tool}")
                            } else {
                                "Flow".to_owned()
                            },
                            reported: state
                                .flow_pct
                                .get(usize::from(tool))
                                .copied()
                                .unwrap_or(100.0),
                            range: FLOW_RANGE,
                            reset: (100.0, "Reset to 100% (M221 S100)"),
                            tooltip: format!("Extrusion flow of T{tool} (M221)"),
                        };
                        if let Some(v) = self.knob_row(ui, &spec, now) {
                            printer.set_flow(tool, v.round());
                        }
                    }
                    for fan in (0..fans).filter_map(|f| u8::try_from(f).ok()) {
                        let pwm = state.fan_speeds.get(usize::from(fan)).copied().unwrap_or(0);
                        let spec = KnobSpec {
                            knob: Knob::Fan(fan),
                            icon: icons::FAN,
                            label: if fans > 1 {
                                format!("Fan {fan}")
                            } else {
                                "Fan".to_owned()
                            },
                            reported: fan_percent(pwm),
                            range: (0.0, 100.0),
                            reset: (0.0, "Turn the fan off (M107)"),
                            tooltip: format!("Fan {fan} speed (M106 P{fan} S0–255)"),
                        };
                        if let Some(v) = self.knob_row(ui, &spec, now) {
                            printer.set_fan(fan, fan_pwm(v));
                        }
                    }
                    ui.add_space(2.0);
                    self.babystep(ui, printer, state);
                });
            },
        );
        // Let stale "sent" values fall back to the reports.
        self.edits
            .retain(|_, p| p.live || now - p.at < PENDING_SECS);
        if self.edits.values().any(|p| !p.live) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    /// The value a knob shows: the one being edited or just sent, else the
    /// printer's report.
    fn shown(&self, knob: Knob, reported: f32, now: f64) -> f32 {
        match self.edits.get(&knob) {
            Some(p) if p.live => p.value,
            Some(p) if now - p.at < PENDING_SECS && (p.value - reported).abs() >= 0.5 => p.value,
            _ => reported,
        }
    }

    /// One labelled slider; returns a value to send.
    fn knob_row(&mut self, ui: &mut Ui, spec: &KnobSpec<'_>, now: f64) -> Option<f32> {
        let (lo, hi) = spec.range;
        let reported = if spec.reported.is_finite() {
            spec.reported
        } else {
            spec.reset.0
        };
        let mut value = self.shown(spec.knob, reported, now).clamp(lo, hi);
        let mut send = None;
        ui.push_id(spec.knob, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let p = theme::ui_palette(ui);
            split_row(
                ui,
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new(spec.icon).color(p.text_dim).size(14.0));
                    ui.label(RichText::new(&spec.label).color(p.text).size(13.0))
                        .on_hover_text(&spec.tooltip);
                },
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let modified = (value - spec.reset.0).abs() >= 0.5;
                    if modified {
                        let r = ui.add(
                            IconButton::new(icons::RESET)
                                .ghost()
                                .small()
                                .tooltip(spec.reset.1),
                        );
                        if r.clicked() {
                            value = spec.reset.0;
                            send = Some(value);
                        }
                    } else {
                        ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                    }
                    value_label(ui, &format!("{}%", value.round()), modified);
                },
            );
            let r = slim_slider(ui, &mut value, spec.range, Some(spec.reset.0), &spec.label);
            let r = r.on_hover_text(&spec.tooltip);
            if r.changed() {
                self.edits.insert(
                    spec.knob,
                    Pending {
                        value,
                        at: now,
                        live: true,
                    },
                );
            }
            let editing = r.is_pointer_button_down_on() || r.dragged();
            if let Some(p) = self.edits.get_mut(&spec.knob)
                && p.live
                && !editing
            {
                p.live = false;
                p.at = now;
                send = Some(p.value);
            }
        });
        if let Some(v) = send {
            self.edits.insert(
                spec.knob,
                Pending {
                    value: v,
                    at: now,
                    live: false,
                },
            );
        }
        send
    }

    fn babystep(&mut self, ui: &mut Ui, printer: &Printer, state: &PrinterState) {
        section_header(ui, "Babystep Z");
        let unsupported = babystep_unsupported(state);
        ui.add_enabled_ui(!unsupported, |ui| {
            let p = theme::ui_palette(ui);
            let options: Vec<(f32, String)> =
                BABYSTEPS.iter().map(|s| (*s, step_label(*s))).collect();
            let offset = offset_label(self.offset);
            let undo_shown = self.offset != 0.0;
            split_row(
                ui,
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // The step selector is already placed: in a narrow panel
                    // the "Offset" caption gives way rather than overlap it.
                    let measure = |text: &str, font: FontId| {
                        ui.painter()
                            .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
                            .size()
                            .x
                    };
                    let caption = RichText::new("Offset").color(p.text_dim).size(13.0);
                    let needed = measure("Offset", FontId::proportional(13.0))
                        + 4.0
                        + measure(&offset, value_font(ui)).max(VALUE_MIN_WIDTH)
                        + if undo_shown { 4.0 + UNDO_SIZE } else { 0.0 };
                    if needed <= ui.available_width() {
                        ui.label(caption);
                    }
                    value_label(ui, &offset, self.offset != 0.0)
                        .on_hover_text("Babystepping applied since connecting");
                    if undo_shown {
                        let undo = format!(
                            "Undo all babystepping (M290 Z{})",
                            sprint_host::format_number(f64::from(-self.offset), 3)
                        );
                        let r = ui.add(IconButton::new(icons::RESET).ghost().small().tooltip(undo));
                        if r.clicked() {
                            let mut left = -self.offset;
                            while left.abs() > 1e-4 {
                                let chunk = left.clamp(-5.0, 5.0);
                                printer.babystep_z(chunk);
                                left -= chunk;
                            }
                            self.offset = 0.0;
                        }
                    }
                },
                |ui| {
                    ui.add(
                        SegmentedControl::new(&mut self.step, &options)
                            .segment_width(STEP_SEGMENT)
                            .id_salt("sprint-controls-babystep"),
                    )
                    .on_hover_text("Babystep size (mm)");
                },
            );
            let step = self.step;
            ui.columns(2, |cols| {
                let lower = cols[0].add(
                    crate::widgets::ActionButton::new("Lower")
                        .icon(icons::CARET_DOWN)
                        .fill_width(true)
                        .tooltip(format!(
                            "Nozzle {} mm closer to the bed (M290 Z−{})",
                            step_label(step),
                            step_label(step)
                        )),
                );
                if lower.clicked() {
                    printer.babystep_z(-step);
                    self.offset = add_offset(self.offset, -step);
                }
                let raise = cols[1].add(
                    crate::widgets::ActionButton::new("Raise")
                        .icon(icons::CARET_UP)
                        .fill_width(true)
                        .tooltip(format!(
                            "Nozzle {} mm further from the bed (M290 Z{})",
                            step_label(step),
                            step_label(step)
                        )),
                );
                if raise.clicked() {
                    printer.babystep_z(step);
                    self.offset = add_offset(self.offset, step);
                }
            });
        })
        .response
        .on_disabled_hover_text("The firmware reports no babystepping support");
        caption(
            ui,
            "Babysteps move the nozzle immediately, even mid-print. They are not saved.",
        );
    }
}

/// A slim slider for whole-number values: a thin track filled with the
/// accent up to the value, a small knob, and a tick at `mark` (the
/// default). Drag or click to set; when focused, arrow keys step by 1
/// (Shift: 10). `changed()` is set whenever the value changes.
fn slim_slider(
    ui: &mut Ui,
    value: &mut f32,
    (lo, hi): (f32, f32),
    mark: Option<f32>,
    label: &str,
) -> Response {
    let height = 20.0;
    let (rect, mut response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let knob_r = 7.0;
    let track = Rect::from_min_max(
        Pos2::new(rect.min.x + knob_r, rect.center().y - 2.0),
        Pos2::new(rect.max.x - knob_r, rect.center().y + 2.0),
    );
    let span = (hi - lo).max(f32::EPSILON);
    let to_x = |v: f32| track.min.x + ((v - lo) / span).clamp(0.0, 1.0) * track.width();
    let set = |value: &mut f32, response: &mut Response, new: f32| {
        let new = new.clamp(lo, hi).round();
        if new != *value {
            *value = new;
            response.mark_changed();
        }
    };
    if response.is_pointer_button_down_on()
        && let Some(pos) = response.interact_pointer_pos()
        && track.width() > 0.0
    {
        let t = ((pos.x - track.min.x) / track.width()).clamp(0.0, 1.0);
        set(value, &mut response, lo + t * span);
    }
    if response.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..Default::default()
                },
            );
        });
        let mut delta = 0.0;
        ui.input_mut(|i| {
            for (key, sign) in [
                (Key::ArrowRight, 1.0),
                (Key::ArrowUp, 1.0),
                (Key::ArrowLeft, -1.0),
                (Key::ArrowDown, -1.0),
            ] {
                delta += sign * i.count_and_consume_key(Modifiers::NONE, key) as f32;
                delta += sign * 10.0 * i.count_and_consume_key(Modifiers::SHIFT, key) as f32;
            }
        });
        if delta != 0.0 {
            set(value, &mut response, *value + delta);
        }
    }
    response.widget_info(|| WidgetInfo::slider(ui.is_enabled(), f64::from(*value), label));
    if ui.is_rect_visible(rect) {
        let p = theme::ui_palette(ui);
        let enabled = ui.is_enabled();
        let hot = enabled && (response.hovered() || response.is_pointer_button_down_on());
        let painter = ui.painter();
        let radius = CornerRadius::same(255);
        painter.rect_filled(
            track,
            radius,
            if p.dark { p.raised } else { p.raised_hover },
        );
        let x = to_x(*value);
        let fill = Rect::from_min_max(track.min, Pos2::new(x, track.max.y));
        if fill.width() > 0.5 {
            painter.rect_filled(fill, radius, p.accent);
        }
        if let Some(mark) = mark.filter(|m| m.is_finite() && *m > lo && *m < hi) {
            let mx = to_x(mark);
            let color = if mx <= x { p.on_accent } else { p.text_faint };
            painter.line_segment(
                [
                    Pos2::new(mx, track.min.y - 3.0),
                    Pos2::new(mx, track.max.y + 3.0),
                ],
                Stroke::new(1.5, mix(color, p.surface, 0.2)),
            );
        }
        let knob = Pos2::new(x, rect.center().y);
        let r = if hot { knob_r + 1.0 } else { knob_r };
        let (knob_fill, ring): (Color32, Color32) = if p.dark {
            (p.text, p.accent)
        } else {
            (Color32::WHITE, p.accent)
        };
        painter.circle(knob, r, knob_fill, Stroke::new(2.0, ring));
        if response.has_focus() {
            painter.circle_stroke(
                knob,
                r + 3.0,
                Stroke::new(2.0, theme::with_alpha(p.accent, 0.5)),
            );
        }
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A right-aligned numeric value; accent-colored when it differs from
/// the default.
fn value_label(ui: &mut Ui, text: &str, modified: bool) -> Response {
    let p = theme::ui_palette(ui);
    let font = value_font(ui);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), p.text);
    let (rect, response) = ui.allocate_exact_size(
        vec2(galley.size().x.max(VALUE_MIN_WIDTH), 20.0),
        Sense::hover(),
    );
    response.widget_info(|| WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    let color = if modified {
        if p.dark {
            p.accent_hover
        } else {
            p.accent_pressed
        }
    } else {
        p.text
    };
    ui.painter()
        .text(rect.right_center(), Align2::RIGHT_CENTER, text, font, color);
    response
}

/// The font of [`value_label`].
fn value_font(ui: &Ui) -> FontId {
    FontId::new(13.0, theme::font(ui, TextRole::Title).family)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_percent_maps_to_pwm() {
        assert_eq!(fan_pwm(0.0), 0);
        assert_eq!(fan_pwm(100.0), 255);
        assert_eq!(fan_pwm(50.0), 128);
        assert_eq!(fan_pwm(1.0), 3);
        assert_eq!(fan_pwm(-20.0), 0);
        assert_eq!(fan_pwm(250.0), 255);
        assert_eq!(fan_pwm(f32::NAN), 0);
        assert_eq!(fan_percent(0), 0.0);
        assert_eq!(fan_percent(255), 100.0);
        assert_eq!(fan_percent(128), 50.0);
        // Round trips stay on the same percentage.
        for pct in 0..=100 {
            assert_eq!(fan_percent(fan_pwm(pct as f32)), pct as f32);
        }
    }

    #[test]
    fn babystep_offsets_accumulate_cleanly() {
        let mut offset = 0.0;
        for _ in 0..10 {
            offset = add_offset(offset, 0.01);
        }
        assert_eq!(offset, 0.1);
        offset = add_offset(offset, -0.05);
        assert_eq!(offset, 0.05);
        assert_eq!(add_offset(offset, f32::NAN), 0.05);
        assert_eq!(offset_label(0.15), "+0.150 mm");
        assert_eq!(offset_label(-0.05), "\u{2212}0.050 mm");
        assert_eq!(offset_label(0.0), "0.000 mm");
        assert_eq!(offset_label(f32::NAN), "0.000 mm");
    }

    #[test]
    fn pending_values_win_until_reported() {
        let mut t = Tuning::default();
        assert_eq!(t.shown(Knob::Speed, 100.0, 0.0), 100.0);
        t.edits.insert(
            Knob::Speed,
            Pending {
                value: 150.0,
                at: 1.0,
                live: false,
            },
        );
        // Sent but not yet reported: show what was sent.
        assert_eq!(t.shown(Knob::Speed, 100.0, 1.5), 150.0);
        // Reported back, or too old: the report wins.
        assert_eq!(t.shown(Knob::Speed, 150.0, 1.5), 150.0);
        assert_eq!(t.shown(Knob::Speed, 100.0, 10.0), 100.0);
        // While dragging the slider value always wins.
        t.edits.insert(
            Knob::Speed,
            Pending {
                value: 42.0,
                at: 0.0,
                live: true,
            },
        );
        assert_eq!(t.shown(Knob::Speed, 100.0, 100.0), 42.0);
    }

    #[test]
    fn babystep_support_detection() {
        let mut state = PrinterState::default();
        assert!(!babystep_unsupported(&state));
        state.capabilities = vec![("BABYSTEPPING".into(), true)];
        assert!(!babystep_unsupported(&state));
        state.capabilities = vec![("babystepping".into(), false)];
        assert!(babystep_unsupported(&state));
    }
}
