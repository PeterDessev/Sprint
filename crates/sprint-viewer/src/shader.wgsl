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
    // x: first visible layer, y: last visible layer, z: top layer, w: layer
    // being printed (with FLAG_CURRENT_LAYER).
    layers: vec4<u32>,
    // x/y: scalar range min/max, z: marker radius in pixels, w: unused.
    range: vec4<f32>,
    // xyz: unit direction towards the fill light; w: intensity.
    fill_light: vec4<f32>,
    // xyz: unit direction towards the rim light; w: intensity.
    rim_light: vec4<f32>,
    // rgb: linear haze color (the background); a: depth-cue strength.
    fog: vec4<f32>,
    // x: depth-cue start distance, y: 1 / (end - start), z/w: unused.
    fog_range: vec4<f32>,
    // x: cavity (groove) darkening, y/z: projected bead thickness in pixels
    // where shading detail fades in (y) and is complete (z), w: exposure.
    shading: vec4<f32>,
    // x: opacity of translucent ghosts, yzw: unused.
    ghost: vec4<f32>,
    // xyz: nozzle tip, w: scale of the tool head mesh.
    head: vec4<f32>,
    // x: shadow radius (mm), y: strength, z: height (mm) above the tip it
    // is cast from, w: unused.
    head_shadow: vec4<f32>,
    // x: highlighted object (index + 1, as in the attributes; 0 = none),
    // yzw: unused.
    objects: vec4<u32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

const FLAG_PROGRESS: u32 = 1u;
const FLAG_HIGHLIGHT: u32 = 2u;
const FLAG_LINE_LIMIT: u32 = 4u;
const FLAG_HOVER: u32 = 8u;
const FLAG_LAYER_TOP: u32 = 16u;
const FLAG_ENCODE_GAMMA: u32 = 32u;
const FLAG_TRANSLUCENT_GHOSTS: u32 = 64u;
const FLAG_HEAD: u32 = 128u;
const FLAG_CURRENT_LAYER: u32 = 256u;

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

// ---------------------------------------------------------------- lighting
//
// A studio rig fixed to the camera, like slicer previews: a warm key light
// from the upper left front, a cool fill from the right, a rim light from
// behind that catches silhouettes, a sky/ground hemisphere and a weak
// headlight so nothing facing the viewer goes black.

const SKY: vec3<f32> = vec3<f32>(0.115, 0.125, 0.15);
const GROUND: vec3<f32> = vec3<f32>(0.03, 0.029, 0.032);
const KEY_COLOR: vec3<f32> = vec3<f32>(1.0, 0.96, 0.9);
const FILL_COLOR: vec3<f32> = vec3<f32>(0.78, 0.86, 1.0);
const HEADLIGHT: f32 = 0.07;

// Keep a normal on the visible side: interpolated normals can tilt away
// near silhouettes.
fn facing(n_in: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    let n = normalize(n_in);
    let nv = dot(n, v);
    if (nv < 0.0) {
        return normalize(n - v * nv * 1.01);
    }
    return n;
}

// Light a surface point with normal `n` (facing the viewer). `gloss`
// scales the specular highlight (0 = matte).
fn light(world: vec3<f32>, n: vec3<f32>, base: vec3<f32>, emissive: f32, gloss: f32) -> vec3<f32> {
    let v = view_vector(world);
    let nv = max(dot(n, v), 0.0);
    let l = g.key_light.xyz;
    let nl = dot(n, l);
    let key = max(nl, 0.0) * g.key_light.w;
    let fill = max(dot(n, g.fill_light.xyz), 0.0) * g.fill_light.w;
    let ambient = mix(GROUND, SKY, n.z * 0.5 + 0.5);
    let diffuse = ambient + KEY_COLOR * key + FILL_COLOR * fill + vec3<f32>(HEADLIGHT * nv);
    let h = normalize(l + v);
    let spec = pow(max(dot(n, h), 0.0), 48.0) * 0.2 * gloss * smoothstep(0.0, 0.2, nl);
    // Rim: grazing surfaces facing away from the camera towards the rim
    // light pick up a soft edge, which separates parts from the background.
    let rim = pow(1.0 - nv, 3.0) * g.rim_light.w * clamp(dot(n, g.rim_light.xyz) + 0.35, 0.0, 1.0);
    return base * (diffuse + vec3<f32>(emissive)) + KEY_COLOR * spec + (base * 0.6 + vec3<f32>(0.4)) * rim;
}

// Generic lit surface (markers, meshes).
fn shade(world: vec3<f32>, normal_in: vec3<f32>, base: vec3<f32>, emissive: f32) -> vec3<f32> {
    let n = facing(normal_in, view_vector(world));
    return light(world, n, base, emissive, 1.0);
}

