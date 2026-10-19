//! Typography: system UI fonts plus the Phosphor icon font.
//!
//! [`install`] looks for the platform's UI fonts at runtime (no fonts are
//! bundled besides egui's defaults and Phosphor):
//!
//! | Family | Windows | Fallback |
//! |---|---|---|
//! | proportional | Segoe UI Variable Text, Segoe UI | egui default |
//! | [`strong`] (semibold) | Segoe UI Variable (wght 600), Segoe UI Semibold, Segoe UI Bold | the proportional font |
//! | monospace | Cascadia Mono, Consolas | egui default (Hack) |
//!
//! On Linux a few common locations of Noto Sans / DejaVu are tried too.
//! Every candidate file is validated before it reaches egui (a corrupt
//! font file would otherwise make egui panic), and anything that fails
//! silently falls back to the next candidate.
//!
//! The Phosphor icon font (regular variant) is merged into every family, so
//! icons from [`crate::icons`] work inline in any text:
//! `ui.label(format!("{} Home", icons::HOME))`.

#![warn(missing_docs)]

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::epaint::text::VariationCoords;
use egui::{Context, FontData, FontDefinitions, FontFamily, FontTweak, Id};

/// Name of the semibold font family registered by [`install`].
pub const STRONG_FAMILY: &str = "sprint-strong";
/// Name under which the Phosphor icon font is registered (also a family of
/// its own, as `FontFamily::Name("phosphor")`).
pub const ICON_FONT: &str = "phosphor";

/// Font files larger than this are ignored (no UI font is anywhere near).
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

/// The semibold family (`FontFamily::Name("sprint-strong")`).
///
/// Only valid after [`install`]; use [`crate::theme::strong_family`] or the
/// [`crate::theme::TextRole`] fonts in widgets, which fall back safely.
pub fn strong() -> FontFamily {
    FontFamily::Name(STRONG_FAMILY.into())
}

/// The Phosphor-only family (`FontFamily::Name("phosphor")`), for drawing an
/// icon glyph without any text font in front of it.
pub fn icon_family() -> FontFamily {
    FontFamily::Name(ICON_FONT.into())
}

/// What [`install`] found. `None` means egui's default font is used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontReport {
    /// The proportional (body) font, e.g. `"Segoe UI"`.
    pub proportional: Option<String>,
    /// The semibold font.
    pub strong: Option<String>,
    /// The monospace font, e.g. `"Cascadia Mono"`.
    pub monospace: Option<String>,
}

impl FontReport {
    /// True when no system font was found (egui defaults everywhere).
    pub fn is_default(&self) -> bool {
        self.proportional.is_none() && self.strong.is_none() && self.monospace.is_none()
    }
}

/// One font file candidate.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    /// Display name.
    name: &'static str,
    /// File name inside a font directory, or an absolute path.
    file: &'static str,
    /// `wght` axis value for variable fonts.
    weight: Option<f32>,
}

const fn candidate(name: &'static str, file: &'static str, weight: Option<f32>) -> Candidate {
    Candidate { name, file, weight }
}

const PROPORTIONAL: &[Candidate] = &[
    candidate("Segoe UI Variable Text", "SegUIVar.ttf", Some(400.0)),
    candidate("Segoe UI", "segoeui.ttf", None),
    candidate("Noto Sans", "NotoSans-Regular.ttf", None),
    candidate("DejaVu Sans", "DejaVuSans.ttf", None),
];

const STRONG: &[Candidate] = &[
    candidate(
        "Segoe UI Variable Text Semibold",
        "SegUIVar.ttf",
        Some(600.0),
    ),
    candidate("Segoe UI Semibold", "seguisb.ttf", None),
    candidate("Segoe UI Bold", "segoeuib.ttf", None),
    candidate("Noto Sans SemiBold", "NotoSans-SemiBold.ttf", None),
    candidate("DejaVu Sans Bold", "DejaVuSans-Bold.ttf", None),
];

const MONOSPACE: &[Candidate] = &[
    candidate("Cascadia Mono", "CascadiaMono.ttf", None),
    candidate("Consolas", "consola.ttf", None),
    candidate("Noto Sans Mono", "NotoSansMono-Regular.ttf", None),
    candidate("DejaVu Sans Mono", "DejaVuSansMono.ttf", None),
];

