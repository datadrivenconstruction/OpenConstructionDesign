//! Trimmed tessellation: loops in normalized parameters ([0, 1] over each domain) cut the
//! surface. The adaptive grid of [`tessellate`] supplies interior vertices; loop vertices,
//! densified to the grid step, become constrained edges of the own constrained Delaunay
//! triangulation (`topo::cdt`); triangles outside the outer loop or inside a hole are dropped.

use super::{tessellate, NurbsError, Surface, SurfaceMesh, TessTol};
use crate::math::{v2, V2};
use crate::mesh::Mesh;
use crate::predicates::point_in_ring;
use crate::topo::cdt::Cdt;

/// Trim loops in normalized (u, v); `outer: None` keeps the whole domain as the outside.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trim {
    pub outer: Option<Vec<[f64; 2]>>,
    pub holes: Vec<Vec<[f64; 2]>>,
}

fn ring(r: &[[f64; 2]]) -> Vec<V2> {
    r.iter().map(|p| v2(p[0], p[1])).collect()
}

fn area2(r: &[[f64; 2]]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
}

impl Trim {
    pub fn is_empty(&self) -> bool {
        self.outer.is_none() && self.holes.is_empty()
    }

    /// Every loop has 3+ finite points inside [0, 1]^2 and a non-zero area; every hole
    /// vertex lies inside the outer loop.
    pub fn validate(&self) -> Result<(), NurbsError> {
        let loops = self.outer.iter().chain(&self.holes);
        for r in loops {
            if r.len() < 3 {
                return Err(NurbsError::TooFewPoints);
            }
            for (k, p) in r.iter().enumerate() {
                if !(p[0].is_finite() && p[1].is_finite())
                    || !(-1e-12..=1.0 + 1e-12).contains(&p[0])
                    || !(-1e-12..=1.0 + 1e-12).contains(&p[1])
                {
                    return Err(NurbsError::BadPoint(k));
                }
            }
            if area2(r).abs() < 1e-12 {
                return Err(NurbsError::Degenerate);
            }
        }
        if let Some(o) = &self.outer {
            let o = ring(o);
            for h in &self.holes {
                if h.iter().any(|p| point_in_ring(v2(p[0], p[1]), &o) <= 0) {
                    return Err(NurbsError::Degenerate);
                }
            }
        }
        Ok(())
    }

    /// The normalized point lies in the kept region (boundary counts as inside).
    pub fn keeps(&self, p: [f64; 2]) -> bool {
        let q = v2(p[0], p[1]);
        if let Some(o) = &self.outer {
            if point_in_ring(q, &ring(o)) < 0 {
                return false;
            }
        }
        !self.holes.iter().any(|h| point_in_ring(q, &ring(h)) > 0)
    }

