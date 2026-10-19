//! Motion controls: the circular XY [`JogPad`], the vertical [`ZJog`] strip
//! and the [`ExtruderControl`].
//!
//! The controls only report what the user asked for ([`JogAction`],
//! [`ExtruderAction`]); turning that into G-code (relative moves,
//! feedrates, M84, ...) is the host's job.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Mesh, Pos2, Rect, Response, Sense,
    Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, epaint::PathShape, pos2, vec2,
};

use crate::icons;
use crate::theme::{self, Palette, TextRole, mix};

use super::{ActionButton, ActionResponse, NumericField, icon_font, paint_focus_ring};

/// A machine axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    /// X.
    X,
    /// Y.
    Y,
    /// Z.
    Z,
    /// Extruder.
    E,
}

impl Axis {
    /// The G-code letter.
    pub fn letter(self) -> char {
        match self {
            Axis::X => 'X',
            Axis::Y => 'Y',
            Axis::Z => 'Z',
            Axis::E => 'E',
        }
    }
}

/// Which axes a home command covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HomeTarget {
    /// X only (`G28 X`).
    X,
    /// Y only (`G28 Y`).
    Y,
    /// Z only (`G28 Z`).
    Z,
    /// X and Y (`G28 X Y`).
    XY,
    /// Every axis (`G28`).
    All,
}

impl HomeTarget {
    /// The homing command for this target.
    pub fn gcode(self) -> &'static str {
        match self {
            HomeTarget::X => "G28 X",
            HomeTarget::Y => "G28 Y",
            HomeTarget::Z => "G28 Z",
            HomeTarget::XY => "G28 X Y",
            HomeTarget::All => "G28",
        }
    }
}

/// What a jog control asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JogAction {
    /// Move `axis` by `distance` mm relative to the current position
    /// (negative = towards the minimum).
    Move {
        /// The axis to move.
        axis: Axis,
        /// Signed distance in mm.
        distance: f32,
    },
    /// Home axes.
    Home(HomeTarget),
    /// Disable the stepper motors (M84).
    MotorsOff,
}

impl JogAction {
    /// A short human description ("Y +10 mm", "Home all axes").
    pub fn describe(&self) -> String {
        match self {
            JogAction::Move { axis, distance } => {
                format!("{} {} mm", axis.letter(), signed_step(*distance))
            }
            JogAction::Home(HomeTarget::All) => "Home all axes".to_owned(),
            JogAction::Home(HomeTarget::XY) => "Home X and Y".to_owned(),
            JogAction::Home(t) => format!("Home {}", t.gcode().trim_start_matches("G28 ")),
            JogAction::MotorsOff => "Motors off".to_owned(),
        }
    }
}

/// A step size as shown on the controls: "0.1", "1", "10" (no trailing
/// zeros, at most 3 decimals); "—" for non-finite values.
pub fn step_label(step: f32) -> String {
    if !step.is_finite() {
        return "—".to_owned();
    }
    let s = format!("{:.3}", step.abs());
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

/// A signed step: "+10", "−0.1" (with a real minus sign).
pub fn signed_step(distance: f32) -> String {
    let sign = if distance < 0.0 { "\u{2212}" } else { "+" };
    format!("{sign}{}", step_label(distance))
}

/// Sanitize user-provided step sizes: finite, positive, else the default.
fn sanitize_steps<const N: usize>(steps: [f32; N], default: [f32; N]) -> [f32; N] {
    let mut out = default;
    for (o, s) in out.iter_mut().zip(steps) {
        if s.is_finite() && s > 0.0 {
            *o = s.min(10_000.0);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// XY jog pad

/// A direction on the XY pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JogDirection {
    /// Y+ (away from the user on most printers).
    Up,
    /// Y−.
    Down,
    /// X−.
    Left,
    /// X+.
    Right,
}

impl JogDirection {
    /// All four, clockwise from the top.
    pub const ALL: [JogDirection; 4] = [
        JogDirection::Up,
        JogDirection::Right,
        JogDirection::Down,
        JogDirection::Left,
    ];

    /// Axis and sign of this direction.
    pub fn axis_sign(self) -> (Axis, f32) {
        match self {
            JogDirection::Up => (Axis::Y, 1.0),
            JogDirection::Down => (Axis::Y, -1.0),
            JogDirection::Left => (Axis::X, -1.0),
            JogDirection::Right => (Axis::X, 1.0),
        }
    }

    /// Screen angle of the sector center (radians, 0 = right, y down).
    fn angle(self) -> f32 {
        match self {
            JogDirection::Right => 0.0,
            JogDirection::Down => FRAC_PI_2,
            JogDirection::Left => PI,
            JogDirection::Up => -FRAC_PI_2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            JogDirection::Up => "Y+",
            JogDirection::Down => "Y\u{2212}",
            JogDirection::Left => "X\u{2212}",
            JogDirection::Right => "X+",
        }
    }
}

/// A corner button of the [`JogPad`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JogCorner {
    /// Home X.
    TopLeft,
    /// Home Y.
    TopRight,
    /// Home all axes.
    BottomLeft,
    /// Motors off.
    BottomRight,
}

impl JogCorner {
    /// All four corners.
    pub const ALL: [JogCorner; 4] = [
        JogCorner::TopLeft,
        JogCorner::TopRight,
        JogCorner::BottomLeft,
        JogCorner::BottomRight,
    ];

    /// The action of this corner.
    pub fn action(self) -> JogAction {
        match self {
            JogCorner::TopLeft => JogAction::Home(HomeTarget::X),
            JogCorner::TopRight => JogAction::Home(HomeTarget::Y),
            JogCorner::BottomLeft => JogAction::Home(HomeTarget::All),
            JogCorner::BottomRight => JogAction::MotorsOff,
        }
    }

    fn icon_and_label(self) -> (&'static str, &'static str) {
        match self {
            JogCorner::TopLeft => (icons::HOME, "X"),
            JogCorner::TopRight => (icons::HOME, "Y"),
            JogCorner::BottomLeft => (icons::HOME, "All"),
            JogCorner::BottomRight => (icons::MOTORS_OFF, "Off"),
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            JogCorner::TopLeft => "Home X (G28 X)",
            JogCorner::TopRight => "Home Y (G28 Y)",
            JogCorner::BottomLeft => "Home all axes (G28)",
            JogCorner::BottomRight => "Motors off (M84)",
        }
    }
}

/// A clickable part of the [`JogPad`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JogRegion {
    /// A ring sector: `ring` 0 is the innermost (smallest step).
    Sector {
        /// Direction of the sector.
        direction: JogDirection,
        /// Ring index, 0 = innermost.
        ring: usize,
    },
    /// The center home button (home X and Y).
    Center,
    /// A corner button.
    Corner(JogCorner),
}

/// The geometry of a [`JogPad`] laid out in `rect` (pure; used for both
/// painting and hit testing).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JogGeometry {
    /// The widget rectangle (square).
    pub rect: Rect,
    /// Center of the rings.
    pub center: Pos2,
    /// Radius of the center home button.
    pub home_radius: f32,
    /// Outer radius of the outermost ring.
    pub outer_radius: f32,
    /// Side of the square corner buttons (0 when they are hidden).
    pub corner_size: f32,
}

