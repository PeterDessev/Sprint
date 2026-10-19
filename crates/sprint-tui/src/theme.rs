//! Colors and styles shared by every Sprint terminal widget.
//!
//! The palette matches the GUI: Sprint orange (`#FF7A1A`) as the accent on a
//! dark background. Terminals differ in what they can show, so a [`Theme`]
//! is built for a [`ColorSupport`] level:
//!
//! * [`ColorSupport::TrueColor`]: the exact 24-bit palette;
//! * [`ColorSupport::Ansi256`]: the palette mapped to the xterm 256-color cube;
//! * [`ColorSupport::Ansi16`]: hand-picked ANSI colors on the terminal's own
//!   background (16-color terminals have custom palettes, so nearest-color
//!   mapping would be unreliable);
//! * [`ColorSupport::None`]: no colors at all, only bold/dim/reverse
//!   attributes, as the [`NO_COLOR`](https://no-color.org) convention asks.
//!
//! [`Theme::detect`] picks the level from the environment.

use std::sync::LazyLock;

use ratatui::style::{Color, Modifier, Style};
use sprint_gcode::Feature;

use crate::widgets::console::ConsoleKind;
use crate::widgets::heater::{HeaterKind, HeaterState};

/// Sprint's accent color (sRGB); the GUI uses the same orange.
pub const ACCENT_RGB: [u8; 3] = [0xFF, 0x7A, 0x1A];

/// The theme widgets use when none is given: [`Theme::dark`].
pub fn default_theme() -> &'static Theme {
    static DEFAULT: LazyLock<Theme> = LazyLock::new(Theme::dark);
    &DEFAULT
}

/// RGB values of the dark palette.
mod rgb {
    pub const BACKGROUND: [u8; 3] = [0x14, 0x16, 0x1B];
    pub const SURFACE: [u8; 3] = [0x1E, 0x21, 0x28];
    pub const TEXT: [u8; 3] = [0xE6, 0xE8, 0xEC];
    pub const TEXT_DIM: [u8; 3] = [0x8A, 0x91, 0x9E];
    pub const BORDER: [u8; 3] = [0x3A, 0x3F, 0x4B];
    pub const SUCCESS: [u8; 3] = [0x3F, 0xB9, 0x50];
    pub const WARNING: [u8; 3] = [0xE3, 0xB3, 0x41];
    pub const ERROR: [u8; 3] = [0xF8, 0x51, 0x49];
    pub const INFO: [u8; 3] = [0x58, 0xA6, 0xFF];
    pub const SENT: [u8; 3] = [0x7E, 0xC8, 0xE3];
    pub const SEARCH: [u8; 3] = [0xFF, 0xD3, 0x3D];
    pub const TRAVEL: [u8; 3] = [0x4A, 0x50, 0x5C];
    pub const BED: [u8; 3] = [0x3D, 0xA5, 0xF4];
    pub const CHAMBER: [u8; 3] = [0xB4, 0x8E, 0xF7];
    pub const HOTENDS: [[u8; 3]; 4] = [
        [0xFF, 0x5A, 0x36],
        [0xF5, 0xC2, 0x3D],
        [0xE0, 0x6C, 0xD8],
        [0x5E, 0xD1, 0xA4],
    ];
}

/// How many colors the terminal can show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColorSupport {
    /// 24-bit RGB colors.
    #[default]
    TrueColor,
    /// The xterm 256-color palette.
    Ansi256,
    /// The 16 basic ANSI colors.
    Ansi16,
    /// No colors (`NO_COLOR`, `TERM=dumb`): attributes only.
    None,
}

impl ColorSupport {
    /// Detect the color support from the process environment.
    pub fn detect() -> ColorSupport {
        ColorSupport::from_env(|name| {
            std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
        })
    }

