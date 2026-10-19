//! Motion: position readout, XY jog pad + Z strip, homing, motors off,
//! absolute move-to, bed centering and (behind an explicit toggle)
//! keyboard jogging.

use egui::{
    Align2, Color32, CornerRadius, DragValue, Event, FontId, Key, Modifiers, Pos2, Rect, Sense,
    Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, vec2,
};
use sprint_host::sprint_proto::PositionReport;
use sprint_host::{Axes, Axis as HostAxis, Printer, PrinterProfile, PrinterState};

use super::{caption, notice, section, split_row};
use crate::context::AppCtx;
use crate::icons;
use crate::theme::{self, Palette, TextRole, Tone};
use crate::widgets::{
    ActionButton, Axis, HomeTarget, IconButton, JogAction, JogPad, SegmentedControl, Toggle, ZJog,
    format_number, section_header, step_label,
};

/// XY jog feedrate when the profile's is unusable (mm/min).
const DEFAULT_XY_FEED: f32 = 3000.0;
/// Z jog feedrate when the profile's is unusable (mm/min).
const DEFAULT_Z_FEED: f32 = 600.0;
/// Keyboard jog step sizes (mm).
const KEY_STEPS: [f32; 3] = [0.1, 1.0, 10.0];
/// The largest keyboard step (Shift × the biggest step).
const MAX_KEY_STEP: f32 = 100.0;

/// UI state of the motion section.
#[derive(Debug)]
pub(super) struct Motion {
    /// Keyboard jogging is on.
    keyboard: bool,
    /// Keyboard step in mm.
    key_step: f32,
    /// The move-to fields (X, Y, Z in mm).
    target: [f32; 3],
    /// `target` was initialised from the profile.
    seeded: bool,
    /// The card was open last frame: keyboard jogging only works while
    /// its controls are in view.
    open: bool,
}

impl Default for Motion {
    fn default() -> Self {
        Motion {
            keyboard: false,
            key_step: 1.0,
            target: [0.0, 0.0, 10.0],
            seeded: false,
            open: true,
        }
    }
}

/// The profile's jog feedrates (XY, Z) in mm/min, with sane fallbacks.
pub(super) fn feedrates(profile: &PrinterProfile) -> (f32, f32) {
    let usable = |f: f32, default: f32| {
        if f.is_finite() && f > 0.0 {
            f.min(100_000.0)
        } else {
            default
        }
    };
    (
        usable(profile.jog_feedrate_xy, DEFAULT_XY_FEED),
        usable(profile.jog_feedrate_z, DEFAULT_Z_FEED),
    )
}

/// The host axis of a widget axis.
pub(super) fn host_axis(axis: Axis) -> HostAxis {
    match axis {
        Axis::X => HostAxis::X,
        Axis::Y => HostAxis::Y,
        Axis::Z => HostAxis::Z,
        Axis::E => HostAxis::E,
    }
}

/// The axes a home target covers.
pub(super) fn home_axes(target: HomeTarget) -> Axes {
    match target {
        HomeTarget::X => Axes::X,
        HomeTarget::Y => Axes::Y,
        HomeTarget::Z => Axes::Z,
        HomeTarget::XY => Axes::XY,
        HomeTarget::All => Axes::ALL,
    }
}

/// The feedrate for jogging `axis`.
fn feed_for(axis: HostAxis, feeds: (f32, f32)) -> f32 {
    match axis {
        HostAxis::Z => feeds.1,
        _ => feeds.0,
    }
}

/// Send what a jog control asked for.
pub(super) fn apply_jog(printer: &Printer, action: JogAction, feeds: (f32, f32)) {
    match action {
        JogAction::Move { axis, distance } => {
            let axis = host_axis(axis);
            printer.jog(axis, distance, feed_for(axis, feeds));
        }
        JogAction::Home(target) => printer.home(home_axes(target)),
        JogAction::MotorsOff => printer.motors_off(),
    }
}

