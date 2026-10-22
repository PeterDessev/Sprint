//! [`Toasts`]: stacked, auto-dismissing notifications.

use egui::{
    Align, Align2, Area, Context, CornerRadius, FontId, Frame, Id, Layout, Margin, Order, Rect,
    Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

use std::time::{Duration, Instant};

use sprint_host::{Delivery, Tracked};

use crate::icons;
use crate::theme::{self, Palette, TextRole, Tone, with_alpha};

use super::{IconButton, icon_font};

/// Severity of a toast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToastKind {
    /// Neutral information.
    Info,
    /// Something finished well.
    Success,
    /// Needs attention.
    Warning,
    /// Something failed.
    Error,
}

impl ToastKind {
    /// Semantic color.
    pub fn tone(self) -> Tone {
        match self {
            ToastKind::Info => Tone::Info,
            ToastKind::Success => Tone::Success,
            ToastKind::Warning => Tone::Warning,
            ToastKind::Error => Tone::Danger,
        }
    }

    /// Icon glyph.
    pub fn icon(self) -> &'static str {
        match self {
            ToastKind::Info => icons::INFO,
            ToastKind::Success => icons::SUCCESS,
            ToastKind::Warning => icons::WARNING,
            ToastKind::Error => icons::ERROR,
        }
    }

    /// Default display time in seconds (errors stay longer).
    pub fn default_duration(self) -> f32 {
        match self {
            ToastKind::Info | ToastKind::Success => 4.0,
            ToastKind::Warning => 6.0,
            ToastKind::Error => 8.0,
        }
    }
}

/// Identifies a toast for [`Toasts::dismiss`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToastId(u64);

/// One notification. Configure it through the `&mut Toast` returned by
/// [`Toasts::add`] and friends.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    id: ToastId,
    kind: ToastKind,
    title: String,
    message: Option<String>,
    duration: Option<f32>,
    elapsed: f32,
    count: u32,
}

impl Toast {
    /// Its id.
    pub fn id(&self) -> ToastId {
        self.id
    }

    /// Its severity.
    pub fn kind(&self) -> ToastKind {
        self.kind
    }

    /// Its title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Its message, if any.
    pub fn message_text(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// How many identical toasts were merged into this one.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Seconds left before it disappears (`None` = sticky).
    pub fn remaining(&self) -> Option<f32> {
        self.duration.map(|d| (d - self.elapsed).max(0.0))
    }

    /// Add a second line of detail.
    pub fn message(&mut self, message: impl Into<String>) -> &mut Self {
        let message = message.into();
        self.message = (!message.trim().is_empty()).then_some(message);
        self
    }

    /// Display time in seconds (non-finite or negative values keep the
    /// default).
    pub fn duration(&mut self, seconds: f32) -> &mut Self {
        if seconds.is_finite() && seconds >= 0.0 {
            self.duration = Some(seconds);
        }
        self
    }

    /// Stay until dismissed.
    pub fn sticky(&mut self) -> &mut Self {
        self.duration = None;
        self
    }

    fn expired(&self) -> bool {
        self.duration.is_some_and(|d| self.elapsed >= d)
    }
}

/// Stacked notifications in a screen corner (top-right by default). Keep
/// one `Toasts` in the app state, add to it from anywhere and call
/// [`show`](Self::show) once per frame.
///
/// ```no_run
/// # use sprint_gui::widgets::Toasts;
/// # fn demo(ctx: &egui::Context, toasts: &mut Toasts) {
/// toasts.success("Print finished");
/// toasts.error("Thermal runaway").message("Heater 0 stopped heating").sticky();
/// toasts.show(ctx);
/// # }
/// ```
///
/// Adding a toast identical to the newest visible one bumps a "×2"
/// counter instead of stacking duplicates.
///
/// A command the printer has yet to carry out gets its toast when the
/// printer acknowledges it: [`after`](Self::after), then
/// [`follow`](Self::follow) once per frame.
#[derive(Debug)]
pub struct Toasts {
    items: Vec<Toast>,
    pending: Vec<Pending>,
    next_id: u64,
    anchor: Align2,
    offset: Vec2,
    width: f32,
    max_visible: usize,
}

impl Default for Toasts {
    fn default() -> Self {
        Toasts::new()
    }
}

/// Default distance from the top-right corner: below a menu bar plus a
/// toolbar, so toasts never cover top-bar controls such as the E-STOP.
pub const TOAST_OFFSET: Vec2 = vec2(-16.0, 84.0);

/// At most this many toasts are kept (the oldest are dropped).
const MAX_TOASTS: usize = 32;

/// How long a followed command may wait for the printer before a toast
/// says that it waits ([`Followup::waiting`]).
pub const FOLLOWUP_NOTE_AFTER: Duration = Duration::from_millis(1500);

/// What to tell about a command sent with
/// [`Printer::send_tracked`](sprint_host::Printer::send_tracked) (see
/// [`Toasts::after`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Followup {
    /// Success toast once the printer acknowledged the command.
    pub done: Notice,
    /// Info toast when it still waits after [`FOLLOWUP_NOTE_AFTER`] (the
    /// printer is busy, for example heating).
    pub waiting: Notice,
    /// Error toast when the printer will not acknowledge it.
    pub lost: Notice,
}

