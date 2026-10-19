//! Reusable Sprint widgets: pure egui, styled by [`crate::theme`].
//!
//! # Getting started
//!
//! Call [`crate::theme::setup`] once (fonts + style), then use the widgets
//! anywhere. Colors come from [`crate::theme::ui_palette`], so a
//! [`crate::theme::scope`] with the other theme renders correctly inside a
//! window of the active one.
//!
//! ```no_run
//! use sprint_gui::{icons, theme::{self, HeaterKind, Theme, Tone}, widgets::*};
//!
//! fn controls(ui: &mut egui::Ui, history: &[f32]) {
//!     ui.add(StatusPill::new("Connected", Tone::Success));
//!     card(ui, icons::MOVE, "Jog", |_| {}, |ui| {
//!         if let Some(action) = JogPad::new().show(ui).action {
//!             println!("{}", action.describe());
//!         }
//!     });
//!     let heater = TemperatureCard::new("Hotend", HeaterKind::Hotend)
//!         .current(212.4)
//!         .target(215.0)
//!         .history(history)
//!         .show(ui);
//!     if let Some(TemperatureAction::SetTarget(t)) = heater.action { /* M104 S{t} */ }
//! }
//! # let ctx = egui::Context::default();
//! # theme::setup(&ctx, Theme::Dark);
//! ```
//!
//! # Conventions
//!
//! * Simple widgets implement [`egui::Widget`] (`ui.add(...)` returns a
//!   [`Response`]); composite controls have a `show(ui)` that returns an
//!   [`ActionResponse`] whose `action` says what the user asked for. Widgets
//!   never talk to the printer: the caller turns actions into G-code.
//! * Builders are `#[must_use]`; invalid builder arguments (NaN sizes,
//!   inverted ranges, ...) are ignored instead of panicking.
//! * NaN and out-of-range data from files or printers are shown as "—",
//!   hatched or clamped; nothing panics on them.
//! * Internal ids derive from the parent `Ui`'s unique id, so copies of a
//!   widget in sibling `ui.columns` stay independent. Set `id_salt` when two
//!   identical widgets share one parent `Ui`.
//! * Everything exposes AccessKit labels, so `egui_kittest` can drive it.
//!
//! # Catalog
//!
//! **Layout**
//! * [`card`] / [`Card`] — raised section with icon, title and header controls:
//!   `card(ui, icons::FAN, "Fan", |ui| { ui.add(IconButton::new(icons::OFF).ghost().small()); }, |ui| { ui.label("100%"); });`
//! * [`card_frame`] — just the card [`egui::Frame`]: `card_frame(&theme::ui_palette(ui)).show(ui, |ui| { /* ... */ });`
//! * [`section_header`] / [`SectionHeader`] — uppercase label with a hairline:
//!   `section_header(ui, "Temperatures");`
//! * [`EmptyState`] — placeholder with icon, message and optional action:
//!   `EmptyState::new(icons::FILE, "No file").action("Open…").show(ui).action_clicked()`
//!
//! **Buttons**
//! * [`IconButton`] — square icon button, ghost / normal / primary / danger in
//!   small / regular / large, with tooltip:
//!   `ui.add(IconButton::new(icons::HOME).tooltip("Home all")).clicked()`
//! * [`ActionButton`] — label with optional icon; `.large()` for big machine actions:
//!   `ui.add(ActionButton::new("Start print").icon(icons::PLAY).primary().large()).clicked()`
//! * [`EStopButton`] — the unmistakable red emergency stop, badge or octagon:
//!   `if ui.add(EStopButton::new()).clicked() { /* M112 */ }`
//!
//! **Status**
//! * [`StatusPill`] — colored dot (or icon) + label capsule:
//!   `ui.add(StatusPill::new("Connecting", Tone::Warning).pulse(true));`
//! * [`Badge`] — small tinted or filled tag: `ui.add(Badge::new("BETA").tone(Tone::Info));`
//! * [`Chip`] — selectable capsule with optional icon or swatch:
//!   `if ui.add(Chip::new("Travel").selected(on)).clicked() { on = !on; }`
//! * [`status_dot`] — bare colored dot: `status_dot(ui, Tone::Danger, 8.0);`
//!
//! **Selection**
//! * [`Toggle`] — on/off switch: `ui.add(Toggle::new(&mut on).label("Autoscroll")).changed()`
//! * [`SegmentedControl`] — one of many with a sliding highlight:
//!   `ui.add(SegmentedControl::new(&mut view, &[(View::Iso, "3D"), (View::Top, "Layer")]))`
//!
//! **Motion**
//! * [`JogPad`] — circular XY jog: four step rings, home center, home / motors-off corners:
//!   `if let Some(a) = JogPad::new().size(240.0).show(ui).action { /* JogAction */ }`
//! * [`ZJog`] — vertical ± strip with a home-Z cell:
//!   `if let Some(a) = ZJog::new().height(240.0).show(ui).action { /* JogAction */ }`
//! * [`ExtruderControl`] — length, speed, quick lengths, extrude / retract:
//!   `ExtruderControl::new(&mut len, &mut speed).blocked(cold.then_some("Too cold")).show(ui).action`
//!
//! **Thermal**
//! * [`TemperatureCard`] — heater readout colored by [`HeaterState`], editable
//!   target (Enter applies), presets, off button, power bar and sparkline:
//!   `TemperatureCard::new("Bed", HeaterKind::Bed).current(59.6).target(60.0).show(ui).action`
//!
//! **Data**
//! * [`Sparkline`] — compact smooth trend: `ui.add(Sparkline::new(&temps).reference(Some(215.0)));`
//! * [`MiniChart`] — smooth multi-series line chart with grid, legend and read-out:
//!   `MiniChart::new().series(ChartSeries::new("Hotend", &pts, color).fill(true)).unit("°C").show(ui);`
//! * [`Heatmap`] — bed mesh / 2D grid with a diverging colormap, labels, legend:
//!   `let cell = Heatmap::from_mesh(&mesh).show(ui).clicked;`
//! * [`KeyValueGrid`] — info table with dim labels and click-to-copy values:
//!   `KeyValueGrid::new("fw").show(ui, |kv| { kv.row("Firmware", "Marlin 2.1"); });`
//!
//! **Progress**
//! * [`ProgressRing`] — circular progress with centered text:
//!   `ui.add(ProgressRing::new(0.42).sublabel("1:23 left"));`
//! * [`LinearProgress`] — slim bar, optional caption, indeterminate mode:
//!   `ui.add(LinearProgress::new(0.66).label("Uploading").show_percentage(true));`
//!
//! **Input**
//! * [`NumericField`] — label + value with unit, range, decimals and a
//!   modified dot / reset against an original value:
//!   `ui.add(NumericField::new(&mut steps).label("X steps").unit("steps/mm").original(80.0)).changed()`
//!
//! **Feedback**
//! * [`Toasts`] — stacked auto-dismissing notifications, top-right by default:
//!   `toasts.success("Print finished"); toasts.show(ctx);`
//! * [`ConfirmDialog`] — modal question with optional danger styling:
//!   `ConfirmDialog::new("reset", "Reset settings?").danger(true).show(ctx)` → `Option<DialogResult>`
//!
//! **Helpers**: [`smooth_points`] (monotone cubic interpolation),
//! [`nice_ticks`], [`finite_runs`], [`diverging_color`], [`GridStats`],
//! [`percent_label`], [`format_number`], [`is_modified`],
//! [`format_temperature`], [`parse_temperature`], [`step_label`],
//! [`signed_step`], [`signed_value`] and the pure jog geometry
//! [`JogGeometry`].

