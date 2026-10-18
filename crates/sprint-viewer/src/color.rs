//! Colors, gradient ramps, scalar ranges and the legend model.
//!
//! Everything here is GPU-agnostic. The shader receives the same palettes and
//! ramp stops as uniforms, so the legend drawn by egui always matches what
//! the GPU renders.

use sprint_gcode::Feature;

use crate::options::ColorMode;

/// Default viewer background (sRGB).
pub const BACKGROUND: [u8; 3] = [24, 26, 31];

/// Gradient used by every scalar color mode, low to high (sRGB). Close to
/// the PrusaSlicer range palette: deep blue, teal, green, yellow, orange,
/// red.
pub const RAMP: [[u8; 3]; 11] = [
    [0x0B, 0x2C, 0x7A],
    [0x13, 0x59, 0x85],
    [0x1C, 0x88, 0x91],
    [0x04, 0xD6, 0x0F],
    [0xAA, 0xF2, 0x00],
    [0xFC, 0xF9, 0x03],
    [0xF5, 0xCE, 0x0A],
    [0xE3, 0x88, 0x20],
    [0xD1, 0x68, 0x30],
    [0xC2, 0x52, 0x3C],
    [0x94, 0x26, 0x16],
];

/// Colors for tools 0..8 (repeats for higher tool numbers), sRGB.
pub const TOOL_COLORS: [[u8; 3]; 8] = [
    [0xFF, 0x8A, 0x3D],
    [0x3D, 0x9C, 0xFF],
    [0x55, 0xD6, 0x6B],
    [0xE0, 0x5C, 0xD6],
    [0xF2, 0xD7, 0x3A],
    [0x3D, 0xD9, 0xD2],
    [0xF0, 0x4E, 0x4E],
    [0x9C, 0x7B, 0xFF],
];

/// Travel move color (sRGB) and opacity.
pub const TRAVEL_COLOR: [u8; 4] = [0x5A, 0x9B, 0xFF, 150];
/// Retraction marker color (sRGB).
pub const RETRACT_COLOR: [u8; 3] = [0xCD, 0x22, 0xD6];
/// Unretract / prime marker color (sRGB).
pub const UNRETRACT_COLOR: [u8; 3] = [0x49, 0xC8, 0xE8];

/// Display color of a feature (sRGB).
pub fn feature_color(feature: Feature) -> [u8; 3] {
    feature.default_color()
}

/// Display color of a tool (sRGB).
pub fn tool_color(tool: u8) -> [u8; 3] {
    TOOL_COLORS[tool as usize % TOOL_COLORS.len()]
}