/// Number of rings (step sizes) of the XY pad.
pub const JOG_RINGS: usize = 4;

impl JogGeometry {
    /// Lay out a pad in `rect` (its largest centered square is used).
    pub fn new(rect: Rect, corner_buttons: bool) -> JogGeometry {
        let side = rect.width().min(rect.height()).max(1.0);
        let rect = Rect::from_center_size(rect.center(), Vec2::splat(side));
        let outer_radius = (side / 2.0 - (side * 0.075).clamp(12.0, 20.0)).max(1.0);
        JogGeometry {
            rect,
            center: rect.center(),
            home_radius: outer_radius * 0.24,
            outer_radius,
            corner_size: if corner_buttons { side * 0.165 } else { 0.0 },
        }
    }

    /// Width of one ring.
    pub fn ring_width(&self) -> f32 {
        (self.outer_radius - self.home_radius) / JOG_RINGS as f32
    }

    /// Inner and outer radius of ring `i`.
    pub fn ring_radii(&self, ring: usize) -> (f32, f32) {
        let w = self.ring_width();
        let r0 = self.home_radius + w * ring as f32;
        (r0, r0 + w)
    }

    /// The rectangle of a corner button.
    pub fn corner_rect(&self, corner: JogCorner) -> Rect {
        let s = self.corner_size;
        let r = self.rect;
        let min = match corner {
            JogCorner::TopLeft => r.min,
            JogCorner::TopRight => pos2(r.max.x - s, r.min.y),
            JogCorner::BottomLeft => pos2(r.min.x, r.max.y - s),
            JogCorner::BottomRight => pos2(r.max.x - s, r.max.y - s),
        };
        Rect::from_min_size(min, Vec2::splat(s))
    }

    /// The region under `pos`, if any.
    pub fn hit(&self, pos: Pos2) -> Option<JogRegion> {
        if !pos.x.is_finite() || !pos.y.is_finite() {
            return None;
        }
        let d = pos - self.center;
        let dist = d.length();
        if dist <= self.home_radius {
            return Some(JogRegion::Center);
        }
        if dist <= self.outer_radius {
            let ring =
                (((dist - self.home_radius) / self.ring_width()) as usize).min(JOG_RINGS - 1);
            // Sectors are 90° wide, centered on the axes.
            let direction = if d.x.abs() >= d.y.abs() {
                if d.x >= 0.0 {
                    JogDirection::Right
                } else {
                    JogDirection::Left
                }
            } else if d.y < 0.0 {
                JogDirection::Up
            } else {
                JogDirection::Down
            };
            return Some(JogRegion::Sector { direction, ring });
        }
        if self.corner_size > 0.0 {
            return JogCorner::ALL
                .into_iter()
                .find(|c| self.corner_rect(*c).contains(pos))
                .map(JogRegion::Corner);
        }
        None
    }
}

/// The inner and outer arc of an annular sector, with a constant `gap`
/// (points) left free on every side. Empty when the sector is too thin.
fn sector_arcs(
    center: Pos2,
    (r_in, r_out): (f32, f32),
    (a0, a1): (f32, f32),
    gap: f32,
) -> (Vec<Pos2>, Vec<Pos2>) {
    let r_in = r_in + gap / 2.0;
    let r_out = r_out - gap / 2.0;
    if r_out <= r_in || r_in <= 0.0 {
        return (Vec::new(), Vec::new());
    }
    let inset_in = gap / 2.0 / r_in;
    let inset_out = gap / 2.0 / r_out;
    let steps = 24;
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let ai = egui::lerp((a0 + inset_in)..=(a1 - inset_in), t);
            let ao = egui::lerp((a0 + inset_out)..=(a1 - inset_out), t);
            (
                center + r_in * Vec2::angled(ai),
                center + r_out * Vec2::angled(ao),
            )
        })
        .unzip()
}

