// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Pictures of leaf cards, drawn once at start: three clusters of broad leaves on a twig and a
//! needle spray, as layers of one texture. Red is coverage (the cards are cut out where it is
//! under a half), green the tone of the leaf (fold, midrib, one leaf lighter than the next).
//! Every smaller level keeps the share of covered texels of the full one, so a far crown does
//! not thin out (alpha-tested coverage, Castaño 2010).

/// Side of a layer in texels.
pub const LEAF_SIZE: u32 = 256;
/// Layers: broad-leaf clusters 0..3, the needle spray last.
pub const LEAF_LAYERS: u32 = 4;
pub const NEEDLE_LAYER: u32 = 3;

struct Rng(u64);

impl Rng {
    fn f(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    }
    fn r(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

/// Coverage and tone of one layer at full size, `n` × `n`, row 0 at the card's foot (v = 0).
struct Canvas {
    n: usize,
    a: Vec<f32>,
    t: Vec<f32>,
}

/// Samples per texel side for the edges.
const SS: usize = 3;

impl Canvas {
    fn new(n: usize) -> Canvas {
        Canvas { n, a: vec![0.0; n * n], t: vec![0.0; n * n] }
    }

    /// Covers every texel of the box `lo..hi` (picture units) where `inside` gives a tone.
    fn paint(&mut self, lo: [f32; 2], hi: [f32; 2], inside: impl Fn(f32, f32) -> Option<f32>) {
        let n = self.n as f32;
        let x0 = ((lo[0] * n).floor().max(0.0)) as usize;
        let y0 = ((lo[1] * n).floor().max(0.0)) as usize;
        let x1 = ((hi[0] * n).ceil().min(n)) as usize;
        let y1 = ((hi[1] * n).ceil().min(n)) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let (mut cov, mut tone) = (0.0, 0.0);
                for sy in 0..SS {
                    for sx in 0..SS {
                        let u = (x as f32 + (sx as f32 + 0.5) / SS as f32) / n;
                        let v = (y as f32 + (sy as f32 + 0.5) / SS as f32) / n;
                        if let Some(t) = inside(u, v) {
                            cov += 1.0;
                            tone += t;
                        }
                    }
                }
                if cov > 0.0 {
                    let k = y * self.n + x;
                    let c = cov / (SS * SS) as f32;
                    // a nearer leaf covers the one under it
                    self.t[k] = self.t[k] * (1.0 - c) + tone / cov * c;
                    self.a[k] = (self.a[k] + c).min(1.0);
                }
            }
        }
    }

    /// A stroke from `p` to `q`, `w` wide, of tone `tone`.
    fn stroke(&mut self, p: [f32; 2], q: [f32; 2], w: f32, tone: f32) {
        let lo = [p[0].min(q[0]) - w, p[1].min(q[1]) - w];
        let hi = [p[0].max(q[0]) + w, p[1].max(q[1]) + w];
        let d = [q[0] - p[0], q[1] - p[1]];
        let l2 = (d[0] * d[0] + d[1] * d[1]).max(1e-12);
        self.paint(lo, hi, |u, v| {
            let s = (((u - p[0]) * d[0] + (v - p[1]) * d[1]) / l2).clamp(0.0, 1.0);
            let (ex, ey) = (u - p[0] - d[0] * s, v - p[1] - d[1] * s);
            (ex * ex + ey * ey <= w * w * 0.25).then_some(tone)
        });
    }

    /// A leaf from its stalk at `p` along the unit `dir`: `len` long, `wid` wide, pointed at the
    /// tip, a fold down the middle (one half lit, one in shade) and a darker midrib.
    fn leaf(&mut self, p: [f32; 2], dir: [f32; 2], len: f32, wid: f32, tone: f32) {
        let side = [-dir[1], dir[0]];
        let r = len.max(wid);
        self.paint([p[0] - r, p[1] - r], [p[0] + r, p[1] + r], |u, v| {
            let (x, y) = (u - p[0], v - p[1]);
            let s = (x * dir[0] + y * dir[1]) / len;
            let c = (x * side[0] + y * side[1]) / (wid * 0.5);
            if !(0.0..=1.0).contains(&s) {
                return None;
            }
            // ovate: widest a third of the way up, a drawn-out tip
            let half = (std::f32::consts::PI * s.powf(0.75)).sin().max(0.0).powf(0.85);
            if c.abs() > half {
                return None;
            }
            let fold = if c > 0.0 { 1.0 } else { 0.84 };
            let rib = if c.abs() < 0.07 { 0.82 } else { 1.0 };
            let edge = 0.9 + 0.1 * (1.0 - (c.abs() / half.max(1e-3)).powi(4));
            Some(tone * fold * rib * edge)
        });
    }
}

