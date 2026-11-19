// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Boolean operations on closed polyhedra (forms, in-place bodies).
//!
//! A body is a set of convex planar faces with tags (`tag` is a stable face name used for
//! painting, push-pull and element binding: [`ftag`] = form-history operation + face index
//! within it). Splitting uses a BSP tree over face planes (the splitter is chosen by the number
//! of cuts); the side of a point is decided with an explicit `Tol::length` tolerance, and inside the
//! uncertainty band the sign comes from the exact `orient3d` predicate (coincident faces and touching
//! are "on the plane"). The result is welded: nearby vertices merge, T-junctions are inserted into
//! neighbouring edges. Validation: every edge is used exactly twice in opposite directions, coordinates are finite.

use std::collections::{BTreeMap, HashMap};

use crate::math::{v3, Tol, V3};
use crate::mesh::Mesh;
use crate::predicates::orient3d;

/// Bits reserved for the face index within an operation.
pub const LOCAL_BITS: u32 = 12;

/// Face tag: history operation `op` (from 1) and face index `local` within it (< 4096).
pub fn ftag(op: u32, local: u32) -> u32 {
    (op << LOCAL_BITS) | (local & ((1 << LOCAL_BITS) - 1))
}

/// The operation that created the tagged face.
pub fn tag_op(tag: u32) -> u32 {
    tag >> LOCAL_BITS
}

/// Plane `n·p = w` and the three points defining it (for the exact predicate).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub n: V3,
    pub w: f64,
    pub p: [V3; 3],
}

impl Plane {
    /// Plane of a convex polygon that is counter-clockwise when viewed from outside.
    pub fn of(pts: &[V3]) -> Option<Plane> {
        let mut n = v3(0.0, 0.0, 0.0);
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            n = n + v3(
                (a.y - b.y) * (a.z + b.z),
                (a.z - b.z) * (a.x + b.x),
                (a.x - b.x) * (a.y + b.y),
            );
        }
        let len = n.len();
        if !(len > 1e-300) || !len.is_finite() {
            return None;
        }
        let n = n * (1.0 / len);
        let p0 = pts[0];
        let p1 = *pts
            .iter()
            .max_by(|a, b| (**a - p0).len().total_cmp(&(**b - p0).len()))?;
        let e = p1 - p0;
        let p2 = *pts.iter().max_by(|a, b| {
            let ka = n.dot(e.cross(**a - p0));
            let kb = n.dot(e.cross(**b - p0));
            ka.total_cmp(&kb)
        })?;
        let c = pts.iter().fold(v3(0., 0., 0.), |s, q| s + *q) * (1.0 / pts.len() as f64);
        Some(Plane {
            n,
            w: n.dot(c),
            p: [p0, p1, p2],
        })
    }

    pub fn flipped(self) -> Plane {
        Plane {
            n: -self.n,
            w: -self.w,
            p: [self.p[0], self.p[2], self.p[1]],
        }
    }

    pub fn dist(&self, q: V3) -> f64 {
        self.n.dot(q) - self.w
    }

    /// -1 behind, 0 on the plane (exactly or within tolerance), 1 in front.
    fn side(&self, q: V3, tol: f64) -> i8 {
        let d = self.dist(q);
        if d.abs() < tol {
            return 0;
        }
        // exact orientation from the defining points confirms the sign near the tolerance boundary
        if d.abs() < 4.0 * tol {
            // orient3d > 0: q below the ccw triangle p0,p1,p2, i.e. behind the normal
            let o = orient3d(self.p[0], self.p[1], self.p[2], q);
            return if o == 0.0 {
                0
            } else if o < 0.0 {
                1
            } else {
                -1
            };
        }
        if d > 0.0 {
            1
        } else {
            -1
        }
    }
}

/// Convex planar face of a body.
#[derive(Clone, Debug, PartialEq)]
pub struct Poly {
    pub pts: Vec<V3>,
    pub plane: Plane,
    pub tag: u32,
}

impl Poly {
    pub fn new(pts: Vec<V3>, tag: u32) -> Option<Poly> {
        let plane = Plane::of(&pts)?;
        Some(Poly { pts, plane, tag })
    }

    fn flip(&mut self) {
        self.pts.reverse();
        self.plane = self.plane.flipped();
    }