/// sRGB 8-bit channel to linear 0..1.
pub fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear 0..1 to sRGB 8-bit channel (clamped).
pub fn linear_to_srgb(c: f32) -> u8 {
    let c = if c.is_finite() {
        c.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// sRGB color to linear RGB.
pub fn srgb_to_linear_rgb(c: [u8; 3]) -> [f32; 3] {
    [
        srgb_to_linear(c[0]),
        srgb_to_linear(c[1]),
        srgb_to_linear(c[2]),
    ]
}

/// Evaluate the gradient at `t` in `0..=1` (clamped; NaN maps to 0). The
/// interpolation happens in sRGB space, exactly like the shader.
pub fn ramp_color(t: f32) -> [u8; 3] {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let n = RAMP.len();
    let x = t * (n - 1) as f32;
    let i = (x as usize).min(n - 2);
    let f = x - i as f32;
    let a = RAMP[i];
    let b = RAMP[i + 1];
    let mut out = [0u8; 3];
    for k in 0..3 {
        let v = a[k] as f32 + (b[k] as f32 - a[k] as f32) * f;
        out[k] = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// A closed interval of finite values, or empty.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScalarRange {
    /// Smallest value seen (`+inf` when empty).
    pub min: f32,
    /// Largest value seen (`-inf` when empty).
    pub max: f32,
}

impl Default for ScalarRange {
    fn default() -> Self {
        ScalarRange::empty()
    }
}

impl ScalarRange {
    /// The empty range.
    pub const fn empty() -> ScalarRange {
        ScalarRange {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
        }
    }

    /// A range from two values (ordered automatically).
    pub fn new(a: f32, b: f32) -> ScalarRange {
        let mut r = ScalarRange::empty();
        r.include(a);
        r.include(b);
        r
    }

    /// Extend the range by `v`; non-finite values are ignored.
    pub fn include(&mut self, v: f32) {
        if v.is_finite() {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
    }

    /// Merge another range into this one.
    pub fn merge(&mut self, other: &ScalarRange) {
        if !other.is_empty() {
            self.include(other.min);
            self.include(other.max);
        }
    }

    /// True when no finite value was included.
    pub fn is_empty(&self) -> bool {
        self.min > self.max
    }

    /// `max - min`, 0 when empty.
    pub fn span(&self) -> f32 {
        if self.is_empty() {
            0.0
        } else {
            self.max - self.min
        }
    }

    /// Map `v` into `0..=1` (clamped). A degenerate range maps everything to
    /// 0.5 so a single-valued file gets the middle color, like the shader.
    pub fn normalize(&self, v: f32) -> f32 {
        let span = self.span();
        if span <= 1e-6 || !v.is_finite() {
            return 0.5;
        }
        ((v - self.min) / span).clamp(0.0, 1.0)
    }

    /// `(min, max)` or `(0, 0)` when empty, for uniforms.
    pub fn or_zero(&self) -> (f32, f32) {
        if self.is_empty() {
            (0.0, 0.0)
        } else {
            (self.min, self.max)
        }
    }
}

/// Value ranges over the extrusions of a toolpath, used for gradient modes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScalarRanges {
    /// Feedrate of extrusions, mm/s.
    pub speed: ScalarRange,
    /// Extrusion width (as rendered), mm.
    pub width: ScalarRange,
    /// Layer height (as rendered), mm.
    pub height: ScalarRange,
    /// Fan speed 0..=255.
    pub fan: ScalarRange,
    /// Number of layers.
    pub layer_count: usize,
    /// Estimated total time, seconds (0 when the toolpath has no timing).
    pub total_time: f32,
}

impl ScalarRanges {
    /// The range the shader normalizes against for `mode`, in the unit the
    /// shader sees (fan 0..255, layer index, time fraction 0..1).
    pub fn shader_range(&self, mode: ColorMode) -> ScalarRange {
        match mode {
            ColorMode::Speed => self.speed,
            ColorMode::Width => self.width,
            ColorMode::LayerHeight => self.height,
            ColorMode::Fan => self.fan,
            ColorMode::Layer => ScalarRange::new(0.0, self.layer_count.saturating_sub(1) as f32),
            ColorMode::Time => ScalarRange::new(0.0, 1.0),
            ColorMode::Feature | ColorMode::Tool => ScalarRange::empty(),
        }
    }

    /// The range shown in the legend for `mode`, in display units (fan in
    /// percent, layers 1-based, time in seconds).
    pub fn display_range(&self, mode: ColorMode) -> ScalarRange {
        match mode {
            ColorMode::Fan => {
                let r = self.fan;
                if r.is_empty() {
                    r
                } else {
                    ScalarRange::new(r.min / 255.0 * 100.0, r.max / 255.0 * 100.0)
                }
            }
            ColorMode::Layer => {
                if self.layer_count == 0 {
                    ScalarRange::empty()
                } else {
                    ScalarRange::new(1.0, self.layer_count as f32)
                }
            }
            ColorMode::Time => ScalarRange::new(0.0, self.total_time),
            other => self.shader_range(other),
        }
    }
}

/// Format a duration in seconds as `1h 02m`, `3m 05s` or `12s`.
pub fn format_duration(seconds: f32) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0s".to_owned();
    }
    let total = seconds.round() as u64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Format a legend value of `mode` (display units) with a sensible number of
/// decimals and its unit.
pub fn format_value(mode: ColorMode, value: f32) -> String {
    if !value.is_finite() {
        return "-".to_owned();
    }
    match mode {
        ColorMode::Time => format_duration(value),
        ColorMode::Layer => format!("{}", value.round() as i64),
        ColorMode::Fan => format!("{value:.0} %"),
        ColorMode::Speed => {
            if value >= 100.0 {
                format!("{value:.0} mm/s")
            } else {
                format!("{value:.1} mm/s")
            }
        }
        ColorMode::Width | ColorMode::LayerHeight => format!("{value:.3} mm"),
        ColorMode::Feature | ColorMode::Tool => format!("{value}"),
    }
}

/// Per-feature totals gathered from the toolpath segments.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FeatureTally {
    /// Number of extrusion segments.
    pub segments: usize,
    /// Sum of segment durations, seconds.
    pub time: f32,
    /// Extruded path length, mm.
    pub distance: f32,
}

/// One row of the feature legend.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureEntry {
    /// The feature.
    pub feature: Feature,
    /// Swatch color (sRGB).
    pub color: [u8; 3],
    /// Number of extrusion segments of this feature.
    pub segments: usize,
    /// Estimated time spent, seconds (0 if unknown).
    pub time: f32,
    /// Fraction of the total extrusion time, when timing is known.
    pub time_share: Option<f32>,
}

