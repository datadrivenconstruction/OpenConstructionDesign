//! Deterministic adaptive tessellation by chord and angle tolerance, thickening into a
//! closed mesh, and booleans on the tessellation (with the loss stated).
//!
//! Surfaces use a tensor grid refined per u and v interval: cells sharing an edge always
//! share its vertices (no T-junctions), seams of closed surfaces and poles are welded by
//! index, so the mesh is crack-free by construction. The same input always yields the
//! same mesh. Refinement aims at `SAFETY * chord` so that points between the checked
//! samples stay within `chord`.

use super::{Curve, NurbsError, Surface};
use crate::csg::{Body, BoolOp};
use crate::math::{Tol, V3};
use crate::mesh::Mesh;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TessTol {
    /// Max distance between the exact shape and the facets, metres.
    pub chord: f64,
    /// Max angle between normals (surfaces) or tangents (curves) across one facet, radians.
    pub angle: f64,
    /// Cap on grid lines per direction; reaching it leaves `converged` false.
    pub max_per_dir: usize,
}

impl Default for TessTol {
    fn default() -> Self {
        TessTol {
            chord: 1e-3,
            angle: 15f64.to_radians(),
            max_per_dir: 2048,
        }
    }
}

const SAFETY: f64 = 0.7;
const ROUNDS: usize = 40;

const LOSS_FACETS: &str = "exact surface replaced by triangles; measure: max deviation, m";
const LOSS_OFFSET: &str =
    "offset sampled at vertices along normals, self-intersection not checked; measure: thickness, m";
const LOSS_BOOLEAN: &str =
    "boolean ran on the tessellation, result is a faceted mesh; measure: max input deviation, m";

/// What a conversion dropped and how much (for receipts and plan cards).
#[derive(Clone, Debug, PartialEq)]
pub struct Loss {
    pub what: &'static str,
    pub measure: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurveTess {
    pub params: Vec<f64>,
    pub points: Vec<V3>,
    /// Largest measured deviation at the checked samples.
    pub max_dev: f64,
    /// Every segment met the chord and angle tolerance.
    pub converged: bool,
}

#[derive(Clone, Debug)]
pub struct SurfaceMesh {
    pub mesh: Mesh,
    /// Parameters of each vertex (seam and pole vertices carry the first occurrence).
    pub uv: Vec<[f64; 2]>,
    pub normals: Vec<Option<V3>>,
    /// Grid lines; pass them to [`tessellate_on_grid`] to match a neighbour's edges.
    pub us: Vec<f64>,
    pub vs: Vec<f64>,
    pub max_dev: f64,
    pub converged: bool,
}

/// Closed mesh solid made from a surface.
#[derive(Clone, Debug)]
pub struct Thick {
    /// `face_of`: 0 the surface side, 1 the offset side, 2 the side walls.
    pub mesh: Mesh,
    pub max_dev: f64,
    pub converged: bool,
    pub losses: Vec<Loss>,
}

/// Distinct knots of the domain, each span split into `p` equal parts.
fn initial_breaks(knots: &[f64], p: usize) -> Vec<f64> {
    let n = knots.len() - p - 1;
    let mut d: Vec<f64> = knots[p..=n].to_vec();
    d.dedup();
    let mut out = vec![d[0]];
    for w in d.windows(2) {
        for k in 1..p {
            out.push(w[0] + (w[1] - w[0]) * k as f64 / p as f64);
        }
        out.push(w[1]);
    }
    out
}

fn angle(a: V3, b: V3) -> f64 {
    let c = a.cross(b).len();
    let d = a.dot(b);
    if c == 0.0 && d == 0.0 {
        0.0
    } else {
        c.atan2(d)
    }
}

fn split(ts: &[f64], mark: &[bool]) -> (Vec<f64>, Vec<bool>) {
    let mut out = vec![ts[0]];
    let mut fresh = Vec::with_capacity(mark.len() * 2);
    for (k, &m) in mark.iter().enumerate() {
        if m {
            out.push(0.5 * (ts[k] + ts[k + 1]));
            fresh.push(true);
            fresh.push(true);
        } else {
            fresh.push(false);
        }
        out.push(ts[k + 1]);
    }
    (out, fresh)
}

fn cap(mark: &mut [bool], ts: &[f64], min_w: f64, max: usize) {
    for (k, m) in mark.iter_mut().enumerate() {
        if *m && ts[k + 1] - ts[k] <= min_w {
            *m = false;
        }
    }
    let add = mark.iter().filter(|&&m| m).count();
    if ts.len() + add > max {
        mark.iter_mut().for_each(|m| *m = false);
    }
}

impl Curve {
    /// (deviation at the quarter points, tangent turn) of the segment [a, b].
    fn seg_err(&self, a: f64, b: f64) -> (f64, f64) {
        let (pa, pb) = (self.point(a), self.point(b));
        let mut dev = 0.0f64;
        for t in [0.25, 0.5, 0.75] {
            let q = self.point(a + (b - a) * t);
            dev = dev.max((q - (pa + (pb - pa) * t)).len());
        }
        let ta = self.derivs(a, 1)[1];
        let tb = self.derivs(b, 1)[1];
        (dev, angle(ta, tb))
    }