/// The title and message of a toast to come.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// Title.
    pub title: String,
    /// Message under the title, if any.
    pub message: Option<String>,
}

impl Notice {
    /// A notice with `title` and no message.
    pub fn new(title: impl Into<String>) -> Notice {
        Notice {
            title: title.into(),
            message: None,
        }
    }

    /// The same with `message`.
    pub fn message(mut self, message: impl Into<String>) -> Notice {
        self.message = Some(message.into());
        self
    }
}

/// A command whose toast waits for the printer.
#[derive(Debug)]
struct Pending {
    tracked: Tracked,
    followup: Followup,
    since: Instant,
    noted: bool,
}

impl Toasts {
    /// Top-right below the toolbars ([`TOAST_OFFSET`]), 340 pt wide, at
    /// most 5 visible.
    pub fn new() -> Self {
        Toasts {
            items: Vec::new(),
            pending: Vec::new(),
            next_id: 1,
            anchor: Align2::RIGHT_TOP,
            offset: TOAST_OFFSET,
            width: 340.0,
            max_visible: 5,
        }
    }

    /// Screen corner and offset from it. Keep toasts clear of controls
    /// that must stay clickable (a toast takes the clicks under it).
    pub fn anchor(mut self, anchor: Align2, offset: Vec2) -> Self {
        self.anchor = anchor;
        if offset.x.is_finite() && offset.y.is_finite() {
            self.offset = offset;
        }
        self
    }

