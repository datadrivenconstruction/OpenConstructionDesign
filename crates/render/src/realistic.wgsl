// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
// Realistic style: procedural surfaces by category, sky reflection and sun highlight. Appended to
// mesh.wgsl as its own module, so a failure here leaves the shaded pipelines intact.

// Lattice hash for value noise: integer cell coordinates, odd multipliers, then a public-domain
// 32-bit avalanche finalizer (xorshift-multiply). Integer maths, so equal on every GPU.
fn hash21(p: vec2<f32>) -> f32 {
    let c = vec2<u32>(bitcast<u32>(i32(floor(p.x))), bitcast<u32>(i32(floor(p.y))));
    var h = (c.x * 0x9e3779b1u) ^ (c.y * 0x85ebca77u);
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    h = h * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return f32(h >> 8u) / 16777216.0;
}

fn vnoise2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let s = f * f * (vec2<f32>(3.0) - 2.0 * f);
    return mix(mix(hash21(i), hash21(i + vec2<f32>(1.0, 0.0)), s.x),
               mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), s.x), s.y);
}

// texture layers (group 2): x: 1 / tile size in metres, y: cos, z: sin of the rotation, w: roughness
@group(2) @binding(0) var tex_arr: texture_2d_array<f32>;
@group(2) @binding(1) var tex_smp: sampler;
@group(2) @binding(2) var<uniform> tex_par: array<vec4<f32>, 64>;

// element anchors: two texels per element, (origin, 1) then (cos, sin); w of the first is 0 when
// the element has none and is projected in world space
@group(2) @binding(3) var tex_anchor: texture_2d<f32>;

struct Frame {
    on: bool,
    o: vec3<f32>,
    c: f32,
    s: f32,
};

fn anchor_frame(elem: u32) -> Frame {
    var f: Frame;
    f.on = false;
    f.o = vec3<f32>(0.0);
    f.c = 1.0;
    f.s = 0.0;
    if (elem < 0x48000000u || elem - 0x48000000u >= 65536u) { return f; }
    let e = elem - 0x48000000u;
    let x = i32((e % 1024u) * 2u);
    let y = i32(e / 1024u);
    let a = textureLoad(tex_anchor, vec2<i32>(x, y), 0);
    if (a.w < 0.5) { return f; }
    let b = textureLoad(tex_anchor, vec2<i32>(x + 1, y), 0);
    f.on = true;
    f.o = a.xyz;
    f.c = b.x;
    f.s = b.y;
    return f;
}

// world vector to the element frame (rotation about the vertical axis only)
fn to_frame(v: vec3<f32>, f: Frame) -> vec3<f32> {
    return vec3<f32>(f.c * v.x + f.s * v.y, -f.s * v.x + f.c * v.y, v.z);
}

// surface coordinates: the two world axes best facing the normal
fn surface_uv(p: vec3<f32>, n: vec3<f32>) -> vec2<f32> {
    let a = abs(n);
    if (a.z >= a.x && a.z >= a.y) { return p.xy; }
    if (a.x >= a.y) { return p.yz; }
    return p.xz;
}

// the axis `surface_uv` takes as u (k = 0) or v (k = 1)
fn surface_axis(n: vec3<f32>, k: u32) -> vec3<f32> {
    let a = abs(n);
    if (a.z >= a.x && a.z >= a.y) { return select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), k == 1u); }
    if (a.x >= a.y) { return select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), k == 1u); }
    return select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), k == 1u);
}

// element frame vector back to world (inverse of to_frame)
fn from_frame(v: vec3<f32>, f: Frame) -> vec3<f32> {
    return vec3<f32>(f.c * v.x - f.s * v.y, f.s * v.x + f.c * v.y, v.z);
}