/// The middle of the bed in machine coordinates.
pub(super) fn bed_center(profile: &PrinterProfile) -> (f32, f32) {
    let (min, max) = profile.build_volume();
    ((min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0)
}

/// `target` clamped into the build volume (NaN becomes the minimum).
pub(super) fn clamp_to_volume(profile: &PrinterProfile, target: [f32; 3]) -> [f32; 3] {
    let (min, max) = profile.build_volume();
    let mut out = [0.0; 3];
    for i in 0..3 {
        let (lo, hi) = (min[i].min(max[i]), min[i].max(max[i]));
        out[i] = if target[i].is_finite() {
            target[i].clamp(lo, hi)
        } else {
            lo
        };
    }
    out
}

/// One absolute move: the axes it sets.
pub(super) type MoveStep = [Option<f32>; 3];

/// Split an absolute move so the nozzle never travels low: going up, Z
/// moves first and XY after; going down, XY first and Z last. Without a
/// known Z (or no Z change) it is one move.
pub(super) fn plan_move(current_z: Option<f32>, target: [f32; 3]) -> Vec<MoveStep> {
    let [x, y, z] = target;
    let xy: MoveStep = [Some(x), Some(y), None];
    let only_z: MoveStep = [None, None, Some(z)];
    match current_z.filter(|c| c.is_finite()) {
        Some(c) if z > c + 1e-3 => vec![only_z, xy],
        Some(c) if z < c - 1e-3 => vec![xy, only_z],
        _ => vec![[Some(x), Some(y), Some(z)]],
    }
}

/// Ask for the position after the machine was moved, unless the firmware
/// reports it by itself (AUTOREPORT_POS): the readout and the planning of
/// absolute moves need the real position, not the one before the move.
pub(super) fn refresh_position(printer: &Printer, state: &PrinterState) {
    if !state.has_capability("AUTOREPORT_POS") {
        printer.request_position();
    }
}

/// The jog for a key press: arrows move X/Y, Page Up/Down move Z, Shift
/// multiplies the step by ten.
pub(super) fn key_jog(key: Key, shift: bool, step: f32) -> Option<(HostAxis, f32)> {
    let step = if step.is_finite() && step > 0.0 {
        step
    } else {
        1.0
    };
    let step = if shift { step * 10.0 } else { step }.min(MAX_KEY_STEP);
    match key {
        Key::ArrowLeft => Some((HostAxis::X, -step)),
        Key::ArrowRight => Some((HostAxis::X, step)),
        Key::ArrowUp => Some((HostAxis::Y, step)),
        Key::ArrowDown => Some((HostAxis::Y, -step)),
        Key::PageUp => Some((HostAxis::Z, step)),
        Key::PageDown => Some((HostAxis::Z, -step)),
        _ => None,
    }
}

/// Take the jog keys out of this frame's events (so nothing else reacts to
/// them) and return the jogs they ask for. Auto-repeats are swallowed
/// without jogging: holding a key must not queue dozens of moves.
pub(super) fn take_jog_keys(events: &mut Vec<Event>, step: f32) -> Vec<(HostAxis, f32)> {
    let mut jogs = Vec::new();
    events.retain(|event| {
        let Event::Key {
            key,
            pressed,
            repeat,
            modifiers,
            ..
        } = event
        else {
            return true;
        };
        let plain = *modifiers == Modifiers::NONE;
        let shift = *modifiers == Modifiers::SHIFT;
        if !(plain || shift) {
            return true;
        }
        let Some(jog) = key_jog(*key, shift, step) else {
            return true;
        };
        if *pressed && !*repeat {
            jogs.push(jog);
        }
        false
    });
    jogs
}

impl Motion {
    /// Draw the section.
    pub(super) fn show(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let printer = ctx.printer;
        let state = ctx.state;
        let connected = ctx.connected();
        let lock = super::motion_lock(ctx);
        let job_active = ctx.job_active();
        let profile = ctx.config.active();
        let feeds = feedrates(profile);
        if !self.seeded {
            let (cx, cy) = bed_center(profile);
            self.target = clamp_to_volume(profile, [cx, cy, 10.0]);
            self.seeded = true;
        }
        // Whether anything here moved the machine this frame.
        let mut moved = false;
        if self.keyboard && self.open && lock.is_none() && keyboard_free(ui) {
            let jogs = ui.input_mut(|i| take_jog_keys(&mut i.events, self.key_step));
            for (axis, distance) in jogs {
                printer.jog(axis, distance, feed_for(axis, feeds));
                moved = true;
            }
        }
        let shown = section(
            ui,
            icons::MOVE,
            "Motion",
            |ui| {
                let refresh = ui.add_enabled(
                    connected,
                    IconButton::new(icons::REFRESH)
                        .ghost()
                        .small()
                        .tooltip("Refresh the position (M114)"),
                );
                if refresh.clicked() {
                    printer.request_position();
                }
            },
            |ui| {
                if job_active && connected {
                    let reason = lock.unwrap_or_default();
                    notice(ui, icons::LOCK, "Locked while a job runs", Tone::Warning)
                        .on_hover_text(reason);
                }
                position_readout(ui, state.position.as_ref().filter(|_| connected));
                ui.add_enabled_ui(lock.is_none(), |ui| {
                    moved |= self.jog_row(ui, printer, feeds, lock);
                    ui.add_space(2.0);
                    moved |=
                        self.move_to(ui, printer, profile, feeds, state.position.as_ref(), lock);
                });
                ui.add_space(2.0);
                self.keyboard_row(ui);
            },
        );
        self.open = shown.is_some();
        if moved {
            refresh_position(printer, state);
        }
    }

    /// The jog pad and Z strip; true when they moved the machine.
    fn jog_row(
        &self,
        ui: &mut Ui,
        printer: &Printer,
        feeds: (f32, f32),
        lock: Option<&str>,
    ) -> bool {
        let avail = ui.available_width();
        let z_width = if avail >= 270.0 { 60.0 } else { 50.0 };
        let gap = 10.0;
        let pad = (avail - z_width - gap).clamp(160.0, 264.0);
        let total = pad + gap + z_width;
        ui.horizontal(|ui| {
            let mut moved = false;
            ui.spacing_mut().item_spacing.x = gap;
            ui.add_space(((avail - total) / 2.0).max(0.0));
            let xy = JogPad::new()
                .size(pad)
                .id_salt("sprint-controls-jog-xy")
                .show(ui);
            let z = ZJog::new()
                .height(pad)
                .width(z_width)
                .id_salt("sprint-controls-jog-z")
                .show(ui);
            if let Some(reason) = lock {
                let _ = xy.response.clone().on_disabled_hover_text(reason);
                let _ = z.response.clone().on_disabled_hover_text(reason);
            }
            for action in [xy.action, z.action].into_iter().flatten() {
                apply_jog(printer, action, feeds);
                moved |= action != JogAction::MotorsOff;
            }
            moved
        })
        .inner
    }

    /// The move-to fields, "Move" and "Center"; true when they moved the
    /// machine.
    fn move_to(
        &mut self,
        ui: &mut Ui,
        printer: &Printer,
        profile: &PrinterProfile,
        feeds: (f32, f32),
        position: Option<&PositionReport>,
        lock: Option<&str>,
    ) -> bool {
        let mut moved = false;
        let (min, max) = profile.build_volume();
        section_header(ui, "Move to");
        ui.columns(3, |cols| {
            for (i, (col, letter)) in cols.iter_mut().zip(["X", "Y", "Z"]).enumerate() {
                let (lo, hi) = (min[i].min(max[i]), min[i].max(max[i]));
                let width = col.available_width();
                let r = col.add_sized(
                    [width, 28.0],
                    DragValue::new(&mut self.target[i])
                        .prefix(format!("{letter}  "))
                        .range(lo..=hi)
                        .speed(0.5)
                        .fixed_decimals(1),
                );
                let r = r.on_hover_text(format!(
                    "Absolute {letter} in mm ({} – {}). Drag or click to type.",
                    format_number(f64::from(lo), 0),
                    format_number(f64::from(hi), 0)
                ));
                if let Some(reason) = lock {
                    let _ = r.on_disabled_hover_text(reason);
                }
            }
        });
        let target = clamp_to_volume(profile, self.target);
        let (cx, cy) = bed_center(profile);
        let current_z = position.map(|p| p.z);
        ui.columns(2, |cols| {
            let go = cols[0].add(
                ActionButton::new("Move")
                    .icon(icons::MOVE)
                    .fill_width(true)
                    .tooltip(format!(
                        "Move to X{} Y{} Z{} (absolute)",
                        format_number(f64::from(target[0]), 1),
                        format_number(f64::from(target[1]), 1),
                        format_number(f64::from(target[2]), 1)
                    )),
            );
            if go.clicked() {
                for [x, y, z] in plan_move(current_z, target) {
                    let feed = if z.is_some() && x.is_none() {
                        feeds.1
                    } else {
                        feeds.0
                    };
                    printer.move_to(x, y, z, feed);
                }
                moved = true;
            }
            let center = cols[1].add(
                ActionButton::new("Center")
                    .icon(icons::POSITION)
                    .fill_width(true)
                    .tooltip(format!(
                        "Move to the bed center X{} Y{}",
                        format_number(f64::from(cx), 1),
                        format_number(f64::from(cy), 1)
                    )),
            );
            if center.clicked() {
                printer.move_to(Some(cx), Some(cy), None, feeds.0);
                moved = true;
            }
            if let Some(reason) = lock {
                let _ = go.on_disabled_hover_text(reason);
                let _ = center.on_disabled_hover_text(reason);
            }
        });
        moved
    }

    fn keyboard_row(&mut self, ui: &mut Ui) {
        let options: Vec<(f32, String)> = KEY_STEPS.iter().map(|s| (*s, step_label(*s))).collect();
        let on = self.keyboard;
        split_row(
            ui,
            |ui| {
                ui.add(Toggle::new(&mut self.keyboard).label("Keyboard jog"))
                    .on_hover_text(
                        "Jog with the keyboard: arrows move X/Y, Page Up/Down move Z, \
                         Shift moves ten times as far. Ignored while typing in a field, \
                         while a menu or dialog is open and while this card is collapsed.",
                    );
            },
            |ui| {
                if on {
                    ui.add(
                        SegmentedControl::new(&mut self.key_step, &options)
                            .segment_width(40.0)
                            .id_salt("sprint-controls-key-step"),
                    )
                    .on_hover_text("Keyboard step (mm)");
                }
            },
        );
        if self.keyboard {
            caption(ui, "← → X   ↑ ↓ Y   PgUp/PgDn Z   Shift ×10");
        }
    }
}

/// Keys may be taken: no widget (text field, slider, ...) has keyboard
/// focus and no modal dialog, menu or drop-down is open (their arrow keys
/// navigate, they must not move the machine).
fn keyboard_free(ui: &Ui) -> bool {
    let ctx = ui.ctx();
    ctx.memory(|m| m.focused().is_none() && m.top_modal_layer().is_none())
        && !egui::Popup::is_any_open(ctx)
}

/// Axis readout: four cells with the axis letter and its position.
fn position_readout(ui: &mut Ui, position: Option<&PositionReport>) {
    let p = theme::ui_palette(ui);
    let values = position.map(|pos| [pos.x, pos.y, pos.z, pos.e]);
    let height = 40.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let text = axis_value;
    let label = match values {
        Some(v) => format!(
            "Position X {} Y {} Z {} E {}",
            text(v[0]),
            text(v[1]),
            text(v[2]),
            text(v[3])
        ),
        None => "Position unknown".to_owned(),
    };
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &label));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(theme::RADIUS),
        p.input,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    let cell_w = rect.width() / 4.0;
    let caption_font = FontId::new(10.5, theme::font(ui, TextRole::Section).family);
    let value_font = theme::builtin_font(ui, egui::TextStyle::Monospace);
    for (i, letter) in ["X", "Y", "Z", "E"].into_iter().enumerate() {
        let cell = Rect::from_min_size(
            Pos2::new(rect.min.x + cell_w * i as f32, rect.min.y),
            vec2(cell_w, height),
        );
        if i > 0 {
            painter.vline(
                cell.min.x,
                (cell.min.y + 8.0)..=(cell.max.y - 8.0),
                Stroke::new(1.0, p.border),
            );
        }
        let clip = ui.painter().with_clip_rect(cell.shrink(2.0));
        clip.text(
            Pos2::new(cell.center().x, cell.min.y + 11.0),
            Align2::CENTER_CENTER,
            letter,
            caption_font.clone(),
            axis_color(&p, i),
        );
        let (value, color) = match values {
            Some(v) if v[i].is_finite() => (text(v[i]), p.text),
            _ => ("—".to_owned(), p.text_faint),
        };
        clip.text(
            Pos2::new(cell.center().x, cell.max.y - 13.0),
            Align2::CENTER_CENTER,
            value,
            value_font.clone(),
            color,
        );
    }
}