    pub fn area(&self) -> f64 {
        let mut s = v3(0., 0., 0.);
        for i in 1..self.pts.len().saturating_sub(1) {
            s = s + (self.pts[i] - self.pts[0]).cross(self.pts[i + 1] - self.pts[0]);
        }
        s.len() * 0.5
    }
}

/// Closed body made of convex faces.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Body {
    pub polys: Vec<Poly>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    Union,
    Subtract,
    Intersect,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BodyError {
    /// Edge (from, to): forward and reverse uses are unequal.
    OpenEdge(V3, V3),
    /// The edge occurs more than twice (bodies touch along an edge).
    NonManifold(V3, V3),
    /// NaN or infinity in the coordinates.
    NonFinite,
    Empty,
}

impl std::fmt::Display for BodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            BodyError::OpenEdge(a, b) => write!(f, "open edge {a:?}-{b:?}"),
            BodyError::NonManifold(a, b) => write!(f, "non-manifold edge {a:?}-{b:?}"),
            BodyError::NonFinite => f.write_str("non-finite coordinates"),
            BodyError::Empty => f.write_str("empty body"),
        }
    }
}

impl Body {
    pub fn is_empty(&self) -> bool {
        self.polys.is_empty()
    }

    pub fn volume(&self) -> f64 {
        let mut six = 0.0;
        for p in &self.polys {
            for i in 1..p.pts.len() - 1 {
                six += p.pts[0].dot(p.pts[i].cross(p.pts[i + 1]));
            }
        }
        six / 6.0
    }

    pub fn area(&self) -> f64 {
        self.polys.iter().map(Poly::area).sum()
    }

    /// Area of the faces with the given tag.
    pub fn tag_area(&self, tag: u32) -> f64 {
        self.polys
            .iter()
            .filter(|p| p.tag == tag)
            .map(Poly::area)
            .sum()
    }

    pub fn max_tag(&self) -> u32 {
        self.polys.iter().map(|p| p.tag).max().unwrap_or(0)
    }

    pub fn tags(&self) -> Vec<u32> {
        let mut t: Vec<u32> = self.polys.iter().map(|p| p.tag).collect();
        t.sort_unstable();
        t.dedup();
        t
    }

    pub fn bbox(&self) -> Option<(V3, V3)> {
        let mut it = self.polys.iter().flat_map(|p| p.pts.iter());
        let f = *it.next()?;
        Some(it.fold((f, f), |(lo, hi), q| {
            (
                v3(lo.x.min(q.x), lo.y.min(q.y), lo.z.min(q.z)),
                v3(hi.x.max(q.x), hi.y.max(q.y), hi.z.max(q.z)),
            )
        }))
    }

    pub fn translated(mut self, d: V3) -> Body {
        for p in &mut self.polys {
            p.pts.iter_mut().for_each(|q| *q = *q + d);
            p.plane.w += p.plane.n.dot(d);
            p.plane.p.iter_mut().for_each(|q| *q = *q + d);
        }
        self
    }

    pub fn flipped(mut self) -> Body {
        self.polys.iter_mut().for_each(Poly::flip);
        self
    }

    /// All tags are shifted by `k` (so the tags of two operands do not collide).
    pub fn retagged(mut self, k: u32) -> Body {
        self.polys.iter_mut().for_each(|p| p.tag += k);
        self
    }

    /// Primitive-local tags (0, 1, 2+i) → tags of operation `op` ([`ftag`]).
    pub fn tagged(mut self, op: u32) -> Body {
        self.polys.iter_mut().for_each(|p| p.tag = ftag(op, p.tag));
        self
    }

    /// Body from a closed triangle mesh (all faces share one tag).
    pub fn from_mesh(m: &Mesh, tag: u32) -> Body {
        let polys = m
            .triangles
            .iter()
            .filter_map(|t| {
                let pts = vec![
                    m.positions[t[0] as usize],
                    m.positions[t[1] as usize],
                    m.positions[t[2] as usize],
                ];
                Poly::new(pts, tag)
            })
            .collect();
        Body { polys }
    }

