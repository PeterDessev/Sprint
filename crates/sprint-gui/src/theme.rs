//! The "Sprint" design language: color palettes, typography roles, metrics
//! and the egui [`Style`] built from them.
//!
//! Call [`setup`] once at startup (for example from eframe's
//! `CreationContext`), before the first frame:
//!
//! ```no_run
//! # fn demo(ctx: &egui::Context) {
//! use sprint_gui::theme::{self, Theme};
//! theme::setup(ctx, Theme::Dark); // fonts + style
//! // Later, e.g. from a settings toggle:
//! theme::apply(ctx, Theme::Light);
//! # }
//! ```
//!
//! Widgets read colors through [`ui_palette`] (which follows the `Ui`'s own
//! dark/light visuals, so a [`scope`] with a different theme works inside a
//! window) or [`palette`] (which follows the context's active theme). Both
//! are a cheap copy of a small `Copy` struct.
//!
//! Fonts for the named text roles ([`TextRole`]) are resolved with [`font`],
//! which falls back to sensible defaults when [`apply`] has not been called,
//! so widgets never panic on a missing text style.

#![warn(missing_docs)]

use std::sync::Arc;

use egui::{
    Color32, Context, CornerRadius, FontFamily, FontId, Id, Margin, Shadow, Stroke, Style,
    TextStyle, Ui, Visuals,
    style::{HandleShape, ScrollStyle, Selection, TextCursorStyle, WidgetVisuals, Widgets},
};

use crate::fonts;

/// Light or dark appearance.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Theme {
    /// The default: calm dark surfaces with an orange accent.
    #[default]
    Dark,
    /// A bright variant for well-lit rooms.
    Light,
}

impl Theme {
    /// Both themes, dark first.
    pub const ALL: [Theme; 2] = [Theme::Dark, Theme::Light];

    /// `true` for [`Theme::Dark`].
    pub fn is_dark(self) -> bool {
        self == Theme::Dark
    }

    /// The other theme.
    #[must_use]
    pub fn toggled(self) -> Theme {
        match self {
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::Dark,
        }
    }

    /// The built-in palette of this theme.
    pub fn palette(self) -> Palette {
        match self {
            Theme::Dark => Palette::dark(),
            Theme::Light => Palette::light(),
        }
    }

    /// Display name ("Dark" / "Light").
    pub fn name(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
        }
    }

    /// The theme matching egui's dark/light flag.
    pub fn from_dark_mode(dark_mode: bool) -> Theme {
        if dark_mode { Theme::Dark } else { Theme::Light }
    }
}

impl From<Theme> for egui::Theme {
    fn from(theme: Theme) -> Self {
        match theme {
            Theme::Dark => egui::Theme::Dark,
            Theme::Light => egui::Theme::Light,
        }
    }
}

impl From<egui::Theme> for Theme {
    fn from(theme: egui::Theme) -> Self {
        match theme {
            egui::Theme::Dark => Theme::Dark,
            egui::Theme::Light => Theme::Light,
        }
    }
}

/// Semantic intent of a colored element (pills, badges, toasts, buttons).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Neutral gray.
    #[default]
    Neutral,
    /// The Sprint orange accent.
    Accent,
    /// Positive / done / connected.
    Success,
    /// Needs attention.
    Warning,
    /// Failure / destructive.
    Danger,
    /// Informational.
    Info,
}

impl Tone {
    /// Every tone, for galleries and tests.
    pub const ALL: [Tone; 6] = [
        Tone::Neutral,
        Tone::Accent,
        Tone::Success,
        Tone::Warning,
        Tone::Danger,
        Tone::Info,
    ];
}

/// The kind of heater, for colors and icons.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HeaterKind {
    /// A hotend (nozzle).
    #[default]
    Hotend,
    /// The heated bed.
    Bed,
    /// A heated chamber.
    Chamber,
}