/// A cluster of broad leaves: a twig up from the foot of the card, side twigs left and right,
/// leaves fanned along them.
fn broadleaf(seed: u64) -> Canvas {
    let mut c = Canvas::new(LEAF_SIZE as usize);
    let mut r = Rng(seed.wrapping_mul(0x51_7CC1_B727_220A) ^ 0x1234);
    let bend = r.r(-0.12, 0.12);
    let twig = |s: f32| [0.5 + bend * s * s, 0.02 + 0.8 * s];
    let steps = 6;
    for k in 0..steps {
        c.stroke(twig(k as f32 / steps as f32), twig((k + 1) as f32 / steps as f32), 0.018 * (1.0 - 0.6 * k as f32 / steps as f32), 0.3);
    }
    let mut leaves: Vec<([f32; 2], [f32; 2], f32, f32, f32)> = vec![];
    let sprigs = 7;
    for k in 0..sprigs {
        let s = 0.1 + 0.78 * (k as f32 + r.r(0.0, 0.5)) / sprigs as f32;
        let at = twig(s);
        let sgn = if k % 2 == 0 { 1.0 } else { -1.0 };
        let ang = sgn * r.r(0.85, 1.4);
        let len = r.r(0.3, 0.42) * (1.0 - 0.4 * s);
        let d = [ang.sin(), ang.cos()];
        let end = [(at[0] + d[0] * len).clamp(0.12, 0.88), (at[1] + d[1] * len).clamp(0.1, 0.9)];
        c.stroke(at, end, 0.01, 0.32);
        // leaves along the side twig, alternate, the last one at its end
        let n = 5;
        for j in 0..n {
            let t = (j as f32 + 1.0) / n as f32;
            let p = [at[0] + (end[0] - at[0]) * t, at[1] + (end[1] - at[1]) * t];
            let out = if j % 2 == 0 { 1.0 } else { -1.0 };
            let a = (end[1] - at[1]).atan2(end[0] - at[0]) + if j + 1 == n { r.r(-0.2, 0.2) } else { out * r.r(0.6, 1.0) };
            let l = r.r(0.13, 0.18);
            leaves.push((p, [a.cos(), a.sin()], l, l * r.r(0.5, 0.64), r.r(0.62, 1.0)));
        }
    }
    // the top of the twig
    let top = twig(1.0);
    for a in [-0.5f32, 0.0, 0.5] {
        let a = std::f32::consts::FRAC_PI_2 + a + r.r(-0.15, 0.15);
        let l = r.r(0.12, 0.16);
        leaves.push((top, [a.cos(), a.sin()], l, l * 0.55, r.r(0.7, 1.0)));
    }
    for (p, d, l, w, t) in leaves {
        // kept inside the card, so no leaf is cut by its edge
        let tip = [p[0] + d[0] * l, p[1] + d[1] * l];
        let k = [(0.97 - p[0]) / (tip[0] - p[0]), (0.03 - p[0]) / (tip[0] - p[0]), (0.97 - p[1]) / (tip[1] - p[1]), (0.03 - p[1]) / (tip[1] - p[1])]
            .into_iter()
            .filter(|k| *k > 0.0)
            .fold(1.0f32, f32::min);
        c.leaf(p, d, l * k.clamp(0.4, 1.0), w * k.clamp(0.6, 1.0), t);
    }
    c
}

/// A needle spray: a shoot up the card with needles in pairs, slanting forward, and side shoots.
fn needles() -> Canvas {
    let mut c = Canvas::new(LEAF_SIZE as usize);
    let mut r = Rng(0xA5A5_1234);
    let shoot = |c: &mut Canvas, r: &mut Rng, p: [f32; 2], q: [f32; 2], len: f32| {
        c.stroke(p, q, 0.016, 0.35);
        let d = [q[0] - p[0], q[1] - p[1]];
        let l = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-6);
        let (dx, dy) = (d[0] / l, d[1] / l);
        let steps = (l / 0.012) as usize;
        for k in 0..steps {
            let s = (k as f32 + 0.5) / steps as f32;
            let at = [p[0] + d[0] * s, p[1] + d[1] * s];
            for sgn in [-1.0f32, 1.0] {
                let a = sgn * r.r(0.75, 1.05);
                let (ca, sa) = (a.cos(), a.sin());
                let nd = [dx * ca - dy * sa, dx * sa + dy * ca];
                let nl = len * r.r(0.75, 1.0) * (0.55 + 0.45 * (1.0 - s).sqrt().min(1.0)).max(0.5);
                let end = [(at[0] + nd[0] * nl).clamp(0.02, 0.98), (at[1] + nd[1] * nl).clamp(0.02, 0.98)];
                c.stroke(at, end, 0.011, r.r(0.6, 1.0));
            }
        }
    };
    shoot(&mut c, &mut r, [0.5, 0.02], [0.52, 0.95], 0.16);
    for (s, sgn) in [(0.12f32, -1.0f32), (0.25, 1.0), (0.38, -1.0), (0.5, 1.0), (0.62, -1.0), (0.74, 1.0)] {
        let p = [0.5 + 0.02 * s, 0.02 + 0.93 * s];
        let q = [(p[0] + sgn * 0.3).clamp(0.12, 0.88), (p[1] + 0.22).min(0.92)];
        shoot(&mut c, &mut r, p, q, 0.11);
    }
    c
}