    /// Generalized winding number of the surface around a point (1 inside, 0 outside).
    pub fn winding(&self, q: V3) -> f64 {
        let mut w = 0.0;
        for p in &self.polys {
            for i in 1..p.pts.len().saturating_sub(1) {
                let (a, b, c) = (p.pts[0] - q, p.pts[i] - q, p.pts[i + 1] - q);
                let (la, lb, lc) = (a.len(), b.len(), c.len());
                let num = a.dot(b.cross(c));
                let den = la * lb * lc + a.dot(b) * lc + a.dot(c) * lb + b.dot(c) * la;
                w += 2.0 * num.atan2(den);
            }
        }
        w / (4.0 * std::f64::consts::PI)
    }

    /// Point inside the body (by winding number).
    pub fn contains(&self, q: V3) -> bool {
        self.winding(q) > 0.5
    }

    /// Convexity: no vertex lies in front of any face plane.
    pub fn is_convex(&self, tol: f64) -> bool {
        self.polys.iter().all(|f| {
            self.polys
                .iter()
                .flat_map(|p| p.pts.iter())
                .all(|q| f.plane.dist(*q) <= tol)
        })
    }

    /// Plane of the tagged face if all its pieces are coplanar (otherwise it is a curved face).
    pub fn face_plane(&self, tag: u32, tol: f64) -> Option<Plane> {
        let first = self.polys.iter().find(|p| p.tag == tag)?.plane;
        let flat = self.polys.iter().filter(|p| p.tag == tag).all(|p| {
            p.plane.n.dot(first.n) > 1.0 - 1e-9 && p.pts.iter().all(|q| first.dist(*q).abs() <= tol)
        });
        flat.then_some(first)
    }

    pub fn boolean(&self, other: &Body, op: BoolOp, tol: Tol) -> Body {
        let out = boolean(self, other, op, tol.length);
        out.welded(tol.length)
    }

    pub fn union(&self, o: &Body, tol: Tol) -> Body {
        self.boolean(o, BoolOp::Union, tol)
    }
    pub fn subtract(&self, o: &Body, tol: Tol) -> Body {
        self.boolean(o, BoolOp::Subtract, tol)
    }
    pub fn intersect(&self, o: &Body, tol: Tol) -> Body {
        self.boolean(o, BoolOp::Intersect, tol)
    }