// Highlight shoulder on the brightest channel: keeps hue and saturation
// while bright lit faces roll off instead of clipping flat.
fn tone(c: vec3<f32>) -> vec3<f32> {
    let x = c * g.shading.w;
    let m = max(x.r, max(x.g, x.b));
    let knee = 0.78;
    if (m <= knee) {
        return x;
    }
    let mapped = knee + (1.0 - knee) * (1.0 - exp(-(m - knee) / (1.0 - knee)));
    return x * (mapped / m);
}

// Gentle depth cueing: distant geometry fades towards the background.
fn depth_cue(world: vec3<f32>, c: vec3<f32>) -> vec3<f32> {
    if (g.fog.a <= 0.0 || g.eye.w < 0.5) {
        return c;
    }
    let d = length(world - g.eye.xyz);
    let f = clamp((d - g.fog_range.x) * g.fog_range.y, 0.0, 1.0);
    return mix(c, g.fog.rgb, f * f * g.fog.a);
}

// Final color of a lit surface: depth cue, then tone.
fn post(world: vec3<f32>, c: vec3<f32>) -> vec3<f32> {
    return tone(depth_cue(world, c));
}

// Soft shadow of the nozzle on whatever lies below it: a blurred disc cast
// from the nozzle body away from the key light, widening and fading with
// the distance below the tip.
fn head_shadow(world: vec3<f32>) -> f32 {
    if (!has_flag(FLAG_HEAD)) {
        return 1.0;
    }
    let below = g.head.z - world.z;
    if (below < -0.05) {
        return 1.0;
    }
    let r0 = max(g.head_shadow.x, 1e-3);
    let depth = max(below, 0.0);
    let r = r0 + 0.2 * depth;
    let away = -g.key_light.xy / max(g.key_light.z, 0.35);
    let centre = g.head.xy + away * (g.head_shadow.z + depth);
    let d = length(world.xy - centre) / r;
    let fade = exp(-depth / (5.0 * r0)) * (r0 / r);
    return 1.0 - g.head_shadow.y * exp(-2.0 * d * d) * fade;
}

// ------------------------------------------------------------ bead shading
//
// Up close a bead is a rounded tube; far away it covers less than a pixel
// and its rounded normals alias (moire) and average out to a flat color, so
// a wall would read the same whichever way it faces. Each bead therefore
// gets a macro normal: the mean normal of what is visible of it, given that
// neighbours hide most of a wall bead's top and most of an infill bead's
// sides. Shading blends from the macro normal to the bead's own normal as
// the bead grows on screen (per instance, identical for prisms and ribbons,
// so LOD switches do not pop), and grooves between beads darken (cavity)
// only once they are resolved.

// Walls: external, inner and overhang perimeters.
const WALL_FEATURES: u32 = 14u;
// Horizontal surfaces: solid, top and bridge infill, gap fill, support
// interface and ironing.
const SURFACE_FEATURES: u32 = 6624u;

struct BeadMacro {
    normal: vec3<f32>,
    // Projected thickness of the bead in pixels.
    pixels: f32,
};

// `right`/`up`: cross-section axes; `a`/`b`: half width / half height;
// `mid`: bead centre.
fn bead_macro(feature: u32, right: vec3<f32>, up: vec3<f32>, a: f32, b: f32, mid: vec3<f32>) -> BeadMacro {
    let v = view_vector(mid);
    let vr = dot(v, right);
    let vu = dot(v, up);
    // Projected areas of the side and top faces that neighbours leave
    // visible: a box for lone beads, sides for walls, tops for surfaces.
    let bit = 1u << min(feature, 31u);
    var ws = b;
    var wt = a;
    if ((bit & WALL_FEATURES) != 0u) {
        ws = 1.0;
        wt = 0.12;
    } else if ((bit & SURFACE_FEATURES) != 0u) {
        ws = 0.12;
        wt = 1.0;
    }
    let mn = right * (vr * ws) + up * (vu * wt);
    let len = length(mn);
    var out: BeadMacro;
    out.normal = v;
    if (len > 1e-6) {
        out.normal = mn / len;
    }
    // Thickness of the bead's cross-section as seen from the camera.
    let k = vr * vr + vu * vu;
    var thick = 2.0 * max(a, b);
    if (k > 1e-4) {
        thick = 2.0 * sqrt((a * a * vu * vu + b * b * vr * vr) / k);
    }
    out.pixels = thick / max(pixel_size_at(mid), 1e-9);
    return out;
}