/// The color tokens of a theme. `Copy` and cheap to pass around.
///
/// Surfaces stack from back to front: [`bg`](Self::bg) (app background) <
/// [`panel`](Self::panel) (side panels) < [`surface`](Self::surface) (cards)
/// < [`raised`](Self::raised) (controls inside cards).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Whether this is a dark palette.
    pub dark: bool,
    /// App background behind panels and the central area.
    pub bg: Color32,
    /// Side and top panels.
    pub panel: Color32,
    /// Cards and windows.
    pub surface: Color32,
    /// Controls (buttons, inputs) placed on a surface.
    pub raised: Color32,
    /// [`raised`](Self::raised) under the pointer.
    pub raised_hover: Color32,
    /// Recessed input fields (text edits, drag values).
    pub input: Color32,
    /// Hairline borders and separators.
    pub border: Color32,
    /// Borders of hovered or emphasised elements.
    pub border_strong: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text: labels, units, captions.
    pub text_dim: Color32,
    /// Tertiary text: placeholders, disabled hints.
    pub text_faint: Color32,
    /// Sprint orange.
    pub accent: Color32,
    /// Accent under the pointer.
    pub accent_hover: Color32,
    /// Accent while pressed.
    pub accent_pressed: Color32,
    /// Translucent accent for highlights and selections.
    pub accent_soft: Color32,
    /// Text and icons drawn on an accent fill.
    pub on_accent: Color32,
    /// Positive state.
    pub success: Color32,
    /// Attention state.
    pub warning: Color32,
    /// Error / destructive state (text, icons, outlines).
    pub danger: Color32,
    /// Solid fill of destructive buttons (deeper than [`danger`](Self::danger)
    /// so white text stays readable).
    pub danger_fill: Color32,
    /// Danger fill under the pointer.
    pub danger_hover: Color32,
    /// Danger fill while pressed.
    pub danger_pressed: Color32,
    /// Text and icons drawn on a danger fill.
    pub on_danger: Color32,
    /// Informational state.
    pub info: Color32,
    /// Hotend heater color.
    pub hotend: Color32,
    /// Bed heater color.
    pub bed: Color32,
    /// Chamber heater color.
    pub chamber: Color32,
    /// Categorical colors for chart series.
    pub plot: [Color32; 8],
    /// Shadow color of floating windows and popups.
    pub shadow: Color32,
}

impl Default for Palette {
    fn default() -> Self {
        Palette::dark()
    }
}

impl Palette {
    /// The default dark palette.
    pub fn dark() -> Palette {
        let accent = hex(0xFF7A1A);
        Palette {
            dark: true,
            bg: hex(0x0E1014),
            panel: hex(0x13161B),
            surface: hex(0x1A1E24),
            raised: hex(0x242931),
            raised_hover: hex(0x2D333C),
            input: hex(0x111419),
            border: hex(0x2A2F37),
            border_strong: hex(0x3B424D),
            text: hex(0xE7E9EC),
            text_dim: hex(0x9AA2AD),
            text_faint: hex(0x6E7682),
            accent,
            accent_hover: hex(0xFF8F3F),
            accent_pressed: hex(0xE5650B),
            accent_soft: with_alpha(accent, 0.18),
            on_accent: hex(0x1A0F06),
            success: hex(0x3FCF8E),
            warning: hex(0xF5B83D),
            danger: hex(0xF2555A),
            danger_fill: hex(0xD93A3F),
            danger_hover: hex(0xE5484D),
            danger_pressed: hex(0xBF2F34),
            on_danger: Color32::WHITE,
            info: hex(0x4DA3FF),
            hotend: hex(0xFF6242),
            bed: hex(0x5B8DFF),
            chamber: hex(0x3CC6B4),
            plot: [
                accent,
                hex(0x4DA3FF),
                hex(0x3FCF8E),
                hex(0xB18CFF),
                hex(0xF5B83D),
                hex(0xFF6FAE),
                hex(0x3CC6C6),
                hex(0xA0A8B4),
            ],
            shadow: Color32::from_black_alpha(110),
        }
    }

