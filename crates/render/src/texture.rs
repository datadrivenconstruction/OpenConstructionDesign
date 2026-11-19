// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Image textures of the realistic style: projection math, own procedural images, mip chains and
//! the hand-off from the model side to the renderer. No GPU code here (`texture_gpu`).
//!
//! Coordinates are world metres. A face projects along the world axis best facing its normal, so
//! one material has the same scale on every element and neighbouring elements line up.

use std::sync::{Arc, Mutex};

/// Side of every layer in the texture array.
pub const LAYER_PX: u32 = 256;
/// Layers the shader table holds (`tex_par` in realistic.wgsl).
pub const MAX_LAYERS: usize = 64;
/// Smallest and largest tile size accepted, metres.
pub const SCALE_MIN: f32 = 0.01;
pub const SCALE_MAX: f32 = 100.0;

/// Own procedural textures: key, metres per repeat, roughness.
pub const BUILTIN: [(&str, f32, f32); 7] = [
    ("brick", 0.6, 0.85),
    ("concrete", 1.0, 0.9),
    ("wood", 0.8, 0.6),
    ("tile", 0.8, 0.25),
    ("metal", 0.5, 0.3),
    ("glass", 1.0, 0.08),
    ("grass", 1.0, 1.0),
];

/// Default tile size (metres) and roughness of a built-in texture.
pub fn builtin_info(key: &str) -> Option<(f32, f32)> {
    BUILTIN.iter().find(|b| b.0 == key).map(|b| (b.1, b.2))
}

// ---- projection ----

/// Two world axes best facing the normal; the same choice as `surface_uv` in realistic.wgsl.
pub fn surface_uv(p: [f32; 3], n: [f32; 3]) -> [f32; 2] {
    let a = [n[0].abs(), n[1].abs(), n[2].abs()];
    if a[2] >= a[0] && a[2] >= a[1] {
        [p[0], p[1]]
    } else if a[0] >= a[1] {
        [p[1], p[2]]
    } else {
        [p[0], p[2]]
    }
}

/// Rotation (degrees, counter-clockwise) then division by the tile size in metres.
pub fn layer_uv(uv: [f32; 2], scale_m: f32, rotation_deg: f32) -> [f32; 2] {
    let (s, c) = rotation_deg.to_radians().sin_cos();
    let k = 1.0 / clamp_scale(scale_m);
    [(uv[0] * c - uv[1] * s) * k, (uv[0] * s + uv[1] * c) * k]
}

/// Texture coordinates of a world point on a face with normal `n`.
pub fn box_uv(p: [f32; 3], n: [f32; 3], scale_m: f32, rotation_deg: f32) -> [f32; 2] {
    layer_uv(surface_uv(p, n), scale_m, rotation_deg)
}

pub fn clamp_scale(s: f32) -> f32 {
    if s.is_finite() {
        s.clamp(SCALE_MIN, SCALE_MAX)
    } else {
        1.0
    }
}

// ---- face tags ----

/// Bit position of "texture layer + 1" in the tag of a plain face.
pub const TAG_LAYER_SHIFT: u32 = 16;

/// A plain palette tag may carry a layer; flag tags (lines, colour, section fill) and glass do not.
pub fn takes_layer(tag: u32) -> bool {
    tag & 0xF0FF_0000 == 0 && tag != crate::scene::TAG_GLASS
}

/// The tag with texture layer `layer` (0-based); unchanged where a layer cannot be carried.
pub fn with_layer(tag: u32, layer: u8) -> u32 {
    if takes_layer(tag) && (layer as usize) < MAX_LAYERS {
        tag | ((layer as u32 + 1) << TAG_LAYER_SHIFT)
    } else {
        tag
    }
}

/// Layer carried by a tag.
pub fn layer_of(tag: u32) -> Option<u8> {
    if tag & 0xF000_0000 != 0 {
        return None;
    }
    match (tag >> TAG_LAYER_SHIFT) & 255 {
        0 => None,
        n => Some((n - 1) as u8),
    }
}

// ---- layers and sets ----

#[derive(Clone, Debug, PartialEq)]
pub struct TexLayer {
    /// `LAYER_PX` x `LAYER_PX` RGBA8, sRGB.
    pub rgba: Vec<u8>,
    pub scale_m: f32,
    pub rotation_deg: f32,
    pub roughness: f32,
}