/// Levels of one layer, full size first: [coverage, tone] per texel as bytes.
fn levels(c: &Canvas) -> Vec<Vec<[u8; 2]>> {
    let covered = |a: &[f32], k: f32| a.iter().filter(|x| **x * k >= 0.5).count() as f32 / a.len() as f32;
    let want = covered(&c.a, 1.0);
    let (mut a, mut t, mut n) = (c.a.clone(), c.t.clone(), c.n);
    let mut out = vec![];
    loop {
        // the scale that keeps the covered share: a bisection (coverage grows with the scale)
        let (mut lo, mut hi) = (0.25f32, 8.0f32);
        for _ in 0..24 {
            let m = 0.5 * (lo + hi);
            if covered(&a, m) < want {
                lo = m;
            } else {
                hi = m;
            }
        }
        let k = if out.is_empty() { 1.0 } else { hi };
        out.push(a.iter().zip(&t).map(|(a, t)| [((a * k).min(1.0) * 255.0).round() as u8, (t.clamp(0.0, 1.0) * 255.0).round() as u8]).collect());
        if n == 1 {
            return out;
        }
        let h = n / 2;
        let (mut a2, mut t2) = (vec![0.0; h * h], vec![0.0; h * h]);
        for y in 0..h {
            for x in 0..h {
                let ks = [2 * y * n + 2 * x, 2 * y * n + 2 * x + 1, (2 * y + 1) * n + 2 * x, (2 * y + 1) * n + 2 * x + 1];
                let sa: f32 = ks.iter().map(|k| a[*k]).sum();
                let st: f32 = ks.iter().map(|k| a[*k] * t[*k]).sum();
                a2[y * h + x] = sa * 0.25;
                // the tone of the leaves, not of the gaps between them
                t2[y * h + x] = if sa > 0.0 { st / sa } else { 0.7 };
            }
        }
        (a, t, n) = (a2, t2, h);
    }
}

/// Every layer's levels, full size first.
pub fn leaf_atlas() -> Vec<Vec<Vec<[u8; 2]>>> {
    let mut out: Vec<_> = (0..NEEDLE_LAYER).map(|k| levels(&broadleaf(k as u64 + 1))).collect();
    out.push(levels(&needles()));
    out
}

#[cfg(feature = "gpu")]
pub struct LeafGpu {
    tex: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    uploaded: bool,
}

#[cfg(feature = "gpu")]
impl LeafGpu {
    pub fn new(device: &wgpu::Device) -> Self {
        let mips = LEAF_SIZE.ilog2() + 1;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ddc-leaves"),
            size: wgpu::Extent3d { width: LEAF_SIZE, height: LEAF_SIZE, depth_or_array_layers: LEAF_LAYERS },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ddc-leaves"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 4,
            ..Default::default()
        });
        Self { tex, view, sampler, uploaded: false }
    }

    /// Draws the pictures into the texture on the first frame.
    pub fn upload(&mut self, queue: &wgpu::Queue) {
        if self.uploaded {
            return;
        }
        self.uploaded = true;
        for (layer, lv) in leaf_atlas().iter().enumerate() {
            for (mip, px) in lv.iter().enumerate() {
                let side = (LEAF_SIZE >> mip).max(1);
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.tex,
                        mip_level: mip as u32,
                        origin: wgpu::Origin3d { x: 0, y: 0, z: layer as u32 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytemuck::cast_slice(px),
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(side * 2), rows_per_image: Some(side) },
                    wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each picture covers some of its card and leaves gaps, keeps that share at every level and
    /// stays inside the card's edge, so no card shows a straight cut.
    #[test]
    fn leaf_pictures_cover_part_of_the_card_at_every_level() {
        let atlas = leaf_atlas();
        assert_eq!(atlas.len(), LEAF_LAYERS as usize);
        for (k, lv) in atlas.iter().enumerate() {
            assert_eq!(lv.len() as u32, LEAF_SIZE.ilog2() + 1);
            let share = |px: &[[u8; 2]]| px.iter().filter(|p| p[0] >= 128).count() as f32 / px.len() as f32;
            let full = share(&lv[0]);
            assert!(full > 0.25 && full < 0.75, "layer {k}: {full}");
            for (m, px) in lv.iter().enumerate().take(6) {
                assert!((share(px) - full).abs() < 0.08, "layer {k} level {m}: {} against {full}", share(px));
            }
            let n = LEAF_SIZE as usize;
            let rim = (0..n).filter(|i| lv[0][i * n][0] >= 128 || lv[0][i * n + n - 1][0] >= 128 || lv[0][n * (n - 1) + i][0] >= 128).count();
            assert!(rim < n / 20, "layer {k}: {rim} covered texels on the edge");
        }
    }
}
