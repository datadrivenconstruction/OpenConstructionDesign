// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cross-section properties of members for structural analysis.
//!
//! Section plane (y, z) as in `member::Shape`: y across (left of the member direction), z up,
//! origin at the bounding-box centre. SI units: lengths in m, areas m2, moduli m3, second
//! moments and the torsion constant m4. `iy` = integral of z^2 dA (bending about y, the strong
//! axis of an upright I), `iz` = integral of y^2 dA. Elastic and plastic moduli and shear areas
//! are about the centroidal axes parallel to y and z; principal values are given separately.
//!
//! Methods:
//! - A, I, W, Wpl: exact polygon integrals (closed forms for circle and pipe).
//! - Shear areas Avy, Avz: Jourawski energy form A_v = I^2 / integral(Q^2 / b), exact
//!   quadrature on each slab between vertex levels: 5/6 A for a rectangle, 9/10 A for a circle.
//!   Walls not parallel to the shear direction (pipes, sloped polygon edges) only carry the
//!   component along the cut, so a thin pipe gives 2/3 A instead of the Timoshenko 1/2 A.
//! - J and the shear centre of parametric shapes: exact series (rectangle), exact (circle,
//!   pipe), thin-walled closed forms (I, channel, tee, angle: sum of b t^3 / 3 over the plates
//!   with net lengths, no fillets; tube: Bredt with the wall centre line). Against the
//!   numerical solution these are within +4 % for open profiles and +2 % for a tube with
//!   t = b / 20.
//! - J and the shear centre of a general polygon with holes: Saint-Venant warping function
//!   solved with bilinear finite elements on a rectilinear grid whose lines pass through the
//!   vertex coordinates (holes need no special treatment). Accuracy at the default resolution:
//!   within 1 % when all edges are parallel to y or z, within 2 % for compact shapes with
//!   curved or sloped edges, up to 5 % for thin walls sloped to the axes (staircase boundary).

use std::f64::consts::PI;

use crate::math::{v2, V2};
use crate::member::Shape;
use crate::poly;

/// How the torsion constant was obtained.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TorsionMethod {
    #[default]
    Exact,
    ThinWalled,
    Grid,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SectionProps {
    pub area: f64,
    /// Centroidal second moments: about y (z^2), about z (y^2), product (y z).
    pub iy: f64,
    pub iz: f64,
    pub iyz: f64,
    /// Principal second moments, i1 >= i2, and the angle from +y to the axis of i1 (rad).
    pub i1: f64,
    pub i2: f64,
    pub alpha: f64,
    /// Saint-Venant torsion constant.
    pub j: f64,
    /// Elastic moduli to the extreme fibre.
    pub wy: f64,
    pub wz: f64,
    /// Plastic moduli about the equal-area axes parallel to y and z.
    pub wpl_y: f64,
    pub wpl_z: f64,
    /// Shear areas for shear along y and along z.
    pub avy: f64,
    pub avz: f64,
    /// Radii of gyration sqrt(I / A).
    pub ry: f64,
    pub rz: f64,
    /// Centroid and shear centre in the input coordinates (bbox centre for `Shape`).
    pub centroid: V2,
    pub shear_center: V2,
    pub torsion: TorsionMethod,
}

/// Default grid resolution: cells along the longer side, and cells across the mean wall
/// thickness 2 A / P.
pub const GRID_N: usize = 100;
pub const GRID_K: f64 = 10.0;

type Ring = Vec<V2>;

/// Properties of a parametric shape: closed forms where they exist, polygon method otherwise.
pub fn of_shape(s: &Shape) -> SectionProps {
    match *s {
        Shape::Circle { d } => round(d, 0.0),
        Shape::Pipe { d, t } => round(d, d - 2.0 * t),
        Shape::Poly(_) => {
            let (o, h) = s.rings();
            polygon(&o, &h.into_iter().collect::<Vec<_>>())
        }
        _ => {
            let (o, h) = s.rings();
            let rings = oriented(&o, &h.into_iter().collect::<Vec<_>>());
            let mut p = exact_part(&rings);
            let (j, sc, m) = closed_torsion(s);
            p.j = j;
            p.shear_center = sc;
            p.torsion = m;
            p
        }
    }
}

/// Properties of a polygon with holes (any orientation): exact integrals plus the grid
/// warping solution for J and the shear centre.
pub fn polygon(outer: &[V2], holes: &[Vec<V2>]) -> SectionProps {
    polygon_with(outer, holes, GRID_N, GRID_K)
}

