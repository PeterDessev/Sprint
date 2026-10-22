//! CPU-generated geometry: the bead template expanded per instance by the
//! vertex shader, retraction markers, axes, the nozzle-shaped tool head and
//! wireframes.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::options::BedSpec;

/// Vertex of the bead template. The vertex shader places it with
/// `pos = mix(start, end, profile.x) + right * profile.y * hw
///        + up * profile.z * hh + dir * profile.w * hw`
/// (centre line lowered by `hh`), see `segment_frame`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct TubeVertex {
    /// `x`: 0 at the start ring, 1 at the end ring; `y`, `z`: profile
    /// coordinates in half-width / half-height units; `w`: cap extension
    /// along the segment in half-width units.
    pub profile: [f32; 4],
    /// Normal in the same (right, up, dir) frame before the anisotropic
    /// scaling; `w` unused.
    pub normal: [f32; 4],
}

/// Plain lit vertex (markers, axes, tool head).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct MeshVertex {
    /// Position (mm).
    pub pos: [f32; 3],
    /// Unit normal.
    pub normal: [f32; 3],
    /// sRGB color + alpha.
    pub color: [u8; 4],
}

/// Colored line vertex.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct LineVertex {
    /// Position (mm).
    pub pos: [f32; 3],
    /// sRGB color + alpha.
    pub color: [u8; 4],
}

/// Indexed mesh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh<V> {
    /// Vertices.
    pub vertices: Vec<V>,
    /// Triangle (or line) indices.
    pub indices: Vec<u32>,
}

/// Hexagonal bead profile with a flat top and bottom: `(right, up)` in
/// half-width / half-height units.
pub const PROFILE: [[f32; 2]; 6] = [
    [1.0, 0.0],
    [0.5, 1.0],
    [-0.5, 1.0],
    [-1.0, 0.0],
    [-0.5, -1.0],
    [0.5, -1.0],
];

/// Profile normals (before anisotropic scaling): sides point sideways, the
/// flats lean outwards so the bead shades round.
pub const PROFILE_NORMALS: [[f32; 2]; 6] = [
    [1.0, 0.0],
    [0.5, 1.0],
    [-0.5, 1.0],
    [-1.0, 0.0],
    [-0.5, -1.0],
    [0.5, -1.0],
];

/// The orthonormal frame used for a segment direction: `(right, up)`.
/// Mirrors `segment_frame` in the shader.
pub fn segment_frame(dir: Vec3) -> (Vec3, Vec3) {
    let right = dir.cross(Vec3::Z);
    let len = right.length();
    let right = if len < 1e-3 { Vec3::X } else { right / len };
    (right, right.cross(dir))
}

/// Place a template vertex for a segment, exactly like the shader.
pub fn place_tube_vertex(v: &TubeVertex, start: Vec3, end: Vec3, hw: f32, hh: f32) -> Vec3 {
    let d = end - start;
    let len = d.length();
    let dir = if len > 1e-5 { d / len } else { Vec3::X };
    let (right, up) = segment_frame(dir);
    let p = v.profile;
    start.lerp(end, p[0]) - Vec3::Z * hh
        + right * (p[1] * hw)
        + up * (p[2] * hh)
        + dir * (p[3] * hw)
}

