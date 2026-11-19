// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Terrain (topo solid): constrained Delaunay triangulation of survey points with breaklines,
//! a boundary and sub-regions; elevation at a point, contours, profile and filled section,
//! cut/fill volumes, a solid of given thickness, building pads (`grade`), smoothing.

pub(crate) mod cdt;
mod exact;
mod grade;
mod index;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod qa_cdt_tests;

use std::collections::{HashMap, HashSet};

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::predicates::{coord_in_range, orient2d, point_in_ring};

pub use exact::incircle;
pub use grade::{grade, smooth, Graded, Pad};

use cdt::{hilbert, Cdt, NONE, SUPER};
use index::Index;

/// Surface input data. z is the absolute elevation, in metres.
#[derive(Clone, Copy, Debug, Default)]
pub struct TinInput<'a> {
    pub points: &'a [V3],
    /// Breaklines (crests, ditch axes): triangulation edges along them are mandatory.
    pub breaklines: &'a [Vec<V3>],
    /// Surface boundary (elevations taken from the surface); if absent, the convex hull.
    pub boundary: Option<&'a [V2]>,
    /// Sub-regions (paths, lawns): their loops are embedded in the surface as edges.
    pub regions: &'a [Vec<V2>],
}

#[derive(Clone, Debug, PartialEq)]
pub enum TinError {
    TooFewPoints,
    Collinear,
    BadCoord,
}

impl std::fmt::Display for TinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TinError::TooFewPoints => "terrain needs at least 3 distinct points",
            TinError::Collinear => "terrain points are collinear",
            TinError::BadCoord => "terrain coordinate is not finite or out of range",
        })
    }
}

impl std::error::Error for TinError {}

/// Triangulated surface.
#[derive(Clone, Debug, Default)]
pub struct Tin {
    pub pts: Vec<V3>,
    /// Counter-clockwise in plan.
    pub tris: Vec<[u32; 3]>,
    /// Constraint edges (breaklines, boundary, sub-region loops, hull).
    pub fixed: Vec<[u32; 2]>,
    /// Sub-region of the triangle (index into `TinInput::regions`), -1 for the main surface.
    pub region_of: Vec<i32>,
    /// Source point of the vertex (index into `TinInput::points`), `u32::MAX` if added.
    pub src: Vec<u32>,
    index: Index,
}

/// Contour: elevation, major flag (every `major_every`-th), polyline.
#[derive(Clone, Debug, PartialEq)]
pub struct Contour {
    pub z: f64,
    pub major: bool,
    pub pts: Vec<V3>,
    pub closed: bool,
}

/// Volumes, m³: cut is soil above the level (to remove), fill is below it (to add).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CutFill {
    pub cut: f64,
    pub fill: f64,
}

fn finite(p: V3) -> bool {
    p.x.is_finite()
        && p.y.is_finite()
        && p.z.is_finite()
        && coord_in_range(p.x)
        && coord_in_range(p.y)
}

fn xy(p: V3) -> V2 {
    v2(p.x, p.y)
}

/// Integrals of the positive and negative parts of a linear function over a triangle
/// (vertex values, area): both ≥ 0, their difference is the integral of the function.
pub(crate) fn pos_neg(f: [f64; 3], area: f64) -> (f64, f64) {
    let total = area * (f[0] + f[1] + f[2]) / 3.0;
    let npos = f.iter().filter(|&&x| x > 0.0).count();
    let nneg = f.iter().filter(|&&x| x < 0.0).count();
    if nneg == 0 {
        return (total.max(0.0), 0.0);
    }
    if npos == 0 {
        return (0.0, (-total).max(0.0));
    }
    let lone_pos = npos == 1;
    let k = f
        .iter()
        .position(|&x| if lone_pos { x > 0.0 } else { x < 0.0 })
        .unwrap_or(0);
    let (fa, fb, fc) = (f[k], f[(k + 1) % 3], f[(k + 2) % 3]);
    // the corner at a lone vertex is cut off by the zero line
    let part = area * (fa / (fa - fb)) * (fa / (fa - fc)) * fa / 3.0;
    if lone_pos {
        (part, (part - total).max(0.0))
    } else {
        ((total - part).max(0.0), -part)
    }
}