/// An annular sector as a filled mesh plus an anti-aliased outline (meshes
/// have no feathering of their own). `outline` defaults to a hairline in
/// the fill color.
fn sector_shapes(
    center: Pos2,
    radii: (f32, f32),
    angles: (f32, f32),
    gap: f32,
    fill: Color32,
    outline: Option<Stroke>,
) -> Vec<Shape> {
    let (inner, outer) = sector_arcs(center, radii, angles, gap);
    if inner.len() < 2 {
        return Vec::new();
    }
    let mut mesh = Mesh::default();
    for (i, (a, b)) in inner.iter().zip(&outer).enumerate() {
        mesh.colored_vertex(*a, fill);
        mesh.colored_vertex(*b, fill);
        if i > 0 {
            let k = (2 * i) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    let mut ring: Vec<Pos2> = outer;
    ring.extend(inner.into_iter().rev());
    let stroke = outline.unwrap_or(Stroke::new(1.0, fill));
    vec![
        Shape::mesh(mesh),
        Shape::Path(PathShape::closed_line(ring, stroke)),
    ]
}

/// Ring fill from the innermost (lightest) to the outermost.
fn ring_fill(p: &Palette, ring: usize) -> Color32 {
    let t = ring as f32 / (JOG_RINGS - 1) as f32;
    if p.dark {
        mix(theme::hex(0x343B45), theme::hex(0x22272E), t)
    } else {
        mix(theme::hex(0xEDEFF3), theme::hex(0xD6DBE2), t)
    }
}

/// The circular XY jog control: four concentric rings of step sizes
/// (default 0.1 / 1 / 10 / 100 mm), four directions per ring, a center
/// button that homes X and Y and corner buttons for home X, home Y, home
/// all and motors off.
///
/// ```no_run
/// # use sprint_gui::widgets::{JogAction, JogPad};
/// # fn demo(ui: &mut egui::Ui) {
/// if let Some(action) = JogPad::new().size(240.0).show(ui).action {
///     match action {
///         JogAction::Move { axis, distance } => { /* G91, G0 X.. F.., G90 */ }
///         JogAction::Home(target) => { /* target.gcode() */ }
///         JogAction::MotorsOff => { /* M84 */ }
///     }
/// }
/// # }
/// ```
#[must_use = "call `.show(ui)`"]
pub struct JogPad {
    size: f32,
    steps: [f32; JOG_RINGS],
    corner_buttons: bool,
    id_salt: Option<Id>,
}

impl Default for JogPad {
    fn default() -> Self {
        JogPad::new()
    }
}

impl JogPad {
    /// Default steps in mm, innermost ring first.
    pub const DEFAULT_STEPS: [f32; JOG_RINGS] = [0.1, 1.0, 10.0, 100.0];

    /// A 240 pt pad with the default steps and corner buttons.
    pub fn new() -> Self {
        JogPad {
            size: 240.0,
            steps: Self::DEFAULT_STEPS,
            corner_buttons: true,
            id_salt: None,
        }
    }

    /// Side length in points (160..=520).
    pub fn size(mut self, size: f32) -> Self {
        if size.is_finite() {
            self.size = size.clamp(160.0, 520.0);
        }
        self
    }

    /// Step sizes in mm, innermost ring first (invalid entries keep the
    /// default).
    pub fn steps(mut self, steps: [f32; JOG_RINGS]) -> Self {
        self.steps = sanitize_steps(steps, Self::DEFAULT_STEPS);
        self
    }

    /// Show the corner buttons (default `true`).
    pub fn corner_buttons(mut self, show: bool) -> Self {
        self.corner_buttons = show;
        self
    }

    /// A stable id salt (by default the id follows the position in the parent).
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(Id::new(salt));
        self
    }

    /// The action of a region.
    pub fn action_for(&self, region: JogRegion) -> JogAction {
        match region {
            JogRegion::Sector { direction, ring } => {
                let (axis, sign) = direction.axis_sign();
                let step = self.steps.get(ring).copied().unwrap_or(1.0);
                JogAction::Move {
                    axis,
                    distance: sign * step,
                }
            }
            JogRegion::Center => JogAction::Home(HomeTarget::XY),
            JogRegion::Corner(c) => c.action(),
        }
    }

    fn tooltip(&self, region: JogRegion) -> String {
        match region {
            JogRegion::Center => "Home X and Y (G28 X Y)".to_owned(),
            JogRegion::Corner(c) => c.tooltip().to_owned(),
            r => format!("Move {}", self.action_for(r).describe()),
        }
    }

    /// Show the pad.
    pub fn show(self, ui: &mut Ui) -> ActionResponse<JogAction> {
        let (rect, response) = match self.id_salt {
            Some(salt) => {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::hover());
                let id = super::widget_id(ui, salt);
                (rect, ui.interact(rect, id, Sense::click()))
            }
            None => ui.allocate_exact_size(Vec2::splat(self.size), Sense::click()),
        };
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), "XY jog"));
        let geo = JogGeometry::new(rect, self.corner_buttons);
        let hovered = response
            .hover_pos()
            .filter(|_| ui.is_enabled())
            .and_then(|pos| geo.hit(pos));
        // Act on the region under the *press*: by the release the pointer
        // may be over a neighbouring ring (like a button that keeps its
        // click when released just outside it).
        let press = super::PressOrigin::track(ui, &response);
        let pressed = press.held.and_then(|pos| geo.hit(pos));
        let action = press
            .clicked
            .and_then(|pos| geo.hit(pos))
            .map(|region| self.action_for(region));
        if ui.is_rect_visible(rect) {
            self.paint(ui, &geo, hovered, pressed);
            paint_focus_ring(ui, &response, rect, CornerRadius::same(theme::RADIUS_LG));
        }
        let response = match hovered {
            Some(region) => response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text_at_pointer(self.tooltip(region)),
            None => response,
        };
        ActionResponse::new(response, action)
    }

    /// Paint the pad. `pressed` is the region held down (it may differ
    /// from `hovered` once the pointer slides off it).
    fn paint(
        &self,
        ui: &Ui,
        geo: &JogGeometry,
        hovered: Option<JogRegion>,
        pressed: Option<JogRegion>,
    ) {
        let p = theme::ui_palette(ui);
        let painter = ui.painter();
        let gap = 2.5;
        let step_font = FontId::new(
            (geo.ring_width() * 0.5).clamp(9.0, 13.0),
            theme::font(ui, TextRole::Caption).family,
        );
        // Fill and outline of a region in its hover / pressed state.
        let highlight = |region: JogRegion, base: Color32| -> (Color32, Option<Stroke>) {
            match (hovered == Some(region), pressed == Some(region)) {
                (_, true) => (p.accent, None),
                (true, false) => (
                    mix(base, p.accent, if p.dark { 0.22 } else { 0.16 }),
                    Some(Stroke::new(1.5, p.accent)),
                ),
                _ => (base, None),
            }
        };
        let hot_text = if p.dark {
            p.accent_hover
        } else {
            p.accent_pressed
        };
        // Rings.
        for direction in JogDirection::ALL {
            let a = direction.angle();
            for ring in 0..JOG_RINGS {
                let region = JogRegion::Sector { direction, ring };
                let (fill, outline) = highlight(region, ring_fill(&p, ring));
                painter.extend(sector_shapes(
                    geo.center,
                    geo.ring_radii(ring),
                    (a - FRAC_PI_4, a + FRAC_PI_4),
                    gap,
                    fill,
                    outline,
                ));
                // Step labels sit on the vertical axis, plus the hovered sector.
                let hot = hovered == Some(region) || pressed == Some(region);
                let show_label = matches!(direction, JogDirection::Up | JogDirection::Down) || hot;
                if show_label {
                    let (r0, r1) = geo.ring_radii(ring);
                    let pos = geo.center + (r0 + r1) / 2.0 * Vec2::angled(a);
                    let color = if pressed == Some(region) {
                        p.on_accent
                    } else if hot {
                        hot_text
                    } else {
                        p.text_dim
                    };
                    let label = if hot {
                        let (_, sign) = direction.axis_sign();
                        signed_step(sign * self.steps[ring])
                    } else {
                        step_label(self.steps[ring])
                    };
                    painter.text(pos, Align2::CENTER_CENTER, label, step_font.clone(), color);
                }
            }
        }
        // Direction carets in the outer ring's edge and axis labels outside.
        let axis_font = FontId::new(11.0, theme::font(ui, TextRole::Section).family);
        for direction in JogDirection::ALL {
            let a = direction.angle();
            let is_hot =
                matches!(hovered, Some(JogRegion::Sector { direction: d, .. }) if d == direction);
            let color = if is_hot { p.accent } else { p.text_faint };
            let label_pos = geo.center + (geo.outer_radius + 10.0) * Vec2::angled(a);
            painter.text(
                label_pos,
                Align2::CENTER_CENTER,
                direction.label(),
                axis_font.clone(),
                color,
            );
        }
        // Center home button.
        let center_fill = if pressed == Some(JogRegion::Center) {
            p.accent_pressed
        } else if hovered == Some(JogRegion::Center) {
            p.accent
        } else if p.dark {
            theme::hex(0x3B434E)
        } else {
            Color32::WHITE
        };
        let center_fg = if hovered == Some(JogRegion::Center) || pressed == Some(JogRegion::Center)
        {
            p.on_accent
        } else {
            p.text
        };
        painter.circle(
            geo.center,
            geo.home_radius - gap / 2.0,
            center_fill,
            Stroke::new(1.0, if p.dark { p.border_strong } else { p.border }),
        );
        painter.text(
            geo.center,
            Align2::CENTER_CENTER,
            icons::HOME,
            icon_font(ui, (geo.home_radius * 0.8).clamp(14.0, 26.0)),
            center_fg,
        );
        // Corner buttons.
        if geo.corner_size > 0.0 {
            let label_font = FontId::new(10.5, theme::font(ui, TextRole::Section).family);
            for corner in JogCorner::ALL {
                let r = geo.corner_rect(corner).shrink(1.0);
                let region = JogRegion::Corner(corner);
                let hot = hovered == Some(region) || pressed == Some(region);
                let danger = corner == JogCorner::BottomRight;
                let fill = if pressed == Some(region) {
                    mix(p.raised, p.bg, 0.35)
                } else if hot {
                    p.raised_hover
                } else {
                    p.raised
                };
                let stroke = Stroke::new(1.0, if hot { p.border_strong } else { p.border });
                painter.rect(
                    r,
                    CornerRadius::same(theme::RADIUS_LG),
                    fill,
                    stroke,
                    StrokeKind::Inside,
                );
                let (icon, label) = corner.icon_and_label();
                let fg = if hot && danger {
                    p.danger
                } else if hot {
                    p.text
                } else {
                    p.text_dim
                };
                let icon_size = (r.height() * 0.36).clamp(12.0, 20.0);
                painter.text(
                    pos2(r.center().x, r.center().y - icon_size * 0.32),
                    Align2::CENTER_CENTER,
                    icon,
                    icon_font(ui, icon_size),
                    fg,
                );
                painter.text(
                    pos2(r.center().x, r.center().y + icon_size * 0.62),
                    Align2::CENTER_CENTER,
                    label,
                    label_font.clone(),
                    fg,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Z strip

/// A vertical jog strip for one axis (Z by default): `+` steps on top,
/// `−` steps below and a home button in the middle.
///
/// ```no_run
/// # use sprint_gui::widgets::ZJog;
/// # fn demo(ui: &mut egui::Ui) {
/// if let Some(action) = ZJog::new().height(240.0).show(ui).action { /* ... */ }
/// # }
/// ```
#[must_use = "call `.show(ui)`"]
pub struct ZJog {
    axis: Axis,
    steps: [f32; 4],
    width: f32,
    height: f32,
    home: bool,
    id_salt: Option<Id>,
}

impl Default for ZJog {
    fn default() -> Self {
        ZJog::new()
    }
}

/// One cell of a [`ZJog`] strip, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ZCell {
    Step(f32),
    Home,
}

impl ZJog {
    /// Default steps in mm, smallest first.
    pub const DEFAULT_STEPS: [f32; 4] = [0.1, 1.0, 10.0, 50.0];

    /// A Z strip 64 × 240 pt with home button and default steps.
    pub fn new() -> Self {
        ZJog {
            axis: Axis::Z,
            steps: Self::DEFAULT_STEPS,
            width: 64.0,
            height: 240.0,
            home: true,
            id_salt: None,
        }
    }

    /// The axis to jog (the home button only exists for Z).
    pub fn axis(mut self, axis: Axis) -> Self {
        self.axis = axis;
        self
    }

    /// Step sizes in mm, smallest first.
    pub fn steps(mut self, steps: [f32; 4]) -> Self {
        self.steps = sanitize_steps(steps, Self::DEFAULT_STEPS);
        self
    }

    /// Width in points (44..=160).
    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.clamp(44.0, 160.0);
        }
        self
    }

    /// Height in points (160..=600), e.g. the [`JogPad`] size.
    pub fn height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.height = height.clamp(160.0, 600.0);
        }
        self
    }

    /// Show the middle home button (default `true`; only for Z).
    pub fn home_button(mut self, home: bool) -> Self {
        self.home = home;
        self
    }

    /// A stable id salt (by default the id follows the position in the parent).
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(Id::new(salt));
        self
    }

    fn cells(&self) -> Vec<ZCell> {
        let mut cells: Vec<ZCell> = self.steps.iter().rev().map(|s| ZCell::Step(*s)).collect();
        if self.home && self.axis == Axis::Z {
            cells.push(ZCell::Home);
        }
        cells.extend(self.steps.iter().map(|s| ZCell::Step(-*s)));
        cells
    }

    /// Show the strip.
    pub fn show(self, ui: &mut Ui) -> ActionResponse<JogAction> {
        let cells = self.cells();
        let gap = 3.0;
        let (rect, alloc) = ui.allocate_exact_size(vec2(self.width, self.height), Sense::hover());
        let base = match self.id_salt {
            Some(salt) => super::widget_id(ui, salt),
            None => alloc.id,
        };
        // The home cell is a little taller.
        let weights: Vec<f32> = cells
            .iter()
            .map(|c| if *c == ZCell::Home { 1.35 } else { 1.0 })
            .collect();
        let total: f32 = weights.iter().sum();
        let unit = (self.height - gap * (cells.len() as f32 - 1.0)) / total;
        let mut y = rect.min.y;
        let mut action = None;
        let p = theme::ui_palette(ui);
        let font = FontId::new(13.0, theme::font(ui, TextRole::Title).family);
        let n = cells.len();
        let mut union: Option<Response> = None;
        for (i, (cell, w)) in cells.iter().zip(&weights).enumerate() {
            let h = unit * w;
            let r = Rect::from_min_size(pos2(rect.min.x, y), vec2(self.width, h));
            y += h + gap;
            let id = base.with(i);
            let resp = ui.interact(r, id, Sense::click());
            let (label, tip, cell_action) = match cell {
                ZCell::Step(d) => {
                    let a = JogAction::Move {
                        axis: self.axis,
                        distance: *d,
                    };
                    (signed_step(*d), format!("Move {}", a.describe()), a)
                }
                ZCell::Home => (
                    "Z".to_owned(),
                    "Home Z (G28 Z)".to_owned(),
                    JogAction::Home(HomeTarget::Z),
                ),
            };
            resp.widget_info(|| {
                WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), tip.clone())
            });
            if resp.clicked() {
                action = Some(cell_action);
            }
            if ui.is_rect_visible(r) {
                let hot = resp.hovered() && ui.is_enabled();
                let down = resp.is_pointer_button_down_on();
                let magnitude = match cell {
                    ZCell::Step(d) => self
                        .steps
                        .iter()
                        .position(|s| (s - d.abs()).abs() < f32::EPSILON)
                        .unwrap_or(0),
                    ZCell::Home => 0,
                };
                let base_fill = match cell {
                    ZCell::Home => {
                        if p.dark {
                            theme::hex(0x3B434E)
                        } else {
                            Color32::WHITE
                        }
                    }
                    ZCell::Step(_) => ring_fill(&p, magnitude),
                };
                let fill = if hot {
                    mix(base_fill, p.accent, if down { 0.55 } else { 0.25 })
                } else {
                    base_fill
                };
                let big = (self.width / 2.0).min(18.0) as u8;
                let small = theme::RADIUS_SM;
                let radius = if i == 0 {
                    CornerRadius {
                        nw: big,
                        ne: big,
                        sw: small,
                        se: small,
                    }
                } else if i + 1 == n {
                    CornerRadius {
                        nw: small,
                        ne: small,
                        sw: big,
                        se: big,
                    }
                } else {
                    CornerRadius::same(small)
                };
                let stroke = if *cell == ZCell::Home {
                    Stroke::new(1.0, if p.dark { p.border_strong } else { p.border })
                } else {
                    Stroke::NONE
                };
                ui.painter()
                    .rect(r, radius, fill, stroke, StrokeKind::Inside);
                let fg = if hot {
                    if p.dark {
                        p.accent_hover
                    } else {
                        p.accent_pressed
                    }
                } else if *cell == ZCell::Home {
                    p.text
                } else {
                    p.text_dim
                };
                if *cell == ZCell::Home {
                    ui.painter().text(
                        pos2(r.center().x - 7.0, r.center().y),
                        Align2::CENTER_CENTER,
                        icons::HOME,
                        icon_font(ui, 15.0),
                        fg,
                    );
                    ui.painter().text(
                        pos2(r.center().x + 9.0, r.center().y),
                        Align2::CENTER_CENTER,
                        label,
                        font.clone(),
                        fg,
                    );
                } else {
                    ui.painter()
                        .text(r.center(), Align2::CENTER_CENTER, label, font.clone(), fg);
                }
            }
            let resp = resp
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text(tip);
            union = Some(match union {
                Some(u) => u | resp,
                None => resp,
            });
        }
        let response = union.unwrap_or_else(|| ui.interact(rect, base, Sense::hover()));
        ActionResponse::new(response, action)
    }
}

