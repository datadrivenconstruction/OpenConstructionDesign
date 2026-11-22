// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
// Scene: sun shadows from the depth map, ambient-occlusion shading from the shading frame,
// outline from the depth jump, crease edges as lines, glass with ordered blending.

@group(1) @binding(0) var depth_tex: texture_depth_2d;
@group(1) @binding(1) var ao_tex: texture_2d<f32>;
@group(1) @binding(2) var lin_smp: sampler;
@group(1) @binding(3) var shadow_tex: texture_depth_2d;
@group(1) @binding(4) var shadow_smp: sampler_comparison;
// leaf card pictures (`leaves.rs`): x coverage, y tone; broad-leaf layers 0..2, needles 3
@group(0) @binding(3) var leaf_tex: texture_2d_array<f32>;
@group(0) @binding(4) var leaf_smp: sampler;

// packed vertex: 21 bits per axis (high 16 in x/y/z, low 5 in w); chunk origin and step are
// in the placement row; normal is an octahedron, for a triangle taken from the provoking vertex (flat)
struct VIn {
    @location(0) q: vec4<u32>,
    @location(1) oct: vec2<f32>,
    @location(2) elem: u32,
    @location(3) tag: u32,
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) nrm: vec3<f32>,
    @location(2) @interpolate(flat) elem: u32,
    @location(3) @interpolate(flat) tag: u32,
    // per-placement hash of the element and the placement origin: tone variation of foliage
    @location(4) @interpolate(flat) seed: u32,
    // interpolated vertex normal: smooth only where the vertices carry smooth normals (foliage)
    @location(5) snrm: vec3<f32>,
    // leaf card: the picture's coordinates (the corner bits of the vertex tag); -1 elsewhere
    @location(6) uv: vec2<f32>,
};