// Shade a bead fragment from its own normal `n_bead` (facing the viewer),
// its macro normal `m` and its projected thickness in pixels. Grooves
// between beads appear as soon as they can be resolved (a few pixels); the
// bead's own rounded normal takes over from the macro normal only up close,
// so the part's form keeps reading at middle distances.
fn bead_light(world: vec3<f32>, n_bead: vec3<f32>, m: vec3<f32>, pixels: f32, base: vec3<f32>, emissive: f32) -> vec3<f32> {
    let groove_detail = smoothstep(g.shading.y, g.shading.z, pixels);
    let shape_detail = smoothstep(g.shading.z * 0.5, g.shading.z * 2.5, pixels);
    let n = normalize(mix(m, n_bead, shape_detail));
    // Grooves: where the bead turns away from the surface it belongs to.
    let groove = 1.0 - clamp(dot(n_bead, m), 0.0, 1.0);
    let cavity = mix(1.0 - 0.18 * g.shading.x, 1.0 - g.shading.x * groove * groove, groove_detail);
    let c = light(world, n, base, emissive, mix(0.3, 1.0, shape_detail));
    return c * (cavity * head_shadow(world));
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

// Index + 1 of the segment's object, 0 outside any object.
fn object_of(s: SegmentIn) -> u32 {
    return (s.attrs >> 24u) & 255u;
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

// Translucent ghosts: a cool, light grey keeping a little of the hue, so
// the printed part shows through what is still to come.
fn ghost_tint(c: vec3<f32>) -> vec3<f32> {
    let l = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return mix(vec3<f32>(l), c, 0.3) * 0.45 + vec3<f32>(0.1, 0.11, 0.13);
}

// Whether a pass draws an instance. Ghosts are drawn with everything else
// unless they are translucent; then only the ghost pass draws them.
fn in_pass(s: SegmentIn, ghost_pass: bool) -> bool {
    if (!extrusion_visible(s)) {
        return false;
    }
    if (!has_flag(FLAG_TRANSLUCENT_GHOSTS)) {
        return !ghost_pass;
    }
    return is_ghost(s.line, s.layer) == ghost_pass;
}

// rgb = state-adjusted color, a = emissive.
fn segment_shade(s: SegmentIn) -> vec4<f32> {
    var c = base_color(s);
    var emissive = 0.0;
    if (is_ghost(s.line, s.layer)) {
        if (has_flag(FLAG_TRANSLUCENT_GHOSTS)) {
            c = ghost_tint(c);
        } else {
            c = ghostify(c);
        }
    } else if (has_flag(FLAG_CURRENT_LAYER) && s.layer == g.layers.w) {
        // The layer being printed: a touch brighter, with a faint glow.
        c = mix(c, vec3<f32>(1.0, 0.97, 0.9), 0.22);
        emissive = 0.12;
    }
    if (g.objects.x != 0u) {
        if (object_of(s) == g.objects.x) {
            // The highlighted object: brighter, with a warm glow.
            c = mix(c, vec3<f32>(1.0, 0.96, 0.86), 0.35);
            emissive = max(emissive, 0.35);
        } else {
            // Everything else steps back, greyed.
            let grey = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
            c = mix(c, vec3<f32>(grey), 0.7) * 0.42;
        }
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

// The highlighted (selected) line is drawn fatter so it stands out.
fn bead_scale(line: u32) -> f32 {
    if (has_flag(FLAG_HIGHLIGHT) && line == g.lines.y) {
        return 1.7;
    }
    return 1.0;
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
    return finish(vec4<f32>(post(in.world, c), 1.0));
}

// ------------------------------------------------------------------ tubes

struct TubeVertex {
    @location(0) profile: vec4<f32>,
    @location(1) normal: vec4<f32>,
};

struct PrismOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) /*SAMPLE*/ world: vec3<f32>,
    @location(1) /*SAMPLE*/ normal: vec3<f32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
    // xyz: macro normal, w: projected bead thickness (pixels).
    @location(3) @interpolate(flat) macro_n: vec4<f32>,
};

fn tube_vertex(v: TubeVertex, s: SegmentIn, ghost_pass: bool) -> PrismOut {
    var out: PrismOut;
    if (!in_pass(s, ghost_pass)) {
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
    let grow = bead_scale(s.line);
    let hw = 0.5 * s.width * grow;
    let hh = 0.5 * s.height * grow;
    let drop = vec3<f32>(0.0, 0.0, 0.5 * s.height);
    let center = mix(s.start, s.end, v.profile.x) - drop;
    // Pointed end caps close the joints of a polyline; translucent ghosts
    // get flat ends instead, since overlapping caps would double the alpha
    // at every joint.
    let cap = select(hw, 0.0, ghost_pass);
    let world = center + right * (v.profile.y * hw) + up * (v.profile.z * hh) + dir * (v.profile.w * cap);
    // Inverse-scale the profile normal (softened so beads read round).
    let sh = mix(hh, hw, 0.3);
    out.normal = right * (v.normal.x * sh) + up * (v.normal.y * hw) + dir * (v.normal.z * sh);
    out.world = world;
    out.clip = g.view_proj * vec4<f32>(world, 1.0);
    out.color = segment_shade(s);
    let bm = bead_macro(feature_of(s), right, up, hw, hh, (s.start + s.end) * 0.5 - drop);
    out.macro_n = vec4<f32>(bm.normal, bm.pixels);
    return out;
}

@vertex
fn vs_tube(v: TubeVertex, s: SegmentIn) -> PrismOut {
    return tube_vertex(v, s, false);
}

// Translucent ghosts are lit per vertex: cheap with heavy overdraw, and
// the bead detail would not show through the transparency anyway.
fn ghost_vertex(clip: vec4<f32>, world: vec3<f32>, n_bead: vec3<f32>, m: vec4<f32>, color: vec4<f32>) -> FlatOut {
    var out: FlatOut;
    out.clip = clip;
    let shape = smoothstep(g.shading.z * 0.5, g.shading.z * 2.5, m.w);
    let n = normalize(mix(m.xyz, facing(n_bead, view_vector(world)), shape));
    let c = light(world, n, color.rgb, color.a, 0.5);
    out.color = vec4<f32>(post(world, c), g.ghost.x);
    return out;
}

@vertex
fn vs_tube_ghost(v: TubeVertex, s: SegmentIn) -> FlatOut {
    if (!in_pass(s, true)) {
        return hidden_flat();
    }
    let p = tube_vertex(v, s, true);
    return ghost_vertex(p.clip, p.world, p.normal, p.macro_n, p.color);
}

@fragment
fn fs_tube(in: PrismOut) -> @location(0) vec4<f32> {
    let n = facing(in.normal, view_vector(in.world));
    let c = bead_light(in.world, n, in.macro_n.xyz, in.macro_n.w, in.color.rgb, in.color.a);
    return finish(vec4<f32>(post(in.world, c), 1.0));
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
    // xyz: macro normal, w: projected bead thickness (pixels).
    @location(5) @interpolate(flat) macro_n: vec4<f32>,
};

fn ribbon_vertex(vi: u32, s: SegmentIn, ghost_pass: bool) -> RibbonOut {
    var out: RibbonOut;
    if (!in_pass(s, ghost_pass)) {
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
    let grow = bead_scale(s.line);
    let a = 0.5 * s.width * grow;
    let b = 0.5 * s.height * grow;
    let drop = vec3<f32>(0.0, 0.0, 0.5 * s.height);
    let mid = (s.start + s.end) * 0.5 - drop;
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
    let overlap = select(0.5 * a, 0.0, ghost_pass);
    let base = mix(s.start, s.end, along) - drop + dir * (end_sign * overlap);
    let world = base + p * side + v * (0.7 * support);
    out.world = world;
    out.u = side;
    out.clip = g.view_proj * vec4<f32>(world, 1.0);
    out.color = segment_shade(s);
    out.n_mid = n_mid;
    out.n_edge = n_edge;
    let bm = bead_macro(feature_of(s), right, up, a, b, mid);
    out.macro_n = vec4<f32>(bm.normal, bm.pixels);
    return out;
}

@vertex
fn vs_ribbon(@builtin(vertex_index) vi: u32, s: SegmentIn) -> RibbonOut {
    return ribbon_vertex(vi, s, false);
}

@vertex
fn vs_ribbon_ghost(@builtin(vertex_index) vi: u32, s: SegmentIn) -> FlatOut {
    if (!in_pass(s, true)) {
        return hidden_flat();
    }
    let r = ribbon_vertex(vi, s, true);
    // Ribbons only exist for thin beads: the macro normal is enough.
    return ghost_vertex(r.clip, r.world, r.macro_n.xyz, r.macro_n, r.color);
}

@fragment
fn fs_ribbon(in: RibbonOut) -> @location(0) vec4<f32> {
    let u = clamp(in.u, -1.0, 1.0);
    // Flatten the middle a little: printed beads are squashed on top.
    let w = mix(u, u * u * u, 0.45);
    let n_bead = facing(in.n_mid * sqrt(max(1.0 - w * w, 0.0)) + in.n_edge * w, view_vector(in.world));
    let c = bead_light(in.world, n_bead, in.macro_n.xyz, in.macro_n.w, in.color.rgb, in.color.a);
    return finish(vec4<f32>(post(in.world, c), 1.0));
}

// ------------------------------------------------------------ flat output

struct FlatOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

fn hidden_flat() -> FlatOut {
    var out: FlatOut;
    out.clip = HIDDEN_POS;
    return out;
}

@fragment
fn fs_flat(in: FlatOut) -> @location(0) vec4<f32> {
    return finish(in.color);
}

// Extrusions drawn as lines (RenderStyle::Lines), two vertices per instance.
fn extrusion_line(vi: u32, s: SegmentIn, ghost_pass: bool) -> FlatOut {
    var out: FlatOut;
    if (!in_pass(s, ghost_pass)) {
        out.clip = HIDDEN_POS;
        return out;
    }
    let drop = vec3<f32>(0.0, 0.0, 0.5 * s.height);
    let p = select(s.start, s.end, vi == 1u) - drop;
    out.clip = g.view_proj * vec4<f32>(p, 1.0);
    let c = segment_shade(s);
    // Lit per vertex from the bead's macro normal, like distant tubes, so
    // the part keeps its form in line mode too.
    let d = s.end - s.start;
    var dir = vec3<f32>(1.0, 0.0, 0.0);
    if (length(d) > 1e-6) {
        dir = normalize(d);
    }
    let right = segment_right(dir);
    let up = cross(right, dir);
    let bm = bead_macro(feature_of(s), right, up, 0.5 * s.width, 0.5 * s.height, (s.start + s.end) * 0.5 - drop);
    let lit = light(p, bm.normal, c.rgb, c.a, 0.3) * ((1.0 - 0.18 * g.shading.x) * head_shadow(p));
    out.color = vec4<f32>(post(p, lit), select(1.0, g.ghost.x, ghost_pass));
    return out;
}

@vertex
fn vs_extrusion_line(@builtin(vertex_index) vi: u32, s: SegmentIn) -> FlatOut {
    return extrusion_line(vi, s, false);
}

@vertex
fn vs_extrusion_line_ghost(@builtin(vertex_index) vi: u32, s: SegmentIn) -> FlatOut {
    return extrusion_line(vi, s, true);
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
    if (has_flag(FLAG_HOVER) && s.line == g.lines.w) {
        c = vec4<f32>(mix(c.rgb, vec3<f32>(1.0), 0.55), 1.0);
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

// ------------------------------------------------------------- tool head
//
// The nozzle mesh is built once around the origin; the uniform places and
// scales it, so it keeps a readable size at every zoom.

@vertex
fn vs_head(v: MeshVertex) -> LitOut {
    var out: LitOut;
    let world = g.head.xyz + v.pos * g.head.w;
    out.world = world;
    out.normal = v.normal;
    out.clip = g.view_proj * vec4<f32>(world, 1.0);
    out.color = vec4<f32>(srgb_to_linear(v.color.rgb), 0.0);
    return out;
}

// Polished metal: little diffuse, a reflection of the studio's bright
// sky and dark floor, and crisp highlights.
fn head_color(in: LitOut) -> vec3<f32> {
    let v = view_vector(in.world);
    let n = facing(in.normal, v);
    let base = in.color.rgb;
    let r = reflect(-v, n);
    let env = mix(vec3<f32>(0.05, 0.05, 0.06), vec3<f32>(0.75, 0.78, 0.85), smoothstep(-0.35, 0.7, r.z));
    let key = max(dot(n, g.key_light.xyz), 0.0) * g.key_light.w;
    let fill = max(dot(n, g.fill_light.xyz), 0.0) * g.fill_light.w;
    let h = normalize(g.key_light.xyz + v);
    let nh = max(dot(n, h), 0.0);
    let spec = pow(nh, 80.0) * 1.2 + pow(nh, 14.0) * 0.18;
    return base * (0.12 + 0.35 * key + 0.25 * fill) + base * env * 0.65 + KEY_COLOR * spec;
}

@fragment
fn fs_head(in: LitOut) -> @location(0) vec4<f32> {
    return finish(vec4<f32>(post(in.world, head_color(in)), 1.0));
}

// Where the print hides the nozzle (inverted depth test): faint.
@fragment
fn fs_head_xray(in: LitOut) -> @location(0) vec4<f32> {
    return finish(vec4<f32>(post(in.world, head_color(in)), 0.3));
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
    color = color * head_shadow(in.world);
    return finish(vec4<f32>(depth_cue(in.world, color), alpha));
}
