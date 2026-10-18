//! Multi-line bed leveling reports: Marlin bilinear (`Bilinear Leveling
//! Grid:`), mesh bed leveling (`Measured points:`), UBL (`Bed Topography
//! Report`) and Klipper (`BED_MESH_OUTPUT`).

use super::text::strip_prefix_ci;
use crate::BedMesh;

/// Largest grid accepted (rows or columns); extra data is ignored.
const MAX_DIM: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MeshKind {
    Bilinear,
    Mbl,
    Ubl,
    KlipperProbed,
    KlipperMesh,
}

impl MeshKind {
    fn name(self) -> &'static str {
        match self {
            MeshKind::Bilinear => "bilinear",
            MeshKind::Mbl => "mbl",
            MeshKind::Ubl => "ubl",
            MeshKind::KlipperProbed => "klipper_probed",
            MeshKind::KlipperMesh => "klipper_mesh",
        }
    }
}

/// Which firmware printed the header lines that precede an unprefixed
/// `Measured points:` block (they decide the row order and the kind).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MblHeader {
    /// Marlin `Num X,Y:` / `... mesh. Z offset:` (or nothing): front row first.
    #[default]
    Marlin,
    /// Prusa G81 `Z search height:`: back row first.
    Prusa,
    /// Klipper `BED_MESH_OUTPUT` via respond_raw (`Mesh X,Y:`): back row first.
    Klipper,
}

/// What a line meant to an open block.
#[derive(Debug)]
pub(crate) enum Step {
    /// The line belonged to the block.
    Consumed,
    /// Unrelated line (e.g. an interleaved temperature report); the block
    /// stays open and the line is handled normally.
    PassThrough,
    /// The block ended at this (blank) line, which is consumed.
    Finished(Option<BedMesh>),
    /// The block ended; this line must be handled normally.
    FinishedPassThrough(Option<BedMesh>),
}

/// An open mesh report being collected.
#[derive(Clone, Debug)]
pub(crate) struct MeshBlock {
    kind: MeshKind,
    /// Unlabelled rows are printed back row (max Y) first.
    back_first: bool,
    /// Lines carry Klipper's "// " prefix.
    klipper: bool,
    rows: Vec<(Option<usize>, Vec<f32>)>,
    corners: Vec<(f32, f32)>,
}

enum LineKind {
    Row(Option<usize>, Vec<f32>),
    /// Column index header ("      0      1      2") or a "   |" spacer.
    Decoration,
    Corners(Vec<(f32, f32)>),
    Foreign,
}

fn value_token(tok: &str) -> Option<f32> {
    if tok.bytes().all(|b| b == b'=') || tok == "." || tok.eq_ignore_ascii_case("nan") {
        return Some(f32::NAN);
    }
    let tok = tok.strip_prefix('+').unwrap_or(tok);
    tok.parse::<f32>().ok().filter(|v| v.is_finite())
}

fn is_integer(tok: &str) -> bool {
    !tok.is_empty() && tok.len() <= 3 && tok.bytes().all(|b| b.is_ascii_digit())
}

/// "(  0,220)    (220,220)" -> corner coordinates.
fn parse_corners(line: &str) -> Option<Vec<(f32, f32)>> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        let inner = rest.strip_prefix('(')?;
        let close = inner.find(')')?;
        let (x, y) = inner[..close].split_once(',')?;
        out.push((x.trim().parse().ok()?, y.trim().parse().ok()?));
        rest = inner[close + 1..].trim_start();
    }
    (!out.is_empty()).then_some(out)
}