    /// Width of each toast (220..=600).
    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.clamp(220.0, 600.0);
        }
        self
    }

    /// How many toasts are visible at once (1..=10, newest first).
    pub fn max_visible(mut self, n: usize) -> Self {
        self.max_visible = n.clamp(1, 10);
        self
    }

    /// Add a toast (or merge it into an identical newest one).
    pub fn add(&mut self, kind: ToastKind, title: impl Into<String>) -> &mut Toast {
        let title = title.into();
        let duplicate = self.items.last().is_some_and(|t| {
            t.kind == kind && t.title == title && t.message.is_none() && !t.expired()
        });
        if !duplicate {
            if self.items.len() >= MAX_TOASTS {
                self.items.remove(0);
            }
            let id = ToastId(self.next_id);
            self.next_id = self.next_id.wrapping_add(1);
            self.items.push(Toast {
                id,
                kind,
                title,
                message: None,
                duration: Some(kind.default_duration()),
                elapsed: 0.0,
                count: 0,
            });
        }
        // `items` is never empty here: we either matched the last toast or
        // just pushed one.
        let last = self.items.len() - 1;
        let toast = &mut self.items[last];
        toast.count = toast.count.saturating_add(1);
        toast.elapsed = 0.0;
        toast
    }

    /// Add an info toast.
    pub fn info(&mut self, title: impl Into<String>) -> &mut Toast {
        self.add(ToastKind::Info, title)
    }

    /// Add a success toast.
    pub fn success(&mut self, title: impl Into<String>) -> &mut Toast {
        self.add(ToastKind::Success, title)
    }

    /// Add a warning toast.
    pub fn warning(&mut self, title: impl Into<String>) -> &mut Toast {
        self.add(ToastKind::Warning, title)
    }

    /// Add an error toast.
    pub fn error(&mut self, title: impl Into<String>) -> &mut Toast {
        self.add(ToastKind::Error, title)
    }

    /// Tell about `tracked` when the printer acknowledges it, or that it
    /// will not, rather than now: see [`Followup`] and
    /// [`follow`](Self::follow).
    pub fn after(&mut self, tracked: Tracked, followup: Followup) {
        self.pending.push(Pending {
            tracked,
            followup,
            since: Instant::now(),
            noted: false,
        });
    }

    /// Commands still waiting for the printer ([`after`](Self::after)).
    pub fn followed(&self) -> usize {
        self.pending.len()
    }

    /// Turn the commands of [`after`](Self::after) the printer
    /// acknowledged, or will not, into toasts. `printer_gone` (it
    /// disconnected or halted) means none of them will be acknowledged
    /// any more. Call once per frame.
    pub fn follow(&mut self, printer_gone: bool) {
        for mut p in std::mem::take(&mut self.pending) {
            match p.tracked.poll() {
                Delivery::Acknowledged => self.notice(ToastKind::Success, p.followup.done),
                Delivery::Rejected(answer) => {
                    let answer = crate::format::sanitize(answer.trim(), 200);
                    let lost = p.followup.lost.message(format!("The printer answered: {answer}"));
                    self.notice(ToastKind::Error, lost);
                }
                Delivery::Lost => self.notice(ToastKind::Error, p.followup.lost),
                Delivery::Pending if printer_gone => {
                    self.notice(ToastKind::Error, p.followup.lost);
                }
                Delivery::Pending => {
                    if !p.noted && p.since.elapsed() >= FOLLOWUP_NOTE_AFTER {
                        p.noted = true;
                        self.notice(ToastKind::Info, p.followup.waiting.clone());
                    }
                    self.pending.push(p);
                }
            }
        }
    }

    fn notice(&mut self, kind: ToastKind, notice: Notice) {
        let toast = self.add(kind, notice.title);
        if let Some(message) = notice.message {
            toast.message(message);
        }
    }

    /// Remove a toast; `false` if it was already gone.
    pub fn dismiss(&mut self, id: ToastId) -> bool {
        let before = self.items.len();
        self.items.retain(|t| t.id != id);
        self.items.len() != before
    }

    /// Remove every toast.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Number of live toasts.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// No live toasts.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Live toasts, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.items.iter()
    }

    /// Advance the timers of the visible toasts by `dt` seconds, except the
    /// `paused` one (under the pointer), and drop expired ones.
    pub fn tick(&mut self, dt: f32, paused: Option<ToastId>) {
        let dt = if dt.is_finite() {
            dt.clamp(0.0, 0.25)
        } else {
            0.0
        };
        let visible = self.visible_ids();
        for t in &mut self.items {
            if Some(t.id) != paused && visible.contains(&t.id) {
                t.elapsed += dt;
            }
        }
        self.items.retain(|t| !t.expired());
    }

    /// The ids currently on screen, newest first.
    fn visible_ids(&self) -> Vec<ToastId> {
        self.items
            .iter()
            .rev()
            .take(self.max_visible)
            .map(|t| t.id)
            .collect()
    }

    /// Show the stack and run the timers. Call once per frame.
    pub fn show(&mut self, ctx: &Context) {
        if self.items.is_empty() {
            return;
        }
        let p = theme::palette(ctx);
        let mut dismissed = None;
        let mut hovered = None;
        let visible = self.visible_ids();
        let width = self.width;
        // Newest at the top for top anchors, at the bottom for bottom ones.
        let mut order: Vec<&Toast> = visible
            .iter()
            .filter_map(|id| self.items.iter().find(|t| t.id == *id))
            .collect();
        if self.anchor.y() == Align::Max {
            order.reverse();
        }
        Area::new(Id::new("sprint-gui::toasts"))
            .order(Order::Tooltip)
            .anchor(self.anchor, self.offset)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 8.0;
                for toast in order {
                    let (r, close) = toast_ui(ui, &p, toast, width);
                    if close {
                        dismissed = Some(toast.id);
                    }
                    if r.contains_pointer() {
                        hovered = Some(toast.id);
                    }
                }
            });
        if let Some(id) = dismissed {
            self.dismiss(id);
        }
        let dt = ctx.input(|i| i.stable_dt);
        self.tick(dt, hovered);
        if self.items.iter().any(|t| t.duration.is_some()) {
            ctx.request_repaint();
        }
    }
}

