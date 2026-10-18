//! Generates the sample G-code files committed in `samples/` at the
//! repository root:
//!
//! * `calibration-cube.gcode`: PrusaSlicer 2.7-style 20 mm calibration cube
//!   with PNG thumbnails, start/end G-code, skirt, two perimeters, 20%
//!   rectilinear infill, 3 bottom/top solid layers, retractions with Z hop,
//!   fan control, M73 progress and the trailing config block.
//! * `twisted-vase.gcode`: Cura-style spiral ("vase mode") print of a 60 mm
//!   tall twisted hexagonal vase.
//! * `arcs-test.gcode`: G2/G3 circles, S-curves, long arcs, a quarter-arc
//!   spiral, relative-mode and inch-mode arcs, arc travels and a helical
//!   tube in both I/J and R forms.
//!
//! ```text
//! cargo run -p sprint-gcode --example gen_samples
//! ```
//!
//! The output is deterministic. The files are synthetic: they imitate the
//! layout of real slicer output so the parser, analyzer and viewers can be
//! tested against it, and say so in a comment near the top. Print-time and
//! filament figures in the headers come from Sprint's own analyzer.

use std::f64::consts::{PI, TAU};
use std::fmt::Write as _;
use std::path::PathBuf;

use base64::Engine as _;
use sprint_gcode::{AnalyzeOptions, GcodeFile, Toolpath, analyze_simple};

