// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
// Fullscreen passes: depth-based ambient-light shading and frame output to the window.

// depth read as a plain float texture: GL has no texel load from depth textures
@group(1) @binding(0) var depth_tex: texture_2d<f32>;
@group(1) @binding(1) var frame_tex: texture_2d<f32>;
@group(1) @binding(2) var frame_smp: sampler;

struct FOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> FOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: FOut;
    o.clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}

fn depth_px(p: vec2<i32>) -> f32 {
    let s = vec2<i32>(textureDimensions(depth_tex)) - vec2<i32>(1);
    return textureLoad(depth_tex, clamp(p, vec2<i32>(0), s), 0).x;
}

// Hemisphere sampling in world coordinates: normal from neighbouring depth points (the side
// with the smaller jump), kernel rotation by per-pixel noise; the main pass does the blur.
@fragment
fn fs_ssao(i: FOut) -> @location(0) vec4<f32> {
    let k = u.view.z;
    let pc = i.clip.xy * k;
    let ip = vec2<i32>(pc);
    let d = depth_px(ip);
    if (d <= 0.0) { return vec4<f32>(1.0); }
    let p = world_at(pc, d);
    let s = i32(k);
    let pr = world_at(pc + vec2<f32>(k, 0.0), depth_px(ip + vec2<i32>(s, 0)));
    let pl = world_at(pc - vec2<f32>(k, 0.0), depth_px(ip - vec2<i32>(s, 0)));
    let pd = world_at(pc + vec2<f32>(0.0, k), depth_px(ip + vec2<i32>(0, s)));
    let pu = world_at(pc - vec2<f32>(0.0, k), depth_px(ip - vec2<i32>(0, s)));
    var dx = pr - p;
    if (length(p - pl) < length(dx)) { dx = p - pl; }
    var dy = pd - p;
    if (length(p - pu) < length(dy)) { dy = p - pu; }
    var n = normalize(cross(dx, dy));
    let to_eye = u.eye.xyz - p;
    if (dot(n, to_eye) < 0.0) { n = -n; }
    let dist = eye_len(p);
    let radius = clamp(dist * 0.035, 0.25, 0.9);
    let t = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), abs(n.x) > 0.8);
    let b1 = normalize(cross(n, t));
    let b2 = cross(n, b1);
    // interleaved gradient noise (published, Jimenez 2014); golden-angle spiral of samples below
    let noise = fract(52.9829189 * fract(dot(i.clip.xy, vec2<f32>(0.06711056, 0.00583715))));
    let count = select(6, 10, u.params.z > 1.5);
    var occ = 0.0;
    for (var j = 0; j < count; j = j + 1) {
        let fj = f32(j);
        let a = (fj + noise) * 2.39996323;
        let r = sqrt((fj + 0.5) / f32(count));
        let h = sqrt(1.0 - r * r);
        let len = radius * mix(0.2, 1.0, fract(fj * 0.618034 + noise));
        let dir = b1 * (cos(a) * r) + b2 * (sin(a) * r) + n * h;
        let sp = p + dir * len;
        let c = u.view_proj * vec4<f32>(sp, 1.0);
        let ndc = c.xy / c.w;
        let spx = vec2<f32>((ndc.x * 0.5 + 0.5) * u.view.x, (0.5 - ndc.y * 0.5) * u.view.y);
        let sd = depth_px(vec2<i32>(spx));
        if (sd <= 0.0) { continue; }
        let q = world_at(spx, sd);
        let dq = eye_len(q);
        let ds = eye_len(sp);
        if (dq < ds - 0.02) {
            occ = occ + smoothstep(0.0, 1.0, radius / max(abs(dist - dq), 1e-3));
        }
    }
    let ao = clamp(1.0 - occ / f32(count) * 1.15, 0.0, 1.0);
    return vec4<f32>(ao, ao, ao, 1.0);
}

fn eye_dist(p: vec2<i32>) -> f32 {
    let d = depth_px(p);
    if (d <= 0.0) { return 1e9; }
    return eye_len(world_at(vec2<f32>(p) + 0.5, d));
}

fn eye_pos(p: vec2<i32>) -> vec3<f32> {
    return world_at(vec2<f32>(p) + 0.5, depth_px(p));
}

// Frame output; outline from two cues, once per pixel and with no face redraw: a depth jump
// (a neighbour clearly farther: silhouette) and, when `look.y` > 0, a fold (a neighbour that
// leaves the tangent plane of the pixel). The width follows the frame height (`look.x`).
@fragment
fn fs_composite(i: FOut) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(frame_tex, frame_smp, i.uv, 0.0);
    let ip = vec2<i32>(i.uv * u.view.xy);
    let d = depth_px(ip);
    if (c.a < 0.99 || d <= 0.0 || u.fx.y < 0.5) { return c; }
    let w = max(i32(round(u.look.x)), 1);
    let dist = eye_len(world_at(vec2<f32>(ip) + 0.5, d));
    let far = max(max(eye_dist(ip + vec2<i32>(w, 0)), eye_dist(ip - vec2<i32>(w, 0))),
                  max(eye_dist(ip + vec2<i32>(0, w)), eye_dist(ip - vec2<i32>(0, w))));
    // diagonal neighbours round off the stair steps of the depth edge
    let dg = max(max(eye_dist(ip + vec2<i32>(w, w)), eye_dist(ip - vec2<i32>(w, w))),
                 max(eye_dist(ip + vec2<i32>(w, -w)), eye_dist(ip - vec2<i32>(w, -w))));
    let jump = max((far - dist) / dist, 0.7 * (dg - dist) / dist);
    var e = smoothstep(0.04, 0.12, jump) * 0.6;
    if (u.look.y > 0.0 && dist < 1e8) {
        // fold: the neighbours' offset from the tangent plane, as the sine of the angle to it
        let p = world_at(vec2<f32>(ip) + 0.5, d);
        let pr = eye_pos(ip + vec2<i32>(w, 0));
        let pl = eye_pos(ip - vec2<i32>(w, 0));
        let pd = eye_pos(ip + vec2<i32>(0, w));
        let pu = eye_pos(ip - vec2<i32>(0, w));
        var dx = pr - p;
        if (length(p - pl) < length(dx)) { dx = p - pl; }
        var dy = pd - p;
        if (length(p - pu) < length(dy)) { dy = p - pu; }
        let n = normalize(cross(dx, dy));
        // only neighbours on this surface count: a depth jump is the other cue's job
        let near = vec4<f32>(length(pr - p), length(pl - p), length(pd - p), length(pu - p));
        let lim = dist * 0.02 * f32(w);
        var fold = 0.0;
        if (near.x < lim) { fold = max(fold, abs(dot(n, pr - p)) / max(near.x, 1e-5)); }
        if (near.y < lim) { fold = max(fold, abs(dot(n, pl - p)) / max(near.y, 1e-5)); }
        if (near.z < lim) { fold = max(fold, abs(dot(n, pd - p)) / max(near.z, 1e-5)); }
        if (near.w < lim) { fold = max(fold, abs(dot(n, pu - p)) / max(near.w, 1e-5)); }
        e = max(e, smoothstep(0.30, 0.65, fold) * 0.5 * u.look.y);
    }
    let ink = mix(vec3<f32>(0.16, 0.17, 0.20), vec3<f32>(0.03, 0.035, 0.045), u.params.x);
    return vec4<f32>(mix(c.rgb, ink, e), c.a);
}