    /// Welding: vertices closer than the tolerance merge, points on edges are inserted (T-junctions),
    /// degenerate faces are discarded.
    pub fn welded(&self, tol: f64) -> Body {
        let mut w = Welder::new(tol.max(1e-12));
        let rings: Vec<(Vec<u32>, &Poly)> = self
            .polys
            .iter()
            .map(|p| {
                let mut r: Vec<u32> = p.pts.iter().map(|q| w.id(*q)).collect();
                r.dedup();
                while r.len() > 1 && r.first() == r.last() {
                    r.pop();
                }
                (r, p)
            })
            .filter(|(r, _)| r.len() >= 3)
            .collect();
        let grid = w.edge_grid();
        let mut polys = Vec::with_capacity(rings.len());
        for (r, p) in rings {
            let mut ids = Vec::with_capacity(r.len());
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                ids.push(a);
                ids.extend(grid.on_edge(&w.pts, a, b, w.tol));
            }
            let pts: Vec<V3> = ids.iter().map(|&i| w.pts[i as usize]).collect();
            if let Some(mut q) = Poly::new(pts, p.tag) {
                // the welded ring keeps the original orientation
                if q.plane.n.dot(p.plane.n) < 0.0 {
                    q.plane = p.plane;
                }
                polys.push(q);
            }
        }
        Body { polys }
    }

    fn finite(&self) -> bool {
        self.polys
            .iter()
            .flat_map(|p| p.pts.iter())
            .all(|q| q.x.is_finite() && q.y.is_finite() && q.z.is_finite())
    }

    /// Closedness: every edge has equal numbers of forward and reverse uses (edge-touching
    /// is allowed).
    pub fn check_closed(&self, tol: Tol) -> Result<(), BodyError> {
        self.closed_uses(tol).map(|_| ())
    }

    #[allow(clippy::type_complexity)]
    fn closed_uses(
        &self,
        tol: Tol,
    ) -> Result<(Vec<V3>, BTreeMap<(u32, u32), (i32, i32)>), BodyError> {
        if self.polys.is_empty() {
            return Err(BodyError::Empty);
        }
        if !self.finite() {
            return Err(BodyError::NonFinite);
        }
        let (pts, cnt) = self.edge_uses(tol);
        match cnt.iter().find(|(_, c)| c.0 != c.1) {
            None => Ok((pts, cnt)),
            Some(((a, b), _)) => Err(BodyError::OpenEdge(pts[*a as usize], pts[*b as usize])),
        }
    }

    /// Manifoldness: every edge has exactly one forward and one reverse use.
    pub fn validate(&self, tol: Tol) -> Result<(), BodyError> {
        let (pts, cnt) = self.closed_uses(tol)?;
        match cnt.iter().find(|(_, c)| c.0 != 1) {
            None => Ok(()),
            Some(((a, b), _)) => Err(BodyError::NonManifold(pts[*a as usize], pts[*b as usize])),
        }
    }

    /// Edge uses after welding and T-junction insertion: (a<b) → (forward, reverse).
    #[allow(clippy::type_complexity)]
    fn edge_uses(&self, tol: Tol) -> (Vec<V3>, BTreeMap<(u32, u32), (i32, i32)>) {
        let mut w = Welder::new(tol.length.max(1e-12));
        let mut cnt: BTreeMap<(u32, u32), (i32, i32)> = BTreeMap::new();
        let rings: Vec<Vec<u32>> = self
            .polys
            .iter()
            .map(|p| p.pts.iter().map(|q| w.id(*q)).collect())
            .collect();
        let grid = w.edge_grid();
        for r in &rings {
            let mut full = vec![];
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                full.push(a);
                full.extend(grid.on_edge(&w.pts, a, b, w.tol));
            }
            full.dedup();
            while full.len() > 1 && full.first() == full.last() {
                full.pop();
            }
            for i in 0..full.len() {
                let (a, b) = (full[i], full[(i + 1) % full.len()]);
                if a == b {
                    continue;
                }
                let e = cnt.entry((a.min(b), a.max(b))).or_default();
                if a < b {
                    e.0 += 1;
                } else {
                    e.1 += 1;
                }
            }
        }
        (w.pts, cnt)
    }

    /// Face rings after welding with T-junctions inserted (a mesh without dangling vertices).
    fn refined_rings(&self, tol: f64) -> (Vec<V3>, Vec<(Vec<u32>, usize)>) {
        let mut w = Welder::new(tol.max(1e-12));
        let rings: Vec<Vec<u32>> = self
            .polys
            .iter()
            .map(|p| p.pts.iter().map(|q| w.id(*q)).collect())
            .collect();
        let grid = w.edge_grid();
        let mut out = vec![];
        for (k, r) in rings.iter().enumerate() {
            let mut full = vec![];
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                full.push(a);
                full.extend(grid.on_edge(&w.pts, a, b, w.tol));
            }
            full.dedup();
            while full.len() > 1 && full.first() == full.last() {
                full.pop();
            }
            if full.len() >= 3 {
                out.push((full, k));
            }
        }
        (w.pts, out)
    }

    /// Welded points and one index ring per face (T-junction vertices inserted, so neighbours
    /// share every edge); rings with fewer than three distinct points are left out.
    pub fn polygon_rings(&self, tol: f64) -> (Vec<V3>, Vec<Vec<u32>>) {
        let (pts, rings) = self.refined_rings(tol);
        let rings = rings
            .into_iter()
            .map(|(mut r, _)| {
                r.dedup();
                while r.len() > 1 && r.first() == r.last() {
                    r.pop();
                }
                r
            })
            .filter(|r| r.len() >= 3)
            .collect();
        (pts, rings)
    }

    /// Triangle mesh for rendering and picking (shared vertices, T-junctions split by a fan from
    /// the face centre): `face_of` is the face tag.
    pub fn to_mesh(&self) -> Mesh {
        let tol = 1e-7;
        let (pts, rings) = self.refined_rings(tol);
        let mut m = Mesh {
            positions: pts,
            ..Default::default()
        };
        for (r, k) in rings {
            let tag = self.polys[k].tag;
            if r.len() == 3 {
                m.triangles.push([r[0], r[1], r[2]]);
                m.face_of.push(tag);
                continue;
            }
            let c = r
                .iter()
                .fold(v3(0., 0., 0.), |s, i| s + m.positions[*i as usize])
                * (1.0 / r.len() as f64);
            let ci = m.positions.len() as u32;
            m.positions.push(c);
            for i in 0..r.len() {
                m.triangles.push([ci, r[i], r[(i + 1) % r.len()]]);
                m.face_of.push(tag);
            }
        }
        m
    }

    /// View edges: boundaries between faces with different tags and creases (cos of angle < `cos_tol`).
    pub fn feature_edges(&self, tol: f64, cos_tol: f64) -> Vec<(V3, V3)> {
        let (pts, rings) = self.refined_rings(tol);
        let mut map: BTreeMap<(u32, u32), Vec<(V3, u32)>> = BTreeMap::new();
        for (r, k) in &rings {
            let p = &self.polys[*k];
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                if a != b {
                    map.entry((a.min(b), a.max(b)))
                        .or_default()
                        .push((p.plane.n, p.tag));
                }
            }
        }
        map.into_iter()
            .filter(|(_, ns)| match ns.as_slice() {
                [(n0, t0), rest @ ..] if !rest.is_empty() => rest
                    .iter()
                    .any(|(n, t)| (t != t0 && n0.dot(*n) < 1.0 - 1e-9) || n0.dot(*n) < cos_tol),
                _ => true,
            })
            .map(|((a, b), _)| (pts[a as usize], pts[b as usize]))
            .collect()
    }

    /// Outlines of the tagged face: outer edges of its pieces, assembled into rings.
    pub fn face_outline(&self, tag: u32, tol: f64) -> Vec<Vec<V3>> {
        let part = Body {
            polys: self
                .polys
                .iter()
                .filter(|p| p.tag == tag)
                .cloned()
                .collect(),
        }
        .welded(tol);
        let mut w = Welder::new(tol.max(1e-12));
        let mut dir: HashMap<(u32, u32), i32> = HashMap::new();
        for p in &part.polys {
            let r: Vec<u32> = p.pts.iter().map(|q| w.id(*q)).collect();
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                if a != b {
                    *dir.entry((a, b)).or_default() += 1;
                    *dir.entry((b, a)).or_default() -= 1;
                }
            }
        }
        let mut next: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut edges: Vec<_> = dir.into_iter().filter(|(_, c)| *c > 0).collect();
        edges.sort_unstable();
        for ((a, b), _) in edges {
            next.entry(a).or_default().push(b);
        }
        let mut starts: Vec<u32> = next.keys().copied().collect();
        starts.sort_unstable();
        let mut out = vec![];
        for s in starts {
            while next.get(&s).is_some_and(|v| !v.is_empty()) {
                let mut ring = vec![s];
                let mut cur = s;
                loop {
                    let Some(n) = next.get_mut(&cur).and_then(|v| v.pop()) else {
                        break;
                    };
                    if n == s {
                        break;
                    }
                    ring.push(n);
                    cur = n;
                    if ring.len() > 100_000 {
                        break;
                    }
                }
                let pts = drop_collinear(ring.iter().map(|&i| w.pts[i as usize]).collect(), tol);
                if pts.len() >= 3 {
                    out.push(pts);
                }
            }
        }
        out
    }
}

