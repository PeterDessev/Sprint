//! Test helper: render a ratatui [`Buffer`] to a PNG, the way a terminal
//! with the Hack font would show it, so screens can be reviewed as images.
//!
//! Text is rasterised from the Hack outlines (bundled with egui) by a small
//! coverage rasteriser; box drawing, block elements and Braille are drawn
//! geometrically so lines join across cells like in a real terminal.
//!
//! Images are written only when `SPRINT_TUI_SNAPSHOTS` is set: to that
//! directory, or to `crates/sprint-tui/tests/snapshots` when it is `1`.

use std::collections::HashMap;
use std::path::PathBuf;

use image::{Rgb, RgbImage};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};
use unicode_width::UnicodeWidthStr;

/// Cell size in pixels.
const CELL_W: usize = 10;
const CELL_H: usize = 20;
/// Font size in pixels (Hack's advance is 0.6 em: about one cell).
const FONT_PX: f32 = 16.5;

/// Default colors of the terminal (Windows Terminal "Campbell").
const DEFAULT_FG: [u8; 3] = [204, 204, 204];
const DEFAULT_BG: [u8; 3] = [12, 12, 12];

/// The 16 ANSI colors (Campbell).
const ANSI: [[u8; 3]; 16] = [
    [12, 12, 12],
    [197, 15, 31],
    [19, 161, 14],
    [193, 156, 0],
    [0, 55, 218],
    [136, 23, 152],
    [58, 150, 221],
    [204, 204, 204],
    [118, 118, 118],
    [231, 72, 86],
    [22, 198, 12],
    [249, 241, 165],
    [59, 120, 255],
    [180, 0, 158],
    [97, 214, 214],
    [242, 242, 242],
];

fn indexed(i: u8) -> [u8; 3] {
    match i {
        0..=15 => ANSI[usize::from(i)],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [level(i / 36), level(i / 6 % 6), level(i % 6)]
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    }
}

fn rgb(color: Color, default: [u8; 3]) -> [u8; 3] {
    match color {
        Color::Reset => default,
        Color::Black => ANSI[0],
        Color::Red => ANSI[1],
        Color::Green => ANSI[2],
        Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5],
        Color::Cyan => ANSI[6],
        Color::Gray => ANSI[7],
        Color::DarkGray => ANSI[8],
        Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10],
        Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12],
        Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14],
        Color::White => ANSI[15],
        Color::Indexed(i) => indexed(i),
        Color::Rgb(r, g, b) => [r, g, b],
    }
}

fn blend(bg: [u8; 3], fg: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    [mix(bg[0], fg[0]), mix(bg[1], fg[1]), mix(bg[2], fg[2])]
}

/// A coverage mask of `w` x `h` pixels, values 0..=1.
#[derive(Clone)]
struct Mask {
    w: usize,
    h: usize,
    a: Vec<f32>,
}

impl Mask {
    fn new(w: usize, h: usize) -> Mask {
        Mask {
            w,
            h,
            a: vec![0.0; w * h],
        }
    }

    fn add(&mut self, x: usize, y: usize, v: f32) {
        if x < self.w && y < self.h {
            let p = &mut self.a[y * self.w + x];
            *p = (*p + v).min(1.0);
        }
    }