fn main() {
    let dir = samples_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create {}: {e}", dir.display());
        std::process::exit(1);
    }
    for (name, text) in samples() {
        let path = dir.join(name);
        match std::fs::write(&path, text.as_bytes()) {
            Ok(()) => println!(
                "wrote {} ({:.0} KiB, {} lines)",
                path.display(),
                text.len() as f64 / 1024.0,
                text.lines().count()
            ),
            Err(e) => {
                eprintln!("cannot write {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
}

/// `samples/` at the repository root.
pub fn samples_dir() -> PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .unwrap_or(manifest)
        .join("samples")
}

/// Every sample as (file name, contents).
pub fn samples() -> Vec<(&'static str, String)> {
    vec![
        ("calibration-cube.gcode", calibration_cube()),
        ("twisted-vase.gcode", twisted_vase()),
        ("arcs-test.gcode", arcs_test()),
    ]
}

// ------------------------------------------------------------------ helpers

const FILAMENT_AREA: f64 = PI * 1.75 * 1.75 / 4.0;

/// PrusaSlicer number style: fixed decimals, trailing zeros trimmed and the
/// leading zero dropped (`.5`, `-.8`).
fn ps(v: f64, decimals: usize) -> String {
    let s = trimmed(v, decimals);
    if let Some(rest) = s.strip_prefix("0.") {
        format!(".{rest}")
    } else if let Some(rest) = s.strip_prefix("-0.") {
        format!("-.{rest}")
    } else {
        s
    }
}

/// Fixed decimals with trailing zeros trimmed (`10`, `0.5`).
fn trimmed(v: f64, decimals: usize) -> String {
    let mut s = format!("{v:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// "1h 2m 3s" / "12m 34s" like PrusaSlicer.
fn prusa_duration(secs: f64) -> String {
    let total = secs.round().max(0.0) as u64;
    let (d, h, m, s) = (
        total / 86400,
        total / 3600 % 24,
        total / 60 % 60,
        total % 60,
    );
    if d > 0 {
        format!("{d}d {h}h {m}m {s}s")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn analyze_text(text: &str) -> Toolpath {
    analyze_simple(
        &GcodeFile::from_text("sample.gcode", text),
        &AnalyzeOptions::default(),
    )
}

/// Replace placeholder lines (exact match) using `f`; other lines are kept.
fn fill_placeholders(text: &str, mut f: impl FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len() + 4096);
    for line in text.lines() {
        match line.strip_prefix(";@@").and_then(&mut f) {
            Some(replacement) => out.push_str(&replacement),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

// --------------------------------------------------------------- PNG output

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Minimal PNG encoder: 8-bit RGBA, no filtering, zlib "stored" blocks.
fn png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = width as usize * 4;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgba.chunks(stride.max(1)) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

/// Is `p` inside the convex quad `q` (any winding)?
fn in_quad(q: &[(f64, f64); 4], p: (f64, f64)) -> bool {
    let mut sign = 0.0f64;
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) % 4];
        let cross = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        if cross.abs() < 1e-12 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// A projected quad and its colour.
type Face = ([(f64, f64); 4], [f64; 3]);

/// Isometric render of an orange cube on a transparent background
/// (4x4 supersampled), as RGBA8.
fn render_cube(width: u32, height: u32) -> Vec<u8> {
    let c30 = (PI / 6.0).cos();
    let proj = |x: f64, y: f64, z: f64| ((x - y) * c30, (x + y) * 0.5 - z);
    let faces: [Face; 3] = [
        // Top (z = 1), lit.
        (
            [
                proj(0., 0., 1.),
                proj(1., 0., 1.),
                proj(1., 1., 1.),
                proj(0., 1., 1.),
            ],
            [247.0, 142.0, 75.0],
        ),
        // Right (x = 1).
        (
            [
                proj(1., 0., 0.),
                proj(1., 1., 0.),
                proj(1., 1., 1.),
                proj(1., 0., 1.),
            ],
            [237.0, 107.0, 33.0],
        ),
        // Left (y = 1), shaded.
        (
            [
                proj(0., 1., 0.),
                proj(1., 1., 0.),
                proj(1., 1., 1.),
                proj(0., 1., 1.),
            ],
            [176.0, 76.0, 18.0],
        ),
    ];
    let (w, h) = (f64::from(width), f64::from(height));
    // The projected cube spans u in [-c30, c30] and v in [-1, 1].
    let scale = 0.42 * (w / c30).min(h);
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    const SS: usize = 4;
    for py in 0..height {
        for px in 0..width {
            let mut rgb = [0.0f64; 3];
            let mut hits = 0usize;
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (f64::from(px) + (sx as f64 + 0.5) / SS as f64 - w / 2.0) / scale;
                    let v = (f64::from(py) + (sy as f64 + 0.5) / SS as f64 - h / 2.0) / scale;
                    if let Some((_, color)) = faces.iter().find(|(q, _)| in_quad(q, (u, v))) {
                        for (acc, c) in rgb.iter_mut().zip(color) {
                            *acc += c;
                        }
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                for c in rgb {
                    out.push((c / hits as f64).round() as u8);
                }
                out.push((255.0 * hits as f64 / (SS * SS) as f64).round() as u8);
            }
        }
    }
    out
}

/// "; thumbnail begin WxH LEN" block with 78-character base64 lines.
fn thumbnail_block(width: u32, height: u32) -> String {
    let png = png_rgba(width, height, &render_cube(width, height));
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let mut s = String::new();
    let _ = writeln!(s, "; thumbnail begin {width}x{height} {}", b64.len());
    for chunk in b64.as_bytes().chunks(78) {
        s.push_str("; ");
        s.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        s.push('\n');
    }
    s.push_str("; thumbnail end\n;\n\n");
    s
}

// ------------------------------------------------------- calibration cube

/// PrusaSlicer-like G-code writer that tracks position, feedrate and
/// retraction state.
struct PrusaWriter {
    out: String,
    x: f64,
    y: f64,
    z: f64,
    feed: f64,
    retracted: bool,
    width: Option<f64>,
    feature: &'static str,
}

const RETRACT: f64 = 0.8;
const LIFT: f64 = 0.4;
const TRAVEL_F: f64 = 10800.0;

impl PrusaWriter {
    fn new() -> Self {
        PrusaWriter {
            out: String::with_capacity(1 << 20),
            x: 0.0,
            y: 0.0,
            z: 0.0,
            feed: 0.0,
            retracted: false,
            width: None,
            feature: "",
        }
    }

    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn retract(&mut self) {
        if !self.retracted {
            self.line(&format!("G1 E-{} F2100", ps(RETRACT, 5)));
            self.feed = 2100.0;
            self.retracted = true;
        }
    }

    fn unretract(&mut self) {
        if self.retracted {
            self.line(&format!("G1 E{} F2100", ps(RETRACT, 5)));
            self.feed = 2100.0;
            self.retracted = false;
        }
    }

    /// Travel; longer moves retract and hop like PrusaSlicer.
    fn travel(&mut self, x: f64, y: f64) {
        let d = (x - self.x).hypot(y - self.y);
        if d < 1e-6 {
            return;
        }
        if d > 2.0 {
            self.retract();
            let z = self.z;
            self.line(&format!("G1 Z{} F720", ps(z + LIFT, 3)));
            self.line(&format!("G1 X{} Y{} F{TRAVEL_F}", ps(x, 3), ps(y, 3)));
            self.line(&format!("G1 Z{} F720", ps(z, 3)));
            self.feed = 720.0;
            self.unretract();
        } else {
            self.line(&format!("G1 X{} Y{} F{TRAVEL_F}", ps(x, 3), ps(y, 3)));
            self.feed = TRAVEL_F;
        }
        self.x = x;
        self.y = y;
    }

    fn feature(&mut self, name: &'static str, width: f64) {
        if self.feature != name {
            self.line(&format!(";TYPE:{name}"));
            self.feature = name;
        }
        if self.width.is_none_or(|w| (w - width).abs() > 1e-9) {
            self.line(&format!(";WIDTH:{}", trimmed(width, 6)));
            self.width = Some(width);
        }
    }

    /// Extrude to (x, y) with PrusaSlicer's rounded-rectangle flow model.
    fn extrude(&mut self, x: f64, y: f64, width: f64, height: f64, feed: f64) {
        if (feed - self.feed).abs() > 1e-9 {
            self.line(&format!("G1 F{}", trimmed(feed, 0)));
            self.feed = feed;
        }
        let len = (x - self.x).hypot(y - self.y);
        let area = (width - height * (1.0 - PI / 4.0)) * height;
        let e = len * area / FILAMENT_AREA;
        self.line(&format!("G1 X{} Y{} E{}", ps(x, 3), ps(y, 3), ps(e, 5)));
        self.x = x;
        self.y = y;
    }

    /// A square loop of half-size `s` around (cx, cy), seam at the rear-left
    /// corner, counter-clockwise. The feature comment follows the travel to
    /// the seam, as in PrusaSlicer.
    #[allow(clippy::too_many_arguments)]
    fn square_loop(
        &mut self,
        feature: &'static str,
        cx: f64,
        cy: f64,
        s: f64,
        width: f64,
        height: f64,
        feed: f64,
    ) {
        let corners = [
            (cx - s, cy + s),
            (cx - s, cy - s),
            (cx + s, cy - s),
            (cx + s, cy + s),
            (cx - s, cy + s),
        ];
        self.travel(corners[0].0, corners[0].1);
        self.feature(feature, width);
        for &(x, y) in &corners[1..] {
            self.extrude(x, y, width, height, feed);
        }
    }
}

/// Rectilinear lines inside the square |x - cx|, |y - cy| <= a at `angle`
/// (radians, line direction), `spacing` apart, in zig-zag order.
fn rectilinear(cx: f64, cy: f64, a: f64, angle: f64, spacing: f64) -> Vec<[(f64, f64); 2]> {
    let d = (angle.cos(), angle.sin());
    let n = (-d.1, d.0);
    let cmax = a * (n.0.abs() + n.1.abs());
    let count = (2.0 * cmax / spacing).floor() as i64;
    let c0 = -(count as f64) * spacing / 2.0;
    let mut lines = Vec::new();
    for i in 0..=count {
        let c = c0 + i as f64 * spacing;
        let (mut t0, mut t1) = (f64::NEG_INFINITY, f64::INFINITY);
        for (nc, dc) in [(n.0, d.0), (n.1, d.1)] {
            let base = c * nc;
            if dc.abs() < 1e-12 {
                if base.abs() > a {
                    t0 = 1.0;
                    t1 = 0.0;
                }
                continue;
            }
            let (lo, hi) = ((-a - base) / dc, (a - base) / dc);
            t0 = t0.max(lo.min(hi));
            t1 = t1.min(lo.max(hi));
        }
        if t1 - t0 < 0.2 {
            continue;
        }
        let p = |t: f64| (cx + c * n.0 + t * d.0, cy + c * n.1 + t * d.1);
        let mut seg = [p(t0), p(t1)];
        if lines.len() % 2 == 1 {
            seg.swap(0, 1);
        }
        lines.push(seg);
    }
    lines
}

/// PrusaSlicer 2.7-style 20 mm calibration cube.
pub fn calibration_cube() -> String {
    const LAYERS: usize = 100;
    const H: f64 = 0.2;
    let (cx, cy) = (125.0, 105.0);
    let half = 10.0;
    let w = 0.45;
    let w_first = 0.42;
    let w_top = 0.40;
    let spacing = w - H * (1.0 - PI / 4.0);
    let ext_s = half - w / 2.0;
    let inner_s = ext_s - spacing;
    let infill_a = inner_s - spacing * 0.85;

    let mut g = PrusaWriter::new();
    g.out
        .push_str("; generated by PrusaSlicer 2.7.1+win64 on 2024-01-15 at 10:00:00 UTC\n");
    g.out.push_str(
        "; Synthetic sample written by sprint-gcode's gen_samples example (PrusaSlicer layout)\n\n",
    );
    g.out.push_str("; \n\n");
    g.out.push_str(&thumbnail_block(16, 16));
    g.out.push_str(&thumbnail_block(220, 124));
    g.out.push_str(
        "; external perimeters extrusion width = 0.45mm\n\
         ; perimeters extrusion width = 0.45mm\n\
         ; infill extrusion width = 0.45mm\n\
         ; solid infill extrusion width = 0.45mm\n\
         ; top infill extrusion width = 0.40mm\n\
         ; first layer extrusion width = 0.42mm\n\n",
    );
    g.line(";@@M73 START");
    g.line("M201 X1000 Y1000 Z200 E5000 ; sets maximum accelerations, mm/sec^2");
    g.line("M203 X200 Y200 Z12 E120 ; sets maximum feedrates, mm / sec");
    g.line(
        "M204 P1250 R1250 T1250 ; sets acceleration (P, T) and retract acceleration (R), mm/sec^2",
    );
    g.line("M205 X8.00 Y8.00 Z0.40 E4.50 ; sets the jerk limits, mm/sec");
    g.line("M205 S0 T0 ; sets the minimum extruding and travel feed rate, mm/sec");
    g.line(";TYPE:Custom");
    g.feature = "Custom";
    for l in [
        "G90 ; use absolute coordinates",
        "M83 ; extruder relative mode",
        "M140 S60 ; set bed temp",
        "M104 S215 ; set extruder temp",
        "M190 S60 ; wait for bed temp",
        "M109 S215 ; wait for extruder temp",
        "G28 ; home all axes",
        "G29 ; auto bed leveling",
        "G1 Z0.2 F720",
        "G1 Y-3 F1000 ; go outside print area",
        "G92 E0",
        "G1 X60 E9 F1000 ; intro line",
        "G1 X100 E12.5 F1000 ; intro line",
        "G92 E0",
        "G21 ; set units to millimeters",
        "G90 ; use absolute coordinates",
        "M83 ; use relative distances for extrusion",
        "; Filament gcode",
    ] {
        g.line(l);
    }
    g.x = 100.0;
    g.y = -3.0;
    g.z = 0.2;
    g.feed = 1000.0;

    for layer in 1..=LAYERS {
        let z = layer as f64 * H;
        let first = layer == 1;
        let zs = trimmed(z, 3);
        g.line(";LAYER_CHANGE");
        g.line(&format!(";Z:{zs}"));
        g.line(&format!(";HEIGHT:{}", trimmed(H, 3)));
        g.line(";BEFORE_LAYER_CHANGE");
        g.line("G92 E0.0");
        g.line(&format!(";{zs}"));
        g.out.push('\n');
        g.retract();
        g.line(&format!("G1 Z{} F720", ps(z + LIFT, 3)));
        g.z = z;
        g.line(";AFTER_LAYER_CHANGE");
        g.line(&format!(";{zs}"));
        match layer {
            1 => g.line("M107"),
            2 => g.line("M106 S255"),
            _ => {}
        }
        g.line(&format!(";@@M73 {}", layer - 1));
        // First feature start: travel at the lifted height, then descend.
        let start = if first {
            let s = half + 2.0 + w_first / 2.0;
            (cx - s, cy + s)
        } else {
            (cx - inner_s, cy + inner_s)
        };
        g.line(&format!(
            "G1 X{} Y{} F{TRAVEL_F}",
            ps(start.0, 3),
            ps(start.1, 3)
        ));
        g.line(&format!("G1 Z{} F720", ps(z, 3)));
        g.x = start.0;
        g.y = start.1;
        g.feed = 720.0;
        g.unretract();

        let (width, perim_f, ext_f) = if first {
            (w_first, 1200.0, 1200.0)
        } else {
            (w, 2700.0, 1500.0)
        };
        if first {
            let s = half + 2.0 + w_first / 2.0;
            g.square_loop("Skirt/Brim", cx, cy, s, w_first, H, 1200.0);
        }
        g.square_loop("Perimeter", cx, cy, inner_s, width, H, perim_f);
        g.square_loop("External perimeter", cx, cy, ext_s, width, H, ext_f);

        // Infill.
        let bottom = layer <= 3;
        let top = layer > LAYERS - 3;
        let angle = if layer % 2 == 1 {
            PI / 4.0
        } else {
            3.0 * PI / 4.0
        };
        let (name, fw, lines, feed) = if layer == LAYERS {
            let lines = rectilinear(cx, cy, infill_a, angle, w_top - H * (1.0 - PI / 4.0));
            ("Top solid infill", w_top, lines, 2400.0)
        } else if bottom || top {
            let lines = rectilinear(cx, cy, infill_a, angle, spacing);
            (
                "Solid infill",
                if first { w_first } else { w },
                lines,
                if first { 1200.0 } else { 4800.0 },
            )
        } else {
            let lines = rectilinear(cx, cy, infill_a, angle, spacing / 0.2);
            ("Internal infill", w, lines, 4800.0)
        };
        for (i, seg) in lines.iter().enumerate() {
            if i == 0 {
                g.travel(seg[0].0, seg[0].1);
                g.feature(name, fw);
            } else {
                // Connected infill: extrude along the boundary.
                g.extrude(seg[0].0, seg[0].1, fw, H, feed);
            }
            g.extrude(seg[1].0, seg[1].1, fw, H, feed);
        }
    }
    // End G-code.
    g.retract();
    g.line(";TYPE:Custom");
    for l in [
        "; Filament-specific end gcode",
        "G1 Z50 F720 ; move print head up",
        "G1 X0 Y200 F3600 ; park",
        "G4 ; wait",
        "M221 S100 ; reset flow",
        "M104 S0 ; turn off temperature",
        "M140 S0 ; turn off heatbed",
        "M107 ; turn off fan",
        "M84 ; disable motors",
        ";@@M73 END",
    ] {
        g.line(l);
    }

    let body = g.out;
    let tp = analyze_text(&body);
    let total = f64::from(tp.stats.estimated_time);
    let m73 = |elapsed: f64| {
        let pct = if total > 0.0 {
            (elapsed / total * 100.0).floor().clamp(0.0, 100.0)
        } else {
            0.0
        };
        let remaining = ((total - elapsed).max(0.0) / 60.0).round();
        format!("M73 P{pct} R{remaining}\nM73 Q{pct} S{remaining}")
    };
    let mut text = fill_placeholders(&body, |tag| match tag {
        "M73 START" => Some(m73(0.0)),
        "M73 END" => Some(m73(total)),
        _ => {
            let index: usize = tag.strip_prefix("M73 ")?.parse().ok()?;
            let layer = tp.layers.get(index)?;
            Some(m73(f64::from(layer.time_start)))
        }
    });

    // Summary and configuration, as PrusaSlicer appends them.
    let st = &tp.stats;
    let grams = f64::from(st.filament_weight);
    let first_layer_time = tp.layers.first().map_or(0.0, |l| f64::from(l.duration));
    text.push('\n');
    let _ = writeln!(text, "; filament used [mm] = {:.2}", st.filament_length);
    let _ = writeln!(
        text,
        "; filament used [cm3] = {:.2}",
        st.filament_volume / 1000.0
    );
    let _ = writeln!(text, "; filament used [g] = {grams:.2}");
    let _ = writeln!(text, "; filament cost = {:.2}", grams * 0.025);
    let _ = writeln!(text, "; total filament used [g] = {grams:.2}");
    let _ = writeln!(text, "; total filament cost = {:.2}", grams * 0.025);
    let _ = writeln!(
        text,
        "; estimated printing time (normal mode) = {}",
        prusa_duration(total)
    );
    let _ = writeln!(
        text,
        "; estimated printing time (silent mode) = {}",
        prusa_duration(total * 1.04)
    );
    let _ = writeln!(
        text,
        "; estimated first layer printing time (normal mode) = {}",
        prusa_duration(first_layer_time)
    );
    let _ = writeln!(
        text,
        "; estimated first layer printing time (silent mode) = {}",
        prusa_duration(first_layer_time * 1.04)
    );
    text.push_str("\n; prusaslicer_config = begin\n");
    for (k, v) in CUBE_CONFIG {
        let _ = writeln!(text, "; {k} = {v}");
    }
    text.push_str("; prusaslicer_config = end\n");
    text
}

/// Configuration dump of the cube (alphabetical, like PrusaSlicer).
const CUBE_CONFIG: &[(&str, &str)] = &[
    ("avoid_crossing_perimeters", "0"),
    ("bed_shape", "0x0,250x0,250x210,0x210"),
    ("bed_temperature", "60"),
    ("bottom_solid_layers", "3"),
    ("bridge_speed", "30"),
    ("compatible_printers_condition_cummulative", "\"\""),
    ("default_acceleration", "1000"),
    ("disable_fan_first_layers", "1"),
    ("external_perimeter_extrusion_width", "0.45"),
    ("external_perimeter_speed", "25"),
    ("extrusion_width", "0.45"),
    ("fan_always_on", "1"),
    ("filament_colour", "#FF8000"),
    ("filament_cost", "25"),
    ("filament_density", "1.24"),
    ("filament_diameter", "1.75"),
    ("filament_settings_id", "\"Generic PLA\""),
    ("filament_type", "PLA"),
    ("fill_angle", "45"),
    ("fill_density", "20%"),
    ("fill_pattern", "rectilinear"),
    ("first_layer_bed_temperature", "60"),
    ("first_layer_extrusion_width", "0.42"),
    ("first_layer_height", "0.2"),
    ("first_layer_speed", "20"),
    ("first_layer_temperature", "215"),
    ("gcode_flavor", "marlin2"),
    ("infill_extrusion_width", "0.45"),
    ("infill_speed", "80"),
    ("layer_height", "0.2"),
    ("machine_max_acceleration_e", "5000"),
    ("machine_max_acceleration_extruding", "1250"),
    ("machine_max_acceleration_retracting", "1250"),
    ("machine_max_acceleration_travel", "1250"),
    ("machine_max_acceleration_x", "1000"),
    ("machine_max_acceleration_y", "1000"),
    ("machine_max_acceleration_z", "200"),
    ("machine_max_feedrate_e", "120"),
    ("machine_max_feedrate_x", "200"),
    ("machine_max_feedrate_y", "200"),
    ("machine_max_feedrate_z", "12"),
    ("machine_max_jerk_e", "4.5"),
    ("machine_max_jerk_x", "8"),
    ("machine_max_jerk_y", "8"),
    ("machine_max_jerk_z", "0.4"),
    ("max_fan_speed", "100"),
    ("min_fan_speed", "100"),
    ("nozzle_diameter", "0.4"),
    (
        "output_filename_format",
        "{input_filename_base}_{layer_height}mm_{filament_type[0]}.gcode",
    ),
    ("perimeter_extrusion_width", "0.45"),
    ("perimeter_speed", "45"),
    ("perimeters", "2"),
    ("print_settings_id", "\"0.20mm QUALITY\""),
    ("printer_settings_id", "\"Generic Marlin 2 printer\""),
    ("retract_before_travel", "2"),
    ("retract_length", "0.8"),
    ("retract_lift", "0.4"),
    ("retract_speed", "35"),
    ("seam_position", "aligned"),
    ("skirt_distance", "2"),
    ("skirt_height", "1"),
    ("skirts", "1"),
    ("solid_infill_extrusion_width", "0.45"),
    ("solid_infill_speed", "80"),
    ("temperature", "215"),
    ("thumbnails", "16x16/PNG, 220x124/PNG"),
    ("top_infill_extrusion_width", "0.4"),
    ("top_solid_infill_speed", "40"),
    ("top_solid_layers", "3"),
    ("travel_speed", "180"),
    ("use_relative_e_distances", "1"),
    ("wipe", "0"),
];

// ------------------------------------------------------------ twisted vase

const VASE_HEIGHT: f64 = 60.0;
const VASE_CENTER: (f64, f64) = (110.0, 110.0);

/// Smooth radius profile (Catmull-Rom through control points), mm.
fn vase_radius(z: f64) -> f64 {
    const PTS: [(f64, f64); 7] = [
        (-12.0, 20.0),
        (0.0, 21.0),
        (14.0, 27.5),
        (30.0, 30.0),
        (46.0, 22.0),
        (60.0, 25.5),
        (72.0, 31.0),
    ];
    let z = z.clamp(0.0, VASE_HEIGHT);
    let i = (1..PTS.len() - 2)
        .find(|&i| z <= PTS[i + 1].0)
        .unwrap_or(PTS.len() - 3);
    let (p0, p1, p2, p3) = (PTS[i - 1], PTS[i], PTS[i + 1], PTS[i + 2]);
    let t = (z - p1.0) / (p2.0 - p1.0);
    let (t2, t3) = (t * t, t * t * t);
    0.5 * (2.0 * p1.1
        + (-p0.1 + p2.1) * t
        + (2.0 * p0.1 - 5.0 * p1.1 + 4.0 * p2.1 - p3.1) * t2
        + (-p0.1 + 3.0 * p1.1 - 3.0 * p2.1 + p3.1) * t3)
}

/// Twist angle at height z: two thirds of a turn of the hexagon's lobes.
fn vase_twist(z: f64) -> f64 {
    let t = (z / VASE_HEIGHT).clamp(0.0, 1.0);
    // Ease in and out so the twist starts and ends gently.
    let eased = t * t * (3.0 - 2.0 * t);
    eased * (2.0 * PI / 3.0)
}

/// Point on the soft hexagonal outline at angle `phi`, height `z`, inset
/// radially by `inset`.
fn vase_point(phi: f64, z: f64, inset: f64) -> (f64, f64) {
    let r = vase_radius(z) * (1.0 + 0.075 * (6.0 * (phi - vase_twist(z))).cos()) - inset;
    (VASE_CENTER.0 + r * phi.cos(), VASE_CENTER.1 + r * phi.sin())
}

/// Intersections of the line through `p` with direction `d` and the closed
/// polygon, as sorted parameters paired into inside intervals.
fn clip_line_polygon(p: (f64, f64), d: (f64, f64), poly: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut ts = Vec::new();
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let e = (b.0 - a.0, b.1 - a.1);
        let denom = d.0 * e.1 - d.1 * e.0;
        if denom.abs() < 1e-12 {
            continue;
        }
        let w = (a.0 - p.0, a.1 - p.1);
        let t = (w.0 * e.1 - w.1 * e.0) / denom;
        let s = (w.0 * d.1 - w.1 * d.0) / denom;
        if (0.0..1.0).contains(&s) {
            ts.push(t);
        }
    }
    ts.sort_by(f64::total_cmp);
    ts.chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| (c[0], c[1]))
        .collect()
}

/// Cura-style writer with absolute extrusion.
struct CuraWriter {
    out: String,
    x: f64,
    y: f64,
    z: f64,
    e: f64,
    feed: f64,
}

impl CuraWriter {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn feed_word(&mut self, f: f64) -> String {
        if (f - self.feed).abs() > 1e-9 {
            self.feed = f;
            format!("F{} ", trimmed(f, 1))
        } else {
            String::new()
        }
    }

    fn travel(&mut self, x: f64, y: f64, z: Option<f64>) {
        let f = self.feed_word(9000.0);
        let zs = match z {
            Some(z) => {
                self.z = z;
                format!(" Z{}", trimmed(z, 3))
            }
            None => String::new(),
        };
        self.line(&format!("G0 {f}X{} Y{}{zs}", trimmed(x, 3), trimmed(y, 3)));
        self.x = x;
        self.y = y;
    }

    fn extrude(&mut self, x: f64, y: f64, z: Option<f64>, feed: f64) {
        let len = match z {
            Some(z) => ((x - self.x).powi(2) + (y - self.y).powi(2) + (z - self.z).powi(2)).sqrt(),
            None => (x - self.x).hypot(y - self.y),
        };
        self.e += len * 0.4 * 0.2 / FILAMENT_AREA;
        let f = self.feed_word(feed);
        let zs = match z {
            Some(z) => {
                self.z = z;
                format!(" Z{}", trimmed(z, 3))
            }
            None => String::new(),
        };
        self.line(&format!(
            "G1 {f}X{} Y{}{zs} E{}",
            trimmed(x, 3),
            trimmed(y, 3),
            trimmed(self.e, 5)
        ));
        self.x = x;
        self.y = y;
    }

    fn retract(&mut self) {
        self.e -= 5.0;
        self.feed = 2700.0;
        let e = trimmed(self.e, 5);
        self.line(&format!("G1 F2700 E{e}"));
    }

    fn unretract(&mut self) {
        self.e += 5.0;
        self.feed = 2700.0;
        let e = trimmed(self.e, 5);
        self.line(&format!("G1 F2700 E{e}"));
    }
}

/// Cura-style spiral vase: 3 solid bottom layers, then one continuously
/// rising outer wall, one `;LAYER:` per revolution.
pub fn twisted_vase() -> String {
    const LAYERS: usize = 300;
    const BOTTOM: usize = 3;
    const H: f64 = 0.2;
    const N: usize = 144;
    let mut g = CuraWriter {
        out: String::with_capacity(3 << 20),
        x: 0.0,
        y: 0.0,
        z: 0.0,
        e: 0.0,
        feed: 0.0,
    };
    for l in [
        ";FLAVOR:Marlin",
        ";@@TIME",
        ";@@FILAMENT",
        ";Layer height: 0.2",
        ";@@BOUNDS",
        ";TARGET_MACHINE.NAME:Generic Marlin Printer",
        ";Generated with Cura_SteamEngine 5.6.0",
        ";Synthetic sample written by sprint-gcode's gen_samples example (Cura layout)",
        "M140 S60",
        "M105",
        "M190 S60",
        "M104 S210",
        "M105",
        "M109 S210",
        "M82 ;absolute extrusion mode",
        "G28 ;Home",
        "G92 E0 ;Reset Extruder",
        "G1 Z2.0 F3000 ;Move Z Axis up",
        "G1 X10.1 Y20 Z0.28 F5000.0 ;Move to start position",
        "G1 X10.1 Y200.0 Z0.28 F1500.0 E15 ;Draw the first line",
        "G1 X10.4 Y200.0 Z0.28 F5000.0 ;Move to side a little",
        "G1 X10.4 Y20 Z0.28 F1500.0 E30 ;Draw the second line",
        "G92 E0 ;Reset Extruder",
        "G1 Z2.0 F3000 ;Move Z Axis up",
        "G92 E0",
        "G92 E0",
        "G1 F2700 E-5",
    ] {
        g.line(l);
    }
    g.x = 10.4;
    g.y = 20.0;
    g.z = 2.0;
    g.e = -5.0;
    g.feed = 2700.0;
    let _ = writeln!(g.out, ";LAYER_COUNT:{LAYERS}");

    let step = TAU / N as f64;
    for layer in 0..LAYERS {
        let z = (layer + 1) as f64 * H;
        let _ = writeln!(g.out, ";LAYER:{layer}");
        match layer {
            0 => g.line("M107"),
            1 => g.line("M106 S85"),
            2 => g.line("M106 S170"),
            3 => g.line("M106 S255"),
            _ => {}
        }
        g.line(";MESH:twisted-vase.stl");
        if layer < BOTTOM {
            // Outer wall loop at constant Z.
            let wall_feed = if layer == 0 { 1200.0 } else { 1500.0 };
            let start = vase_point(0.0, z, 0.2);
            g.travel(start.0, start.1, Some(z));
            g.line(";TYPE:WALL-OUTER");
            if layer == 0 {
                g.unretract();
            }
            for k in 1..=N {
                let p = vase_point(k as f64 * step, z, 0.2);
                g.extrude(p.0, p.1, None, wall_feed);
            }
            // Bottom skin: zig-zag lines clipped to the inset outline.
            let outline: Vec<(f64, f64)> = (0..N)
                .map(|k| vase_point(k as f64 * step, z, 0.6))
                .collect();
            let angle = if layer % 2 == 0 { PI / 4.0 } else { -PI / 4.0 };
            let d = (angle.cos(), angle.sin());
            let n = (-d.1, d.0);
            let reach = vase_radius(z) * 1.1;
            let mut lines = Vec::new();
            let mut c = -reach;
            while c <= reach {
                let p = (VASE_CENTER.0 + c * n.0, VASE_CENTER.1 + c * n.1);
                for (t0, t1) in clip_line_polygon(p, d, &outline) {
                    if t1 - t0 > 0.3 {
                        lines.push([
                            (p.0 + t0 * d.0, p.1 + t0 * d.1),
                            (p.0 + t1 * d.0, p.1 + t1 * d.1),
                        ]);
                    }
                }
                c += 0.4;
            }
            g.line(";TYPE:SKIN");
            let skin_feed = if layer == 0 { 1200.0 } else { 1800.0 };
            for (i, mut seg) in lines.into_iter().enumerate() {
                if i % 2 == 1 {
                    seg.swap(0, 1);
                }
                g.travel(seg[0].0, seg[0].1, None);
                g.extrude(seg[1].0, seg[1].1, None, skin_feed);
            }
            // Back to the wall seam for the next layer.
        } else {
            g.line(";TYPE:WALL-OUTER");
            if layer == BOTTOM {
                let start = vase_point(0.0, z - H, 0.2);
                g.travel(start.0, start.1, None);
            }
            // One revolution rising from the previous layer's Z to this one.
            let z0 = z - H;
            for k in 1..=N {
                let t = k as f64 / N as f64;
                let zz = z0 + H * t;
                let p = vase_point(k as f64 * step, zz, 0.2);
                g.extrude(p.0, p.1, Some(zz), 1500.0);
            }
        }
        let _ = writeln!(g.out, ";@@TIME_ELAPSED {layer}");
    }
    g.retract();
    for l in [
        "M140 S0",
        "M107",
        "G91 ;Relative positioning",
        "G1 E-2 F2700 ;Retract a bit",
        "G1 E-2 Z0.2 F2400 ;Retract and raise Z",
        "G1 X5 Y5 Z10 F3000 ;Wipe out",
        "G1 Z10 ;Raise Z more",
        "G90 ;Absolute positioning",
        "",
        "G1 X0 Y220 ;Present print",
        "M106 S0 ;Turn-off fan",
        "M104 S0 ;Turn-off hotend",
        "M140 S0 ;Turn-off bed",
        "",
        "M84 X Y E ;Disable all steppers but Z",
        "",
        "M82 ;absolute extrusion mode",
        "M104 S0",
        ";End of Gcode",
    ] {
        g.line(l);
    }
    let settings = "{\"global_quality\": \"[general]\\\\nversion = 4\\\\nname = Vase #1\\\\ndefinition = custom\\\\n\\\\n[metadata]\\\\ntype = quality_changes\\\\nquality_type = standard\\\\nsetting_version = 22\\\\n\\\\n[values]\\\\nadhesion_type = none\\\\nlayer_height = 0.2\\\\nmagic_spiralize = True\\\\n\\\\n\", \"extruder_quality\": [\"[general]\\\\nversion = 4\\\\nname = Vase #1\\\\ndefinition = custom\\\\n\\\\n[metadata]\\\\nposition = 0\\\\ntype = quality_changes\\\\nquality_type = standard\\\\nsetting_version = 22\\\\n\\\\n[values]\\\\nbottom_layers = 3\\\\nmaterial_print_temperature = 210\\\\nspeed_wall_0 = 25\\\\n\\\\n\"]}";
    let chunks: Vec<char> = settings.chars().collect();
    for chunk in chunks.chunks(80) {
        let s: String = chunk.iter().collect();
        let _ = writeln!(g.out, ";SETTING_3 {s}");
    }

    let body = g.out;
    let tp = analyze_text(&body);
    let b = tp.bounds;
    fill_placeholders(&body, |tag| match tag {
        "TIME" => Some(format!(";TIME:{}", tp.stats.estimated_time.round())),
        "FILAMENT" => Some(format!(
            ";Filament used: {}m",
            trimmed(f64::from(tp.stats.filament_length) / 1000.0, 5)
        )),
        "BOUNDS" => Some(format!(
            ";MINX:{:.3}\n;MINY:{:.3}\n;MINZ:{:.3}\n;MAXX:{:.3}\n;MAXY:{:.3}\n;MAXZ:{:.3}",
            b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]
        )),
        _ => {
            let index: usize = tag.strip_prefix("TIME_ELAPSED ")?.parse().ok()?;
            let end = match tp.layers.get(index + 1) {
                Some(next) => next.time_start,
                None => tp.stats.estimated_time,
            };
            Some(format!(";TIME_ELAPSED:{end:.6}"))
        }
    })
}

// --------------------------------------------------------------- arcs test

/// Writer for the arcs test: absolute extrusion, E by path length.
struct ArcWriter {
    out: String,
    e: f64,
}

const E_PER_MM: f64 = 0.4 * 0.2 / FILAMENT_AREA;

impl ArcWriter {
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    /// Advance absolute E by `length` mm of extrusion and format it.
    fn e(&mut self, length: f64) -> String {
        self.e += length * E_PER_MM;
        trimmed(self.e, 5)
    }
}

/// G2/G3 coverage: I/J and R forms, full circles, >180° arcs, relative and
/// inch modes, arc travels, a stacked tube and a 50-turn helix.
pub fn arcs_test() -> String {
    let mut g = ArcWriter {
        out: String::with_capacity(1 << 16),
        e: 0.0,
    };
    for l in [
        "; Sprint arcs test: G2/G3 in I/J and R forms, full circles, arcs over 180 degrees,",
        "; relative-mode and inch-mode arcs, arc travels and helices with P turns.",
        "; Synthetic sample written by sprint-gcode's gen_samples example.",
        "G21 ; millimetres",
        "G90 ; absolute positioning",
        "M82 ; absolute extrusion",
        "M140 S60",
        "M104 S210",
        "M190 S60",
        "M109 S210",
        "G28 ; home all axes",
        "G92 E0",
        "G1 Z5 F600",
        "M106 S255",
    ] {
        g.line(l);
    }

    // A: full circle, I/J form, clockwise (start == end).
    g.line("; A: full circle, I/J, clockwise");
    g.line(";TYPE:External perimeter");
    g.line("G0 X55 Y40 F6000");
    g.line("G0 Z0.2 F600");
    let e = g.e(TAU * 15.0);
    g.line(&format!("G2 X55 Y40 I-15 J0 E{e} F1200"));
    // B: concentric counter-clockwise circle.
    g.line("; B: full circle, I/J, counter-clockwise");
    g.line("G1 X52 Y40 F6000");
    let e = g.e(TAU * 12.0);
    g.line(&format!("G3 X52 Y40 I-12 J0 E{e} F1200"));

    // C: S-curve of half circles in R form.
    g.line("; C: S-curve of half circles, R form");
    g.line(";TYPE:Perimeter");
    g.line("G0 X80 Y30 F6000");
    for k in 0..6 {
        let x = 80.0 + 10.0 * (k + 1) as f64;
        let e = g.e(PI * 5.0);
        let cmd = if k % 2 == 0 { "G2" } else { "G3" };
        g.line(&format!("{cmd} X{} Y30 R5 E{e} F1500", trimmed(x, 3)));
    }

    // D: three quarters of a circle with a negative R (> 180 degrees).
    g.line("; D: 270 degree arc, negative R");
    g.line(";TYPE:Overhang perimeter");
    g.line("G0 X160 Y30 F6000");
    let e = g.e(0.75 * TAU * 15.0);
    g.line(&format!("G2 X175 Y45 R-15 E{e} F1200"));

    // Travel arc to the next section (no extrusion).
    g.line("; travel arc");
    g.line("G3 X175 Y75 I0 J15 F6000");

    // E: spiral of quarter arcs with growing radius around (130, 100).
    g.line("; E: quarter-arc spiral, I/J with short radial steps");
    g.line(";TYPE:Internal infill");
    let (scx, scy) = (130.0, 100.0);
    let mut r: f64 = 2.0;
    g.line(&format!(
        "G0 X{} Y{} F6000",
        trimmed(scx + r, 3),
        trimmed(scy, 3)
    ));
    let mut angle: f64 = 0.0;
    for _ in 0..16 {
        let (sx, sy) = (scx + r * angle.cos(), scy + r * angle.sin());
        let next = angle + PI / 2.0;
        let (ex, ey) = (scx + r * next.cos(), scy + r * next.sin());
        let e = g.e(PI / 2.0 * r);
        g.line(&format!(
            "G3 X{} Y{} I{} J{} E{e} F1500",
            trimmed(ex, 3),
            trimmed(ey, 3),
            trimmed(scx - sx, 3),
            trimmed(scy - sy, 3)
        ));
        angle = next;
        r += 1.0;
        let (ox, oy) = (scx + r * angle.cos(), scy + r * angle.sin());
        let e = g.e(1.0);
        g.line(&format!("G1 X{} Y{} E{e}", trimmed(ox, 3), trimmed(oy, 3)));
    }

    // F: relative-mode arcs (G91 makes E relative too).
    g.line("; F: relative positioning, bumps of half circles (E relative under G91)");
    g.line(";TYPE:Solid infill");
    g.line("G0 X20 Y80 F6000");
    g.line("G91");
    for k in 0..5 {
        let de = PI * 4.0 * E_PER_MM;
        g.e += de;
        let cmd = if k % 2 == 0 { "G2" } else { "G3" };
        g.line(&format!("{cmd} X8 Y0 I4 J0 E{} F1200", trimmed(de, 5)));
    }
    g.line("G90");
    g.line(&format!("G92 E{}", trimmed(g.e, 5)));

    // G: inch mode circle (radius 0.4 in = 10.16 mm).
    g.line("; G: inch units, full circle of radius 0.4 in around (3.6 in, 6 in)");
    g.line(";TYPE:Gap fill");
    g.line("G20");
    g.line("G0 X4 Y6 F236.2");
    g.e += TAU * 10.16 * E_PER_MM;
    g.line(&format!(
        "G2 X4 Y6 I-0.4 J0 E{} F47.2",
        trimmed(g.e / 25.4, 7)
    ));
    g.line("G21");

    // Tube: stacked full circles at increasing Z (planar layers).
    g.line("; Tube: full circles stacked 0.2 mm apart");
    g.line(";TYPE:External perimeter");
    g.line("G0 X70 Y160 F6000");
    for layer in 1..=10 {
        let z = layer as f64 * 0.2;
        g.line(&format!("G1 Z{} F600", trimmed(z, 3)));
        let e = g.e(TAU * 10.0);
        g.line(&format!("G2 X70 Y160 I-10 J0 E{e} F1500"));
    }
    // Helix: one command, 50 turns (P49) rising 10 mm.
    g.line("; Helix: G2 with Z and P49 (50 turns), continuing the tube as a spiral");
    let helix = ((50.0 * TAU * 10.0f64).powi(2) + 100.0).sqrt();
    let e = g.e(helix);
    g.line(&format!("G2 X70 Y160 I-10 J0 Z12 P49 E{e} F1500"));
    // R-form helix half turns.
    g.line("; Helix half turns in R form");
    for (k, x) in [50.0, 70.0].iter().enumerate() {
        let z = 12.0 + 0.1 * (k + 1) as f64;
        let e = g.e(((PI * 10.0f64).powi(2) + 0.01).sqrt());
        g.line(&format!("G2 X{x} Y160 Z{} R10 E{e} F1500", trimmed(z, 3)));
    }

    let retract = format!("G1 E{} F2400", trimmed(g.e - 2.0, 5));
    for l in [
        "; End",
        retract.as_str(),
        "G91",
        "G1 Z10 F600",
        "G90",
        "G0 X0 Y200 F6000",
        "M104 S0",
        "M140 S0",
        "M107",
        "M84",
    ] {
        g.line(l);
    }
    g.out
}