    /// The default light palette.
    pub fn light() -> Palette {
        let accent = hex(0xF26A0C);
        Palette {
            dark: false,
            bg: hex(0xEEF0F3),
            panel: hex(0xF6F7F9),
            surface: hex(0xFFFFFF),
            raised: hex(0xF0F2F5),
            raised_hover: hex(0xE5E8ED),
            input: hex(0xFFFFFF),
            border: hex(0xDCE0E6),
            border_strong: hex(0xC2C8D1),
            text: hex(0x15181D),
            text_dim: hex(0x5A626E),
            text_faint: hex(0x7E8692),
            accent,
            accent_hover: hex(0xFF7A1A),
            accent_pressed: hex(0xD35906),
            accent_soft: with_alpha(accent, 0.14),
            on_accent: Color32::WHITE,
            success: hex(0x17915A),
            warning: hex(0xB97C06),
            danger: hex(0xD92F35),
            danger_fill: hex(0xD92F35),
            danger_hover: hex(0xE5434A),
            danger_pressed: hex(0xB9252B),
            on_danger: Color32::WHITE,
            info: hex(0x1F72D8),
            hotend: hex(0xE2452A),
            bed: hex(0x3D6BE0),
            chamber: hex(0x10998A),
            plot: [
                accent,
                hex(0x1F72D8),
                hex(0x17915A),
                hex(0x8A5CE6),
                hex(0xB97C06),
                hex(0xD9418A),
                hex(0x13979C),
                hex(0x6B7380),
            ],
            shadow: Color32::from_black_alpha(30),
        }
    }

    /// The solid color of a tone (text, icons, dots).
    pub fn tone(&self, tone: Tone) -> Color32 {
        match tone {
            Tone::Neutral => self.text_dim,
            Tone::Accent => self.accent,
            Tone::Success => self.success,
            Tone::Warning => self.warning,
            Tone::Danger => self.danger,
            Tone::Info => self.info,
        }
    }

    /// A translucent background for a tone (pill and badge fills).
    pub fn tone_soft(&self, tone: Tone) -> Color32 {
        let amount = if self.dark { 0.16 } else { 0.12 };
        match tone {
            Tone::Neutral => with_alpha(self.text_dim, if self.dark { 0.14 } else { 0.10 }),
            _ => with_alpha(self.tone(tone), amount),
        }
    }

    /// The color of a heater kind.
    pub fn heater(&self, kind: HeaterKind) -> Color32 {
        match kind {
            HeaterKind::Hotend => self.hotend,
            HeaterKind::Bed => self.bed,
            HeaterKind::Chamber => self.chamber,
        }
    }

    /// Categorical chart color `index` (wraps around).
    pub fn plot_color(&self, index: usize) -> Color32 {
        self.plot[index % self.plot.len()]
    }

    /// Text color that reads well on `fill`: [`text`](Self::text) or a
    /// contrasting near-black / white.
    pub fn text_on(&self, fill: Color32) -> Color32 {
        readable_text_on(fill)
    }
}

// ---------------------------------------------------------------------------
// Metrics

/// Corner radius of small elements (badges, checkboxes), in points.
pub const RADIUS_SM: u8 = 4;
/// Corner radius of controls (buttons, inputs), in points.
pub const RADIUS: u8 = 6;
/// Corner radius of cards, in points.
pub const RADIUS_LG: u8 = 8;
/// Corner radius of windows and dialogs, in points.
pub const RADIUS_XL: u8 = 10;
/// Inner margin of cards, in points.
pub const CARD_PADDING: i8 = 12;
/// Standard gap between related items, in points.
pub const GAP: f32 = 8.0;
/// Gap between groups (cards, sections), in points.
pub const GAP_LG: f32 = 12.0;
/// Default height of interactive controls, in points.
pub const CONTROL_HEIGHT: f32 = 28.0;

// ---------------------------------------------------------------------------
// Typography

/// Named typography roles beyond egui's built-in text styles.
///
/// The built-in styles are set too: Heading 18 (semibold), Body 14,
/// Button 14, Small 12, Monospace 13.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextRole {
    /// Large numeric readouts (temperatures, progress): 28 pt semibold.
    Display,
    /// Medium numeric readouts (positions, stats): 20 pt semibold.
    Readout,
    /// Card and dialog titles: 14 pt semibold.
    Title,
    /// Section labels (shown uppercase): 11 pt semibold.
    Section,
    /// Axis ticks, legends and fine print: 11 pt.
    Caption,
}