/// A position for the readout: two decimals, one from 1000 mm, scientific
/// notation for absurd values (so it fits its cell), "—" when not finite.
pub(super) fn axis_value(v: f32) -> String {
    if !v.is_finite() {
        "—".to_owned()
    } else if v.abs() >= 100_000.0 {
        format!("{v:.1e}")
    } else if v.abs() >= 1000.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// Axis letter colors: X, Y, Z in the familiar red/green/blue family, E dim.
fn axis_color(p: &Palette, index: usize) -> Color32 {
    match index {
        0 => p.danger,
        1 => p.success,
        2 => p.info,
        _ => p.text_dim,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::BedOrigin;

    #[test]
    fn feedrates_fall_back() {
        let mut profile = PrinterProfile::default();
        assert_eq!(feedrates(&profile), (3000.0, 600.0));
        profile.jog_feedrate_xy = f32::NAN;
        profile.jog_feedrate_z = -1.0;
        assert_eq!(feedrates(&profile), (DEFAULT_XY_FEED, DEFAULT_Z_FEED));
        profile.jog_feedrate_xy = 6000.0;
        profile.jog_feedrate_z = 1e9;
        assert_eq!(feedrates(&profile), (6000.0, 100_000.0));
    }

    #[test]
    fn home_targets_map_to_axes() {
        assert_eq!(home_axes(HomeTarget::All), Axes::ALL);
        assert_eq!(home_axes(HomeTarget::XY), Axes::XY);
        assert_eq!(home_axes(HomeTarget::Z), Axes::Z);
        assert_eq!(host_axis(Axis::Y), HostAxis::Y);
        assert_eq!(host_axis(Axis::E), HostAxis::E);
    }

    #[test]
    fn center_and_clamping_follow_the_origin() {
        let mut profile = PrinterProfile {
            bed_size: [235.0, 225.0],
            ..PrinterProfile::default()
        };
        assert_eq!(bed_center(&profile), (117.5, 112.5));
        assert_eq!(
            clamp_to_volume(&profile, [-5.0, 500.0, f32::NAN]),
            [0.0, 225.0, 0.0]
        );
        profile.origin = BedOrigin::Center;
        assert_eq!(bed_center(&profile), (0.0, 0.0));
        assert_eq!(
            clamp_to_volume(&profile, [-500.0, 10.0, 20.0]),
            [-117.5, 10.0, 20.0]
        );
    }

    #[test]
    fn moves_never_travel_low() {
        // Going up: lift first.
        assert_eq!(
            plan_move(Some(1.0), [10.0, 20.0, 30.0]),
            [[None, None, Some(30.0)], [Some(10.0), Some(20.0), None]]
        );
        // Going down: travel first.
        assert_eq!(
            plan_move(Some(30.0), [10.0, 20.0, 0.2]),
            [[Some(10.0), Some(20.0), None], [None, None, Some(0.2)]]
        );
        // Same height or unknown: one move.
        assert_eq!(
            plan_move(Some(5.0), [1.0, 2.0, 5.0]),
            [[Some(1.0), Some(2.0), Some(5.0)]]
        );
        assert_eq!(plan_move(None, [1.0, 2.0, 3.0]).len(), 1);
        assert_eq!(plan_move(Some(f32::NAN), [1.0, 2.0, 3.0]).len(), 1);
    }

    #[test]
    fn keys_map_to_jogs() {
        assert_eq!(
            key_jog(Key::ArrowLeft, false, 1.0),
            Some((HostAxis::X, -1.0))
        );
        assert_eq!(key_jog(Key::ArrowUp, false, 0.1), Some((HostAxis::Y, 0.1)));
        assert_eq!(
            key_jog(Key::PageDown, true, 1.0),
            Some((HostAxis::Z, -10.0))
        );
        assert_eq!(
            key_jog(Key::PageUp, true, 50.0),
            Some((HostAxis::Z, MAX_KEY_STEP))
        );
        assert_eq!(
            key_jog(Key::ArrowRight, false, f32::NAN),
            Some((HostAxis::X, 1.0))
        );
        assert_eq!(key_jog(Key::A, false, 1.0), None);
    }

    #[test]
    fn axis_values_fit_their_cells() {
        assert_eq!(axis_value(110.0), "110.00");
        assert_eq!(axis_value(-0.5), "-0.50");
        assert_eq!(axis_value(1234.56), "1234.6");
        assert_eq!(axis_value(-1e30), "-1.0e30");
        assert_eq!(axis_value(f32::MAX).len(), 6);
        assert_eq!(axis_value(f32::NAN), "—");
        assert_eq!(axis_value(f32::NEG_INFINITY), "—");
    }

    fn key(key: Key, pressed: bool, repeat: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        }
    }

    #[test]
    fn jog_keys_are_consumed_and_repeats_ignored() {
        let mut events = vec![
            key(Key::ArrowUp, true, false, Modifiers::NONE),
            key(Key::ArrowUp, true, true, Modifiers::NONE),
            key(Key::ArrowUp, false, false, Modifiers::NONE),
            key(Key::PageUp, true, false, Modifiers::SHIFT),
            // Ctrl+arrow and other keys belong to someone else.
            key(Key::ArrowLeft, true, false, Modifiers::CTRL),
            key(Key::Enter, true, false, Modifiers::NONE),
            Event::Text("x".into()),
        ];
        let jogs = take_jog_keys(&mut events, 1.0);
        assert_eq!(jogs, [(HostAxis::Y, 1.0), (HostAxis::Z, 10.0)]);
        assert_eq!(events.len(), 3);
    }
}
