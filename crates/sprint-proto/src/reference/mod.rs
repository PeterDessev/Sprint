//! G-code reference: names, usage and short descriptions of the commands
//! common to Marlin, Prusa and RepRapFirmware, for tooltips, autocomplete
//! and confirmation of dangerous commands.

mod data_g;
mod data_m_high;
mod data_m_low;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::settings::normalize_code;

/// Reference entry for one command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcodeDoc {
    /// Canonical code, e.g. "G1", "M104", "G38.2", "T0".
    pub code: &'static str,
    /// Short name, e.g. "Linear move".
    pub name: &'static str,
    /// Usage, e.g. `"G1 [X<pos>] [Y<pos>] [Z<pos>] [E<pos>] [F<mm/min>]"`.
    pub usage: &'static str,
    /// One or two sentences on what the command does.
    pub description: &'static str,
    /// Grouping.
    pub category: GcodeCategory,
    /// Wipes settings, stops/kills the machine, flashes firmware, deletes
    /// data or drives raw pins: ask before sending.
    pub dangerous: bool,
}

/// Grouping of reference entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GcodeCategory {
    /// Moves, positioning modes, homing.
    Motion,
    /// Heaters and temperature reports.
    Temperature,
    /// Extrusion, filament, tools.
    Extruder,
    /// Fans.
    Fan,
    /// SD card / media.
    SdCard,
    /// Configuration and EEPROM.
    Settings,
    /// Probing, autotune and calibration routines.
    Calibration,
    /// Bed leveling.
    Leveling,
    /// Reports and diagnostics.
    Info,
    /// Flow of execution, power, stops.
    Control,
    /// Display and messages.
    Display,
    /// Everything else (spindle, LEDs, servos, I2C...).
    Other,
}

impl GcodeCategory {
    /// Display name.
    pub fn name(&self) -> &'static str {
        match self {
            GcodeCategory::Motion => "Motion",
            GcodeCategory::Temperature => "Temperature",
            GcodeCategory::Extruder => "Extruder",
            GcodeCategory::Fan => "Fan",
            GcodeCategory::SdCard => "SD card",
            GcodeCategory::Settings => "Settings",
            GcodeCategory::Calibration => "Calibration",
            GcodeCategory::Leveling => "Leveling",
            GcodeCategory::Info => "Info",
            GcodeCategory::Control => "Control",
            GcodeCategory::Display => "Display",
            GcodeCategory::Other => "Other",
        }
    }
}

/// Entry constructor used by the data tables.
const fn doc(
    code: &'static str,
    name: &'static str,
    usage: &'static str,
    description: &'static str,
    category: GcodeCategory,
) -> GcodeDoc {
    GcodeDoc {
        code,
        name,
        usage,
        description,
        category,
        dangerous: false,
    }
}

/// Like [`doc`] for commands that need confirmation.
const fn danger(
    code: &'static str,
    name: &'static str,
    usage: &'static str,
    description: &'static str,
    category: GcodeCategory,
) -> GcodeDoc {
    GcodeDoc {
        dangerous: true,
        ..doc(code, name, usage, description, category)
    }
}

/// Sort key: letter (G, M, T, others), number, sub-code.
fn sort_key(code: &str) -> (u8, u32, u32) {
    let mut chars = code.chars();
    let letter = match chars.next() {
        Some('G') => 0,
        Some('M') => 1,
        Some('T') => 2,
        _ => 3,
    };
    let rest = chars.as_str();
    let (num, sub) = rest.split_once('.').unwrap_or((rest, ""));
    (
        letter,
        num.parse().unwrap_or(u32::MAX),
        sub.parse().unwrap_or(0),
    )
}

/// Every reference entry, ordered G, M, T by number (~200 entries).
pub fn gcode_docs() -> &'static [GcodeDoc] {
    static ALL: OnceLock<Vec<GcodeDoc>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut all = [data_g::DOCS, data_m_low::DOCS, data_m_high::DOCS].concat();
        all.sort_by_key(|d| sort_key(d.code));
        all
    })
}