/// The bead template: two hexagonal rings plus a pointed cap at each end.
/// 14 vertices, 24 triangles, wound counter-clockwise when seen from
/// outside.
pub fn tube_template() -> Mesh<TubeVertex> {
    let mut vertices = Vec::with_capacity(14);
    for ring in 0..2 {
        for (p, n) in PROFILE.iter().zip(PROFILE_NORMALS.iter()) {
            vertices.push(TubeVertex {
                profile: [ring as f32, p[0], p[1], 0.0],
                normal: [n[0], n[1], 0.0, 0.0],
            });
        }
    }
    let tip_start = vertices.len() as u32;
    vertices.push(TubeVertex {
        profile: [0.0, 0.0, 0.0, -1.0],
        normal: [0.0, 0.0, -1.0, 0.0],
    });
    let tip_end = vertices.len() as u32;
    vertices.push(TubeVertex {
        profile: [1.0, 0.0, 0.0, 1.0],
        normal: [0.0, 0.0, 1.0, 0.0],
    });

    let mut tris: Vec<[u32; 3]> = Vec::with_capacity(24);
    let k = PROFILE.len() as u32;
    for i in 0..k {
        let j = (i + 1) % k;
        let (s0, s1, e0, e1) = (i, j, k + i, k + j);
        tris.push([s0, s1, e1]);
        tris.push([s0, e1, e0]);
        tris.push([tip_start, s1, s0]);
        tris.push([tip_end, e0, e1]);
    }
    let mut mesh = Mesh {
        vertices,
        indices: Vec::with_capacity(72),
    };
    // Fix the winding against a reference placement so every triangle faces
    // outwards (the frame is left-handed, so do not trust intuition here).
    let (start, end, hw, hh) = (Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), 0.5, 0.25);
    for t in tris {
        let p: Vec<Vec3> = t
            .iter()
            .map(|&i| place_tube_vertex(&mesh.vertices[i as usize], start, end, hw, hh))
            .collect();
        let normal = (p[1] - p[0]).cross(p[2] - p[0]);
        let centroid = (p[0] + p[1] + p[2]) / 3.0;
        let axis = Vec3::new(centroid.x.clamp(0.0, 2.0), 0.0, -hh);
        if normal.dot(centroid - axis) >= 0.0 {
            mesh.indices.extend_from_slice(&t);
        } else {
            mesh.indices.extend_from_slice(&[t[0], t[2], t[1]]);
        }
    }
    mesh
}

/// Unit octahedron with flat-shaded faces (24 vertices) for retraction
/// markers; positions are unit offsets scaled in the shader.
pub fn marker_template() -> Mesh<MeshVertex> {
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    let mut mesh = Mesh::default();
    for sx in [-1.0f32, 1.0] {
        for sy in [-1.0f32, 1.0] {
            for sz in [-1.0f32, 1.0] {
                let a = axes[0] * sx;
                let b = axes[1] * sy;
                let c = axes[2] * sz;
                let n = Vec3::new(sx, sy, sz).normalize();
                let base = mesh.vertices.len() as u32;
                let tri = if (b - a).cross(c - a).dot(n) > 0.0 {
                    [a, b, c]
                } else {
                    [a, c, b]
                };
                for p in tri {
                    mesh.vertices.push(MeshVertex {
                        pos: p.to_array(),
                        normal: n.to_array(),
                        color: [255; 4],
                    });
                }
                mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
            }
        }
    }
    mesh
}