    /// Detect the color support from environment variables looked up with
    /// `var` (for tests and embedding):
    ///
    /// * `NO_COLOR` set to a non-empty value, or `TERM=dumb`: no colors;
    /// * `COLORTERM=truecolor|24bit`, Windows Terminal (`WT_SESSION`),
    ///   terminals known for 24-bit color (`TERM_PROGRAM`), a `TERM` naming
    ///   direct color, or Windows without `TERM` (the Windows 10+ console):
    ///   true color;
    /// * a `TERM` containing `256color`: 256 colors;
    /// * anything else: 16 colors.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> ColorSupport {
        if var("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorSupport::None;
        }
        let term = var("TERM").unwrap_or_default().to_ascii_lowercase();
        if term == "dumb" {
            return ColorSupport::None;
        }
        let colorterm = var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" {
            return ColorSupport::TrueColor;
        }
        if var("WT_SESSION").is_some_and(|v| !v.is_empty()) {
            return ColorSupport::TrueColor;
        }
        let program = var("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
        if ["iterm.app", "vscode", "wezterm", "ghostty"].contains(&program.as_str()) {
            return ColorSupport::TrueColor;
        }
        if term.contains("truecolor") || term.contains("24bit") || term.contains("direct") {
            return ColorSupport::TrueColor;
        }
        if term.contains("256color") {
            return ColorSupport::Ansi256;
        }
        if term.is_empty() && cfg!(windows) {
            return ColorSupport::TrueColor;
        }
        ColorSupport::Ansi16
    }

    /// True unless colors are disabled.
    pub fn has_color(self) -> bool {
        self != ColorSupport::None
    }

    /// The terminal color for `rgb`, using `ansi` on 16-color terminals.
    pub fn color(self, rgb: [u8; 3], ansi: Color) -> Color {
        let [r, g, b] = rgb;
        match self {
            ColorSupport::TrueColor => Color::Rgb(r, g, b),
            ColorSupport::Ansi256 => Color::Indexed(rgb_to_xterm256(rgb)),
            ColorSupport::Ansi16 => ansi,
            ColorSupport::None => Color::Reset,
        }
    }
}

/// Nearest entry of the xterm 256-color palette (6x6x6 cube or gray ramp).
pub fn rgb_to_xterm256(rgb: [u8; 3]) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_level = |v: u8| -> usize {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| l.abs_diff(v))
            .map_or(0, |(i, _)| i)
    };
    let dist = |a: [u8; 3], b: [u8; 3]| -> u32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| u32::from(x.abs_diff(y)).pow(2))
            .sum()
    };
    let idx = rgb.map(nearest_level);
    let cube = [LEVELS[idx[0]], LEVELS[idx[1]], LEVELS[idx[2]]];
    let cube_index = 16 + 36 * idx[0] + 6 * idx[1] + idx[2];
    let avg = (u32::from(rgb[0]) + u32::from(rgb[1]) + u32::from(rgb[2])) / 3;
    let step = (avg.saturating_sub(3) / 10).min(23);
    let gray_level = u8::try_from(8 + 10 * step).unwrap_or(238);
    let gray = [gray_level; 3];
    if dist(rgb, gray) < dist(rgb, cube) {
        u8::try_from(232 + step).unwrap_or(255)
    } else {
        u8::try_from(cube_index).unwrap_or(15)
    }
}

/// Linear blend of `from` towards `to` by `t` (0 = from, 1 = to).
fn blend(from: [u8; 3], to: [u8; 3], t: f32) -> [u8; 3] {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mix = |a: u8, b: u8| -> u8 {
        let v = f32::from(a) + (f32::from(b) - f32::from(a)) * t;
        // Clamped to 0..=255 first, so the cast cannot truncate.
        v.round().clamp(0.0, 255.0) as u8
    };
    [
        mix(from[0], to[0]),
        mix(from[1], to[1]),
        mix(from[2], to[2]),
    ]
}

/// ANSI fallback for a feature color on 16-color terminals.
fn feature_ansi(feature: Feature) -> Color {
    match feature {
        Feature::Unknown => Color::White,
        Feature::ExternalPerimeter => Color::LightRed,
        Feature::Perimeter => Color::Yellow,
        Feature::OverhangPerimeter => Color::Blue,
        Feature::InternalInfill => Color::Red,
        Feature::SolidInfill => Color::Magenta,
        Feature::TopSolidInfill => Color::LightMagenta,
        Feature::BridgeInfill => Color::LightBlue,
        Feature::GapFill => Color::White,
        Feature::Skirt => Color::Cyan,
        Feature::Support => Color::LightGreen,
        Feature::SupportInterface => Color::Green,
        Feature::Ironing => Color::LightYellow,
        Feature::WipeTower => Color::LightCyan,
        Feature::Custom => Color::Gray,
    }
}