impl TextRole {
    /// Every role.
    pub const ALL: [TextRole; 5] = [
        TextRole::Display,
        TextRole::Readout,
        TextRole::Title,
        TextRole::Section,
        TextRole::Caption,
    ];

    /// The name of the egui [`TextStyle::Name`] for this role.
    pub fn name(self) -> &'static str {
        match self {
            TextRole::Display => "Display",
            TextRole::Readout => "Readout",
            TextRole::Title => "Title",
            TextRole::Section => "Section",
            TextRole::Caption => "Caption",
        }
    }

    /// The egui text style (only present after [`apply`]; prefer [`font`]).
    pub fn text_style(self) -> TextStyle {
        TextStyle::Name(self.name().into())
    }

    /// Size in points.
    pub fn size(self) -> f32 {
        match self {
            TextRole::Display => 28.0,
            TextRole::Readout => 20.0,
            TextRole::Title => 14.0,
            TextRole::Section => 11.0,
            TextRole::Caption => 11.0,
        }
    }

    /// Whether the role uses the semibold family.
    pub fn is_strong(self) -> bool {
        !matches!(self, TextRole::Caption)
    }
}

/// The font of a typography role in `ui`'s style, falling back to a
/// proportional font of the role's size when the style has no such entry
/// (i.e. [`apply`] was not called) or names a font family the active fonts
/// lack. Never panics.
pub fn font(ui: &Ui, role: TextRole) -> FontId {
    usable_font(ui.ctx(), font_in(ui.style(), role))
}

/// `font`, or a proportional font of its size when its family is not
/// defined by the fonts egui uses right now (egui would panic on it).
fn usable_font(ctx: &Context, font: FontId) -> FontId {
    match &font.family {
        FontFamily::Name(_) if !fonts::family_is_active(ctx, &font.family) => {
            FontId::proportional(font.size)
        }
        _ => font,
    }
}

/// [`font`] for a [`Style`].
pub fn font_in(style: &Style, role: TextRole) -> FontId {
    style
        .text_styles
        .get(&role.text_style())
        .cloned()
        .unwrap_or_else(|| FontId::proportional(role.size()))
}

/// The font of one of egui's built-in text styles, with a fallback when the
/// style map lacks it. Never panics.
pub fn builtin_font(ui: &Ui, style: TextStyle) -> FontId {
    let fallback = match style {
        TextStyle::Small => FontId::proportional(12.0),
        TextStyle::Heading => FontId::proportional(18.0),
        TextStyle::Monospace => FontId::monospace(13.0),
        _ => FontId::proportional(14.0),
    };
    let font = ui
        .style()
        .text_styles
        .get(&style)
        .cloned()
        .unwrap_or(fallback);
    usable_font(ui.ctx(), font)
}

/// The font family for semibold text: Sprint's strong family when
/// [`fonts::install`] registered it and egui already uses those fonts,
/// otherwise the proportional family.
///
/// Before the context's first pass the active fonts cannot be checked, so
/// this answers the proportional family; [`apply`] then switches the
/// semibold roles over at the start of the first pass that has the fonts.
pub fn strong_family(ctx: &Context) -> FontFamily {
    if fonts::is_installed(ctx)
        && ctx.cumulative_pass_nr() > 0
        && fonts::family_is_active(ctx, &fonts::strong())
    {
        fonts::strong()
    } else {
        FontFamily::Proportional
    }
}

/// The text styles that use the semibold family.
fn strong_text_styles() -> impl Iterator<Item = TextStyle> {
    std::iter::once(TextStyle::Heading).chain(
        TextRole::ALL
            .into_iter()
            .filter(|role| role.is_strong())
            .map(TextRole::text_style),
    )
}

/// Point the semibold text styles of `style` at `family`, leaving entries
/// an app switched to another family alone.
fn set_strong_family(style: &mut Style, family: &FontFamily) {
    for key in strong_text_styles() {
        if let Some(font) = style.text_styles.get_mut(&key)
            && (font.family == FontFamily::Proportional || font.family == fonts::strong())
        {
            font.family = family.clone();
        }
    }
}

fn strong_applied_id() -> Id {
    Id::new("sprint-gui::theme::strong-applied")
}

