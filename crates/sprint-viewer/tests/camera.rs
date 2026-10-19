//! Camera math: projection round trips, rays, fitting, zoom/pan/orbit and
//! frustum culling.

use std::f32::consts::{FRAC_PI_2, PI};

use glam::{Vec2, Vec3};
use sprint_viewer::ViewPreset;
use sprint_viewer::camera::{Camera, FitFrame, Frustum, Projection, wrap_angle};

const SIZE: Vec2 = Vec2::new(1280.0, 720.0);

fn cameras() -> Vec<Camera> {
    let mut out = Vec::new();
    for projection in [Projection::Perspective, Projection::Orthographic] {
        for preset in ViewPreset::ALL {
            let mut cam = Camera {
                target: Vec3::new(100.0, 120.0, 15.0),
                distance: 250.0,
                projection,
                ..Camera::default()
            };
            cam.set_preset(preset);
            out.push(cam);
        }
        out.push(Camera {
            target: Vec3::new(-30.0, 10.0, 2.0),
            distance: 12.0,
            yaw: 2.3,
            pitch: -0.7,
            projection,
            ..Camera::default()
        });
    }
    out
}

fn sample_points(cam: &Camera) -> Vec<Vec3> {
    let s = cam.distance * 0.2;
    vec![
        cam.target,
        cam.target + Vec3::new(s, 0.0, 0.0),
        cam.target + Vec3::new(0.0, -s, s * 0.5),
        cam.target + Vec3::new(-s * 0.7, s * 0.3, -s * 0.2),
    ]
}

#[test]
fn project_unproject_round_trip() {
    for cam in cameras() {
        for p in sample_points(&cam) {
            let s = cam
                .project_to_screen(p, SIZE)
                .expect("in front of the camera");
            assert!(s.z > 0.0 && s.z <= 1.0, "depth {} out of range", s.z);
            let back = cam
                .unproject(Vec2::new(s.x, s.y), s.z, SIZE)
                .expect("invertible");
            let err = (back - p).length();
            assert!(
                err <= 1e-3 * cam.distance.max(1.0),
                "{cam:?}: {p:?} -> {s:?} -> {back:?} (err {err})"
            );
        }
    }
}

#[test]
fn screen_rays_pass_through_projected_points() {
    for cam in cameras() {
        for p in sample_points(&cam) {
            let s = cam.project_to_screen(p, SIZE).expect("visible");
            let ray = cam.ray_from_screen(Vec2::new(s.x, s.y), SIZE);
            assert!((ray.dir.length() - 1.0).abs() < 1e-4);
            let t = (p - ray.origin).dot(ray.dir);
            assert!(t > 0.0, "point behind ray origin");
            let dist = (ray.at(t) - p).length();
            assert!(
                dist <= 1e-3 * cam.distance.max(1.0),
                "{cam:?}: ray misses {p:?} by {dist}"
            );
        }
    }
}

#[test]
fn centre_ray_is_forward_and_edges_match_fov() {
    let cam = Camera::default();
    let center = cam.ray_from_screen(SIZE * 0.5, SIZE);
    assert!((center.dir - cam.forward()).length() < 1e-5);
    let top = cam.ray_from_screen(Vec2::new(SIZE.x * 0.5, 0.0), SIZE);
    let angle = center.dir.angle_between(top.dir);
    assert!((angle - cam.fov_y * 0.5).abs() < 1e-4, "angle {angle}");
}

#[test]
fn orthographic_rays_are_parallel_and_start_in_front_of_the_scene() {
    let mut cam = Camera {
        projection: Projection::Orthographic,
        scene_radius: 200.0,
        ..Camera::default()
    };
    cam.set_preset(ViewPreset::Top);
    let a = cam.ray_from_screen(Vec2::new(10.0, 10.0), SIZE);
    let b = cam.ray_from_screen(Vec2::new(900.0, 600.0), SIZE);
    assert!((a.dir - b.dir).length() < 1e-6);
    // Origins sit above anything within the scene radius of the target.
    assert!(a.origin.z > cam.target.z + cam.scene_radius);
}