/// Semantic colors and ready-made styles for the terminal widgets.
///
/// Fields are public so applications can tweak a theme; the style methods
/// take care of the monochrome case (they then use attributes instead of
/// colors).
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// The color level this theme was built for.
    pub support: ColorSupport,
    /// Screen background.
    pub background: Color,
    /// Background of raised panels (popups, overlays).
    pub surface: Color,
    /// Normal text.
    pub text: Color,
    /// Secondary text and not-yet-printed parts.
    pub text_dim: Color,
    /// Panel borders.
    pub border: Color,
    /// Sprint orange: focus, titles, the tool marker.
    pub accent: Color,
    /// Success / at target.
    pub success: Color,
    /// Warnings / heating.
    pub warning: Color,
    /// Errors.
    pub error: Color,
    /// Informational messages / cooling.
    pub info: Color,
    /// Console lines sent to the printer.
    pub sent: Color,
    /// Console lines received from the printer.
    pub received: Color,
    /// Background of search matches.
    pub search: Color,
    /// Travel moves in the layer view.
    pub travel: Color,
    /// Hotend colors, cycled by tool index.
    pub hotends: [Color; 4],
    /// Hotend target colors (dimmer), cycled by tool index.
    pub hotend_targets: [Color; 4],
    /// Bed color.
    pub bed: Color,
    /// Bed target color (dimmer).
    pub bed_target: Color,
    /// Chamber color.
    pub chamber: Color,
    /// Chamber target color (dimmer).
    pub chamber_target: Color,
    /// Feature colors, indexed by [`Feature::index`].
    pub features: [Color; Feature::COUNT],
    /// Ghosted feature colors (not yet printed, previous layer).
    pub features_dim: [Color; Feature::COUNT],
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark()
    }
}

impl Theme {
    /// The 24-bit dark theme.
    pub fn dark() -> Theme {
        Theme::for_support(ColorSupport::TrueColor)
    }

    /// The theme for 16-color terminals (terminal background, ANSI colors).
    pub fn ansi16() -> Theme {
        Theme::for_support(ColorSupport::Ansi16)
    }

    /// The colorless theme (`NO_COLOR`).
    pub fn monochrome() -> Theme {
        Theme::for_support(ColorSupport::None)
    }

    /// The theme for the color support detected from the environment.
    pub fn detect() -> Theme {
        Theme::for_support(ColorSupport::detect())
    }