/// At the start of each pass (fonts are active then): keep the semibold
/// text styles on Sprint's strong family exactly while egui's fonts define
/// it. Fonts installed after [`apply`] (or before the first pass) switch
/// them on; fonts replaced by an app without Sprint's families switch them
/// off instead of making egui panic. `root` is the pass's root `Ui`, made
/// before this runs, so it gets the change too.
fn sync_strong_family(root: &mut Ui) {
    let ctx = root.ctx().clone();
    let wanted = fonts::is_installed(&ctx) && fonts::family_is_active(&ctx, &fonts::strong());
    let applied = ctx
        .data(|d| d.get_temp::<bool>(strong_applied_id()))
        .unwrap_or(false);
    if wanted == applied {
        return;
    }
    let family = if wanted {
        fonts::strong()
    } else {
        FontFamily::Proportional
    };
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| set_strong_family(style, &family));
    }
    set_strong_family(root.style_mut(), &family);
    ctx.data_mut(|d| d.insert_temp(strong_applied_id(), wanted));
}

/// Register [`sync_strong_family`] once per context.
fn register_font_sync(ctx: &Context) {
    let registered = ctx.data_mut(|d| {
        std::mem::replace(
            d.get_temp_mut_or_default::<bool>(Id::new("sprint-gui::theme::font-sync")),
            true,
        )
    });
    if !registered {
        ctx.on_begin_pass(
            "sprint-gui font sync",
            Arc::new(|ui: &mut Ui| sync_strong_family(ui)),
        );
    }
}

/// The complete text style map for a given strong family.
fn text_styles(strong: &FontFamily) -> std::collections::BTreeMap<TextStyle, FontId> {
    let mut styles = std::collections::BTreeMap::new();
    styles.insert(TextStyle::Small, FontId::proportional(12.0));
    styles.insert(TextStyle::Body, FontId::proportional(14.0));
    styles.insert(TextStyle::Button, FontId::proportional(14.0));
    styles.insert(TextStyle::Heading, FontId::new(18.0, strong.clone()));
    styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    for role in TextRole::ALL {
        let family = if role.is_strong() {
            strong.clone()
        } else {
            FontFamily::Proportional
        };
        styles.insert(role.text_style(), FontId::new(role.size(), family));
    }
    styles
}

// ---------------------------------------------------------------------------
// Style

/// The egui [`Visuals`] for a palette.
pub fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let radius = CornerRadius::same(RADIUS);
    let widget = |bg: Color32, stroke: Stroke, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: stroke,
        corner_radius: radius,
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    };
    let pressed = mix(p.raised, p.bg, 0.35);
    v.widgets = Widgets {
        noninteractive: WidgetVisuals {
            bg_fill: p.surface,
            weak_bg_fill: p.surface,
            bg_stroke: Stroke::new(1.0, p.border),
            corner_radius: radius,
            fg_stroke: Stroke::new(1.0, p.text),
            expansion: 0.0,
        },
        inactive: widget(p.raised, Stroke::new(1.0, p.border), p.text),
        hovered: widget(p.raised_hover, Stroke::new(1.0, p.border_strong), p.text),
        active: widget(pressed, Stroke::new(1.0, p.accent), p.text),
        open: widget(p.raised_hover, Stroke::new(1.0, p.border_strong), p.text),
    };
    v.selection = Selection {
        bg_fill: with_alpha(p.accent, if p.dark { 0.30 } else { 0.22 }),
        stroke: Stroke::new(
            1.0,
            if p.dark {
                p.accent_hover
            } else {
                p.accent_pressed
            },
        ),
    };
    v.override_text_color = None;
    v.weak_text_color = Some(p.text_dim);
    v.hyperlink_color = if p.dark {
        p.accent_hover
    } else {
        p.accent_pressed
    };
    v.faint_bg_color = mix(p.surface, p.text, 0.025);
    v.extreme_bg_color = p.input;
    v.text_edit_bg_color = Some(p.input);
    v.code_bg_color = p.raised;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.window_corner_radius = CornerRadius::same(RADIUS_XL);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: p.shadow,
    };
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_highlight_topmost = false;
    v.menu_corner_radius = CornerRadius::same(RADIUS_LG);
    v.panel_fill = p.panel;
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: p.shadow,
    };
    v.text_cursor = TextCursorStyle {
        stroke: Stroke::new(2.0, p.accent),
        ..TextCursorStyle::default()
    };
    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = true;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = HandleShape::Circle;
    v.disabled_alpha = 0.45;
    v
}