/// `polygon` with an explicit grid resolution (see `GRID_N`, `GRID_K`).
pub fn polygon_with(outer: &[V2], holes: &[Vec<V2>], n: usize, k: f64) -> SectionProps {
    let rings = oriented(outer, holes);
    let mut p = exact_part(&rings);
    if p.area <= 0.0 {
        return p;
    }
    if let Some((j, off)) = warping(&rings, n, k) {
        p.j = j;
        p.shear_center = v2(p.centroid.x + off.x, p.centroid.y + off.y);
    }
    p.torsion = TorsionMethod::Grid;
    p
}

fn round(d: f64, di: f64) -> SectionProps {
    let area = PI * (d * d - di * di) / 4.0;
    let i = PI * (d.powi(4) - di.powi(4)) / 64.0;
    let av = if di <= 0.0 {
        0.9 * area
    } else {
        // Jourawski on a fine polygon, scaled to the exact area
        let n = 256;
        let c = |r: f64| -> Ring {
            (0..n)
                .map(|k| {
                    let a = 2.0 * PI * k as f64 / n as f64;
                    v2(r * a.cos(), r * a.sin())
                })
                .collect()
        };
        let mut hole = c(di / 2.0);
        hole.reverse();
        let rings = vec![c(d / 2.0), hole];
        shear_area_z(&rings) * area / moments(&rings)[0]
    };
    let w = if d > 0.0 { i / (d / 2.0) } else { 0.0 };
    let wpl = (d.powi(3) - di.powi(3)) / 6.0;
    let r = if area > 0.0 { (i / area).sqrt() } else { 0.0 };
    SectionProps {
        area,
        iy: i,
        iz: i,
        iyz: 0.0,
        i1: i,
        i2: i,
        alpha: 0.0,
        j: 2.0 * i,
        wy: w,
        wz: w,
        wpl_y: wpl,
        wpl_z: wpl,
        avy: av,
        avz: av,
        ry: r,
        rz: r,
        centroid: v2(0.0, 0.0),
        shear_center: v2(0.0, 0.0),
        torsion: TorsionMethod::Exact,
    }
}

/// Exact torsion constant of a solid rectangle (series, 20 terms).
pub fn rect_torsion(b: f64, h: f64) -> f64 {
    let (a, c) = if b >= h { (b, h) } else { (h, b) };
    if c <= 0.0 {
        return 0.0;
    }
    let s: f64 = (0..20)
        .map(|k| {
            let n = (2 * k + 1) as f64;
            (n * PI * a / (2.0 * c)).tanh() / n.powi(5)
        })
        .sum();
    a * c.powi(3) / 3.0 * (1.0 - 192.0 / PI.powi(5) * c / a * s)
}

/// (J, shear centre, method) for the polygonal parametric shapes.
fn closed_torsion(s: &Shape) -> (f64, V2, TorsionMethod) {
    let open = |plates: &[(f64, f64)]| -> f64 {
        plates.iter().map(|(b, t)| b * t.powi(3) / 3.0).sum()
    };
    match *s {
        Shape::Rect { b, h } => (rect_torsion(b, h), v2(0.0, 0.0), TorsionMethod::Exact),
        Shape::Tube { h, b, t } => {
            let (bm, hm) = (b - t, h - t);
            let j = 2.0 * t * bm * bm * hm * hm / (bm + hm);
            (j, v2(0.0, 0.0), TorsionMethod::ThinWalled)
        }
        Shape::I { h, b, tw, tf } => {
            let j = open(&[(b, tf), (b, tf), (h - 2.0 * tf, tw)]);
            (j, v2(0.0, 0.0), TorsionMethod::ThinWalled)
        }
        Shape::Channel { h, b, tw, tf } => {
            let j = open(&[(b, tf), (b, tf), (h - 2.0 * tf, tw)]);
            // thin-walled: e from the web centre line, away from the flanges
            let (bf, hw) = (b - tw / 2.0, h - tf);
            let e = 3.0 * bf * bf * tf / (6.0 * bf * tf + hw * tw);
            (j, v2(-b / 2.0 + tw / 2.0 - e, 0.0), TorsionMethod::ThinWalled)
        }
        Shape::Tee { h, b, tw, tf } => {
            let j = open(&[(b, tf), (h - tf, tw)]);
            (j, v2(0.0, h / 2.0 - tf / 2.0), TorsionMethod::ThinWalled)
        }
        Shape::Angle { h, b, t } => {
            let j = open(&[(b, t), (h - t, t)]);
            (j, v2(-b / 2.0 + t / 2.0, -h / 2.0 + t / 2.0), TorsionMethod::ThinWalled)
        }
        Shape::Circle { .. } | Shape::Pipe { .. } | Shape::Poly(_) => {
            (0.0, v2(0.0, 0.0), TorsionMethod::Exact)
        }
    }
}