/// Removes vertices lying on the line between their neighbours.
pub fn drop_collinear(mut pts: Vec<V3>, tol: f64) -> Vec<V3> {
    let mut i = 0;
    while pts.len() > 3 && i < pts.len() {
        let n = pts.len();
        let (a, b, c) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
        let ac = c - a;
        let l = ac.len();
        if l < tol || (b - a).cross(ac).len() / l < tol {
            pts.remove(i);
            i = i.saturating_sub(1);
        } else {
            i += 1;
        }
    }
    pts
}

struct Welder {
    tol: f64,
    pts: Vec<V3>,
    cells: HashMap<(i64, i64, i64), Vec<u32>>,
}

impl Welder {
    fn new(tol: f64) -> Welder {
        Welder {
            tol,
            pts: vec![],
            cells: HashMap::new(),
        }
    }

    fn key(&self, q: V3) -> (i64, i64, i64) {
        let s = 4.0 * self.tol;
        (
            (q.x / s).floor() as i64,
            (q.y / s).floor() as i64,
            (q.z / s).floor() as i64,
        )
    }

    fn id(&mut self, q: V3) -> u32 {
        let (kx, ky, kz) = self.key(q);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(v) = self.cells.get(&(kx + dx, ky + dy, kz + dz)) {
                        for &i in v {
                            if (self.pts[i as usize] - q).len() <= self.tol {
                                return i;
                            }
                        }
                    }
                }
            }
        }
        let i = self.pts.len() as u32;
        self.pts.push(q);
        self.cells.entry((kx, ky, kz)).or_default().push(i);
        i
    }

    fn edge_grid(&self) -> EdgeGrid {
        EdgeGrid::new(&self.pts)
    }
}

