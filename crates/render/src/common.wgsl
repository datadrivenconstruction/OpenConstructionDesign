// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
struct U {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    light_vp: mat4x4<f32>,
    light_dir: vec4<f32>,   // xyz: light direction, w: ambient fraction
    eye: vec4<f32>,         // xyz: eye, w: camera distance
    sel: vec4<u32>,         // [x, y): selected elements, [z, w): under the cursor
    accent: vec4<f32>,      // selection colour (linear)
    hover: vec4<f32>,       // hover colour (linear)
    params: vec4<f32>,      // x: dark theme, y: effects, z: shading quality 0/1/2, w: shadows
    clip_min: vec4<f32>,    // w > 0.5: clip box enabled
    clip_max: vec4<f32>,
    view: vec4<f32>,        // xy: frame size in pixels, z: shading-frame divisor, w: ground level
    style: vec4<f32>,       // x: style 0 shaded/1 colours/2 hidden lines/3 wireframe/4 x-ray, y: x-ray coverage, z: no fills, w: ortho eye offset
    cats: array<vec4<f32>, 4>, // coverage of categories 0..15 (tag bits 24-27)
    fx: vec4<f32>,          // x: edges of unselected chunks, y: silhouette, w: surface detail
    sel_n: vec4<u32>,       // x: ranges in sel_tex (0: sel.xy alone), y: texels per row, z: generation
    wind: vec4<f32>,        // x: foliage sway, m at 10 m (0: still), y: time, s
    sky: array<vec4<f32>, 3>, // linear colours: zenith, horizon (w: haze density), ground
    look: vec4<f32>,        // x: edge width in pixels, y: normal-edge weight, z: penumbra, w: reserved
};
@group(0) @binding(0) var<uniform> u: U;
// whole selection: sorted merged [start, end) element ranges, one per texel, rows of sel_n.y
@group(0) @binding(1) var sel_tex: texture_2d<u32>;

// show-as-built animation (reveal.rs): two texels per element, (offset.xyz, alpha) then
// (tint.rgb linear, mix); read only while sel_n.w holds an element count, else nothing is touched
@group(0) @binding(2) var anim_tex: texture_2d<f32>;
const ANIM_ROW: u32 = 2048u;

fn anim_on(e: u32) -> bool { return e < (u.sel_n.w & 0xFFFFFFu); }

fn anim_texel(e: u32, k: u32) -> vec4<f32> {
    let t = e * 2u + k;
    return textureLoad(anim_tex, vec2<i32>(i32(t % ANIM_ROW), i32(t / ANIM_ROW)), 0);
}

fn anim_off(e: u32) -> vec3<f32> {
    if (!anim_on(e)) { return vec3<f32>(0.0); }
    return anim_texel(e, 0u).xyz;
}

fn anim_alpha(e: u32) -> f32 {
    if (!anim_on(e)) { return 1.0; }
    return anim_texel(e, 0u).w;
}

fn anim_tint(e: u32) -> vec4<f32> {
    if (!anim_on(e)) { return vec4<f32>(0.0); }
    return anim_texel(e, 1u);
}

fn sel_range(k: u32) -> vec2<u32> {
    let w = max(u.sel_n.y, 1u);
    return textureLoad(sel_tex, vec2<i32>(i32(k % w), i32(k / w)), 0).xy;
}

// element in the selection: [sel.x, sel.y) bounds it; a set of 2+ ranges is binary searched
fn is_selected(e: u32) -> bool {
    if (e < u.sel.x || e >= u.sel.y) { return false; }
    let n = u.sel_n.x;
    if (n < 2u) { return true; }
    var lo = 0u;
    var hi = n;
    while (lo < hi) {
        let mid = (lo + hi) / 2u;
        if (sel_range(mid).x <= e) { lo = mid + 1u; } else { hi = mid; }
    }
    // lo >= 1: the first range starts at sel.x <= e
    return lo > 0u && e < sel_range(lo - 1u).y;
}

fn is_hovered(e: u32) -> bool {
    return e >= u.sel.z && e < u.sel.w;
}

fn clipped(p: vec3<f32>) -> bool {
    return u.clip_min.w > 0.5 && (any(p < u.clip_min.xyz - vec3<f32>(1e-4)) || any(p > u.clip_max.xyz + vec3<f32>(1e-4)));
}

// world point from pixel coordinates and frame depth
fn world_at(px: vec2<f32>, d: f32) -> vec3<f32> {
    let ndc = vec2<f32>(px.x / u.view.x * 2.0 - 1.0, 1.0 - px.y / u.view.y * 2.0);
    let h = u.inv_view_proj * vec4<f32>(ndc, d, 1.0);
    return h.xyz / h.w;
}