    /// Fill the rectangle `[x0, x1) x [y0, y1)` (pixel coordinates,
    /// fractional edges anti-aliased).
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, alpha: f32) {
        for y in 0..self.h {
            let cy = (y1.min(y as f32 + 1.0) - y0.max(y as f32)).clamp(0.0, 1.0);
            if cy == 0.0 {
                continue;
            }
            for x in 0..self.w {
                let cx = (x1.min(x as f32 + 1.0) - x0.max(x as f32)).clamp(0.0, 1.0);
                if cx > 0.0 {
                    self.add(x, y, cx * cy * alpha);
                }
            }
        }
    }

    /// A filled disc (anti-aliased by 4x4 supersampling).
    fn disc(&mut self, cx: f32, cy: f32, r: f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = x as f32 + (sx as f32 + 0.5) / 4.0 - cx;
                        let py = y as f32 + (sy as f32 + 0.5) / 4.0 - cy;
                        if px * px + py * py <= r * r {
                            hits += 1;
                        }
                    }
                }
                if hits > 0 {
                    self.add(x, y, hits as f32 / 16.0);
                }
            }
        }
    }

    /// A quarter ring around (cx, cy) with radius `r` and thickness `t`,
    /// in the quadrant given by the signs (sx, sy).
    fn arc(&mut self, cx: f32, cy: f32, r: f32, t: f32, sx: f32, sy: f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let mut hits = 0;
                for ky in 0..4 {
                    for kx in 0..4 {
                        let px = x as f32 + (kx as f32 + 0.5) / 4.0 - cx;
                        let py = y as f32 + (ky as f32 + 0.5) / 4.0 - cy;
                        if px * sx < 0.0 || py * sy < 0.0 {
                            continue;
                        }
                        let d = (px * px + py * py).sqrt();
                        if (d - r).abs() <= t / 2.0 {
                            hits += 1;
                        }
                    }
                }
                if hits > 0 {
                    self.add(x, y, hits as f32 / 16.0);
                }
            }
        }
    }

    /// Faux bold: smear one pixel to the right.
    fn embolden(&mut self) {
        let mut out = self.a.clone();
        for y in 0..self.h {
            for x in 1..self.w {
                let i = y * self.w + x;
                out[i] = out[i].max(self.a[i - 1]);
            }
        }
        self.a = out;
    }
}

/// Signed-area accumulation rasteriser (as in font-rs).
struct Raster {
    w: usize,
    h: usize,
    acc: Vec<f32>,
}

impl Raster {
    fn new(w: usize, h: usize) -> Raster {
        Raster {
            w,
            h,
            acc: vec![0.0; w * h + 4],
        }
    }

    fn clamp(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (
            x.clamp(0.0, self.w as f32 - 1.001),
            y.clamp(0.0, self.h as f32),
        )
    }

    fn line(&mut self, p0: (f32, f32), p1: (f32, f32)) {
        let (p0, p1) = (self.clamp(p0), self.clamp(p1));
        if (p0.1 - p1.1).abs() <= f32::EPSILON {
            return;
        }
        let (dir, p0, p1) = if p0.1 < p1.1 {
            (1.0, p0, p1)
        } else {
            (-1.0, p1, p0)
        };
        let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
        let mut x = p0.0;
        let y_start = p0.1 as usize;
        let y_end = self.h.min(p1.1.ceil() as usize);
        for y in y_start..y_end {
            let line_start = y * self.w;
            let dy = ((y + 1) as f32).min(p1.1) - (y as f32).max(p0.1);
            let x_next = x + dxdy * dy;
            let d = dy * dir;
            let (x0, x1) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let x0_floor = x0.floor();
            let x0i = x0_floor as usize;
            let x1_ceil = x1.ceil();
            let x1i = x1_ceil as usize;
            if x1i <= x0i + 1 {
                let xmf = 0.5 * (x + x_next) - x0_floor;
                self.acc[line_start + x0i] += d - d * xmf;
                self.acc[line_start + x0i + 1] += d * xmf;
            } else {
                let s = (x1 - x0).recip();
                let x0f = x0 - x0_floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1_ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.acc[line_start + x0i] += d * a0;
                if x1i == x0i + 2 {
                    self.acc[line_start + x0i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.acc[line_start + x0i + 1] += d * (a1 - a0);
                    for xi in x0i + 2..x1i - 1 {
                        self.acc[line_start + xi] += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.acc[line_start + x1i - 1] += d * (1.0 - a2 - am);
                }
                self.acc[line_start + x1i] += d * am;
            }
            x = x_next;
        }
    }

    fn mask(&self) -> Mask {
        let mut mask = Mask::new(self.w, self.h);
        let mut sum = 0.0f32;
        for (i, out) in mask.a.iter_mut().enumerate() {
            sum += self.acc[i];
            *out = sum.abs().min(1.0);
        }
        mask
    }
}

/// Feeds glyph outlines into a [`Raster`], flipping Y and offsetting to
/// the cell's baseline.
struct Pen<'a> {
    raster: &'a mut Raster,
    origin: (f32, f32),
    start: (f32, f32),
    current: (f32, f32),
}

impl Pen<'_> {
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (self.origin.0 + x, self.origin.1 - y)
    }
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.start = p;
        self.current = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.raster.line(self.current, p);
        self.current = p;
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let c = self.map(cx0, cy0);
        let p = self.map(x, y);
        let s = self.current;
        const STEPS: usize = 6;
        let mut prev = s;
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            let u = 1.0 - t;
            let q = (
                u * u * s.0 + 2.0 * u * t * c.0 + t * t * p.0,
                u * u * s.1 + 2.0 * u * t * c.1 + t * t * p.1,
            );
            self.raster.line(prev, q);
            prev = q;
        }
        self.current = p;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let c0 = self.map(cx0, cy0);
        let c1 = self.map(cx1, cy1);
        let p = self.map(x, y);
        let s = self.current;
        const STEPS: usize = 10;
        let mut prev = s;
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            let u = 1.0 - t;
            let q = (
                u * u * u * s.0 + 3.0 * u * u * t * c0.0 + 3.0 * u * t * t * c1.0 + t * t * t * p.0,
                u * u * u * s.1 + 3.0 * u * u * t * c0.1 + 3.0 * u * t * t * c1.1 + t * t * t * p.1,
            );
            self.raster.line(prev, q);
            prev = q;
        }
        self.current = p;
    }

    fn close(&mut self) {
        if self.current != self.start {
            self.raster.line(self.current, self.start);
        }
        self.current = self.start;
    }
}

