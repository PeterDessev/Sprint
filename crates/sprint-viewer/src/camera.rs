//! Orbit camera math: view/projection matrices (reversed-Z), rays for
//! picking, orbit/pan/zoom, presets and fitting. GPU-agnostic.
//!
//! Conventions: the world is Z-up (machine coordinates, mm). Screen
//! coordinates have their origin at the top-left corner with Y pointing
//! down; NDC has Y up. Depth is reversed (near = 1, far = 0), which gives
//! Depth32Float excellent precision across a whole printer-sized scene.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::options::ViewPreset;

/// Projection kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Projection {
    /// Perspective with an infinite far plane.
    #[default]
    Perspective,
    /// Orthographic; the visible half-height is `distance * tan(fov_y / 2)`
    /// so zooming works the same way in both projections.
    Orthographic,
}

/// A half-line in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    /// Start point.
    pub origin: Vec3,
    /// Unit direction.
    pub dir: Vec3,
}

impl Ray {
    /// The point at parameter `t`.
    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }

    /// Parameter where the ray crosses the horizontal plane `z`, if it does
    /// so in front of the origin.
    pub fn intersect_plane_z(&self, z: f32) -> Option<f32> {
        if self.dir.z.abs() < 1e-8 {
            return None;
        }
        let t = (z - self.origin.z) / self.dir.z;
        (t.is_finite() && t >= 0.0).then_some(t)
    }

    /// Parameter where the ray crosses the plane through `point` with
    /// normal `normal`, if in front of the origin.
    pub fn intersect_plane(&self, point: Vec3, normal: Vec3) -> Option<f32> {
        let denom = self.dir.dot(normal);
        if denom.abs() < 1e-8 {
            return None;
        }
        let t = (point - self.origin).dot(normal) / denom;
        (t.is_finite() && t >= 0.0).then_some(t)
    }
}

/// An orbit camera looking at `target` from `distance`, oriented by `yaw`
/// (around +Z) and `pitch` (elevation).
///
/// `yaw = 0, pitch = 0` looks from the front (-Y) towards +Y; positive yaw
/// moves the eye towards +X; `pitch = +pi/2` looks straight down with +Y up
/// on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Orbit centre (world, mm).
    pub target: Vec3,
    /// Distance from eye to target (mm).
    pub distance: f32,
    /// Rotation around +Z, radians, wrapped to `-pi..=pi`.
    pub yaw: f32,
    /// Elevation, radians, clamped to `-pi/2..=pi/2`.
    pub pitch: f32,
    /// Vertical field of view, radians.
    pub fov_y: f32,
    /// Perspective or orthographic.
    pub projection: Projection,
    /// Radius of the scene around the target; sizes the orthographic depth
    /// range so nothing is clipped.
    pub scene_radius: f32,
}

impl Default for Camera {
    fn default() -> Self {
        let (yaw, pitch) = ViewPreset::Iso.angles();
        Camera {
            target: Vec3::new(110.0, 110.0, 0.0),
            distance: 450.0,
            yaw,
            pitch,
            fov_y: 30f32.to_radians(),
            projection: Projection::Perspective,
            scene_radius: 300.0,
        }
    }
}

/// Replace a non-finite or non-positive aspect ratio by 1.
fn sane_aspect(aspect: f32) -> f32 {
    if aspect.is_finite() && aspect > 1e-6 {
        aspect.clamp(1e-3, 1e3)
    } else {
        1.0
    }
}

impl Camera {
    /// Closest allowed eye distance (mm).
    pub const MIN_DISTANCE: f32 = 0.5;
    /// Farthest allowed eye distance (mm).
    pub const MAX_DISTANCE: f32 = 50_000.0;
    /// Extra room added around fitted bounds (fraction of the view).
    pub const FIT_MARGIN: f32 = 1.08;

