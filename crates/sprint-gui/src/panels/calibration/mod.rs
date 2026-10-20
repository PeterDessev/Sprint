//! Calibration tab: step-by-step wizards for PID autotune, extruder
//! E-steps, the Z offset / first layer and flow (wall thickness).

mod esteps;
mod flow;
mod pid;
mod zoffset;

use egui::{
    Align, Align2, CornerRadius, FontId, Layout, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder,
    pos2, vec2,
};
use sprint_host::{Heater, PrinterState};

use crate::context::AppCtx;
use crate::icons;
use crate::insight::ui as kit;
use crate::theme::{self, TextRole, Tone, with_alpha};
use crate::widgets::{Badge, Chip, SegmentedControl};

/// What a running job disables.
const JOB_NOTE: &str = concat!(
    "Moves, extrusion and autotune are disabled until it ends. Babystepping stays ",
    "available.",
);

/// Shown while the firmware is halted.
const HALTED_NOTE: &str = concat!(
    "It ignores commands until its firmware restarts: restart it (M999) or reconnect ",
    "to run the wizards.",
);

pub use esteps::EstepsWizard;
pub use flow::FlowWizard;
pub use pid::PidWizard;
pub use zoffset::ZOffsetWizard;

/// The wizards, in display order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Wizard {
    /// PID autotune of a heater.
    #[default]
    Pid,
    /// Extruder steps per mm.
    Esteps,
    /// Z offset / first layer by babystepping.
    ZOffset,
    /// Flow from measured wall thickness.
    Flow,
}

impl Wizard {
    /// Every wizard.
    pub const ALL: [Wizard; 4] = [Wizard::Pid, Wizard::Esteps, Wizard::ZOffset, Wizard::Flow];

    /// Title.
    pub fn title(self) -> &'static str {
        match self {
            Wizard::Pid => "PID autotune",
            Wizard::Esteps => "Extruder E-steps",
            Wizard::ZOffset => "Z offset",
            Wizard::Flow => "Flow",
        }
    }

    /// Short title, for the switcher of a narrow tab.
    pub fn short_title(self) -> &'static str {
        match self {
            Wizard::Pid => "PID",
            Wizard::Esteps => "E-steps",
            Wizard::ZOffset => "Z offset",
            Wizard::Flow => "Flow",
        }
    }

    /// One-line description.
    pub fn description(self) -> &'static str {
        match self {
            Wizard::Pid => "Stable temperatures for a hotend or the bed",
            Wizard::Esteps => "Extrude exactly what the slicer asks for",
            Wizard::ZOffset => "Dial in the first layer by babystepping",
            Wizard::Flow => "Correct flow from measured wall thickness",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Wizard::Pid => icons::HEATING,
            Wizard::Esteps => icons::EXTRUDE,
            Wizard::ZOffset => icons::BABYSTEP,
            Wizard::Flow => icons::FLOW,
        }
    }
}

/// Calibration tab: PID autotune, E-steps and Z-offset wizards.
#[derive(Default)]
pub struct CalibrationTab {
    wizard: Wizard,
    pid: PidWizard,
    esteps: EstepsWizard,
    zoffset: ZOffsetWizard,
    flow: FlowWizard,
}

/// Below this width the wizard list becomes a segmented control.
const LIST_MIN_TOTAL: f32 = 760.0;
/// Width of the wizard list.
const LIST_WIDTH: f32 = 236.0;