// table w: roughness + 2 * metallic eighths (`pack_surface` in texture.rs)
fn tex_surface(w: f32) -> vec2<f32> {
    let m = floor(w * 0.5);
    return vec2<f32>(w - 2.0 * m, m / 8.0);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// x: brightness variation, y: roughness
fn surface(tag: u32, uv: vec2<f32>) -> vec2<f32> {
    let fine = vnoise2(uv * 24.0);
    let wide = vnoise2(uv * 2.5);
    switch (tag) {
        case 5u: { return vec2<f32>(0.82 + 0.3 * vnoise2(vec2<f32>(uv.x * 40.0, uv.y * 1.5)), 0.45); }
        case 8u, 25u: { return vec2<f32>(1.0, 0.08); }
        case 9u: { return vec2<f32>(1.0, 0.3); }
        case 11u: { return vec2<f32>(0.9 + 0.12 * fine + 0.06 * wide, 0.8); }
        case 20u, 23u: { return vec2<f32>(0.78 + 0.4 * fine, 0.95); }
        case 21u: { return vec2<f32>(0.72 + 0.5 * fine * wide + 0.1 * wide, 0.95); }
        case 22u: { return vec2<f32>(0.85 + 0.3 * vnoise2(uv * 60.0), 0.9); }
        case 24u: { return vec2<f32>(0.85 + 0.2 * step(0.06, fract(uv.x * 5.0)) * step(0.06, fract(uv.y * 5.0)), 0.8); }
        default: { return vec2<f32>(0.95 + 0.06 * fine + 0.04 * wide, 0.7); }
    }
}

// Foliage: a soft mass of leaves. Wrapped diffuse (light reaches past the terminator), no sun
// highlight (roughness 0.9), light through the leaves when the crown is seen against the sun,
// and a tone of its own per placement (hue towards yellow-green or blue-green, value).
fn foliage(base0: vec3<f32>, n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, world: vec3<f32>, px: vec2<f32>, seed: u32) -> vec3<f32> {
    let h1 = f32(seed & 0xFFFFu) / 65535.0;
    let h2 = f32(seed >> 16u) / 65535.0;
    let hue = mix(vec3<f32>(0.90, 1.0, 1.06), vec3<f32>(1.10, 1.03, 0.80), h1);
    // a cluster of leaves of its own tone: hue and value drift over the crown
    let cl = vnoise(world * 1.9 + vec3<f32>(f32(seed & 255u), 0.0, 0.0));
    let drift = mix(vec3<f32>(0.86, 0.97, 1.08), vec3<f32>(1.14, 1.04, 0.78), smoothstep(0.2, 0.8, cl));
    var base = base0 * hue * drift * (0.70 + 0.26 * h2 + 0.30 * cl) * (0.66 + 0.5 * smoothstep(0.25, 0.7, vnoise(world * 3.3 + vec3<f32>(1.7, 9.1, 3.3))));
    // less saturated greens
    let grey = vec3<f32>(dot(base, vec3<f32>(0.3, 0.59, 0.11)));
    base = mix(base, grey, 0.28);
    let ndl = dot(n, l);
    let wrap = clamp((ndl + 0.5) / 1.5, 0.0, 1.0);
    let shadow = sun_light(world, n);
    var ao = ambient_occ(px);
    if (u.params.z < 0.5) { ao = mix(0.8, 1.0, smoothstep(0.0, 0.9, world.z - u.view.w)); }
    let sky = vec3<f32>(0.80, 0.88, 1.0);
    let bounce = vec3<f32>(0.42, 0.39, 0.35);
    let hemi = mix(bounce, sky, n.z * 0.5 + 0.5);
    let sun_c = vec3<f32>(1.0, 0.95, 0.86) * 1.15;
    var c = base * (hemi * 0.62 * ao + sun_c * wrap * shadow * 0.85);
    // translucency: the side away from the sun glows when the eye looks towards the sun
    let against = pow(clamp(dot(v, -l), 0.0, 1.0), 3.0);
    let through = clamp(0.35 - ndl, 0.0, 1.0);
    c += base * vec3<f32>(0.85, 1.0, 0.45) * against * through * mix(0.35, 1.0, shadow) * 0.9;
    // only a faint sky sheen at grazing angles (rough)
    let fres = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 5.0);
    c = mix(c, mix(bounce, sky, 0.7) * ao, fres * 0.1 * 0.5);
    return tone_filmic(c);
}