    /// Unit vector from the eye towards the target.
    pub fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        -Vec3::new(cp * sy, -cp * cy, sp)
    }

    /// Unit vector pointing right on screen (always horizontal).
    pub fn right(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cy, sy, 0.0)
    }

    /// Unit vector pointing up on screen.
    pub fn up(&self) -> Vec3 {
        self.right().cross(self.forward())
    }

    /// Eye position for perspective; for orthographic it is the point at
    /// `distance` in front of the target (used for zoom bookkeeping).
    pub fn eye(&self) -> Vec3 {
        self.target - self.forward() * self.distance
    }

    /// How far behind the target the orthographic view origin sits.
    fn ortho_back(&self) -> f32 {
        let r = self.scene_radius.max(1.0);
        self.distance.max(r) + r + 10.0
    }

    /// Origin of the view transform (perspective eye, or the far-back
    /// orthographic origin).
    pub fn view_origin(&self) -> Vec3 {
        match self.projection {
            Projection::Perspective => self.eye(),
            Projection::Orthographic => self.target - self.forward() * self.ortho_back(),
        }
    }

    /// Visible half-height at the target distance (world units).
    pub fn half_height(&self) -> f32 {
        self.distance * (self.fov_y * 0.5).tan()
    }

    /// Near plane distance for perspective.
    pub fn near(&self) -> f32 {
        (self.distance * 0.005).clamp(0.01, 5.0)
    }

    /// World-to-view matrix (right-handed, camera looks down -Z).
    pub fn view_matrix(&self) -> Mat4 {
        let f = self.forward();
        let r = self.right();
        let u = self.up();
        let e = self.view_origin();
        Mat4::from_cols(
            Vec4::new(r.x, u.x, -f.x, 0.0),
            Vec4::new(r.y, u.y, -f.y, 0.0),
            Vec4::new(r.z, u.z, -f.z, 0.0),
            Vec4::new(-r.dot(e), -u.dot(e), f.dot(e), 1.0),
        )
    }

    /// View-to-clip matrix with reversed depth (near = 1, far = 0).
    /// Perspective uses an infinite far plane.
    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        let aspect = sane_aspect(aspect);
        match self.projection {
            Projection::Perspective => {
                let f = 1.0 / (self.fov_y * 0.5).tan();
                let n = self.near();
                Mat4::from_cols(
                    Vec4::new(f / aspect, 0.0, 0.0, 0.0),
                    Vec4::new(0.0, f, 0.0, 0.0),
                    Vec4::new(0.0, 0.0, 0.0, -1.0),
                    Vec4::new(0.0, 0.0, n, 0.0),
                )
            }
            Projection::Orthographic => {
                let hh = self.half_height().max(1e-4);
                let hw = hh * aspect;
                let n = 0.01;
                let far = self.ortho_back() + self.scene_radius.max(1.0) * 2.0 + 10.0;
                let range = far - n;
                Mat4::from_cols(
                    Vec4::new(1.0 / hw, 0.0, 0.0, 0.0),
                    Vec4::new(0.0, 1.0 / hh, 0.0, 0.0),
                    Vec4::new(0.0, 0.0, 1.0 / range, 0.0),
                    Vec4::new(0.0, 0.0, far / range, 1.0),
                )
            }
        }
    }

    /// `projection * view`.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect) * self.view_matrix()
    }

    /// World size of one pixel at view depth `depth` (distance along
    /// `forward` from the eye) for a viewport `viewport_height` pixels tall.
    pub fn world_per_pixel(&self, depth: f32, viewport_height: f32) -> f32 {
        let h = viewport_height.max(1.0);
        match self.projection {
            Projection::Perspective => 2.0 * depth.max(0.0) * (self.fov_y * 0.5).tan() / h,
            Projection::Orthographic => 2.0 * self.half_height() / h,
        }
    }

    /// Ray through a point given in normalized device coordinates
    /// (`-1..=1`, Y up).
    pub fn ray_from_ndc(&self, ndc: Vec2, aspect: f32) -> Ray {
        let aspect = sane_aspect(aspect);
        let f = self.forward();
        let r = self.right();
        let u = self.up();
        match self.projection {
            Projection::Perspective => {
                let t = (self.fov_y * 0.5).tan();
                let dir = (f + r * (ndc.x * t * aspect) + u * (ndc.y * t)).normalize_or(f);
                Ray {
                    origin: self.eye(),
                    dir,
                }
            }
            Projection::Orthographic => {
                let hh = self.half_height();
                Ray {
                    origin: self.view_origin() + r * (ndc.x * hh * aspect) + u * (ndc.y * hh),
                    dir: f,
                }
            }
        }
    }

    /// Ray through a screen position (`pos` and `size` in the same units,
    /// origin top-left, Y down).
    pub fn ray_from_screen(&self, pos: Vec2, size: Vec2) -> Ray {
        let size = size.max(Vec2::ONE);
        let ndc = Vec2::new(2.0 * pos.x / size.x - 1.0, 1.0 - 2.0 * pos.y / size.y);
        self.ray_from_ndc(ndc, size.x / size.y)
    }

    /// Project a world point to screen coordinates; returns `(x, y, depth)`
    /// or `None` when the point is behind the camera.
    pub fn project_to_screen(&self, p: Vec3, size: Vec2) -> Option<Vec3> {
        let size = size.max(Vec2::ONE);
        let clip = self.view_proj(size.x / size.y) * p.extend(1.0);
        if clip.w <= 1e-9 || !clip.is_finite() {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some(Vec3::new(
            (ndc.x + 1.0) * 0.5 * size.x,
            (1.0 - ndc.y) * 0.5 * size.y,
            ndc.z,
        ))
    }

    /// Inverse of [`Camera::project_to_screen`]: the world point at a screen
    /// position and (reversed) depth.
    pub fn unproject(&self, screen: Vec2, depth: f32, size: Vec2) -> Option<Vec3> {
        let size = size.max(Vec2::ONE);
        let inv = self.view_proj(size.x / size.y).inverse();
        let ndc = Vec4::new(
            2.0 * screen.x / size.x - 1.0,
            1.0 - 2.0 * screen.y / size.y,
            depth,
            1.0,
        );
        let p = inv * ndc;
        if p.w.abs() <= 1e-12 || !p.is_finite() {
            return None;
        }
        Some(p.truncate() / p.w)
    }

    /// Rotate around the target by the given angles (radians). Pitch is
    /// clamped to straight up/down; yaw wraps.
    pub fn orbit(&mut self, d_yaw: f32, d_pitch: f32) {
        if d_yaw.is_finite() {
            self.yaw = wrap_angle(self.yaw + d_yaw);
        }
        if d_pitch.is_finite() {
            self.pitch = (self.pitch + d_pitch).clamp(-FRAC_PI_2, FRAC_PI_2);
        }
    }

    /// Move the target so the scene follows a drag of `delta` screen units
    /// in a viewport `viewport_height` units tall.
    pub fn pan(&mut self, delta: Vec2, viewport_height: f32) {
        if !delta.is_finite() {
            return;
        }
        let wpp = self.world_per_pixel(self.distance, viewport_height);
        self.target += self.up() * (delta.y * wpp) - self.right() * (delta.x * wpp);
    }

    /// Scale the distance by `factor` (< 1 zooms in), keeping `anchor` (if
    /// any) fixed on screen.
    pub fn zoom(&mut self, factor: f32, anchor: Option<Vec3>) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let new_distance = (self.distance * factor).clamp(Self::MIN_DISTANCE, Self::MAX_DISTANCE);
        let applied = new_distance / self.distance.max(1e-6);
        if let Some(p) = anchor.filter(|p| p.is_finite()) {
            self.target = p + (self.target - p) * applied;
        }
        self.distance = new_distance;
    }

    /// Orient the camera to a preset (target and distance unchanged).
    pub fn set_preset(&mut self, preset: ViewPreset) {
        let (yaw, pitch) = preset.angles();
        self.yaw = yaw;
        self.pitch = pitch;
    }

    /// Centre the target on a point (e.g. a double-clicked spot).
    pub fn focus_on(&mut self, p: Vec3) {
        if p.is_finite() {
            self.target = p;
        }
    }

    /// Choose target and distance so the box `min..max` fills the view for
    /// the current orientation. Degenerate boxes are grown to 10 mm.
    pub fn fit_bounds(&mut self, min: Vec3, max: Vec3, aspect: f32) {
        self.fit_bounds_framed(min, max, aspect, FitFrame::FULL);
    }

    /// Like [`Camera::fit_bounds`], but the box fills `frame`, a part of
    /// the view (cards drawn over the rest of it keep clear of the part):
    /// the distance fits the box into the frame and the target is shifted
    /// so the box's centre lands in the frame's centre. Exact for the
    /// orthographic projection, very close for perspective.
    pub fn fit_bounds_framed(&mut self, min: Vec3, max: Vec3, aspect: f32, frame: FitFrame) {
        if !min.is_finite() || !max.is_finite() || min.cmpgt(max).any() {
            return;
        }
        let frame = frame.sanitized();
        let center = (min + max) * 0.5;
        let half = ((max - min) * 0.5).max(Vec3::splat(5.0));
        let aspect = sane_aspect(aspect);
        let (r, u, f) = (self.right(), self.up(), self.forward());
        // The frame's share of the view scales the field of view it offers.
        let (width, height) = (frame.width() * 0.5, frame.height() * 0.5);
        let tan_y = (self.fov_y * 0.5).tan() / Self::FIT_MARGIN * height;
        let tan_x = (self.fov_y * 0.5).tan() / Self::FIT_MARGIN * aspect * width;
        let mut needed = 0.0f32;
        for i in 0..8 {
            let s = Vec3::new(
                if i & 1 == 0 { -1.0 } else { 1.0 },
                if i & 2 == 0 { -1.0 } else { 1.0 },
                if i & 4 == 0 { -1.0 } else { 1.0 },
            );
            let c = half * s;
            let (x, y, z) = (c.dot(r).abs(), c.dot(u).abs(), c.dot(f));
            needed = match self.projection {
                Projection::Perspective => needed.max(x / tan_x - z).max(y / tan_y - z),
                // Orthographic: half-height needed, converted to a distance.
                Projection::Orthographic => needed.max(y / tan_y).max(x / tan_x),
            };
        }
        self.distance = needed.clamp(Self::MIN_DISTANCE, Self::MAX_DISTANCE);
        // Shift the target so the centre projects to the frame's centre:
        // at the target's depth one NDC unit is `half_height` world units
        // tall (times the aspect ratio across).
        let (cx, cy) = frame.center();
        let hh = self.half_height();
        self.target = center - r * (cx * hh * aspect) - u * (cy * hh);
        self.scene_radius = self.scene_radius.max(half.length());
    }

    /// Interpolate between two cameras (`t` in 0..=1): positions linearly,
    /// distance geometrically, angles along the shortest arc.
    pub fn lerp(&self, other: &Camera, t: f32) -> Camera {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let mut dyaw = wrap_angle(other.yaw - self.yaw);
        if (dyaw.abs() - PI).abs() < 1e-4 {
            dyaw = dyaw.abs();
        }
        let d0 = self.distance.max(1e-3);
        let d1 = other.distance.max(1e-3);
        Camera {
            target: self.target.lerp(other.target, t),
            distance: d0 * (d1 / d0).powf(t),
            yaw: wrap_angle(self.yaw + dyaw * t),
            pitch: self.pitch + (other.pitch - self.pitch) * t,
            fov_y: self.fov_y + (other.fov_y - self.fov_y) * t,
            projection: other.projection,
            scene_radius: self.scene_radius.max(other.scene_radius),
        }
    }
}