/// Uniform vertex grid for finding points on edges.
struct EdgeGrid {
    lo: V3,
    cell: f64,
    cells: HashMap<(i64, i64, i64), Vec<u32>>,
}

impl EdgeGrid {
    fn new(pts: &[V3]) -> EdgeGrid {
        let mut lo = v3(f64::MAX, f64::MAX, f64::MAX);
        let mut hi = v3(f64::MIN, f64::MIN, f64::MIN);
        for q in pts {
            lo = v3(lo.x.min(q.x), lo.y.min(q.y), lo.z.min(q.z));
            hi = v3(hi.x.max(q.x), hi.y.max(q.y), hi.z.max(q.z));
        }
        let diag = if pts.is_empty() {
            1.0
        } else {
            (hi - lo).len().max(1e-9)
        };
        let cell = diag / (pts.len() as f64).cbrt().max(1.0);
        let mut g = EdgeGrid {
            lo,
            cell,
            cells: HashMap::new(),
        };
        for (i, q) in pts.iter().enumerate() {
            let k = g.key(*q);
            g.cells.entry(k).or_default().push(i as u32);
        }
        g
    }

    fn key(&self, q: V3) -> (i64, i64, i64) {
        let d = q - self.lo;
        (
            (d.x / self.cell).floor() as i64,
            (d.y / self.cell).floor() as i64,
            (d.z / self.cell).floor() as i64,
        )
    }

    /// Vertices strictly inside edge a→b (within tolerance), ordered from a.
    fn on_edge(&self, pts: &[V3], a: u32, b: u32, tol: f64) -> Vec<u32> {
        let (pa, pb) = (pts[a as usize], pts[b as usize]);
        let e = pb - pa;
        let l2 = e.dot(e);
        if l2 <= tol * tol {
            return vec![];
        }
        let lo = v3(
            pa.x.min(pb.x) - tol,
            pa.y.min(pb.y) - tol,
            pa.z.min(pb.z) - tol,
        );
        let hi = v3(
            pa.x.max(pb.x) + tol,
            pa.y.max(pb.y) + tol,
            pa.z.max(pb.z) + tol,
        );
        let (k0, k1) = (self.key(lo), self.key(hi));
        let mut found: Vec<(f64, u32)> = vec![];
        let span = (k1.0 - k0.0 + 1) * (k1.1 - k0.1 + 1) * (k1.2 - k0.2 + 1);
        let mut visit = |v: &Vec<u32>| {
            for &i in v {
                if i == a || i == b {
                    continue;
                }
                let q = pts[i as usize];
                let t = (q - pa).dot(e) / l2;
                if t <= 0.0 || t >= 1.0 {
                    continue;
                }
                let foot = pa + e * t;
                if (q - foot).len() <= tol && (q - pa).len() > tol && (q - pb).len() > tol {
                    found.push((t, i));
                }
            }
        };
        if span as usize > self.cells.len() {
            for v in self.cells.values() {
                visit(v);
            }
        } else {
            for x in k0.0..=k1.0 {
                for y in k0.1..=k1.1 {
                    for z in k0.2..=k1.2 {
                        if let Some(v) = self.cells.get(&(x, y, z)) {
                            visit(v);
                        }
                    }
                }
            }
        }
        found.sort_by(|p, q| p.0.total_cmp(&q.0).then(p.1.cmp(&q.1)));
        found.dedup_by_key(|p| p.1);
        found.into_iter().map(|p| p.1).collect()
    }
}

// ---------- BSP ----------

#[derive(Default)]
struct Node {
    plane: Option<Plane>,
    front: Option<usize>,
    back: Option<usize>,
    polys: Vec<Poly>,
}

struct Bsp {
    nodes: Vec<Node>,
    tol: f64,
}

#[derive(Default)]
struct Split {
    cf: Vec<Poly>,
    cb: Vec<Poly>,
    f: Vec<Poly>,
    b: Vec<Poly>,
}