// ---------------------------------------------------------------------------
// Extruder

/// Smallest length (mm) and speed (mm/s) the [`ExtruderControl`] reports.
const MIN_EXTRUDER_AMOUNT: f32 = 0.1;

/// What the [`ExtruderControl`] asks for. Speeds are in mm/s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExtruderAction {
    /// Push `length` mm of filament at `speed` mm/s.
    Extrude {
        /// Length in mm (positive).
        length: f32,
        /// Feed speed in mm/s.
        speed: f32,
    },
    /// Pull back `length` mm of filament at `speed` mm/s.
    Retract {
        /// Length in mm (positive).
        length: f32,
        /// Feed speed in mm/s.
        speed: f32,
    },
}

/// Extrude / retract with an editable length and speed, quick length chips
/// and an optional reason that blocks extrusion (e.g. a cold hotend).
///
/// A length or speed that is NaN, infinite or not positive is reset to
/// 5, and one below 0.1 or above its [limit](Self::limits) is clamped, so
/// the control never reports a move that cannot be sent or exceeds the
/// limits.
///
/// ```no_run
/// # use sprint_gui::widgets::{ExtruderAction, ExtruderControl};
/// # fn demo(ui: &mut egui::Ui, length: &mut f32, speed: &mut f32, cold: bool) {
/// let r = ExtruderControl::new(length, speed)
///     .blocked(cold.then_some("Hotend below 170 °C"))
///     .show(ui);
/// if let Some(ExtruderAction::Extrude { length, speed }) = r.action { /* G1 E.. F.. */ }
/// # }
/// ```
#[must_use = "call `.show(ui)`"]
pub struct ExtruderControl<'a> {
    length: &'a mut f32,
    speed: &'a mut f32,
    presets: &'a [f32],
    blocked: Option<&'a str>,
    max_length: f32,
    max_speed: f32,
}