// 32-bit avalanche (xorshift-multiply, public-domain constants): integer maths, equal on every GPU
fn mix32(x: u32) -> u32 {
    var h = x ^ (x >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    h = h * 0x846ca68bu;
    return h ^ (h >> 16u);
}

fn vpos(q: vec4<u32>) -> vec3<f32> {
    let lo = vec3<u32>(q.w & 31u, (q.w >> 5u) & 31u, (q.w >> 10u) & 31u);
    return vec3<f32>((q.xyz << vec3<u32>(5u)) | lo);
}

// octahedral normal decoding (published, Cigolle et al. 2014)
fn unoct(e: vec2<f32>) -> vec3<f32> {
    var n = vec3<f32>(e, 1.0 - abs(e.x) - abs(e.y));
    let t = max(-n.z, 0.0);
    n.x += select(t, -t, n.x >= 0.0);
    n.y += select(t, -t, n.y >= 0.0);
    return n;
}

// instance: in w of the first columns - element id (low 16 bits + 1, high bits)
fn xform(v: VIn) -> VOut {
    let m = mat4x4<f32>(vec4<f32>(v.m0.xyz, 0.0), vec4<f32>(v.m1.xyz, 0.0), vec4<f32>(v.m2.xyz, 0.0), v.m3);
    let lp = vpos(v.q);
    var w = m * vec4<f32>(lp, 1.0);
    var e = v.elem;
    if (v.m0.w > 0.5) {
        e = (u32(v.m0.w + 0.5) - 1u) | (u32(v.m1.w + 0.5) << 16u);
    }
    // show-as-built: the element's offset moves the vertex (shadows follow)
    w = vec4<f32>(w.xyz + anim_off(e), w.w);
    // wind: crowns sway about their base, more the higher the vertex, each tree in its own phase
    if (u.wind.x > 0.0 && (v.tag & 0xF0000000u) == 0x50000000u) {
        let at0 = bitcast<vec3<u32>>(v.m3.xyz);
        let ph = f32(mix32(at0.x ^ mix32(at0.y)) & 1023u) * 0.00614;
        let hh = clamp(lp.z * 0.1, 0.0, 2.0);
        let k = u.wind.x * hh * hh;
        w = vec4<f32>(w.x + k * sin(u.wind.y * 1.3 + ph), w.y + k * 0.6 * sin(u.wind.y * 1.7 + ph * 1.9), w.z, w.w);
    }
    var o: VOut;
    o.clip = u.view_proj * w;
    o.world = w.xyz;
    // normal via the inverse transpose (adjugate signed by the determinant):
    // correct under non-uniform scale and mirroring; the fragment normalises the length
    let a = v.m0.xyz;
    let b = v.m1.xyz;
    let c = v.m2.xyz;
    let det = dot(a, cross(b, c));
    let cof = mat3x3<f32>(cross(b, c), cross(c, a), cross(a, b));
    // the row carries the packing step (~1e-5): the adjugate is small, the length is restored here
    o.nrm = normalize(cof * unoct(v.oct)) * select(1.0, -1.0, det < 0.0);
    o.snrm = o.nrm;
    o.elem = e;
    o.tag = v.tag;
    o.uv = vec2<f32>(-1.0);
    if (is_card(v.tag)) { o.uv = vec2<f32>(f32((v.tag >> 1u) & 1u), f32((v.tag >> 2u) & 1u)); }
    let at = bitcast<vec3<u32>>(v.m3.xyz);
    o.seed = mix32(e * 0x9e3779b1u ^ mix32(at.x ^ mix32(at.y ^ mix32(at.z))));
    // hidden category: vertex outside the clip volume - the whole triangle is discarded
    if (vertex_hidden(v.tag)) { o.clip = vec4<f32>(0.0, 0.0, -1.0, 1.0); }
    return o;
}

@vertex
fn vs(v: VIn) -> VOut { return xform(v); }

@vertex
fn vs_light(v: VIn) -> VOut {
    var o = xform(v);
    if (!vertex_hidden(v.tag)) { o.clip = u.light_vp * vec4<f32>(o.world, 1.0); }
    return o;
}

// depth without colour (shadow map and pre-pass): clip box only
// lattice noise in 0..1: the leaf clusters of a crown, in world space (a crown keeps its holes
// when the camera moves)
fn lat3(p: vec3<f32>) -> f32 {
    let c = vec3<i32>(floor(p));
    return f32(mix32(u32(c.x) * 0x9e3779b1u ^ u32(c.y) * 0x85ebca6bu ^ u32(c.z) * 0xc2b2ae35u)) / 4294967295.0;
}
fn vnoise(p: vec3<f32>) -> f32 {
    let f = fract(p);
    let s = f * f * (3.0 - 2.0 * f);
    let b = vec3<f32>(floor(p));
    let x00 = mix(lat3(b), lat3(b + vec3<f32>(1.0, 0.0, 0.0)), s.x);
    let x10 = mix(lat3(b + vec3<f32>(0.0, 1.0, 0.0)), lat3(b + vec3<f32>(1.0, 1.0, 0.0)), s.x);
    let x01 = mix(lat3(b + vec3<f32>(0.0, 0.0, 1.0)), lat3(b + vec3<f32>(1.0, 0.0, 1.0)), s.x);
    let x11 = mix(lat3(b + vec3<f32>(0.0, 1.0, 1.0)), lat3(b + vec3<f32>(1.0, 1.0, 1.0)), s.x);
    return mix(mix(x00, x10, s.y), mix(x01, x11, s.y), s.z);
}

// Foliage cut-out: the smooth crown solid is eaten into leaf clusters, more where the surface
// turns from the eye (the edge of the crown), so the silhouette breaks up and the branches and
// sky show through. Fades out when a cluster is smaller than a few pixels.
fn leaf_cut(world: vec3<f32>, sn: vec3<f32>, px_m: f32) -> bool {
    let fade = 1.0 - smoothstep(0.06, 0.16, px_m);
    if (fade <= 0.0) { return false; }
    let e = normalize(u.eye.xyz - world);
    let edge = 1.0 - abs(dot(normalize(sn), e));
    let n = vnoise(world * 3.1 + vec3<f32>(7.3, 1.9, 4.1));
    let gaps = smoothstep(0.30, 0.0, vnoise(world * 1.3 + vec3<f32>(3.1, 8.7, 2.2)));
    let t = (0.30 + 0.80 * smoothstep(0.25, 0.95, edge) + 0.30 * gaps) * fade;
    return n < t;
}

fn is_leaf(tag: u32) -> bool { return (tag & 0xF0000000u) == 0x50000000u; }
fn is_card(tag: u32) -> bool { return is_leaf(tag) && (tag & 1u) != 0u; }

// The picture of a leaf card at `uv` (x coverage, y tone): a needle spray or one of the
// broad-leaf clusters by the card's jitter, mirrored on every other card.
fn card_texel(uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>, tag: u32) -> vec2<f32> {
    let j = (tag >> 8u) & 7u;
    var layer = i32(j % 3u);
    if ((tag & 0x10000u) != 0u) { layer = 3; }
    var q = uv;
    if ((j & 4u) != 0u) { q.x = 1.0 - q.x; }
    return textureSampleGrad(leaf_tex, leaf_smp, q, layer, gx, gy).xy;
}

// Brightness of a card's texel: the leaf's own tone, darker the deeper the card sits in the
// crown (depth tone in the tag), a little up or down per card.
fn card_tone(tag: u32, tone: f32) -> f32 {
    let depth = f32((tag >> 17u) & 3u) / 3.0;
    let jit = f32((tag >> 8u) & 7u) / 7.0;
    return (0.35 + 0.75 * tone) * (0.6 + 0.4 * depth) * (0.9 + 0.2 * jit);
}

@fragment
fn fs_clip(i: VOut) {
    let px = length(dpdx(i.world)) + length(dpdy(i.world));
    let gx = dpdx(i.uv);
    let gy = dpdy(i.uv);
    if (clipped(i.world) || face_dropped(i.tag, i.clip.xy, i.elem)) { discard; }
    if (is_card(i.tag)) {
        if (card_texel(i.uv, gx, gy, i.tag).x < 0.5) { discard; }
    } else if (is_leaf(i.tag) && leaf_cut(i.world, i.snrm, px)) { discard; }
}

// Ground: a large quad that receives sun shadow; colour is a darkening over the background.
@vertex
fn vs_ground(@builtin(vertex_index) k: u32) -> VOut {
    var c = array<vec2<f32>, 6>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
                                vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0));
    let r = max(u.eye.w * 6.0, 60.0);
    let w = vec3<f32>(orbit_eye().xy + c[k] * r, u.view.w);
    var o: VOut;
    o.clip = u.view_proj * vec4<f32>(w, 1.0);
    o.world = w;
    o.nrm = vec3<f32>(0.0, 0.0, 1.0);
    o.elem = 0xFFFFFFFEu;
    o.tag = 0u;
    o.seed = 0u;
    o.snrm = o.nrm;
    o.uv = vec2<f32>(-1.0);
    return o;
}