fn classify(body: &str) -> LineKind {
    if body.starts_with('(') {
        return parse_corners(body).map_or(LineKind::Foreign, LineKind::Corners);
    }
    let cleaned: String = body
        .chars()
        .map(|c| match c {
            '|' | '[' | ']' | ',' | '\t' => ' ',
            c => c,
        })
        .collect();
    let toks: Vec<&str> = cleaned.split_whitespace().collect();
    if toks.is_empty() {
        return LineKind::Decoration;
    }
    if toks.iter().all(|t| is_integer(t)) {
        return LineKind::Decoration;
    }
    let (label, values) = if toks.len() >= 2 && is_integer(toks[0]) {
        (toks[0].parse::<usize>().ok(), &toks[1..])
    } else {
        (None, &toks[..])
    };
    let mut row = Vec::with_capacity(values.len().min(MAX_DIM));
    for tok in values.iter().take(MAX_DIM) {
        match value_token(tok) {
            Some(v) => row.push(v),
            None => return LineKind::Foreign,
        }
    }
    LineKind::Row(label.filter(|&l| l < MAX_DIM), row)
}

impl MeshBlock {
    fn new(kind: MeshKind, back_first: bool, klipper: bool) -> MeshBlock {
        MeshBlock {
            kind,
            back_first,
            klipper,
            rows: Vec::new(),
            corners: Vec::new(),
        }
    }