/// Append a closed cone/cylinder frustum between `a` (radius `ra`) and `b`
/// (radius `rb`) along an arbitrary axis, with flat end discs.
fn push_frustum(
    mesh: &mut Mesh<MeshVertex>,
    a: Vec3,
    b: Vec3,
    ra: f32,
    rb: f32,
    color: [u8; 4],
    sides: u32,
) {
    let axis = (b - a).normalize_or(Vec3::Z);
    let (u, v) = {
        let helper = if axis.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
        let u = axis.cross(helper).normalize();
        (u, axis.cross(u))
    };
    let len = (b - a).length().max(1e-6);
    let slope = (ra - rb) / len;
    let base = mesh.vertices.len() as u32;
    for i in 0..=sides {
        let ang = i as f32 / sides as f32 * std::f32::consts::TAU;
        let radial = u * ang.cos() + v * ang.sin();
        let n = (radial + axis * slope).normalize();
        for (c, r) in [(a, ra), (b, rb)] {
            mesh.vertices.push(MeshVertex {
                pos: (c + radial * r).to_array(),
                normal: n.to_array(),
                color,
            });
        }
    }
    for i in 0..sides {
        let s = base + i * 2;
        // (a_i, b_i, a_{i+1}, b_{i+1})
        mesh.indices
            .extend_from_slice(&[s, s + 2, s + 1, s + 1, s + 2, s + 3]);
    }
    // Caps.
    for (c, r, n) in [(a, ra, -axis), (b, rb, axis)] {
        if r <= 1e-6 {
            continue;
        }
        let center = mesh.vertices.len() as u32;
        mesh.vertices.push(MeshVertex {
            pos: c.to_array(),
            normal: n.to_array(),
            color,
        });
        for i in 0..=sides {
            let ang = i as f32 / sides as f32 * std::f32::consts::TAU;
            let radial = u * ang.cos() + v * ang.sin();
            mesh.vertices.push(MeshVertex {
                pos: (c + radial * r).to_array(),
                normal: n.to_array(),
                color,
            });
        }
        for i in 0..sides {
            let (p, q) = (center + 1 + i, center + 2 + i);
            if n.dot(axis) > 0.0 {
                mesh.indices.extend_from_slice(&[center, p, q]);
            } else {
                mesh.indices.extend_from_slice(&[center, q, p]);
            }
        }
    }
    fix_winding_outward(mesh, base);
}

/// Flip triangles appended after vertex `first_vertex` whose geometric
/// normal disagrees with the stored vertex normals.
fn fix_winding_outward(mesh: &mut Mesh<MeshVertex>, first_vertex: u32) {
    for tri in mesh.indices.as_chunks_mut::<3>().0 {
        if tri[0] < first_vertex {
            continue;
        }
        let p = |i: u32| Vec3::from(mesh.vertices[i as usize].pos);
        let n = |i: u32| Vec3::from(mesh.vertices[i as usize].normal);
        let geo = (p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0]));
        let avg = n(tri[0]) + n(tri[1]) + n(tri[2]);
        if geo.dot(avg) < 0.0 {
            tri.swap(1, 2);
        }
    }
}

/// XYZ axes with arrow heads at `origin` plus a small origin cube.
pub fn axes_mesh(origin: Vec3, length: f32) -> Mesh<MeshVertex> {
    let mut mesh = Mesh::default();
    let shaft = (length * 0.025).max(0.2);
    let head_len = length * 0.2;
    let head_r = shaft * 2.6;
    let colors = [
        [0xE5, 0x48, 0x4D, 0xFF],
        [0x4C, 0xC3, 0x5A, 0xFF],
        [0x3E, 0x7B, 0xF0, 0xFF],
    ];
    for (axis, color) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().zip(colors) {
        let tip = origin + axis * length;
        let neck = tip - axis * head_len;
        push_frustum(&mut mesh, origin, neck, shaft, shaft, color, 12);
        push_frustum(&mut mesh, neck, tip, head_r, 0.0, color, 16);
    }
    push_frustum(
        &mut mesh,
        origin - Vec3::Z * shaft * 1.6,
        origin + Vec3::Z * shaft * 1.6,
        shaft * 1.8,
        shaft * 1.8,
        [0xE8, 0xE8, 0xEC, 0xFF],
        16,
    );
    mesh
}