fn sun_light(p: vec3<f32>, n: vec3<f32>) -> f32 {
    if (u.params.w < 0.5) { return 1.0; }
    let c = u.light_vp * vec4<f32>(p + n * 0.03, 1.0);
    let uv = vec2<f32>(c.x * 0.5 + 0.5, 0.5 - c.y * 0.5);
    // reverse depth: beyond the far plane z < 0, towards the light - larger
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || c.z < 0.0) { return 1.0; }
    let texel = 1.0 / vec2<f32>(textureDimensions(shadow_tex));
    let z = c.z + 0.0015;
    // comparison taps with bilinear filtering: 4 at a 0.9 texel step; at high quality
    // 4 more at a 1.8 step in a cross - a wider, softer edge
    let o = texel * 0.9;
    var s = textureSampleCompareLevel(shadow_tex, shadow_smp, uv + vec2<f32>(-o.x, -o.y), z)
          + textureSampleCompareLevel(shadow_tex, shadow_smp, uv + vec2<f32>(o.x, -o.y), z)
          + textureSampleCompareLevel(shadow_tex, shadow_smp, uv + vec2<f32>(-o.x, o.y), z)
          + textureSampleCompareLevel(shadow_tex, shadow_smp, uv + vec2<f32>(o.x, o.y), z);
    if (u.params.z < 1.5) { return s * 0.25; }
    let w = texel * 1.8;
    s += textureSampleCompareLevel(shadow_tex, shadow_smp, uv + vec2<f32>(-w.x, 0.0), z)
       + textureSampleCompareLevel(shadow_tex, shadow_smp, uv + vec2<f32>(w.x, 0.0), z)
       + textureSampleCompareLevel(shadow_tex, shadow_smp, uv + vec2<f32>(0.0, -w.y), z)
       + textureSampleCompareLevel(shadow_tex, shadow_smp, uv + vec2<f32>(0.0, w.y), z);
    return s * 0.125;
}

