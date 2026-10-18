// Sprint toolpath viewer shaders.
//
// All lighting happens in linear space. The render target is sRGB (the
// hardware encodes) unless FLAG_ENCODE_GAMMA is set, in which case the
// fragment shaders encode manually (GL fallback).

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: eye position; w: 1 = perspective, 0 = orthographic.
    eye: vec4<f32>,
    // xyz: camera forward; w: world size of a pixel per unit depth
    // (perspective) or absolute (orthographic).
    forward: vec4<f32>,
    // xyz: unit direction towards the key light; w: intensity.
    key_light: vec4<f32>,
    // Bed rectangle: min x, min y, max x, max y.
    bed: vec4<f32>,
    feature_colors: array<vec4<f32>, 16>, // linear rgb
    tool_colors: array<vec4<f32>, 8>,     // linear rgb
    ramp: array<vec4<f32>, 12>,           // sRGB stops (0..1)
    travel_color: vec4<f32>,              // linear rgb + alpha
    retract_color: vec4<f32>,             // linear
    unretract_color: vec4<f32>,           // linear
    plate_color: vec4<f32>,               // linear rgb + alpha
    grid_color: vec4<f32>,                // linear rgb + strength
    grid_bold_color: vec4<f32>,           // linear rgb + strength
    // x: color mode, y: hidden feature mask, z: flags, w: ramp stop count.
    mode: vec4<u32>,
    // x: progress line, y: highlight line, z: line limit, w: hover line.
    lines: vec4<u32>,
    // x: first visible layer, y: last visible layer, z: top layer, w: unused.
    layers: vec4<u32>,
    // x/y: scalar range min/max, z: marker radius in pixels, w: unused.
    range: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

const FLAG_PROGRESS: u32 = 1u;
const FLAG_HIGHLIGHT: u32 = 2u;
const FLAG_LINE_LIMIT: u32 = 4u;
const FLAG_HOVER: u32 = 8u;
const FLAG_LAYER_TOP: u32 = 16u;
const FLAG_ENCODE_GAMMA: u32 = 32u;

const TRAVEL_BIT: u32 = 32u;

// ---------------------------------------------------------------- helpers

fn has_flag(f: u32) -> bool {
    return (g.mode.z & f) != 0u;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(hi, lo, x <= vec3<f32>(0.0031308));
}

fn finish(c: vec4<f32>) -> vec4<f32> {
    if (has_flag(FLAG_ENCODE_GAMMA)) {
        return vec4<f32>(linear_to_srgb(c.rgb), c.a);
    }
    return c;
}

fn ramp(t: f32) -> vec3<f32> {
    let n = clamp(g.mode.w, 2u, 12u);
    let x = clamp(t, 0.0, 1.0) * f32(n - 1u);
    let i = min(u32(x), n - 2u);
    let f = x - f32(i);
    return srgb_to_linear(mix(g.ramp[i].rgb, g.ramp[i + 1u].rgb, f));
}

fn normalized(v: f32) -> f32 {
    let span = g.range.y - g.range.x;
    if (span <= 1e-6) {
        return 0.5;
    }
    return (v - g.range.x) / span;
}

// World size of one pixel at a point (for screen-sized markers).
fn pixel_size_at(p: vec3<f32>) -> f32 {
    if (g.eye.w > 0.5) {
        return max(dot(p - g.eye.xyz, g.forward.xyz), 0.0) * g.forward.w;
    }
    return g.forward.w;
}

fn view_vector(world: vec3<f32>) -> vec3<f32> {
    if (g.eye.w > 0.5) {
        return normalize(g.eye.xyz - world);
    }
    return -g.forward.xyz;
}

