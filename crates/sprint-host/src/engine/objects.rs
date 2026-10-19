//! Object labels in a host print, for excluding (cancelling) single
//! objects while printing.
//!
//! The engine reads the labels from the stream itself; one marker style is
//! used per file, the first present in this order:
//!
//! 1. Klipper: `EXCLUDE_OBJECT_DEFINE NAME=..`, `EXCLUDE_OBJECT_START
//!    NAME=..`, `EXCLUDE_OBJECT_END`;
//! 2. Marlin `M486`: `T<count>` declares objects 0..count, `S<id>` starts
//!    one, `S-1` ends it, `A"name"` (or `Aname`) names the current or
//!    given one; unnamed objects are called `"Object <id>"`;
//! 3. PrusaSlicer / OrcaSlicer comments: `; printing object NAME` and
//!    `; stop printing object NAME`;
//! 4. Cura: `;MESH:NAME` until the next `;MESH:` (`;MESH:NONMESH` ends it)
//!    or the end of the layer (`;TIME_ELAPSED:`, `;LAYER:`): Cura never
//!    closes the last mesh of a layer, and after the last layer the end
//!    G-code follows.
//!
//! Names are the exact marker text, trimmed.

use std::collections::BTreeMap;

use sprint_gcode::{ExtendedCommand, GcodeFile, MachineState, parse_line};

use crate::script::format_number;

/// Most objects an `M486 T<count>` declares (a garbage count must not
/// create millions of names).
const MAX_M486_OBJECTS: i64 = 1000;
/// Feedrate of the travel past a skipped object (mm/min).
const SKIP_TRAVEL_MM_MIN: f64 = 6000.0;

/// The marker style of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Style {
    Klipper,
    M486,
    Comments,
    Cura,
}

/// What a line says about objects.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Marker {
    /// Declares an object (Klipper DEFINE).
    Define(String),
    /// M486 T<count>.
    Count(i64),
    /// An object starts (by name).
    Start(String),
    /// An M486 object starts, possibly named on the same line.
    StartId(i64, Option<String>),
    /// M486 A"name" without S: names the current object.
    NameCurrent(String),
    /// The current object ends.
    End,
}

/// The objects of a running host print.
#[derive(Clone, Debug, Default)]
pub(super) struct Objects {
    style: Option<Style>,
    /// Every object of the file, in first-seen order.
    pub(super) names: Vec<String>,
    /// M486 id -> name.
    m486: BTreeMap<i64, String>,
    /// Excluded by the user.
    pub(super) excluded: Vec<String>,
    /// The object whose block is being streamed.
    pub(super) current: Option<String>,
    m486_current: Option<i64>,
}

impl Objects {
    /// Find the marker style and every object of `file`.
    pub(super) fn scan(file: &GcodeFile) -> Objects {
        let mut found: BTreeMap<Style, Vec<Marker>> = BTreeMap::new();
        for raw in file.lines() {
            if let Some((style, marker)) = marker(raw) {
                found.entry(style).or_default().push(marker);
            }
        }
        let Some((&style, markers)) = found.iter().next() else {
            return Objects::default();
        };
        let mut objects = Objects {
            style: Some(style),
            ..Objects::default()
        };
        let mut count = 0;
        let mut current = None;
        for m in markers {
            match m {
                Marker::Define(name) | Marker::Start(name) => objects.add(name),
                Marker::Count(n) => count = count.max(*n),
                Marker::StartId(id, name) => {
                    current = Some(*id);
                    if let Some(name) = name {
                        objects.m486.entry(*id).or_insert_with(|| name.clone());
                    }
                    count = count.max(id + 1);
                }
                Marker::NameCurrent(name) => {
                    if let Some(id) = current {
                        objects.m486.entry(id).or_insert_with(|| name.clone());
                    }
                }
                Marker::End => current = None,
            }
        }
        if style == Style::M486 {
            for id in 0..count.min(MAX_M486_OBJECTS) {
                let name = objects.m486_name(id);
                objects.add(&name);
            }
        }
        objects
    }

    fn add(&mut self, name: &str) {
        if !name.is_empty() && !self.names.iter().any(|n| n == name) {
            self.names.push(name.to_string());
        }
    }