/// Look up a command, case-insensitively and ignoring leading zeros
/// (`"g1"`, `"G1"` and `"G01"` all find G1). Parameters after the code are
/// ignored, so `"M104 S200"` finds M104.
pub fn gcode_doc(code: &str) -> Option<&'static GcodeDoc> {
    let first = code.split_whitespace().next()?;
    let normalized = normalize_code(first)?;
    gcode_docs().iter().find(|d| d.code == normalized)
}

/// True when the query looks like (the start of) a code: a letter followed
/// by digits and dots only.
fn is_code_query(q: &str) -> bool {
    let mut chars = q.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_digit() || c == '.')
}

/// Lowercase words of a text.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// Search for autocomplete, ordered by relevance:
///
/// 1. the exact code (`"g01"` -> G1),
/// 2. codes starting with the query, in numeric order (`"M1"` -> M1, M17,
///    M18, M100, M104, M105, ...),
/// 3. for word queries (`"temp"`, `"bed level"`), entries whose name and
///    then description contain words starting with every query word.
///
/// An empty query returns nothing.
pub fn search_gcode(prefix: &str) -> Vec<&'static GcodeDoc> {
    let query = prefix.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let docs = gcode_docs();
    let mut out: Vec<&'static GcodeDoc> = Vec::new();

    if is_code_query(query) {
        let upper = query.to_ascii_uppercase();
        // "G01" -> "G1"; a bare letter or "G38." stays as typed.
        let normalized = normalize_code(&upper).unwrap_or_else(|| {
            let (letter, rest) = upper.split_at(1);
            let trimmed = rest.trim_start_matches('0');
            if trimmed.is_empty() || trimmed.starts_with('.') {
                upper.clone()
            } else {
                format!("{letter}{trimmed}")
            }
        });
        if let Some(exact) = docs.iter().find(|d| d.code == normalized) {
            out.push(exact);
        }
        // A zero-padded number ("G01") is a complete code, not a prefix.
        let zero_padded =
            query.len() > 2 && query.as_bytes()[1] == b'0' && query.as_bytes()[2].is_ascii_digit();
        if zero_padded {
            return out;
        }
        out.extend(
            docs.iter()
                .filter(|d| d.code != normalized && d.code.starts_with(&normalized)),
        );
        return out;
    }

    let terms: Vec<String> = words(query).collect();
    if terms.is_empty() {
        return out;
    }
    let matches = |text: &str| {
        let ws: Vec<String> = words(text).collect();
        terms
            .iter()
            .all(|t| ws.iter().any(|w| w.starts_with(t.as_str())))
    };
    out.extend(docs.iter().filter(|d| matches(d.name)));
    let in_desc: Vec<&GcodeDoc> = docs
        .iter()
        .filter(|d| !matches(d.name) && matches(&format!("{} {}", d.name, d.description)))
        .collect();
    out.extend(in_desc);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn entries_are_unique_and_complete() {
        let docs = gcode_docs();
        assert!(docs.len() >= 150, "only {} entries", docs.len());
        let mut seen = HashSet::new();
        for d in docs {
            assert!(seen.insert(d.code), "duplicate {}", d.code);
            assert_eq!(
                normalize_code(d.code).as_deref(),
                Some(d.code),
                "{}",
                d.code
            );
            assert!(
                !d.name.is_empty() && !d.description.is_empty(),
                "{}",
                d.code
            );
            let usage_code = d.usage.split_whitespace().next().unwrap_or("");
            assert_eq!(
                usage_code, d.code,
                "usage of {} must start with the code",
                d.code
            );
            assert!(d.description.ends_with('.'), "{} description", d.code);
        }
        let keys: Vec<_> = docs.iter().map(|d| sort_key(d.code)).collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "not sorted");
    }

    #[test]
    fn required_codes_present() {
        let mut required: Vec<String> = Vec::new();
        for g in [
            "G0", "G1", "G2", "G3", "G4", "G10", "G11", "G12", "G17", "G18", "G19", "G20", "G21",
            "G26", "G27", "G28", "G29", "G30", "G31", "G32", "G33", "G34", "G35", "G38.2", "G42",
            "G53", "G54", "G55", "G56", "G57", "G58", "G59", "G60", "G61", "G76", "G90", "G91",
            "G92",
        ] {
            required.push(g.into());
        }
        let m_ranges: [(u32, u32); 40] = [
            (0, 1),
            (3, 5),
            (17, 18),
            (84, 84),
            (20, 34),
            (42, 42),
            (48, 48),
            (73, 73),
            (75, 78),
            (80, 83),
            (85, 85),
            (92, 92),
            (100, 100),
            (104, 109),
            (110, 115),
            (117, 123),
            (125, 125),
            (140, 141),
            (145, 145),
            (149, 150),
            (155, 155),
            (163, 166),
            (190, 193),
            (200, 201),
            (203, 209),
            (211, 211),
            (217, 218),
            (220, 221),
            (226, 226),
            (240, 240),
            (250, 250),
            (255, 256),
            (260, 261),
            (280, 281),
            (290, 290),
            (300, 304),
            (350, 351),
            (355, 355),
            (380, 381),
            (400, 407),
        ];
        let more: [(u32, u32); 30] = [
            (410, 410),
            (412, 413),
            (420, 423),
            (425, 425),
            (428, 428),
            (430, 430),
            (486, 486),
            (500, 504),
            (510, 512),
            (524, 524),
            (540, 540),
            (569, 569),
            (575, 575),
            (592, 593),
            (600, 600),
            (603, 603),
            (605, 605),
            (665, 666),
            (672, 672),
            (701, 702),
            (710, 710),
            (808, 808),
            (851, 852),
            (860, 869),
            (871, 871),
            (876, 876),
            (900, 900),
            (906, 919),
            (928, 928),
            (951, 951),
        ];
        for (a, b) in m_ranges
            .iter()
            .chain(more.iter())
            .chain([(993, 995), (997, 997), (999, 999)].iter())
        {
            for n in *a..=*b {
                required.push(format!("M{n}"));
            }
        }
        for t in 0..8 {
            required.push(format!("T{t}"));
        }
        for code in &required {
            assert!(gcode_doc(code).is_some(), "missing {code}");
        }
    }

    #[test]
    fn dangerous_flags() {
        for code in ["M112", "M502", "M997", "M999", "M81", "M994"] {
            assert!(gcode_doc(code).unwrap().dangerous, "{code}");
        }
        for code in ["G1", "M104", "M105", "M500", "M503", "M108", "M410", "M876"] {
            assert!(!gcode_doc(code).unwrap().dangerous, "{code}");
        }
    }

    #[test]
    fn lookup_normalizes() {
        assert_eq!(gcode_doc("g1").unwrap().code, "G1");
        assert_eq!(gcode_doc("G01").unwrap().code, "G1");
        assert_eq!(gcode_doc("G00").unwrap().code, "G0");
        assert_eq!(gcode_doc(" m104 S200").unwrap().code, "M104");
        assert_eq!(gcode_doc("g38.2").unwrap().code, "G38.2");
        assert_eq!(gcode_doc("t3").unwrap().code, "T3");
        assert!(gcode_doc("").is_none());
        assert!(gcode_doc("M9999").is_none());
        assert!(gcode_doc("hello").is_none());
    }

    #[test]
    fn search_orders_by_relevance() {
        let codes = |q: &str| search_gcode(q).iter().map(|d| d.code).collect::<Vec<_>>();
        let m1 = codes("M1");
        assert_eq!(&m1[..5], &["M1", "M17", "M18", "M100", "M104"]);
        assert!(m1.contains(&"M190"));
        assert!(!m1.contains(&"M20"));
        let g2 = codes("g2");
        assert_eq!(g2[0], "G2");
        assert_eq!(&g2[1..4], &["G20", "G21", "G26"]);
        assert_eq!(codes("G01"), vec!["G1"]);
        assert_eq!(codes("G38."), vec!["G38.2", "G38.3", "G38.4", "G38.5"]);
        assert_eq!(codes("T")[0], "T0");
        assert!(codes("M").len() > 100);
        assert!(codes("").is_empty());
        assert!(codes("   ").is_empty());
        assert!(codes("M99999").is_empty());

        let temp = codes("temperature");
        assert!(temp.contains(&"M104") && temp.contains(&"M109") && temp.contains(&"M105"));
        let home = codes("home");
        assert_eq!(home[0], "G28", "{home:?}");
        let bed_level = codes("bed level");
        assert!(bed_level.contains(&"G29"), "{bed_level:?}");
        assert!(codes("zzzzqqq").is_empty());
    }
}