fn ambient_occ(px: vec2<f32>) -> f32 {
    if (u.params.z < 0.5) { return 1.0; }
    let t = 1.0 / vec2<f32>(textureDimensions(ao_tex));
    let uv = px / u.view.xy;
    let a = textureSampleLevel(ao_tex, lin_smp, uv + vec2<f32>(-t.x, -t.y), 0.0).r
          + textureSampleLevel(ao_tex, lin_smp, uv + vec2<f32>(t.x, -t.y), 0.0).r
          + textureSampleLevel(ao_tex, lin_smp, uv + vec2<f32>(-t.x, t.y), 0.0).r
          + textureSampleLevel(ao_tex, lin_smp, uv + vec2<f32>(t.x, t.y), 0.0).r;
    return a * 0.25;
}

@fragment
fn fs_ground(i: VOut) -> @location(0) vec4<f32> {
    let s = sun_light(i.world, vec3<f32>(0.0, 0.0, 1.0));
    let ao = ambient_occ(i.clip.xy);
    let fade = 1.0 - smoothstep(u.eye.w * 1.2, u.eye.w * 3.0, distance(i.world.xy, orbit_eye().xy));
    let k = clamp((1.0 - s) * mix(0.22, 0.38, u.params.x) + (1.0 - ao) * 0.45, 0.0, 0.6);
    // shadow over a light shading of the ground: "over" of two layers in one output
    let tint = mix(vec3<f32>(0.60, 0.60, 0.58), vec3<f32>(0.20, 0.22, 0.25), u.params.x);
    let g = 0.10;
    let a = 1.0 - (1.0 - k) * (1.0 - g);
    return vec4<f32>(tint * g * (1.0 - k) / a, a * fade);
}

// face tag without the category bits and, for plain tags, without the texture layer (bits 16-23)
fn base_tag(t: u32) -> u32 {
    if ((t & 0xF0000000u) == 0u) { return t & 0xF000FFFFu; }
    // a leaf card keeps its data in the low colour bits: the colour is their middle
    if (is_card(t)) { return (t & 0xF0F8F8F8u) | 0x040404u; }
    return t & 0xF0FFFFFFu;
}

// texture layer + 1 of a plain face tag, 0 when none (flag tags carry colour in those bits)
fn tex_layer(t: u32) -> u32 {
    if ((t & 0xF0000000u) != 0u) { return 0u; }
    return (t >> 16u) & 255u;
}

fn palette(t: u32) -> vec3<f32> {
    // colour in the tag (sRGB 0xRRGGBB) - network systems
    if ((t & 0xC0000000u) == 0x40000000u) {
        let c = vec3<f32>(f32((t >> 16u) & 255u), f32((t >> 8u) & 255u), f32(t & 255u)) / 255.0;
        return pow(c, vec3<f32>(2.2));
    }
    switch (t) {
        case 5u: { return vec3<f32>(0.55, 0.40, 0.27); }   // door
        case 6u: { return vec3<f32>(0.90, 0.91, 0.93); }   // window
        case 8u: { return vec3<f32>(0.30, 0.52, 0.62); }   // glass
        case 9u: { return vec3<f32>(0.30, 0.33, 0.37); }   // mullion
        case 10u: { return vec3<f32>(0.66, 0.68, 0.71); }  // blind panel
        case 11u: { return vec3<f32>(0.74, 0.73, 0.70); }  // floor slab
        case 20u: { return vec3<f32>(0.62, 0.52, 0.40); }  // terrain soil
        case 21u: { return vec3<f32>(0.45, 0.62, 0.36); }  // lawn
        case 22u: { return vec3<f32>(0.36, 0.37, 0.39); }  // asphalt
        case 23u: { return vec3<f32>(0.68, 0.65, 0.58); }  // gravel
        case 24u: { return vec3<f32>(0.74, 0.66, 0.58); }  // paving
        case 25u: { return vec3<f32>(0.38, 0.58, 0.76); }  // water
        default: {}
    }
    switch (t % 4u) {
        case 0u: { return vec3<f32>(0.86, 0.85, 0.82); }
        case 1u: { return vec3<f32>(0.70, 0.66, 0.60); }
        case 2u: { return vec3<f32>(0.80, 0.82, 0.86); }
        default: { return vec3<f32>(0.58, 0.62, 0.68); }
    }
}