impl Tin {
    /// Surface from points, breaklines, boundary and sub-regions.
    pub fn build(inp: &TinInput) -> Result<Tin, TinError> {
        let lines_ok = inp.breaklines.iter().flatten().all(|&p| finite(p));
        let ring_ok = |r: &[V2]| r.iter().all(|&p| finite(v3(p.x, p.y, 0.0)));
        if !inp.points.iter().all(|&p| finite(p))
            || !lines_ok
            || !inp.boundary.map_or(true, ring_ok)
            || !inp.regions.iter().all(|r| ring_ok(r.as_slice()))
        {
            return Err(TinError::BadCoord);
        }
        let base = Self::pass(inp.points, inp.breaklines, None)?;
        if inp.boundary.is_none() && inp.regions.is_empty() {
            return Ok(base);
        }
        let mut lines: Vec<Vec<V3>> = inp.breaklines.to_vec();
        let boundary = inp.boundary.filter(|r| r.len() >= 3);
        if let Some(b) = boundary {
            lines.push(base.drape(b));
        }
        lines.extend(
            inp.regions
                .iter()
                .filter(|r| r.len() >= 3)
                .map(|r| base.drape(r)),
        );
        let mut t = Self::pass(inp.points, &lines, boundary)?;
        t.classify(inp.regions);
        Ok(t)
    }

    /// The same surface with new sub-regions (constraint edges and the outer boundary are kept).
    pub fn with_regions(&self, regions: &[Vec<V2>]) -> Result<Tin, TinError> {
        if regions.is_empty() {
            let mut t = self.clone();
            t.region_of = vec![-1; t.tris.len()];
            return Ok(t);
        }
        let mut lines: Vec<Vec<V3>> = self
            .fixed
            .iter()
            .map(|e| vec![self.pts[e[0] as usize], self.pts[e[1] as usize]])
            .collect();
        let outer = self.outer_ring();
        if let Some(o) = &outer {
            lines.push(self.drape(o));
        }
        lines.extend(
            regions
                .iter()
                .filter(|r| r.len() >= 3)
                .map(|r| self.drape(r)),
        );
        let mut t = Self::pass(&self.pts, &lines, outer.as_deref())?;
        t.classify(regions);
        Ok(t)
    }