#[test]
fn fit_keeps_every_corner_on_screen_and_is_tight() {
    let min = Vec3::new(20.0, 40.0, 0.0);
    let max = Vec3::new(180.0, 110.0, 35.0);
    for projection in [Projection::Perspective, Projection::Orthographic] {
        for preset in ViewPreset::ALL {
            for aspect in [0.5f32, 1.0, 16.0 / 9.0, 3.0] {
                let size = Vec2::new(800.0 * aspect, 800.0);
                let mut cam = Camera {
                    projection,
                    ..Camera::default()
                };
                cam.set_preset(preset);
                cam.fit_bounds(min, max, aspect);
                let mut extent = 0.0f32;
                for i in 0..8 {
                    let c = Vec3::new(
                        if i & 1 == 0 { min.x } else { max.x },
                        if i & 2 == 0 { min.y } else { max.y },
                        if i & 4 == 0 { min.z } else { max.z },
                    );
                    let s = cam.project_to_screen(c, size).expect("corner visible");
                    assert!(
                        s.x >= -0.5 && s.x <= size.x + 0.5 && s.y >= -0.5 && s.y <= size.y + 0.5,
                        "{projection:?} {preset:?} aspect {aspect}: corner {c:?} at {s:?}"
                    );
                    let rel = ((s.truncate() - size * 0.5) / (size * 0.5)).abs();
                    extent = extent.max(rel.max_element());
                }
                assert!(
                    extent > 0.8,
                    "{projection:?} {preset:?} aspect {aspect}: fit too loose ({extent})"
                );
            }
        }
    }
}

#[test]
fn framed_fits_keep_the_box_inside_the_frame() {
    let min = Vec3::new(20.0, 40.0, 0.0);
    let max = Vec3::new(180.0, 110.0, 35.0);
    let size = Vec2::new(1200.0, 800.0);
    // A legend on the left, a card and a slider on the right, a scrub bar
    // at the bottom.
    let insets = [300.0, 360.0, 20.0, 60.0];
    let frame = FitFrame::from_insets(size, insets);
    let to_screen = |ndc_x: f32, ndc_y: f32| {
        Vec2::new((ndc_x + 1.0) * 0.5 * size.x, (1.0 - ndc_y) * 0.5 * size.y)
    };
    let lo = to_screen(frame.left, frame.top);
    let hi = to_screen(frame.right, frame.bottom);
    assert!((lo.x - 300.0).abs() < 0.01 && (hi.x - 840.0).abs() < 0.01);
    assert!((lo.y - 20.0).abs() < 0.01 && (hi.y - 740.0).abs() < 0.01);
    for projection in [Projection::Perspective, Projection::Orthographic] {
        for preset in ViewPreset::ALL {
            let mut cam = Camera {
                projection,
                ..Camera::default()
            };
            cam.set_preset(preset);
            cam.fit_bounds_framed(min, max, size.x / size.y, frame);
            // Perspective shifts near and far corners a little differently.
            let slack = if projection == Projection::Orthographic {
                0.5
            } else {
                12.0
            };
            let mut extent = 0.0f32;
            for i in 0..8 {
                let c = Vec3::new(
                    if i & 1 == 0 { min.x } else { max.x },
                    if i & 2 == 0 { min.y } else { max.y },
                    if i & 4 == 0 { min.z } else { max.z },
                );
                let s = cam.project_to_screen(c, size).expect("corner visible");
                assert!(
                    s.x >= lo.x - slack
                        && s.x <= hi.x + slack
                        && s.y >= lo.y - slack
                        && s.y <= hi.y + slack,
                    "{projection:?} {preset:?}: corner {c:?} at {s:?} outside {lo:?}..{hi:?}"
                );
                let half = (hi - lo) * 0.5;
                let rel = ((s.truncate() - (lo + half)) / half).abs();
                extent = extent.max(rel.max_element());
            }
            assert!(extent > 0.75, "{projection:?} {preset:?}: loose ({extent})");
            let centre = cam
                .project_to_screen((min + max) * 0.5, size)
                .expect("centre visible");
            let want = (lo + hi) * 0.5;
            assert!(
                (centre.truncate() - want).length() < slack.max(2.0),
                "{projection:?} {preset:?}: centre at {centre:?}, frame centre {want:?}"
            );
        }
    }
    // The whole view is the plain fit.
    let mut plain = Camera::default();
    plain.fit_bounds(min, max, 1.5);
    let mut framed = Camera::default();
    framed.fit_bounds_framed(min, max, 1.5, FitFrame::FULL);
    assert_eq!(plain, framed);
    assert_eq!(FitFrame::from_insets(size, [0.0; 4]), FitFrame::FULL);
}