/// Outer ring counter-clockwise, holes clockwise.
fn oriented(outer: &[V2], holes: &[Vec<V2>]) -> Vec<Ring> {
    let mut o = outer.to_vec();
    if poly::area(&o) < 0.0 {
        o.reverse();
    }
    let mut v = vec![o];
    for h in holes {
        let mut h = h.clone();
        if poly::area(&h) > 0.0 {
            h.reverse();
        }
        v.push(h);
    }
    v
}

/// Signed (A, int y, int z, int z^2, int y^2, int y z) of one ring.
fn ring_moments(r: &[V2]) -> [f64; 6] {
    let n = r.len();
    let mut m = [0.0; 6];
    if n < 3 {
        return m;
    }
    for i in 0..n {
        let (p, q) = (r[i], r[(i + 1) % n]);
        let c = p.x * q.y - q.x * p.y;
        m[0] += c;
        m[1] += (p.x + q.x) * c;
        m[2] += (p.y + q.y) * c;
        m[3] += (p.y * p.y + p.y * q.y + q.y * q.y) * c;
        m[4] += (p.x * p.x + p.x * q.x + q.x * q.x) * c;
        m[5] += (p.x * q.y + 2.0 * p.x * p.y + 2.0 * q.x * q.y + q.x * p.y) * c;
    }
    [m[0] / 2.0, m[1] / 6.0, m[2] / 6.0, m[3] / 12.0, m[4] / 12.0, m[5] / 24.0]
}

fn moments(rings: &[Ring]) -> [f64; 6] {
    let mut s = [0.0; 6];
    for r in rings {
        let m = ring_moments(r);
        for (s, m) in s.iter_mut().zip(m) {
            *s += m;
        }
    }
    s
}

/// Moments of the part with z >= `z`.
fn above(rings: &[Ring], z: f64) -> [f64; 6] {
    let n = v2(0.0, -1.0);
    let parts: Vec<Ring> = rings.iter().map(|r| poly::clip_half(r, n, -z)).collect();
    moments(&parts)
}

/// Total material width of the horizontal line at `z` (outer CCW, holes CW).
fn width(rings: &[Ring], z: f64) -> f64 {
    let mut w = 0.0;
    for r in rings {
        for i in 0..r.len() {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            if (p.y <= z) != (q.y <= z) {
                let x = p.x + (z - p.y) * (q.x - p.x) / (q.y - p.y);
                w += if q.y > p.y { x } else { -x };
            }
        }
    }
    w
}

/// Quarter turn (y, z) -> (-z, y): keeps orientation, maps the y direction onto z.
fn turn(rings: &[Ring]) -> Vec<Ring> {
    rings
        .iter()
        .map(|r| r.iter().map(|p| v2(-p.y, p.x)).collect())
        .collect()
}

const GL8: [(f64, f64); 8] = [
    (-0.960_289_856_497_536_3, 0.101_228_536_290_376_3),
    (-0.796_666_477_413_626_7, 0.222_381_034_453_374_5),
    (-0.525_532_409_916_329_0, 0.313_706_645_877_887_3),
    (-0.183_434_642_495_649_8, 0.362_683_783_378_362_0),
    (0.183_434_642_495_649_8, 0.362_683_783_378_362_0),
    (0.525_532_409_916_329_0, 0.313_706_645_877_887_3),
    (0.796_666_477_413_626_7, 0.222_381_034_453_374_5),
    (0.960_289_856_497_536_3, 0.101_228_536_290_376_3),
];