    fn m486_name(&self, id: i64) -> String {
        self.m486
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("Object {id}"))
    }

    /// Follow the stream: `raw` is the next file line. Returns whether the
    /// current object changed.
    pub(super) fn track(&mut self, raw: &str) -> bool {
        let Some(style) = self.style else {
            return false;
        };
        let Some((s, m)) = marker(raw) else {
            return false;
        };
        if s != style {
            return false;
        }
        let before = self.current.clone();
        match m {
            Marker::Define(_) | Marker::Count(_) => {}
            Marker::Start(name) => self.current = Some(name),
            Marker::StartId(id, name) => {
                if let Some(name) = name {
                    self.m486.entry(id).or_insert(name);
                }
                self.m486_current = Some(id);
                self.current = Some(self.m486_name(id));
            }
            Marker::NameCurrent(name) => {
                if let Some(id) = self.m486_current {
                    self.m486.entry(id).or_insert_with(|| name.clone());
                    self.current = Some(self.m486_name(id));
                }
            }
            Marker::End => {
                self.m486_current = None;
                self.current = None;
            }
        }
        self.current != before
    }

    /// The stream is inside an excluded object.
    pub(super) fn skipping(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|c| self.excluded.iter().any(|e| e == c))
    }
}

/// The marker on a raw file line, with its style.
fn marker(raw: &str) -> Option<(Style, Marker)> {
    let t = raw.trim_start();
    let first = t.as_bytes().first()?;
    let starts = |prefix: &str| {
        t.get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
    };
    match first {
        b';' => comment_marker(t[1..].trim()),
        b'E' | b'e' if starts("EXCLUDE_OBJECT_") => klipper_marker(t),
        b'M' | b'm' if starts("M486") => m486_marker(t),
        _ => None,
    }
}

fn comment_marker(c: &str) -> Option<(Style, Marker)> {
    if c.starts_with("stop printing object") {
        return Some((Style::Comments, Marker::End));
    }
    if let Some(name) = c.strip_prefix("printing object ") {
        let name = name.trim();
        return (!name.is_empty()).then(|| (Style::Comments, Marker::Start(name.to_string())));
    }
    let Some(name) = c.strip_prefix("MESH:").map(str::trim) else {
        // Cura ends every layer with ";TIME_ELAPSED:" and starts the next
        // with ";LAYER:" without closing its last mesh: the layer change
        // and the end G-code belong to no object.
        let layer_end = c.starts_with("TIME_ELAPSED:") || c.starts_with("LAYER:");
        return layer_end.then_some((Style::Cura, Marker::End));
    };
    Some((
        Style::Cura,
        if name == "NONMESH" || name.is_empty() {
            Marker::End
        } else {
            Marker::Start(name.to_string())
        },
    ))
}

fn klipper_marker(t: &str) -> Option<(Style, Marker)> {
    let code = t.split(';').next().unwrap_or(t);
    let cmd = ExtendedCommand::parse(code)?;
    let name = || cmd.param("NAME").map(|n| n.trim().to_string());
    let m = if cmd.is("EXCLUDE_OBJECT_DEFINE") {
        Marker::Define(name()?)
    } else if cmd.is("EXCLUDE_OBJECT_START") {
        Marker::Start(name()?)
    } else if cmd.is("EXCLUDE_OBJECT_END") {
        Marker::End
    } else {
        return None;
    };
    Some((Style::Klipper, m))
}

fn m486_marker(t: &str) -> Option<(Style, Marker)> {
    let code = t.split(';').next().unwrap_or(t);
    let parsed = parse_line(code);
    if !parsed.is('M', 486) {
        return None;
    }
    let name = m486_name(code);
    let int = |v: f64| v.is_finite().then(|| v.round() as i64);
    let m = if let Some(s) = parsed.get('S').and_then(int) {
        if s < 0 {
            Marker::End
        } else {
            Marker::StartId(s, name)
        }
    } else if let Some(n) = parsed.get('T').and_then(int) {
        Marker::Count(n.max(0))
    } else {
        Marker::NameCurrent(name?)
    };
    Some((Style::M486, m))
}

/// `A"name"` or `Aname` (to the end of the line) of an M486 line.
fn m486_name(code: &str) -> Option<String> {
    let rest = code.trim_start().get(4..)?;
    let b = rest.as_bytes();
    for i in 0..b.len() {
        let at_word_start = i == 0 || b[i - 1].is_ascii_whitespace();
        if at_word_start && (b[i] == b'A' || b[i] == b'a') {
            let after = rest.get(i + 1..)?;
            let name = match after.strip_prefix('"') {
                Some(quoted) => quoted.split('"').next().unwrap_or(quoted),
                None => after,
            }
            .trim();
            return (!name.is_empty()).then(|| name.to_string());
        }
    }
    None
}

