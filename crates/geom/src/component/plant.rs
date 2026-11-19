//! Procedural plants: `plant <name> part(wood|foliage) form(..) height(..) crown(..) trunk(..)
//! clear(..) leaf(..) seed(..)`. A tree is decided once from its spec and seed (lean, uneven crown,
//! cluster sites, branch angles), so every level of detail shows the same tree: coarse is a
//! few crown lumps round a spindle and a trunk prism, medium has leaf clusters as low solids and a few branches,
//! fine has the full skeleton and rounder clusters. Seeds differ per tree, so neighbours of one
//! species do not repeat. Wood and foliage are separate solids (separate materials); the foliage
//! solid also carries the plan outline of the crown, the wood solid the trunk circle.

use super::Lod;
use crate::math::{v2, v3, V2, V3};
use std::collections::HashMap;
use std::f64::consts::{PI, TAU};

/// Crown form: the species table names one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Round,
    Oval,
    Conical,
    Columnar,
    Weeping,
    Palm,
    Vase,
    Spreading,
}

impl Form {
    pub const NAMES: [&'static str; 8] =
        ["round", "oval", "conical", "columnar", "weeping", "palm", "vase", "spreading"];

    pub fn from_code(c: f64) -> Option<Form> {
        Some(match c.round() as i64 {
            0 => Form::Round,
            1 => Form::Oval,
            2 => Form::Conical,
            3 => Form::Columnar,
            4 => Form::Weeping,
            5 => Form::Palm,
            6 => Form::Vase,
            7 => Form::Spreading,
            _ => return None,
        })
    }

    pub fn code(self) -> f64 {
        Form::NAMES.iter().position(|n| *n == self.name()).unwrap_or(0) as f64
    }

    pub fn name(self) -> &'static str {
        Form::NAMES[self as usize]
    }