/// Jourawski shear area for shear along z.
fn shear_area_z(rings: &[Ring]) -> f64 {
    let m = moments(rings);
    let a = m[0];
    if a <= 0.0 {
        return 0.0;
    }
    let cz = m[2] / a;
    let iy = m[3] - a * cz * cz;
    let mut zs: Vec<f64> = rings.iter().flatten().map(|p| p.y).collect();
    zs.sort_by(f64::total_cmp);
    zs.dedup();
    let span = zs.last().copied().unwrap_or(0.0) - zs.first().copied().unwrap_or(0.0);
    let mut s = 0.0;
    for w in zs.windows(2) {
        let half = (w[1] - w[0]) / 2.0;
        if half <= 1e-12 * span {
            continue;
        }
        let mid = (w[0] + w[1]) / 2.0;
        for (x, wt) in GL8 {
            let z = mid + x * half;
            let b = width(rings, z);
            if b <= 0.0 {
                continue;
            }
            let u = above(rings, z);
            let q = u[2] - cz * u[0];
            s += wt * half * q * q / b;
        }
    }
    if s > 0.0 {
        iy * iy / s
    } else {
        0.0
    }
}

/// Plastic modulus for bending about the equal-area axis parallel to y.
fn plastic_z(rings: &[Ring]) -> f64 {
    let m = moments(rings);
    let a = m[0];
    let (mut lo, mut hi) = rings
        .iter()
        .flatten()
        .fold((f64::MAX, f64::MIN), |(l, h), p| (l.min(p.y), h.max(p.y)));
    if a <= 0.0 || lo >= hi {
        return 0.0;
    }
    for _ in 0..100 {
        let z = 0.5 * (lo + hi);
        if above(rings, z)[0] > a / 2.0 {
            lo = z;
        } else {
            hi = z;
        }
    }
    let z = 0.5 * (lo + hi);
    let u = above(rings, z);
    (u[2] - z * u[0]) - ((m[2] - u[2]) - z * (a - u[0]))
}

/// Everything except J and the shear centre (set to 0 and the centroid).
fn exact_part(rings: &[Ring]) -> SectionProps {
    let m = moments(rings);
    let a = m[0];
    if a <= 1e-300 {
        return SectionProps::default();
    }
    let (cy, cz) = (m[1] / a, m[2] / a);
    let iy = m[3] - a * cz * cz;
    let iz = m[4] - a * cy * cy;
    let iyz = m[5] - a * cy * cz;
    let mean = (iy + iz) / 2.0;
    let rad = ((iy - iz) / 2.0).hypot(iyz);
    let alpha = if rad <= 1e-12 * mean.abs() {
        0.0
    } else {
        0.5 * (-2.0 * iyz).atan2(iy - iz)
    };
    let (dy, dz) = rings[0].iter().fold((0.0f64, 0.0f64), |(y, z), p| {
        (y.max((p.x - cy).abs()), z.max((p.y - cz).abs()))
    });
    let turned = turn(rings);
    SectionProps {
        area: a,
        iy,
        iz,
        iyz,
        i1: mean + rad,
        i2: mean - rad,
        alpha,
        j: 0.0,
        wy: if dz > 0.0 { iy / dz } else { 0.0 },
        wz: if dy > 0.0 { iz / dy } else { 0.0 },
        wpl_y: plastic_z(rings),
        wpl_z: plastic_z(&turned),
        avy: shear_area_z(&turned),
        avz: shear_area_z(rings),
        ry: (iy.max(0.0) / a).sqrt(),
        rz: (iz.max(0.0) / a).sqrt(),
        centroid: v2(cy, cz),
        shear_center: v2(cy, cz),
        torsion: TorsionMethod::Grid,
    }
}

/// Grid lines from `lo` to `hi` through the given coordinates, no cell wider than `h`.
fn grid_lines(vals: &[f64], lo: f64, hi: f64, h: f64) -> Vec<f64> {
    let mut c: Vec<f64> = vals.iter().copied().filter(|v| *v > lo && *v < hi).collect();
    c.sort_by(f64::total_cmp);
    let mut keep = vec![lo];
    for v in c {
        if v - keep[keep.len() - 1] >= h / 2.0 && hi - v >= h / 2.0 {
            keep.push(v);
        }
    }
    keep.push(hi);
    let mut out = vec![lo];
    for w in keep.windows(2) {
        let m = ((w[1] - w[0]) / h - 1e-9).ceil().max(1.0) as usize;
        for k in 1..=m {
            out.push(w[0] + (w[1] - w[0]) * k as f64 / m as f64);
        }
    }
    out
}