#[test]
fn frames_keep_room_whatever_the_insets() {
    let size = Vec2::new(600.0, 400.0);
    // Insets covering everything are scaled down to leave 30% of the view.
    let frame = FitFrame::from_insets(size, [500.0, 500.0, 1e9, f32::INFINITY]);
    assert!((frame.width() - 0.6).abs() < 1e-4, "{frame:?}");
    assert!((frame.height() - 0.6).abs() < 1e-4, "{frame:?}");
    // The proportions between the two sides are kept.
    assert!((frame.center().0).abs() < 1e-4, "{frame:?}");
    // Garbage counts as no inset.
    let frame = FitFrame::from_insets(size, [f32::NAN, -20.0, 0.0, 0.0]);
    assert_eq!(frame, FitFrame::FULL);
    assert_eq!(FitFrame::from_insets(Vec2::ZERO, [10.0; 4]), FitFrame::FULL);
    // Fitting into a nonsensical frame still gives a usable camera.
    let mut cam = Camera::default();
    let broken = FitFrame {
        left: 0.9,
        right: -0.9,
        bottom: f32::NAN,
        top: 1.0,
    };
    cam.fit_bounds_framed(Vec3::ZERO, Vec3::splat(20.0), 1.5, broken);
    assert!(cam.target.is_finite() && cam.distance.is_finite() && cam.distance > 0.0);
    let squeezed = FitFrame {
        left: 0.95,
        right: 0.96,
        bottom: -1.0,
        top: 1.0,
    };
    let mut cam = Camera::default();
    cam.fit_bounds_framed(Vec3::ZERO, Vec3::splat(20.0), 1.5, squeezed);
    assert!(cam.target.is_finite() && cam.distance < 5_000.0, "{cam:?}");
}

#[test]
fn fit_handles_degenerate_and_invalid_boxes() {
    let mut cam = Camera::default();
    let before = cam;
    cam.fit_bounds(Vec3::splat(f32::NAN), Vec3::ONE, 1.5);
    assert_eq!(cam, before, "invalid boxes are ignored");
    cam.fit_bounds(Vec3::ONE, Vec3::ZERO, 1.5);
    assert_eq!(cam, before, "inverted boxes are ignored");
    cam.fit_bounds(Vec3::splat(7.0), Vec3::splat(7.0), 1.5);
    assert_eq!(cam.target, Vec3::splat(7.0));
    assert!(cam.distance.is_finite() && cam.distance >= Camera::MIN_DISTANCE);
    cam.fit_bounds(Vec3::ZERO, Vec3::ONE, f32::NAN);
    assert!(cam.distance.is_finite());
}

#[test]
fn zoom_keeps_the_anchor_under_the_cursor() {
    for mut cam in cameras() {
        let anchor = cam.target + cam.right() * cam.distance * 0.1 - cam.up() * cam.distance * 0.05;
        let before = cam.project_to_screen(anchor, SIZE).expect("visible");
        cam.zoom(0.6, Some(anchor));
        let after = cam.project_to_screen(anchor, SIZE).expect("visible");
        assert!(
            (before.truncate() - after.truncate()).length() < 0.05,
            "{before:?} -> {after:?}"
        );
        cam.zoom(1.7, Some(anchor));
        let again = cam.project_to_screen(anchor, SIZE).expect("visible");
        assert!((before.truncate() - again.truncate()).length() < 0.05);
    }
}

#[test]
fn zoom_is_clamped_and_ignores_garbage() {
    let mut cam = Camera::default();
    cam.zoom(1e-12, None);
    assert_eq!(cam.distance, Camera::MIN_DISTANCE);
    cam.zoom(1e12, None);
    assert_eq!(cam.distance, Camera::MAX_DISTANCE);
    let before = cam;
    cam.zoom(f32::NAN, None);
    cam.zoom(-2.0, None);
    cam.zoom(0.5, Some(Vec3::splat(f32::INFINITY)));
    assert_eq!(cam.target, before.target);
}