    /// Polyline within `tol.chord` and `tol.angle`, refined by bisection from the knots.
    pub fn tessellate(&self, tol: &TessTol) -> CurveTess {
        let (a, b) = self.domain();
        let min_w = (b - a) * 1e-9;
        let lim = SAFETY * tol.chord;
        let mut ts = initial_breaks(&self.knots, self.degree);
        let mut fresh = vec![true; ts.len() - 1];
        for _ in 0..ROUNDS {
            let mut mark = vec![false; ts.len() - 1];
            for (k, m) in mark.iter_mut().enumerate() {
                if fresh[k] {
                    let (dev, ang) = self.seg_err(ts[k], ts[k + 1]);
                    *m = dev > lim || ang > tol.angle;
                }
            }
            cap(&mut mark, &ts, min_w, tol.max_per_dir);
            if !mark.contains(&true) {
                break;
            }
            (ts, fresh) = split(&ts, &mark);
        }
        let mut max_dev = 0.0f64;
        let mut converged = true;
        for w in ts.windows(2) {
            let (dev, ang) = self.seg_err(w[0], w[1]);
            max_dev = max_dev.max(dev);
            converged &= dev <= tol.chord && ang <= tol.angle;
        }
        CurveTess {
            points: ts.iter().map(|&t| self.point(t)).collect(),
            params: ts,
            max_dev,
            converged,
        }
    }
}

type C = (usize, usize);

struct Grid {
    nv: usize,
    p: Vec<V3>,
    n: Vec<Option<V3>>,
}

impl Grid {
    fn eval(s: &Surface, us: &[f64], vs: &[f64]) -> Grid {
        let mut p = Vec::with_capacity(us.len() * vs.len());
        let mut n = Vec::with_capacity(us.len() * vs.len());
        for &u in us {
            for &v in vs {
                p.push(s.point(u, v));
                n.push(s.normal(u, v));
            }
        }
        Grid { nv: vs.len(), p, n }
    }
    fn at(&self, c: C) -> V3 {
        self.p[c.0 * self.nv + c.1]
    }
    fn nrm(&self, c: C) -> Option<V3> {
        self.n[c.0 * self.nv + c.1]
    }
}

/// Two triangles of cell (i, j), counter-clockwise in (u, v), split on the shorter diagonal.
fn cell_tris(g: &Grid, i: usize, j: usize) -> [[C; 3]; 2] {
    let (a, b, c, d) = ((i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1));
    if (g.at(a) - g.at(c)).len() <= (g.at(b) - g.at(d)).len() {
        [[a, b, c], [a, c, d]]
    } else {
        [[a, b, d], [b, c, d]]
    }
}

#[derive(Default)]
struct CellErr {
    /// Deviation at midpoints of edges along u, along v, and on the diagonal or centroid.
    dev_u: f64,
    dev_v: f64,
    dev_x: f64,
    ang_u: f64,
    ang_v: f64,
}

impl CellErr {
    fn dev(&self) -> f64 {
        self.dev_u.max(self.dev_v).max(self.dev_x)
    }
}

fn cell_err(s: &Surface, g: &Grid, us: &[f64], vs: &[f64], i: usize, j: usize) -> CellErr {
    let mut e = CellErr::default();
    for tri in cell_tris(g, i, j) {
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            let q = s.point(0.5 * (us[a.0] + us[b.0]), 0.5 * (vs[a.1] + vs[b.1]));
            let dev = (q - (g.at(a) + g.at(b)) * 0.5).len();
            if a.1 == b.1 {
                e.dev_u = e.dev_u.max(dev);
            } else if a.0 == b.0 {
                e.dev_v = e.dev_v.max(dev);
            } else {
                e.dev_x = e.dev_x.max(dev);
            }
        }
        let third = 1.0 / 3.0;
        let uc = (us[tri[0].0] + us[tri[1].0] + us[tri[2].0]) * third;
        let vc = (vs[tri[0].1] + vs[tri[1].1] + vs[tri[2].1]) * third;
        let lin = (g.at(tri[0]) + g.at(tri[1]) + g.at(tri[2])) * third;
        e.dev_x = e.dev_x.max((s.point(uc, vc) - lin).len());
    }
    let ang = |a: C, b: C| match (g.nrm(a), g.nrm(b)) {
        (Some(x), Some(y)) => angle(x, y),
        _ => 0.0,
    };
    let (c00, c10, c11, c01) = ((i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1));
    e.ang_u = ang(c00, c10).max(ang(c01, c11));
    e.ang_v = ang(c00, c01).max(ang(c10, c11));
    e
}