/// The complete egui [`Style`] for a palette. `strong` is the family used
/// for semibold roles (see [`strong_family`]).
pub fn style(p: &Palette, strong: &FontFamily) -> Style {
    let mut style = Style {
        visuals: visuals(p),
        text_styles: text_styles(strong),
        ..Style::default()
    };
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(8.0, 6.0);
    s.button_padding = egui::vec2(10.0, 4.0);
    s.window_margin = Margin::same(14);
    s.menu_margin = Margin::same(6);
    s.indent = 16.0;
    s.interact_size = egui::vec2(40.0, 24.0);
    s.slider_width = 160.0;
    s.slider_rail_height = 4.0;
    s.combo_width = 120.0;
    s.text_edit_width = 160.0;
    s.icon_width = 16.0;
    s.icon_width_inner = 9.0;
    s.icon_spacing = 6.0;
    s.tooltip_width = 320.0;
    s.menu_width = 220.0;
    s.combo_height = 260.0;
    let mut scroll = ScrollStyle::floating();
    scroll.bar_width = 8.0;
    scroll.floating_width = 3.0;
    scroll.handle_min_length = 24.0;
    s.scroll = scroll;
    style.interaction.tooltip_delay = 0.35;
    style.interaction.show_tooltips_only_when_still = false;
    // A control surface, not a document: plain labels are not selectable
    // (values that matter offer click-to-copy instead).
    style.interaction.selectable_labels = false;
    style.animation_time = 0.12;
    style.explanation_tooltips = false;
    style
}

/// Both palettes as stored in the context.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Palettes {
    dark: Palette,
    light: Palette,
}

fn palettes_id() -> Id {
    Id::new("sprint-gui::theme::palettes")
}

/// Install the Sprint style for both themes and activate `theme`.
///
/// Safe to call at any time, including from inside a frame (for a theme
/// toggle). The semibold text roles use Sprint's strong font once
/// [`fonts::install`] ran and its fonts are active (from the start of the
/// next pass; see [`setup`]); until then they use the proportional font.
pub fn apply(ctx: &Context, theme: Theme) {
    apply_palettes(ctx, theme, Palette::dark(), Palette::light());
}

/// Like [`apply`] with custom palettes (e.g. a user-tweaked accent).
pub fn apply_palettes(ctx: &Context, theme: Theme, dark: Palette, light: Palette) {
    let strong = strong_family(ctx);
    ctx.set_style_of(egui::Theme::Dark, Arc::new(style(&dark, &strong)));
    ctx.set_style_of(egui::Theme::Light, Arc::new(style(&light, &strong)));
    ctx.data_mut(|d| {
        d.insert_temp(palettes_id(), Palettes { dark, light });
        d.insert_temp(strong_applied_id(), strong == fonts::strong());
    });
    ctx.set_theme(egui::Theme::from(theme));
    register_font_sync(ctx);
}

/// Install fonts ([`fonts::install`]) and then the style ([`apply`]).
///
/// Call it once, ideally before the first frame (from eframe's
/// `CreationContext`). Calling it inside a frame is safe too: new fonts
/// become active at the start of the next pass, and the semibold roles
/// switch over then.
pub fn setup(ctx: &Context, theme: Theme) -> fonts::FontReport {
    let report = fonts::install(ctx);
    apply(ctx, theme);
    report
}

/// The theme currently active in `ctx`.
pub fn current(ctx: &Context) -> Theme {
    ctx.theme().into()
}

/// The palette of the context's active theme.
pub fn palette(ctx: &Context) -> Palette {
    palette_for(ctx, current(ctx))
}

/// The installed palette of `theme` (the defaults if [`apply`] never ran).
pub fn palette_for(ctx: &Context, theme: Theme) -> Palette {
    let stored = ctx.data(|d| d.get_temp::<Palettes>(palettes_id()));
    match (stored, theme) {
        (Some(p), Theme::Dark) => p.dark,
        (Some(p), Theme::Light) => p.light,
        (None, theme) => theme.palette(),
    }
}