impl CalibrationTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        // "Calibrate PID" on the Printer tab's thermal card.
        if let Some(heater) = kit::take_pid_calibration(ui.ctx()) {
            self.wizard = Wizard::Pid;
            self.pid.select(heater);
        }
        kit::page_header(
            ui,
            icons::CALIBRATION,
            "Calibration",
            Some("Guided tuning for heaters, extrusion and the first layer"),
            |_| {},
        );
        ui.add_space(theme::GAP);
        if !ctx.connected() {
            kit::notice(
                ui,
                Tone::Info,
                icons::INFO,
                "Not connected",
                Some("Connect to a printer to run the wizards. The flow calculator works offline."),
                None,
            );
            ui.add_space(theme::GAP);
        } else if ctx.state.halted {
            kit::notice(
                ui,
                Tone::Danger,
                icons::WARNING,
                "The printer is halted",
                Some(HALTED_NOTE),
                None,
            );
            ui.add_space(theme::GAP);
        } else if ctx.printer_busy() {
            kit::notice(
                ui,
                Tone::Warning,
                icons::WARNING,
                if ctx.job_active() {
                    "A job is running"
                } else {
                    "The printer prints from its card"
                },
                Some(JOB_NOTE),
                None,
            );
            ui.add_space(theme::GAP);
        }
        let avail = ui.available_rect_before_wrap();
        let wide = avail.width() >= LIST_MIN_TOTAL;
        let content_rect = if wide {
            let list_rect =
                Rect::from_min_max(avail.min, pos2(avail.min.x + LIST_WIDTH, avail.max.y));
            let mut list = ui.new_child(
                UiBuilder::new()
                    .id_salt("calibration-list")
                    .max_rect(list_rect)
                    .layout(Layout::top_down(Align::Min)),
            );
            self.wizard_list(&mut list, ctx);
            Rect::from_min_max(
                pos2(list_rect.max.x + theme::GAP_LG, avail.min.y),
                avail.max,
            )
        } else {
            avail
        };
        let mut content = ui.new_child(
            UiBuilder::new()
                .id_salt("calibration-content")
                .max_rect(content_rect)
                .layout(Layout::top_down(Align::Min)),
        );
        if !wide {
            self.switcher(&mut content);
            content.add_space(theme::GAP);
        }
        egui::ScrollArea::vertical()
            .id_salt(("calibration-scroll", self.wizard))
            .auto_shrink([false, false])
            .show(&mut content, |ui| {
                match self.wizard {
                    Wizard::Pid => self.pid.ui(ui, ctx),
                    Wizard::Esteps => self.esteps.ui(ui, ctx),
                    Wizard::ZOffset => self.zoffset.ui(ui, ctx),
                    Wizard::Flow => self.flow.ui(ui, ctx),
                }
                ui.add_space(theme::GAP);
            });
        ui.advance_cursor_after_rect(avail);
    }

    /// The wizard picker of a narrow tab: a segmented control with the
    /// titles, or the short titles, whichever fits; a drop-down when
    /// neither does. A control wider than the tab would widen every card
    /// below it past the tab's edge.
    fn switcher(&mut self, ui: &mut Ui) {
        let avail = ui.available_width();
        for title in [Wizard::title, Wizard::short_title] {
            let labels: Vec<&str> = Wizard::ALL.iter().map(|w| title(*w)).collect();
            if kit::segmented_width(ui, &labels) <= avail {
                let options: Vec<(Wizard, &str)> = Wizard::ALL
                    .iter()
                    .zip(labels)
                    .map(|(w, l)| (*w, l))
                    .collect();
                ui.add(SegmentedControl::new(&mut self.wizard, &options).fill_width(true));
                return;
            }
        }
        egui::ComboBox::from_id_salt("calibration-wizard")
            .selected_text(self.wizard.title())
            .width(avail)
            .show_ui(ui, |ui| {
                for w in Wizard::ALL {
                    ui.selectable_value(&mut self.wizard, w, w.title());
                }
            });
    }

    /// The wizard shown.
    pub fn wizard(&self) -> Wizard {
        self.wizard
    }

    /// Show a wizard.
    pub fn set_wizard(&mut self, wizard: Wizard) {
        self.wizard = wizard;
    }

    /// The heater the PID wizard tunes.
    pub fn pid_heater(&self) -> Heater {
        self.pid.heater()
    }

    fn status(&self, wizard: Wizard, state: &PrinterState) -> Option<(String, Tone)> {
        match wizard {
            Wizard::Pid => match &state.autotune {
                Some(a) if a.running => Some(("Running".into(), Tone::Accent)),
                Some(a) if a.error.is_some() => Some(("Failed".into(), Tone::Danger)),
                Some(a) if a.result.is_some() => Some(("Done".into(), Tone::Success)),
                _ => None,
            },
            Wizard::Esteps => self.esteps.status(),
            Wizard::ZOffset => self.zoffset.status(state),
            Wizard::Flow => None,
        }
    }

    fn wizard_list(&mut self, ui: &mut Ui, ctx: &AppCtx<'_>) {
        ui.spacing_mut().item_spacing.y = 6.0;
        for wizard in Wizard::ALL {
            let status = self.status(wizard, ctx.state);
            if wizard_tile(ui, wizard, self.wizard == wizard, status) {
                self.wizard = wizard;
            }
        }
    }
}