/// The part of the view a fit frames the scene in
/// ([`Camera::fit_bounds_framed`]), in normalized device coordinates
/// (`-1..=1`, Y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitFrame {
    /// Left edge.
    pub left: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge.
    pub bottom: f32,
    /// Top edge.
    pub top: f32,
}

impl FitFrame {
    /// The whole view.
    pub const FULL: FitFrame = FitFrame {
        left: -1.0,
        right: 1.0,
        bottom: -1.0,
        top: 1.0,
    };

    /// Smallest share of the view's width or height a frame keeps: insets
    /// that would leave less are scaled down until this much is left.
    pub const MIN_SHARE: f32 = 0.3;

    /// The part of a view of `size` left inside `insets` (`[left, right,
    /// top, bottom]`, in the units of `size`, e.g. logical points). Negative
    /// or non-finite insets count as 0; insets leaving less than
    /// [`FitFrame::MIN_SHARE`] of a dimension are scaled down.
    pub fn from_insets(size: Vec2, insets: [f32; 4]) -> FitFrame {
        let clean = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
        let [l, r, t, b] = insets.map(clean);
        let span = |lo: f32, hi: f32, total: f32| -> (f32, f32) {
            if !total.is_finite() || total < 1.0 {
                return (0.0, 0.0);
            }
            let room = total * (1.0 - Self::MIN_SHARE);
            let scale = if lo + hi > room {
                room / (lo + hi)
            } else {
                1.0
            };
            (lo * scale / total, hi * scale / total)
        };
        let (l, r) = span(l, r, size.x);
        let (t, b) = span(t, b, size.y);
        FitFrame {
            left: -1.0 + 2.0 * l,
            right: 1.0 - 2.0 * r,
            bottom: -1.0 + 2.0 * b,
            top: 1.0 - 2.0 * t,
        }
    }