#[test]
fn pan_moves_the_scene_with_the_pointer() {
    for mut cam in cameras() {
        let p = cam.target;
        let before = cam.project_to_screen(p, SIZE).expect("visible");
        let delta = Vec2::new(37.0, -21.0);
        cam.pan(delta, SIZE.y);
        let after = cam.project_to_screen(p, SIZE).expect("visible");
        let moved = after.truncate() - before.truncate();
        assert!(
            (moved - delta).length() < 0.05,
            "{:?}: moved {moved:?}, expected {delta:?}",
            cam.projection
        );
    }
}

#[test]
fn orbit_clamps_pitch_and_wraps_yaw() {
    let mut cam = Camera::default();
    cam.orbit(0.0, 10.0);
    assert_eq!(cam.pitch, FRAC_PI_2);
    cam.orbit(0.0, -10.0);
    assert_eq!(cam.pitch, -FRAC_PI_2);
    cam.yaw = 3.0;
    cam.orbit(0.5, 0.0);
    assert!(cam.yaw >= -PI && cam.yaw <= PI);
    assert!((cam.yaw - (3.5 - 2.0 * PI)).abs() < 1e-5);
    let before = cam;
    cam.orbit(f32::NAN, f32::INFINITY);
    assert_eq!(cam.yaw, before.yaw);
    assert_eq!(cam.pitch, before.pitch);
    assert_eq!(wrap_angle(f32::NAN), 0.0);
    assert!((wrap_angle(7.0) - (7.0 - 2.0 * PI)).abs() < 1e-5);
}

#[test]
fn lerp_hits_endpoints_and_takes_the_short_way_round() {
    let a = Camera {
        yaw: 3.0,
        distance: 100.0,
        ..Camera::default()
    };
    let b = Camera {
        yaw: -3.0,
        distance: 400.0,
        target: Vec3::new(5.0, 6.0, 7.0),
        ..Camera::default()
    };
    let start = a.lerp(&b, 0.0);
    let end = a.lerp(&b, 1.0);
    assert!((start.yaw - a.yaw).abs() < 1e-5 && (start.distance - 100.0).abs() < 1e-3);
    assert!((wrap_angle(end.yaw - b.yaw)).abs() < 1e-5 && (end.distance - 400.0).abs() < 1e-2);
    assert!((end.target - b.target).length() < 1e-5);
    let mid = a.lerp(&b, 0.5);
    // Short way from 3.0 to -3.0 passes through +-pi, not through 0.
    assert!(mid.yaw.abs() > 3.0, "mid yaw {}", mid.yaw);
    assert!(
        (mid.distance - 200.0).abs() < 1e-2,
        "geometric distance blend"
    );
}

#[test]
fn frustum_culls_boxes_outside_the_view() {
    for cam in cameras() {
        let f = Frustum::from_matrix(cam.view_proj(SIZE.x / SIZE.y));
        let t = cam.target;
        assert!(f.intersects_aabb(t - Vec3::ONE, t + Vec3::ONE), "{cam:?}");
        let behind = cam.target - cam.forward() * (cam.distance * 3.0 + cam.scene_radius * 4.0);
        if cam.projection == Projection::Perspective {
            assert!(!f.intersects_aabb(behind - Vec3::ONE, behind + Vec3::ONE));
        }
        let side = cam.target + cam.right() * cam.distance * 50.0;
        assert!(!f.intersects_aabb(side - Vec3::ONE, side + Vec3::ONE));
        // Huge boxes containing the camera are never culled.
        assert!(f.intersects_aabb(Vec3::splat(-1e5), Vec3::splat(1e5)));
        // Non-finite boxes are kept (conservative).
        assert!(f.intersects_aabb(Vec3::splat(f32::NAN), Vec3::ONE));
    }
}

#[test]
fn reversed_depth_prefers_near_points() {
    for cam in cameras() {
        let near = cam.target - cam.forward() * cam.distance * 0.3;
        let far = cam.target + cam.forward() * cam.distance * 0.3;
        let a = cam.project_to_screen(near, SIZE).expect("visible");
        let b = cam.project_to_screen(far, SIZE).expect("visible");
        assert!(a.z > b.z, "{:?}: {} vs {}", cam.projection, a.z, b.z);
    }
}
