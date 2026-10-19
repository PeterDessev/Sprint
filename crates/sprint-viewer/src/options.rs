//! Public option types of the viewer: color modes, render styles, view
//! modes and presets, [`ViewerOptions`] and the printer bed description.

use sprint_gcode::{Bounds, Feature};

/// How extrusions are colored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColorMode {
    /// Print feature (perimeter, infill, ...), PrusaSlicer-like palette.
    #[default]
    Feature,
    /// Commanded feedrate (mm/s).
    Speed,
    /// Extrusion width (mm).
    Width,
    /// Layer height (mm).
    LayerHeight,
    /// Active tool / extruder.
    Tool,
    /// Part-cooling fan speed (%).
    Fan,
    /// Layer index.
    Layer,
    /// Estimated elapsed print time.
    Time,
}

impl ColorMode {
    /// Every color mode, in menu order.
    pub const ALL: [ColorMode; 8] = [
        ColorMode::Feature,
        ColorMode::Speed,
        ColorMode::Width,
        ColorMode::LayerHeight,
        ColorMode::Tool,
        ColorMode::Fan,
        ColorMode::Layer,
        ColorMode::Time,
    ];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            ColorMode::Feature => "Feature type",
            ColorMode::Speed => "Speed",
            ColorMode::Width => "Width",
            ColorMode::LayerHeight => "Layer height",
            ColorMode::Tool => "Tool",
            ColorMode::Fan => "Fan speed",
            ColorMode::Layer => "Layer",
            ColorMode::Time => "Print time",
        }
    }

    /// Stable index used by the shader (`0..8`).
    pub fn index(self) -> u32 {
        match self {
            ColorMode::Feature => 0,
            ColorMode::Speed => 1,
            ColorMode::Width => 2,
            ColorMode::LayerHeight => 3,
            ColorMode::Tool => 4,
            ColorMode::Fan => 5,
            ColorMode::Layer => 6,
            ColorMode::Time => 7,
        }
    }

    /// True for modes that map a scalar onto a color gradient.
    pub fn is_gradient(self) -> bool {
        !matches!(self, ColorMode::Feature | ColorMode::Tool)
    }

    /// Unit shown next to gradient legend values ("" when unitless).
    pub fn unit(self) -> &'static str {
        match self {
            ColorMode::Speed => "mm/s",
            ColorMode::Width | ColorMode::LayerHeight => "mm",
            ColorMode::Fan => "%",
            ColorMode::Feature | ColorMode::Tool | ColorMode::Layer | ColorMode::Time => "",
        }
    }
}

/// How extrusions are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderStyle {
    /// Lit, rounded beads (instanced prisms).
    #[default]
    Tubes,
    /// One-pixel lines: for huge files or weak GPUs.
    Lines,
}

impl RenderStyle {
    /// Every style, in menu order.
    pub const ALL: [RenderStyle; 2] = [RenderStyle::Tubes, RenderStyle::Lines];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            RenderStyle::Tubes => "Tubes",
            RenderStyle::Lines => "Lines",
        }
    }
}

/// Projection / interaction mode of the viewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// Free orbit camera with perspective projection.
    #[default]
    Perspective,
    /// Orthographic top-down view of the top visible layer; lower layers are
    /// ghosted.
    LayerTop,
}

impl ViewMode {
    /// Every view mode, in menu order.
    pub const ALL: [ViewMode; 2] = [ViewMode::Perspective, ViewMode::LayerTop];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            ViewMode::Perspective => "3D",
            ViewMode::LayerTop => "Layer",
        }
    }
}

/// How ghosted geometry is drawn: moves not printed yet (beyond
/// [`ViewerOptions::progress_line`]) and, in [`ViewMode::LayerTop`], the
/// layers below the current one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GhostStyle {
    /// Dim, nearly grey solid beads.
    #[default]
    Opaque,
    /// See-through beads drawn after everything else (about 25 % opaque):
    /// the printed part shows through what is still to come.
    Translucent,
}

impl GhostStyle {
    /// Every style, in menu order.
    pub const ALL: [GhostStyle; 2] = [GhostStyle::Opaque, GhostStyle::Translucent];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            GhostStyle::Opaque => "Solid",
            GhostStyle::Translucent => "Translucent",
        }
    }
}

/// Standard camera orientations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewPreset {
    /// Elevated view from the front left.
    Iso,
    /// Straight down (X right, Y up).
    Top,
    /// Straight up from below the bed.
    Bottom,
    /// From the front (-Y), looking towards +Y.
    Front,
    /// From the back (+Y).
    Back,
    /// From the left (-X).
    Left,
    /// From the right (+X).
    Right,
}