/// What the printer needs after moves of an excluded object were left out,
/// before the next move: a non-extruding travel to where the file is
/// (`state`, the file's state over every line, skipped ones included), the
/// file's feedrate again and, for absolute extrusion, `G92 E` to the
/// file's extruder position so later E values stay consistent; in
/// absolute millimetres, the file's modes restored after.
pub(super) fn resync(state: &MachineState) -> Vec<String> {
    let mut out = Vec::new();
    if state.inches {
        out.push("G21".to_string());
    }
    if !state.absolute {
        out.push("G90".to_string());
    }
    let [x, y, z, e] = state.position;
    out.push(format!(
        "G0 X{} Y{} Z{} F{}",
        format_number(x, 3),
        format_number(y, 3),
        format_number(z, 3),
        format_number(SKIP_TRAVEL_MM_MIN, 0)
    ));
    out.push(format!("G1 F{}", format_number(state.feedrate_mm_min, 1)));
    if state.absolute_e {
        out.push(format!("G92 E{}", format_number(e, 5)));
    }
    if !state.absolute {
        out.push("G91".to_string());
    }
    if state.inches {
        out.push("G20".to_string());
    }
    out
}

/// Moves the excluded objects leave out: G0..G3 and firmware retraction
/// (G10/G11, but not RepRapFirmware's `G10 P`/`L` tool settings).
pub(super) fn is_object_motion(cmd: &str) -> bool {
    let p = parse_line(cmd);
    let Some(c) = p.code.filter(|c| c.letter == b'G' && c.sub.is_none()) else {
        return false;
    };
    match c.number {
        0..=3 | 11 => true,
        10 => !p.has('P') && !p.has('L'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(text: &str) -> Objects {
        Objects::scan(&GcodeFile::from_text("t", text))
    }

    fn track(objects: &mut Objects, text: &str) -> Vec<Option<String>> {
        text.lines()
            .map(|l| {
                objects.track(l);
                objects.current.clone()
            })
            .collect()
    }

    #[test]
    fn marlin_m486_labels() {
        let text = "M486 T3\nM486 S0 A\"cube\"\nG1 X1\nM486 S-1\nM486 S1\nM486 A\"cylinder\"\nG1 X2\nM486 S-1\nG1 X3";
        let mut o = scan(text);
        assert_eq!(o.names, ["cube", "cylinder", "Object 2"]);
        let seen = track(&mut o, text);
        assert_eq!(
            seen,
            [
                None,
                Some("cube".to_string()),
                Some("cube".to_string()),
                None,
                Some("cylinder".to_string()),
                Some("cylinder".to_string()),
                Some("cylinder".to_string()),
                None,
                None
            ]
        );
        // Unnamed and quote-less names.
        assert_eq!(
            scan("M486 S4\nM486 S-1").names,
            ["Object 0", "Object 1", "Object 2", "Object 3", "Object 4"]
        );
        assert_eq!(
            scan("M486 S0 Abolt head ; c\nM486 S-1").names,
            ["bolt head"]
        );
    }

    #[test]
    fn klipper_labels_win_over_the_others() {
        let text = "EXCLUDE_OBJECT_DEFINE NAME=part_1 CENTER=1,1\nEXCLUDE_OBJECT_DEFINE NAME='second part'\n; printing object ignored\nEXCLUDE_OBJECT_START NAME=part_1\nG1 X1\nEXCLUDE_OBJECT_END NAME=part_1\nEXCLUDE_OBJECT_START NAME='second part'\nG1 X2\nEXCLUDE_OBJECT_END";
        let mut o = scan(text);
        assert_eq!(o.style, Some(Style::Klipper));
        assert_eq!(o.names, ["part_1", "second part"]);
        let seen = track(&mut o, text);
        assert_eq!(seen[2], None, "comment labels are not this file's style");
        assert_eq!(seen[4].as_deref(), Some("part_1"));
        assert_eq!(seen[5], None);
        assert_eq!(seen[7].as_deref(), Some("second part"));
        assert_eq!(seen[8], None);
    }

    #[test]
    fn slicer_comment_labels() {
        let text = "; printing object cube id:0 copy 0\nG1 X1\n; stop printing object cube id:0 copy 0\n;  printing object  other \nG1 X2";
        let mut o = scan(text);
        assert_eq!(o.style, Some(Style::Comments));
        assert_eq!(o.names, ["cube id:0 copy 0", "other"]);
        let seen = track(&mut o, text);
        assert_eq!(seen[1].as_deref(), Some("cube id:0 copy 0"));
        assert_eq!(seen[2], None);
        assert_eq!(seen[4].as_deref(), Some("other"));
    }

    #[test]
    fn cura_meshes() {
        let text = ";LAYER:0\n;MESH:vase.stl\nG1 X1\n;MESH:NONMESH\nG0 Z1\n;LAYER:1\n;MESH:other.stl\nG1 X2\n;MESH:vase.stl\nG1 X3";
        let mut o = scan(text);
        assert_eq!(o.style, Some(Style::Cura));
        assert_eq!(o.names, ["vase.stl", "other.stl"]);
        let seen = track(&mut o, text);
        assert_eq!(seen[2].as_deref(), Some("vase.stl"));
        assert_eq!(seen[4], None);
        assert_eq!(seen[7].as_deref(), Some("other.stl"));
        assert_eq!(seen[9].as_deref(), Some("vase.stl"));
        // The end of a layer ends the mesh Cura left open.
        let text = ";MESH:vase.stl\nG1 X1\n;TIME_ELAPSED:12.5\nG1 E-2\n;LAYER:1\nG0 Z1\n;MESH:vase.stl\nG1 X2\n;TIME_ELAPSED:20\nG1 X0 Y200";
        let mut o = scan(text);
        assert_eq!(o.names, ["vase.stl"]);
        let seen = track(&mut o, text);
        assert_eq!(seen[1].as_deref(), Some("vase.stl"));
        assert_eq!(&seen[2..6], [None, None, None, None]);
        assert_eq!(seen[7].as_deref(), Some("vase.stl"));
        assert_eq!(seen[9], None, "the end G-code is no object's");
        // ";LAYER:" alone (other slicers) names no object.
        assert!(scan(";LAYER:0\nG1 X1\n;LAYER:1").names.is_empty());
    }

    #[test]
    fn files_without_labels_and_garbage() {
        let o = scan("G28\nG1 X1\nM4860\nEXCLUDE\n;MESH\n");
        assert!(o.names.is_empty() && o.style.is_none());
        for garbage in [
            "M486 S1e300",
            "M486 SNaN",
            "M486 A",
            "M486 T-5",
            "EXCLUDE_OBJECT_START",
            "\u{1F600}",
        ] {
            let _ = scan(garbage);
        }
        assert!(scan("M486 T99999999999").names.len() <= 1000);
    }

    #[test]
    fn excluded_objects_are_skipped() {
        let mut o = scan("M486 T2");
        o.excluded.push("Object 1".to_string());
        o.track("M486 S0");
        assert!(!o.skipping());
        o.track("M486 S1");
        assert!(o.skipping());
        o.track("M486 S-1");
        assert!(!o.skipping());
    }

    #[test]
    fn resync_travels_to_the_file_position() {
        let mut s = MachineState::new();
        s.apply(&parse_line("G1 X10 Y20 Z0.3 E5.5 F1800"));
        assert_eq!(
            resync(&s),
            ["G0 X10 Y20 Z0.3 F6000", "G1 F1800", "G92 E5.5"]
        );
        s.apply(&parse_line("M83"));
        assert_eq!(resync(&s), ["G0 X10 Y20 Z0.3 F6000", "G1 F1800"]);
        s.apply(&parse_line("G91"));
        s.apply(&parse_line("G20"));
        let r = resync(&s);
        assert_eq!(r.first().map(String::as_str), Some("G21"));
        assert_eq!(&r[r.len() - 2..], ["G91", "G20"]);
    }

    #[test]
    fn object_motion() {
        for m in ["G1 X1 E1", "G0 Z1", "G2 X1 I1", "G3 X1 J1", "G10", "G11"] {
            assert!(is_object_motion(m), "{m}");
        }
        for m in ["G92 E0", "M106 S255", "G10 P0 S200", "G28", "M83", "G4 P10"] {
            assert!(!is_object_motion(m), "{m}");
        }
    }
}