    fn classify(&mut self, regions: &[Vec<V2>]) {
        self.region_of = self
            .tris
            .iter()
            .map(|t| {
                let m = self.centroid(t);
                regions
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, r)| r.len() >= 3 && point_in_ring(m, r) > 0)
                    .map_or(-1, |(i, _)| i as i32)
            })
            .collect();
    }

    fn centroid(&self, t: &[u32; 3]) -> V2 {
        let [a, b, c] = t.map(|i| self.pts[i as usize]);
        v2((a.x + b.x + c.x) / 3.0, (a.y + b.y + c.y) / 3.0)
    }

    /// One triangulation: points and line vertices (coincident within 1 µm are merged), lines are
    /// constraints, then the hull or clipping by `clip`.
    pub(crate) fn pass(
        points: &[V3],
        lines: &[Vec<V3>],
        clip: Option<&[V2]>,
    ) -> Result<Tin, TinError> {
        let n_in = points.len();
        let all: Vec<V3> = points
            .iter()
            .copied()
            .chain(lines.iter().flatten().copied())
            .collect();
        if all.len() < 3 {
            return Err(TinError::TooFewPoints);
        }
        let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
        for p in all
            .iter()
            .map(|&p| xy(p))
            .chain(clip.into_iter().flatten().copied())
        {
            lo = v2(lo.x.min(p.x), lo.y.min(p.y));
            hi = v2(hi.x.max(p.x), hi.y.max(p.y));
        }
        let mut c = Cdt::new(lo, hi);
        let q = 1e-6;
        // merge when closer than q along each axis: the point's cell and its neighbours (pairs across the cell boundary)
        let mut seen: HashMap<(i64, i64), Vec<u32>> = HashMap::with_capacity(all.len());
        let ids: Vec<u32> = all
            .iter()
            .map(|p| {
                let (kx, ky) = ((p.x / q).floor() as i64, (p.y / q).floor() as i64);
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        if let Some(vs) = seen.get(&(kx + dx, ky + dy)) {
                            for &v in vs {
                                let o = c.p[v as usize];
                                if (o.x - p.x).abs() <= q && (o.y - p.y).abs() <= q {
                                    return v;
                                }
                            }
                        }
                    }
                }
                let v = c.push(xy(*p), p.z);
                seen.entry((kx, ky)).or_default().push(v);
                v
            })
            .collect();
        let nv = c.p.len() as u32;
        if nv - SUPER < 3 {
            return Err(TinError::TooFewPoints);
        }
        let (w, h) = ((hi.x - lo.x).max(1e-12), (hi.y - lo.y).max(1e-12));
        let mut order: Vec<(u64, u32)> = (SUPER..nv)
            .map(|v| {
                let p = c.p[v as usize];
                let gx = (((p.x - lo.x) / w) * 65535.0).clamp(0.0, 65535.0) as u32;
                let gy = (((p.y - lo.y) / h) * 65535.0).clamp(0.0, 65535.0) as u32;
                (hilbert(gx, gy), v)
            })
            .collect();
        order.sort_unstable();
        let mut alias: Vec<u32> = (0..nv).collect();
        for &(_, v) in &order {
            alias[v as usize] = c.insert(v);
        }
        let mut k = n_in;
        for line in lines {
            for j in 1..line.len() {
                let a = alias[ids[k + j - 1] as usize];
                let b = alias[ids[k + j] as usize];
                c.constrain(a, b);
            }
            k += line.len();
        }
        if clip.is_none() {
            let hull = c.hull();
            if hull.len() < 3 {
                return Err(TinError::Collinear);
            }
            for i in 0..hull.len() {
                c.constrain(hull[i], hull[(i + 1) % hull.len()]);
            }
        }
        let mut first_in = vec![NONE; c.p.len()];
        for (i, &id) in ids.iter().take(n_in).enumerate() {
            let v = alias[id as usize] as usize;
            if first_in[v] == NONE {
                first_in[v] = i as u32;
            }
        }
        let out = c.finish(clip);
        if out.tris.is_empty() {
            return Err(TinError::Collinear);
        }
        let pts: Vec<V3> = out
            .p
            .iter()
            .zip(&out.z)
            .map(|(p, &z)| v3(p.x, p.y, z))
            .collect();
        let src = out
            .old
            .iter()
            .map(|&o| first_in.get(o as usize).copied().unwrap_or(NONE))
            .collect();
        let index = Index::new(&pts, &out.tris);
        Ok(Tin {
            region_of: vec![-1; out.tris.len()],
            pts,
            tris: out.tris,
            fixed: out.fixed,
            src,
            index,
        })
    }

    /// Loop on the surface: vertices with surface elevations (outside, those of the nearest vertex),
    /// edges split at intersections with triangulation edges; closed by repeating the first point.
    pub fn drape(&self, ring: &[V2]) -> Vec<V3> {
        let mut out = vec![];
        let n = ring.len();
        for i in 0..n {
            let (s, e) = (ring[i], ring[(i + 1) % n]);
            out.push(v3(s.x, s.y, self.z_or_nearest(s)));
            let d = e - s;
            let lo = v2(s.x.min(e.x), s.y.min(e.y));
            let hi = v2(s.x.max(e.x), s.y.max(e.y));
            let mut hits: Vec<(f64, V3)> = vec![];
            let mut done: HashSet<(u32, u32)> = HashSet::new();
            for t in self.index.rect(lo, hi) {
                let tr = self.tris[t as usize];
                for k in 0..3 {
                    let (a, b) = (tr[k], tr[(k + 1) % 3]);
                    if !done.insert((a.min(b), a.max(b))) {
                        continue;
                    }
                    let (pa, pb) = (self.pts[a as usize], self.pts[b as usize]);
                    let f = xy(pb) - xy(pa);
                    let den = d.cross(f);
                    if den == 0.0 {
                        continue;
                    }
                    let w = xy(pa) - s;
                    let (tt, u) = (w.cross(f) / den, w.cross(d) / den);
                    if tt > 1e-9 && tt < 1.0 - 1e-9 && (0.0..=1.0).contains(&u) {
                        let p = s + d * tt;
                        hits.push((tt, v3(p.x, p.y, pa.z + (pb.z - pa.z) * u)));
                    }
                }
            }
            hits.sort_by(|a, b| a.0.total_cmp(&b.0));
            hits.dedup_by(|a, b| (a.0 - b.0).abs() * d.len() < 1e-7);
            out.extend(hits.into_iter().map(|h| h.1));
        }
        if let Some(&f) = out.first() {
            out.push(f);
        }
        out
    }

    fn z_or_nearest(&self, p: V2) -> f64 {
        self.z_at(p)
            .or_else(|| self.nearest_vertex(p).map(|v| self.pts[v as usize].z))
            .unwrap_or(0.0)
    }

    /// Check: triangles are counter-clockwise and non-degenerate, every edge belongs to at most two
    /// triangles with opposite winding, all vertices are used, and non-constraint edges
    /// satisfy the Delaunay condition.
    pub fn validate(&self) -> Result<(), String> {
        let mut used = vec![false; self.pts.len()];
        let mut dir: HashMap<(u32, u32), usize> = HashMap::new();
        for (i, t) in self.tris.iter().enumerate() {
            if t.iter().any(|&v| v as usize >= self.pts.len()) {
                return Err(format!("triangle {i}: vertex out of range"));
            }
            let [a, b, c] = t.map(|v| xy(self.pts[v as usize]));
            if orient2d(a, b, c) <= 0.0 {
                return Err(format!("triangle {i}: not counter-clockwise"));
            }
            for k in 0..3 {
                used[t[k] as usize] = true;
                if dir.insert((t[k], t[(k + 1) % 3]), i).is_some() {
                    return Err(format!("triangle {i}: directed edge repeated"));
                }
            }
        }
        if let Some(v) = used.iter().position(|u| !u) {
            return Err(format!("vertex {v} unused"));
        }
        let fixed: HashSet<(u32, u32)> = self.fixed.iter().map(|e| (e[0], e[1])).collect();
        for (&(a, b), &t) in &dir {
            if a > b {
                continue;
            }
            let Some(&u) = dir.get(&(b, a)) else {
                continue;
            };
            if fixed.contains(&(a, b)) {
                continue;
            }
            let tt = self.tris[t];
            let d = *self.tris[u]
                .iter()
                .find(|&&x| x != a && x != b)
                .ok_or("bad neighbour")?;
            let [p, q, r] = tt.map(|v| xy(self.pts[v as usize]));
            if incircle(p, q, r, xy(self.pts[d as usize])) > 0.0 {
                return Err(format!("edge {a}-{b}: not Delaunay"));
            }
        }
        if self.region_of.len() != self.tris.len() || self.src.len() != self.pts.len() {
            return Err("per-triangle or per-vertex table length mismatch".into());
        }
        Ok(())
    }

    /// Surface elevation at a plan point; None outside the surface.
    pub fn z_at(&self, p: V2) -> Option<f64> {
        self.locate(p).map(|(t, w)| {
            let [a, b, c] = self.tris[t].map(|i| self.pts[i as usize].z);
            (w[0] * a + w[1] * b + w[2] * c) / (w[0] + w[1] + w[2])
        })
    }

    /// Triangle containing the plan point; None outside the surface.
    pub fn tri_at(&self, p: V2) -> Option<usize> {
        self.locate(p).map(|(t, _)| t)
    }

    /// Triangle under the point and its (unnormalised) barycentric weights.
    fn locate(&self, p: V2) -> Option<(usize, [f64; 3])> {
        for &t in self.index.at(p) {
            let tr = self.tris[t as usize];
            let [a, b, c] = tr.map(|i| xy(self.pts[i as usize]));
            let w0 = orient2d(b, c, p);
            let w1 = orient2d(c, a, p);
            let w2 = orient2d(a, b, p);
            if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 && w0 + w1 + w2 > 0.0 {
                return Some((t as usize, [w0, w1, w2]));
            }
        }
        None
    }

    /// Nearest vertex in plan.
    pub fn nearest_vertex(&self, p: V2) -> Option<u32> {
        let d2 = |v: u32| {
            let q = self.pts[v as usize];
            (q.x - p.x).powi(2) + (q.y - p.y).powi(2)
        };
        let near = self
            .index
            .at(p)
            .iter()
            .flat_map(|&t| self.tris[t as usize])
            .min_by(|&a, &b| d2(a).total_cmp(&d2(b)));
        if near.is_some() && self.z_at(p).is_some() {
            return near;
        }
        (0..self.pts.len() as u32).min_by(|&a, &b| d2(a).total_cmp(&d2(b)))
    }

    fn tri_area(&self, t: &[u32; 3]) -> f64 {
        let [a, b, c] = t.map(|i| xy(self.pts[i as usize]));
        0.5 * (b - a).cross(c - a)
    }

    pub fn plan_area(&self) -> f64 {
        self.tris.iter().map(|t| self.tri_area(t)).sum()
    }

    pub fn surface_area(&self) -> f64 {
        self.tris
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| self.pts[i as usize]);
                0.5 * (b - a).cross(c - a).len()
            })
            .sum()
    }

    pub fn bounds(&self) -> Option<(V3, V3)> {
        let f = self.pts.first()?;
        Some(self.pts.iter().fold((*f, *f), |(lo, hi), p| {
            (
                v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
                v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
            )
        }))
    }

    /// Mean edge length (sampling step for pads and profiles).
    pub fn spacing(&self) -> f64 {
        if self.tris.is_empty() {
            return 1.0;
        }
        (2.0 * self.plan_area() / self.tris.len() as f64)
            .sqrt()
            .clamp(0.05, 100.0)
    }

    /// Vertex neighbours via edges.
    pub fn neighbors(&self) -> Vec<Vec<u32>> {
        let mut nb = vec![vec![]; self.pts.len()];
        for t in &self.tris {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                nb[a as usize].push(b);
                nb[b as usize].push(a);
            }
        }
        for v in &mut nb {
            v.sort_unstable();
            v.dedup();
        }
        nb
    }

    /// Boundary loops (edges of a single triangle); outer ones are traversed counter-clockwise.
    pub fn boundary_loops(&self) -> Vec<Vec<u32>> {
        let dir: HashSet<(u32, u32)> = self
            .tris
            .iter()
            .flat_map(|t| (0..3).map(move |k| (t[k], t[(k + 1) % 3])))
            .collect();
        let mut next: HashMap<u32, Vec<u32>> = HashMap::new();
        for &(a, b) in &dir {
            if !dir.contains(&(b, a)) {
                next.entry(a).or_default().push(b);
            }
        }
        let mut starts: Vec<u32> = next.keys().copied().collect();
        starts.sort_unstable();
        let mut loops = vec![];
        for s in starts {
            while next.get(&s).is_some_and(|v| !v.is_empty()) {
                let mut lp = vec![s];
                let mut cur = s;
                loop {
                    let Some(n) = next.get_mut(&cur).and_then(|v| v.pop()) else {
                        break;
                    };
                    if n == s {
                        break;
                    }
                    lp.push(n);
                    cur = n;
                    if lp.len() > self.pts.len() {
                        break;
                    }
                }
                loops.push(lp);
            }
        }
        loops
    }

    /// Outer loop of the largest plan area.
    pub fn outer_ring(&self) -> Option<Vec<V2>> {
        let area = |r: &[V2]| {
            (0..r.len())
                .map(|i| r[i].cross(r[(i + 1) % r.len()]))
                .sum::<f64>()
                * 0.5
        };
        self.boundary_loops()
            .into_iter()
            .map(|l| {
                l.iter()
                    .map(|&v| xy(self.pts[v as usize]))
                    .collect::<Vec<V2>>()
            })
            .filter(|r| r.len() >= 3 && area(r) > 0.0)
            .max_by(|a, b| area(a).total_cmp(&area(b)))
    }

    /// Contours at step `interval` (elevations are multiples of the step); every `major_every`-th is major.
    pub fn contours(&self, interval: f64, major_every: u32) -> Vec<Contour> {
        let Some((lo, hi)) = self.bounds() else {
            return vec![];
        };
        if !(interval > 0.0) || ((hi.z - lo.z) / interval) > 20_000.0 {
            return vec![];
        }
        let k0 = (lo.z / interval).ceil() as i64;
        // segments per level: edge key (min, max) → intersection point
        let mut segs: HashMap<i64, Vec<[(u32, u32); 2]>> = HashMap::new();
        for t in &self.tris {
            let z = t.map(|i| self.pts[i as usize].z);
            let (zmin, zmax) = (z[0].min(z[1]).min(z[2]), z[0].max(z[1]).max(z[2]));
            let mut k = ((zmin / interval).floor() as i64).max(k0);
            while (k as f64) * interval < zmax {
                let level = k as f64 * interval;
                let above = z.map(|v| v > level);
                let mut ends = vec![];
                for e in 0..3 {
                    let (a, b) = (e, (e + 1) % 3);
                    if above[a] != above[b] {
                        let (va, vb) = (t[a], t[b]);
                        ends.push((va.min(vb), va.max(vb)));
                    }
                }
                if ends.len() == 2 {
                    segs.entry(k).or_default().push([ends[0], ends[1]]);
                }
                k += 1;
            }
        }
        let mut levels: Vec<i64> = segs.keys().copied().collect();
        levels.sort_unstable();
        let mut out = vec![];
        for k in levels {
            let level = k as f64 * interval;
            let at = |e: (u32, u32)| {
                let (a, b) = (self.pts[e.0 as usize], self.pts[e.1 as usize]);
                let s = ((level - a.z) / (b.z - a.z)).clamp(0.0, 1.0);
                v3(a.x + (b.x - a.x) * s, a.y + (b.y - a.y) * s, level)
            };
            let major = major_every > 0 && k.rem_euclid(major_every as i64) == 0;
            for (pts, closed) in chain(&segs[&k]) {
                let mut p: Vec<V3> = pts.into_iter().map(at).collect();
                p.dedup_by(|a, b| (a.x - b.x).abs() < 1e-12 && (a.y - b.y).abs() < 1e-12);
                if p.len() >= 2 {
                    out.push(Contour {
                        z: level,
                        major,
                        pts: p,
                        closed,
                    });
                }
            }
        }
        out
    }

    /// Profile along segment a→b: pieces (s from a, z) where the segment lies over the surface.
    pub fn profile(&self, a: V2, b: V2) -> Vec<Vec<V2>> {
        let d = b - a;
        let len = d.len();
        if !(len > 0.0) {
            return vec![];
        }
        let lo = v2(a.x.min(b.x), a.y.min(b.y));
        let hi = v2(a.x.max(b.x), a.y.max(b.y));
        let mut samples: Vec<(f64, Option<f64>)> = vec![(0.0, self.z_at(a)), (1.0, self.z_at(b))];
        let mut done: HashSet<(u32, u32)> = HashSet::new();
        for t in self.index.rect(lo, hi) {
            let tr = self.tris[t as usize];
            for k in 0..3 {
                let (u, v) = (tr[k], tr[(k + 1) % 3]);
                if !done.insert((u.min(v), u.max(v))) {
                    continue;
                }
                let (pu, pv) = (self.pts[u as usize], self.pts[v as usize]);
                let f = xy(pv) - xy(pu);
                let den = d.cross(f);
                if den == 0.0 {
                    continue;
                }
                let w = xy(pu) - a;
                let (tt, s) = (w.cross(f) / den, w.cross(d) / den);
                if (0.0..=1.0).contains(&tt) && (0.0..=1.0).contains(&s) {
                    samples.push((tt, Some(pu.z + (pv.z - pu.z) * s)));
                }
            }
        }
        samples.sort_by(|x, y| x.0.total_cmp(&y.0));
        // coinciding parameters: a known elevation takes precedence over an empty one
        let mut uniq: Vec<(f64, Option<f64>)> = vec![];
        for s in samples {
            match uniq.last_mut() {
                Some(l) if (s.0 - l.0).abs() * len < 1e-9 => {
                    if l.1.is_none() {
                        l.1 = s.1;
                    }
                }
                _ => uniq.push(s),
            }
        }
        let mut pieces: Vec<Vec<V2>> = vec![];
        let mut cur: Vec<V2> = vec![];
        for w in 0..uniq.len() {
            let (t, z) = uniq[w];
            let Some(z) = z else {
                if cur.len() >= 2 {
                    pieces.push(std::mem::take(&mut cur));
                }
                cur.clear();
                continue;
            };
            if let Some(&(tp, Some(_))) = w.checked_sub(1).map(|i| &uniq[i]) {
                let mid = a + d * (0.5 * (t + tp));
                if self.z_at(mid).is_none() {
                    if cur.len() >= 2 {
                        pieces.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                }
            }
            cur.push(v2(t * len, z));
        }
        if cur.len() >= 2 {
            pieces.push(cur);
        }
        pieces
    }

    /// Section of a solid of thickness `thickness` along segment a→b: closed (s, z) loops, counter-clockwise.
    pub fn poche(&self, a: V2, b: V2, thickness: f64) -> Vec<Vec<V2>> {
        self.profile(a, b)
            .into_iter()
            .map(|top| {
                let mut r: Vec<V2> = top.iter().map(|p| v2(p.x, p.y - thickness)).collect();
                r.extend(top.iter().rev().copied());
                r
            })
            .collect()
    }

    /// Cut and fill relative to the horizontal plane z0.
    pub fn volume_to_plane(&self, z0: f64) -> CutFill {
        let mut cf = CutFill::default();
        for t in &self.tris {
            let f = t.map(|i| self.pts[i as usize].z - z0);
            let (p, n) = pos_neg(f, self.tri_area(t));
            cf.cut += p;
            cf.fill += n;
        }
        cf
    }

    /// Closed solid: the top is the surface, the bottom is `thickness` below, side faces follow the boundary.
    /// `face_of`: 0 is top, 1 is bottom, 2 is sides.
    pub fn solid(&self, thickness: f64) -> Mesh {
        let all: Vec<usize> = (0..self.tris.len()).collect();
        self.solid_part(&all, thickness)
    }

    /// Closed solid from a subset of triangles `which` (a single-material piece, a pad).
    pub fn solid_part(&self, which: &[usize], thickness: f64) -> Mesh {
        let top = self.top_mesh(which);
        let n = top.positions.len() as u32;
        let mut m = Mesh {
            positions: top
                .positions
                .iter()
                .copied()
                .chain(top.positions.iter().map(|p| v3(p.x, p.y, p.z - thickness)))
                .collect(),
            triangles: top.triangles.clone(),
            face_of: vec![0; top.triangles.len()],
        };
        for t in &top.triangles {
            m.triangles.push([t[0] + n, t[2] + n, t[1] + n]);
            m.face_of.push(1);
        }
        let dir: HashSet<(u32, u32)> = top
            .triangles
            .iter()
            .flat_map(|t| (0..3).map(move |k| (t[k], t[(k + 1) % 3])))
            .collect();
        let mut edges: Vec<(u32, u32)> = dir
            .iter()
            .copied()
            .filter(|&(a, b)| !dir.contains(&(b, a)))
            .collect();
        edges.sort_unstable();
        for (a, b) in edges {
            m.triangles.push([a, a + n, b + n]);
            m.triangles.push([a, b + n, b]);
            m.face_of.extend([2, 2]);
        }
        m
    }

    /// Solid top: triangles `which` (indices) as a separate mesh without unused vertices.
    pub fn top_mesh(&self, which: &[usize]) -> Mesh {
        let mut map = vec![NONE; self.pts.len()];
        let mut m = Mesh::default();
        for &t in which {
            let mut nt = [0u32; 3];
            for (k, &v) in self.tris[t].iter().enumerate() {
                if map[v as usize] == NONE {
                    map[v as usize] = m.positions.len() as u32;
                    m.positions.push(self.pts[v as usize]);
                }
                nt[k] = map[v as usize];
            }
            m.triangles.push(nt);
            m.face_of.push(0);
        }
        m
    }
}