/// Adaptive tessellation of a surface within `tol`.
pub fn tessellate(s: &Surface, tol: &TessTol) -> SurfaceMesh {
    let mut us = initial_breaks(s.knots_u(), s.deg_u());
    let mut vs = initial_breaks(s.knots_v(), s.deg_v());
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    let (min_u, min_v) = ((u1 - u0) * 1e-9, (v1 - v0) * 1e-9);
    let lim = SAFETY * tol.chord;
    let mut fu = vec![true; us.len() - 1];
    let mut fv = vec![true; vs.len() - 1];
    for _ in 0..ROUNDS {
        let g = Grid::eval(s, &us, &vs);
        let mut mu = vec![false; fu.len()];
        let mut mv = vec![false; fv.len()];
        for i in 0..fu.len() {
            for j in 0..fv.len() {
                if !(fu[i] || fv[j]) {
                    continue;
                }
                let e = cell_err(s, &g, &us, &vs, i, j);
                if e.dev_u > lim || e.dev_x > lim || e.ang_u > tol.angle {
                    mu[i] = true;
                }
                if e.dev_v > lim || e.dev_x > lim || e.ang_v > tol.angle {
                    mv[j] = true;
                }
            }
        }
        cap(&mut mu, &us, min_u, tol.max_per_dir);
        cap(&mut mv, &vs, min_v, tol.max_per_dir);
        if !mu.contains(&true) && !mv.contains(&true) {
            break;
        }
        (us, fu) = split(&us, &mu);
        (vs, fv) = split(&vs, &mv);
    }
    tessellate_on_grid(s, &us, &vs, tol)
}

fn clean_lines(ts: &[f64], lo: f64, hi: f64) -> Vec<f64> {
    let mut v: Vec<f64> = ts
        .iter()
        .copied()
        .filter(|t| t.is_finite())
        .map(|t| t.clamp(lo, hi))
        .collect();
    v.push(lo);
    v.push(hi);
    v.sort_by(f64::total_cmp);
    v.dedup();
    v
}

/// Mesh on the given grid lines (clamped to the domain, ends added). Two surfaces that
/// share a boundary curve with the same parameterization get identical edge vertices when
/// they use the same lines along it. `tol` only measures `max_dev` and `converged`.
pub fn tessellate_on_grid(s: &Surface, us: &[f64], vs: &[f64], tol: &TessTol) -> SurfaceMesh {
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    let us = clean_lines(us, u0, u1);
    let vs = clean_lines(vs, v0, v1);
    let g = Grid::eval(s, &us, &vs);
    let (nu, nv) = (us.len(), vs.len());
    let mut max_dev = 0.0f64;
    let mut converged = true;
    for i in 0..nu - 1 {
        for j in 0..nv - 1 {
            let e = cell_err(s, &g, &us, &vs, i, j);
            max_dev = max_dev.max(e.dev());
            converged &= e.dev() <= tol.chord && e.ang_u <= tol.angle && e.ang_v <= tol.angle;
        }
    }
    let (cu, cv) = (s.closed_u(), s.closed_v());
    let (pu0, pu1) = (s.pole_u(false), s.pole_u(true));
    let (pv0, pv1) = (s.pole_v(false), s.pole_v(true));
    // Rules only move indices toward 0, so the loop reaches a fixed point.
    let canon = |i: usize, j: usize| -> C {
        let (mut a, mut b) = (i, j);
        loop {
            let prev = (a, b);
            if cu && a == nu - 1 {
                a = 0;
            }
            if cv && b == nv - 1 {
                b = 0;
            }
            if (pv0 && b == 0) || (pv1 && b == nv - 1) {
                a = 0;
            }
            if (pu0 && a == 0) || (pu1 && a == nu - 1) {
                b = 0;
            }
            if (a, b) == prev {
                return (a, b);
            }
        }
    };
    let mut keys: BTreeMap<C, u32> = BTreeMap::new();
    let mut ids = vec![0u32; nu * nv];
    let mut mesh = Mesh::default();
    let mut uv = Vec::new();
    let mut normals = Vec::new();
    for i in 0..nu {
        for j in 0..nv {
            let c = canon(i, j);
            let id = *keys.entry(c).or_insert_with(|| {
                mesh.positions.push(g.at(c));
                uv.push([us[c.0], vs[c.1]]);
                normals.push(g.nrm(c));
                (mesh.positions.len() - 1) as u32
            });
            ids[i * nv + j] = id;
        }
    }
    for i in 0..nu - 1 {
        for j in 0..nv - 1 {
            for tri in cell_tris(&g, i, j) {
                let t = tri.map(|c| ids[c.0 * nv + c.1]);
                if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                    mesh.triangles.push(t);
                    mesh.face_of.push(0);
                }
            }
        }
    }
    SurfaceMesh {
        mesh,
        uv,
        normals,
        us,
        vs,
        max_dev,
        converged,
    }
}