@fragment
fn fs_real(i: VOut) -> @location(0) vec4<f32> {
    // derivatives first, in uniform control flow; sampling below uses explicit gradients
    let dp_x = dpdx(i.world);
    let dp_y = dpdy(i.world);
    let duv_x = dpdx(i.uv);
    let duv_y = dpdy(i.uv);
    if (i.tag >= 0x80000000u || clipped(i.world) || face_dropped(i.tag, i.clip.xy, i.elem)) { discard; }
    let selected = is_selected(i.elem);
    let hovered = is_hovered(i.elem);
    var n = normalize(i.nrm);
    let to_eye = u.eye.xyz - i.world;
    if (dot(n, to_eye) < 0.0) { n = -n; }
    if ((i.tag & 0xF0000000u) == 0x10000000u) {
        let pc = vec3<f32>(f32((i.tag >> 16u) & 255u), f32((i.tag >> 8u) & 255u), f32(i.tag & 255u)) / 255.0;
        return vec4<f32>(mark(pow(pc, vec3<f32>(2.2)), n, to_eye, selected, hovered, i.elem), 1.0);
    }
    let tag = base_tag(i.tag);
    let v = normalize(to_eye);
    if (is_card(i.tag)) {
        let tx = card_texel(i.uv, duv_x, duv_y, i.tag);
        if (tx.x < 0.5) { discard; }
        let leaves = foliage(palette(tag) * card_tone(i.tag, tx.y), normalize(i.snrm), v, normalize(-u.light_dir.xyz), i.world, i.clip.xy, i.seed);
        return vec4<f32>(mark(leaves, n, to_eye, selected, hovered, i.elem), 1.0);
    }
    if ((i.tag & 0xF0000000u) == 0x50000000u) {
        if (leaf_cut(i.world, i.snrm, length(dp_x) + length(dp_y))) { discard; }
        // the inside of the crown seen through its holes: turned normal, darker
        let back = dot(normalize(i.nrm), to_eye) < 0.0;
        let sn = select(normalize(i.snrm), -normalize(i.snrm), back);
        let leaves = foliage(palette(tag), sn, v, normalize(-u.light_dir.xyz), i.world, i.clip.xy, i.seed) * select(1.0, 0.55, back);
        return vec4<f32>(mark(leaves, n, to_eye, selected, hovered, i.elem), 1.0);
    }
    let dist = eye_len(i.world);
    // fades with distance: a pattern finer than a pixel would only shimmer
    let detail = u.fx.w * (1.0 - smoothstep(25.0, 120.0, dist));
    let s = surface(tag, surface_uv(i.world, n));
    let tone = mix(1.0, s.x, detail);
    var rough = s.y;
    var metal = 0.0;
    var base = palette(tag) * tone;
    // image texture: world-metre box projection, flat colour when switched off
    let layer = tex_layer(i.tag);
    if (layer > 0u && u.fx.z * u.fx.w > 0.5) {
        let k = layer - 1u;
        let par = tex_par[k];
        let rot = mat2x2<f32>(par.y, par.z, -par.z, par.y);
        // anchored element: project in its own frame so the pattern follows it
        var pw = i.world;
        var nw = n;
        var qx = dp_x;
        var qy = dp_y;
        let fr = anchor_frame(i.elem);
        if (fr.on) {
            pw = to_frame(i.world - fr.o, fr);
            nw = to_frame(n, fr);
            qx = to_frame(dp_x, fr);
            qy = to_frame(dp_y, fr);
        }
        let uv = rot * surface_uv(pw, nw) * par.x;
        let gx = rot * surface_uv(qx, nw) * par.x;
        let gy = rot * surface_uv(qy, nw) * par.x;
        base = textureSampleGrad(tex_arr, tex_smp, uv, k, gx, gy).rgb;
        let sf = tex_surface(par.w);
        rough = sf.x;
        metal = sf.y;
        // relief from the image: brightness slope one texel across, as a normal tilt along
        // the projection axes; fades with distance like the other detail
        let e = 1.0 / 256.0;
        let h0 = luma(base);
        let hu = luma(textureSampleGrad(tex_arr, tex_smp, uv + vec2<f32>(e, 0.0), k, gx, gy).rgb);
        let hv = luma(textureSampleGrad(tex_arr, tex_smp, uv + vec2<f32>(0.0, e), k, gx, gy).rgb);
        let g = transpose(rot) * vec2<f32>(hu - h0, hv - h0);
        var tilt = g.x * surface_axis(nw, 0u) + g.y * surface_axis(nw, 1u);
        if (fr.on) { tilt = from_frame(tilt, fr); }
        n = normalize(n - tilt * 3.0 * detail);
    }

    let l = normalize(-u.light_dir.xyz);
    let sun = max(dot(n, l), 0.0) * sun_light(i.world, n);
    var ao = ambient_occ_at(i.clip.xy, i.world);
    if (u.params.z < 0.5) { ao = mix(0.8, 1.0, smoothstep(0.0, 0.9, i.world.z - u.view.w)); }
    let aoc = ao_bounce(ao, clamp(base, vec3<f32>(0.0), vec3<f32>(1.0)));
    let hemi = sky_ambient(n);
    let sun_c = vec3<f32>(1.0, 0.95, 0.86) * 1.15;
    // a metal has little diffuse light; its highlight and reflection take its own colour
    var c = base * (hemi * 0.62 * aoc + sun_c * sun) * (1.0 - 0.6 * metal);

    // highlight and Fresnel sky reflection, both by roughness
    let h = normalize(l + v);
    let power = mix(160.0, 12.0, rough);
    let spec_c = mix(vec3<f32>(1.0), base * 1.6, metal);
    c += sun_c * spec_c * pow(max(dot(n, h), 0.0), power) * sun * (1.0 - rough) * 0.6;
    let fres = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 5.0);
    let r = reflect(-v, n);
    let refl = sky_dir(r) * spec_c;
    let f0 = mix(0.04, 0.6, metal);
    c = mix(c, refl * ao, (f0 + (1.0 - f0) * fres) * (1.0 - rough) * 0.6);
    // filmic curve keeps highlights from clipping, then distance haze
    c = haze(tone_filmic(c), dist);
    return vec4<f32>(mark(c, n, to_eye, selected, hovered, i.elem), 1.0);
}