impl ViewPreset {
    /// Every preset, in the order of the PrusaSlicer keyboard shortcuts 0-6.
    pub const ALL: [ViewPreset; 7] = [
        ViewPreset::Iso,
        ViewPreset::Top,
        ViewPreset::Bottom,
        ViewPreset::Front,
        ViewPreset::Back,
        ViewPreset::Left,
        ViewPreset::Right,
    ];

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            ViewPreset::Iso => "Iso",
            ViewPreset::Top => "Top",
            ViewPreset::Bottom => "Bottom",
            ViewPreset::Front => "Front",
            ViewPreset::Back => "Back",
            ViewPreset::Left => "Left",
            ViewPreset::Right => "Right",
        }
    }

    /// Camera `(yaw, pitch)` in radians for this preset (see
    /// [`crate::camera::Camera`] for the conventions).
    pub fn angles(self) -> (f32, f32) {
        use std::f32::consts::{FRAC_PI_2, PI};
        match self {
            ViewPreset::Iso => (-40f32.to_radians(), 32f32.to_radians()),
            ViewPreset::Top => (0.0, FRAC_PI_2),
            ViewPreset::Bottom => (0.0, -FRAC_PI_2),
            ViewPreset::Front => (0.0, 0.0),
            ViewPreset::Back => (PI, 0.0),
            ViewPreset::Left => (-FRAC_PI_2, 0.0),
            ViewPreset::Right => (FRAC_PI_2, 0.0),
        }
    }
}

/// Everything that controls what the viewer shows. Changing any field never
/// re-uploads geometry: it only updates shader uniforms or draw ranges.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewerOptions {
    /// How extrusions are colored.
    pub color_mode: ColorMode,
    /// Tubes or lines.
    pub style: RenderStyle,
    /// 3D orbit view or top-down layer view.
    pub view_mode: ViewMode,
    /// Draw travel moves as thin translucent lines.
    pub show_travel: bool,
    /// Draw retraction / prime markers.
    pub show_retractions: bool,
    /// Draw the bed plate with its grid.
    pub show_bed: bool,
    /// Draw the build-volume wireframe.
    pub show_build_volume: bool,
    /// Draw the origin marker and XYZ axes.
    pub show_axes: bool,
    /// `true` = hidden, indexed by [`Feature::index`].
    pub hidden_features: [bool; Feature::COUNT],
    /// Inclusive layer range shown (clamped to the toolpath).
    pub layer_range: (usize, usize),
    /// Moves within the top layer are shown up to this source line (for
    /// "scrubbing" through a layer); None = whole layer.
    pub line_limit: Option<u32>,
    /// Print progress: segments with line > this are drawn ghosted.
    pub progress_line: Option<u32>,
    /// Source line to highlight (selection from the G-code text view).
    pub highlight_line: Option<u32>,
    /// Marker at the current progress/scrub position.
    pub show_tool_head: bool,
    /// Background color (sRGB).
    pub background: [u8; 3],
    /// How ghosted (not yet printed / lower layer) geometry is drawn.
    pub ghost_style: GhostStyle,
}

impl Default for ViewerOptions {
    fn default() -> Self {
        ViewerOptions {
            color_mode: ColorMode::Feature,
            style: RenderStyle::Tubes,
            view_mode: ViewMode::Perspective,
            show_travel: false,
            show_retractions: false,
            show_bed: true,
            show_build_volume: false,
            show_axes: true,
            hidden_features: [false; Feature::COUNT],
            layer_range: (0, usize::MAX),
            line_limit: None,
            progress_line: None,
            highlight_line: None,
            show_tool_head: true,
            background: crate::color::BACKGROUND,
            ghost_style: GhostStyle::Opaque,
        }
    }
}

impl ViewerOptions {
    /// Whether `feature` is currently hidden.
    pub fn is_feature_hidden(&self, feature: Feature) -> bool {
        self.hidden_features
            .get(feature.index())
            .copied()
            .unwrap_or(false)
    }

    /// Hide or show one feature.
    pub fn set_feature_hidden(&mut self, feature: Feature, hidden: bool) {
        if let Some(slot) = self.hidden_features.get_mut(feature.index()) {
            *slot = hidden;
        }
    }

    /// Hidden features as a bit mask (bit `i` = feature index `i`).
    pub fn hidden_mask(&self) -> u32 {
        self.hidden_features
            .iter()
            .enumerate()
            .filter(|(_, hidden)| **hidden)
            .fold(0u32, |mask, (i, _)| mask | (1u32 << i))
    }

    /// The layer range clamped to `layer_count` layers, ordered so that
    /// `lo <= hi`. `None` when there are no layers.
    pub fn clamped_layer_range(&self, layer_count: usize) -> Option<(usize, usize)> {
        if layer_count == 0 {
            return None;
        }
        let last = layer_count - 1;
        let a = self.layer_range.0.min(last);
        let b = self.layer_range.1.min(last);
        Some((a.min(b), a.max(b)))
    }
}

/// Margins of the view, in logical points, that camera fits keep the part
/// out of: cards or toolbars an application draws over the view's edges
/// (see [`GcodeViewer::set_fit_insets`](crate::GcodeViewer::set_fit_insets)).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ViewInsets {
    /// From the left edge.
    pub left: f32,
    /// From the right edge.
    pub right: f32,
    /// From the top edge.
    pub top: f32,
    /// From the bottom edge.
    pub bottom: f32,
}

impl ViewInsets {
    /// No insets: fits use the whole view.
    pub const NONE: ViewInsets = ViewInsets {
        left: 0.0,
        right: 0.0,
        top: 0.0,
        bottom: 0.0,
    };