/// Closed mesh solid between the surface and its offset by `thickness` along the normal
/// (negative goes the other way). Open boundaries get side walls; seams and poles need
/// none. Triangles face outward.
pub fn thicken(s: &Surface, thickness: f64, tol: &TessTol) -> Result<Thick, NurbsError> {
    if !thickness.is_finite() || thickness == 0.0 {
        return Err(NurbsError::BadThickness);
    }
    thicken_between(&tessellate(s, tol), 0.0, thickness)
}

/// Closed mesh between the offsets `a` and `b` (metres along the normal) of a tessellated
/// surface, trimmed or not: `a = -t/2, b = t/2` centres the shell on the surface.
pub fn thicken_between(sm: &SurfaceMesh, a: f64, b: f64) -> Result<Thick, NurbsError> {
    let thickness = b - a;
    if !(a.is_finite() && b.is_finite()) || thickness == 0.0 {
        return Err(NurbsError::BadThickness);
    }
    let top = &sm.mesh;
    let n = top.positions.len() as u32;
    let mut positions = Vec::with_capacity(top.positions.len() * 2);
    let mut lower = Vec::with_capacity(top.positions.len());
    for (k, nrm) in sm.normals.iter().enumerate() {
        let nrm = nrm.ok_or(NurbsError::NoNormal)?;
        positions.push(top.positions[k] + nrm * a);
        lower.push(top.positions[k] + nrm * b);
    }
    positions.extend(lower);
    let directed: BTreeSet<(u32, u32)> = top
        .triangles
        .iter()
        .flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
        .collect();
    let mut triangles = Vec::with_capacity(top.triangles.len() * 2);
    let mut face_of = Vec::with_capacity(top.triangles.len() * 2);
    for t in &top.triangles {
        triangles.push(*t);
        face_of.push(0);
    }
    for t in &top.triangles {
        triangles.push([t[0] + n, t[2] + n, t[1] + n]);
        face_of.push(1);
    }
    for t in &top.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if !directed.contains(&(b, a)) {
                triangles.push([b, a, a + n]);
                triangles.push([b, a + n, b + n]);
                face_of.push(2);
                face_of.push(2);
            }
        }
    }
    let mut mesh = Mesh {
        positions,
        triangles,
        face_of,
    };
    if mesh.volume() < 0.0 {
        for t in mesh.triangles.iter_mut() {
            t.swap(1, 2);
        }
    }
    Ok(Thick {
        mesh,
        max_dev: sm.max_dev,
        converged: sm.converged,
        losses: vec![
            Loss {
                what: LOSS_FACETS,
                measure: sm.max_dev,
            },
            Loss {
                what: LOSS_OFFSET,
                measure: thickness.abs(),
            },
        ],
    })
}

/// Boolean of two thickened freeform solids, run on their meshes by the planar CSG.
/// Exact surfaces do not survive; the loss names the facet deviation carried in.
pub fn boolean_on_tessellation(a: &Thick, b: &Thick, op: BoolOp, tol: Tol) -> (Mesh, Vec<Loss>) {
    let tagged = |m: &Mesh, tag: u32| Mesh {
        face_of: vec![tag; m.triangles.len()],
        ..m.clone()
    };
    let big = a.mesh.triangles.len() + b.mesh.triangles.len() >= crate::csg2::MIN_FACES;
    let fast = big
        .then(|| crate::csg2::mesh_boolean(&tagged(&a.mesh, 1), &tagged(&b.mesh, 2), op, tol));
    let mut losses = vec![];
    let out = match fast {
        Some(Ok(r)) => r.mesh,
        refused => {
            if let Some(Err(e)) = refused {
                losses.push(Loss {
                    what: e.fallback_note(),
                    measure: (a.mesh.triangles.len() + b.mesh.triangles.len()) as f64,
                });
            }
            Body::from_mesh(&a.mesh, 1)
                .boolean(&Body::from_mesh(&b.mesh, 2), op, tol)
                .to_mesh()
        }
    };
    losses.insert(
        0,
        Loss {
            what: LOSS_BOOLEAN,
            measure: a.max_dev.max(b.max_dev),
        },
    );
    (out, losses)
}
