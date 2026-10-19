//! The Sprint application icon, drawn procedurally: an orange rounded
//! square with a white hotend nozzle laying down a wavy bead.
//!
//! Shapes are signed-distance functions sampled once per pixel with one
//! pixel of anti-aliasing, so the icon is crisp at any size.

/// The icon as RGBA8 pixels, `size` x `size` (clamped to 16..=1024).
pub fn icon_rgba(size: u32) -> (u32, Vec<u8>) {
    let size = size.clamp(16, 1024);
    let s = size as f32 / 256.0;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            // Work in a 256-unit design space.
            let p = [(x as f32 + 0.5) / s, (y as f32 + 0.5) / s];
            let aa = 1.0 / s;
            out.extend_from_slice(&shade(p, aa));
        }
    }
    (size, out)
}

/// The icon for the window (`eframe` / winit).
pub fn icon_data() -> egui::IconData {
    let (size, rgba) = icon_rgba(256);
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

/// The icon as an egui image (About dialog).
pub fn color_image(size: u32) -> egui::ColorImage {
    let (size, rgba) = icon_rgba(size);
    egui::ColorImage::from_rgba_unmultiplied([size as usize, size as usize], &rgba)
}

/// Coverage of a shape at signed distance `d` with an `aa`-wide edge.
fn coverage(d: f32, aa: f32) -> f32 {
    (0.5 - d / aa).clamp(0.0, 1.0)
}

/// One pixel of the icon (straight alpha).
fn shade(p: [f32; 2], aa: f32) -> [u8; 4] {
    // Background tile: rounded square with a vertical gradient.
    let tile = sd_round_rect(p, [128.0, 128.0], [116.0, 116.0], 52.0);
    let tile_a = coverage(tile, aa);
    if tile_a <= 0.0 {
        return [0, 0, 0, 0];
    }
    let t = ((p[1] - 12.0) / 232.0).clamp(0.0, 1.0);
    let top = [255.0, 146.0, 66.0];
    let bottom = [232.0, 92.0, 10.0];
    let mut rgb = [
        top[0] + (bottom[0] - top[0]) * t,
        top[1] + (bottom[1] - top[1]) * t,
        top[2] + (bottom[2] - top[2]) * t,
    ];
    // A soft highlight along the top edge.
    let rim = coverage(sd_round_rect(p, [128.0, 128.0], [110.0, 110.0], 46.0), aa);
    let glow = (1.0 - rim) * 0.18 * (1.0 - t);
    for c in &mut rgb {
        *c += (255.0 - *c) * glow;
    }

    // White artwork: heat block, nozzle cone, tip and the printed bead.
    let block = sd_round_rect(p, [128.0, 74.0], [44.0, 26.0], 9.0);
    let cone = sd_polygon(
        p,
        &[[96.0, 98.0], [160.0, 98.0], [138.0, 146.0], [118.0, 146.0]],
    );
    let tip = sd_round_rect(p, [128.0, 152.0], [10.0, 8.0], 3.0);
    let bead = sd_wave(p);
    let art = block.min(cone).min(tip).min(bead);
    let art_a = coverage(art, aa);
    // A thin darker groove across the heat block reads as a fin.
    let fin = sd_round_rect(p, [128.0, 74.0], [34.0, 3.0], 3.0);
    let fin_a = coverage(fin, aa) * 0.55;

    let white = [255.0, 250.0, 244.0];
    for (c, w) in rgb.iter_mut().zip(white) {
        *c += (w - *c) * art_a;
    }
    let groove = [214.0, 84.0, 12.0];
    for (c, g) in rgb.iter_mut().zip(groove) {
        *c += (g - *c) * fin_a;
    }
    [
        rgb[0].round() as u8,
        rgb[1].round() as u8,
        rgb[2].round() as u8,
        (tile_a * 255.0).round() as u8,
    ]
}

/// Signed distance to a rounded rectangle (centre, half size, radius).
fn sd_round_rect(p: [f32; 2], center: [f32; 2], half: [f32; 2], radius: f32) -> f32 {
    let qx = (p[0] - center[0]).abs() - (half[0] - radius);
    let qy = (p[1] - center[1]).abs() - (half[1] - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

/// Signed distance to a convex or concave polygon (even-odd rule).
fn sd_polygon(p: [f32; 2], v: &[[f32; 2]]) -> f32 {
    let n = v.len();
    if n < 3 {
        return f32::INFINITY;
    }
    let sub = |a: [f32; 2], b: [f32; 2]| [a[0] - b[0], a[1] - b[1]];
    let dot = |a: [f32; 2], b: [f32; 2]| a[0] * b[0] + a[1] * b[1];
    let mut d = dot(sub(p, v[0]), sub(p, v[0]));
    let mut sign = 1.0;
    let mut j = n - 1;
    for i in 0..n {
        let e = sub(v[j], v[i]);
        let w = sub(p, v[i]);
        let h = (dot(w, e) / dot(e, e).max(1e-6)).clamp(0.0, 1.0);
        let b = [w[0] - e[0] * h, w[1] - e[1] * h];
        d = d.min(dot(b, b));
        let c1 = p[1] >= v[i][1];
        let c2 = p[1] < v[j][1];
        let c3 = e[0] * w[1] > e[1] * w[0];
        if (c1 && c2 && c3) || (!c1 && !c2 && !c3) {
            sign = -sign;
        }
        j = i;
    }
    sign * d.sqrt()
}

/// Distance to the bead: a sine wave below the tip, 7 units thick, with
/// round ends.
fn sd_wave(p: [f32; 2]) -> f32 {
    const X0: f32 = 52.0;
    const X1: f32 = 204.0;
    const STEPS: usize = 48;
    let wave = |x: f32| {
        let t = (x - X0) / (X1 - X0);
        190.0 + 13.0 * (t * std::f32::consts::TAU * 1.5 + 0.6).sin()
    };
    // Quick reject far from the band.
    if p[1] < 160.0 || p[1] > 222.0 || p[0] < X0 - 12.0 || p[0] > X1 + 12.0 {
        return 30.0;
    }
    let mut best = f32::INFINITY;
    let mut prev = [X0, wave(X0)];
    for i in 1..=STEPS {
        let x = X0 + (X1 - X0) * i as f32 / STEPS as f32;
        let next = [x, wave(x)];
        best = best.min(segment_distance(p, prev, next));
        prev = next;
    }
    best - 7.0
}

fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let pa = [p[0] - a[0], p[1] - a[1]];
    let ba = [b[0] - a[0], b[1] - a[1]];
    let h = ((pa[0] * ba[0] + pa[1] * ba[1]) / (ba[0] * ba[0] + ba[1] * ba[1]).max(1e-6))
        .clamp(0.0, 1.0);
    let dx = pa[0] - ba[0] * h;
    let dy = pa[1] - ba[1] * h;
    (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn icon_has_transparent_corners_an_orange_tile_and_white_art() {
        let (size, rgba) = icon_rgba(256);
        assert_eq!(rgba.len(), (size * size * 4) as usize);
        assert_eq!(pixel(&rgba, size, 0, 0)[3], 0);
        assert_eq!(pixel(&rgba, size, 255, 255)[3], 0);
        let tile = pixel(&rgba, size, 30, 128);
        assert_eq!(tile[3], 255);
        assert!(tile[0] > 200 && tile[1] < 160 && tile[2] < 90, "{tile:?}");
        let block = pixel(&rgba, size, 128, 64);
        assert!(block.iter().take(3).all(|&c| c > 235), "{block:?}");
    }

    #[test]
    fn sizes_are_clamped_and_data_matches() {
        assert_eq!(icon_rgba(0).0, 16);
        assert_eq!(icon_rgba(5000).0, 1024);
        let data = icon_data();
        assert_eq!(data.rgba.len(), (data.width * data.height * 4) as usize);
        assert_eq!(color_image(32).size, [32, 32]);
    }
}