    /// `[left, right, top, bottom]`.
    pub fn to_array(self) -> [f32; 4] {
        [self.left, self.right, self.top, self.bottom]
    }

    /// Whether no edge is inset (by half a point or more).
    pub fn is_none(&self) -> bool {
        self.to_array().iter().all(|v| !v.is_finite() || *v < 0.5)
    }

    /// Whether `other` differs from these by more than `tolerance` points on
    /// any edge.
    pub fn differs(&self, other: &ViewInsets, tolerance: f32) -> bool {
        let clean = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
        self.to_array()
            .iter()
            .zip(other.to_array())
            .any(|(a, b)| (clean(*a) - clean(b)).abs() > tolerance)
    }
}

/// Printer bed and build volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BedSpec {
    /// Bed size X, Y in mm.
    pub size: [f32; 2],
    /// Maximum printable Z in mm.
    pub max_z: f32,
    /// `true` when the origin is the bed centre (delta printers), `false`
    /// for a front-left origin.
    pub origin_center: bool,
}

impl Default for BedSpec {
    fn default() -> Self {
        BedSpec {
            size: [220.0, 220.0],
            max_z: 250.0,
            origin_center: false,
        }
    }
}

impl BedSpec {
    /// Largest accepted bed dimension in mm; larger or invalid values fall
    /// back to the defaults.
    pub const MAX_SIZE: f32 = 10_000.0;

    /// A copy with non-finite / non-positive / absurd values replaced by the
    /// defaults.
    pub fn sanitized(&self) -> BedSpec {
        let fix = |v: f32, default: f32| {
            if v.is_finite() && v > 0.0 && v <= Self::MAX_SIZE {
                v
            } else {
                default
            }
        };
        let d = BedSpec::default();
        BedSpec {
            size: [fix(self.size[0], d.size[0]), fix(self.size[1], d.size[1])],
            max_z: fix(self.max_z, d.max_z),
            origin_center: self.origin_center,
        }
    }

    /// Front-left corner of the bed (mm).
    pub fn min_xy(&self) -> [f32; 2] {
        let s = self.sanitized();
        if s.origin_center {
            [-s.size[0] * 0.5, -s.size[1] * 0.5]
        } else {
            [0.0, 0.0]
        }
    }

    /// Back-right corner of the bed (mm).
    pub fn max_xy(&self) -> [f32; 2] {
        let s = self.sanitized();
        let min = self.min_xy();
        [min[0] + s.size[0], min[1] + s.size[1]]
    }

    /// The build volume as a bounding box.
    pub fn volume(&self) -> Bounds {
        let min = self.min_xy();
        let max = self.max_xy();
        Bounds {
            min: [min[0], min[1], 0.0],
            max: [max[0], max[1], self.sanitized().max_z],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_mask_matches_features() {
        let mut o = ViewerOptions::default();
        assert_eq!(o.hidden_mask(), 0);
        o.set_feature_hidden(Feature::Skirt, true);
        o.set_feature_hidden(Feature::Unknown, true);
        assert!(o.is_feature_hidden(Feature::Skirt));
        assert!(!o.is_feature_hidden(Feature::Perimeter));
        assert_eq!(o.hidden_mask(), (1 << Feature::Skirt.index()) | 1);
    }

    #[test]
    fn layer_range_is_clamped_and_ordered() {
        let mut o = ViewerOptions::default();
        assert_eq!(o.clamped_layer_range(0), None);
        assert_eq!(o.clamped_layer_range(10), Some((0, 9)));
        o.layer_range = (7, 3);
        assert_eq!(o.clamped_layer_range(10), Some((3, 7)));
        o.layer_range = (50, 60);
        assert_eq!(o.clamped_layer_range(10), Some((9, 9)));
    }

    #[test]
    fn bed_bounds_respect_origin() {
        let bed = BedSpec {
            size: [200.0, 100.0],
            max_z: 180.0,
            origin_center: true,
        };
        assert_eq!(bed.min_xy(), [-100.0, -50.0]);
        assert_eq!(bed.max_xy(), [100.0, 50.0]);
        let v = bed.volume();
        assert_eq!(v.max[2], 180.0);
        let bad = BedSpec {
            size: [f32::NAN, -3.0],
            max_z: f32::INFINITY,
            origin_center: false,
        };
        assert_eq!(bad.sanitized(), BedSpec::default());
    }

    #[test]
    fn ghosts_are_solid_by_default() {
        assert_eq!(ViewerOptions::default().ghost_style, GhostStyle::Opaque);
        assert_eq!(GhostStyle::default(), GhostStyle::Opaque);
        let names: Vec<_> = GhostStyle::ALL.iter().map(|g| g.name()).collect();
        assert_eq!(names, ["Solid", "Translucent"]);
    }

    #[test]
    fn mode_indices_are_unique() {
        let mut seen = [false; 8];
        for m in ColorMode::ALL {
            let i = m.index() as usize;
            assert!(!seen[i]);
            seen[i] = true;
        }
        assert!(ColorMode::Speed.is_gradient());
        assert!(!ColorMode::Tool.is_gradient());
    }
}
