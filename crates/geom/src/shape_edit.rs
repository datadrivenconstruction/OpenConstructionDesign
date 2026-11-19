// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Slab shape editing: dz offsets of contour vertices, added points and split
//! lines (dz at the ends, linear along). The top is a piecewise-linear surface on a
//! conforming Delaunay triangulation: contour edges and split lines are forced to be edges
//! (missing ones are bisected, the midpoint dz taken from the line). Vertical thickness:
//! `Varying` keeps the bottom flat, V = A·t + Σ area·mean dz; `Constant` makes the bottom follow the top, V = A·t.
//! Also here: a flat roof from a slope arrow and a dormer cut-out in the roof.

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::predicates::point_in_ring;
use crate::roof::{LineKind, Roof, RoofFace, RoofLine};
use crate::slope::Plane;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Thickness {
    /// Flat bottom, varying thickness (screed sloping to drains).
    #[default]
    Varying,
    /// Bottom follows the top (a sheet of constant thickness).
    Constant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeLine {
    pub a: V2,
    pub b: V2,
    pub dza: f64,
    pub dzb: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlabShape {
    /// dz of each ring's vertices ([outer, holes…]); if shorter than the ring, the missing ones are 0.
    pub ring_dz: Vec<Vec<f64>>,
    pub points: Vec<(V2, f64)>,
    pub lines: Vec<ShapeLine>,
    pub mode: Thickness,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ShapeError {
    NoBoundary,
    /// Point or line outside the slab (index into points / lines).
    PointOutside(usize),
    LineOutside(usize),
    NotFinite,
    /// Conforming did not converge (angles too sharp).
    TooManyPoints,
    /// `Varying`: the top is pushed below the flat bottom (dz < −t) at vertex `at`.
    TooDeep {
        at: usize,
        dz: f64,
        t: f64,
    },
    /// One plan point given with different dz values (coincident points or crossing lines).
    Conflict {
        at: V2,
        a: f64,
        b: f64,
    },
}

/// Triangulated top: plan, dz, triangles (counter-clockwise).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeMesh {
    pub pts: Vec<V2>,
    pub dz: Vec<f64>,
    pub tris: Vec<[usize; 3]>,
    pub mode: Thickness,
}

const MAX_PTS: usize = 4000;

fn tri_area(a: V2, b: V2, c: V2) -> f64 {
    (b - a).cross(c - a) / 2.0
}

/// Delaunay (Bowyer–Watson) over points; returns counter-clockwise triangles.
fn delaunay(pts: &[V2]) -> Vec<[usize; 3]> {
    let n = pts.len();
    let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
    for p in pts {
        lo = v2(lo.x.min(p.x), lo.y.min(p.y));
        hi = v2(hi.x.max(p.x), hi.y.max(p.y));
    }
    let span = (hi.x - lo.x).max(hi.y - lo.y).max(1.0);
    let mid = (lo + hi) * 0.5;
    let mut all = pts.to_vec();
    // far vertices: with a nearby triangle, the edges of a nearly flat hull (round slab)
    // would drop out of the triangulation, and restoring them splits the contour up to MAX_PTS
    let far = 1e4 * span;
    all.push(mid + v2(-far, -far));
    all.push(mid + v2(far, -far));
    all.push(mid + v2(0.0, far));
    let mut tris: Vec<[usize; 3]> = vec![[n, n + 1, n + 2]];
    let in_circle = |t: &[usize; 3], p: V2, all: &[V2]| -> bool {
        let (a, b, c) = (all[t[0]] - p, all[t[1]] - p, all[t[2]] - p);
        let (d1, d2, d3) = (
            a.dot(a) * b.cross(c),
            b.dot(b) * a.cross(c),
            c.dot(c) * a.cross(b),
        );
        // relative tolerance: cocircular points do not break the cavity
        d1 - d2 + d3 > 1e-13 * (d1.abs() + d2.abs() + d3.abs())
    };
    for i in 0..n {
        let p = all[i];
        let (bad, keep): (Vec<[usize; 3]>, Vec<[usize; 3]>) =
            tris.into_iter().partition(|t| in_circle(t, p, &all));
        let mut edges: Vec<(usize, usize)> = vec![];
        for t in &bad {
            for k in 0..3 {
                let e = (t[k], t[(k + 1) % 3]);
                if let Some(j) = edges.iter().position(|&(a, b)| a == e.1 && b == e.0) {
                    edges.swap_remove(j);
                } else {
                    edges.push(e);
                }
            }
        }
        tris = keep;
        for (a, b) in edges {
            if tri_area(all[a], all[b], p).abs() > 0.0 {
                tris.push([a, b, i]);
            }
        }
    }
    tris.retain(|t| t.iter().all(|&k| k < n));
    tris
}

fn has_edge(tris: &[[usize; 3]], a: usize, b: usize) -> bool {
    tris.iter().any(|t| {
        (0..3).any(|k| (t[k] == a && t[(k + 1) % 3] == b) || (t[k] == b && t[(k + 1) % 3] == a))
    })
}

fn on_open_seg(a: V2, b: V2, p: V2) -> Option<f64> {
    let d = b - a;
    let l2 = d.dot(d);
    let t = (p - a).dot(d) / l2;
    let off = (p - a).cross(d).abs() / l2.sqrt();
    (t > 1e-9 && t < 1.0 - 1e-9 && off < 1e-9).then_some(t)
}

impl SlabShape {
    /// Slope to a drain: point `at` is lowered by `depth`, with split lines from it to the contour vertices.
    pub fn drain(outer: &[V2], at: V2, depth: f64) -> SlabShape {
        SlabShape {
            ring_dz: vec![],
            points: vec![(at, -depth)],
            lines: outer
                .iter()
                .map(|&v| ShapeLine {
                    a: at,
                    b: v,
                    dza: -depth,
                    dzb: 0.0,
                })
                .collect(),
            mode: Thickness::Varying,
        }
    }

    /// Drain with a given slope: depth = slope × distance to the farthest vertex.
    pub fn drain_slope(outer: &[V2], at: V2, slope: f64) -> SlabShape {
        let far = outer.iter().map(|v| (*v - at).len()).fold(0.0, f64::max);
        Self::drain(outer, at, slope * far)
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
            && self.lines.is_empty()
            && self.ring_dz.iter().all(|r| r.iter().all(|z| *z == 0.0))
    }

    pub fn build(&self, rings: &[Vec<V2>]) -> Result<ShapeMesh, ShapeError> {
        if rings.is_empty() || rings[0].len() < 3 {
            return Err(ShapeError::NoBoundary);
        }
        let inside = |p: V2| -> bool {
            point_in_ring(p, &rings[0]) >= 0 && rings[1..].iter().all(|h| point_in_ring(p, h) <= 0)
        };
        let fin = |v: f64| v.is_finite();
        let mut pts: Vec<V2> = vec![];
        let mut dz: Vec<f64> = vec![];
        // explicitly given (including 0): a repeat with a different dz is a conflict, not a silent replacement
        let mut set: Vec<bool> = vec![];
        let mut add = |p: V2,
                       z: f64,
                       explicit: bool,
                       pts: &mut Vec<V2>,
                       dz: &mut Vec<f64>|
         -> Result<usize, ShapeError> {
            if let Some(i) = pts.iter().position(|q| (*q - p).len() < 1e-9) {
                if explicit {
                    if set[i] && (dz[i] - z).abs() > 1e-9 {
                        return Err(ShapeError::Conflict {
                            at: p,
                            a: dz[i],
                            b: z,
                        });
                    }
                    dz[i] = z;
                    set[i] = true;
                }
                return Ok(i);
            }
            pts.push(p);
            dz.push(z);
            set.push(explicit);
            Ok(pts.len() - 1)
        };
        let mut cons: Vec<(usize, usize)> = vec![];
        for (r, ring) in rings.iter().enumerate() {
            let first = pts.len();
            for (k, &p) in ring.iter().enumerate() {
                let given = self.ring_dz.get(r).and_then(|v| v.get(k)).copied();
                let z = given.unwrap_or(0.0);
                if !fin(z) {
                    return Err(ShapeError::NotFinite);
                }
                add(p, z, given.is_some(), &mut pts, &mut dz)?;
            }
            let m = ring.len();
            for k in 0..m {
                cons.push((first + k, first + (k + 1) % m));
            }
        }
        for (i, &(p, z)) in self.points.iter().enumerate() {
            if !fin(z) || !p.x.is_finite() || !p.y.is_finite() {
                return Err(ShapeError::NotFinite);
            }
            if !inside(p) {
                return Err(ShapeError::PointOutside(i));
            }
            add(p, z, true, &mut pts, &mut dz)?;
        }
        for (i, l) in self.lines.iter().enumerate() {
            if ![l.dza, l.dzb, l.a.x, l.a.y, l.b.x, l.b.y]
                .iter()
                .all(|v| fin(*v))
            {
                return Err(ShapeError::NotFinite);
            }
            // whole segment: the midpoint of each piece between intersections with the contour and holes
            let ts = crate::poly::crossings(l.a, l.b, rings);
            let d = l.b - l.a;
            if !inside(l.a)
                || !inside(l.b)
                || ts
                    .windows(2)
                    .any(|w| !inside(l.a + d * (0.5 * (w[0] + w[1]))))
            {
                return Err(ShapeError::LineOutside(i));
            }
            let a = add(l.a, l.dza, true, &mut pts, &mut dz)?;
            let b = add(l.b, l.dzb, true, &mut pts, &mut dz)?;
            if a != b {
                cons.push((a, b));
            }
        }
        // split lines crossing each other produce shared vertices (dz taken from the first line)
        let mut k = 0;
        while k < cons.len() {
            let mut j = k + 1;
            while j < cons.len() {
                let (a, b) = cons[k];
                let (c, d) = cons[j];
                if [a, b].contains(&c) || [a, b].contains(&d) {
                    j += 1;
                    continue;
                }
                let (pa, pb, pc, pd) = (pts[a], pts[b], pts[c], pts[d]);
                let r = pb - pa;
                let s = pd - pc;
                let den = r.cross(s);
                if den.abs() > 1e-15 {
                    let t = (pc - pa).cross(s) / den;
                    let u = (pc - pa).cross(r) / den;
                    if t > 1e-9 && t < 1.0 - 1e-9 && u > 1e-9 && u < 1.0 - 1e-9 {
                        let z = dz[a] + (dz[b] - dz[a]) * t;
                        let z2 = dz[c] + (dz[d] - dz[c]) * u;
                        if (z - z2).abs() > 1e-9 {
                            return Err(ShapeError::Conflict {
                                at: pa + r * t,
                                a: z,
                                b: z2,
                            });
                        }
                        let x = add(pa + r * t, z, true, &mut pts, &mut dz)?;
                        cons[k] = (a, x);
                        cons.push((x, b));
                        cons[j] = (c, x);
                        cons.push((x, d));
                    }
                }
                j += 1;
            }
            k += 1;
        }
        // points on constraints split them; then midpoint conforming
        loop {
            if pts.len() > MAX_PTS {
                return Err(ShapeError::TooManyPoints);
            }
            let mut changed = false;
            let mut next = vec![];
            for &(a, b) in &cons {
                let hit = (0..pts.len())
                    .filter(|&i| i != a && i != b)
                    .find_map(|i| on_open_seg(pts[a], pts[b], pts[i]).map(|t| (i, t)));
                if let Some((i, _)) = hit {
                    next.push((a, i));
                    next.push((i, b));
                    changed = true;
                } else {
                    next.push((a, b));
                }
            }
            cons = next;
            if changed {
                continue;
            }
            let tris = delaunay(&pts);
            let missing: Vec<(usize, usize)> = cons
                .iter()
                .copied()
                .filter(|&(a, b)| !has_edge(&tris, a, b))
                .collect();
            if missing.is_empty() {
                let tris: Vec<[usize; 3]> = tris
                    .into_iter()
                    .filter(|t| {
                        let c = (pts[t[0]] + pts[t[1]] + pts[t[2]]) * (1.0 / 3.0);
                        inside(c) && tri_area(pts[t[0]], pts[t[1]], pts[t[2]]) > 1e-14
                    })
                    .collect();
                return Ok(ShapeMesh {
                    pts,
                    dz,
                    tris,
                    mode: self.mode,
                });
            }
            for (a, b) in missing {
                let m = (pts[a] + pts[b]) * 0.5;
                let z = 0.5 * (dz[a] + dz[b]);
                pts.push(m);
                dz.push(z);
                let i = pts.len() - 1;
                let p = cons.iter().position(|&e| e == (a, b)).unwrap();
                cons[p] = (a, i);
                cons.push((i, b));
            }
        }
    }
}

impl ShapeMesh {
    /// `Varying`: the bottom is flat, so dz + t > 0 at all vertices (the drain is no deeper than the thickness).
    pub fn check_thickness(&self, t: f64) -> Result<(), ShapeError> {
        if self.mode == Thickness::Constant {
            return Ok(());
        }
        match self.dz.iter().enumerate().find(|(_, &z)| z <= -t + 1e-9) {
            Some((at, &dz)) => Err(ShapeError::TooDeep { at, dz, t }),
            None => Ok(()),
        }
    }
    fn v3(&self, i: usize, z: f64) -> V3 {
        v3(self.pts[i].x, self.pts[i].y, z + self.dz[i])
    }
    pub fn plan_area(&self) -> f64 {
        self.tris
            .iter()
            .map(|t| tri_area(self.pts[t[0]], self.pts[t[1]], self.pts[t[2]]))
            .sum()
    }
    /// True area of the top.
    pub fn top_area(&self) -> f64 {
        self.tris
            .iter()
            .map(|t| {
                let (a, b, c) = (self.v3(t[0], 0.0), self.v3(t[1], 0.0), self.v3(t[2], 0.0));
                (b - a).cross(c - a).len() / 2.0
            })
            .sum()
    }
    /// Volume at (vertical) thickness `t`; exact for a piecewise-linear top.
    pub fn volume(&self, t: f64) -> f64 {
        match self.mode {
            Thickness::Constant => self.plan_area() * t,
            Thickness::Varying => self
                .tris
                .iter()
                .map(|k| {
                    let a = tri_area(self.pts[k[0]], self.pts[k[1]], self.pts[k[2]]);
                    a * (t + (self.dz[k[0]] + self.dz[k[1]] + self.dz[k[2]]) / 3.0)
                })
                .sum(),
        }
    }
    /// Top dz at a plan point; None outside the slab.
    pub fn dz_at(&self, p: V2) -> Option<f64> {
        for t in &self.tris {
            let (a, b, c) = (self.pts[t[0]], self.pts[t[1]], self.pts[t[2]]);
            let ar = tri_area(a, b, c);
            let (wa, wb, wc) = (
                tri_area(p, b, c) / ar,
                tri_area(a, p, c) / ar,
                tri_area(a, b, p) / ar,
            );
            let e = -1e-9;
            if wa >= e && wb >= e && wc >= e {
                return Some(wa * self.dz[t[0]] + wb * self.dz[t[1]] + wc * self.dz[t[2]]);
            }
        }
        None
    }
    /// dz polyline along a→b: (u from a, dz) at kinks (intersections with triangle edges).
    pub fn along(&self, a: V2, b: V2) -> Vec<(f64, f64)> {
        let d = b - a;
        let len = d.len();
        let mut us = vec![0.0, len];
        for t in &self.tris {
            for k in 0..3 {
                let (p, q) = (self.pts[t[k]], self.pts[t[(k + 1) % 3]]);
                let s = q - p;
                let den = d.cross(s);
                if den.abs() < 1e-15 {
                    continue;
                }
                let tt = (p - a).cross(s) / den;
                let uu = (p - a).cross(d) / den;
                if (0.0..=1.0).contains(&tt) && (0.0..=1.0).contains(&uu) {
                    us.push(tt * len);
                }
            }
        }
        us.sort_by(f64::total_cmp);
        us.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
        us.into_iter()
            .filter_map(|u| self.dz_at(a + d * (u / len)).map(|z| (u, z)))
            .collect()
    }
    /// Body: top at `top_z + dz`, bottom flat (`Varying`) or top − t (`Constant`).
    pub fn mesh(&self, top_z: f64, t: f64) -> Mesh {
        let n = self.pts.len();
        let mut m = Mesh::default();
        for i in 0..n {
            m.positions.push(self.v3(i, top_z));
        }
        for i in 0..n {
            let z = match self.mode {
                Thickness::Varying => top_z - t,
                Thickness::Constant => top_z - t + self.dz[i],
            };
            m.positions.push(v3(self.pts[i].x, self.pts[i].y, z));
        }
        let mut count = std::collections::HashMap::new();
        for t in &self.tris {
            let [a, b, c] = t.map(|k| k as u32);
            m.triangles.push([a, b, c]);
            m.face_of.push(0);
            m.triangles.push([a + n as u32, c + n as u32, b + n as u32]);
            m.face_of.push(1);
            for k in 0..3 {
                let (p, q) = (t[k], t[(k + 1) % 3]);
                *count.entry((p.min(q), p.max(q))).or_insert(0) += 1;
            }
        }
        for t in &self.tris {
            for k in 0..3 {
                let (p, q) = (t[k], t[(k + 1) % 3]);
                if count[&(p.min(q), p.max(q))] == 1 {
                    let (p, q, nu) = (p as u32, q as u32, n as u32);
                    m.triangles.push([q, p, p + nu]);
                    m.triangles.push([q, p + nu, q + nu]);
                    m.face_of.extend([2, 2]);
                }
            }
        }
        m
    }
}

/// Flat roof from a slope arrow over a contour: edges do not set the slope, the arrow defines the plane.
/// The plane elevation at the arrow tail is `base_z`; end faces above the eave are vertical gables.
pub fn arrow_roof(ring: &[V2], plane: &Plane, base_z: f64) -> Roof {
    let mut r: Vec<V2> = ring.to_vec();
    if crate::poly::area(&r) < 0.0 {
        r.reverse();
    }
    let z = |p: V2| base_z + plane.rise(p);
    let eave_z = r.iter().map(|&p| z(p)).fold(f64::MAX, f64::min);
    let top: Vec<V3> = r.iter().map(|&p| v3(p.x, p.y, z(p))).collect();
    let mut faces = vec![RoofFace {
        pts: top.clone(),
        sloped: true,
        edge: 0,
        phase: 0,
    }];
    let mut lines = vec![];
    let n = r.len();
    for i in 0..n {
        let (a, b) = (top[i], top[(i + 1) % n]);
        let level = (a.z - eave_z).abs() < 1e-9 && (b.z - eave_z).abs() < 1e-9;
        lines.push(RoofLine {
            a,
            b,
            kind: if level {
                LineKind::Eave
            } else {
                LineKind::Rake
            },
        });
        if !level {
            let mut pts = vec![v3(a.x, a.y, eave_z), v3(b.x, b.y, eave_z), b, a];
            pts.dedup_by(|p, q| (*p - *q).len() < 1e-12);
            if (pts[0] - pts[pts.len() - 1]).len() < 1e-12 {
                pts.pop();
            }
            if pts.len() >= 3 {
                faces.push(RoofFace {
                    pts,
                    sloped: false,
                    edge: i,
                    phase: 0,
                });
            }
        }
    }
    Roof {
        faces,
        lines,
        eave_z,
    }
}

/// Dormer cut-out (convex plan `hole`) from the roof slopes it touches
/// (horizontal faces are not cut).
pub fn cut_dormer(roof: &Roof, hole: &[V2]) -> Roof {
    cut_roof(roof, hole, true)
}

/// Cut-out of convex plan `hole` from the roof faces it touches: `inclined_only` means
/// slopes only (dormer), otherwise horizontal faces too; gables are not cut.
/// A face minus a convex polygon = the union of pieces "outside edge i and inside edges < i".
pub fn cut_roof(roof: &Roof, hole: &[V2], inclined_only: bool) -> Roof {
    let mut h = hole.to_vec();
    if crate::poly::area(&h) < 0.0 {
        h.reverse();
    }
    let m = h.len();
    let bb = |pts: &mut dyn Iterator<Item = V2>| {
        pts.fold(
            (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN)),
            |(lo, hi), p| {
                (
                    v2(lo.x.min(p.x), lo.y.min(p.y)),
                    v2(hi.x.max(p.x), hi.y.max(p.y)),
                )
            },
        )
    };
    let (hlo, hhi) = bb(&mut h.iter().copied());
    let mut faces = vec![];
    for f in &roof.faces {
        let nrm = newell(&f.pts);
        let flat = nrm.x.hypot(nrm.y) <= 1e-9 * nrm.len();
        let (flo, fhi) = bb(&mut f.pts.iter().map(|p| v2(p.x, p.y)));
        let apart = fhi.x <= hlo.x || flo.x >= hhi.x || fhi.y <= hlo.y || flo.y >= hhi.y;
        if !f.sloped || nrm.z.abs() < 1e-12 || (inclined_only && flat) || apart {
            faces.push(f.clone());
            continue;
        }
        let p0 = f.pts[0];
        let z_at = |p: V2| p0.z - (nrm.x * (p.x - p0.x) + nrm.y * (p.y - p0.y)) / nrm.z;
        let mut plan: Vec<V2> = f.pts.iter().map(|p| v2(p.x, p.y)).collect();
        if crate::poly::area(&plan) < 0.0 {
            plan.reverse();
        }
        // non-convex face (valley of an L-shaped plan): half-plane clipping produces bridges, so cut
        // into convex pieces
        let pieces = if is_convex(&plan) {
            vec![plan]
        } else {
            crate::poly::convex_pieces(&plan)
        };
        let face = |pts: &[V2]| RoofFace {
            pts: pts.iter().map(|&p| v3(p.x, p.y, z_at(p))).collect(),
            sloped: true,
            edge: f.edge,
            phase: f.phase,
        };
        let whole = pieces.len() == 1;
        for pl in &pieces {
            if crate::poly::area(&crate::poly::clip_convex(pl, &h)).abs() <= 1e-12 {
                if whole {
                    faces.push(f.clone());
                } else {
                    faces.push(face(pl));
                }
                continue;
            }
            for i in 0..m {
                let (a, b) = (h[i], h[(i + 1) % m]);
                let d = b - a;
                let out_n = v2(d.y, -d.x); // outward for counter-clockwise winding
                                           // "outside edge i": n·p ≥ n·a ⇔ (−n)·p ≤ −n·a
                let mut piece = crate::poly::clip_half(pl, out_n * -1.0, -out_n.dot(a));
                for j in 0..i {
                    let (c, e) = (h[j], h[(j + 1) % m]);
                    let dj = e - c;
                    let nj = v2(dj.y, -dj.x);
                    piece = crate::poly::clip_half(&piece, nj, nj.dot(c));
                }
                piece.dedup_by(|x, y| (*x - *y).len() < 1e-12);
                if piece.len() >= 3 && crate::poly::area(&piece).abs() > 1e-12 {
                    faces.push(face(&piece));
                }
            }
        }
    }
    // pieces around the cut-out share edges: without welding, T-junctions and false body end faces appear
    crate::roof_shape::weld(&mut faces, 1e-7);
    Roof {
        faces,
        lines: roof.lines.clone(),
        eave_z: roof.eave_z,
    }
}

/// Convex contour (collinear vertices allowed).
pub(crate) fn is_convex(r: &[V2]) -> bool {
    let n = r.len();
    let mut sign = 0.0;
    for i in 0..n {
        let (a, b, c) = (r[i], r[(i + 1) % n], r[(i + 2) % n]);
        let k = (b - a).cross(c - b);
        if k.abs() <= 1e-12 * (b - a).len().max(1.0) * (c - b).len().max(1.0) {
            continue;
        }
        if sign == 0.0 {
            sign = k.signum();
        } else if k.signum() != sign {
            return false;
        }
    }
    true
}

fn newell(p: &[V3]) -> V3 {
    let mut n = v3(0.0, 0.0, 0.0);
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        n = n + v3(
            (a.y - b.y) * (a.z + b.z),
            (a.z - b.z) * (a.x + b.x),
            (a.x - b.x) * (a.y + b.y),
        );
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roof::Footprint;

    #[test]
    fn round_slab_keeps_its_boundary_points() {
        let ring: Vec<V2> = (0..96)
            .map(|k| {
                let a = k as f64 * std::f64::consts::TAU / 96.0;
                v2(4.0 + 2.213 * a.cos(), 3.0 + 2.213 * a.sin())
            })
            .collect();
        let shape = SlabShape {
            ring_dz: vec![],
            points: vec![(v2(3.5158, 2.3664), -0.07)],
            lines: vec![],
            mode: Thickness::Varying,
        };
        let m = shape.build(&[ring]).unwrap();
        assert_eq!(m.pts.len(), 97);
        assert_eq!(m.tris.len(), 96);
    }

    fn sq(s: f64) -> Vec<V2> {
        vec![v2(0.0, 0.0), v2(s, 0.0), v2(s, s), v2(0.0, s)]
    }

    #[test]
    fn drain_at_center_exact_volume_and_area() {
        // 10×10, t = 0.3, drain at the centre by 0.06: V = 30 − 0.06·100/3 = 28
        let ring = sq(10.0);
        let sh = SlabShape::drain(&ring, v2(5.0, 5.0), 0.06);
        let m = sh.build(&[ring.clone()]).unwrap();
        assert!((m.plan_area() - 100.0).abs() < 1e-12);
        assert!((m.volume(0.3) - 28.0).abs() < 1e-12, "{}", m.volume(0.3));
        let want = 20.0 * (25.0f64 + 0.0036).sqrt();
        assert!((m.top_area() - want).abs() < 1e-12);
        assert!((m.dz_at(v2(5.0, 2.5)).unwrap() + 0.03).abs() < 1e-12);
        // constant thickness: the volume does not change
        let mut c = sh.clone();
        c.mode = Thickness::Constant;
        assert!((c.build(&[ring.clone()]).unwrap().volume(0.3) - 30.0).abs() < 1e-12);
        // the body mesh is closed and its volume equals the exact one
        let mesh = m.mesh(0.0, 0.3);
        assert!(mesh.is_watertight());
        assert!((mesh.volume() - 28.0).abs() < 1e-9, "{}", mesh.volume());
        // 2 % slope to the drain: depth = 0.02·√50
        let s2 = SlabShape::drain_slope(&ring, v2(5.0, 5.0), 0.02);
        assert!((s2.points[0].1 + 0.02 * 50f64.sqrt()).abs() < 1e-15);
    }

    #[test]
    fn raised_corner_and_split_line_ridge() {
        // 8×4: the split line at y=2 raised by 0.1 (ridge), edges 0 → V = 32·0.2 + 0.1·32/2
        let ring = vec![v2(0.0, 0.0), v2(8.0, 0.0), v2(8.0, 4.0), v2(0.0, 4.0)];
        let sh = SlabShape {
            lines: vec![ShapeLine {
                a: v2(0.0, 2.0),
                b: v2(8.0, 2.0),
                dza: 0.1,
                dzb: 0.1,
            }],
            ..Default::default()
        };
        let m = sh.build(&[ring.clone()]).unwrap();
        assert!(
            (m.volume(0.2) - (6.4 + 1.6)).abs() < 1e-12,
            "{}",
            m.volume(0.2)
        );
        assert!((m.dz_at(v2(3.3, 2.0)).unwrap() - 0.1).abs() < 1e-12);
        assert!((m.dz_at(v2(3.3, 1.0)).unwrap() - 0.05).abs() < 1e-12);
        // raised corner: V = A·t + dz·(corner star area)/3; checked against the mesh
        let sh = SlabShape {
            ring_dz: vec![vec![0.0, 0.0, 0.3, 0.0]],
            ..Default::default()
        };
        let m = sh.build(&[ring.clone()]).unwrap();
        let mesh = m.mesh(0.0, 0.2);
        assert!((m.volume(0.2) - mesh.volume()).abs() < 1e-9);
        // along the wall at the raised corner
        let al = m.along(v2(8.0, 0.0), v2(8.0, 4.0));
        assert_eq!(al.first().unwrap().1, 0.0);
        assert!((al.last().unwrap().1 - 0.3).abs() < 1e-12);
        // a point outside is an error
        let bad = SlabShape {
            points: vec![(v2(9.0, 1.0), 0.1)],
            ..Default::default()
        };
        assert_eq!(bad.build(&[ring]), Err(ShapeError::PointOutside(0)));
    }

    #[test]
    fn hole_is_respected() {
        let ring = sq(6.0);
        let hole = vec![v2(2.0, 2.0), v2(2.0, 4.0), v2(4.0, 4.0), v2(4.0, 2.0)];
        let sh = SlabShape {
            points: vec![(v2(1.0, 1.0), -0.05)],
            ..Default::default()
        };
        let m = sh.build(&[ring, hole]).unwrap();
        assert!((m.plan_area() - 32.0).abs() < 1e-12);
        assert!(m.dz_at(v2(3.0, 3.0)).is_none());
    }

    #[test]
    fn arrow_roof_and_dormer_cut_exact() {
        // 10×6 mono-pitch from an arrow: rise 1.5 over 6 m
        let ring = vec![v2(0.0, 0.0), v2(10.0, 0.0), v2(10.0, 6.0), v2(0.0, 6.0)];
        let pl = Plane::arrow(v2(5.0, 0.0), v2(5.0, 6.0), 1.5).unwrap();
        let r = arrow_roof(&ring, &pl, 3.0);
        let q = r.quantities();
        let k = (1.0f64 + 0.25 * 0.25).sqrt();
        assert!((q.plan_area - 60.0).abs() < 1e-12);
        assert!((q.area - 60.0 * k).abs() < 1e-12);
        assert!((q.volume_under - 60.0 * 0.75).abs() < 1e-12);
        assert!((q.gable_area - (2.0 * 4.5 + 15.0)).abs() < 1e-12);
        assert_eq!(r.top_at(v2(1.0, 4.0)), Some(4.0));
        // a 2×1.5 dormer cuts 3 m² of the plan
        let cut = cut_dormer(
            &r,
            &[v2(4.0, 2.0), v2(6.0, 2.0), v2(6.0, 3.5), v2(4.0, 3.5)],
        );
        let qc = cut.quantities();
        assert!((qc.plan_area - 57.0).abs() < 1e-12, "{}", qc.plan_area);
        assert!((qc.area - 57.0 * k).abs() < 1e-12);
        assert!(cut.top_at(v2(5.0, 3.0)).is_none());
        assert_eq!(cut.top_at(v2(5.0, 1.0)), Some(3.25));
    }

    #[test]
    fn dormer_on_hip_roof_and_wall_under_hip() {
        // 12×8 hip roof, 30°: a 2×2 cut-out in the south slope
        let fp = Footprint {
            ring: vec![v2(0.0, 0.0), v2(12.0, 0.0), v2(12.0, 8.0), v2(0.0, 8.0)],
            slopes: vec![Some(30.0); 4],
            base_z: 3.0,
            overhang: 0.0,
            mansard: None,
        };
        let r = fp.build().unwrap();
        let q0 = r.quantities();
        let cut = cut_dormer(
            &r,
            &[v2(5.0, 1.0), v2(7.0, 1.0), v2(7.0, 3.0), v2(5.0, 3.0)],
        );
        let q1 = cut.quantities();
        let k = 1.0 / 30f64.to_radians().cos();
        assert!((q0.plan_area - q1.plan_area - 4.0).abs() < 1e-9);
        assert!((q0.area - q1.area - 4.0 * k).abs() < 1e-9);
        // hip top height at points: z = 3 + tan30·distance to the nearest eave
        let t = 30f64.to_radians().tan();
        for (p, d) in [
            (v2(6.0, 2.0), 2.0),
            (v2(1.0, 4.0), 1.0),
            (v2(6.0, 4.0), 4.0),
        ] {
            assert!((r.top_at(p).unwrap() - (3.0 + t * d)).abs() < 1e-9);
        }
    }

    #[test]
    fn explicit_zero_conflicts_and_line_through_hole() {
        let ring = sq(10.0);
        let l = |a: V2, b: V2, dza: f64, dzb: f64| ShapeLine { a, b, dza, dzb };
        // a point given as 0 explicitly, then −0.05, is a conflict; identical values are allowed
        let sh = SlabShape {
            points: vec![(v2(5.0, 5.0), 0.0), (v2(5.0, 5.0), -0.05)],
            ..Default::default()
        };
        assert!(matches!(
            sh.build(&[ring.clone()]),
            Err(ShapeError::Conflict { .. })
        ));
        // lines with different dz crossing at a common point
        let x = SlabShape {
            lines: vec![
                l(v2(1.0, 5.0), v2(9.0, 5.0), 0.1, 0.1),
                l(v2(5.0, 1.0), v2(5.0, 9.0), 0.0, 0.0),
            ],
            ..Default::default()
        };
        assert!(matches!(
            x.build(&[ring.clone()]),
            Err(ShapeError::Conflict { .. })
        ));
        // a line whose ends and midpoint are inside but which passes through a hole
        let hole = vec![v2(2.0, 4.0), v2(2.0, 6.0), v2(3.0, 6.0), v2(3.0, 4.0)];
        let through = SlabShape {
            lines: vec![l(v2(1.0, 5.0), v2(9.0, 5.0), 0.0, 0.0)],
            ..Default::default()
        };
        assert_eq!(
            through.build(&[ring.clone(), hole]),
            Err(ShapeError::LineOutside(0))
        );
        // drain deeper than the thickness with a flat bottom
        let deep = SlabShape::drain(&ring, v2(5.0, 5.0), 0.4);
        let m = deep.build(&[ring.clone()]).unwrap();
        assert!(matches!(
            m.check_thickness(0.3),
            Err(ShapeError::TooDeep { .. })
        ));
        assert!(m.check_thickness(0.5).is_ok());
    }
}