// Headlight + key light + hemispherical ambient with a subtle specular.
fn shade(world: vec3<f32>, normal_in: vec3<f32>, base: vec3<f32>, emissive: f32) -> vec3<f32> {
    let v = view_vector(world);
    var n = normalize(normal_in);
    // Interpolated normals can tilt away near silhouettes; keep them facing us.
    let nv = dot(n, v);
    if (nv < 0.0) {
        n = normalize(n - v * nv * 1.01);
    }
    let l = g.key_light.xyz;
    let head = max(dot(n, v), 0.0);
    let key = max(dot(n, l), 0.0);
    let hemi = n.z * 0.5 + 0.5;
    let ambient = mix(vec3<f32>(0.035, 0.036, 0.042), vec3<f32>(0.15, 0.16, 0.18), hemi);
    let diffuse = ambient + vec3<f32>(0.62 * head) + vec3<f32>(1.0, 0.97, 0.92) * (g.key_light.w * key);
    let h = normalize(l + v);
    let spec = pow(max(dot(n, h), 0.0), 56.0) * 0.22 + pow(head, 24.0) * 0.06;
    return base * diffuse + vec3<f32>(spec) + base * emissive;
}

// --------------------------------------------------------- segment attrs

struct SegmentIn {
    @location(2) start: vec3<f32>,
    @location(3) width: f32,
    @location(4) end: vec3<f32>,
    @location(5) height: f32,
    @location(6) line: u32,
    @location(7) layer: u32,
    @location(8) attrs: u32,
    @location(9) scalars: u32,
};

fn feature_of(s: SegmentIn) -> u32 {
    return s.attrs & 31u;
}

fn limited_out(line: u32, layer: u32) -> bool {
    return has_flag(FLAG_LINE_LIMIT) && layer >= g.layers.z && line > g.lines.z;
}

fn extrusion_visible(s: SegmentIn) -> bool {
    if (((g.mode.y >> feature_of(s)) & 1u) != 0u) {
        return false;
    }
    return !limited_out(s.line, s.layer);
}

fn base_color(s: SegmentIn) -> vec3<f32> {
    var c: vec3<f32>;
    switch g.mode.x {
        case 1u: {
            c = ramp(normalized(unpack2x16float(s.scalars).x));
        }
        case 2u: {
            c = ramp(normalized(s.width));
        }
        case 3u: {
            c = ramp(normalized(s.height));
        }
        case 4u: {
            c = g.tool_colors[((s.attrs >> 8u) & 255u) % 8u].rgb;
        }
        case 5u: {
            c = ramp(normalized(f32((s.attrs >> 16u) & 255u)));
        }
        case 6u: {
            c = ramp(normalized(f32(s.layer)));
        }
        case 7u: {
            c = ramp(f32(s.scalars >> 16u) / 65535.0);
        }
        default: {
            c = g.feature_colors[min(feature_of(s), 15u)].rgb;
        }
    }
    return c;
}

fn is_ghost(line: u32, layer: u32) -> bool {
    if (has_flag(FLAG_PROGRESS) && line > g.lines.x) {
        return true;
    }
    return has_flag(FLAG_LAYER_TOP) && layer < g.layers.z;
}

// Not-yet-printed / lower-layer look: a dim, nearly neutral grey that keeps
// a hint of the original hue.
fn ghostify(c: vec3<f32>) -> vec3<f32> {
    let l = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return mix(vec3<f32>(l), c, 0.15) * 0.16 + vec3<f32>(0.02, 0.022, 0.03);
}

// rgb = state-adjusted color, a = emissive.
fn segment_shade(s: SegmentIn) -> vec4<f32> {
    var c = base_color(s);
    var emissive = 0.0;
    if (is_ghost(s.line, s.layer)) {
        c = ghostify(c);
    }
    if (has_flag(FLAG_HOVER) && s.line == g.lines.w) {
        c = mix(c, vec3<f32>(1.0), 0.3);
        emissive = 0.2;
    }
    if (has_flag(FLAG_HIGHLIGHT) && s.line == g.lines.y) {
        c = mix(c, vec3<f32>(1.0, 0.98, 0.85), 0.55);
        emissive = 0.9;
    }
    return vec4<f32>(c, emissive);
}

// Orthonormal frame around a segment direction; mirrors scene::segment_frame.
fn segment_right(dir: vec3<f32>) -> vec3<f32> {
    let r = cross(dir, vec3<f32>(0.0, 0.0, 1.0));
    let len = length(r);
    if (len < 1e-3) {
        return vec3<f32>(1.0, 0.0, 0.0);
    }
    return r / len;
}