fn split(plane: &Plane, p: Poly, tol: f64, out: &mut Split) {
    let sides: Vec<i8> = p.pts.iter().map(|q| plane.side(*q, tol)).collect();
    let any_f = sides.iter().any(|s| *s > 0);
    let any_b = sides.iter().any(|s| *s < 0);
    match (any_f, any_b) {
        (false, false) => {
            if plane.n.dot(p.plane.n) > 0.0 {
                out.cf.push(p)
            } else {
                out.cb.push(p)
            }
        }
        (true, false) => out.f.push(p),
        (false, true) => out.b.push(p),
        (true, true) => {
            let n = p.pts.len();
            let (mut f, mut b) = (vec![], vec![]);
            for i in 0..n {
                let j = (i + 1) % n;
                let (si, sj) = (sides[i], sides[j]);
                let (pi, pj) = (p.pts[i], p.pts[j]);
                if si >= 0 {
                    f.push(pi);
                }
                if si <= 0 {
                    b.push(pi);
                }
                if si * sj < 0 {
                    // canonical direction so both neighbours compute the same point
                    let (a, c) = if (pi.x, pi.y, pi.z) < (pj.x, pj.y, pj.z) {
                        (pi, pj)
                    } else {
                        (pj, pi)
                    };
                    let (da, dc) = (plane.dist(a), plane.dist(c));
                    let t = da / (da - dc);
                    let v = a + (c - a) * t;
                    f.push(v);
                    b.push(v);
                }
            }
            if f.len() >= 3 {
                out.f.push(Poly {
                    pts: f,
                    plane: p.plane,
                    tag: p.tag,
                });
            }
            if b.len() >= 3 {
                out.b.push(Poly {
                    pts: b,
                    plane: p.plane,
                    tag: p.tag,
                });
            }
        }
    }
}

/// Node splitter: among ≤ 8 evenly sampled faces, the one with the fewest cuts and the smallest
/// side imbalance (deterministic; on ties, the first).
fn pick_plane(ps: &[Poly], tol: f64) -> Plane {
    let n = ps.len();
    if n <= 2 {
        return ps[0].plane;
    }
    let k = n.min(8);
    let mut best = (usize::MAX, ps[0].plane);
    for c in 0..k {
        let pl = ps[c * n / k].plane;
        let (mut cuts, mut f, mut b) = (0usize, 0usize, 0usize);
        for p in ps {
            let (mut pf, mut pb) = (false, false);
            for q in &p.pts {
                match pl.side(*q, tol) {
                    1 => pf = true,
                    -1 => pb = true,
                    _ => {}
                }
            }
            match (pf, pb) {
                (true, true) => cuts += 1,
                (true, false) => f += 1,
                (false, true) => b += 1,
                _ => {}
            }
        }
        let score = cuts * 8 + f.abs_diff(b);
        if score < best.0 {
            best = (score, pl);
        }
    }
    best.1
}

impl Bsp {
    fn new(polys: Vec<Poly>, tol: f64) -> Bsp {
        let mut t = Bsp {
            nodes: vec![Node::default()],
            tol,
        };
        t.build(0, polys);
        t
    }

    fn build(&mut self, root: usize, polys: Vec<Poly>) {
        let mut stack = vec![(root, polys)];
        while let Some((i, ps)) = stack.pop() {
            if ps.is_empty() {
                continue;
            }
            let pick = pick_plane(&ps, self.tol);
            let plane = *self.nodes[i].plane.get_or_insert(pick);
            let mut s = Split::default();
            for p in ps {
                split(&plane, p, self.tol, &mut s);
            }
            self.nodes[i].polys.append(&mut s.cf);
            self.nodes[i].polys.append(&mut s.cb);
            if !s.f.is_empty() {
                let j = self.child(i, true);
                stack.push((j, s.f));
            }
            if !s.b.is_empty() {
                let j = self.child(i, false);
                stack.push((j, s.b));
            }
        }
    }

    fn child(&mut self, i: usize, front: bool) -> usize {
        let cur = if front {
            self.nodes[i].front
        } else {
            self.nodes[i].back
        };
        if let Some(j) = cur {
            return j;
        }
        let j = self.nodes.len();
        self.nodes.push(Node::default());
        if front {
            self.nodes[i].front = Some(j);
        } else {
            self.nodes[i].back = Some(j);
        }
        j
    }