#![warn(missing_docs)]

mod button;
mod card;
mod chart;
mod dialog;
mod heatmap;
mod jog;
mod keyvalue;
mod numeric;
mod progress;
mod status;
mod temperature;
mod toast;
mod toggle;

pub use button::{ActionButton, ButtonKind, ButtonSize, EStopButton, EStopStyle, IconButton};
pub use card::{
    Card, EmptyState, EmptyStateResponse, SectionHeader, card, card_frame, section_header,
};
pub use chart::{ChartSeries, MiniChart, Sparkline, finite_runs, nice_ticks, smooth_points};
pub use dialog::{ConfirmDialog, DialogResult};
pub use heatmap::{
    GridStats, Heatmap, HeatmapCell, HeatmapResponse, diverging_color, signed_value,
};
pub use jog::{
    Axis, ExtruderAction, ExtruderControl, HomeTarget, JOG_RINGS, JogAction, JogCorner,
    JogDirection, JogGeometry, JogPad, JogRegion, ZJog, signed_step, step_label,
};
pub use keyvalue::{KeyValueGrid, KeyValueRows};
pub use numeric::{NumericField, format_number, is_modified};
pub use progress::{LinearProgress, ProgressRing, percent_label};
pub use status::{Badge, Chip, StatusPill, status_dot};
pub use temperature::{
    HOT_THRESHOLD, HeaterState, TemperatureAction, TemperatureCard, format_temperature,
    parse_temperature,
};
pub use toast::{Toast, ToastId, ToastKind, Toasts};
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

/// An id for a widget's state or interaction, derived from `salt` and the
/// *unique* id of the parent `Ui`.
///
/// `Ui::make_persistent_id` uses the parent's *stable* id, which sibling
/// `Ui`s can share (e.g. the columns of `ui.columns`), so two copies of a
/// widget in neighbouring columns would clash. The unique id is stable as
/// long as the widget hierarchy is.
pub(crate) fn widget_id(ui: &Ui, salt: impl std::hash::Hash + std::fmt::Debug) -> egui::Id {
    ui.unique_id().with(salt)
}

/// The largest width or height (points) any widget accepts from a builder.
pub(crate) const MAX_EXTENT: f32 = 8192.0;

/// `value` clamped to `min..=max`, or `None` when it is NaN or infinite
/// (so builders keep their default).
pub(crate) fn finite_clamped(value: f32, min: f32, max: f32) -> Option<f32> {
    value.is_finite().then(|| value.clamp(min, max))
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