const HIDDEN_POS: vec4<f32> = vec4<f32>(0.0, 0.0, -2.0, 1.0);

// ------------------------------------------------------------ lit output

// `/*SAMPLE*/` is replaced by `@interpolate(perspective, sample)` on
// backends with sample-rate shading: lighting is then evaluated per MSAA
// sample, which supersamples the fine bead shading and suppresses moire.
struct LitOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) /*SAMPLE*/ world: vec3<f32>,
    @location(1) /*SAMPLE*/ normal: vec3<f32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
};

@fragment
fn fs_lit(in: LitOut) -> @location(0) vec4<f32> {
    let c = shade(in.world, in.normal, in.color.rgb, in.color.a);
    return finish(vec4<f32>(c, 1.0));
}

// Occluded parts of markers (drawn with an inverted depth test): faint.
@fragment
fn fs_xray(in: LitOut) -> @location(0) vec4<f32> {
    let c = shade(in.world, in.normal, in.color.rgb, in.color.a);
    return finish(vec4<f32>(c, 0.32));
}

// ------------------------------------------------------------------ tubes

struct TubeVertex {
    @location(0) profile: vec4<f32>,
    @location(1) normal: vec4<f32>,
};

@vertex
fn vs_tube(v: TubeVertex, s: SegmentIn) -> LitOut {
    var out: LitOut;
    if (!extrusion_visible(s)) {
        out.clip = HIDDEN_POS;
        return out;
    }
    let d = s.end - s.start;
    let len = length(d);
    var dir = vec3<f32>(1.0, 0.0, 0.0);
    if (len > 1e-5) {
        dir = d / len;
    }
    let right = segment_right(dir);
    let up = cross(right, dir);
    let hw = 0.5 * s.width;
    let hh = 0.5 * s.height;
    let center = mix(s.start, s.end, v.profile.x) - vec3<f32>(0.0, 0.0, hh);
    let world = center + right * (v.profile.y * hw) + up * (v.profile.z * hh) + dir * (v.profile.w * hw);
    // Inverse-scale the profile normal (softened so beads read round).
    let sh = mix(hh, hw, 0.3);
    out.normal = right * (v.normal.x * sh) + up * (v.normal.y * hw) + dir * (v.normal.z * sh);
    out.world = world;
    out.clip = g.view_proj * vec4<f32>(world, 1.0);
    out.color = segment_shade(s);
    return out;
}

// ---------------------------------------------------------------- ribbons
//
// Level of detail for beads only a few pixels wide: a camera-facing strip
// across the bead's silhouette (two triangles instead of 24). The fragment
// shader rebuilds the normal of the elliptic cross-section analytically, so
// shading matches the prisms and averages correctly when sub-pixel.

struct RibbonOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) /*SAMPLE*/ world: vec3<f32>,
    @location(1) /*SAMPLE*/ u: f32,
    @location(2) @interpolate(flat) color: vec4<f32>,
    @location(3) @interpolate(flat) n_mid: vec3<f32>,
    @location(4) @interpolate(flat) n_edge: vec3<f32>,
};