    /// Kept fraction of the parameter square (exact for straight loops).
    pub fn kept_fraction(&self) -> f64 {
        let o = self.outer.as_ref().map_or(1.0, |o| 0.5 * area2(o).abs());
        o - self.holes.iter().map(|h| 0.5 * area2(h).abs()).sum::<f64>()
    }
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let ab = v2(b.x - a.x, b.y - a.y);
    let l2 = ab.x * ab.x + ab.y * ab.y;
    let t = if l2 > 0.0 {
        (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (dx, dy) = (p.x - a.x - ab.x * t, p.y - a.y - ab.y * t);
    (dx * dx + dy * dy).sqrt()
}

/// Tessellation of the kept region within `tol` along the grid; loop edges are straight in
/// the parameters. An empty trim returns the plain tessellation.
pub fn tessellate_trimmed(s: &Surface, trim: &Trim, tol: &TessTol) -> Result<SurfaceMesh, NurbsError> {
    let base = tessellate(s, tol);
    if trim.is_empty() {
        return Ok(base);
    }
    trim.validate()?;
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    let nrm_u: Vec<f64> = base.us.iter().map(|u| (u - u0) / (u1 - u0)).collect();
    let nrm_v: Vec<f64> = base.vs.iter().map(|v| (v - v0) / (v1 - v0)).collect();
    let step = |t: &[f64]| t.windows(2).map(|w| w[1] - w[0]).fold(1.0f64, f64::min);
    let h = step(&nrm_u).min(step(&nrm_v)).max(1e-4);
    // densified loops (closed, without the repeated first point)
    let mut loops: Vec<Vec<V2>> = vec![];
    let outer = trim.outer.clone().unwrap_or_else(|| vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    for r in std::iter::once(&outer).chain(&trim.holes) {
        let mut d = vec![];
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let k = ((len / h).ceil() as usize).clamp(1, 4096);
            for m in 0..k {
                let t = m as f64 / k as f64;
                d.push(v2(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t));
            }
        }
        loops.push(d);
    }
    let mut c = Cdt::new(v2(0.0, 0.0), v2(1.0, 1.0));
    let mut loop_ids: Vec<Vec<u32>> = vec![];
    for l in &loops {
        let ids: Vec<u32> = l.iter().map(|p| c.push(*p, 0.0)).collect();
        loop_ids.push(ids);
    }
    let near_loop = |p: V2| {
        loops.iter().any(|l| {
            (0..l.len()).any(|i| seg_dist(p, l[i], l[(i + 1) % l.len()]) < 0.35 * h)
        })
    };
    for &u in &nrm_u {
        for &v in &nrm_v {
            let p = v2(u, v);
            if trim.keeps([u, v]) && !near_loop(p) {
                c.push(p, 0.0);
            }
        }
    }
    let n = c.p.len() as u32;
    let mut alias: Vec<u32> = (0..n).collect();
    for v in 3..n {
        alias[v as usize] = c.insert(v);
    }
    for ids in &loop_ids {
        for i in 0..ids.len() {
            let (a, b) = (alias[ids[i] as usize], alias[ids[(i + 1) % ids.len()] as usize]);
            if a != b {
                c.constrain(a, b);
            }
        }
    }
    let out = c.finish(None);
    let mut mesh = Mesh::default();
    let mut uv = vec![];
    let mut normals = vec![];
    let mut map = vec![u32::MAX; out.p.len()];
    let mut max_dev = 0.0f64;
    for t in &out.tris {
        let [a, b, cc] = t.map(|k| out.p[k as usize]);
        let m = v2((a.x + b.x + cc.x) / 3.0, (a.y + b.y + cc.y) / 3.0);
        if !trim.keeps([m.x, m.y]) {
            continue;
        }
        let ccw = (b.x - a.x) * (cc.y - a.y) - (b.y - a.y) * (cc.x - a.x);
        if ccw.abs() < 1e-18 {
            continue;
        }
        let mut tri = [0u32; 3];
        for (k, &vi) in t.iter().enumerate() {
            if map[vi as usize] == u32::MAX {
                let q = out.p[vi as usize];
                let (u, v) = (u0 + (u1 - u0) * q.x, v0 + (v1 - v0) * q.y);
                mesh.positions.push(s.point(u, v));
                uv.push([u, v]);
                normals.push(s.normal(u, v));
                map[vi as usize] = (mesh.positions.len() - 1) as u32;
            }
            tri[k] = map[vi as usize];
        }
        if ccw < 0.0 {
            tri.swap(1, 2);
        }
        let [pa, pb, pc] = tri.map(|k| mesh.positions[k as usize]);
        let exact = s.point(u0 + (u1 - u0) * m.x, v0 + (v1 - v0) * m.y);
        max_dev = max_dev.max((exact - (pa + pb + pc) * (1.0 / 3.0)).len());
        mesh.triangles.push(tri);
        mesh.face_of.push(0);
    }
    if mesh.triangles.is_empty() {
        return Err(NurbsError::Degenerate);
    }
    Ok(SurfaceMesh {
        mesh,
        uv,
        normals,
        us: base.us,
        vs: base.vs,
        max_dev: max_dev.max(base.max_dev),
        converged: base.converged && max_dev <= tol.chord,
    })
}