/// Append a closed prism with `sides` flat faces (e.g. a hex nut) between
/// `a` and `b`, faces shaded flat.
fn push_flat_prism(
    mesh: &mut Mesh<MeshVertex>,
    a: Vec3,
    b: Vec3,
    radius: f32,
    color: [u8; 4],
    sides: u32,
) {
    let axis = (b - a).normalize_or(Vec3::Z);
    let helper = if axis.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
    let u = axis.cross(helper).normalize();
    let v = axis.cross(u);
    let corner = |i: u32| {
        let ang = (i as f32 + 0.5) / sides as f32 * std::f32::consts::TAU;
        u * ang.cos() + v * ang.sin()
    };
    let first = mesh.vertices.len() as u32;
    for i in 0..sides {
        let (c0, c1) = (corner(i), corner(i + 1));
        let n = (c0 + c1).normalize();
        let base = mesh.vertices.len() as u32;
        for p in [
            a + c0 * radius,
            b + c0 * radius,
            a + c1 * radius,
            b + c1 * radius,
        ] {
            mesh.vertices.push(MeshVertex {
                pos: p.to_array(),
                normal: n.to_array(),
                color,
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 2, base + 1, base + 1, base + 2, base + 3]);
    }
    for (c, n) in [(a, -axis), (b, axis)] {
        let center = mesh.vertices.len() as u32;
        mesh.vertices.push(MeshVertex {
            pos: c.to_array(),
            normal: n.to_array(),
            color,
        });
        for i in 0..sides {
            mesh.vertices.push(MeshVertex {
                pos: (c + corner(i) * radius).to_array(),
                normal: n.to_array(),
                color,
            });
        }
        for i in 0..sides {
            let (p, q) = (center + 1 + i, center + 1 + (i + 1) % sides);
            mesh.indices.extend_from_slice(&[center, p, q]);
        }
    }
    fix_winding_outward(mesh, first);
}

/// Height of [`tool_head_mesh`] above its tip (mm).
pub const TOOL_HEAD_HEIGHT: f32 = 7.0;

/// Radius of the widest part of [`tool_head_mesh`] (mm).
pub const TOOL_HEAD_RADIUS: f32 = 1.9;

/// Nozzle-shaped tool marker with its tip at `tip`, [`TOOL_HEAD_HEIGHT`]
/// tall: a small brass cone and hex nut under a steel heat break. The
/// viewer scales it with the zoom so it keeps a readable size on screen.
pub fn tool_head_mesh(tip: Vec3) -> Mesh<MeshVertex> {
    let mut mesh = Mesh::default();
    let brass = [0xE6, 0xB0, 0x55, 0xFF];
    let nut = [0xC9, 0x93, 0x3C, 0xFF];
    let steel = [0xB8, 0xC0, 0xCE, 0xFF];
    let up = Vec3::Z;
    push_frustum(&mut mesh, tip, tip + up * 0.3, 0.16, 0.36, brass, 24);
    push_frustum(
        &mut mesh,
        tip + up * 0.3,
        tip + up * 2.1,
        0.36,
        1.2,
        brass,
        24,
    );
    push_flat_prism(
        &mut mesh,
        tip + up * 2.1,
        tip + up * 3.5,
        TOOL_HEAD_RADIUS,
        nut,
        6,
    );
    push_frustum(
        &mut mesh,
        tip + up * 3.5,
        tip + up * TOOL_HEAD_HEIGHT,
        0.8,
        0.8,
        steel,
        16,
    );
    mesh
}

/// Number of vertices of [`tool_head_mesh`] (the same wherever the tip is).
pub fn tool_head_vertex_count() -> usize {
    tool_head_mesh(Vec3::ZERO).vertices.len()
}

/// Build-volume wireframe (12 edges) as a line list.
pub fn build_volume_lines(bed: &BedSpec) -> Vec<LineVertex> {
    let v = bed.volume();
    let (min, max) = (Vec3::from(v.min), Vec3::from(v.max));
    let corner = |i: u32| {
        Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        )
    };
    let color = [0x8C, 0x98, 0xB4, 0x8C];
    let mut out = Vec::with_capacity(24);
    for i in 0..8u32 {
        for bit in [1u32, 2, 4] {
            if i & bit == 0 {
                out.push(LineVertex {
                    pos: corner(i).to_array(),
                    color,
                });
                out.push(LineVertex {
                    pos: corner(i | bit).to_array(),
                    color,
                });
            }
        }
    }
    out
}