/// Directories searched for font files, in order.
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR")
            .or_else(|| std::env::var_os("SystemRoot"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        dirs.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
        }
    } else {
        for dir in [
            "/usr/share/fonts/truetype/noto",
            "/usr/share/fonts/noto",
            "/usr/share/fonts/google-noto",
            "/usr/share/fonts/truetype/dejavu",
            "/usr/share/fonts/dejavu",
            "/usr/share/fonts/TTF",
        ] {
            dirs.push(PathBuf::from(dir));
        }
    }
    dirs
}

/// Validate font bytes the same way egui will parse them, so a broken file
/// can never reach egui's font loader (which panics on parse errors).
/// Also requires basic Latin coverage.
pub fn is_valid_font(bytes: &[u8]) -> bool {
    use skrifa::MetadataProvider as _;
    let Ok(font) = skrifa::FontRef::from_index(bytes, 0) else {
        return false;
    };
    let charmap = font.charmap();
    ['A', 'a', '0', '.']
        .iter()
        .all(|&c| charmap.map(c).is_some())
}

fn read_font(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FONT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    is_valid_font(&bytes).then_some(bytes)
}

/// A loaded font: display name and egui data.
struct Loaded {
    name: &'static str,
    data: FontData,
}

/// Load the first candidate that exists and validates. Reads each file at
/// most once (`cache` keeps bytes shared between families).
fn load_first(
    candidates: &[Candidate],
    dirs: &[PathBuf],
    cache: &mut Vec<(PathBuf, Option<Arc<[u8]>>)>,
) -> Option<Loaded> {
    for c in candidates {
        for dir in dirs {
            let path = dir.join(c.file);
            let bytes = match cache.iter().find(|(p, _)| *p == path) {
                Some((_, bytes)) => bytes.clone(),
                None => {
                    let bytes: Option<Arc<[u8]>> = read_font(&path).map(Arc::from);
                    cache.push((path, bytes.clone()));
                    bytes
                }
            };
            if let Some(bytes) = bytes {
                return Some(Loaded {
                    name: c.name,
                    data: font_data(&bytes, c.weight),
                });
            }
        }
    }
    None
}

fn font_data(bytes: &Arc<[u8]>, weight: Option<f32>) -> FontData {
    let mut tweak = FontTweak::default();
    if let Some(weight) = weight {
        tweak.coords = VariationCoords::new([(b"wght", weight)]);
    }
    FontData {
        font: Cow::Owned(bytes.to_vec()),
        index: 0,
        tweak,
    }
}

/// Font definitions built from explicit font bytes (for tests, or an app
/// that bundles its own fonts). Every argument is optional; invalid bytes
/// are ignored. Returns the definitions and the names that were used.
pub fn definitions_from(
    proportional: Option<(&str, Vec<u8>)>,
    strong: Option<(&str, Vec<u8>)>,
    monospace: Option<(&str, Vec<u8>)>,
) -> (FontDefinitions, FontReport) {
    let load = |font: Option<(&str, Vec<u8>)>| {
        font.and_then(|(name, bytes)| {
            is_valid_font(&bytes).then(|| (name.to_owned(), FontData::from_owned(bytes)))
        })
    };
    build(load(proportional), load(strong), load(monospace))
}

/// Font definitions with the system fonts found on this machine (see the
/// module docs), Phosphor icons and egui's defaults as fallbacks.
pub fn system_definitions() -> (FontDefinitions, FontReport) {
    let dirs = font_dirs();
    let mut cache = Vec::new();
    let named = |l: Option<Loaded>| l.map(|l| (l.name.to_owned(), l.data));
    let proportional = named(load_first(PROPORTIONAL, &dirs, &mut cache));
    let strong = named(load_first(STRONG, &dirs, &mut cache));
    let monospace = named(load_first(MONOSPACE, &dirs, &mut cache));
    build(proportional, strong, monospace)
}