@vertex
fn vs_ribbon(@builtin(vertex_index) vi: u32, s: SegmentIn) -> RibbonOut {
    var out: RibbonOut;
    if (!extrusion_visible(s)) {
        out.clip = HIDDEN_POS;
        return out;
    }
    let along = f32(vi >> 1u);
    let side = f32(vi & 1u) * 2.0 - 1.0;
    let d = s.end - s.start;
    let len = length(d);
    var dir = vec3<f32>(1.0, 0.0, 0.0);
    if (len > 1e-5) {
        dir = d / len;
    }
    let right = segment_right(dir);
    let up = cross(right, dir);
    let a = 0.5 * s.width;
    let b = 0.5 * s.height;
    let mid = (s.start + s.end) * 0.5 - vec3<f32>(0.0, 0.0, b);
    let v = view_vector(mid);
    let vr = dot(v, right);
    let vu = dot(v, up);
    // Silhouette half-chord p, the normal at the silhouette (n_edge) and the
    // mean visible normal (n_mid) of the ellipse x^2/a^2 + y^2/b^2 = 1.
    var p = right * a;
    var n_edge = right;
    var n_mid = v;
    let q = a * a * vu * vu + b * b * vr * vr;
    if (q > 1e-14) {
        p = (right * (a * a * vu) - up * (b * b * vr)) * inverseSqrt(q);
        n_edge = normalize(right * vu - up * vr);
        n_mid = normalize(right * (b * b * vr) + up * (a * a * vu));
    }
    // Never thinner than ~0.6 px so distant beads do not break up.
    let min_half = 0.3 * pixel_size_at(mid);
    let plen = length(p);
    if (plen < min_half) {
        p = p * (min_half / max(plen, 1e-9));
    }
    // Pull the strip towards the viewer to roughly the bead's front surface.
    let support = sqrt(a * a * vr * vr + b * b * vu * vu);
    let end_sign = along * 2.0 - 1.0;
    let base = mix(s.start, s.end, along) - vec3<f32>(0.0, 0.0, b) + dir * (end_sign * 0.5 * a);
    let world = base + p * side + v * (0.7 * support);
    out.world = world;
    out.u = side;
    out.clip = g.view_proj * vec4<f32>(world, 1.0);
    out.color = segment_shade(s);
    out.n_mid = n_mid;
    out.n_edge = n_edge;
    return out;
}

@fragment
fn fs_ribbon(in: RibbonOut) -> @location(0) vec4<f32> {
    let u = clamp(in.u, -1.0, 1.0);
    // Flatten the middle a little: printed beads are squashed on top.
    let w = mix(u, u * u * u, 0.45);
    let n = in.n_mid * sqrt(max(1.0 - w * w, 0.0)) + in.n_edge * w;
    let c = shade(in.world, n, in.color.rgb, in.color.a);
    return finish(vec4<f32>(c, 1.0));
}

// ------------------------------------------------------------ flat output

struct FlatOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@fragment
fn fs_flat(in: FlatOut) -> @location(0) vec4<f32> {
    return finish(in.color);
}

// Extrusions drawn as lines (RenderStyle::Lines), two vertices per instance.
@vertex
fn vs_extrusion_line(@builtin(vertex_index) vi: u32, s: SegmentIn) -> FlatOut {
    var out: FlatOut;
    if (!extrusion_visible(s)) {
        out.clip = HIDDEN_POS;
        return out;
    }
    let p = select(s.start, s.end, vi == 1u) - vec3<f32>(0.0, 0.0, 0.5 * s.height);
    out.clip = g.view_proj * vec4<f32>(p, 1.0);
    let c = segment_shade(s);
    // Cheap line lighting (Kajiya-Kay style diffuse from the tangent) so
    // line mode keeps some depth cues.
    let d = s.end - s.start;
    var t = vec3<f32>(1.0, 0.0, 0.0);
    if (length(d) > 1e-6) {
        t = normalize(d);
    }
    let tl = dot(t, g.key_light.xyz);
    let tv = dot(t, view_vector(p));
    let lit = 0.3 + 0.45 * sqrt(max(1.0 - tl * tl, 0.0)) + 0.4 * sqrt(max(1.0 - tv * tv, 0.0));
    out.color = vec4<f32>(c.rgb * lit * (1.0 + c.a), 1.0);
    return out;
}

// Travel moves: thin translucent lines.
@vertex
fn vs_travel(@builtin(vertex_index) vi: u32, s: SegmentIn) -> FlatOut {
    var out: FlatOut;
    if (limited_out(s.line, s.layer)) {
        out.clip = HIDDEN_POS;
        return out;
    }
    let p = select(s.start, s.end, vi == 1u);
    out.clip = g.view_proj * vec4<f32>(p, 1.0);
    var c = g.travel_color;
    if (is_ghost(s.line, s.layer)) {
        c = vec4<f32>(c.rgb * 0.5, c.a * 0.35);
    }
    if (has_flag(FLAG_HIGHLIGHT) && s.line == g.lines.y) {
        c = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }
    out.color = c;
    return out;
}