/// Paint one toast; returns its response and whether close was clicked.
fn toast_ui(ui: &mut Ui, p: &Palette, toast: &Toast, width: f32) -> (egui::Response, bool) {
    let tone = p.tone(toast.kind.tone());
    // Fade out over the last 0.3 s.
    let opacity = toast
        .remaining()
        .map_or(1.0, |left| (left / 0.3).clamp(0.0, 1.0));
    let mut close = false;
    let frame = Frame::new()
        .fill(if p.dark { p.raised } else { p.surface })
        .stroke(Stroke::new(
            1.0,
            if p.dark { p.border_strong } else { p.border },
        ))
        .corner_radius(CornerRadius::same(theme::RADIUS_LG))
        .inner_margin(Margin {
            left: 12,
            right: 8,
            top: 10,
            bottom: 12,
        })
        .shadow(ui.visuals().popup_shadow);
    let inner = ui
        .scope(|ui| {
            ui.multiply_opacity(opacity);
            frame.show(ui, |ui| {
                ui.set_width(width - 20.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let (icon_rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                    ui.painter().circle_filled(
                        icon_rect.center(),
                        14.0,
                        with_alpha(tone, if p.dark { 0.18 } else { 0.14 }),
                    );
                    ui.painter().text(
                        icon_rect.center(),
                        Align2::CENTER_CENTER,
                        toast.kind.icon(),
                        icon_font(ui, 16.0),
                        tone,
                    );
                    let text_w = ui.available_width() - 30.0;
                    ui.allocate_ui_with_layout(
                        vec2(text_w.max(40.0), 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.add_space(4.0);
                            let mut title = toast.title.clone();
                            if toast.count > 1 {
                                title.push_str(&format!("  ×{}", toast.count));
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(title)
                                        .font(FontId::new(
                                            13.5,
                                            theme::font(ui, TextRole::Title).family,
                                        ))
                                        .color(p.text),
                                )
                                .wrap(),
                            );
                            if let Some(message) = &toast.message {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(message).size(12.5).color(p.text_dim),
                                    )
                                    .wrap(),
                                );
                            }
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        let r = ui.add(
                            IconButton::new(icons::CLOSE)
                                .ghost()
                                .small()
                                .tooltip("Dismiss"),
                        );
                        close = r.clicked();
                    });
                });
            })
        })
        .inner;
    let rect = inner.response.rect;
    // Remaining-time line along the bottom edge.
    if let (Some(left), Some(total)) = (toast.remaining(), toast.duration)
        && total > 0.0
    {
        let frac = (left / total).clamp(0.0, 1.0);
        let y = rect.max.y - 3.0;
        let x0 = rect.min.x + 10.0;
        let x1 = rect.max.x - 10.0;
        let bar = Rect::from_min_max(pos2(x0, y - 1.0), pos2(x0 + (x1 - x0) * frac, y + 1.0));
        ui.painter().rect_filled(
            bar,
            CornerRadius::same(255),
            with_alpha(tone, 0.55 * opacity),
        );
    }
    let response = ui.interact(rect, Id::new(("sprint-toast", toast.id.0)), Sense::hover());
    let label = match &toast.message {
        Some(m) => format!("{}: {m}", toast.title),
        None => toast.title.clone(),
    };
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &label));
    (response, close)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_configure_and_dismiss() {
        let mut t = Toasts::new();
        let id = t.error("Thermal runaway").message("Heater 0").sticky().id();
        t.success("Done").duration(1.0);
        assert_eq!(t.len(), 2);
        let first = t.iter().next().expect("toast");
        assert_eq!(first.kind(), ToastKind::Error);
        assert_eq!(first.message_text(), Some("Heater 0"));
        assert_eq!(first.remaining(), None);
        assert!(t.dismiss(id));
        assert!(!t.dismiss(id));
        t.clear();
        assert!(t.is_empty());
    }

    #[test]
    fn timers_expire_and_pause() {
        let mut t = Toasts::new();
        let a = t.info("a").duration(1.0).id();
        t.info("b").duration(1.0);
        t.tick(0.25, Some(a));
        t.tick(0.25, Some(a));
        t.tick(0.25, Some(a));
        t.tick(0.25, Some(a));
        // "b" expired, the hovered "a" did not.
        assert_eq!(t.len(), 1);
        assert_eq!(t.iter().next().map(Toast::id), Some(a));
        for _ in 0..4 {
            t.tick(0.25, None);
        }
        assert!(t.is_empty());
    }

    #[test]
    fn tick_ignores_bad_dt_and_clamps_big_steps() {
        let mut t = Toasts::new();
        t.info("x").duration(1.0);
        t.tick(f32::NAN, None);
        t.tick(-5.0, None);
        t.tick(100.0, None);
        assert_eq!(t.len(), 1, "one huge frame must not skip the toast");
        let left = t.iter().next().and_then(Toast::remaining).unwrap_or(0.0);
        assert!((left - 0.75).abs() < 1e-6);
    }

    #[test]
    fn duplicates_merge() {
        let mut t = Toasts::new();
        t.warning("Link noisy");
        t.warning("Link noisy");
        let id = t.warning("Link noisy").id();
        assert_eq!(t.len(), 1);
        assert_eq!(t.iter().next().map(Toast::count), Some(3));
        t.info("other");
        t.warning("Link noisy");
        assert_eq!(t.len(), 3);
        assert_ne!(t.iter().last().map(Toast::id), Some(id));
    }

    #[test]
    fn only_visible_toasts_count_down_and_list_is_bounded() {
        let mut t = Toasts::new().max_visible(1);
        let old = t.info("old").duration(1.0).id();
        t.info("new").duration(1.0);
        t.tick(0.25, None);
        t.tick(0.25, None);
        t.tick(0.25, None);
        t.tick(0.25, None);
        // Only the newest was visible; the old one waits its turn.
        assert_eq!(t.iter().map(Toast::id).collect::<Vec<_>>(), vec![old]);
        let mut many = Toasts::new();
        for i in 0..100 {
            many.info(format!("t{i}"));
        }
        assert_eq!(many.len(), MAX_TOASTS);
    }

    #[test]
    fn invalid_settings_are_ignored() {
        let mut t = Toasts::new()
            .width(f32::NAN)
            .max_visible(0)
            .anchor(Align2::LEFT_BOTTOM, vec2(f32::NAN, 0.0));
        assert_eq!(t.width, 340.0);
        assert_eq!(t.max_visible, 1);
        assert_eq!(t.offset, TOAST_OFFSET);
        let toast = t.info("x").duration(f32::INFINITY).message("  ");
        assert_eq!(toast.remaining(), Some(4.0));
        assert_eq!(toast.message_text(), None);
    }
}