    pub fn from_name(s: &str) -> Option<Form> {
        let k = Form::NAMES.iter().position(|n| *n == s.trim())?;
        Form::from_code(k as f64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Organ {
    Wood,
    Foliage,
    /// The shaded inner crown: a darker mass inside the foliage that shows through the gaps
    /// and from below; none for conifers, palms, bare trees and the coarse level.
    Shade,
    /// Lit lobes: the outer and top lobes of the foliage again, a hair larger, in a lighter tone.
    Light,
    /// Vertical bark streaks: thin dark ribs up the clear trunk.
    Streak,
}

impl Organ {
    pub fn from_name(s: &str) -> Option<Organ> {
        match s.trim() {
            "wood" => Some(Organ::Wood),
            "foliage" => Some(Organ::Foliage),
            "shade" => Some(Organ::Shade),
            "light" => Some(Organ::Light),
            "streak" => Some(Organ::Streak),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub form: Form,
    pub height: f64,
    /// Crown width, m.
    pub crown: f64,
    /// Trunk diameter at the base, m.
    pub trunk: f64,
    /// Height of the lowest foliage, m.
    pub clear: f64,
    /// Foliage fullness 0..1; 0 is a bare tree.
    pub leaf: f64,
    pub seed: u64,
}

#[derive(Default)]
pub struct Grown {
    pub pos: Vec<V3>,
    pub tris: Vec<[u32; 3]>,
    /// Volume of the organ, m3.
    pub volume: f64,
    pub plan: Vec<(V2, V2)>,
}

impl Spec {
    pub fn check(&self) -> Result<(), String> {
        let ok = |v: f64, lo: f64, hi: f64| v.is_finite() && v >= lo && v <= hi;
        if !ok(self.height, 0.2, 90.0) {
            return Err("plant height must be within 0.2..90 m".into());
        }
        if !ok(self.crown, 0.1, 70.0) {
            return Err("plant crown must be within 0.1..70 m".into());
        }
        if !ok(self.trunk, 0.01, 6.0) || self.trunk > self.crown.max(self.height) {
            return Err("plant trunk diameter must be within 0.01..6 m".into());
        }
        if !ok(self.clear, 0.0, self.height * 0.9) {
            return Err("plant clear height must be within 0..0.9 of the height".into());
        }
        if !ok(self.leaf, 0.0, 1.0) {
            return Err("plant leaf must be within 0..1".into());
        }
        Ok(())
    }
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64, salt: u64) -> Rng {
        let mut r = Rng(seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        r.next();
        r.next();
        r
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn r(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.f()
    }
}

/// Hash of two integers to 0..1: the same vertex always gets the same bump.
fn hash01(a: u64, b: u64) -> f64 {
    Rng::new(a, b.wrapping_add(7)).f()
}

#[derive(Default)]
struct Soup {
    pos: Vec<V3>,
    tris: Vec<[u32; 3]>,
}

impl Soup {
    fn v(&mut self, p: V3) -> u32 {
        self.pos.push(p);
        self.pos.len() as u32 - 1
    }
    fn t(&mut self, a: u32, b: u32, c: u32) {
        self.tris.push([a, b, c]);
    }
    fn volume(&self) -> f64 {
        let mut s = 0.0;
        for t in &self.tris {
            let (a, b, c) = (self.pos[t[0] as usize], self.pos[t[1] as usize], self.pos[t[2] as usize]);
            s += a.dot(b.cross(c));
        }
        (s / 6.0).abs()
    }
}

/// Tube along a polyline with a radius per point; closed at the tip, at the base on request.
fn tube(m: &mut Soup, path: &[V3], rad: &[f64], sides: usize, close_base: bool) {
    let n = path.len();
    if n < 2 || rad.len() != n || sides < 3 {
        return;
    }
    let dir = |i: usize| -> V3 {
        let a = if i == 0 { path[0] } else { path[i - 1] };
        let b = if i + 1 < n { path[i + 1] } else { path[i] };
        let d = b - a;
        let l = d.len();
        if l < 1e-12 {
            v3(0.0, 0.0, 1.0)
        } else {
            d * (1.0 / l)
        }
    };
    let d0 = dir(0);
    let refv = if d0.z.abs() > 0.9 { v3(1.0, 0.0, 0.0) } else { v3(0.0, 0.0, 1.0) };
    let mut u = d0.cross(refv).normalized();
    let mut rings: Vec<Vec<u32>> = Vec::with_capacity(n);
    for i in 0..n {
        let d = dir(i);
        let q = u - d * u.dot(d);
        let l = q.len();
        u = if l < 1e-9 { d.cross(refv).normalized() } else { q * (1.0 / l) };
        let w = d.cross(u);
        let mut ring = Vec::with_capacity(sides);
        for j in 0..sides {
            let a = TAU * j as f64 / sides as f64;
            ring.push(m.v(path[i] + (u * a.cos() + w * a.sin()) * rad[i].max(1e-4)));
        }
        rings.push(ring);
    }
    for i in 0..n - 1 {
        for j in 0..sides {
            let k = (j + 1) % sides;
            let (a, b, c, d) = (rings[i][j], rings[i][k], rings[i + 1][k], rings[i + 1][j]);
            m.t(a, b, c);
            m.t(a, c, d);
        }
    }
    let top = m.v(path[n - 1]);
    for j in 0..sides {
        m.t(top, rings[n - 1][j], rings[n - 1][(j + 1) % sides]);
    }
    if close_base {
        let c = m.v(path[0]);
        for j in 0..sides {
            m.t(c, rings[0][(j + 1) % sides], rings[0][j]);
        }
    }
}

const ICO: [[f64; 3]; 12] = {
    let t = 1.618_033_988_749_895;
    [
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ]
};

const ICO_F: [[u32; 3]; 20] = [
    [0, 11, 5],
    [0, 5, 1],
    [0, 1, 7],
    [0, 7, 10],
    [0, 10, 11],
    [1, 5, 9],
    [5, 11, 4],
    [11, 10, 2],
    [10, 7, 6],
    [7, 1, 8],
    [3, 9, 4],
    [3, 4, 2],
    [3, 2, 6],
    [3, 6, 8],
    [3, 8, 9],
    [4, 9, 5],
    [2, 4, 11],
    [6, 2, 10],
    [8, 6, 7],
    [9, 8, 1],
];

/// A bumpy ellipsoid: icosahedron (`sub` = 0) or its once subdivided form (`sub` = 1).
fn blob(m: &mut Soup, c: V3, rx: f64, rz: f64, sub: u32, seed: u64, bump: f64) {
    let mut dirs: Vec<V3> = ICO.iter().map(|p| v3(p[0], p[1], p[2]).normalized()).collect();
    let mut faces: Vec<[u32; 3]> = ICO_F.to_vec();
    for _ in 0..sub {
        let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
        let mut out = Vec::with_capacity(faces.len() * 4);
        for f in &faces {
            let mut ix = [0u32; 3];
            for k in 0..3 {
                let (a, b) = (f[k], f[(k + 1) % 3]);
                let key = (a.min(b), a.max(b));
                ix[k] = *mid.entry(key).or_insert_with(|| {
                    let p = (dirs[a as usize] + dirs[b as usize]).normalized();
                    dirs.push(p);
                    dirs.len() as u32 - 1
                });
            }
            out.push([f[0], ix[0], ix[2]]);
            out.push([f[1], ix[1], ix[0]]);
            out.push([f[2], ix[2], ix[1]]);
            out.push([ix[0], ix[1], ix[2]]);
        }
        faces = out;
    }
    let base = m.pos.len() as u32;
    for (i, d) in dirs.iter().enumerate() {
        let k = 1.0 + bump * (hash01(seed, i as u64) - 0.5) * 2.0;
        m.v(c + v3(d.x * rx * k, d.y * rx * k, d.z * rz * k));
    }
    for f in &faces {
        let (a, b, cc) = (m.pos[(base + f[0]) as usize], m.pos[(base + f[1]) as usize], m.pos[(base + f[2]) as usize]);
        let n = (b - a).cross(cc - a);
        let mid = (a + b + cc) * (1.0 / 3.0) - c;
        if n.dot(mid) >= 0.0 {
            m.t(base + f[0], base + f[1], base + f[2]);
        } else {
            m.t(base + f[0], base + f[2], base + f[1]);
        }
    }
}

/// Crown radius as a share of the half width at height share `t` of the crown.
fn prof(form: Form, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let ell = |u: f64| (1.0 - (2.0 * u - 1.0).powi(2)).max(0.0).sqrt();
    match form {
        Form::Round => ell(t.powf(0.85)),
        Form::Oval => ell(t),
        Form::Conical => (1.0 - t).powf(0.9),
        Form::Columnar => (1.0 - (2.0 * t - 1.0).abs().powi(3)).max(0.0).sqrt(),
        Form::Weeping => (PI * t.powf(0.7)).sin().max(0.0).powf(0.7),
        Form::Vase => (0.28 + 0.72 * t) * (1.0 - 0.7 * t.powi(8)),
        Form::Spreading => ell(t.powf(1.1)),
        Form::Palm => 0.1,
    }
}

/// What is decided once per tree and shared by all levels of detail.
struct Look {
    lean: V2,
    amp: [f64; 2],
    ph: [f64; 2],
    /// Seed of the fine bumps.
    bump: u64,
}

impl Look {
    fn of(s: &Spec) -> Look {
        let mut r = Rng::new(s.seed, 0);
        let lean_r = s.crown * 0.07 * r.f();
        let a = r.r(0.0, TAU);
        Look {
            lean: v2(lean_r * a.cos(), lean_r * a.sin()),
            amp: [r.r(0.06, 0.14), r.r(0.03, 0.08)],
            ph: [r.r(0.0, TAU), r.r(0.0, TAU)],
            bump: r.next(),
        }
    }
    /// Uneven crown: radius factor around the tree.
    fn modul(&self, th: f64) -> f64 {
        1.0 + self.amp[0] * (th + self.ph[0]).sin() + self.amp[1] * (2.0 * th + self.ph[1]).sin()
    }
}

struct Cl {
    c: V3,
    rx: f64,
    rz: f64,
}

fn z_factor(form: Form) -> f64 {
    match form {
        Form::Columnar => 1.9,
        Form::Oval => 1.15,
        Form::Spreading => 0.6,
        Form::Round => 0.85,
        _ => 1.0,
    }
}

/// Cluster sites of a broadleaf crown, in a low-discrepancy order: every second one is still
/// spread over the whole crown.
fn clusters(s: &Spec, lk: &Look) -> Vec<Cl> {
    let mut r = Rng::new(s.seed, 2);
    let (z0, z1) = (s.clear, s.height);
    let depth = z1 - z0;
    let rr = s.crown / 2.0;
    let n = match s.form {
        Form::Weeping => 16,
        Form::Columnar => 20,
        Form::Spreading => 22,
        Form::Oval => 24,
        _ => 26,
    };
    let (off_t, off_a) = (r.f(), r.r(0.0, TAU));
    let base = 0.5 * (0.55 * rr + 0.2 * depth);
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        let t = 0.08 + 0.84 * (k as f64 * 0.618_033_988_7 + off_t).fract();
        let th = off_a + k as f64 * 2.399_963;
        let env = prof(s.form, t) * rr * lk.modul(th);
        let cr = (base * r.r(0.85, 1.15) * (0.9 + 0.1 * lk.modul(th))).min(0.62 * rr).max(0.12 * s.crown);
        let rho = (env - cr * 0.7).max(0.0) * r.r(0.55, 1.0);
        let rz = cr * z_factor(s.form);
        let mut z = z0 + t * depth;
        // the bumps swell a cluster by up to a fifth: keep it inside the tree's height
        z = z.max(z0 + 0.3 * rz).max(0.2 + 1.3 * rz).min(z1 - 1.4 * rz);
        out.push(Cl { c: v3(lk.lean.x + rho * th.cos(), lk.lean.y + rho * th.sin(), z), rx: cr, rz });
    }
    if s.form == Form::Weeping {
        // curtains of hanging growth round the crown
        let m = 8;
        let len = depth * 0.62;
        for k in 0..m {
            let th = off_a + TAU * k as f64 / m as f64 + r.r(-0.2, 0.2);
            let rho = rr * 0.82 * lk.modul(th) * r.r(0.85, 1.0);
            let rz = len * r.r(0.45, 0.55);
            let z = (z0 + rz).max(z1 - depth * 0.45 - rz * 0.2);
            out.push(Cl { c: v3(lk.lean.x + rho * th.cos(), lk.lean.y + rho * th.sin(), z.min(z1 - 1.4 * rz)), rx: rr * 0.2, rz });
        }
    }
    out
}

/// Distance from the trunk to the far edge of the crown discs along the angle `th`.
fn reach(discs: &[(f64, f64, f64)], floor: f64, th: f64) -> f64 {
    let (u, q) = ((th.cos(), th.sin()), (-th.sin(), th.cos()));
    let mut best = floor;
    for &(cx, cy, r) in discs {
        let (s, p) = (cx * u.0 + cy * u.1, cx * q.0 + cy * q.1);
        if p.abs() < r {
            best = best.max(s + (r * r - p * p).sqrt());
        }
    }
    best
}

/// Plan symbol of the crown by species: a conifer is a star (tips on the crown edge, rays from
/// the trunk to them), a palm a rosette of fronds, any other tree a scalloped crown outline
/// with branch spokes. The radius of every shape follows the crown's own reach, so the symbol
/// is as large as the (scaled) crown; a palm has no crown discs along its fronds, so it takes the
/// crown radius itself.
fn symbol(form: Form, discs: &[(f64, f64, f64)], floor: f64, crown_r: f64, seed: u64, lod: Lod) -> Vec<(V2, V2)> {
    let mut r = Rng::new(seed, 11);
    let off = r.r(0.0, TAU);
    let at = |th: f64, len: f64| v2(th.cos() * len, th.sin() * len);
    let close = |pts: &[V2]| -> Vec<(V2, V2)> { (0..pts.len()).map(|k| (pts[k], pts[(k + 1) % pts.len()])).collect() };
    match form {
        Form::Conical | Form::Columnar => {
            let k = match lod {
                Lod::Coarse => 7,
                Lod::Medium => 9,
                Lod::Fine => 11,
            };
            let mut pts = vec![];
            let mut out = vec![];
            for i in 0..k {
                let th = off + TAU * i as f64 / k as f64;
                let tip = at(th, reach(discs, floor, th));
                let vth = th + TAU / (2 * k) as f64;
                pts.push(tip);
                pts.push(at(vth, 0.55 * reach(discs, floor, vth)));
                out.push((at(th, floor), tip * 0.8));
            }
            let mut star = close(&pts);
            star.extend(out);
            star
        }
        Form::Palm => {
            let m = match lod {
                Lod::Coarse => 6,
                Lod::Medium => 8,
                Lod::Fine => 10,
            };
            let mut out = vec![];
            for i in 0..m {
                let th = off + TAU * i as f64 / m as f64 + r.r(-0.12, 0.12);
                let len = crown_r * r.r(0.85, 1.05);
                let (tip, root) = (at(th, len), at(th, floor));
                let (u, n) = (at(th, 1.0), at(th + PI / 2.0, 1.0));
                let mid = u * (0.55 * len);
                let (a, b) = (mid + n * (0.14 * len), mid - n * (0.14 * len));
                out.extend([(root, a), (a, tip), (tip, b), (b, root)]);
            }
            out
        }
        _ => {
            let (lobes, seg) = match lod {
                Lod::Coarse => (10, 3),
                Lod::Medium => (14, 4),
                Lod::Fine => (18, 5),
            };
            let n = lobes * seg;
            let pts: Vec<V2> = (0..n)
                .map(|j| {
                    let th = off + TAU * j as f64 / n as f64;
                    let t = (j % seg) as f64 / seg as f64;
                    // a lobe is a bump: full radius at its middle, pinched at the joints
                    at(th, reach(discs, floor, th) * (0.84 + 0.16 * (PI * (t + 0.5 / seg as f64)).sin()))
                })
                .collect();
            let mut out = close(&pts);
            out.extend(spokes(discs, floor, seed, lod));
            out
        }
    }
}

/// Branch pattern of the plan symbol: spokes from the trunk towards the crown edge, forked at
/// the finer levels, so a tree reads as a tree and not as a bare circle.
fn spokes(discs: &[(f64, f64, f64)], floor: f64, seed: u64, lod: Lod) -> Vec<(V2, V2)> {
    let (n, forks) = match lod {
        Lod::Coarse => (6, false),
        Lod::Medium => (9, true),
        Lod::Fine => (13, true),
    };
    let mut r = Rng::new(seed, 9);
    let off = r.r(0.0, TAU);
    let mut out = vec![];
    for k in 0..n {
        let th = off + TAU * k as f64 / n as f64 + r.r(-0.3, 0.3);
        let len = (reach(discs, floor, th) * r.r(0.55, 0.85)).max(floor);
        let dir = v2(th.cos(), th.sin());
        out.push((dir * floor, dir * len));
        if forks {
            let at = dir * (len * 0.6);
            for side in [-1.0, 1.0] {
                let a = th + side * r.r(0.4, 0.7);
                let d = v2(a.cos(), a.sin());
                let mut l = len * r.r(0.3, 0.45);
                // a twig stays inside the crown outline along its whole length
                let inside = |l: f64| [0.5, 1.0].iter().all(|t| {
                    let e = at + d * (l * t);
                    e.len() <= 0.92 * reach(discs, floor, e.y.atan2(e.x))
                });
                for _ in 0..6 {
                    if inside(l) {
                        break;
                    }
                    l *= 0.7;
                }
                if !inside(l) {
                    l = 0.0;
                }
                out.push((at, at + d * l));
            }
        }
    }
    out
}

fn circle_lines(r: f64, n: usize) -> Vec<(V2, V2)> {
    let p = |k: usize| v2(r * (TAU * k as f64 / n as f64).cos(), r * (TAU * k as f64 / n as f64).sin());
    (0..n).map(|k| (p(k), p((k + 1) % n))).collect()
}

fn env_volume(s: &Spec) -> f64 {
    let (n, rr) = (24, s.crown / 2.0);
    let dz = (s.height - s.clear) / n as f64;
    let mut v = 0.0;
    for i in 0..n {
        let r = prof(s.form, (i as f64 + 0.5) / n as f64) * rr;
        v += PI * r * r * dz;
    }
    v * 0.6 * s.leaf
}

/// Stacked skirts of a conifer.
fn conifer(s: &Spec, lk: &Look, lod: Lod) -> (Soup, Vec<(f64, f64, f64)>) {
    let mut m = Soup::default();
    let mut r = Rng::new(s.seed, 3);
    let (n, sides) = match lod {
        Lod::Coarse => (4, 6),
        Lod::Medium => (6, 8),
        Lod::Fine => (9, 11),
    };
    let (z0, z1, rr) = (s.clear, s.height, s.crown / 2.0);
    let depth = z1 - z0;
    // the random numbers are drawn for the fine count so coarse tiers match the fine ones
    let mut discs = vec![];
    for i in 0..n {
        let t = i as f64 / n as f64;
        let zb = z0 + depth * t * 0.9;
        let h = (depth / n as f64 * 2.1).min(z1 - zb);
        let rad = rr * prof(Form::Conical, t + 0.4 / n as f64) * lk.modul(i as f64 * 1.7);
        let off = r.r(0.0, TAU);
        let (cx, cy) = (lk.lean.x * (zb / s.height), lk.lean.y * (zb / s.height));
        let mut ring = vec![];
        for j in 0..sides {
            let a = off + TAU * j as f64 / sides as f64;
            let k = (0.82 + 0.28 * r.f()) * if j % 2 == 0 { 1.0 } else { 0.8 };
            let q = rad * k;
            ring.push(m.v(v3(cx + q * a.cos(), cy + q * a.sin(), zb - 0.08 * q)));
        }
        let apex = m.v(v3(cx, cy, zb + h));
        let centre = m.v(v3(cx, cy, zb + 0.1 * h));
        for j in 0..sides {
            let k = (j + 1) % sides;
            m.t(ring[j], ring[k], apex);
            m.t(centre, ring[k], ring[j]);
        }
        discs.push((cx, cy, rad * 0.93));
    }
    (m, discs)
}

/// Fronds of a palm round the top of the trunk at `top`.
fn fronds(s: &Spec, lk: &Look, lod: Lod, top: V3) -> (Soup, Vec<(f64, f64, f64)>) {
    let mut m = Soup::default();
    let mut r = Rng::new(s.seed, 4);
    let (n, segs) = match lod {
        Lod::Coarse => (6, 1),
        Lod::Medium => (9, 2),
        Lod::Fine => (14, 5),
    };
    let rr = s.crown / 2.0;
    let mut discs = vec![];
    for i in 0..n {
        let a = TAU * i as f64 / n as f64 + r.r(-0.2, 0.2);
        let len = rr * r.r(0.9, 1.1) * lk.modul(a);
        let rise = len * r.r(0.4, 0.7);
        let w = (len * 0.07).max(0.02);
        let mut path = vec![];
        let mut rad = vec![];
        for k in 0..=segs {
            let u = k as f64 / segs as f64;
            let rho = len * u;
            let z = rise * u * 1.5 - rise * 1.9 * u * u + len * 0.05;
            path.push(top + v3(rho * a.cos(), rho * a.sin(), z));
            rad.push(w * (1.0 - 0.85 * u));
        }
        tube(&mut m, &path, &rad, 3, true);
        let tip = path[path.len() - 1];
        discs.push((tip.x, tip.y, len * 0.22));
    }
    (m, discs)
}

struct Trunk {
    height: f64,
    r0: f64,
    lean: V2,
    taper: f64,
    wob: f64,
    ph: f64,
}

impl Trunk {
    fn at(&self, f: f64) -> V3 {
        let w = (f * 5.0 + self.ph).sin() * self.wob * f.min(1.0);
        let k = f.powf(1.7);
        v3(self.lean.x * k + w, self.lean.y * k - w * 0.6, f * self.height)
    }
    fn r(&self, f: f64) -> f64 {
        let flare = 1.0 + 0.25 * (1.0 - (f * 8.0).min(1.0));
        (self.r0 * (1.0 - self.taper * f) * flare).max(self.r0 * 0.06)
    }
}

fn branch_rise(form: Form, r: &mut Rng) -> f64 {
    let deg = match form {
        Form::Vase => r.r(55.0, 75.0),
        Form::Columnar => r.r(70.0, 82.0),
        Form::Spreading => r.r(12.0, 28.0),
        Form::Weeping => r.r(35.0, 55.0),
        _ => r.r(30.0, 55.0),
    };
    deg.to_radians()
}

/// Where the palm's trunk ends and the fronds start.
fn palm_top(s: &Spec) -> f64 {
    s.height * 0.97 - 0.15 * s.crown
}

/// The trunk of a tree and the height it reaches, with the generator its branches go on with.
fn trunk_of(s: &Spec, lk: &Look) -> (Trunk, f64, Rng) {
    let mut r = Rng::new(s.seed, 1);
    let palm = s.form == Form::Palm;
    let conic = s.form == Form::Conical;
    let top_z = match s.form {
        Form::Palm => palm_top(s),
        Form::Conical => s.height * 0.96,
        Form::Columnar => s.height * 0.9,
        _ => s.clear + (s.height - s.clear) * 0.8,
    };
    let tr = Trunk {
        height: top_z,
        r0: s.trunk / 2.0,
        lean: if palm { lk.lean * 1.8 } else { lk.lean },
        taper: if conic { 0.92 } else if palm { 0.25 } else { 0.5 },
        wob: if palm { 0.0 } else { s.trunk * 0.25 * r.f() },
        ph: r.r(0.0, TAU),
    };
    (tr, top_z, r)
}

/// Thin ribs up the clear trunk, dark against the bark: none at the coarse level.
fn streak(s: &Spec, lk: &Look, lod: Lod) -> Soup {
    let mut m = Soup::default();
    let n = match lod {
        Lod::Coarse => return m,
        Lod::Medium => 3,
        Lod::Fine => 5,
    };
    let (tr, top_z, _) = trunk_of(s, lk);
    let up = if s.form == Form::Palm { 0.95 } else { (s.clear / top_z).clamp(0.25, 0.95) };
    let mut r = Rng::new(s.seed, 3);
    for i in 0..n {
        let az = (i as f64 + 0.4 * r.f()) * TAU / n as f64;
        let (mut path, mut rad) = (vec![], vec![]);
        for k in 0..4 {
            let f = up * k as f64 / 3.0;
            let rt = tr.r(f);
            path.push(tr.at(f) + v3(az.cos() * rt * 0.97, az.sin() * rt * 0.97, 0.0));
            rad.push((rt * 0.12).max(0.004));
        }
        tube(&mut m, &path, &rad, 4, true);
    }
    m
}

/// Outer and top lobes of the foliage, copied slightly larger so they cover the originals.
fn light(s: &Spec, lk: &Look, lod: Lod) -> Soup {
    let mut m = Soup::default();
    if s.leaf < 0.05 || lod == Lod::Coarse || matches!(s.form, Form::Conical | Form::Palm) {
        return m;
    }
    let cl = clusters(s, lk);
    let (top, out) = (s.clear + LIT_TOP * (s.height - s.clear), LIT_OUT * s.crown / 2.0);
    for (k, c) in cl.iter().enumerate() {
        if (k as f64) >= cl.len() as f64 * s.leaf.max(0.3) + 1.0 || (c.c.z < top && c.c.x.hypot(c.c.y) < out) {
            continue;
        }
        match lod {
            Lod::Fine => blob(&mut m, c.c, c.rx * LIT_GROW, c.rz * LIT_GROW, 1, lk.bump ^ k as u64, 0.16),
            _ if k % 2 == 0 => blob(&mut m, c.c, c.rx * 1.25 * LIT_GROW, c.rz * 1.2 * LIT_GROW, 0, lk.bump ^ k as u64, 0.12),
            _ => {}
        }
    }
    m
}

/// A lobe is lit when it is in the top share of the crown or this share of the radius out.
const LIT_TOP: f64 = 0.6;
const LIT_OUT: f64 = 0.6;
const LIT_GROW: f64 = 1.04;

fn wood(s: &Spec, lk: &Look, lod: Lod) -> Soup {
    wood_tips(s, lk, lod).0
}

/// The wood and the ends of its branches and twigs.
fn wood_tips(s: &Spec, lk: &Look, lod: Lod) -> (Soup, Vec<V3>) {
    let mut m = Soup::default();
    let mut tips: Vec<V3> = vec![];
    let (rings, sides) = match lod {
        Lod::Coarse => (3, 5),
        Lod::Medium => (5, 8),
        Lod::Fine => (8, 12),
    };
    let palm = s.form == Form::Palm;
    let conic = s.form == Form::Conical;
    let (tr, top_z, mut r) = trunk_of(s, lk);
    let (mut path, mut rad) = (vec![], vec![]);
    for i in 0..rings {
        let f = i as f64 / (rings - 1) as f64;
        path.push(tr.at(f));
        rad.push(tr.r(f));
    }
    tube(&mut m, &path, &rad, sides, true);
    if palm || conic || lod == Lod::Coarse {
        return (m, tips);
    }
    let cl = clusters(s, lk);
    let (_, lobes) = lobes_of(s, lk, lod, &cl);
    let bare = s.leaf < 0.2;
    let mut nb = if lod == Lod::Fine { 7 } else { 4 };
    if bare {
        nb = nb * 2 - 1;
    }
    let bsides = if lod == Lod::Fine { 6 } else { 4 };
    let (fs, fe) = ((s.clear / top_z * 0.85).min(0.9), 0.92);
    let rr = s.crown / 2.0;
    let depth = s.height - s.clear;
    if !lobes.is_empty() {
        // every lobe is reached: the main branches end in lobes spread over the crown, the
        // others are twigs from the nearest point of a main branch
        let bn = nb.min(lobes.len());
        let mains: Vec<usize> = (0..bn).map(|i| i * lobes.len() / bn).collect();
        let mut joints: Vec<V3> = vec![];
        for &i in &mains {
            let c = lobes[i].c;
            let dh = (c.x - tr.at(0.5).x).hypot(c.y - tr.at(0.5).y);
            let fb = ((c.z - 0.35 * dh) / top_z).clamp(fs, fe);
            let (p0, rb) = (tr.at(fb), tr.r(fb) * 0.6);
            let mid = p0 + (c - p0) * 0.5 + v3(0.0, 0.0, 0.1 * dh);
            tube(&mut m, &[p0, mid, c], &[rb, rb * 0.55, rb * 0.2], bsides, false);
            joints.extend([p0, mid, c]);
            tips.push(c);
        }
        for (_, l) in lobes.iter().enumerate().filter(|(i, _)| !mains.contains(i)) {
            let from = joints.iter().copied().min_by(|a, b| (*a - l.c).len().total_cmp(&(*b - l.c).len())).unwrap_or(l.c);
            let mid = from + (l.c - from) * 0.5 + v3(0.0, 0.0, 0.05 * (l.c - from).len());
            let rb = tr.r(fe) * 0.25;
            tube(&mut m, &[from, mid, l.c], &[rb, rb * 0.6, rb * 0.25], 4, false);
            tips.push(l.c);
        }
        return (m, tips);
    }
    for b in 0..nb {
        let fb = fs + (fe - fs) * (b as f64 * 0.618_033_988_7 + 0.13).fract();
        let az = b as f64 * 2.399_963 + r.r(-0.3, 0.3);
        let rise_a = branch_rise(s.form, &mut r);
        let (p0, rb) = (tr.at(fb), tr.r(fb) * 0.6);
        let t = ((p0.z - s.clear) / depth).clamp(0.0, 1.0);
        let reach = (rr * prof(s.form, t).max(0.3) * lk.modul(az) * r.r(0.7, 0.95)).max(0.2);
        let rise = (reach * rise_a.tan()).min((s.height - p0.z) * 0.9);
        let d = (az.cos(), az.sin());
        let droop = if s.form == Form::Weeping { rise * 1.6 } else { 0.0 };
        let pts = [
            p0,
            p0 + v3(d.0 * reach * 0.5, d.1 * reach * 0.5, rise * 0.55),
            p0 + v3(d.0 * reach, d.1 * reach, rise - droop),
        ];
        tube(&mut m, &pts, &[rb, rb * 0.55, rb * 0.2], bsides, false);
        if lod == Lod::Fine {
            let twigs = if bare { 3 } else { 2 };
            for k in 0..twigs {
                let a2 = az + (k as f64 - (twigs as f64 - 1.0) / 2.0) * 0.8;
                let (q, l) = (pts[1], reach * 0.45);
                let tw = [
                    q,
                    q + v3(a2.cos() * l * 0.5, a2.sin() * l * 0.5, l * 0.25),
                    q + v3(a2.cos() * l, a2.sin() * l, l * 0.3 - droop * 0.2),
                ];
                tube(&mut m, &tw, &[rb * 0.4, rb * 0.25, rb * 0.1], 4, false);
            }
        }
    }
    (m, tips)
}

/// One blob of the broadleaf crown as it is drawn at a level.
struct Lobe {
    c: V3,
    rx: f64,
    rz: f64,
    sub: u32,
    amp: f64,
    /// Index of the cluster it stands for: the seed of its bumps.
    k: usize,
}

/// What of an ellipsoid blob is surely solid: its mesh is inscribed (an icosahedron of the first
/// level reaches 0.79 of the radius) and bumped by up to a fifth.
const SOLID: f64 = 0.65;

fn solid(l: &Lobe) -> Ell {
    Ell { c: l.c, rx: l.rx * SOLID, rz: l.rz * SOLID }
}

/// The core (coarse level only) and the lobes drawn at `lod`; none for a bare, conifer or palm
/// crown. At the coarse level every lump is drawn pulled towards the core until their solid
/// parts overlap, so none floats.
fn lobes_of(s: &Spec, lk: &Look, lod: Lod, cl: &[Cl]) -> (Option<Lobe>, Vec<Lobe>) {
    if s.leaf < 0.05 || matches!(s.form, Form::Conical | Form::Palm) {
        return (None, vec![]);
    }
    // fewer leaves, fewer clusters: every k-th of the spread order stays
    let keep = |k: usize, step: usize| k % step == 0 && (k as f64) < cl.len() as f64 * s.leaf.max(0.3) + 1.0;
    let mut out = vec![];
    match lod {
        Lod::Fine => {
            for (k, c) in cl.iter().enumerate().filter(|(k, _)| keep(*k, 1)) {
                out.push(Lobe { c: c.c, rx: c.rx, rz: c.rz, sub: 1, amp: 0.16, k });
            }
            (None, out)
        }
        Lod::Medium => {
            for (k, c) in cl.iter().enumerate().filter(|(k, _)| keep(*k, 2)) {
                out.push(Lobe { c: c.c, rx: c.rx * 1.25, rz: c.rz * 1.2, sub: 0, amp: 0.12, k });
            }
            (None, out)
        }
        Lod::Coarse => {
            // a few crown lumps round a spindle, so the silhouette is lobed, not a barrel; the
            // foliage share thins the crown here too: fewer lumps, a slimmer spindle
            let (z0, z1, rr) = (s.clear, s.height, s.crown / 2.0);
            let lumps = ((4.0 * s.leaf).round() as usize).clamp(1, 4);
            let core = Lobe {
                c: v3(lk.lean.x, lk.lean.y, 0.5 * (z0 + z1)),
                rx: 0.42 * rr * prof(s.form, 0.5).max(0.35) * s.leaf.max(0.5),
                rz: 0.46 * (z1 - z0),
                sub: 0,
                amp: 0.0,
                k: 0,
            };
            for (k, c) in cl.iter().take(lumps).enumerate() {
                let mut l = Lobe { c: c.c, rx: c.rx * 1.2, rz: c.rz * 1.2, sub: 0, amp: 0.1, k };
                for step in 0..=20 {
                    l.c = core.c + (c.c - core.c) * (1.0 - step as f64 * 0.05);
                    if solid(&core).meets(&solid(&l)) {
                        break;
                    }
                }
                out.push(l);
            }
            (Some(core), out)
        }
    }
}

/// An ellipsoid of the solid part of a blob, for checking that the pieces of a tree touch.
#[derive(Clone, Copy, Debug)]
pub struct Ell {
    pub c: V3,
    pub rx: f64,
    pub rz: f64,
}

impl Ell {
    pub fn contains(&self, p: V3) -> bool {
        ((p.x - self.c.x) / self.rx).powi(2) + ((p.y - self.c.y) / self.rx).powi(2) + ((p.z - self.c.z) / self.rz).powi(2) <= 1.0
    }

    /// Some point of the line between the centres is inside both.
    pub fn meets(&self, o: &Ell) -> bool {
        (0..=32).any(|i| {
            let p = self.c + (o.c - self.c) * (i as f64 / 32.0);
            self.contains(p) && o.contains(p)
        })
    }
}

/// How the parts of a broadleaf tree join at a level: the core (coarse), the lobes drawn, and the
/// ends of the branches and twigs (medium and fine). Every lobe meets the core or holds a tip.
pub struct Anatomy {
    pub core: Option<Ell>,
    pub lobes: Vec<Ell>,
    pub tips: Vec<V3>,
}

pub fn anatomy(s: &Spec, lod: Lod) -> Anatomy {
    let lk = Look::of(s);
    let cl = clusters(s, &lk);
    let (core, lobes) = lobes_of(s, &lk, lod, &cl);
    let tips = if lod == Lod::Coarse { vec![] } else { wood_tips(s, &lk, lod).1 };
    Anatomy { core: core.as_ref().map(solid), lobes: lobes.iter().map(solid).collect(), tips }
}

fn foliage(s: &Spec, lk: &Look, lod: Lod) -> (Soup, Vec<(f64, f64, f64)>) {
    if s.leaf < 0.05 {
        return (Soup::default(), vec![]);
    }
    if s.form == Form::Conical {
        return conifer(s, lk, lod);
    }
    if s.form == Form::Palm {
        let top = Trunk {
            height: palm_top(s),
            r0: 0.0,
            lean: lk.lean * 1.8,
            taper: 0.0,
            wob: 0.0,
            ph: 0.0,
        }
        .at(1.0);
        return fronds(s, lk, lod, top);
    }
    let cl = clusters(s, lk);
    let discs: Vec<(f64, f64, f64)> = cl.iter().map(|c| (c.c.x, c.c.y, c.rx)).collect();
    let mut m = Soup::default();
    let (core, lobes) = lobes_of(s, lk, lod, &cl);
    for l in core.iter().chain(&lobes) {
        blob(&mut m, l.c, l.rx, l.rz, l.sub, lk.bump ^ l.k as u64, l.amp);
    }
    (m, discs)
}

/// The inner crown of a broadleaf tree: one dark mass at the heart of the foliage.
fn shade(s: &Spec, lk: &Look, lod: Lod) -> Soup {
    let mut m = Soup::default();
    if s.leaf < 0.05 || lod == Lod::Coarse || matches!(s.form, Form::Conical | Form::Palm) {
        return m;
    }
    let (z0, z1, rr) = (s.clear, s.height, s.crown / 2.0);
    let core = 0.45 * rr * prof(s.form, 0.5).max(0.35) * s.leaf.max(0.5);
    let sub = if lod == Lod::Fine { 1 } else { 0 };
    blob(&mut m, v3(lk.lean.x, lk.lean.y, 0.5 * (z0 + z1)), core, 0.4 * (z1 - z0), sub, lk.bump, 0.1);
    m
}

/// One organ of the tree at a level of detail.
pub fn grow(s: &Spec, organ: Organ, lod: Lod) -> Grown {
    let lk = Look::of(s);
    match organ {
        Organ::Wood => {
            let m = wood(s, &lk, lod);
            let volume = m.volume();
            Grown { pos: m.pos, tris: m.tris, volume, plan: circle_lines(s.trunk / 2.0, 12) }
        }
        Organ::Light => {
            let m = light(s, &lk, lod);
            Grown { pos: m.pos, tris: m.tris, volume: 0.0, plan: vec![] }
        }
        Organ::Streak => {
            let m = streak(s, &lk, lod);
            Grown { pos: m.pos, tris: m.tris, volume: 0.0, plan: vec![] }
        }
        Organ::Shade => {
            let m = shade(s, &lk, lod);
            Grown { pos: m.pos, tris: m.tris, volume: 0.0, plan: vec![] }
        }
        Organ::Foliage => {
            // the outline shows the crown also when the tree is bare
            let full = Spec { leaf: s.leaf.max(0.3), ..*s };
            let (_, discs) = foliage(&full, &lk, lod);
            let plan = symbol(s.form, &discs, s.trunk / 2.0, s.crown / 2.0, s.seed, lod);
            let (m, _) = foliage(s, &lk, lod);
            let volume = if m.pos.is_empty() {
                0.0
            } else if s.form == Form::Palm {
                m.volume()
            } else {
                env_volume(s)
            };
            Grown { pos: m.pos, tris: m.tris, volume, plan }
        }
    }
}