    /// Edges within `-1..=1`, in order and at least [`FitFrame::MIN_SHARE`]
    /// apart; anything non-finite gives the whole view.
    fn sanitized(self) -> FitFrame {
        let edges = [self.left, self.right, self.bottom, self.top];
        if !edges.iter().all(|v| v.is_finite()) {
            return FitFrame::FULL;
        }
        let axis = |lo: f32, hi: f32| -> (f32, f32) {
            let (lo, hi) = (lo.clamp(-1.0, 1.0), hi.clamp(-1.0, 1.0));
            let min = 2.0 * Self::MIN_SHARE;
            if hi - lo >= min {
                return (lo, hi);
            }
            let mid = ((lo + hi) * 0.5).clamp(-1.0 + min * 0.5, 1.0 - min * 0.5);
            (mid - min * 0.5, mid + min * 0.5)
        };
        let (left, right) = axis(self.left, self.right);
        let (bottom, top) = axis(self.bottom, self.top);
        FitFrame {
            left,
            right,
            bottom,
            top,
        }
    }

    /// Width in NDC units (2 = the whole view).
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    /// Height in NDC units (2 = the whole view).
    pub fn height(&self) -> f32 {
        self.top - self.bottom
    }

    /// The frame's centre in NDC.
    pub fn center(&self) -> (f32, f32) {
        (
            (self.left + self.right) * 0.5,
            (self.bottom + self.top) * 0.5,
        )
    }
}