/// Line weights of a box-drawing character: (up, down, left, right), each
/// 0 (none), 1 (light), 2 (heavy) or 3 (double).
fn box_arms(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' => [0, 0, 1, 1],
        '━' => [0, 0, 2, 2],
        '│' => [1, 1, 0, 0],
        '┃' => [2, 2, 0, 0],
        '┌' | '╭' => [0, 1, 0, 1],
        '┐' | '╮' => [0, 1, 1, 0],
        '└' | '╰' => [1, 0, 0, 1],
        '┘' | '╯' => [1, 0, 1, 0],
        '├' => [1, 1, 0, 1],
        '┤' => [1, 1, 1, 0],
        '┬' => [0, 1, 1, 1],
        '┴' => [1, 0, 1, 1],
        '┼' => [1, 1, 1, 1],
        '┏' => [0, 2, 0, 2],
        '┓' => [0, 2, 2, 0],
        '┗' => [2, 0, 0, 2],
        '┛' => [2, 0, 2, 0],
        '┣' => [2, 2, 0, 2],
        '┫' => [2, 2, 2, 0],
        '┳' => [0, 2, 2, 2],
        '┻' => [2, 0, 2, 2],
        '╋' => [2, 2, 2, 2],
        '═' => [0, 0, 3, 3],
        '║' => [3, 3, 0, 0],
        '╔' => [0, 3, 0, 3],
        '╗' => [0, 3, 3, 0],
        '╚' => [3, 0, 0, 3],
        '╝' => [3, 0, 3, 0],
        '╠' => [3, 3, 0, 3],
        '╣' => [3, 3, 3, 0],
        '╦' => [0, 3, 3, 3],
        '╩' => [3, 0, 3, 3],
        '╬' => [3, 3, 3, 3],
        '╴' => [0, 0, 1, 0],
        '╵' => [1, 0, 0, 0],
        '╶' => [0, 0, 0, 1],
        '╷' => [0, 1, 0, 0],
        _ => return None,
    })
}