    /// The theme for a given color support level.
    pub fn for_support(support: ColorSupport) -> Theme {
        let c = |rgb: [u8; 3], ansi: Color| support.color(rgb, ansi);
        // Dim colors are blends towards the background; 16-color terminals
        // get dark gray instead.
        let dim =
            |rgb: [u8; 3], t: f32| support.color(blend(rgb::BACKGROUND, rgb, t), Color::DarkGray);
        let hotend_ansi = [Color::LightRed, Color::Yellow, Color::Magenta, Color::Green];
        let hotend_target_ansi = [Color::Red, Color::Yellow, Color::Magenta, Color::Green];
        let mut hotends = [Color::Reset; 4];
        let mut hotend_targets = [Color::Reset; 4];
        for (i, rgb) in rgb::HOTENDS.iter().enumerate() {
            hotends[i] = c(*rgb, hotend_ansi[i]);
            hotend_targets[i] =
                support.color(blend(rgb::BACKGROUND, *rgb, 0.55), hotend_target_ansi[i]);
        }
        let mut features = [Color::Reset; Feature::COUNT];
        let mut features_dim = [Color::Reset; Feature::COUNT];
        for feature in Feature::ALL {
            let rgb = feature.default_color();
            features[feature.index()] = c(rgb, feature_ansi(feature));
            features_dim[feature.index()] = dim(rgb, 0.3);
        }
        // On 16-color terminals the terminal's own background is kept.
        let (background, surface) = match support {
            ColorSupport::Ansi16 | ColorSupport::None => (Color::Reset, Color::Reset),
            _ => (
                c(rgb::BACKGROUND, Color::Reset),
                c(rgb::SURFACE, Color::Reset),
            ),
        };
        Theme {
            support,
            background,
            surface,
            text: c(rgb::TEXT, Color::Reset),
            text_dim: c(rgb::TEXT_DIM, Color::DarkGray),
            border: c(rgb::BORDER, Color::DarkGray),
            accent: c(ACCENT_RGB, Color::LightRed),
            success: c(rgb::SUCCESS, Color::Green),
            warning: c(rgb::WARNING, Color::Yellow),
            error: c(rgb::ERROR, Color::Red),
            info: c(rgb::INFO, Color::LightBlue),
            sent: c(rgb::SENT, Color::Cyan),
            received: c(rgb::TEXT, Color::Reset),
            search: c(rgb::SEARCH, Color::Yellow),
            travel: c(rgb::TRAVEL, Color::DarkGray),
            hotends,
            hotend_targets,
            bed: c(rgb::BED, Color::LightBlue),
            bed_target: support.color(blend(rgb::BACKGROUND, rgb::BED, 0.55), Color::Blue),
            chamber: c(rgb::CHAMBER, Color::LightMagenta),
            chamber_target: support
                .color(blend(rgb::BACKGROUND, rgb::CHAMBER, 0.55), Color::Magenta),
            features,
            features_dim,
        }
    }

    /// True unless this is the monochrome theme.
    pub fn has_color(&self) -> bool {
        self.support.has_color()
    }