/// The palette matching `ui`'s visuals (dark or light). Widgets use this so
/// that a [`scope`] with another theme renders correctly.
pub fn ui_palette(ui: &Ui) -> Palette {
    palette_for(ui.ctx(), Theme::from_dark_mode(ui.visuals().dark_mode))
}

/// Run `add_contents` with `theme`'s style, regardless of the context's
/// active theme, on a background of that theme's panel color.
pub fn scope<R>(ui: &mut Ui, theme: Theme, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let ctx = ui.ctx().clone();
    let style = ctx.style_of(theme.into());
    let p = palette_for(&ctx, theme);
    ui.scope(|ui| {
        ui.set_style(style);
        egui::Frame::new()
            .fill(p.bg)
            .inner_margin(Margin::same(16))
            .show(ui, add_contents)
            .inner
    })
    .inner
}

// ---------------------------------------------------------------------------
// Color helpers

/// A color from `0xRRGGBB`.
pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// `color` with its opacity multiplied by `alpha` (0..=1, clamped; NaN = 0).
pub fn with_alpha(color: Color32, alpha: f32) -> Color32 {
    let alpha = if alpha.is_nan() {
        0.0
    } else {
        alpha.clamp(0.0, 1.0)
    };
    color.gamma_multiply(alpha)
}

/// Linear blend in sRGB space: `t = 0` gives `a`, `t = 1` gives `b`
/// (clamped; NaN = 0).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    let lerp = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}