fn mark(c0: vec3<f32>, n: vec3<f32>, to_eye: vec3<f32>, selected: bool, hovered: bool, e: u32) -> vec3<f32> {
    // show-as-built recolour: fades from the track colour to the element's own
    let tn = anim_tint(e);
    let c = mix(c0, tn.xyz, tn.w);
    if (selected) {
        // selection: accent fill and silhouette halo (stronger at grazing angles)
        let rim = pow(1.0 - abs(dot(n, normalize(to_eye))), 2.0);
        return mix(c, u.accent.xyz, 0.45) + u.accent.xyz * rim * 0.55;
    }
    if (hovered) {
        // hover: own colour and a light silhouette outline, no selection fill
        let rim = pow(1.0 - abs(dot(n, normalize(to_eye))), 2.0);
        return mix(c, u.hover.xyz, 0.3) + u.hover.xyz * rim * 0.35;
    }
    return c;
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let gx = dpdx(i.uv);
    let gy = dpdy(i.uv);
    let selected = is_selected(i.elem);
    let hovered = is_hovered(i.elem);
    let dark = u.params.x;
    if (i.tag >= 0x80000000u) {
        if ((i.tag & 0xF0FFFFFFu) == 0x80000000u) {
            if (clipped(i.world)) { discard; }
            let ea = anim_alpha(i.elem);
            if (ea < 1.0 && bayer4(i.clip.xy) > ea) { discard; }
            if (u.fx.x < 0.5 && !selected && !hovered) { discard; }
            // crease edges: selected one in accent, others thin, lighter in the dark theme
            if (selected) { return vec4<f32>(u.accent.xyz, 1.0); }
            if (hovered) { return vec4<f32>(u.hover.xyz, 1.0); }
            var c = mix(vec3<f32>(0.10, 0.11, 0.13), vec3<f32>(0.04, 0.045, 0.055), dark);
            // wireframe and x-ray in the dark theme: light edges over a dark background
            if (dark > 0.5 && u.style.x > 2.5) { c = vec3<f32>(0.78, 0.81, 0.86); }
            return vec4<f32>(c, mix(0.55, 0.7, dark));
        }
        // ground grid: thin, fades with distance from the look-at point (relative to camera distance)
        let d = distance(i.world.xy, orbit_eye().xy);
        let fade = 1.0 - smoothstep(u.eye.w * 0.35, u.eye.w * 1.35, d);
        switch (i.tag) {
            case 0x80000001u: {
                let c = mix(vec3<f32>(0.42, 0.45, 0.50), vec3<f32>(0.55, 0.60, 0.68), dark);
                return vec4<f32>(c, mix(0.14, 0.07, dark) * fade);
            }
            case 0x80000002u: {
                let c = mix(vec3<f32>(0.38, 0.41, 0.46), vec3<f32>(0.60, 0.65, 0.74), dark);
                return vec4<f32>(c, mix(0.28, 0.15, dark) * fade);
            }
            case 0x80000003u: { return vec4<f32>(0.78, 0.28, 0.25, mix(0.75, 0.55, dark) * fade); }
            default: { return vec4<f32>(0.28, 0.62, 0.32, mix(0.75, 0.55, dark) * fade); }
        }
    }
    if (clipped(i.world) || face_dropped(i.tag, i.clip.xy, i.elem)) { discard; }
    var tone = 1.0;
    if (is_card(i.tag)) {
        let tx = card_texel(i.uv, gx, gy, i.tag);
        if (tx.x < 0.5) { discard; }
        tone = card_tone(i.tag, tx.y);
    }
    var n = normalize(i.nrm);
    if (is_card(i.tag)) { n = normalize(i.snrm); }
    let to_eye = u.eye.xyz - i.world;
    if (dot(n, to_eye) < 0.0 && !is_card(i.tag)) { n = -n; }
    let l = normalize(-u.light_dir.xyz);
    if ((i.tag & 0xF0000000u) == 0x10000000u) {
        // section fill (poche): material colour from the tag, unlit
        let pc = vec3<f32>(f32((i.tag >> 16u) & 255u), f32((i.tag >> 8u) & 255u), f32(i.tag & 255u)) / 255.0;
        return vec4<f32>(mark(pow(pc, vec3<f32>(2.2)), n, to_eye, selected, hovered, i.elem), 1.0);
    }
    if (u.style.x > 1.5 && u.style.x < 2.5) {
        // hidden lines: paper-coloured faces cover invisible edges
        return vec4<f32>(mark(vec3<f32>(0.96, 0.96, 0.95), n, to_eye, selected, hovered, i.elem), 1.0);
    }
    if (u.style.x > 0.5 && u.style.x < 1.5) {
        // consistent colours: category colour, slight per-face tint by slope, no shadows
        let k = 0.8 + 0.2 * abs(dot(n, normalize(vec3<f32>(0.35, 0.55, 0.76))));
        return vec4<f32>(mark(palette(base_tag(i.tag)) * k, n, to_eye, selected, hovered, i.elem), 1.0);
    }

    // hemisphere (sky/ground) + sun with soft transition and shadow + counter fill light
    let sky = vec3<f32>(0.93, 0.96, 1.0);
    let ground = vec3<f32>(0.55, 0.52, 0.49);
    let hemi = mix(ground, sky, n.z * 0.5 + 0.5);
    let wrap = clamp((dot(n, l) + 0.2) / 1.2, 0.0, 1.0);
    let sun = wrap * sun_light(i.world, n);
    let fill = max(dot(n, normalize(vec3<f32>(0.6, 0.8, 0.3))), 0.0) * 0.10;
    let amb = u.light_dir.w;
    let base = palette(base_tag(i.tag)) * tone;
    var ao = ambient_occ(i.clip.xy);
    if (u.params.z < 0.5) {
        // no effects: ground-contact shading approximation
        ao = mix(0.8, 1.0, smoothstep(0.0, 0.9, i.world.z - u.view.w));
    }
    var c = base * ((hemi * amb + fill) * ao + vec3<f32>(1.0, 0.97, 0.92) * (1.0 - amb) * sun * mix(0.85, 1.0, ao));
    return vec4<f32>(mark(c, n, to_eye, selected, hovered, i.elem), 1.0);
}