    /// A style with foreground `color`, or the plain style without colors.
    pub fn fg(&self, color: Color) -> Style {
        if self.has_color() {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    /// Normal text on the screen background.
    pub fn base(&self) -> Style {
        if self.has_color() {
            Style::default().fg(self.text).bg(self.background)
        } else {
            Style::default()
        }
    }

    /// Normal text, without a background (for spans inside panels).
    pub fn text(&self) -> Style {
        self.fg(self.text)
    }

    /// Raised panels (popups, overlays).
    pub fn surface(&self) -> Style {
        if self.has_color() {
            Style::default().fg(self.text).bg(self.surface)
        } else {
            Style::default()
        }
    }

    /// Secondary text.
    pub fn dim(&self) -> Style {
        if self.has_color() {
            Style::default().fg(self.text_dim)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        }
    }

    /// Accented text (titles, values that matter).
    pub fn accent(&self) -> Style {
        self.fg(self.accent).add_modifier(Modifier::BOLD)
    }

    /// Panel borders; `focused` panels use the accent.
    pub fn border(&self, focused: bool) -> Style {
        match (self.has_color(), focused) {
            (true, true) => Style::default().fg(self.accent),
            (true, false) => Style::default().fg(self.border),
            (false, true) => Style::default().add_modifier(Modifier::BOLD),
            (false, false) => Style::default(),
        }
    }

    /// Panel titles.
    pub fn title(&self) -> Style {
        self.accent()
    }

    /// A key in a hint bar or help screen.
    pub fn key(&self) -> Style {
        if self.has_color() {
            Style::default()
                .fg(self.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        }
    }

    /// Selected row of a list or popup.
    pub fn selected(&self) -> Style {
        if self.has_color() {
            Style::default()
                .fg(self.background_or_black())
                .bg(self.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    /// Search matches in the console.
    pub fn search_highlight(&self) -> Style {
        if self.has_color() {
            Style::default()
                .fg(self.background_or_black())
                .bg(self.search)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    /// Success messages.
    pub fn success(&self) -> Style {
        self.fg(self.success)
    }

    /// Warnings.
    pub fn warning(&self) -> Style {
        self.fg(self.warning).add_modifier(Modifier::BOLD)
    }

    /// Errors.
    pub fn error(&self) -> Style {
        self.fg(self.error).add_modifier(Modifier::BOLD)
    }

    /// Style of a console line of the given kind.
    pub fn console(&self, kind: ConsoleKind) -> Style {
        if !self.has_color() {
            return match kind {
                ConsoleKind::Sent | ConsoleKind::Received | ConsoleKind::Info => Style::default(),
                ConsoleKind::Warning | ConsoleKind::Error => {
                    Style::default().add_modifier(Modifier::BOLD)
                }
            };
        }
        match kind {
            ConsoleKind::Sent => Style::default().fg(self.sent),
            ConsoleKind::Received => Style::default().fg(self.received),
            ConsoleKind::Info => Style::default().fg(self.info),
            ConsoleKind::Warning => Style::default().fg(self.warning),
            ConsoleKind::Error => Style::default().fg(self.error).add_modifier(Modifier::BOLD),
        }
    }

    /// Series color of a heater's current temperature.
    pub fn heater(&self, heater: HeaterKind) -> Color {
        match heater {
            HeaterKind::Hotend(tool) => self.hotends[usize::from(tool) % self.hotends.len()],
            HeaterKind::Bed => self.bed,
            HeaterKind::Chamber => self.chamber,
        }
    }

    /// Series color of a heater's target temperature.
    pub fn heater_target(&self, heater: HeaterKind) -> Color {
        match heater {
            HeaterKind::Hotend(tool) => {
                self.hotend_targets[usize::from(tool) % self.hotend_targets.len()]
            }
            HeaterKind::Bed => self.bed_target,
            HeaterKind::Chamber => self.chamber_target,
        }
    }

    /// Style of a heater readout in the given state.
    pub fn heater_state(&self, state: HeaterState) -> Style {
        if !self.has_color() {
            return match state {
                HeaterState::Off | HeaterState::Unknown => {
                    Style::default().add_modifier(Modifier::DIM)
                }
                HeaterState::Heating | HeaterState::Cooling => {
                    Style::default().add_modifier(Modifier::BOLD)
                }
                HeaterState::AtTarget => Style::default(),
            };
        }
        match state {
            HeaterState::Off | HeaterState::Unknown => Style::default().fg(self.text_dim),
            HeaterState::Heating => Style::default()
                .fg(self.accent)
                .add_modifier(Modifier::BOLD),
            HeaterState::Cooling => Style::default().fg(self.info),
            HeaterState::AtTarget => Style::default()
                .fg(self.success)
                .add_modifier(Modifier::BOLD),
        }
    }

    /// Display color of a print feature.
    pub fn feature(&self, feature: Feature) -> Color {
        self.features
            .get(feature.index())
            .copied()
            .unwrap_or(self.text)
    }

    /// Ghosted color of a print feature (not yet printed, previous layer).
    pub fn feature_dim(&self, feature: Feature) -> Color {
        self.features_dim
            .get(feature.index())
            .copied()
            .unwrap_or(self.text_dim)
    }

    /// A dark color for text drawn on accent backgrounds.
    pub(crate) fn background_or_black(&self) -> Color {
        match self.background {
            Color::Reset => Color::Black,
            c => c,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn no_color_wins_over_everything() {
        let support = ColorSupport::from_env(env(&[
            ("NO_COLOR", "1"),
            ("COLORTERM", "truecolor"),
            ("TERM", "xterm-256color"),
        ]));
        assert_eq!(support, ColorSupport::None);
    }

    #[test]
    fn empty_no_color_is_ignored() {
        let support = ColorSupport::from_env(env(&[("NO_COLOR", ""), ("COLORTERM", "24bit")]));
        assert_eq!(support, ColorSupport::TrueColor);
    }

    #[test]
    fn detects_levels_from_term() {
        assert_eq!(
            ColorSupport::from_env(env(&[("TERM", "dumb")])),
            ColorSupport::None
        );
        assert_eq!(
            ColorSupport::from_env(env(&[("TERM", "xterm-256color")])),
            ColorSupport::Ansi256
        );
        assert_eq!(
            ColorSupport::from_env(env(&[("TERM", "linux")])),
            ColorSupport::Ansi16
        );
        assert_eq!(
            ColorSupport::from_env(env(&[("TERM", "xterm-direct")])),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env(env(&[("TERM", "xterm"), ("WT_SESSION", "abc")])),
            ColorSupport::TrueColor
        );
        assert_eq!(
            ColorSupport::from_env(env(&[("TERM", "xterm"), ("TERM_PROGRAM", "WezTerm")])),
            ColorSupport::TrueColor
        );
        let bare = ColorSupport::from_env(env(&[]));
        if cfg!(windows) {
            assert_eq!(bare, ColorSupport::TrueColor);
        } else {
            assert_eq!(bare, ColorSupport::Ansi16);
        }
    }

    #[test]
    fn accent_is_sprint_orange() {
        assert_eq!(Theme::dark().accent, Color::Rgb(0xFF, 0x7A, 0x1A));
        assert_eq!(Theme::ansi16().accent, Color::LightRed);
        assert_eq!(Theme::dark().background, Color::Rgb(0x14, 0x16, 0x1B));
        assert_eq!(Theme::ansi16().background, Color::Reset);
    }

    #[test]
    fn monochrome_uses_no_colors() {
        let theme = Theme::monochrome();
        assert!(!theme.has_color());
        let styles = [
            theme.base(),
            theme.dim(),
            theme.accent(),
            theme.border(true),
            theme.key(),
            theme.selected(),
            theme.search_highlight(),
            theme.error(),
            theme.console(ConsoleKind::Sent),
            theme.console(ConsoleKind::Error),
            theme.heater_state(HeaterState::Heating),
            theme.fg(theme.accent),
        ];
        for style in styles {
            assert_eq!(style.fg, None, "{style:?}");
            assert_eq!(style.bg, None, "{style:?}");
        }
        assert_ne!(theme.search_highlight(), Style::default());
        assert_ne!(theme.selected(), Style::default());
    }

    #[test]
    fn xterm256_mapping() {
        assert_eq!(rgb_to_xterm256([0, 0, 0]), 16);
        assert_eq!(rgb_to_xterm256([255, 255, 255]), 231);
        assert_eq!(rgb_to_xterm256([255, 0, 0]), 196);
        // A mid gray lands on the gray ramp.
        let gray = rgb_to_xterm256([128, 128, 128]);
        assert!((232..=255).contains(&gray), "{gray}");
        // Accent orange: red 5, green 2 (135), blue 0.
        assert_eq!(rgb_to_xterm256(ACCENT_RGB), 16 + 36 * 5 + 6 * 2);
    }

    #[test]
    fn ansi256_theme_uses_indexed_colors() {
        let theme = Theme::for_support(ColorSupport::Ansi256);
        assert!(matches!(theme.accent, Color::Indexed(_)));
        assert!(matches!(
            theme.feature(Feature::Perimeter),
            Color::Indexed(_)
        ));
    }

    #[test]
    fn dim_features_differ_from_bright() {
        let theme = Theme::dark();
        for f in Feature::ALL {
            assert_ne!(theme.feature(f), theme.feature_dim(f), "{f:?}");
        }
        assert_eq!(
            theme.feature(Feature::ExternalPerimeter),
            Color::Rgb(0xFF, 0x7D, 0x38)
        );
    }

    #[test]
    fn heater_colors_cycle_by_tool() {
        let theme = Theme::dark();
        assert_eq!(
            theme.heater(HeaterKind::Hotend(0)),
            theme.heater(HeaterKind::Hotend(4))
        );
        assert_ne!(
            theme.heater(HeaterKind::Hotend(0)),
            theme.heater(HeaterKind::Bed)
        );
        assert_ne!(
            theme.heater(HeaterKind::Bed),
            theme.heater_target(HeaterKind::Bed)
        );
        assert_eq!(
            theme.heater(HeaterKind::Hotend(255)),
            theme.hotends[255 % 4]
        );
    }

    #[test]
    fn blend_handles_extremes() {
        assert_eq!(blend([0, 0, 0], [255, 255, 255], 0.0), [0, 0, 0]);
        assert_eq!(blend([0, 0, 0], [255, 255, 255], 1.0), [255, 255, 255]);
        assert_eq!(blend([0, 0, 0], [255, 255, 255], f32::NAN), [0, 0, 0]);
        assert_eq!(blend([0, 0, 0], [200, 100, 50], 5.0), [200, 100, 50]);
    }
}