/// A selectable tile: icon, title, description and an optional status.
fn wizard_tile(
    ui: &mut Ui,
    wizard: Wizard,
    selected: bool,
    status: Option<(String, Tone)>,
) -> bool {
    let p = theme::ui_palette(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 62.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            wizard.title(),
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = CornerRadius::same(theme::RADIUS_LG);
        let (fill, stroke) = if selected {
            (p.accent_soft, Stroke::new(1.0, with_alpha(p.accent, 0.6)))
        } else if response.hovered() {
            (p.raised, Stroke::new(1.0, p.border_strong))
        } else {
            (p.surface, Stroke::new(1.0, p.border))
        };
        painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
        let icon_rect =
            Rect::from_center_size(pos2(rect.min.x + 26.0, rect.center().y), vec2(32.0, 32.0));
        painter.rect_filled(
            icon_rect,
            CornerRadius::same(theme::RADIUS),
            if selected {
                with_alpha(p.accent, 0.22)
            } else {
                p.raised
            },
        );
        painter.text(
            icon_rect.center(),
            Align2::CENTER_CENTER,
            wizard.icon(),
            kit::icon_font(ui, 17.0),
            if selected { p.accent } else { p.text_dim },
        );
        let x = icon_rect.max.x + 10.0;
        let mut text_end = rect.max.x - 10.0;
        if let Some((label, tone)) = &status {
            let badge = Badge::new(label).tone(*tone);
            let w = badge.width(ui);
            let badge_rect = Rect::from_min_size(
                pos2(rect.max.x - 10.0 - w, rect.min.y + 10.0),
                vec2(w, 18.0),
            );
            // A child Ui: `put` would move this Ui's cursor to the badge.
            ui.new_child(
                UiBuilder::new()
                    .max_rect(badge_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            )
            .add(badge);
            text_end = badge_rect.min.x - 6.0;
        }
        let title = kit::truncated(
            ui,
            wizard.title(),
            FontId::new(13.5, theme::font(ui, TextRole::Title).family),
            p.text,
            text_end - x,
        );
        let desc = kit::truncated(
            ui,
            wizard.description(),
            FontId::proportional(12.0),
            p.text_dim,
            rect.max.x - 10.0 - x,
        );
        let painter = ui.painter();
        painter.galley(pos2(x, rect.min.y + 12.0), title, p.text);
        painter.galley(pos2(x, rect.min.y + 34.0), desc, p.text_dim);
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// A horizontal step indicator: numbered circles joined by lines; steps
/// before `current` are done (check marks).
pub(crate) fn stepper(ui: &mut Ui, steps: &[&str], current: usize) {
    let p = theme::ui_palette(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 46.0), Sense::hover());
    if !ui.is_rect_visible(rect) || steps.is_empty() {
        return;
    }
    let n = steps.len();
    let seg = rect.width() / n as f32;
    let painter = ui.painter();
    let y = rect.min.y + 12.0;
    for (i, label) in steps.iter().enumerate() {
        let cx = rect.min.x + seg * (i as f32 + 0.5);
        if i + 1 < n {
            let next = rect.min.x + seg * (i as f32 + 1.5);
            let color = if i < current {
                p.success
            } else {
                p.border_strong
            };
            painter.line_segment(
                [pos2(cx + 14.0, y), pos2(next - 14.0, y)],
                Stroke::new(2.0, color),
            );
        }
        let (fill, fg, text) = if i < current {
            (p.success, p.text_on(p.success), icons::CHECK.to_owned())
        } else if i == current {
            (p.accent, p.on_accent, (i + 1).to_string())
        } else {
            (p.raised, p.text_dim, (i + 1).to_string())
        };
        painter.circle_filled(pos2(cx, y), 11.0, fill);
        if i > current {
            painter.circle_stroke(pos2(cx, y), 11.0, Stroke::new(1.0, p.border_strong));
        }
        let font = if i < current {
            kit::icon_font(ui, 13.0)
        } else {
            FontId::new(12.0, theme::font(ui, TextRole::Title).family)
        };
        painter.text(pos2(cx, y), Align2::CENTER_CENTER, text, font, fg);
        let label_font = if i == current {
            FontId::new(12.0, theme::font(ui, TextRole::Title).family)
        } else {
            FontId::proportional(12.0)
        };
        let g = kit::truncated(
            ui,
            label,
            label_font,
            if i == current { p.text } else { p.text_dim },
            seg - 6.0,
        );
        ui.painter()
            .galley(pos2(cx - g.size().x / 2.0, y + 16.0), g, p.text);
    }
}

/// Heaters to offer: hotends from the firmware (or the temperatures), the
/// bed and chamber when present.
pub(crate) fn heaters(state: &PrinterState, heated_bed: bool) -> Vec<Heater> {
    let hotends = state
        .firmware
        .as_ref()
        .and_then(|f| f.extruder_count)
        .map(usize::from)
        .unwrap_or(0)
        .max(state.temps.hotends.len())
        .clamp(1, 8);
    let mut out: Vec<Heater> = (0..hotends).map(|i| Heater::Hotend(i as u8)).collect();
    if state.temps.bed.is_some() || heated_bed {
        out.push(Heater::Bed);
    }
    if state.temps.chamber.is_some() {
        out.push(Heater::Chamber);
    }
    out
}

/// The current reading and target of a heater.
pub(crate) fn reading(state: &PrinterState, heater: Heater) -> (f32, f32) {
    let r = match heater {
        Heater::Hotend(t) => state.temps.hotends.get(usize::from(t)),
        Heater::Bed => state.temps.bed.as_ref(),
        Heater::Chamber => state.temps.chamber.as_ref(),
    };
    r.map_or((f32::NAN, 0.0), |r| (r.current, r.target.unwrap_or(0.0)))
}

/// Preset chips for a temperature; returns the clicked value.
pub(crate) fn preset_chips(ui: &mut Ui, presets: &[(String, f32)], current: f32) -> Option<f32> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for (name, value) in presets {
            if !value.is_finite() || *value <= 0.0 {
                continue;
            }
            let label = format!("{name} {value:.0}°");
            let selected = (current - value).abs() < 0.5;
            if ui.add(Chip::new(&label).selected(selected)).clicked() {
                picked = Some(*value);
            }
        }
    });
    picked
}