/// Chains segments sharing edge keys into polylines: (keys in order, closed).
fn chain(segs: &[[(u32, u32); 2]]) -> Vec<(Vec<(u32, u32)>, bool)> {
    let mut by: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, s) in segs.iter().enumerate() {
        by.entry(s[0]).or_default().push(i);
        by.entry(s[1]).or_default().push(i);
    }
    let mut used = vec![false; segs.len()];
    let mut out = vec![];
    let walk = |start: usize, from: (u32, u32), used: &mut Vec<bool>| {
        let mut keys = vec![from];
        let mut cur = start;
        let mut at = from;
        loop {
            used[cur] = true;
            let s = segs[cur];
            let other = if s[0] == at { s[1] } else { s[0] };
            keys.push(other);
            at = other;
            let Some(&nx) = by[&other].iter().find(|&&j| !used[j]) else {
                break;
            };
            cur = nx;
        }
        keys
    };
    // open ones first (a polyline end is the key of a single segment), then closed ones
    let mut open: Vec<(u32, u32)> = by
        .iter()
        .filter(|(_, v)| v.len() == 1)
        .map(|(k, _)| *k)
        .collect();
    open.sort_unstable();
    for k in open {
        let i = by[&k][0];
        if used[i] {
            continue;
        }
        out.push((walk(i, k, &mut used), false));
    }
    for i in 0..segs.len() {
        if used[i] {
            continue;
        }
        let mut keys = walk(i, segs[i][0], &mut used);
        let closed = keys.len() > 2 && keys.first() == keys.last();
        if closed {
            keys.pop();
        }
        out.push((keys, closed));
    }
    out
}