/// Draw a box-drawing, block or Braille character geometrically; `None`
/// for anything else.
fn geometric(c: char, w: usize, h: usize) -> Option<Mask> {
    let (wf, hf) = (w as f32, h as f32);
    let mut m = Mask::new(w, h);
    // Lines run through the centre of the middle pixel so they stay crisp.
    let (cx, cy) = ((wf / 2.0).floor() + 0.5, (hf / 2.0).floor() + 0.5);
    let code = u32::from(c);
    if let Some([up, down, left, right]) = box_arms(c) {
        let rounded = matches!(c, '╭' | '╮' | '╯' | '╰');
        if rounded {
            let r = (wf / 2.0).min(hf / 2.0).floor();
            let (sx, sy, ox, oy) = match c {
                '╭' => (1.0, 1.0, cx + r, cy + r),
                '╮' => (-1.0, 1.0, cx - r, cy + r),
                '╯' => (-1.0, -1.0, cx - r, cy - r),
                _ => (1.0, -1.0, cx + r, cy - r),
            };
            m.arc(ox, oy, r, 1.1, -sx, -sy);
            // Straight continuations to the cell edges.
            if down > 0 {
                m.rect(cx - 0.5, cy + r, cx + 0.5, hf, 1.0);
            }
            if up > 0 {
                m.rect(cx - 0.5, 0.0, cx + 0.5, cy - r, 1.0);
            }
            if right > 0 {
                m.rect(cx + r, cy - 0.5, wf, cy + 0.5, 1.0);
            }
            if left > 0 {
                m.rect(0.0, cy - 0.5, cx - r, cy + 0.5, 1.0);
            }
            return Some(m);
        }
        // (low, high) offsets of a light, heavy or double line around the
        // centre line.
        let spans = |k: u8| -> Vec<(f32, f32)> {
            match k {
                0 => Vec::new(),
                2 => vec![(-1.5, 0.5)],
                3 => vec![(-2.5, -1.5), (1.5, 2.5)],
                _ => vec![(-0.5, 0.5)],
            }
        };
        let vertical = |m: &mut Mask, k: u8, y0: f32, y1: f32| {
            for (a, b) in spans(k) {
                m.rect(cx + a, y0, cx + b, y1, 1.0);
            }
        };
        let horizontal = |m: &mut Mask, k: u8, x0: f32, x1: f32| {
            for (a, b) in spans(k) {
                m.rect(x0, cy + a, x1, cy + b, 1.0);
            }
        };
        // Arms reach past the centre by the half-width of the crossing
        // line, so corners and junctions close.
        let ext = |k: u8| match k {
            3 => 2.5,
            2 => 1.5,
            _ => 0.5,
        };
        let h_ext = ext(up.max(down));
        let v_ext = ext(left.max(right));
        vertical(&mut m, up, 0.0, cy + v_ext);
        vertical(&mut m, down, cy - v_ext, hf);
        horizontal(&mut m, left, 0.0, cx + h_ext);
        horizontal(&mut m, right, cx - h_ext, wf);
        return Some(m);
    }
    match code {
        // Dashed lines.
        0x2504 | 0x2508 | 0x254C => {
            let n = if code == 0x254C {
                2
            } else if code == 0x2504 {
                3
            } else {
                4
            };
            let seg = wf / n as f32;
            for i in 0..n {
                let x0 = i as f32 * seg;
                m.rect(x0 + seg * 0.15, cy - 0.5, x0 + seg * 0.85, cy + 0.5, 1.0);
            }
        }
        0x2506 | 0x250A | 0x254E => {
            let n = if code == 0x254E {
                2
            } else if code == 0x2506 {
                3
            } else {
                4
            };
            let seg = hf / n as f32;
            for i in 0..n {
                let y0 = i as f32 * seg;
                m.rect(cx - 0.5, y0 + seg * 0.15, cx + 0.5, y0 + seg * 0.85, 1.0);
            }
        }
        // ▀ upper half.
        0x2580 => m.rect(0.0, 0.0, wf, hf / 2.0, 1.0),
        // ▁..█ lower eighths.
        0x2581..=0x2588 => {
            let eighths = (code - 0x2580) as f32;
            m.rect(0.0, hf - hf * eighths / 8.0, wf, hf, 1.0);
        }
        // ▉..▏ left eighths (7/8 down to 1/8).
        0x2589..=0x258F => {
            let eighths = (0x2590 - code) as f32;
            m.rect(0.0, 0.0, wf * eighths / 8.0, hf, 1.0);
        }
        // ▐ right half.
        0x2590 => m.rect(wf / 2.0, 0.0, wf, hf, 1.0),
        // ░ ▒ ▓ shades.
        0x2591..=0x2593 => {
            let alpha = (code - 0x2590) as f32 * 0.25;
            m.rect(0.0, 0.0, wf, hf, alpha);
        }
        // ▔ upper eighth, ▕ right eighth.
        0x2594 => m.rect(0.0, 0.0, wf, hf / 8.0, 1.0),
        0x2595 => m.rect(wf * 7.0 / 8.0, 0.0, wf, hf, 1.0),
        // Quadrants.
        0x2596..=0x259F => {
            // Bits: upper-left, upper-right, lower-left, lower-right.
            let bits: u8 = match code {
                0x2596 => 0b0010,
                0x2597 => 0b0001,
                0x2598 => 0b1000,
                0x2599 => 0b1011,
                0x259A => 0b1001,
                0x259B => 0b1110,
                0x259C => 0b1101,
                0x259D => 0b0100,
                0x259E => 0b0110,
                _ => 0b0111,
            };
            if bits & 0b1000 != 0 {
                m.rect(0.0, 0.0, cx, cy, 1.0);
            }
            if bits & 0b0100 != 0 {
                m.rect(cx, 0.0, wf, cy, 1.0);
            }
            if bits & 0b0010 != 0 {
                m.rect(0.0, cy, cx, hf, 1.0);
            }
            if bits & 0b0001 != 0 {
                m.rect(cx, cy, wf, hf, 1.0);
            }
        }
        // Braille: dots 1-2-3-7 in the left column, 4-5-6-8 in the right.
        0x2800..=0x28FF => {
            let bits = code - 0x2800;
            let dots = [
                (0, 0),
                (0, 1),
                (0, 2),
                (1, 0),
                (1, 1),
                (1, 2),
                (0, 3),
                (1, 3),
            ];
            for (i, (col, row)) in dots.iter().enumerate() {
                if bits & (1 << i) != 0 {
                    let x = (*col as f32 + 0.5) * wf / 2.0;
                    let y = (*row as f32 + 0.5) * hf / 4.0;
                    m.disc(x, y, 1.35);
                }
            }
        }
        _ => return None,
    }
    Some(m)
}