/// One row of the tool legend.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolEntry {
    /// Tool index.
    pub tool: u8,
    /// Swatch color (sRGB).
    pub color: [u8; 3],
    /// Number of extrusion segments printed with this tool.
    pub segments: usize,
}

/// A gradient legend: min/max values in display units plus tick labels.
#[derive(Clone, Debug, PartialEq)]
pub struct GradientLegend {
    /// The color mode.
    pub mode: ColorMode,
    /// Value at the low end (display units).
    pub min: f32,
    /// Value at the high end (display units).
    pub max: f32,
    /// Ticks as `(t in 0..=1, label)`, low to high.
    pub ticks: Vec<(f32, String)>,
}

/// Content of the legend for one color mode.
#[derive(Clone, Debug, PartialEq)]
pub enum Legend {
    /// Feature rows (only features that occur).
    Features(Vec<FeatureEntry>),
    /// Tool rows (only tools that occur).
    Tools(Vec<ToolEntry>),
    /// Gradient bar.
    Gradient(GradientLegend),
    /// Nothing to show (no data).
    Empty,
}

/// Inputs for [`build_legend`], gathered while building the scene.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LegendData {
    /// Per-feature tallies from the segments, indexed by feature index.
    pub features: [FeatureTally; Feature::COUNT],
    /// Time per feature from `Toolpath::stats.features` when present.
    pub stat_times: Option<[f32; Feature::COUNT]>,
    /// Segment counts per tool index (0..=255).
    pub tools: Vec<(u8, usize)>,
    /// Value ranges for gradient modes.
    pub ranges: ScalarRanges,
}

/// Build the legend model for `mode`.
pub fn build_legend(data: &LegendData, mode: ColorMode) -> Legend {
    match mode {
        ColorMode::Feature => {
            let times: [f32; Feature::COUNT] = match &data.stat_times {
                Some(t) if t.iter().any(|v| *v > 0.0) => *t,
                _ => {
                    let mut t = [0.0; Feature::COUNT];
                    for (slot, tally) in t.iter_mut().zip(data.features.iter()) {
                        *slot = tally.time;
                    }
                    t
                }
            };
            let total: f32 = times.iter().filter(|v| v.is_finite()).sum();
            let rows: Vec<FeatureEntry> = Feature::ALL
                .iter()
                .zip(data.features.iter())
                .zip(times.iter())
                .filter(|((_, tally), _)| tally.segments > 0)
                .map(|((feature, tally), time)| FeatureEntry {
                    feature: *feature,
                    color: feature_color(*feature),
                    segments: tally.segments,
                    time: *time,
                    time_share: (total > 0.0).then(|| (*time / total).clamp(0.0, 1.0)),
                })
                .collect();
            if rows.is_empty() {
                Legend::Empty
            } else {
                Legend::Features(rows)
            }
        }
        ColorMode::Tool => {
            let rows: Vec<ToolEntry> = data
                .tools
                .iter()
                .filter(|(_, n)| *n > 0)
                .map(|(tool, n)| ToolEntry {
                    tool: *tool,
                    color: tool_color(*tool),
                    segments: *n,
                })
                .collect();
            if rows.is_empty() {
                Legend::Empty
            } else {
                Legend::Tools(rows)
            }
        }
        _ => {
            let r = data.ranges.display_range(mode);
            if r.is_empty() {
                return Legend::Empty;
            }
            let ticks = gradient_ticks(mode, r);
            Legend::Gradient(GradientLegend {
                mode,
                min: r.min,
                max: r.max,
                ticks,
            })
        }
    }
}