/// The last temperature-history time stamp (printer clock).
pub(crate) fn printer_time(state: &PrinterState) -> f64 {
    state
        .temp_history
        .back()
        .map(|s| s.t)
        .filter(|t| t.is_finite())
        .unwrap_or(state.link.connected_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heaters_from_state() {
        let mut state = PrinterState::default();
        assert_eq!(heaters(&state, false), vec![Heater::Hotend(0)]);
        assert_eq!(heaters(&state, true), vec![Heater::Hotend(0), Heater::Bed]);
        state.temps.hotends = vec![Default::default(), Default::default()];
        state.temps.chamber = Some(Default::default());
        assert_eq!(
            heaters(&state, true),
            vec![
                Heater::Hotend(0),
                Heater::Hotend(1),
                Heater::Bed,
                Heater::Chamber
            ]
        );
    }

    #[test]
    fn readings_and_time() {
        let mut state = PrinterState::default();
        assert!(reading(&state, Heater::Bed).0.is_nan());
        state.temps.bed = Some(sprint_proto::HeaterReading {
            current: 59.5,
            target: Some(60.0),
            power: None,
        });
        assert_eq!(reading(&state, Heater::Bed), (59.5, 60.0));
        state.link.connected_secs = 12.0;
        assert_eq!(printer_time(&state), 12.0);
    }

    #[test]
    fn wizard_names() {
        for w in Wizard::ALL {
            assert!(!w.title().is_empty());
            assert!(!w.description().is_empty());
        }
    }
}