// -------------------------------------------------------------- markers

struct MarkerVertex {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

struct MarkerIn {
    @location(2) center: vec3<f32>,
    @location(3) attrs: u32,
    @location(4) layer: u32,
    @location(5) line: u32,
};

@vertex
fn vs_marker(v: MarkerVertex, m: MarkerIn) -> LitOut {
    var out: LitOut;
    if (m.layer < g.layers.x || m.layer > g.layers.y || limited_out(m.line, m.layer)) {
        out.clip = HIDDEN_POS;
        return out;
    }
    if (has_flag(FLAG_LAYER_TOP) && m.layer != g.layers.z) {
        out.clip = HIDDEN_POS;
        return out;
    }
    let radius = clamp(g.range.z * pixel_size_at(m.center), 0.12, 1.2);
    let world = m.center + v.pos * radius;
    out.world = world;
    out.normal = v.normal;
    out.clip = g.view_proj * vec4<f32>(world, 1.0);
    var c = g.retract_color.rgb;
    if ((m.attrs & 1u) != 0u) {
        c = g.unretract_color.rgb;
    }
    if (is_ghost(m.line, m.layer)) {
        c = ghostify(c);
    }
    out.color = vec4<f32>(c, 0.15);
    return out;
}

// ------------------------------------------------------- generic meshes

struct MeshVertex {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_mesh(v: MeshVertex) -> LitOut {
    var out: LitOut;
    out.world = v.pos;
    out.normal = v.normal;
    out.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    out.color = vec4<f32>(srgb_to_linear(v.color.rgb), 0.08);
    return out;
}

// --------------------------------------------------------- overlay lines

struct LineVertex {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_overlay(v: LineVertex) -> FlatOut {
    var out: FlatOut;
    out.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    out.color = vec4<f32>(srgb_to_linear(v.color.rgb), v.color.a);
    return out;
}

// ------------------------------------------------------------ bed plate

struct BedOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
};

@vertex
fn vs_bed(@location(0) pos: vec3<f32>) -> BedOut {
    var out: BedOut;
    out.world = pos;
    out.clip = g.view_proj * vec4<f32>(pos, 1.0);
    return out;
}

// Anti-aliased grid line coverage for lines every `spacing` mm that are
// `width_px` pixels wide; fades out where lines would crowd together.
fn grid_coverage(p: vec2<f32>, spacing: f32, width_px: f32) -> f32 {
    let c = p / spacing;
    let fw = max(fwidth(c), vec2<f32>(1e-6));
    let d = abs(fract(c + vec2<f32>(0.5)) - vec2<f32>(0.5)) / fw;
    let line = clamp(width_px * 0.5 + 0.5 - min(d.x, d.y), 0.0, 1.0);
    let crowd = max(fw.x, fw.y);
    return line * (1.0 - smoothstep(0.12, 0.35, crowd));
}

@fragment
fn fs_bed(in: BedOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let p = in.world.xy;
    let fine = grid_coverage(p, 10.0, 1.0) * g.grid_color.a;
    let bold = grid_coverage(p, 50.0, 1.6) * g.grid_bold_color.a;
    var color = g.plate_color.rgb;
    color = mix(color, g.grid_color.rgb, fine);
    color = mix(color, g.grid_bold_color.rgb, bold);
    // Border: distance to the closest bed edge in pixels.
    let px = max(length(fwidth(p)), 1e-6);
    let edge = min(min(p.x - g.bed.x, g.bed.z - p.x), min(p.y - g.bed.y, g.bed.w - p.y)) / px;
    let border = clamp(2.0 - edge, 0.0, 1.0);
    color = mix(color, g.grid_bold_color.rgb * 1.35, border);
    var alpha = g.plate_color.a;
    if (!front) {
        alpha = alpha * 0.35;
    }
    return finish(vec4<f32>(color, alpha));
}