    /// Recognise the header line of a mesh report (already trimmed, with
    /// any `echo:` prefix removed). `header` tells which firmware's header
    /// lines preceded an unprefixed "Measured points:".
    pub(crate) fn start(line: &str, header: MblHeader) -> Option<MeshBlock> {
        if let Some(body) = line.strip_prefix("//") {
            let body = body.trim();
            let lower = body.to_ascii_lowercase();
            if lower.starts_with("mesh leveling probed z positions") {
                return Some(MeshBlock::new(MeshKind::KlipperProbed, false, true));
            }
            if lower.starts_with("measured points") {
                return Some(MeshBlock::new(MeshKind::KlipperMesh, true, true));
            }
            if lower.starts_with("probed_matrix") {
                return Some(MeshBlock::new(MeshKind::KlipperProbed, false, true));
            }
            if lower.starts_with("mesh_matrix") {
                return Some(MeshBlock::new(MeshKind::KlipperMesh, false, true));
            }
            return None;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("bilinear leveling grid") {
            return Some(MeshBlock::new(MeshKind::Bilinear, false, false));
        }
        if lower.starts_with("bed topography report") {
            return Some(MeshBlock::new(MeshKind::Ubl, true, false));
        }
        if lower.starts_with("measured points") {
            return Some(match header {
                MblHeader::Marlin => MeshBlock::new(MeshKind::Mbl, false, false),
                MblHeader::Prusa => MeshBlock::new(MeshKind::Mbl, true, false),
                MblHeader::Klipper => MeshBlock::new(MeshKind::KlipperMesh, true, false),
            });
        }
        None
    }

    /// Feed the next (trimmed) line.
    pub(crate) fn feed(&mut self, line: &str) -> Step {
        let body = if self.klipper {
            match line.strip_prefix("//") {
                Some(b) => b.trim(),
                None if line.is_empty() => "",
                None => return Step::FinishedPassThrough(self.finish()),
            }
        } else {
            line
        };
        if body.is_empty() {
            return if self.rows.is_empty() {
                Step::Consumed
            } else {
                Step::Finished(self.finish())
            };
        }
        if !self.klipper && is_interleaved(line) {
            return Step::PassThrough;
        }
        match classify(body) {
            LineKind::Row(label, values) => {
                if self.rows.len() < MAX_DIM {
                    self.rows.push((label, values));
                }
                Step::Consumed
            }
            LineKind::Decoration => Step::Consumed,
            LineKind::Corners(c) => {
                self.corners.extend(c);
                Step::Consumed
            }
            LineKind::Foreign => Step::FinishedPassThrough(self.finish()),
        }
    }

    /// Close the block and build the mesh (`None` when no rows were seen).
    pub(crate) fn finish(&mut self) -> Option<BedMesh> {
        let rows = std::mem::take(&mut self.rows);
        if rows.is_empty() {
            return None;
        }
        let labelled = rows.iter().all(|(l, _)| l.is_some());
        let points: Vec<Vec<f32>> = if labelled {
            let max = rows.iter().filter_map(|(l, _)| *l).max().unwrap_or(0);
            let mut grid = vec![Vec::new(); max + 1];
            for (label, values) in rows {
                if let Some(slot) = label.and_then(|l| grid.get_mut(l)) {
                    *slot = values;
                }
            }
            grid
        } else {
            let mut grid: Vec<Vec<f32>> = rows.into_iter().map(|(_, v)| v).collect();
            if self.back_first {
                grid.reverse();
            }
            grid
        };
        let mut mesh = BedMesh::from_rows(points, self.kind.name());
        if mesh.cols() == 0 {
            return None;
        }
        let corners = std::mem::take(&mut self.corners);
        if let (Some(min_x), Some(max_x), Some(min_y), Some(max_y)) = (
            corners.iter().map(|c| c.0).reduce(f32::min),
            corners.iter().map(|c| c.0).reduce(f32::max),
            corners.iter().map(|c| c.1).reduce(f32::min),
            corners.iter().map(|c| c.1).reduce(f32::max),
        ) && min_x < max_x
            && min_y < max_y
        {
            mesh.set_bounds(min_x, max_x, min_y, max_y);
        }
        Some(mesh)
    }
}

/// Lines that may interleave with a Marlin mesh report without ending it.
fn is_interleaved(line: &str) -> bool {
    super::parse_temperatures(line).is_some()
        || strip_prefix_ci(line, "echo:busy:").is_some()
        || strip_prefix_ci(line, "busy:").is_some()
}

/// Klipper-style inline matrix: `[[0.1, 0.2], [0.3, 0.4]]` (row 0 = front).
pub(crate) fn parse_inline_matrix(text: &str) -> Option<Vec<Vec<f32>>> {
    let text = text.trim();
    let inner = text.strip_prefix('[')?.strip_suffix(']')?.trim();
    let mut rows = Vec::new();
    let mut rest = inner;
    while !rest.is_empty() {
        let after_open = rest.strip_prefix('[')?;
        let close = after_open.find(']')?;
        let row: Option<Vec<f32>> = after_open[..close]
            .split(',')
            .filter(|t| !t.trim().is_empty())
            .take(MAX_DIM)
            .map(|t| value_token(t.trim()))
            .collect();
        rows.push(row?);
        if rows.len() > MAX_DIM {
            return None;
        }
        rest = after_open[close + 1..].trim_start();
        rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
    }
    (!rows.is_empty()).then_some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_classification() {
        assert!(matches!(
            classify("      0      1      2"),
            LineKind::Decoration
        ));
        assert!(matches!(classify("   |"), LineKind::Decoration));
        match classify(" 1 +0.050 ===== -0.012") {
            LineKind::Row(Some(1), v) => {
                assert_eq!(v.len(), 3);
                assert!(v[1].is_nan());
            }
            _ => panic!("not a row"),
        }
        match classify(" 4 | +0.025  +0.050 [+0.075] -0.012   .   ") {
            LineKind::Row(Some(4), v) => {
                assert_eq!(v.len(), 5);
                assert!(v[4].is_nan());
            }
            _ => panic!("not a ubl row"),
        }
        match classify("0.1125\t0.13125\tNAN") {
            LineKind::Row(None, v) => assert!(v[2].is_nan()),
            _ => panic!("not a csv row"),
        }
        assert!(matches!(
            classify("echo:Bed Leveling ON"),
            LineKind::Foreign
        ));
        assert!(
            matches!(classify("(  0,220)      (220,220)"), LineKind::Corners(c) if c.len() == 2)
        );
        assert!(matches!(classify("(abc)"), LineKind::Foreign));
    }

    #[test]
    fn inline_matrix() {
        assert_eq!(
            parse_inline_matrix("[[0.1, 0.2], [0.3, 0.4]]"),
            Some(vec![vec![0.1, 0.2], vec![0.3, 0.4]])
        );
        assert_eq!(parse_inline_matrix("[]"), None);
        assert_eq!(parse_inline_matrix("[[0.1, x]]"), None);
        assert_eq!(parse_inline_matrix("[[0.1, 0.2]"), None);
        assert_eq!(parse_inline_matrix("nope"), None);
    }
}