impl<'a> ExtruderControl<'a> {
    /// Default quick lengths in mm.
    pub const DEFAULT_PRESETS: [f32; 4] = [5.0, 10.0, 50.0, 100.0];

    /// A control bound to a length (mm) and a speed (mm/s).
    pub fn new(length: &'a mut f32, speed: &'a mut f32) -> Self {
        ExtruderControl {
            length,
            speed,
            presets: &Self::DEFAULT_PRESETS,
            blocked: None,
            max_length: 1000.0,
            max_speed: 100.0,
        }
    }

    /// Quick length chips (empty hides them).
    pub fn presets(mut self, presets: &'a [f32]) -> Self {
        self.presets = presets;
        self
    }

    /// When `Some`, extrusion is disabled and the reason is shown.
    pub fn blocked(mut self, reason: Option<&'a str>) -> Self {
        self.blocked = reason;
        self
    }

    /// Upper limits for length (mm) and speed (mm/s); values not above
    /// 0.1 are ignored.
    pub fn limits(mut self, max_length: f32, max_speed: f32) -> Self {
        if max_length.is_finite() && max_length > MIN_EXTRUDER_AMOUNT {
            self.max_length = max_length;
        }
        if max_speed.is_finite() && max_speed > MIN_EXTRUDER_AMOUNT {
            self.max_speed = max_speed;
        }
        self
    }