/// Assemble the families. Font keys are internal (`sprint-*`), so a
/// display name can never collide with egui's own entries.
fn build(
    proportional: Option<(String, FontData)>,
    semibold: Option<(String, FontData)>,
    monospace: Option<(String, FontData)>,
) -> (FontDefinitions, FontReport) {
    let mut defs = FontDefinitions::default();
    let mut report = FontReport::default();
    let egui_proportional = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let egui_monospace = defs
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    let mut proportional_keys = Vec::new();
    if let Some((name, data)) = proportional {
        defs.font_data
            .insert("sprint-ui".to_owned(), Arc::new(data));
        proportional_keys.push("sprint-ui".to_owned());
        report.proportional = Some(name);
    }
    let mut strong_keys = Vec::new();
    if let Some((name, data)) = semibold {
        defs.font_data
            .insert("sprint-ui-strong".to_owned(), Arc::new(data));
        strong_keys.push("sprint-ui-strong".to_owned());
        report.strong = Some(name);
    }
    let mut monospace_keys = Vec::new();
    if let Some((name, data)) = monospace {
        defs.font_data
            .insert("sprint-mono".to_owned(), Arc::new(data));
        monospace_keys.push("sprint-mono".to_owned());
        report.monospace = Some(name);
    }

    defs.font_data.insert(
        ICON_FONT.to_owned(),
        Arc::new(FontData::from_static(
            egui_phosphor::Variant::Regular.font_bytes(),
        )),
    );
    let icon = ICON_FONT.to_owned();

    // Proportional: UI font, icons, then egui's fonts (wide glyph coverage).
    let mut prop = proportional_keys.clone();
    prop.push(icon.clone());
    prop.extend(egui_proportional.iter().cloned());

    // Strong: semibold, else the regular UI font, then the same fallbacks.
    let mut strong_family = if strong_keys.is_empty() {
        proportional_keys.clone()
    } else {
        strong_keys
    };
    strong_family.push(icon.clone());
    strong_family.extend(egui_proportional.iter().cloned());

    // Monospace: code font, icons, egui's monospace chain.
    let mut mono = monospace_keys;
    mono.push(icon.clone());
    mono.extend(egui_monospace.iter().cloned());

    // Icons alone (plus text fallbacks so stray text still renders).
    let mut icons_only = vec![icon];
    icons_only.extend(egui_proportional.iter().cloned());

    defs.families.insert(FontFamily::Proportional, dedup(prop));
    defs.families.insert(strong(), dedup(strong_family));
    defs.families.insert(FontFamily::Monospace, dedup(mono));
    defs.families.insert(icon_family(), dedup(icons_only));
    (defs, report)
}