/// Tick labels for a gradient: min, max and up to three evenly spaced
/// values in between (just one when the range is degenerate).
fn gradient_ticks(mode: ColorMode, r: ScalarRange) -> Vec<(f32, String)> {
    if r.span() <= 1e-6 {
        return vec![(0.5, format_value(mode, r.min))];
    }
    let steps = if mode == ColorMode::Layer && r.span() < 4.0 {
        r.span().round().max(1.0) as usize
    } else {
        4
    };
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            (t, format_value(mode, r.min + r.span() * t))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trip() {
        for c in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(c)), c);
        }
        assert_eq!(linear_to_srgb(f32::NAN), 0);
        assert_eq!(linear_to_srgb(7.0), 255);
    }

    #[test]
    fn ramp_endpoints_and_clamping() {
        assert_eq!(ramp_color(0.0), RAMP[0]);
        assert_eq!(ramp_color(1.0), RAMP[RAMP.len() - 1]);
        assert_eq!(ramp_color(-3.0), RAMP[0]);
        assert_eq!(ramp_color(9.0), RAMP[RAMP.len() - 1]);
        assert_eq!(ramp_color(f32::NAN), RAMP[0]);
        // Exactly on an inner stop.
        assert_eq!(ramp_color(0.5), RAMP[5]);
    }

    #[test]
    fn scalar_range_basics() {
        let mut r = ScalarRange::empty();
        assert!(r.is_empty());
        assert_eq!(r.span(), 0.0);
        r.include(f32::NAN);
        r.include(f32::INFINITY);
        assert!(r.is_empty());
        r.include(10.0);
        assert_eq!(r.normalize(10.0), 0.5);
        r.include(30.0);
        assert_eq!(r.normalize(20.0), 0.5);
        assert_eq!(r.normalize(0.0), 0.0);
        assert_eq!(r.normalize(99.0), 1.0);
        assert_eq!(ScalarRange::new(5.0, 1.0).or_zero(), (1.0, 5.0));
    }

    #[test]
    fn durations_format() {
        assert_eq!(format_duration(0.0), "0s");
        assert_eq!(format_duration(f32::NAN), "0s");
        assert_eq!(format_duration(42.4), "42s");
        assert_eq!(format_duration(185.0), "3m 05s");
        assert_eq!(format_duration(3720.0), "1h 02m");
    }

    #[test]
    fn feature_legend_uses_stats_times() {
        let mut data = LegendData::default();
        data.features[Feature::Perimeter.index()] = FeatureTally {
            segments: 10,
            time: 1.0,
            distance: 5.0,
        };
        data.features[Feature::InternalInfill.index()] = FeatureTally {
            segments: 30,
            time: 3.0,
            distance: 50.0,
        };
        let Legend::Features(rows) = build_legend(&data, ColorMode::Feature) else {
            panic!("expected feature legend");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].feature, Feature::Perimeter);
        assert!((rows[0].time_share.unwrap_or(0.0) - 0.25).abs() < 1e-6);

        let mut stat = [0.0; Feature::COUNT];
        stat[Feature::Perimeter.index()] = 30.0;
        stat[Feature::InternalInfill.index()] = 10.0;
        data.stat_times = Some(stat);
        let Legend::Features(rows) = build_legend(&data, ColorMode::Feature) else {
            panic!("expected feature legend");
        };
        assert!((rows[0].time_share.unwrap_or(0.0) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn gradient_legend_ranges_and_units() {
        let data = LegendData {
            ranges: ScalarRanges {
                speed: ScalarRange::new(20.0, 120.0),
                fan: ScalarRange::new(0.0, 255.0),
                layer_count: 40,
                total_time: 3600.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let Legend::Gradient(g) = build_legend(&data, ColorMode::Speed) else {
            panic!("expected gradient");
        };
        assert_eq!((g.min, g.max), (20.0, 120.0));
        assert_eq!(g.ticks.first().map(|t| t.1.as_str()), Some("20.0 mm/s"));
        assert_eq!(g.ticks.last().map(|t| t.1.as_str()), Some("120 mm/s"));
        let Legend::Gradient(g) = build_legend(&data, ColorMode::Fan) else {
            panic!("expected gradient");
        };
        assert_eq!(g.max, 100.0);
        let Legend::Gradient(g) = build_legend(&data, ColorMode::Layer) else {
            panic!("expected gradient");
        };
        assert_eq!((g.min, g.max), (1.0, 40.0));
        let Legend::Gradient(g) = build_legend(&data, ColorMode::Time) else {
            panic!("expected gradient");
        };
        assert_eq!(g.ticks.last().map(|t| t.1.as_str()), Some("1h 00m"));
        // Width has no data: empty legend.
        assert_eq!(build_legend(&data, ColorMode::Width), Legend::Empty);
    }

    #[test]
    fn degenerate_gradient_has_single_tick() {
        let data = LegendData {
            ranges: ScalarRanges {
                width: ScalarRange::new(0.45, 0.45),
                ..Default::default()
            },
            ..Default::default()
        };
        let Legend::Gradient(g) = build_legend(&data, ColorMode::Width) else {
            panic!("expected gradient");
        };
        assert_eq!(g.ticks.len(), 1);
        assert_eq!(g.ticks[0].1, "0.450 mm");
    }
}