impl TexLayer {
    /// Any image size is fitted to the layer size.
    pub fn from_image(
        w: u32,
        h: u32,
        rgba: &[u8],
        scale_m: f32,
        rotation_deg: f32,
        roughness: f32,
    ) -> Option<TexLayer> {
        let data = fit_to_layer(w, h, rgba)?;
        Some(TexLayer {
            rgba: data,
            scale_m: clamp_scale(scale_m),
            rotation_deg: if rotation_deg.is_finite() { rotation_deg } else { 0.0 },
            roughness: roughness.clamp(0.0, 1.0),
        })
    }

    /// A built-in texture with its own tile size unless `scale_m` is given.
    pub fn builtin(key: &str, scale_m: Option<f32>, rotation_deg: f32) -> Option<TexLayer> {
        let (tile, rough) = builtin_info(key)?;
        Some(TexLayer {
            rgba: generate(key)?,
            scale_m: clamp_scale(scale_m.unwrap_or(tile)),
            rotation_deg: if rotation_deg.is_finite() { rotation_deg } else { 0.0 },
            roughness: rough,
        })
    }
}

/// Textures of one frame set; layer `i` is addressed by tag layer `i`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextureSet {
    pub layers: Vec<TexLayer>,
}

impl TextureSet {
    /// Shader table: 1/scale, cos, sin, roughness. Unused rows are zero.
    pub fn params(&self) -> [[f32; 4]; MAX_LAYERS] {
        let mut out = [[0.0; 4]; MAX_LAYERS];
        for (o, l) in out.iter_mut().zip(&self.layers) {
            let (s, c) = l.rotation_deg.to_radians().sin_cos();
            *o = [1.0 / clamp_scale(l.scale_m), c, s, l.roughness];
        }
        out
    }
}

// ---- hand-off to the renderer ----

struct Slot {
    rev: u64,
    set: Arc<TextureSet>,
}

static SLOT: Mutex<Option<Slot>> = Mutex::new(None);

/// Replaces the textures every renderer shows; an empty set switches textures off.
pub fn publish(set: TextureSet) {
    if let Ok(mut g) = SLOT.lock() {
        let rev = g.as_ref().map_or(1, |s| s.rev + 1);
        *g = Some(Slot {
            rev,
            set: Arc::new(set),
        });
    }
}

static WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A renderer says textures are on screen (realistic style, textures on); the model side decodes
/// images only after that. Tests may set it back.
pub fn set_wanted(on: bool) {
    WANTED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether some renderer shows image textures right now.
pub fn wanted() -> bool {
    WANTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The published set when it is newer than revision `seen`.
pub fn current(seen: u64) -> Option<(u64, Arc<TextureSet>)> {
    let g = SLOT.lock().ok()?;
    let s = g.as_ref().filter(|s| s.rev != seen)?;
    Some((s.rev, s.set.clone()))
}

// ---- resampling and mips ----

fn srgb_to_lin(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn lin_to_srgb(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let c = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (c * 255.0 + 0.5) as u8
}

fn lut() -> [f32; 256] {
    let mut t = [0.0; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = srgb_to_lin(i as u8);
    }
    t
}

/// Area average of an RGBA8 image to the layer size (nearest where it grows); None when the
/// buffer does not match the size.
pub fn fit_to_layer(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let px = LAYER_PX as usize;
    let (w, h) = (w as usize, h as usize);
    if w == px && h == px {
        return Some(rgba.to_vec());
    }
    let t = lut();
    let mut out = vec![0u8; px * px * 4];
    for y in 0..px {
        let (y0, y1) = span(y, px, h);
        for x in 0..px {
            let (x0, x1) = span(x, px, w);
            let mut acc = [0.0f32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = (sy * w + sx) * 4;
                    acc[0] += t[rgba[i] as usize];
                    acc[1] += t[rgba[i + 1] as usize];
                    acc[2] += t[rgba[i + 2] as usize];
                    acc[3] += rgba[i + 3] as f32 / 255.0;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            let o = (y * px + x) * 4;
            out[o] = lin_to_srgb(acc[0] / n);
            out[o + 1] = lin_to_srgb(acc[1] / n);
            out[o + 2] = lin_to_srgb(acc[2] / n);
            out[o + 3] = ((acc[3] / n) * 255.0 + 0.5) as u8;
        }
    }
    Some(out)
}

/// Source range [a, b) of destination cell `i` of `dst` cells over `src` samples; never empty.
fn span(i: usize, dst: usize, src: usize) -> (usize, usize) {
    let a = i * src / dst;
    let b = ((i + 1) * src / dst).max(a + 1);
    (a, b.min(src))
}

/// Levels from `px` down to 1; level 0 is `base`. Averages in linear light.
pub fn mip_chain(base: &[u8], px: u32) -> Vec<Vec<u8>> {
    let mut out = vec![base.to_vec()];
    let t = lut();
    let mut n = px as usize;
    while n > 1 {
        let prev = out.last().map(|v| v.as_slice()).unwrap_or(base);
        let m = n / 2;
        let mut next = vec![0u8; m * m * 4];
        for y in 0..m {
            for x in 0..m {
                let mut acc = [0.0f32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let i = ((2 * y + dy) * n + 2 * x + dx) * 4;
                    acc[0] += t[prev[i] as usize];
                    acc[1] += t[prev[i + 1] as usize];
                    acc[2] += t[prev[i + 2] as usize];
                    acc[3] += prev[i + 3] as f32 / 255.0;
                }
                let o = (y * m + x) * 4;
                next[o] = lin_to_srgb(acc[0] / 4.0);
                next[o + 1] = lin_to_srgb(acc[1] / 4.0);
                next[o + 2] = lin_to_srgb(acc[2] / 4.0);
                next[o + 3] = ((acc[3] / 4.0) * 255.0 + 0.5) as u8;
            }
        }
        out.push(next);
        n = m;
    }
    out
}

pub fn mip_levels(px: u32) -> u32 {
    32 - px.max(1).leading_zeros()
}

// ---- own procedural images (tileable) ----

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1)
        ^ (y as u32).wrapping_mul(0x85EB_CA77)
        ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h >> 8) as f32 / 16_777_216.0
}

/// Smooth value noise on a lattice of `pu` x `pv` cells that wraps with the texture.
fn noise(u: f32, v: f32, pu: i32, pv: i32, seed: u32) -> f32 {
    let (x, y) = (u * pu as f32, v * pv as f32);
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (x0, y0) = ((xi as i32).rem_euclid(pu), (yi as i32).rem_euclid(pv));
    let (x1, y1) = ((x0 + 1) % pu, (y0 + 1) % pv);
    let a = hash(x0, y0, seed) * (1.0 - sx) + hash(x1, y0, seed) * sx;
    let b = hash(x0, y1, seed) * (1.0 - sx) + hash(x1, y1, seed) * sx;
    a * (1.0 - sy) + b * sy
}

fn fbm(u: f32, v: f32, base: i32, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut p) = (0.0, 1.0, 0.0, base);
    for o in 0..octaves {
        sum += amp * noise(u, v, p, p, seed + o);
        norm += amp;
        amp *= 0.5;
        p *= 2;
    }
    sum / norm
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn scale3(c: [f32; 3], k: f32) -> [f32; 3] {
    [c[0] * k, c[1] * k, c[2] * k]
}

fn brick(u: f32, v: f32) -> [f32; 3] {
    // 3 bricks across, 8 courses; odd courses shift by half a brick
    let (rows, cols) = (8i32, 3i32);
    let ry = v * rows as f32;
    let row = ry.floor() as i32;
    let shift = if row.rem_euclid(2) == 1 { 0.5 } else { 0.0 };
    let bx = u * cols as f32 + shift;
    let col = (bx.floor() as i32).rem_euclid(cols);
    let (fx, fy) = (bx - bx.floor(), ry - ry.floor());
    // joints are about 3 px wide in both directions
    let ex = fx.min(1.0 - fx) / cols as f32;
    let ey = fy.min(1.0 - fy) / rows as f32;
    let mortar = ex.min(ey) < 0.0065;
    if mortar {
        let n = noise(u, v, 64, 64, 11);
        return scale3([0.66, 0.64, 0.60], 0.9 + 0.2 * n);
    }
    let tone = hash(col, row.rem_euclid(rows), 3);
    let base = mix3([0.55, 0.22, 0.15], [0.68, 0.32, 0.20], tone);
    let grain = fbm(u, v, 16, 3, 5);
    scale3(base, 0.82 + 0.3 * grain)
}

fn concrete(u: f32, v: f32) -> [f32; 3] {
    let wide = fbm(u, v, 4, 4, 21);
    let fine = noise(u, v, 128, 128, 22);
    let mut k = 0.52 + 0.22 * wide + 0.08 * fine;
    if noise(u, v, 40, 40, 23) > 0.86 {
        k *= 0.78;
    }
    [k, k * 0.995, k * 0.98]
}

fn wood(u: f32, v: f32) -> [f32; 3] {
    // four planks, grain along u
    let planks = 4.0;
    let id = (v * planks).floor() as i32;
    let fy = v * planks - (v * planks).floor();
    let tone = hash(id, 0, 31);
    let warp = noise(u, v, 2, 16, 32);
    let rings = 0.5 + 0.5 * (std::f32::consts::TAU * (v * 20.0 + 2.5 * warp)).sin();
    let fiber = noise(u, v, 4, 192, 33);
    let t = (0.55 * rings + 0.35 * fiber + 0.1 * tone).clamp(0.0, 1.0);
    let c = mix3([0.36, 0.22, 0.12], [0.66, 0.46, 0.27], t);
    let c = scale3(c, 0.88 + 0.24 * tone);
    if fy.min(1.0 - fy) < 0.012 {
        scale3(c, 0.45)
    } else {
        c
    }
}

fn tile(u: f32, v: f32) -> [f32; 3] {
    let n = 4.0;
    let (gx, gy) = (u * n, v * n);
    let (fx, fy) = (gx - gx.floor(), gy - gy.floor());
    let (ix, iy) = ((gx.floor() as i32).rem_euclid(4), (gy.floor() as i32).rem_euclid(4));
    if fx.min(1.0 - fx).min(fy.min(1.0 - fy)) < 0.018 {
        let k = 0.6 + 0.08 * noise(u, v, 64, 64, 41);
        return [k, k, k * 0.98];
    }
    let tone = hash(ix, iy, 42);
    let sheen = 0.04 * (1.0 - (fx - 0.5).abs() - (fy - 0.5).abs());
    let k = 0.84 + 0.06 * tone + sheen + 0.03 * noise(u, v, 96, 96, 43);
    [k, k, k * 0.97]
}

fn metal(u: f32, v: f32) -> [f32; 3] {
    // brushed: long streaks along u
    let streak = noise(u, v, 2, 160, 51);
    let fine = noise(u, v, 64, 256, 52);
    let k = 0.58 + 0.12 * streak + 0.05 * fine;
    [k * 0.97, k, k * 1.04]
}

fn glass(u: f32, v: f32) -> [f32; 3] {
    let k = fbm(u, v, 2, 2, 61);
    mix3([0.55, 0.74, 0.78], [0.68, 0.84, 0.86], k)
}

fn grass(u: f32, v: f32) -> [f32; 3] {
    let patch = fbm(u, v, 4, 3, 71);
    let blade = noise(u, v, 128, 128, 72);
    let c = mix3([0.20, 0.36, 0.11], [0.38, 0.55, 0.20], patch);
    scale3(c, 0.78 + 0.4 * blade)
}

/// RGBA8 sRGB image of a built-in texture, `LAYER_PX` square and tileable.
pub fn generate(key: &str) -> Option<Vec<u8>> {
    let f: fn(f32, f32) -> [f32; 3] = match key {
        "brick" => brick,
        "concrete" => concrete,
        "wood" => wood,
        "tile" => tile,
        "metal" => metal,
        "glass" => glass,
        "grass" => grass,
        _ => return None,
    };
    let n = LAYER_PX as usize;
    let mut out = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let c = f((x as f32 + 0.5) / n as f32, (y as f32 + 0.5) / n as f32);
            for ch in c {
                out.push((ch.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
            out.push(255);
        }
    }
    Some(out)
}

/// A small swatch (RGBA8, `px` square) of a texture at its real scale: `repeats` tiles per side.
pub fn swatch(layer: &TexLayer, px: u32, repeats: f32) -> Vec<u8> {
    let n = LAYER_PX as usize;
    let px = px.max(1) as usize;
    let mut out = Vec::with_capacity(px * px * 4);
    for y in 0..px {
        for x in 0..px {
            let (u, v) = (
                ((x as f32 + 0.5) / px as f32 * repeats).rem_euclid(1.0),
                ((y as f32 + 0.5) / px as f32 * repeats).rem_euclid(1.0),
            );
            let (tx, ty) = (
                ((u * n as f32) as usize).min(n - 1),
                ((v * n as f32) as usize).min(n - 1),
            );
            let i = (ty * n + tx) * 4;
            out.extend_from_slice(&layer.rgba[i..i + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    #[test]
    fn projection_follows_the_dominant_axis() {
        let p = [1.0, 2.0, 3.0];
        assert_eq!(surface_uv(p, [0.0, 0.0, 1.0]), [1.0, 2.0]);
        assert_eq!(surface_uv(p, [0.0, 0.0, -1.0]), [1.0, 2.0]);
        assert_eq!(surface_uv(p, [1.0, 0.0, 0.0]), [2.0, 3.0]);
        assert_eq!(surface_uv(p, [0.0, -1.0, 0.0]), [1.0, 3.0]);
        // ties go to z, then x: the shader does the same
        assert_eq!(surface_uv(p, [0.7, 0.7, 0.7]), [1.0, 2.0]);
        assert_eq!(surface_uv(p, [0.7, 0.7, 0.1]), [2.0, 3.0]);
    }

    #[test]
    fn one_tile_is_one_repeat_in_metres() {
        // a 0.6 m brick tile: 0.6 m along the wall is exactly one repeat
        let a = box_uv([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.6, 0.0);
        let b = box_uv([0.6, 5.0, 0.0], [0.0, 1.0, 0.0], 0.6, 0.0);
        assert!(close([b[0] - a[0], b[1] - a[1]], [1.0, 0.0]));
        // 0.6 m up is one repeat in v
        let c = box_uv([0.0, 0.0, 0.6], [0.0, 1.0, 0.0], 0.6, 0.0);
        assert!(close([c[0] - a[0], c[1] - a[1]], [0.0, 1.0]));
    }

    #[test]
    fn same_scale_on_every_element() {
        // a point shared by a wall and a slab gets the same coordinate along a shared axis
        let wall = box_uv([2.0, 1.0, 0.0], [0.0, 1.0, 0.0], 1.0, 0.0);
        let floor = box_uv([2.0, 1.0, 0.0], [0.0, 0.0, 1.0], 1.0, 0.0);
        assert!((wall[0] - floor[0]).abs() < 1e-6);
        // neighbouring elements on one plane agree exactly
        let n = [0.0, 1.0, 0.0];
        assert_eq!(box_uv([3.3, 0.0, 1.2], n, 0.4, 30.0), box_uv([3.3, 0.0, 1.2], n, 0.4, 30.0));
    }

    #[test]
    fn rotation_keeps_distances() {
        let p = [1.0, 0.0, 0.0];
        let r0 = layer_uv([p[0], p[1]], 1.0, 0.0);
        let r90 = layer_uv([p[0], p[1]], 1.0, 90.0);
        assert!(close(r0, [1.0, 0.0]));
        assert!(close(r90, [0.0, 1.0]));
        let l = |a: [f32; 2]| (a[0] * a[0] + a[1] * a[1]).sqrt();
        assert!((l(layer_uv([0.3, 0.7], 0.5, 37.0)) - l([0.6, 1.4])).abs() < 1e-4);
    }

    #[test]
    fn bad_scales_are_clamped() {
        assert_eq!(clamp_scale(f32::NAN), 1.0);
        assert_eq!(clamp_scale(0.0), SCALE_MIN);
        assert_eq!(clamp_scale(1e9), SCALE_MAX);
        let uv = layer_uv([1.0, 1.0], 0.0, 0.0);
        assert!(uv[0].is_finite() && uv[0] <= 1.0 / SCALE_MIN + 1e-3);
    }

    #[test]
    fn tags_carry_a_layer_only_where_they_may() {
        let t = with_layer(3, 5);
        assert_eq!(layer_of(t), Some(5));
        assert_eq!(t & 0xFFFF, 3);
        assert_eq!(layer_of(3), None);
        // glass, colour, flag and section-fill tags stay as they are
        assert_eq!(with_layer(crate::scene::TAG_GLASS, 1), crate::scene::TAG_GLASS);
        let rgb = crate::scene::tag_rgb([10, 20, 30], false);
        assert_eq!(with_layer(rgb, 1), rgb);
        assert_eq!(with_layer(crate::scene::TAG_EDGE, 1), crate::scene::TAG_EDGE);
        assert_eq!(with_layer(0x1000_0000 | 0x0033_4455, 1), 0x1000_0000 | 0x0033_4455);
        // a category in bits 24-27 does not hide the layer
        let c = crate::renderer::with_cat(with_layer(0, 7), 3);
        assert_eq!(layer_of(c), Some(7));
        // the table is the limit
        assert_eq!(with_layer(0, MAX_LAYERS as u8), 0);
        assert_eq!(layer_of(with_layer(0, (MAX_LAYERS - 1) as u8)), Some((MAX_LAYERS - 1) as u8));
    }

    #[test]
    fn built_ins_exist_and_tile() {
        for (key, tile_m, rough) in BUILTIN {
            let img = generate(key).unwrap_or_else(|| panic!("{key}"));
            let n = LAYER_PX as usize;
            assert_eq!(img.len(), n * n * 4, "{key}");
            assert!(img.chunks(4).all(|p| p[3] == 255));
            assert!(tile_m > 0.0 && (0.0..=1.0).contains(&rough));
            // not flat
            let first = &img[..3];
            assert!(img.chunks(4).any(|p| &p[..3] != first), "{key} is flat");
            // seamless: the wrap-around step is not much bigger than a step inside
            let at = |x: usize, y: usize| {
                let i = (y * n + x) * 4;
                img[i] as i32 + img[i + 1] as i32 + img[i + 2] as i32
            };
            let (mut inner, mut seam) = (0i64, 0i64);
            for y in 0..n {
                inner += (at(n / 2, y) - at(n / 2 - 1, y)).abs() as i64;
                seam += (at(0, y) - at(n - 1, y)).abs() as i64;
            }
            assert!(seam <= inner * 5 + 2000, "{key}: seam {seam} vs inner {inner}");
        }
        assert!(generate("nope").is_none());
        assert!(builtin_info("brick").is_some());
    }

    #[test]
    fn generation_is_deterministic() {
        assert_eq!(generate("brick"), generate("brick"));
    }

    #[test]
    fn mips_halve_down_to_one_pixel() {
        assert_eq!(mip_levels(256), 9);
        assert_eq!(mip_levels(1), 1);
        let base = vec![200u8; 8 * 8 * 4];
        let ch = mip_chain(&base, 8);
        assert_eq!(ch.len() as u32, mip_levels(8));
        assert_eq!(ch[1].len(), 4 * 4 * 4);
        assert_eq!(ch[3].len(), 4);
        // a flat image stays flat through the sRGB round trip
        assert!(ch.iter().all(|l| l.chunks(4).all(|p| p[..3] == [200, 200, 200] && p[3] == 200)));
    }

    #[test]
    fn mips_average_in_linear_light() {
        // black and white pixels average to about 188 in sRGB, not 128
        let mut base = vec![0u8; 2 * 2 * 4];
        for (i, p) in base.chunks_mut(4).enumerate() {
            let v = if i % 2 == 0 { 255 } else { 0 };
            p.copy_from_slice(&[v, v, v, 255]);
        }
        let ch = mip_chain(&base, 2);
        assert!((ch[1][0] as i32 - 188).abs() <= 2, "{}", ch[1][0]);
    }

    #[test]
    fn images_fit_the_layer() {
        assert!(fit_to_layer(0, 4, &[]).is_none());
        assert!(fit_to_layer(2, 2, &[0; 15]).is_none());
        let img = vec![255u8; 600 * 300 * 4];
        let out = fit_to_layer(600, 300, &img).unwrap();
        assert_eq!(out.len(), (LAYER_PX * LAYER_PX * 4) as usize);
        assert!(out.iter().all(|&b| b == 255));
        // upscale of a tiny image
        let tiny = [10u8, 20, 30, 255];
        let out = fit_to_layer(1, 1, &tiny).unwrap();
        assert_eq!(&out[..4], &tiny);
    }

    #[test]
    fn params_table_matches_the_layers() {
        let set = TextureSet {
            layers: vec![TexLayer::builtin("brick", Some(0.5), 90.0).unwrap()],
        };
        let p = set.params();
        assert!((p[0][0] - 2.0).abs() < 1e-5);
        assert!(p[0][1].abs() < 1e-5 && (p[0][2] - 1.0).abs() < 1e-5);
        assert_eq!(p[1], [0.0; 4]);
        assert!(TexLayer::builtin("nope", None, 0.0).is_none());
    }

    #[test]
    fn publish_is_seen_once_per_revision() {
        publish(TextureSet::default());
        let (rev, set) = current(0).expect("published");
        assert!(set.layers.is_empty());
        assert!(current(rev).is_none());
        publish(TextureSet::default());
        assert!(current(rev).is_some());
    }

    #[test]
    fn swatch_has_the_requested_size() {
        let l = TexLayer::builtin("tile", None, 0.0).unwrap();
        assert_eq!(swatch(&l, 32, 2.0).len(), 32 * 32 * 4);
    }
}