fn dedup(keys: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(keys.len());
    for k in keys {
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

fn installed_id() -> Id {
    Id::new("sprint-gui::fonts::installed")
}

/// Install `defs` (from [`system_definitions`] or [`definitions_from`]) and
/// remember that Sprint's families exist.
pub fn install_definitions(ctx: &Context, defs: FontDefinitions, report: &FontReport) {
    ctx.set_fonts(defs);
    let report = report.clone();
    ctx.data_mut(|d| d.insert_temp(installed_id(), report));
}

/// Find the system fonts, merge Phosphor icons and install everything into
/// `ctx`. Fonts become active at the start of the next frame, so call this
/// before the first frame or between frames (see [`crate::theme::setup`]).
/// Never panics; missing fonts fall back to egui's defaults.
pub fn install(ctx: &Context) -> FontReport {
    let (defs, report) = system_definitions();
    if report.is_default() {
        log::info!("sprint-gui: no system UI fonts found, using egui defaults");
    } else {
        log::debug!("sprint-gui: fonts {report:?}");
    }
    install_definitions(ctx, defs, &report);
    report
}

/// Whether the fonts egui uses *right now* define `family`.
///
/// New definitions only become active at the start of the next pass, and
/// an app may replace them, so code that names one of Sprint's families
/// checks this first (egui panics on a family it does not know). Only call
/// it during a pass, or after the context ran at least one: before that
/// egui has no fonts at all.
pub(crate) fn family_is_active(ctx: &Context, family: &FontFamily) -> bool {
    ctx.fonts(|f| f.definitions().families.contains_key(family))
}

/// Whether [`install`] (or [`install_definitions`]) ran on this context.
pub fn is_installed(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<FontReport>(installed_id()).is_some())
}

/// The report of the last [`install`] on this context.
pub fn report(ctx: &Context) -> Option<FontReport> {
    ctx.data(|d| d.get_temp::<FontReport>(installed_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_garbage_bytes() {
        assert!(!is_valid_font(&[]));
        assert!(!is_valid_font(b"not a font at all"));
        assert!(!is_valid_font(&[0u8; 4096]));
        // A TrueType header with a bogus table directory.
        let mut bogus = vec![0x00, 0x01, 0x00, 0x00, 0x00, 0xFF];
        bogus.extend_from_slice(&[0xAB; 64]);
        assert!(!is_valid_font(&bogus));
    }

    #[test]
    fn accepts_a_real_font() {
        // Phosphor has no Latin letters, so it does not count as a UI font.
        assert!(!is_valid_font(egui_phosphor::Variant::Regular.font_bytes()));
        let defs = FontDefinitions::default();
        let ubuntu = defs
            .font_data
            .get("Ubuntu-Light")
            .expect("egui default font");
        assert!(is_valid_font(&ubuntu.font));
    }

    #[test]
    fn invalid_bytes_fall_back_to_defaults() {
        let (defs, report) = definitions_from(
            Some(("Broken", vec![1, 2, 3])),
            Some(("Broken", b"xx".to_vec())),
            None,
        );
        assert!(report.is_default());
        // Every family is bound to existing font data, so egui can never
        // panic on a missing family or font name.
        for family in [
            FontFamily::Proportional,
            FontFamily::Monospace,
            strong(),
            icon_family(),
        ] {
            let keys = defs.families.get(&family).expect("family bound");
            assert!(!keys.is_empty(), "{family:?} is empty");
            for k in keys {
                assert!(defs.font_data.contains_key(k), "missing {k}");
            }
        }
    }

    #[test]
    fn valid_bytes_take_priority() {
        let defs = FontDefinitions::default();
        let hack = defs.font_data["Hack"].font.to_vec();
        let (defs, report) = definitions_from(
            Some(("Hack as UI", hack.clone())),
            None,
            Some(("Hack", hack)),
        );
        assert_eq!(report.proportional.as_deref(), Some("Hack as UI"));
        assert_eq!(report.monospace.as_deref(), Some("Hack"));
        assert_eq!(report.strong, None);
        assert_eq!(defs.families[&FontFamily::Proportional][0], "sprint-ui");
        assert_eq!(defs.families[&FontFamily::Proportional][1], ICON_FONT);
        // The strong family reuses the regular UI font when no semibold exists.
        assert_eq!(defs.families[&strong()][0], "sprint-ui");
        assert_eq!(defs.families[&FontFamily::Monospace][0], "sprint-mono");
    }

    #[test]
    fn system_definitions_never_panic_and_are_consistent() {
        let (defs, _report) = system_definitions();
        for keys in defs.families.values() {
            for k in keys {
                assert!(defs.font_data.contains_key(k));
            }
        }
    }

    #[test]
    fn missing_files_are_skipped() {
        let dirs = vec![PathBuf::from("Z:/definitely/not/a/font/dir")];
        let mut cache = Vec::new();
        assert!(load_first(PROPORTIONAL, &dirs, &mut cache).is_none());
        assert!(read_font(Path::new("Z:/nope.ttf")).is_none());
    }

    #[test]
    fn install_marks_context_and_renders() {
        let ctx = Context::default();
        assert!(!is_installed(&ctx));
        let report = install(&ctx);
        assert!(is_installed(&ctx));
        assert_eq!(super::report(&ctx), Some(report));
        crate::theme::apply(&ctx, crate::theme::Theme::Dark);
        // Run frames that lay out text in every family, icons included.
        for _ in 0..2 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.label(format!("{} Home", crate::icons::HOME));
                ui.label(
                    egui::RichText::new("215.3 °C")
                        .font(crate::theme::font(ui, crate::theme::TextRole::Display)),
                );
                ui.monospace("G28 X Y");
                ui.label(egui::RichText::new(crate::icons::FAN).family(icon_family()));
            });
            // No renderer here: drop the font atlas uploads.
            output.textures_delta.clear();
        }
    }
}