// Glass: tinted, Fresnel sky reflection and sun glint; order - far to near.
@fragment
fn fs_glass(i: VOut) -> @location(0) vec4<f32> {
    if (clipped(i.world) || (u.style.x > 2.5 && u.style.x < 3.5)) { discard; }
    let selected = is_selected(i.elem);
    let hovered = is_hovered(i.elem);
    var n = normalize(i.nrm);
    let to_eye = u.eye.xyz - i.world;
    if (dot(n, to_eye) < 0.0) { n = -n; }
    let v = normalize(to_eye);
    let fres = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 4.0);
    let l = normalize(-u.light_dir.xyz);
    let h = normalize(l + v);
    let spec = pow(max(dot(n, h), 0.0), 90.0) * sun_light(i.world, n);
    let r = reflect(-v, n);
    let skyc = mix(vec3<f32>(0.62, 0.66, 0.70), vec3<f32>(0.86, 0.92, 1.0), clamp(r.z * 0.5 + 0.6, 0.0, 1.0));
    // network shell (insulation): system colour, weaker reflection
    let shell = (i.tag & 0xE0000000u) == 0x60000000u;
    var tint = palette(8u) * (0.55 + 0.45 * max(dot(n, l), 0.0));
    if (shell) { tint = palette(i.tag & 0x5FFFFFFFu) * (0.7 + 0.3 * max(dot(n, l), 0.0)); }
    var c = mix(tint, skyc, select(0.25 + fres * 0.6, 0.1 + fres * 0.3, shell)) + vec3<f32>(spec * 0.9);
    c = mark(c, n, to_eye, selected, hovered, i.elem);
    var a = clamp(0.30 + fres * 0.45 + spec, 0.0, 0.9);
    if (shell) { a = clamp(0.22 + fres * 0.35, 0.0, 0.7); }
    return vec4<f32>(c, a * cat_cov(i.tag) * anim_alpha(i.elem));
}