/// Contour label positions every `spacing` m: (point, text angle in rad; text is never upside down).
pub fn label_spots(c: &Contour, spacing: f64) -> Vec<(V2, f64)> {
    let mut out = vec![];
    if !(spacing > 0.0) || c.pts.len() < 2 {
        return out;
    }
    let mut segs: Vec<(V2, V2)> = c.pts.windows(2).map(|w| (xy(w[0]), xy(w[1]))).collect();
    if c.closed {
        segs.push((xy(c.pts[c.pts.len() - 1]), xy(c.pts[0])));
    }
    let total: f64 = segs.iter().map(|(a, b)| (*b - *a).len()).sum();
    if total < spacing * 0.5 {
        return out;
    }
    let mut next = if total < spacing {
        total * 0.5
    } else {
        spacing * 0.5
    };
    let mut run = 0.0;
    for (a, b) in segs {
        let l = (b - a).len();
        while l > 0.0 && next <= run + l {
            let t = (next - run) / l;
            let mut ang = (b.y - a.y).atan2(b.x - a.x);
            if ang > std::f64::consts::FRAC_PI_2 {
                ang -= std::f64::consts::PI;
            } else if ang < -std::f64::consts::FRAC_PI_2 {
                ang += std::f64::consts::PI;
            }
            out.push((a + (b - a) * t, ang));
            next += spacing;
        }
        run += l;
    }
    out
}