/// Node order of a cell: (0,0), (1,0), (1,1), (0,1) as signs in [-1, 1].
const SG: [(f64, f64); 4] = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
const KX: [[f64; 4]; 4] = [
    [2.0, -2.0, -1.0, 1.0],
    [-2.0, 2.0, 1.0, -1.0],
    [-1.0, 1.0, 2.0, -2.0],
    [1.0, -1.0, -2.0, 2.0],
];
const KZ: [[f64; 4]; 4] = [
    [2.0, 1.0, -1.0, -2.0],
    [1.0, 2.0, -2.0, -1.0],
    [-1.0, -2.0, 2.0, 1.0],
    [-2.0, -1.0, 1.0, 2.0],
];

struct Cell {
    dofs: [usize; 4],
    hy: f64,
    hz: f64,
    yc: f64,
    zc: f64,
}

/// Saint-Venant warping on a rectilinear grid: (J, shear centre minus centroid).
/// `n` cells along the longer side, at least `k` cells across the mean wall 2 A / P.
pub fn warping_props(outer: &[V2], holes: &[Vec<V2>], n: usize, k: f64) -> Option<(f64, V2)> {
    warping(&oriented(outer, holes), n, k)
}

fn warping(rings: &[Ring], n: usize, k: f64) -> Option<(f64, V2)> {
    let a = moments(rings)[0];
    let per: f64 = rings.iter().map(|r| poly::perimeter(r)).sum();
    let (y0, y1, z0, z1) = rings[0].iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(a, b, c, d), p| (a.min(p.x), b.max(p.x), c.min(p.y), d.max(p.y)),
    );
    if a <= 0.0 || per <= 0.0 || y1 <= y0 || z1 <= z0 {
        return None;
    }
    let h = ((y1 - y0).max(z1 - z0) / n.max(1) as f64).min(2.0 * a / per / k.max(1.0));
    let ys: Vec<f64> = rings.iter().flatten().map(|p| p.x).collect();
    let zs: Vec<f64> = rings.iter().flatten().map(|p| p.y).collect();
    let gy = grid_lines(&ys, y0, y1, h);
    let gz = grid_lines(&zs, z0, z1, h);
    let nx = gy.len() - 1;
    let ycen: Vec<f64> = gy.windows(2).map(|w| 0.5 * (w[0] + w[1])).collect();
    // material cells by the even-odd rule on each row centre
    let mut ij: Vec<(usize, usize)> = vec![];
    for j in 0..gz.len() - 1 {
        let zc = 0.5 * (gz[j] + gz[j + 1]);
        let mut xs = vec![];
        for r in rings {
            for i in 0..r.len() {
                let (p, q) = (r[i], r[(i + 1) % r.len()]);
                if (p.y <= zc) != (q.y <= zc) {
                    xs.push(p.x + (zc - p.y) * (q.x - p.x) / (q.y - p.y));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for pr in xs.chunks_exact(2) {
            let i0 = ycen.partition_point(|y| *y <= pr[0]);
            let i1 = ycen.partition_point(|y| *y < pr[1]);
            ij.extend((i0..i1).map(|i| (i, j)));
        }
    }
    if ij.is_empty() {
        return None;
    }
    let mut map = vec![usize::MAX; (nx + 1) * gz.len()];
    let mut ndof = 0;
    let mut cells = Vec::with_capacity(ij.len());
    for &(i, j) in &ij {
        let mut dofs = [0; 4];
        for (c, (s, t)) in SG.iter().enumerate() {
            let id = (j + (*t > 0.0) as usize) * (nx + 1) + i + (*s > 0.0) as usize;
            if map[id] == usize::MAX {
                map[id] = ndof;
                ndof += 1;
            }
            dofs[c] = map[id];
        }
        cells.push(Cell {
            dofs,
            hy: gy[i + 1] - gy[i],
            hz: gz[j + 1] - gz[j],
            yc: ycen[i],
            zc: 0.5 * (gz[j] + gz[j + 1]),
        });
    }
    // grid centroid and second moments (the grid domain is used consistently)
    let ag: f64 = cells.iter().map(|c| c.hy * c.hz).sum();
    let cy = cells.iter().map(|c| c.hy * c.hz * c.yc).sum::<f64>() / ag;
    let cz = cells.iter().map(|c| c.hy * c.hz * c.zc).sum::<f64>() / ag;
    let (mut iy, mut iz, mut iyz) = (0.0, 0.0, 0.0);
    for c in &cells {
        let ar = c.hy * c.hz;
        let (dy, dz) = (c.yc - cy, c.zc - cz);
        iy += ar * (dz * dz + c.hz * c.hz / 12.0);
        iz += ar * (dy * dy + c.hy * c.hy / 12.0);
        iyz += ar * dy * dz;
    }
    let g = 1.0 / 3f64.sqrt();
    let gp = [(-g, -g), (g, -g), (g, g), (-g, g)];
    let mut f = vec![0.0; ndof];
    let mut diag = vec![0.0; ndof];
    for c in &cells {
        let ky = c.hz / (6.0 * c.hy);
        let kz = c.hy / (6.0 * c.hz);
        for (p, (s, t)) in SG.iter().enumerate() {
            diag[c.dofs[p]] += ky * KX[p][p] + kz * KZ[p][p];
            for (a, b) in gp {
                let wt = c.hy * c.hz / 4.0;
                let dny = s * (1.0 + t * b) / (2.0 * c.hy);
                let dnz = t * (1.0 + s * a) / (2.0 * c.hz);
                let y = c.yc - cy + a * c.hy / 2.0;
                let z = c.zc - cz + b * c.hz / 2.0;
                f[c.dofs[p]] += (z * dny - y * dnz) * wt;
            }
        }
    }
    let apply = |x: &[f64], out: &mut [f64]| {
        out.iter_mut().for_each(|v| *v = 0.0);
        for c in &cells {
            let ky = c.hz / (6.0 * c.hy);
            let kz = c.hy / (6.0 * c.hz);
            for (p, (kxp, kzp)) in KX.iter().zip(&KZ).enumerate() {
                let mut s = 0.0;
                for ((a, b), d) in kxp.iter().zip(kzp).zip(&c.dofs) {
                    s += (ky * a + kz * b) * x[*d];
                }
                out[c.dofs[p]] += s;
            }
        }
        // dof 0 pinned: removes the constant mode
        out[0] = 0.0;
    };
    // Jacobi-preconditioned conjugate gradients
    let mut x = vec![0.0; ndof];
    let mut r = f.clone();
    r[0] = 0.0;
    let norm0 = r.iter().map(|v| v * v).sum::<f64>().sqrt();
    if norm0 > 0.0 {
        let mut z: Vec<f64> = r.iter().zip(&diag).map(|(r, d)| r / d).collect();
        let mut p = z.clone();
        let mut rz: f64 = r.iter().zip(&z).map(|(a, b)| a * b).sum();
        let mut q = vec![0.0; ndof];
        for _ in 0..(20 * ndof).max(1000) {
            apply(&p[..], &mut q[..]);
            let pq: f64 = p.iter().zip(&q).map(|(a, b)| a * b).sum();
            if pq <= 0.0 {
                break;
            }
            let al = rz / pq;
            for ((xi, ri), (pi, qi)) in x.iter_mut().zip(r.iter_mut()).zip(p.iter().zip(&q)) {
                *xi += al * pi;
                *ri -= al * qi;
            }
            if r.iter().map(|v| v * v).sum::<f64>().sqrt() <= 1e-11 * norm0 {
                break;
            }
            for ((zi, ri), di) in z.iter_mut().zip(&r).zip(&diag) {
                *zi = ri / di;
            }
            let rz2: f64 = r.iter().zip(&z).map(|(a, b)| a * b).sum();
            let be = rz2 / rz;
            rz = rz2;
            for (pi, zi) in p.iter_mut().zip(&z) {
                *pi = zi + be * *pi;
            }
        }
    }
    let j = iy + iz - x.iter().zip(&f).map(|(a, b)| a * b).sum::<f64>();
    // sectorial products of the warping about the centroid
    let (mut sy, mut sz) = (0.0, 0.0);
    for c in &cells {
        for (a, b) in gp {
            let mut o = 0.0;
            for (p, (s, t)) in SG.iter().enumerate() {
                o += (1.0 + s * a) * (1.0 + t * b) / 4.0 * x[c.dofs[p]];
            }
            let wt = c.hy * c.hz / 4.0;
            sy += o * (c.yc - cy + a * c.hy / 2.0) * wt;
            sz += o * (c.zc - cz + b * c.hz / 2.0) * wt;
        }
    }
    // pole P with zero sectorial products: omega_P = omega_C - zP y + yP z
    let det = iy * iz - iyz * iyz;
    if det <= 0.0 {
        return Some((j, v2(0.0, 0.0)));
    }
    let yp = (sy * iyz - iz * sz) / det;
    let zp = (iy * sy - iyz * sz) / det;
    Some((j, v2(yp, zp)))
}

#[cfg(test)]
#[path = "section_props_tests.rs"]
mod tests;
