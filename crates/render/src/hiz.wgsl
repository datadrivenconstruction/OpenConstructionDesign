// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
// Depth tiles for occlusion culling: per 8x8 tile, the farthest value
// (reverse depth: farther is smaller); holes in occluders stay holes.

@group(0) @binding(0) var depth_tex: texture_depth_2d;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = select(-1.0, 3.0, i == 1u);
    let y = select(-1.0, 3.0, i == 2u);
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let s = vec2<i32>(textureDimensions(depth_tex));
    let o = vec2<i32>(p.xy) * 8;
    var d = 1.0;
    for (var y = 0; y < 8; y += 1) {
        for (var x = 0; x < 8; x += 1) {
            let q = o + vec2<i32>(x, y);
            if (q.x < s.x && q.y < s.y) {
                d = min(d, textureLoad(depth_tex, q, 0));
            }
        }
    }
    return vec4<f32>(d, 0.0, 0.0, 1.0);
}