// 4x4 (Bayer) screen-door transparency threshold for a frame pixel
fn bayer4(px: vec2<f32>) -> f32 {
    var m = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
    let x = u32(px.x) & 3u;
    let y = u32(px.y) & 3u;
    return (m[y * 4u + x] + 0.5) / 16.0;
}

// category coverage (tag bits 24-27): 1 - as is, 0 - hidden
fn cat_cov(tag: u32) -> f32 {
    let k = (tag >> 24u) & 15u;
    return u.cats[k >> 2u][k & 3u];
}

// vertex is not drawn: category hidden or section fill disabled
fn vertex_hidden(tag: u32) -> bool {
    return cat_cov(tag) <= 0.0 || (u.style.z > 0.5 && (tag & 0xF0000000u) == 0x10000000u);
}

// distance "from the eye" for effects: in ortho the eye is virtual - the offset is subtracted
fn eye_len(p: vec3<f32>) -> f32 {
    return max(length(p - u.eye.xyz) - u.style.w, 0.02 * u.eye.w);
}

// orbit eye: in ortho, forward from the virtual eye by the offset (ground centre and grid fade)
fn orbit_eye() -> vec3<f32> {
    if (u.style.w <= 0.0) { return u.eye.xyz; }
    let a = u.inv_view_proj * vec4<f32>(0.0, 0.0, 1.0, 1.0);
    let b = u.inv_view_proj * vec4<f32>(0.0, 0.0, 0.5, 1.0);
    let f = normalize(b.xyz / b.w - a.xyz / a.w);
    return u.eye.xyz + f * u.style.w;
}

// face coverage: category transparency and x-ray; 1 - opaque (edges ignore
// category transparency)
fn coverage(tag: u32, e: u32) -> f32 {
    var c = anim_alpha(e);
    if ((tag & 0x80000000u) == 0u) { c = c * cat_cov(tag); }
    if (u.style.x > 3.5) { c = min(c, u.style.y); }
    return c;
}

// face is not drawn: wireframe or screen-door transparency (identical in the pre-pass and in colour)
fn face_dropped(tag: u32, px: vec2<f32>, e: u32) -> bool {
    if (u.style.x > 2.5 && u.style.x < 3.5) { return true; }
    let c = coverage(tag, e);
    return c < 1.0 && bayer4(px) > c;
}

// Sky seen from direction `d` (z up): horizon band, zenith above, ground below.
fn sky_dir(d: vec3<f32>) -> vec3<f32> {
    let up = clamp(d.z, -1.0, 1.0);
    // what a surface mirrors near the horizon is darker than the pale band painted behind the model
    let hz = u.sky[1].xyz * 0.72;
    if (up >= 0.0) { return mix(hz, u.sky[0].xyz, pow(up, 0.55)); }
    return mix(hz, u.sky[2].xyz, smoothstep(0.0, 0.45, -up));
}

// Diffuse ambient light for a surface normal: ground below, sky above, the same colours the
// viewport background is painted with.
fn sky_ambient(n: vec3<f32>) -> vec3<f32> {
    let top = mix(u.sky[1].xyz, u.sky[0].xyz, 0.6);
    return mix(u.sky[2].xyz, top, n.z * 0.5 + 0.5);
}

// Filmic curve (published fit): contrast and soft shoulder, for the realistic style.
fn tone_filmic(c: vec3<f32>) -> vec3<f32> {
    return c * (2.51 * c + vec3<f32>(0.03)) / (c * (2.43 * c + vec3<f32>(0.59)) + vec3<f32>(0.14));
}

// Neutral curve: colours below the shoulder pass unchanged (material colours stay true),
// only values near white are compressed and slightly desaturated.
fn tone_neutral(c0: vec3<f32>) -> vec3<f32> {
    let start = 0.76;
    let d = 1.0 - start;
    var c = max(c0, vec3<f32>(0.0));
    let x = min(c.x, min(c.y, c.z));
    let off = select(0.04, x - 6.25 * x * x, x < 0.08);
    c = c - vec3<f32>(off);
    let peak = max(c.x, max(c.y, c.z));
    if (peak < start) { return c + vec3<f32>(off); }
    let np = 1.0 - d * d / (peak + d - start);
    c = c * (np / peak);
    let g = 1.0 - 1.0 / (0.15 * (peak - np) + 1.0);
    return mix(c, vec3<f32>(np), g);
}

// Distance haze towards the horizon colour; density 0 (plan, ortho) leaves `c` untouched.
fn haze(c: vec3<f32>, dist: f32) -> vec3<f32> {
    let k = u.sky[1].w;
    if (k <= 0.0) { return c; }
    // starts well beyond the camera distance, so the model itself keeps its colours
    let x = max(dist / max(u.eye.w * 10.0, 1.0) - 0.3, 0.0);
    let f = 1.0 - exp(-x * x * 3.0 * k);
    return mix(c, u.sky[1].xyz, clamp(f, 0.0, 0.5));
}