    /// Pieces of polygons outside the tree body (faces coincident in direction count as outside).
    fn clip(&self, polys: Vec<Poly>) -> Vec<Poly> {
        let mut out = vec![];
        let mut stack = vec![(0usize, polys)];
        while let Some((i, ps)) = stack.pop() {
            let node = &self.nodes[i];
            let Some(plane) = node.plane else {
                out.extend(ps);
                continue;
            };
            let mut s = Split::default();
            for p in ps {
                split(&plane, p, self.tol, &mut s);
            }
            let mut f = s.f;
            f.append(&mut s.cf);
            let mut b = s.b;
            b.append(&mut s.cb);
            match node.front {
                Some(j) => stack.push((j, f)),
                None => out.extend(f),
            }
            if let Some(j) = node.back {
                stack.push((j, b));
            }
        }
        out
    }

    fn invert(&mut self) {
        for n in &mut self.nodes {
            n.plane = n.plane.map(Plane::flipped);
            std::mem::swap(&mut n.front, &mut n.back);
        }
    }
}

fn flip_all(mut v: Vec<Poly>) -> Vec<Poly> {
    v.iter_mut().for_each(Poly::flip);
    v
}

fn overlaps(p: &Poly, lo: V3, hi: V3) -> bool {
    p.pts.iter().any(|q| q.x >= lo.x)
        && p.pts.iter().any(|q| q.x <= hi.x)
        && p.pts.iter().any(|q| q.y >= lo.y)
        && p.pts.iter().any(|q| q.y <= hi.y)
        && p.pts.iter().any(|q| q.z >= lo.z)
        && p.pts.iter().any(|q| q.z <= hi.z)
}

fn grow(b: (V3, V3), t: f64) -> (V3, V3) {
    (b.0 - v3(t, t, t), b.1 + v3(t, t, t))
}

fn boolean(a: &Body, b: &Body, op: BoolOp, tol: f64) -> Body {
    let (Some(ba), Some(bb)) = (a.bbox(), b.bbox()) else {
        return match op {
            BoolOp::Union => Body {
                polys: a.polys.iter().chain(&b.polys).cloned().collect(),
            },
            BoolOp::Subtract => a.clone(),
            BoolOp::Intersect => Body::default(),
        };
    };
    let (ba, bb) = (grow(ba, 2.0 * tol), grow(bb, 2.0 * tol));
    let disjoint = ba.1.x < bb.0.x
        || bb.1.x < ba.0.x
        || ba.1.y < bb.0.y
        || bb.1.y < ba.0.y
        || ba.1.z < bb.0.z
        || bb.1.z < ba.0.z;
    if disjoint {
        return match op {
            BoolOp::Union => Body {
                polys: a.polys.iter().chain(&b.polys).cloned().collect(),
            },
            BoolOp::Subtract => a.clone(),
            BoolOp::Intersect => Body::default(),
        };
    }
    let (near_a, far_a): (Vec<Poly>, Vec<Poly>) = a
        .polys
        .iter()
        .cloned()
        .partition(|p| overlaps(p, bb.0, bb.1));
    let (near_b, far_b): (Vec<Poly>, Vec<Poly>) = b
        .polys
        .iter()
        .cloned()
        .partition(|p| overlaps(p, ba.0, ba.1));
    let ta = Bsp::new(a.polys.clone(), tol);
    let tb = Bsp::new(b.polys.clone(), tol);
    let mut polys = vec![];
    match op {
        BoolOp::Union => {
            polys.extend(far_a);
            polys.extend(tb.clip(near_a));
            polys.extend(far_b);
            let b1 = ta.clip(near_b);
            polys.extend(flip_all(ta.clip(flip_all(b1))));
        }
        BoolOp::Subtract => {
            let mut na = ta;
            na.invert();
            polys.extend(far_a);
            polys.extend(flip_all(tb.clip(flip_all(near_a))));
            let b1 = na.clip(near_b);
            polys.extend(na.clip(flip_all(b1)));
        }
        BoolOp::Intersect => {
            let mut na = ta;
            na.invert();
            let mut nb = tb;
            nb.invert();
            let b1 = na.clip(near_b);
            let a1 = nb.clip(flip_all(near_a));
            let b2 = na.clip(flip_all(b1));
            polys.extend(flip_all(a1));
            polys.extend(flip_all(b2));
        }
    }
    Body { polys }
}