/// WCAG relative luminance of an opaque color (0 = black, 1 = white).
pub fn luminance(color: Color32) -> f32 {
    let channel = |c: u8| {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// WCAG contrast ratio between two opaque colors (1..=21).
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Near-black or white, whichever contrasts more with `fill` (for text on
/// user-chosen colors such as macro buttons).
pub fn readable_text_on(fill: Color32) -> Color32 {
    let dark = hex(0x15181D);
    if contrast_ratio(fill, dark) >= contrast_ratio(fill, Color32::WHITE) {
        dark
    } else {
        Color32::WHITE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque_over(fg: Color32, bg: Color32) -> Color32 {
        // Composite a premultiplied color over an opaque background.
        let a = f32::from(fg.a()) / 255.0;
        let ch = |f: u8, b: u8| (f32::from(f) + f32::from(b) * (1.0 - a)).round() as u8;
        Color32::from_rgb(ch(fg.r(), bg.r()), ch(fg.g(), bg.g()), ch(fg.b(), bg.b()))
    }

    #[test]
    fn hex_parses_channels() {
        assert_eq!(hex(0xFF7A1A), Color32::from_rgb(0xFF, 0x7A, 0x1A));
        assert_eq!(hex(0x000000), Color32::BLACK);
    }

    #[test]
    fn text_contrast_meets_wcag() {
        for p in [Palette::dark(), Palette::light()] {
            for bg in [p.bg, p.panel, p.surface, p.raised] {
                assert!(contrast_ratio(p.text, bg) >= 7.0, "text on {bg:?}");
                assert!(contrast_ratio(p.text_dim, bg) >= 4.5, "dim on {bg:?}");
                assert!(contrast_ratio(p.text_faint, bg) >= 2.8, "faint on {bg:?}");
            }
            assert!(contrast_ratio(p.on_accent, p.accent) >= 3.0);
            assert!(contrast_ratio(p.on_danger, p.danger_fill) >= 4.0);
            assert!(contrast_ratio(p.on_danger, p.danger_hover) >= 3.5);
        }
    }

    #[test]
    fn status_colors_are_visible_on_surfaces() {
        for p in [Palette::dark(), Palette::light()] {
            for tone in Tone::ALL {
                let c = p.tone(tone);
                assert!(
                    contrast_ratio(c, p.surface) >= 3.0,
                    "{tone:?} on surface: {}",
                    contrast_ratio(c, p.surface)
                );
                // Text in a tone on its soft fill stays readable.
                let soft = opaque_over(p.tone_soft(tone), p.surface);
                assert!(contrast_ratio(c, soft) >= 2.6, "{tone:?} on soft fill");
            }
            for kind in [HeaterKind::Hotend, HeaterKind::Bed, HeaterKind::Chamber] {
                assert!(contrast_ratio(p.heater(kind), p.surface) >= 3.0);
            }
        }
    }

    #[test]
    fn surfaces_stack_in_order() {
        let d = Palette::dark();
        assert!(luminance(d.bg) < luminance(d.panel));
        assert!(luminance(d.panel) < luminance(d.surface));
        assert!(luminance(d.surface) < luminance(d.raised));
        assert!(luminance(d.raised) < luminance(d.raised_hover));
        let l = Palette::light();
        assert!(luminance(l.bg) < luminance(l.panel));
        assert!(luminance(l.panel) < luminance(l.surface));
    }

    #[test]
    fn mix_and_alpha_clamp() {
        let a = Color32::BLACK;
        let b = Color32::WHITE;
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 7.0), b);
        assert_eq!(mix(a, b, -1.0), a);
        assert_eq!(mix(a, b, f32::NAN), a);
        assert_eq!(with_alpha(b, f32::NAN), Color32::TRANSPARENT);
        assert_eq!(with_alpha(b, 2.0), b);
    }

    #[test]
    fn readable_text_picks_contrast() {
        assert_eq!(readable_text_on(Color32::WHITE), hex(0x15181D));
        assert_eq!(readable_text_on(Color32::BLACK), Color32::WHITE);
        assert_eq!(readable_text_on(hex(0x1F72D8)), Color32::WHITE);
    }

    #[test]
    fn plot_colors_wrap() {
        let p = Palette::dark();
        assert_eq!(p.plot_color(0), p.plot_color(8));
        assert_eq!(p.plot_color(3), p.plot[3]);
    }

    #[test]
    fn theme_round_trips() {
        for t in Theme::ALL {
            assert_eq!(Theme::from(egui::Theme::from(t)), t);
            assert_eq!(t.toggled().toggled(), t);
            assert_eq!(Theme::from_dark_mode(t.is_dark()), t);
        }
    }

    #[test]
    fn style_defines_every_role() {
        let style = style(&Palette::dark(), &FontFamily::Proportional);
        for role in TextRole::ALL {
            assert_eq!(font_in(&style, role).size, role.size());
        }
        assert_eq!(style.text_styles[&TextStyle::Body].size, 14.0);
        assert_eq!(style.text_styles[&TextStyle::Heading].size, 18.0);
        assert_eq!(style.text_styles[&TextStyle::Small].size, 12.0);
        assert_eq!(style.text_styles[&TextStyle::Monospace].size, 13.0);
        assert_eq!(style.text_styles[&TextStyle::Button].size, 14.0);
    }

    #[test]
    fn font_falls_back_without_style_entry() {
        let style = Style::default();
        assert_eq!(
            font_in(&style, TextRole::Display),
            FontId::proportional(28.0)
        );
    }

    #[test]
    fn apply_stores_palettes_and_theme() {
        let ctx = Context::default();
        apply(&ctx, Theme::Light);
        assert_eq!(current(&ctx), Theme::Light);
        assert_eq!(palette(&ctx), Palette::light());
        apply(&ctx, Theme::Dark);
        assert_eq!(palette(&ctx), Palette::dark());
        let mut custom = Palette::dark();
        custom.accent = Color32::RED;
        apply_palettes(&ctx, Theme::Dark, custom, Palette::light());
        assert_eq!(palette(&ctx).accent, Color32::RED);
        assert_eq!(palette_for(&ctx, Theme::Light), Palette::light());
        // Without fonts::install the strong roles use the proportional family.
        let style = ctx.style_of(egui::Theme::Dark);
        assert_eq!(
            font_in(&style, TextRole::Display).family,
            FontFamily::Proportional
        );
    }

    #[test]
    fn palette_defaults_without_apply() {
        let ctx = Context::default();
        assert_eq!(palette_for(&ctx, Theme::Dark), Palette::dark());
        assert_eq!(palette_for(&ctx, Theme::Light), Palette::light());
    }
}