/// The bed plate as two triangles slightly below Z = 0 (4 vertices).
pub fn bed_quad(bed: &BedSpec, z: f32) -> [[f32; 3]; 4] {
    let min = bed.min_xy();
    let max = bed.max_xy();
    [
        [min[0], min[1], z],
        [max[0], min[1], z],
        [max[0], max[1], z],
        [min[0], max[1], z],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_outward(start: Vec3, end: Vec3) {
        let mesh = tube_template();
        let (hw, hh) = (0.3, 0.1);
        let d = (end - start).normalize_or(Vec3::X);
        for tri in mesh.indices.chunks_exact(3) {
            let p: Vec<Vec3> = tri
                .iter()
                .map(|&i| place_tube_vertex(&mesh.vertices[i as usize], start, end, hw, hh))
                .collect();
            let n = (p[1] - p[0]).cross(p[2] - p[0]);
            let c = (p[0] + p[1] + p[2]) / 3.0;
            let len = (end - start).length();
            let along = (c - start).dot(d).clamp(0.0, len);
            let axis = start + d * along - Vec3::Z * hh;
            assert!(
                n.dot(c - axis) > 0.0,
                "inward triangle {tri:?} for {start:?}->{end:?}"
            );
        }
    }

    #[test]
    fn tube_template_faces_outwards_in_every_direction() {
        let mesh = tube_template();
        assert_eq!(mesh.vertices.len(), 14);
        assert_eq!(mesh.indices.len(), 72);
        let o = Vec3::new(10.0, 20.0, 0.3);
        for dir in [
            Vec3::X,
            -Vec3::X,
            Vec3::Y,
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-3.0, 1.0, 0.2),
            Vec3::Z,
            -Vec3::Z,
        ] {
            check_outward(o, o + dir * 5.0);
        }
    }

    #[test]
    fn tube_vertices_stay_inside_bead_box() {
        let mesh = tube_template();
        let (s, e, hw, hh) = (Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), 0.25, 0.1);
        for v in &mesh.vertices {
            let p = place_tube_vertex(v, s, e, hw, hh);
            assert!(p.z <= 1e-6 && p.z >= -2.0 * hh - 1e-6, "{p:?}");
            assert!(p.y.abs() <= hw + 1e-6);
            assert!(p.x >= -hw - 1e-6 && p.x <= 10.0 + hw + 1e-6);
        }
    }

    #[test]
    fn meshes_are_consistent() {
        for mesh in [
            marker_template(),
            axes_mesh(Vec3::ZERO, 20.0),
            tool_head_mesh(Vec3::ONE),
        ] {
            assert_eq!(mesh.indices.len() % 3, 0);
            assert!(
                mesh.indices
                    .iter()
                    .all(|&i| (i as usize) < mesh.vertices.len())
            );
            for tri in mesh.indices.chunks_exact(3) {
                let p = |i: u32| Vec3::from(mesh.vertices[i as usize].pos);
                let n = |i: u32| Vec3::from(mesh.vertices[i as usize].normal);
                let geo = (p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0]));
                assert!(geo.dot(n(tri[0]) + n(tri[1]) + n(tri[2])) >= 0.0);
            }
        }
        assert_eq!(
            tool_head_vertex_count(),
            tool_head_mesh(Vec3::new(5.0, 1.0, 2.0)).vertices.len()
        );
        // The head stands on its tip, within its documented size.
        let tip = Vec3::new(5.0, 1.0, 2.0);
        let head = tool_head_mesh(tip);
        let lowest = head
            .vertices
            .iter()
            .map(|v| v.pos[2])
            .fold(f32::MAX, f32::min);
        let highest = head
            .vertices
            .iter()
            .map(|v| v.pos[2])
            .fold(f32::MIN, f32::max);
        assert!((lowest - tip.z).abs() < 1e-5);
        assert!((highest - tip.z - TOOL_HEAD_HEIGHT).abs() < 1e-4);
        for v in &head.vertices {
            let r = (Vec3::from(v.pos) - tip).truncate().length();
            assert!(r <= TOOL_HEAD_RADIUS + 1e-4, "{r}");
        }
        assert_eq!(build_volume_lines(&BedSpec::default()).len(), 24);
    }
}