/// Renders buffers to images, caching glyph masks.
pub(crate) struct Renderer {
    font: FontRef<'static>,
    baseline: f32,
    advance: f32,
    cache: HashMap<(char, bool, usize), Option<Mask>>,
}

impl Renderer {
    pub(crate) fn new() -> Renderer {
        let font = FontRef::new(epaint_default_fonts::HACK_REGULAR).expect("bundled Hack font");
        let metrics = font.metrics(Size::new(FONT_PX), LocationRef::default());
        let line = metrics.ascent - metrics.descent;
        let baseline = ((CELL_H as f32 - line) / 2.0 + metrics.ascent).round();
        let advance = font
            .glyph_metrics(Size::new(FONT_PX), LocationRef::default())
            .advance_width(font.charmap().map('M').unwrap_or_default())
            .unwrap_or(CELL_W as f32);
        Renderer {
            font,
            baseline,
            advance,
            cache: HashMap::new(),
        }
    }

    /// The coverage mask of `c` drawn `cols` cells wide.
    fn glyph(&mut self, c: char, bold: bool, cols: usize) -> Option<Mask> {
        let key = (c, bold, cols);
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone();
        }
        let w = CELL_W * cols;
        let mask = geometric(c, w, CELL_H).or_else(|| {
            let gid = self.font.charmap().map(c)?;
            let glyph = self.font.outline_glyphs().get(gid)?;
            let mut raster = Raster::new(w, CELL_H);
            let mut pen = Pen {
                raster: &mut raster,
                origin: ((w as f32 - self.advance) / 2.0, self.baseline),
                start: (0.0, 0.0),
                current: (0.0, 0.0),
            };
            glyph
                .draw(
                    DrawSettings::unhinted(Size::new(FONT_PX), LocationRef::default()),
                    &mut pen,
                )
                .ok()?;
            let mut mask = raster.mask();
            if bold {
                mask.embolden();
            }
            Some(mask)
        });
        // Unknown characters show as a small hollow box.
        let mask = mask.or_else(|| {
            if c == ' ' || c.is_control() {
                return None;
            }
            let mut m = Mask::new(w, CELL_H);
            let (x0, y0, x1, y1) = (2.0, 5.0, w as f32 - 2.0, CELL_H as f32 - 4.0);
            m.rect(x0, y0, x1, y0 + 1.0, 1.0);
            m.rect(x0, y1 - 1.0, x1, y1, 1.0);
            m.rect(x0, y0, x0 + 1.0, y1, 1.0);
            m.rect(x1 - 1.0, y0, x1, y1, 1.0);
            Some(m)
        });
        self.cache.insert(key, mask.clone());
        mask
    }

    /// The image of a buffer.
    pub(crate) fn render(&mut self, buf: &Buffer) -> RgbImage {
        let area = buf.area;
        let (cols, rows) = (usize::from(area.width), usize::from(area.height));
        let mut img = RgbImage::new((cols * CELL_W) as u32, (rows * CELL_H) as u32);
        // Backgrounds first, so wide glyphs can paint over their second
        // cell.
        let colors = |x: u16, y: u16| {
            let cell = &buf[(area.x + x, area.y + y)];
            let mut fg = rgb(cell.fg, DEFAULT_FG);
            let mut bg = rgb(cell.bg, DEFAULT_BG);
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.modifier.contains(Modifier::DIM) {
                fg = blend(bg, fg, 0.55);
            }
            (fg, bg)
        };
        for y in 0..area.height {
            for x in 0..area.width {
                let (_, bg) = colors(x, y);
                let (px, py) = (usize::from(x) * CELL_W, usize::from(y) * CELL_H);
                for dy in 0..CELL_H {
                    for dx in 0..CELL_W {
                        img.put_pixel((px + dx) as u32, (py + dy) as u32, Rgb(bg));
                    }
                }
            }
        }
        for y in 0..area.height {
            let mut skip = 0;
            for x in 0..area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let cell = &buf[(area.x + x, area.y + y)];
                let symbol = cell.symbol();
                let cols_wide = UnicodeWidthStr::width(symbol).clamp(1, 2);
                skip = cols_wide - 1;
                if cell.modifier.contains(Modifier::HIDDEN) {
                    continue;
                }
                let Some(c) = symbol.chars().next() else {
                    continue;
                };
                let (fg, _) = colors(x, y);
                let (px, py) = (usize::from(x) * CELL_W, usize::from(y) * CELL_H);
                let bold = cell.modifier.contains(Modifier::BOLD);
                if let Some(mask) = self.glyph(c, bold, cols_wide) {
                    for dy in 0..mask.h {
                        for dx in 0..mask.w {
                            let a = mask.a[dy * mask.w + dx];
                            let (ix, iy) = ((px + dx) as u32, (py + dy) as u32);
                            if a <= 0.0 || ix >= img.width() || iy >= img.height() {
                                continue;
                            }
                            let under = img.get_pixel(ix, iy).0;
                            img.put_pixel(ix, iy, Rgb(blend(under, fg, a)));
                        }
                    }
                }
                if cell.modifier.contains(Modifier::UNDERLINED) {
                    let uy = (py + CELL_H - 3) as u32;
                    for dx in 0..CELL_W * cols_wide {
                        let ix = (px + dx) as u32;
                        if ix < img.width() {
                            img.put_pixel(ix, uy, Rgb(fg));
                        }
                    }
                }
            }
        }
        img
    }
}

