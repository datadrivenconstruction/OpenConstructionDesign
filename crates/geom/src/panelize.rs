// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Panelization of a freeform surface (GEOM_PRO_PLAN W7). A structured node grid on the
//! surface: columns and rows at equal parameter steps, at equal arc length along iso-curves,
//! or rows where the surface crosses given heights (floor and fin lines). Panels are quads,
//! triangles or diamonds. Per panel: planarity (distance between the diagonals), warp angle
//! (between the normals of its two triangles) and kind (flat, cold-bent, hot-bent by the
//! limits). Repetition clusters by sorted edge and diagonal lengths within a tolerance.
//! `planarize` moves the nodes towards planar quads by alternating projection: every quad is
//! projected on its plane, each node takes the mean of its proposals and is pulled back to the
//! surface by `stick` (boundary nodes stay on their boundary). Arc lengths are sampled
//! ([`ARC_SAMPLES`] chords per iso-curve).

use std::fmt;

use crate::math::V3;
use crate::nurbs::Surface;

/// Chords per iso-curve when lengths are measured.
pub const ARC_SAMPLES: usize = 512;
/// Most panels along one direction.
pub const MAX_DIV: usize = 500;
/// Over-relaxation of the planarisation step (1 = plain mean of the quad projections).
/// The mean step is Jacobi on the plane-distance energy with the node's quad count as the
/// diagonal. Each quad's centring projection has norm 1, so the eigenvalues of the
/// preconditioned Hessian lie in (0, 1], and the checkerboard twist reaches 1. Stability
/// therefore needs w < 2. The optimal w is 2 / (1 + l_min). The smoothest twist mode across
/// `n` panels gives l_min >= sin^2(pi / 2n) (twist is a product of two such factors). Using
/// that larger value keeps w below the optimum, so it stays stable (6 panels: 1.87).
fn relax(cols: usize, rows: usize) -> f64 {
    let n = cols.max(rows).max(1) as f64;
    let l_min = (std::f64::consts::PI / (2.0 * n)).sin().powi(2);
    2.0 / (1.0 + l_min)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spacing {
    /// `n` panels at equal parameter steps.
    Params(usize),
    /// `n` panels of equal arc length.
    Equal(usize),
    /// Panels of about this arc length (m); the count is rounded up on the middle iso-curve.
    Width(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Rows {
    Spacing(Spacing),
    /// Row lines where the surface crosses these heights (rising along v).
    Levels(Vec<f64>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    Quad,
    /// Each grid cell split along its (i, j)-(i+1, j+1) diagonal.
    Triangle,
    /// Quads around every node with i + j odd; at the border they keep that node.
    Diamond,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelLimits {
    /// A panel whose diagonals pass within this distance is flat, m.
    pub flat: f64,
    /// Up to this diagonal distance a panel can be cold-bent; beyond it is hot-bent, m.
    pub cold: f64,
    /// Panels are one type when sorted edges and diagonals match within this, m.
    pub cluster: f64,
}

impl Default for PanelLimits {
    fn default() -> Self {
        PanelLimits { flat: 1e-3, cold: 0.02, cluster: 2e-3 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Flat,
    ColdBent,
    HotBent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    /// Node indices, counter-clockwise in (u, v): 3 or 4.
    pub nodes: Vec<usize>,
    /// Area of its triangles, m2.
    pub area: f64,
    /// Distance between the diagonals (0 for a triangle), m.
    pub planarity: f64,
    /// Angle between the normals of its two triangles, rad.
    pub warp: f64,
    pub kind: PanelKind,
    /// Type (index into `Panels::clusters`).
    pub cluster: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    pub count: usize,
    /// Panel the type was taken from.
    pub first: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Panels {
    /// Grid nodes, index `j * (cols + 1) + i` (i along u, j along v).
    pub nodes: Vec<V3>,
    /// Surface parameters of each node.
    pub uv: Vec<[f64; 2]>,
    pub cols: usize,
    pub rows: usize,
    pub pattern: Pattern,
    pub limits: PanelLimits,
    pub panels: Vec<Panel>,
    pub clusters: Vec<Cluster>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PanelError {
    /// A direction needs 1 ..= [`MAX_DIV`] panels.
    Count(char),
    Input(&'static str),
    /// Column `col` of the grid does not cross height `level`.
    Level { level: f64, col: usize },
}

impl fmt::Display for PanelError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PanelError::Count(d) => write!(f, "panels along {d} must number 1 to {MAX_DIV}"),
            PanelError::Input(s) => f.write_str(s),
            PanelError::Level { level, col } => {
                write!(f, "grid column {col} does not cross the height {level:.3} m")
            }
        }
    }
}

impl std::error::Error for PanelError {}

/// Cumulative chord lengths of `f` sampled on [a, b].
fn arc_table(f: &dyn Fn(f64) -> V3, a: f64, b: f64) -> (Vec<f64>, Vec<f64>) {
    let ts: Vec<f64> = (0..=ARC_SAMPLES).map(|k| a + (b - a) * k as f64 / ARC_SAMPLES as f64).collect();
    let mut cum = vec![0.0];
    let mut prev = f(a);
    for &t in &ts[1..] {
        let p = f(t);
        cum.push(cum[cum.len() - 1] + (p - prev).len());
        prev = p;
    }
    (ts, cum)
}

/// Panel count of a spacing on the curve `f` over [a, b].
fn count(sp: Spacing, f: &dyn Fn(f64) -> V3, a: f64, b: f64, dir: char) -> Result<usize, PanelError> {
    let n = match sp {
        Spacing::Params(n) | Spacing::Equal(n) => n,
        Spacing::Width(w) => {
            if !(w > 0.0 && w.is_finite()) {
                return Err(PanelError::Input("panel width must be > 0"));
            }
            let (_, cum) = arc_table(f, a, b);
            (cum[cum.len() - 1] / w - 1e-9).ceil().max(1.0) as usize
        }
    };
    if n == 0 || n > MAX_DIV {
        return Err(PanelError::Count(dir));
    }
    Ok(n)
}

/// `n + 1` parameters on [a, b] splitting `f` into `n` panels.
fn divide(sp: Spacing, n: usize, f: &dyn Fn(f64) -> V3, a: f64, b: f64) -> Vec<f64> {
    if let Spacing::Params(_) = sp {
        return (0..=n).map(|k| a + (b - a) * k as f64 / n as f64).collect();
    }
    let (ts, cum) = arc_table(f, a, b);
    let total = cum[cum.len() - 1];
    let mut out = vec![a];
    let mut s = 1;
    for k in 1..n {
        let target = total * k as f64 / n as f64;
        while s < cum.len() - 1 && cum[s] < target {
            s += 1;
        }
        let (c0, c1) = (cum[s - 1], cum[s]);
        let w = if c1 > c0 { (target - c0) / (c1 - c0) } else { 0.0 };
        out.push(ts[s - 1] + (ts[s] - ts[s - 1]) * w);
    }
    out.push(b);
    out
}

/// Parameter v in [v0, v1] where z of `S(u, v)` equals `level` (first crossing).
fn at_level(s: &Surface, u: f64, level: f64) -> Option<f64> {
    let (v0, v1) = s.domain_v();
    let z = |v: f64| s.point(u, v).z - level;
    let k = 128;
    let mut a = v0;
    let mut za = z(a);
    if za.abs() < 1e-12 {
        return Some(a);
    }
    for i in 1..=k {
        let b = v0 + (v1 - v0) * i as f64 / k as f64;
        let zb = z(b);
        if zb.abs() < 1e-12 {
            return Some(b);
        }
        if za * zb < 0.0 {
            let (mut lo, mut hi) = (a, b);
            for _ in 0..80 {
                let m = 0.5 * (lo + hi);
                if z(m) * za > 0.0 {
                    lo = m;
                } else {
                    hi = m;
                }
            }
            return Some(0.5 * (lo + hi));
        }
        (a, za) = (b, zb);
    }
    None
}

/// Node grid and panels of `s`.
pub fn panelize(
    s: &Surface,
    cols: Spacing,
    rows: &Rows,
    pattern: Pattern,
    limits: PanelLimits,
) -> Result<Panels, PanelError> {
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    let (um, vm) = (0.5 * (u0 + u1), 0.5 * (v0 + v1));
    let along_u = |v: f64| move |u: f64| s.point(u, v);
    let mid_u = along_u(vm);
    let nc = count(cols, &mid_u, u0, u1, 'u')?;
    let mut uv: Vec<[f64; 2]> = vec![];
    let nr = match rows {
        Rows::Spacing(rs) => {
            let mid_v = |v: f64| s.point(um, v);
            let nr = count(*rs, &mid_v, v0, v1, 'v')?;
            let vs = divide(*rs, nr, &mid_v, v0, v1);
            for &v in &vs {
                let f = along_u(v);
                for u in divide(cols, nc, &f, u0, u1) {
                    uv.push([u, v]);
                }
            }
            nr
        }
        Rows::Levels(ls) => {
            if ls.len() < 2 || ls.len() > MAX_DIV + 1 {
                return Err(PanelError::Count('v'));
            }
            if !ls.windows(2).all(|w| w[1] > w[0]) || !ls.iter().all(|l| l.is_finite()) {
                return Err(PanelError::Input("levels must be finite and rising"));
            }
            let us = divide(cols, nc, &mid_u, u0, u1);
            let mut grid = vec![[0.0; 2]; us.len() * ls.len()];
            for (i, &u) in us.iter().enumerate() {
                for (j, &l) in ls.iter().enumerate() {
                    let v = at_level(s, u, l).ok_or(PanelError::Level { level: l, col: i })?;
                    grid[j * us.len() + i] = [u, v];
                }
            }
            uv = grid;
            ls.len() - 1
        }
    };
    let nodes = uv.iter().map(|p| s.point(p[0], p[1])).collect();
    let mut out = Panels {
        nodes,
        uv,
        cols: nc,
        rows: nr,
        pattern,
        limits,
        panels: vec![],
        clusters: vec![],
    };
    out.panels = topology(nc, nr, pattern)
        .into_iter()
        .map(|nodes| Panel { nodes, area: 0.0, planarity: 0.0, warp: 0.0, kind: PanelKind::Flat, cluster: 0 })
        .collect();
    out.measure();
    Ok(out)
}

/// Node lists of the panels of a `cols` x `rows` grid.
fn topology(cols: usize, rows: usize, pattern: Pattern) -> Vec<Vec<usize>> {
    let id = |i: usize, j: usize| j * (cols + 1) + i;
    let mut out = vec![];
    match pattern {
        Pattern::Quad | Pattern::Triangle => {
            for j in 0..rows {
                for i in 0..cols {
                    let (a, b, c, d) = (id(i, j), id(i + 1, j), id(i + 1, j + 1), id(i, j + 1));
                    if pattern == Pattern::Quad {
                        out.push(vec![a, b, c, d]);
                    } else {
                        out.push(vec![a, b, c]);
                        out.push(vec![a, c, d]);
                    }
                }
            }
        }
        Pattern::Diamond => {
            for j in 0..=rows {
                for i in 0..=cols {
                    if (i + j) % 2 == 0 {
                        continue;
                    }
                    let (ii, jj) = (i as isize, j as isize);
                    let inside = |a: isize, b: isize| a >= 0 && b >= 0 && a <= cols as isize && b <= rows as isize;
                    let mut ring: Vec<usize> = vec![];
                    for (a, b) in [(ii, jj - 1), (ii + 1, jj), (ii, jj + 1), (ii - 1, jj)] {
                        let n = if inside(a, b) { id(a as usize, b as usize) } else { id(i, j) };
                        if ring.last() != Some(&n) {
                            ring.push(n);
                        }
                    }
                    while ring.len() > 1 && ring[0] == ring[ring.len() - 1] {
                        ring.pop();
                    }
                    if ring.len() >= 3 {
                        out.push(ring);
                    }
                }
            }
        }
    }
    out
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    v
}

impl Panels {
    /// Recomputes area, planarity, warp, kind and the clusters from the node positions.
    pub fn measure(&mut self) {
        let lim = self.limits;
        let mut sigs: Vec<Vec<f64>> = vec![];
        for p in &mut self.panels {
            let q: Vec<V3> = p.nodes.iter().map(|&k| self.nodes[k]).collect();
            if q.len() == 3 {
                p.area = (q[1] - q[0]).cross(q[2] - q[0]).len() / 2.0;
                (p.planarity, p.warp, p.kind) = (0.0, 0.0, PanelKind::Flat);
                sigs.push(sorted(vec![(q[1] - q[0]).len(), (q[2] - q[1]).len(), (q[0] - q[2]).len()]));
                continue;
            }
            let n1 = (q[1] - q[0]).cross(q[2] - q[0]);
            let n2 = (q[2] - q[0]).cross(q[3] - q[0]);
            p.area = (n1.len() + n2.len()) / 2.0;
            p.warp = n1.cross(n2).len().atan2(n1.dot(n2));
            let (d1, d2) = (q[2] - q[0], q[3] - q[1]);
            let c = d1.cross(d2);
            p.planarity = if c.len() > 1e-12 {
                (q[1] - q[0]).dot(c).abs() / c.len()
            } else {
                0.0
            };
            p.kind = if p.planarity <= lim.flat {
                PanelKind::Flat
            } else if p.planarity <= lim.cold {
                PanelKind::ColdBent
            } else {
                PanelKind::HotBent
            };
            let edges = (0..4).map(|k| (q[(k + 1) % 4] - q[k]).len()).collect();
            let mut sig = sorted(edges);
            sig.extend(sorted(vec![d1.len(), d2.len()]));
            sigs.push(sig);
        }
        self.clusters.clear();
        let mut reps: Vec<usize> = vec![];
        for (k, sig) in sigs.iter().enumerate() {
            let found = reps.iter().position(|&r| {
                sigs[r].len() == sig.len()
                    && sigs[r].iter().zip(sig).all(|(a, b)| (a - b).abs() <= lim.cluster)
            });
            let c = match found {
                Some(c) => c,
                None => {
                    reps.push(k);
                    self.clusters.push(Cluster { count: 0, first: k });
                    reps.len() - 1
                }
            };
            self.clusters[c].count += 1;
            self.panels[k].cluster = c;
        }
    }

    /// Largest diagonal distance over the quads, m.
    pub fn max_planarity(&self) -> f64 {
        self.panels.iter().map(|p| p.planarity).fold(0.0, f64::max)
    }

    /// Largest distance of a node from the surface point at its parameters, m.
    pub fn off_surface(&self, s: &Surface) -> f64 {
        self.nodes
            .iter()
            .zip(&self.uv)
            .map(|(p, t)| (*p - s.point(t[0], t[1])).len())
            .fold(0.0, f64::max)
    }

    /// Planar-quad optimisation: `iters` rounds of projection of each quad on its plane, mean
    /// of the proposals per node, pull back to the surface by `stick` (0..1). Returns the
    /// largest planarity before and after.
    pub fn planarize(&mut self, s: &Surface, iters: usize, stick: f64) -> (f64, f64) {
        let before = self.max_planarity();
        let stick = stick.clamp(0.0, 1.0);
        let (u0, u1) = s.domain_u();
        let (v0, v1) = s.domain_v();
        let quads: Vec<[usize; 4]> = self
            .panels
            .iter()
            .filter(|p| p.nodes.len() == 4)
            .map(|p| [p.nodes[0], p.nodes[1], p.nodes[2], p.nodes[3]])
            .collect();
        let w = self.cols + 1;
        let omega = relax(self.cols, self.rows);
        for _ in 0..iters {
            let mut sum = vec![V3::default(); self.nodes.len()];
            let mut cnt = vec![0usize; self.nodes.len()];
            for q in &quads {
                let p: Vec<V3> = q.iter().map(|&k| self.nodes[k]).collect();
                let n = (p[2] - p[0]).cross(p[3] - p[1]);
                if n.len() <= 1e-15 {
                    continue;
                }
                let n = n * (1.0 / n.len());
                let c = (p[0] + p[1] + p[2] + p[3]) * 0.25;
                for (k, &id) in q.iter().enumerate() {
                    sum[id] = sum[id] + p[k] - n * (p[k] - c).dot(n);
                    cnt[id] += 1;
                }
            }
            for id in 0..self.nodes.len() {
                if cnt[id] == 0 {
                    continue;
                }
                let mean = sum[id] * (1.0 / cnt[id] as f64);
                let (i, j) = (id % w, id / w);
                let fix_u = i == 0 || i == self.cols;
                let fix_v = j == 0 || j == self.rows;
                let t = foot(s, mean, self.uv[id], fix_u, fix_v, (u0, u1), (v0, v1));
                let on = s.point(t[0], t[1]);
                self.uv[id] = t;
                // plain step: mean of the projections pulled toward the surface by `stick`. The
                // over-relaxation scales the whole step, so the fixed point (and the distance
                // kept from the surface) is that of the plain iteration; the pull is a convex
                // blend with a projection, so the (0, 1] spectrum and w < 2 still hold
                let target = mean + (on - mean) * stick;
                self.nodes[id] = self.nodes[id] + (target - self.nodes[id]) * omega;
            }
        }
        self.measure();
        (before, self.max_planarity())
    }
}

/// Foot of `p` on the surface by Newton from `t`, with u and/or v held when asked.
fn foot(s: &Surface, p: V3, t: [f64; 2], fix_u: bool, fix_v: bool, du: (f64, f64), dv: (f64, f64)) -> [f64; 2] {
    let (mut u, mut v) = (t[0], t[1]);
    for _ in 0..8 {
        let d = s.derivs(u, v, 2);
        let f = d[0][0] - p;
        let (su, sv) = (d[1][0], d[0][1]);
        let g = [f.dot(su), f.dot(sv)];
        let a = su.dot(su) + f.dot(d[2][0]);
        let b = su.dot(sv) + f.dot(d[1][1]);
        let c = sv.dot(sv) + f.dot(d[0][2]);
        let (mut x, mut y) = match (fix_u, fix_v) {
            (true, true) => (0.0, 0.0),
            (true, false) => (0.0, if c > 1e-300 { g[1] / c } else { 0.0 }),
            (false, true) => (if a > 1e-300 { g[0] / a } else { 0.0 }, 0.0),
            (false, false) => {
                let det = a * c - b * b;
                if det > 1e-300 && a > 0.0 {
                    ((c * g[0] - b * g[1]) / det, (a * g[1] - b * g[0]) / det)
                } else {
                    let k = 1.0 / (su.dot(su) + sv.dot(sv)).max(1e-300);
                    (g[0] * k, g[1] * k)
                }
            }
        };
        if !x.is_finite() || !y.is_finite() {
            (x, y) = (0.0, 0.0);
        }
        u = (u - x).clamp(du.0, du.1);
        v = (v - y).clamp(dv.0, dv.1);
        if x.abs() + y.abs() < 1e-14 {
            break;
        }
    }
    [u, v]
}

#[cfg(test)]
#[path = "panelize_tests.rs"]
mod tests;
