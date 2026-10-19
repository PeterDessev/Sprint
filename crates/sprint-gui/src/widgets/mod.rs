//! Reusable Sprint widgets (pure egui, styled by [`crate::theme`]).
//!
//! Every widget reads its colors from [`crate::theme::ui_palette`], so call
//! [`crate::theme::setup`] once at startup. Widgets never panic on odd
//! input: NaN and out-of-range values are clamped or shown as "—".
//!
//! # Layout
//! * [`card`] / [`Card`] — raised section with icon + title header:
//!   `card(ui, icons::FAN, "Fan", |_| {}, |ui| { ui.label("100%"); });`
//! * [`section_header`] / [`SectionHeader`] — uppercase label with a hairline:
//!   `section_header(ui, "Temperatures");`
//! * [`EmptyState`] — placeholder with icon, message and optional action:
//!   `EmptyState::new(icons::FILE, "No file").action("Open…").show(ui).action_clicked()`
//!
//! # Buttons
//! * [`IconButton`] — square icon button (ghost / normal / primary / danger,
//!   small / regular / large) with tooltip:
//!   `ui.add(IconButton::new(icons::HOME).tooltip("Home all")).clicked()`
//! * [`ActionButton`] — icon + label button, e.g. big machine actions:
//!   `ui.add(ActionButton::new("Print").icon(icons::PLAY).primary().large())`
//! * [`EStopButton`] — the red emergency stop (badge or octagon):
//!   `ui.add(EStopButton::new()).clicked()`
//!
//! # Status
//! * [`StatusPill`] — dot + label capsule:
//!   `ui.add(StatusPill::new("Connected", Tone::Success));`
//! * [`Badge`] — small tinted tag: `ui.add(Badge::new("BETA").tone(Tone::Info));`
//! * [`Chip`] — selectable capsule: `ui.add(Chip::new("Travel").selected(on)).clicked()`
//! * [`status_dot`] — bare colored dot: `status_dot(ui, Tone::Danger, 8.0);`
//!
//! # Selection
//! * [`Toggle`] — on/off switch: `ui.add(Toggle::new(&mut on).label("Autoscroll")).changed()`
//! * [`SegmentedControl`] — one of many with a sliding highlight:
//!   `ui.add(SegmentedControl::new(&mut view, &[(View::Iso, "3D"), (View::Top, "Layer")]))`

#![warn(missing_docs)]

mod button;
mod card;
mod status;
mod toggle;

pub use button::{ActionButton, ButtonKind, ButtonSize, EStopButton, EStopStyle, IconButton};
pub use card::{
    Card, EmptyState, EmptyStateResponse, SectionHeader, card, card_frame, section_header,
};
pub use status::{Badge, Chip, StatusPill, status_dot};
pub use toggle::{SegmentedControl, Toggle};

use egui::{CornerRadius, FontId, Rect, Response, Stroke, StrokeKind, Ui};

use crate::{fonts, theme};

/// A widget's response plus the action the user triggered this frame, if
/// any. Returned by the composite controls (jog pad, temperature card, ...).
#[derive(Debug)]
pub struct ActionResponse<A> {
    /// The response of the whole widget.
    pub response: Response,
    /// The action triggered this frame.
    pub action: Option<A>,
}

impl<A> ActionResponse<A> {
    /// Wrap a response with an optional action.
    pub fn new(response: Response, action: Option<A>) -> Self {
        ActionResponse { response, action }
    }

    /// Map the action type.
    pub fn map<B>(self, f: impl FnOnce(A) -> B) -> ActionResponse<B> {
        ActionResponse {
            response: self.response,
            action: self.action.map(f),
        }
    }
}

/// Font for an icon glyph: the Phosphor-only family when Sprint's fonts are
/// installed (best vertical centering), else the proportional family (so a
/// missing family can never panic).
pub(crate) fn icon_font(ui: &Ui, size: f32) -> FontId {
    if fonts::is_installed(ui.ctx()) {
        FontId::new(size, fonts::icon_family())
    } else {
        FontId::proportional(size)
    }
}

/// Animated hover amount (0..=1) of a response, for smooth color fades.
pub(crate) fn hover_t(ui: &Ui, response: &Response) -> f32 {
    let hovered = response.hovered() && ui.is_enabled();
    ui.ctx()
        .animate_bool_with_time(response.id.with("hover"), hovered, 0.08)
}

/// Accent ring around a widget that has keyboard focus.
pub(crate) fn paint_focus_ring(ui: &Ui, response: &Response, rect: Rect, radius: CornerRadius) {
    if response.has_focus() {
        let p = theme::ui_palette(ui);
        ui.painter().rect_stroke(
            rect.expand(2.0),
            radius + 2,
            Stroke::new(2.0, theme::with_alpha(p.accent, 0.8)),
            StrokeKind::Outside,
        );
    }
}