    /// Show the control.
    pub fn show(self, ui: &mut Ui) -> ActionResponse<ExtruderAction> {
        let p = theme::ui_palette(ui);
        let mut action = None;
        // Repair values that cannot be sent.
        if !self.length.is_finite() || *self.length <= 0.0 {
            *self.length = 5.0;
        }
        if !self.speed.is_finite() || *self.speed <= 0.0 {
            *self.speed = 5.0;
        }
        let max_length = self.max_length;
        let max_speed = self.max_speed;
        // The fields keep out-of-range values (they only warn), so bound
        // them here: a saved 2000 mm must not become a 2000 mm extrusion.
        *self.length = self.length.clamp(MIN_EXTRUDER_AMOUNT, max_length);
        *self.speed = self.speed.clamp(MIN_EXTRUDER_AMOUNT, max_speed);
        let response = ui
            .vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let label_w = 56.0;
                ui.add(
                    NumericField::new(&mut *self.length)
                        .label("Length")
                        .unit("mm")
                        .range(MIN_EXTRUDER_AMOUNT..=max_length)
                        .decimals(1)
                        .speed(0.1)
                        .label_width(label_w),
                );
                if !self.presets.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(label_w + 6.0);
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for preset in self.presets.iter().filter(|v| v.is_finite() && **v > 0.0) {
                            let selected = (*self.length - preset).abs() < 1e-3;
                            let text = step_label(*preset);
                            if ui.add(super::Chip::new(&text).selected(selected)).clicked() {
                                *self.length = preset.clamp(MIN_EXTRUDER_AMOUNT, max_length);
                            }
                        }
                    });
                }
                ui.add(
                    NumericField::new(&mut *self.speed)
                        .label("Speed")
                        .unit("mm/s")
                        .range(MIN_EXTRUDER_AMOUNT..=max_speed)
                        .decimals(1)
                        .speed(0.1)
                        .label_width(label_w),
                );
                if let Some(reason) = self.blocked {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(icons::WARNING)
                                .color(p.warning)
                                .size(14.0),
                        );
                        ui.label(egui::RichText::new(reason).color(p.warning).size(12.5));
                    });
                }
                ui.add_enabled_ui(self.blocked.is_none(), |ui| {
                    ui.columns(2, |cols| {
                        let length = *self.length;
                        let speed = *self.speed;
                        if cols[0]
                            .add(
                                ActionButton::new("Retract")
                                    .icon(icons::RETRACT)
                                    .fill_width(true)
                                    .tooltip(format!("Retract {} mm", step_label(length))),
                            )
                            .clicked()
                        {
                            action = Some(ExtruderAction::Retract { length, speed });
                        }
                        if cols[1]
                            .add(
                                ActionButton::new("Extrude")
                                    .icon(icons::EXTRUDE)
                                    .primary()
                                    .fill_width(true)
                                    .tooltip(format!("Extrude {} mm", step_label(length))),
                            )
                            .clicked()
                        {
                            action = Some(ExtruderAction::Extrude { length, speed });
                        }
                    });
                });
            })
            .response;
        ActionResponse::new(response, action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo() -> JogGeometry {
        JogGeometry::new(Rect::from_min_size(Pos2::ZERO, Vec2::splat(240.0)), true)
    }

    #[test]
    fn hit_center_and_rings() {
        let g = geo();
        let c = g.center;
        assert_eq!(g.hit(c), Some(JogRegion::Center));
        let w = g.ring_width();
        for ring in 0..JOG_RINGS {
            let r = g.home_radius + w * (ring as f32 + 0.5);
            assert_eq!(
                g.hit(c + vec2(0.0, -r)),
                Some(JogRegion::Sector {
                    direction: JogDirection::Up,
                    ring
                })
            );
            assert_eq!(
                g.hit(c + vec2(r, 0.0)),
                Some(JogRegion::Sector {
                    direction: JogDirection::Right,
                    ring
                })
            );
            assert_eq!(
                g.hit(c + vec2(0.0, r)),
                Some(JogRegion::Sector {
                    direction: JogDirection::Down,
                    ring
                })
            );
            assert_eq!(
                g.hit(c + vec2(-r, 0.0)),
                Some(JogRegion::Sector {
                    direction: JogDirection::Left,
                    ring
                })
            );
        }
    }

    #[test]
    fn hit_corners_and_outside() {
        let g = geo();
        assert_eq!(
            g.hit(pos2(4.0, 4.0)),
            Some(JogRegion::Corner(JogCorner::TopLeft))
        );
        assert_eq!(
            g.hit(pos2(236.0, 4.0)),
            Some(JogRegion::Corner(JogCorner::TopRight))
        );
        assert_eq!(
            g.hit(pos2(4.0, 236.0)),
            Some(JogRegion::Corner(JogCorner::BottomLeft))
        );
        assert_eq!(
            g.hit(pos2(236.0, 236.0)),
            Some(JogRegion::Corner(JogCorner::BottomRight))
        );
        // Between the circle and the edge midpoints: nothing.
        assert_eq!(g.hit(pos2(120.0, 2.0)), None);
        assert_eq!(g.hit(pos2(f32::NAN, 3.0)), None);
        // Without corner buttons the corners are inert.
        let bare = JogGeometry::new(g.rect, false);
        assert_eq!(bare.hit(pos2(4.0, 4.0)), None);
    }

    #[test]
    fn corners_do_not_overlap_rings() {
        for side in [160.0, 240.0, 520.0] {
            let g = JogGeometry::new(Rect::from_min_size(Pos2::ZERO, Vec2::splat(side)), true);
            for corner in JogCorner::ALL {
                let r = g.corner_rect(corner);
                // The corner nearest the center lies outside the rings.
                let nearest = pos2(
                    g.center.x.clamp(r.min.x, r.max.x),
                    g.center.y.clamp(r.min.y, r.max.y),
                );
                assert!(
                    (nearest - g.center).length() > g.outer_radius,
                    "{side} {corner:?}"
                );
            }
        }
    }

    #[test]
    fn actions_follow_steps_and_signs() {
        let pad = JogPad::new();
        assert_eq!(
            pad.action_for(JogRegion::Sector {
                direction: JogDirection::Up,
                ring: 2
            }),
            JogAction::Move {
                axis: Axis::Y,
                distance: 10.0
            }
        );
        assert_eq!(
            pad.action_for(JogRegion::Sector {
                direction: JogDirection::Left,
                ring: 0
            }),
            JogAction::Move {
                axis: Axis::X,
                distance: -0.1
            }
        );
        assert_eq!(
            pad.action_for(JogRegion::Center),
            JogAction::Home(HomeTarget::XY)
        );
        assert_eq!(
            pad.action_for(JogRegion::Corner(JogCorner::BottomRight)),
            JogAction::MotorsOff
        );
        // Out-of-range ring indices fall back instead of panicking.
        assert!(matches!(
            pad.action_for(JogRegion::Sector {
                direction: JogDirection::Up,
                ring: 99
            }),
            JogAction::Move { .. }
        ));
    }

    #[test]
    fn steps_are_sanitized() {
        let pad = JogPad::new().steps([0.05, f32::NAN, -3.0, 1e9]);
        assert_eq!(pad.steps, [0.05, 1.0, 10.0, 10_000.0]);
        let z = ZJog::new().steps([0.0, 2.0, f32::INFINITY, 25.0]);
        assert_eq!(z.steps, [0.1, 2.0, 10.0, 25.0]);
    }

    #[test]
    fn labels_format_cleanly() {
        assert_eq!(step_label(0.1), "0.1");
        assert_eq!(step_label(10.0), "10");
        assert_eq!(step_label(0.025), "0.025");
        assert_eq!(step_label(0.0), "0");
        assert_eq!(step_label(f32::NAN), "—");
        assert_eq!(signed_step(-0.1), "\u{2212}0.1");
        assert_eq!(signed_step(50.0), "+50");
        assert_eq!(
            JogAction::Move {
                axis: Axis::Z,
                distance: -1.0
            }
            .describe(),
            "Z \u{2212}1 mm"
        );
        assert_eq!(JogAction::Home(HomeTarget::Z).describe(), "Home Z");
        assert_eq!(JogAction::Home(HomeTarget::All).describe(), "Home all axes");
        assert_eq!(HomeTarget::XY.gcode(), "G28 X Y");
    }

    #[test]
    fn z_cells_are_ordered_top_down() {
        let cells = ZJog::new().cells();
        assert_eq!(cells.len(), 9);
        assert_eq!(cells[0], ZCell::Step(50.0));
        assert_eq!(cells[3], ZCell::Step(0.1));
        assert_eq!(cells[4], ZCell::Home);
        assert_eq!(cells[5], ZCell::Step(-0.1));
        assert_eq!(cells[8], ZCell::Step(-50.0));
        assert_eq!(ZJog::new().axis(Axis::E).cells().len(), 8);
        assert_eq!(ZJog::new().home_button(false).cells().len(), 8);
    }

    #[test]
    fn degenerate_sectors_draw_nothing() {
        let s = sector_shapes(
            Pos2::ZERO,
            (10.0, 11.0),
            (0.0, 1.0),
            4.0,
            Color32::RED,
            None,
        );
        assert!(s.is_empty());
        let s = sector_shapes(
            Pos2::ZERO,
            (10.0, 30.0),
            (0.0, 1.0),
            2.0,
            Color32::RED,
            None,
        );
        assert_eq!(s.len(), 2);
        let (inner, outer) = sector_arcs(Pos2::ZERO, (10.0, 30.0), (0.0, 1.0), 2.0);
        assert_eq!(inner.len(), outer.len());
        // The gap is respected on the radial edges.
        assert!(
            inner
                .iter()
                .all(|p| (p.to_vec2().length() - 11.0).abs() < 1e-3)
        );
        assert!(
            outer
                .iter()
                .all(|p| (p.to_vec2().length() - 29.0).abs() < 1e-3)
        );
    }
}