impl Default for FitFrame {
    fn default() -> Self {
        FitFrame::FULL
    }
}

/// View frustum for culling: a point `p` is inside when
/// `dot(plane.xyz, p) + plane.w >= 0` for all six planes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    /// Extract the planes of a view-projection matrix with clip depth in
    /// `0..=w` (both normal and reversed depth work; infinite far planes
    /// degenerate into "always inside").
    pub fn from_matrix(m: Mat4) -> Frustum {
        let (r0, r1, r2, r3) = (m.row(0), m.row(1), m.row(2), m.row(3));
        Frustum {
            planes: [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2],
        }
    }

    /// Conservative box test: false only when the box is entirely outside.
    pub fn intersects_aabb(&self, min: Vec3, max: Vec3) -> bool {
        if !min.is_finite() || !max.is_finite() {
            return true;
        }
        self.planes.iter().all(|p| {
            let n = p.truncate();
            if n.length_squared() < 1e-24 {
                return p.w >= 0.0;
            }
            let v = Vec3::select(n.cmpge(Vec3::ZERO), max, min);
            n.dot(v) + p.w >= 0.0
        })
    }
}

/// Wrap an angle into `-pi..=pi`.
pub fn wrap_angle(a: f32) -> f32 {
    if !a.is_finite() {
        return 0.0;
    }
    let mut a = a % TAU;
    if a > PI {
        a -= TAU;
    } else if a < -PI {
        a += TAU;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3, eps: f32) -> bool {
        (a - b).length() <= eps
    }

    #[test]
    fn basis_is_orthonormal() {
        let mut cam = Camera::default();
        for &(yaw, pitch) in &[(0.0, 0.0), (1.0, 0.5), (-2.5, -1.2), (0.3, FRAC_PI_2)] {
            cam.yaw = yaw;
            cam.pitch = pitch;
            let (r, u, f) = (cam.right(), cam.up(), cam.forward());
            assert!((r.length() - 1.0).abs() < 1e-5);
            assert!((u.length() - 1.0).abs() < 1e-5);
            assert!((f.length() - 1.0).abs() < 1e-5);
            assert!(r.dot(u).abs() < 1e-5 && r.dot(f).abs() < 1e-5 && u.dot(f).abs() < 1e-5);
        }
    }

    #[test]
    fn presets_look_the_right_way() {
        let mut cam = Camera::default();
        cam.set_preset(ViewPreset::Front);
        assert!(close(cam.forward(), Vec3::Y, 1e-6));
        cam.set_preset(ViewPreset::Top);
        assert!(close(cam.forward(), -Vec3::Z, 1e-6));
        assert!(close(cam.up(), Vec3::Y, 1e-6));
        cam.set_preset(ViewPreset::Right);
        assert!(close(cam.forward(), -Vec3::X, 1e-6));
        cam.set_preset(ViewPreset::Left);
        assert!(close(cam.forward(), Vec3::X, 1e-6));
        cam.set_preset(ViewPreset::Back);
        assert!(close(cam.forward(), -Vec3::Y, 1e-5));
        cam.set_preset(ViewPreset::Bottom);
        assert!(close(cam.forward(), Vec3::Z, 1e-6));
    }

    #[test]
    fn target_projects_to_centre_with_near_depth_order() {
        let cam = Camera::default();
        let size = Vec2::new(800.0, 600.0);
        let p = cam.project_to_screen(cam.target, size).expect("in front");
        assert!((p.x - 400.0).abs() < 1e-2 && (p.y - 300.0).abs() < 1e-2);
        // Reversed Z: nearer points have larger depth.
        let nearer = cam.target - cam.forward() * 50.0;
        let q = cam.project_to_screen(nearer, size).expect("in front");
        assert!(q.z > p.z);
        assert!(p.z > 0.0 && q.z <= 1.0);
    }

    #[test]
    fn behind_camera_is_rejected() {
        let cam = Camera::default();
        let behind = cam.eye() - cam.forward() * 10.0;
        assert!(
            cam.project_to_screen(behind, Vec2::new(100.0, 100.0))
                .is_none()
        );
    }
}