/// Where snapshots go, if they are wanted.
fn snapshot_dir() -> Option<PathBuf> {
    let value = std::env::var_os("SPRINT_TUI_SNAPSHOTS")?;
    if value.is_empty() {
        return None;
    }
    if value == "1" {
        return Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots"));
    }
    Some(PathBuf::from(value))
}

/// Write `buf` as `<name>.png` when snapshots are enabled.
pub(crate) fn save(buf: &Buffer, name: &str) {
    let Some(dir) = snapshot_dir() else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("snapshot directory");
    let img = Renderer::new().render(buf);
    let path = dir.join(format!("{name}.png"));
    img.save(&path)
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn cells_get_their_colors_and_glyphs() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        buf.set_string(0, 0, "█", Style::default().fg(Color::Rgb(255, 0, 0)));
        buf.set_string(1, 0, "A", Style::default().fg(Color::White).bg(Color::Blue));
        buf.set_string(2, 0, "─", Style::default());
        buf.set_string(3, 0, "⣿", Style::default().fg(Color::Green));
        buf.set_string(0, 1, "日", Style::default());
        let img = Renderer::new().render(&buf);
        assert_eq!(img.dimensions(), (40, 40));
        // A full block covers its cell.
        assert_eq!(img.get_pixel(5, 10).0, [255, 0, 0]);
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0]);
        // Background of the second cell; the letter leaves corners alone.
        assert_eq!(img.get_pixel(10, 0).0, ANSI[4]);
        let letter: u32 = (0..CELL_H as u32)
            .flat_map(|y| (10..20).map(move |x| (x, y)))
            .filter(|&(x, y)| img.get_pixel(x, y).0 != ANSI[4])
            .count() as u32;
        assert!(letter > 10, "the A is drawn");
        // The horizontal line runs through the middle row, edge to edge.
        assert_eq!(img.get_pixel(20, 10).0, DEFAULT_FG);
        assert_eq!(img.get_pixel(29, 10).0, DEFAULT_FG);
        assert_eq!(img.get_pixel(25, 2).0, DEFAULT_BG);
        // All eight Braille dots.
        assert_ne!(img.get_pixel(32, 2).0, DEFAULT_BG);
        assert_ne!(img.get_pixel(37, 17).0, DEFAULT_BG);
        // A wide character spans two cells.
        let wide = (0..20u32)
            .flat_map(|x| (20..40u32).map(move |y| (x, y)))
            .filter(|&(x, y)| x >= 10 && img.get_pixel(x, y).0 != DEFAULT_BG)
            .count();
        assert!(wide > 0, "the second half of 日 is drawn");
    }

    /// A screen of widgets, for checking the renderer by eye.
    #[test]
    fn widget_screen_snapshot() {
        use crate::widgets::{
            ConsoleLine, ConsoleView, HeaterGauge, HeaterKind, JobProgress, JobTone, KeyHints,
            LayerCanvas,
        };
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::{Constraint, Layout};
        use ratatui::widgets::{Block, BorderType};
        use sprint_host::sprint_gcode::{AnalyzeOptions, GcodeFile, analyze_simple};

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/calibration-cube.gcode"
        );
        let file = GcodeFile::load(path).expect("sample");
        let toolpath = analyze_simple(&file, &AnalyzeOptions::default());
        let theme = crate::Theme::dark();
        let lines = vec![
            ConsoleLine::sent("M105"),
            ConsoleLine::received("ok T:205.3 /210.0 B:60.0 /60.0 @:64 B@:30"),
            ConsoleLine::warning("Resend: 42"),
            ConsoleLine::error("Error:Printer halted. kill() called!"),
        ];
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal
            .draw(|f| {
                f.render_widget(Block::new().style(theme.base()), f.area());
                let [top, canvas, console, hints] = Layout::vertical([
                    Constraint::Length(6),
                    Constraint::Fill(1),
                    Constraint::Length(6),
                    Constraint::Length(1),
                ])
                .areas(f.area());
                let block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(theme.border(false))
                    .title(" Heaters ");
                let inner = block.inner(top);
                f.render_widget(block, top);
                let [a, b, c] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);
                f.render_widget(
                    HeaterGauge::new(HeaterKind::Hotend(0), 205.3, Some(210.0))
                        .power(Some(0.64))
                        .theme(&theme),
                    a,
                );
                f.render_widget(
                    HeaterGauge::new(HeaterKind::Bed, 60.0, Some(60.0))
                        .power(Some(0.3))
                        .theme(&theme),
                    b,
                );
                f.render_widget(
                    JobProgress::new("calibration-cube.gcode", 0.42)
                        .status("Printing", JobTone::Active)
                        .theme(&theme),
                    c,
                );
                let progress = toolpath.layers.get(20).map(|l| l.lines.start + 300);
                f.render_widget(
                    LayerCanvas::new(&toolpath, 20, progress, [220.0, 220.0])
                        .fit(crate::LayerFit::Object)
                        .theme(&theme)
                        .block(Block::bordered().border_style(theme.border(true))),
                    canvas,
                );
                f.render_widget(
                    ConsoleView::new(&lines)
                        .theme(&theme)
                        .block(Block::bordered().title(" Console ")),
                    console,
                );
                f.render_widget(
                    KeyHints::new([("F1", "Help"), ("F5", "Print"), ("F10", "Quit")]).theme(&theme),
                    hints,
                );
            })
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let img = Renderer::new().render(&buf);
        assert_eq!(img.dimensions(), (800, 480));
        save(&buf, "renderer-selftest");
    }

    #[test]
    fn palettes() {
        assert_eq!(indexed(196), [255, 0, 0]);
        assert_eq!(indexed(16), [0, 0, 0]);
        assert_eq!(indexed(232), [8, 8, 8]);
        assert_eq!(rgb(Color::Reset, DEFAULT_FG), DEFAULT_FG);
        assert_eq!(blend([0, 0, 0], [200, 100, 50], 0.5), [100, 50, 25]);
    }
}
