// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Booleans on closed triangle meshes for large curved bodies (tessellated freeform, spheres,
//! cylinders): cost grows with the number of touching triangle pairs, not with the product
//! of face counts as in the planar BSP path (`csg`, kept for small planar bodies).
//!
//! Method: candidate triangle pairs from a box hierarchy; each pair is classified with exact
//! `orient3d` on input coordinates and yields intersection points with symbolic keys (input
//! vertex, edge x face, edge x edge), so every triangle that shares a point gets the very same
//! point. A new point is implicit: an input segment cut by a plane (or, for coplanar pairs,
//! by a line in an axis projection); predicates on it are filtered and fall back to exact
//! expansions. Each touched triangle is retriangulated with the points and intersection
//! segments as constraints (no Steiner points), patches between constraints are classified
//! by exact ray parity against the other operand, coplanar pieces by containment and normal
//! direction. Output coordinates are the implicit points rounded to `f64`; the result is
//! checked closed and consistently oriented before it is returned.
//!
//! Loss: none beyond the operands' own facets; `face_of` of every output triangle is the
//! `face_of` of the input triangle it came from, so exact source surfaces stay addressable.

mod bvh;
mod num;
#[cfg(test)]
mod tests;
mod tri2;

use std::collections::{HashMap, HashSet};

use crate::csg::{Body, BoolOp, Poly};
use crate::math::{Tol, V3};
use crate::mesh::Mesh;
use crate::predicates::orient3d;
use bvh::Bvh;
use num::{orient2, sgn, Lin, Store, P};
use tri2::{SplitError, Tri2};

/// Face pairs (faces of a times faces of b) from which `csg_check::checked_boolean` and the
/// freeform booleans switch from the BSP path to this one.
pub const MESH_PAIRS: usize = 4_096;

/// The BSP path clips every face of one operand against the other's tree, so its work and its
/// float splits grow with the product of the face counts, not their sum: a faceted ring of 1024
/// faces cut by a 6-face bar is a large pair, two 40-face blocks are not.
pub fn prefers_mesh(faces_a: usize, faces_b: usize) -> bool {
    faces_a.saturating_mul(faces_b) >= MESH_PAIRS
}

/// What the mesh path keeps and drops (for receipts).
pub const NOTE: &str =
    "mesh boolean: new vertices lie exactly on the input facets, rounded to f64; face ids kept";

#[derive(Clone, Debug, PartialEq)]
pub enum MeshBoolError {
    /// Operand is not a closed, consistently oriented triangle mesh (or has no triangles).
    Open { which: char },
    /// NaN or infinity in an operand.
    NonFinite { which: char },
    /// Triangle with three collinear corners.
    Degenerate { which: char, tri: usize },
    /// Intersections could not be arranged in a triangle: an operand intersects itself.
    SelfIntersecting { which: char },
    /// A triangle could not be split for another reason: `why` is "outside" (a point off the
    /// triangle) or "coincident" (two different points at one place).
    Split { which: char, tri: usize, why: &'static str },
    /// The assembled result is not closed (reported, never returned as a solid).
    OpenResult,
}

impl std::fmt::Display for MeshBoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            MeshBoolError::Open { which } => write!(f, "operand {which} is not a closed mesh"),
            MeshBoolError::NonFinite { which } => {
                write!(f, "operand {which} has non-finite coordinates")
            }
            MeshBoolError::Degenerate { which, tri } => {
                write!(f, "operand {which} has a degenerate triangle #{tri}")
            }
            MeshBoolError::SelfIntersecting { which } => {
                write!(f, "operand {which} intersects itself")
            }
            MeshBoolError::Split { which, tri, why } => {
                write!(f, "operand {which}: triangle #{tri} could not be split ({why})")
            }
            MeshBoolError::OpenResult => f.write_str("result is not closed"),
        }
    }
}

impl MeshBoolError {
    /// Receipt line when a caller falls back to the planar BSP path after this refusal.
    pub fn fallback_note(&self) -> &'static str {
        match self {
            MeshBoolError::Open { .. } => {
                "mesh boolean refused (operand not closed), planar BSP used; measure: faces"
            }
            MeshBoolError::NonFinite { .. } => {
                "mesh boolean refused (non-finite coordinates), planar BSP used; measure: faces"
            }
            MeshBoolError::Degenerate { .. } => {
                "mesh boolean refused (degenerate triangle), planar BSP used; measure: faces"
            }
            MeshBoolError::SelfIntersecting { .. } => {
                "mesh boolean refused (operand intersects itself), planar BSP used; measure: faces"
            }
            MeshBoolError::Split { .. } => {
                "mesh boolean refused (triangle split failed), planar BSP used; measure: faces"
            }
            MeshBoolError::OpenResult => {
                "mesh boolean refused (result not closed), planar BSP used; measure: faces"
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshBoolStats {
    /// Triangle pairs that meet.
    pub pairs: usize,
    pub coplanar_pairs: usize,
    /// New points (not input vertices).
    pub points: usize,
    /// Input triangles that were retriangulated.
    pub split_tris: usize,
    /// Patches classified by ray parity.
    pub patches: usize,
}

#[derive(Clone, Debug)]
pub struct MeshBool {
    pub mesh: Mesh,
    pub stats: MeshBoolStats,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Key {
    /// Edge (vertex ids) crossing the interior of a triangle (unified index).
    Ef(u64, u32),
    /// Edge of A crossing edge of B at interior points of both.
    Ee(u64, u64),
}

fn ekey(a: u32, b: u32) -> u64 {
    (u64::from(a.min(b)) << 32) | u64::from(a.max(b))
}

#[derive(Clone, Copy, PartialEq)]
enum Class {
    In,
    Out,
    OnSame,
    OnOpp,
}

struct Ctx {
    st: Store,
    /// Input vertex positions; vertex `i` is point `i` in `st`.
    nv: usize,
    pos: Vec<V3>,
    /// Triangles of A, then of B.
    tris: Vec<[u32; 3]>,
    face: Vec<u32>,
    na: usize,
    axis: Vec<u8>,
    ccw: Vec<bool>,
    keys: HashMap<Key, u32>,
    pts: Vec<Vec<u32>>,
    segs: Vec<Vec<(u32, u32)>>,
    cop: Vec<Vec<u32>>,
    /// Point lies on both surfaces.
    on: Vec<bool>,
    stats: MeshBoolStats,
}

fn bits(p: V3) -> [u64; 3] {
    // +0 and -0 are one coordinate
    [p.x + 0.0, p.y + 0.0, p.z + 0.0].map(f64::to_bits)
}

fn closed(tris: &[[u32; 3]]) -> bool {
    let mut e: HashMap<(u32, u32), i32> = HashMap::with_capacity(tris.len() * 2);
    for t in tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *e.entry((a.min(b), a.max(b))).or_default() += if a < b { 1 } else { -1 };
        }
    }
    e.values().all(|v| *v == 0)
}

impl Ctx {
    fn new(a: &Mesh, b: &Mesh, tol: Tol) -> Result<Ctx, MeshBoolError> {
        let mut pos: Vec<V3> = Vec::with_capacity(a.positions.len() + b.positions.len());
        let mut by_bits: HashMap<[u64; 3], u32> = HashMap::new();
        let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
        let h = tol.length;
        let cell = |p: V3| [p.x, p.y, p.z].map(|c| (c / h).floor() as i64);
        let mut ids = [Vec::new(), Vec::new()];
        for (s, (m, which)) in [(a, 'a'), (b, 'b')].into_iter().enumerate() {
            for &p in &m.positions {
                if !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()) {
                    return Err(MeshBoolError::NonFinite { which });
                }
                let mut id = by_bits.get(&bits(p)).copied();
                if id.is_none() && s == 1 && h > 0.0 {
                    // B vertices within the length tolerance of an A vertex take its place
                    let c = cell(p);
                    let mut best = (h, None);
                    for dx in -1..=1 {
                        for dy in -1..=1 {
                            for dz in -1..=1 {
                                for &g in grid
                                    .get(&[c[0] + dx, c[1] + dy, c[2] + dz])
                                    .into_iter()
                                    .flatten()
                                {
                                    let d = (pos[g as usize] - p).len();
                                    if d <= best.0 {
                                        best = (d, Some(g));
                                    }
                                }
                            }
                        }
                    }
                    id = best.1;
                }
                let id = id.unwrap_or_else(|| {
                    let g = pos.len() as u32;
                    pos.push(p);
                    by_bits.insert(bits(p), g);
                    if s == 0 && h > 0.0 {
                        grid.entry(cell(p)).or_default().push(g);
                    }
                    g
                });
                ids[s].push(id);
            }
        }
        let mut tris = Vec::new();
        let mut face = Vec::new();
        let mut na = 0;
        for (s, (m, which)) in [(a, 'a'), (b, 'b')].into_iter().enumerate() {
            let start = tris.len();
            for (i, t) in m.triangles.iter().enumerate() {
                let g = t.map(|x| ids[s][x as usize]);
                if g[0] == g[1] || g[1] == g[2] || g[0] == g[2] {
                    continue;
                }
                tris.push(g);
                face.push(m.face_of.get(i).copied().unwrap_or(0));
            }
            if tris.len() == start || !closed(&tris[start..]) {
                return Err(MeshBoolError::Open { which });
            }
            if s == 0 {
                na = tris.len();
            }
        }
        let mut axis = Vec::with_capacity(tris.len());
        let mut ccw = Vec::with_capacity(tris.len());
        for (i, t) in tris.iter().enumerate() {
            let [p, q, r] = t.map(|x| pos[x as usize]);
            let n = (q - p).cross(r - p);
            let mut ks = [(n.x.abs(), 0u8), (n.y.abs(), 1), (n.z.abs(), 2)];
            ks.sort_by(|x, y| y.0.total_cmp(&x.0));
            let Some((k, s)) = ks
                .iter()
                .map(|(_, k)| (*k, orient2(*k, p, q, r)))
                .find(|(_, s)| *s != 0)
            else {
                let (which, tri) = if i < na { ('a', i) } else { ('b', i - na) };
                return Err(MeshBoolError::Degenerate { which, tri });
            };
            axis.push(k);
            ccw.push(s > 0);
        }
        let mut st = Store::default();
        for p in &pos {
            st.push(P::E(*p));
        }
        let n = tris.len();
        Ok(Ctx {
            st,
            nv: pos.len(),
            on: vec![false; pos.len()],
            pos,
            tris,
            face,
            na,
            axis,
            ccw,
            keys: HashMap::new(),
            pts: vec![Vec::new(); n],
            segs: vec![Vec::new(); n],
            cop: vec![Vec::new(); n],
            stats: MeshBoolStats::default(),
        })
    }

    fn corners(&self, t: usize) -> [V3; 3] {
        self.tris[t].map(|x| self.pos[x as usize])
    }

    fn point(&mut self, k: Key, p: P) -> u32 {
        if let Some(&i) = self.keys.get(&k) {
            return i;
        }
        let i = self.st.push(p);
        self.on.push(false);
        self.keys.insert(k, i);
        self.stats.points += 1;
        i
    }

    /// Closed containment of `x` in triangle `t` (coplanar), with the edges it lies on.
    fn in_tri(&self, k: u8, t: usize, x: V3) -> Option<[bool; 3]> {
        let c = self.corners(t);
        let s = orient2(k, c[0], c[1], c[2]);
        let o = [0, 1, 2].map(|j| orient2(k, c[j], c[(j + 1) % 3], x) * s);
        o.iter().all(|v| *v >= 0).then(|| o.map(|v| v == 0))
    }

    fn commit(&mut self, ia: usize, ib: usize, mut ps: Vec<u32>) {
        ps.sort_unstable();
        ps.dedup();
        if ps.is_empty() {
            return;
        }
        self.stats.pairs += 1;
        for &p in &ps {
            self.on[p as usize] = true;
        }
        self.pts[ia].extend_from_slice(&ps);
        self.pts[ib].extend_from_slice(&ps);
        if let Some(s) = self.extremes(&ps) {
            self.segs[ia].push(s);
            self.segs[ib].push(s);
        }
    }

    /// First and last of collinear points in lexicographic order (monotone along the line).
    fn extremes(&self, ps: &[u32]) -> Option<(u32, u32)> {
        let lex = |a: &&u32, b: &&u32| self.st.lex([0, 1, 2], **a, **b);
        let lo = *ps.iter().min_by(lex)?;
        let hi = *ps.iter().max_by(lex)?;
        (lo != hi).then_some((lo, hi))
    }

    fn pair(&mut self, ia: usize, ib: usize) {
        let (ga, gb) = (self.tris[ia], self.tris[ib]);
        let (pa, pb) = (self.corners(ia), self.corners(ib));
        let sa = pa.map(|x| sgn(orient3d(pb[0], pb[1], pb[2], x)));
        let sb = pb.map(|x| sgn(orient3d(pa[0], pa[1], pa[2], x)));
        let one_side = |s: [i8; 3]| s.iter().all(|v| *v > 0) || s.iter().all(|v| *v < 0);
        if one_side(sa) || one_side(sb) {
            return;
        }
        if sa == [0; 3] || sb == [0; 3] {
            return self.coplanar(ia, ib);
        }
        let mut ps = Vec::new();
        for i in 0..3 {
            if sa[i] == 0 && self.in_tri(self.axis[ib], ib, pa[i]).is_some() {
                ps.push(ga[i]);
            }
            if sb[i] == 0 && self.in_tri(self.axis[ia], ia, pb[i]).is_some() {
                ps.push(gb[i]);
            }
        }
        // edges of one triangle crossing the plane of the other strictly
        for side in 0..2 {
            let (ge, pe, se, gf, pf, f) = if side == 0 {
                (ga, pa, sa, gb, pb, ib)
            } else {
                (gb, pb, sb, ga, pa, ia)
            };
            for i in 0..3 {
                let i1 = (i + 1) % 3;
                if se[i] * se[i1] >= 0 {
                    continue;
                }
                let (p, q) = (pe[i], pe[i1]);
                let o = [0, 1, 2].map(|k| sgn(orient3d(p, q, pf[k], pf[(k + 1) % 3])));
                if o.contains(&1) && o.contains(&-1) {
                    continue;
                }
                let z: Vec<usize> = (0..3).filter(|k| o[*k] == 0).collect();
                let e = ekey(ge[i], ge[i1]);
                let lp = P::L {
                    p,
                    q,
                    f: Lin::Plane(pf),
                };
                match z.len() {
                    0 => ps.push(self.point(Key::Ef(e, f as u32), lp)),
                    1 => {
                        let ef = ekey(gf[z[0]], gf[(z[0] + 1) % 3]);
                        let k = if side == 0 {
                            Key::Ee(e, ef)
                        } else {
                            Key::Ee(ef, e)
                        };
                        ps.push(self.point(k, lp));
                    }
                    2 => {
                        // the vertex shared by the two edges it lies on
                        let v = if (z[0] + 1) % 3 == z[1] { z[1] } else { z[0] };
                        ps.push(gf[v]);
                    }
                    _ => {}
                }
            }
        }
        self.commit(ia, ib, ps);
    }

    fn coplanar(&mut self, ia: usize, ib: usize) {
        let (ga, gb) = (self.tris[ia], self.tris[ib]);
        let (pa, pb) = (self.corners(ia), self.corners(ib));
        let k = self.axis[ia];
        let mut ps = Vec::new();
        let mut on_a: [Vec<u32>; 3] = Default::default();
        let mut on_b: [Vec<u32>; 3] = Default::default();
        for i in 0..3 {
            if let Some(z) = self.in_tri(k, ib, pa[i]) {
                ps.push(ga[i]);
                on_a[i].push(ga[i]);
                on_a[(i + 2) % 3].push(ga[i]);
                (0..3).filter(|j| z[*j]).for_each(|j| on_b[j].push(ga[i]));
            }
            if let Some(z) = self.in_tri(k, ia, pb[i]) {
                ps.push(gb[i]);
                on_b[i].push(gb[i]);
                on_b[(i + 2) % 3].push(gb[i]);
                (0..3).filter(|j| z[*j]).for_each(|j| on_a[j].push(gb[i]));
            }
        }
        for i in 0..3 {
            let (p, q) = (pa[i], pa[(i + 1) % 3]);
            for j in 0..3 {
                let (r, s) = (pb[j], pb[(j + 1) % 3]);
                let (o1, o2) = (orient2(k, r, s, p), orient2(k, r, s, q));
                let (o3, o4) = (orient2(k, p, q, r), orient2(k, p, q, s));
                if o1 * o2 < 0 && o3 * o4 < 0 {
                    let key = Key::Ee(ekey(ga[i], ga[(i + 1) % 3]), ekey(gb[j], gb[(j + 1) % 3]));
                    let x = self.point(
                        key,
                        P::L {
                            p,
                            q,
                            f: Lin::Line(k, [r, s]),
                        },
                    );
                    ps.push(x);
                    on_a[i].push(x);
                    on_b[j].push(x);
                }
            }
        }
        ps.sort_unstable();
        ps.dedup();
        if ps.is_empty() {
            return;
        }
        self.stats.pairs += 1;
        self.stats.coplanar_pairs += 1;
        self.cop[ia].push(ib as u32);
        self.cop[ib].push(ia as u32);
        for &p in &ps {
            self.on[p as usize] = true;
        }
        self.pts[ia].extend_from_slice(&ps);
        self.pts[ib].extend_from_slice(&ps);
        for j in 0..3 {
            if let Some(s) = self.extremes(&on_b[j]) {
                self.segs[ia].push(s);
            }
            if let Some(s) = self.extremes(&on_a[j]) {
                self.segs[ib].push(s);
            }
        }
    }

    /// Fragments of triangle `t` (input orientation) and its constrained edges.
    fn split(&self, t: usize) -> Result<(Vec<[u32; 3]>, Vec<(u32, u32)>), MeshBoolError> {
        let g = self.tris[t];
        if self.pts[t].is_empty() && self.segs[t].is_empty() {
            return Ok((vec![g], vec![]));
        }
        let (which, tri) = if t < self.na { ('a', t) } else { ('b', t - self.na) };
        let bad = |e: SplitError| match e {
            SplitError::Crossing => MeshBoolError::SelfIntersecting { which },
            SplitError::Outside => MeshBoolError::Split { which, tri, why: "outside" },
            SplitError::Coincident => MeshBoolError::Split { which, tri, why: "coincident" },
        };
        let c = if self.ccw[t] { g } else { [g[0], g[2], g[1]] };
        let mut tr = Tri2::new(&self.st, self.axis[t], c);
        for &p in &self.pts[t] {
            tr.insert(p).map_err(bad)?;
        }
        for &(a, b) in &self.segs[t] {
            tr.constrain(a, b).map_err(bad)?;
        }
        let (mut f, fixed) = tr.finish();
        if !self.ccw[t] {
            f.iter_mut().for_each(|x| x.swap(1, 2));
        }
        Ok((f, fixed))
    }

    /// Fragment of `t` lies on a coplanar triangle of the other operand: same or opposite normal.
    fn on_surface(&self, t: usize, f: [u32; 3]) -> Option<Class> {
        let k = self.axis[t];
        for &o in &self.cop[t] {
            let o = o as usize;
            let go = self.tris[o];
            let po = self.corners(o);
            let so = orient2(k, po[0], po[1], po[2]);
            let inside = f
                .iter()
                .all(|&x| (0..3).all(|j| self.st.orient(k, go[j], go[(j + 1) % 3], x) * so >= 0));
            if inside {
                let pt = self.corners(t);
                let nt = (pt[1] - pt[0]).cross(pt[2] - pt[0]);
                let no = (po[1] - po[0]).cross(po[2] - po[0]);
                return Some(if nt.dot(no) > 0.0 {
                    Class::OnSame
                } else {
                    Class::OnOpp
                });
            }
        }
        None
    }
}

// arbitrary skew ray directions; 0.7071 is a sample value, not 1/sqrt(2)
#[allow(clippy::approx_constant)]
const DIRS: [(f64, f64, f64); 7] = [
    (0.5773, 0.6011, 0.5528),
    (-0.3127, 0.8411, 0.4413),
    (0.7071, -0.2917, -0.6441),
    (-0.6125, -0.5051, 0.6081),
    (0.1239, -0.9133, 0.3881),
    (0.9011, 0.3413, -0.2677),
    (-0.4567, 0.1234, -0.8807),
];

/// Point strictly off the surface of the triangles `range` is inside them (ray parity with
/// exact predicates; a ray through an edge or vertex is retried in another direction).
fn inside(ctx: &Ctx, bvh: &Bvh, base: usize, o: V3) -> bool {
    let Some((lo, hi)) = bvh.bounds() else {
        return false;
    };
    let far = [lo.x, hi.x]
        .iter()
        .flat_map(|x| [lo.y, hi.y].map(|y| (*x, y)))
        .flat_map(|(x, y)| [lo.z, hi.z].map(|z| (V3 { x, y, z } - o).len()))
        .fold(0.0, f64::max);
    let len = 2.0 * far + 1.0;
    let mut cand = Vec::new();
    'dir: for d in DIRS {
        let e = o + V3 {
            x: d.0,
            y: d.1,
            z: d.2,
        } * len;
        bvh.query_seg(o, e, &mut cand);
        let mut odd = false;
        for &i in &cand {
            let [a, b, c] = ctx.corners(base + i as usize);
            let s1 = sgn(orient3d(a, b, c, o));
            let s2 = sgn(orient3d(a, b, c, e));
            // `o` is off the surface and `e` outside its box: a ray starting or ending in
            // the plane of a triangle misses it unless it runs inside that plane
            if s1 == 0 && s2 == 0 {
                continue 'dir;
            }
            if s1 == 0 || s2 == 0 || s1 == s2 {
                continue;
            }
            let w = [
                sgn(orient3d(o, e, a, b)),
                sgn(orient3d(o, e, b, c)),
                sgn(orient3d(o, e, c, a)),
            ];
            if w.contains(&1) && w.contains(&-1) {
                continue;
            }
            if w.contains(&0) {
                continue 'dir;
            }
            odd = !odd;
        }
        return odd;
    }
    // every direction grazed an edge: generalized winding number
    let mut w = 0.0;
    for i in 0..bvh.boxes.len() {
        let [a, b, c] = ctx.corners(base + i).map(|p| p - o);
        let (la, lb, lc) = (a.len(), b.len(), c.len());
        let num = a.dot(b.cross(c));
        let den = la * lb * lc + a.dot(b) * lc + a.dot(c) * lb + b.dot(c) * la;
        w += 2.0 * num.atan2(den);
    }
    w / (4.0 * std::f64::consts::PI) > 0.5
}

fn tri_box(c: [V3; 3]) -> (V3, V3) {
    let lo = V3 {
        x: c[0].x.min(c[1].x).min(c[2].x),
        y: c[0].y.min(c[1].y).min(c[2].y),
        z: c[0].z.min(c[1].z).min(c[2].z),
    };
    let hi = V3 {
        x: c[0].x.max(c[1].x).max(c[2].x),
        y: c[0].y.max(c[1].y).max(c[2].y),
        z: c[0].z.max(c[1].z).max(c[2].z),
    };
    (lo, hi)
}

/// Two triangles of one operand cross at interior points (an edge strictly through the other's
/// interior, or coplanar edges crossing strictly). Touching at vertices or along edges passes.
fn crosses(p: [V3; 3], q: [V3; 3], k: u8) -> bool {
    let sp = p.map(|x| sgn(orient3d(q[0], q[1], q[2], x)));
    let sq = q.map(|x| sgn(orient3d(p[0], p[1], p[2], x)));
    let one_side = |s: [i8; 3]| s.iter().all(|v| *v > 0) || s.iter().all(|v| *v < 0);
    if one_side(sp) || one_side(sq) {
        return false;
    }
    if sp == [0; 3] || sq == [0; 3] {
        return (0..3).any(|i| {
            let (a, b) = (p[i], p[(i + 1) % 3]);
            (0..3).any(|j| {
                let (c, d) = (q[j], q[(j + 1) % 3]);
                orient2(k, c, d, a) * orient2(k, c, d, b) < 0
                    && orient2(k, a, b, c) * orient2(k, a, b, d) < 0
            })
        });
    }
    [(p, sp, q), (q, sq, p)].iter().any(|(e, s, f)| {
        (0..3).any(|i| {
            let i1 = (i + 1) % 3;
            if s[i] * s[i1] >= 0 {
                return false;
            }
            let o = [0, 1, 2].map(|j| sgn(orient3d(e[i], e[i1], f[j], f[(j + 1) % 3])));
            o == [1; 3] || o == [-1; 3]
        })
    })
}

/// Cheap self-intersection check of one operand (triangles `base..base + bvh.len`).
fn self_crossing(ctx: &Ctx, bvh: &Bvh, base: usize) -> bool {
    let mut cand = Vec::new();
    for i in 0..bvh.boxes.len() {
        bvh.query(&bvh.boxes[i], &mut cand);
        let p = ctx.corners(base + i);
        for &j in &cand {
            let j = j as usize;
            if j > i && crosses(p, ctx.corners(base + j), ctx.axis[base + i]) {
                return true;
            }
        }
    }
    false
}

struct Frag {
    v: [u32; 3],
    src: u32,
    class: Option<Class>,
}

struct Dsu(Vec<u32>);
impl Dsu {
    fn find(&mut self, mut x: u32) -> u32 {
        while self.0[x as usize] != x {
            let p = self.0[self.0[x as usize] as usize];
            self.0[x as usize] = p;
            x = p;
        }
        x
    }
    fn join(&mut self, a: u32, b: u32) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a as usize] = b;
        }
    }
}

/// `a op b` on closed, consistently oriented triangle meshes. Neither mesh is changed.
pub fn mesh_boolean(a: &Mesh, b: &Mesh, op: BoolOp, tol: Tol) -> Result<MeshBool, MeshBoolError> {
    let mut ctx = Ctx::new(a, b, tol)?;
    let (na, n) = (ctx.na, ctx.tris.len());
    let boxes: Vec<(V3, V3)> = (0..n).map(|t| tri_box(ctx.corners(t))).collect();
    let bvh_a = Bvh::new(boxes[..na].to_vec());
    let bvh_b = Bvh::new(boxes[na..].to_vec());
    for (which, bvh, base) in [('a', &bvh_a, 0), ('b', &bvh_b, na)] {
        if self_crossing(&ctx, bvh, base) {
            return Err(MeshBoolError::SelfIntersecting { which });
        }
    }
    let mut cand = Vec::new();
    for ia in 0..na {
        bvh_b.query(&boxes[ia], &mut cand);
        for &j in &cand {
            ctx.pair(ia, na + j as usize);
        }
    }
    let mut frags: [Vec<Frag>; 2] = [Vec::with_capacity(na), Vec::with_capacity(n - na)];
    let mut fixed: [HashSet<(u32, u32)>; 2] = Default::default();
    for t in 0..n {
        let s = usize::from(t >= na);
        let (fs, fx) = ctx.split(t)?;
        if fs.len() > 1 || !fx.is_empty() {
            ctx.stats.split_tris += 1;
        }
        fixed[s].extend(fx);
        for v in fs {
            let class = if ctx.cop[t].is_empty() {
                None
            } else {
                ctx.on_surface(t, v)
            };
            frags[s].push(Frag {
                v,
                src: t as u32,
                class,
            });
        }
    }
    for s in 0..2 {
        let other = if s == 0 { (&bvh_b, na) } else { (&bvh_a, 0) };
        ctx.stats.patches += classify(&ctx, &mut frags[s], &fixed[s], other);
    }
    let mut out = assemble(&ctx, &frags, op);
    // planar faces come back as fragments of every earlier split; refilled from their boundary
    // loops they hold boundary + 2 * holes - 2 triangles, so repeated cuts stay linear
    let all = vec![true; out.positions.len()];
    crate::face_fill::refill_flat_faces(&mut out, &all, 0);
    flip_caps(&mut out, tol.length);
    if !out.is_watertight() {
        return Err(MeshBoolError::OpenResult);
    }
    Ok(MeshBool {
        mesh: out,
        stats: ctx.stats,
    })
}

/// Patches between constrained edges get one ray-parity class each.
fn classify(
    ctx: &Ctx,
    frags: &mut [Frag],
    fixed: &HashSet<(u32, u32)>,
    other: (&Bvh, usize),
) -> usize {
    let mut dsu = Dsu((0..frags.len() as u32).collect());
    let mut edges: HashMap<(u32, u32), (u32, u32)> = HashMap::with_capacity(frags.len() * 2);
    for (i, f) in frags.iter().enumerate() {
        if f.class.is_some() {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (f.v[k], f.v[(k + 1) % 3]);
            let e = (a.min(b), a.max(b));
            if fixed.contains(&e) {
                continue;
            }
            let ent = edges.entry(e).or_insert((u32::MAX, 0));
            if ent.0 == u32::MAX {
                ent.0 = i as u32;
            }
            ent.1 += 1;
            if ent.1 == 2 {
                dsu.join(ent.0, i as u32);
            }
        }
    }
    // probe per patch: an input vertex off the other surface, else the largest fragment's centroid
    let mut probe: HashMap<u32, (bool, f64, V3)> = HashMap::new();
    for i in 0..frags.len() {
        if frags[i].class.is_some() {
            continue;
        }
        let r = dsu.find(i as u32);
        let f = &frags[i];
        let exact =
            f.v.iter()
                .find(|&&x| (x as usize) < ctx.nv && !ctx.on[x as usize])
                .map(|&x| ctx.pos[x as usize]);
        let ent = probe.entry(r).or_insert((false, -1.0, V3::default()));
        if ent.0 {
            continue;
        }
        if let Some(p) = exact {
            *ent = (true, 0.0, p);
            continue;
        }
        let [a, b, c] = f.v.map(|x| ctx.st.approx(x));
        let ar = (b - a).cross(c - a).len();
        if ar > ent.1 {
            *ent = (false, ar, (a + b + c) * (1.0 / 3.0));
        }
    }
    let patches = probe.len();
    let cls: HashMap<u32, Class> = probe
        .into_iter()
        .map(|(r, (_, _, p))| {
            let c = if inside(ctx, other.0, other.1, p) {
                Class::In
            } else {
                Class::Out
            };
            (r, c)
        })
        .collect();
    for i in 0..frags.len() {
        if frags[i].class.is_none() {
            let r = dsu.find(i as u32);
            frags[i].class = Some(cls[&r]);
        }
    }
    patches
}

fn assemble(ctx: &Ctx, frags: &[Vec<Frag>; 2], op: BoolOp) -> Mesh {
    let mut map: HashMap<u32, u32> = HashMap::new();
    let mut by_pos: HashMap<[u64; 3], u32> = HashMap::new();
    let mut m = Mesh::default();
    for (s, list) in frags.iter().enumerate() {
        for f in list {
            let c = f.class.expect("classified");
            let (keep, flip) = match (s, op, c) {
                (0, BoolOp::Union, Class::Out | Class::OnSame) => (true, false),
                (0, BoolOp::Intersect, Class::In | Class::OnSame) => (true, false),
                (0, BoolOp::Subtract, Class::Out | Class::OnOpp) => (true, false),
                (1, BoolOp::Union, Class::Out) => (true, false),
                (1, BoolOp::Intersect, Class::In) => (true, false),
                (1, BoolOp::Subtract, Class::In) => (true, true),
                _ => (false, false),
            };
            if !keep {
                continue;
            }
            let mut t = f.v.map(|x| {
                *map.entry(x).or_insert_with(|| {
                    let p = ctx.st.approx(x);
                    *by_pos.entry(bits(p)).or_insert_with(|| {
                        m.positions.push(p);
                        (m.positions.len() - 1) as u32
                    })
                })
            });
            if flip {
                t.swap(1, 2);
            }
            // distinct points that round to one coordinate leave a zero-length edge
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                continue;
            }
            m.triangles.push(t);
            m.face_of.push(ctx.face[f.src as usize]);
        }
    }
    m
}

/// Height of a triangle over its longest edge, and that edge's index.
fn needle_height(m: &Mesh, t: [u32; 3]) -> (f64, usize) {
    let p = t.map(|i| m.positions[i as usize]);
    let k = (0..3)
        .max_by(|&i, &j| {
            let li = (p[(i + 1) % 3] - p[i]).len();
            let lj = (p[(j + 1) % 3] - p[j]).len();
            li.total_cmp(&lj)
        })
        .unwrap_or(0);
    let e = (p[(k + 1) % 3] - p[k]).len();
    let h = (p[1] - p[0]).cross(p[2] - p[0]).len() / e.max(1e-300);
    (h, k)
}

/// Rounded split points can leave caps: a vertex within `tol` of the opposite long edge, so the
/// triangle has almost no height and may fold over its neighbour, and the next boolean rejects
/// that operand as self-intersecting. Flipping the long edge with the neighbour across it
/// removes the cap; both new triangles lie in the neighbour's plane up to the cap height.
fn flip_caps(m: &mut Mesh, tol: f64) {
    for _ in 0..8 {
        let collapsed = collapse_short_edges(m, tol);
        let mut by_edge: HashMap<(u32, u32), usize> = HashMap::with_capacity(m.triangles.len() * 3);
        for (i, t) in m.triangles.iter().enumerate() {
            for k in 0..3 {
                by_edge.insert((t[k], t[(k + 1) % 3]), i);
            }
        }
        let mut touched = vec![false; m.triangles.len()];
        let mut drop = vec![false; m.triangles.len()];
        let mut flips = 0;
        for i in 0..m.triangles.len() {
            if touched[i] {
                continue;
            }
            let t = m.triangles[i];
            let (h, k) = needle_height(m, t);
            if !(h < tol) {
                continue;
            }
            let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
            let Some(&u) = by_edge.get(&(b, a)) else {
                continue;
            };
            if touched[u] || u == i {
                continue;
            }
            let tu = m.triangles[u];
            let d = tu[(0..3).find(|&j| tu[j] != a && tu[j] != b).unwrap_or(0)];
            if d == c {
                // (a, b, c) and (b, a, c): the pair uses each of its edges once each way and
                // closes on itself, so dropping both keeps the rest watertight
                drop[i] = true;
                drop[u] = true;
                touched[i] = true;
                touched[u] = true;
                flips += 1;
                continue;
            }
            if by_edge.contains_key(&(c, d)) || by_edge.contains_key(&(d, c)) {
                continue;
            }
            let (n1, n2) = ([a, d, c], [d, b, c]);
            if needle_height(m, n1).0 < tol || needle_height(m, n2).0 < tol {
                continue;
            }
            m.triangles[i] = n1;
            m.triangles[u] = n2;
            m.face_of[i] = m.face_of[u];
            touched[i] = true;
            touched[u] = true;
            flips += 1;
        }
        if drop.iter().any(|x| *x) {
            let keep = |i: &usize| !drop[*i];
            m.triangles = (0..drop.len()).filter(keep).map(|i| m.triangles[i]).collect();
            m.face_of = (0..drop.len()).filter(keep).map(|i| m.face_of[i]).collect();
        }
        if flips == 0 && !collapsed {
            return;
        }
    }
}

/// Two split points can be built along different exact paths and round to coordinates 1e-15
/// apart, so they stay separate vertices joined by an edge far below `tol`. Collapsing that
/// edge merges them; the two triangles on it degenerate and go, and each one's other two
/// neighbours pair up with each other, so a closed mesh stays closed.
fn collapse_short_edges(m: &mut Mesh, tol: f64) -> bool {
    let mut to: Vec<u32> = (0..m.positions.len() as u32).collect();
    fn root(to: &mut [u32], mut x: u32) -> u32 {
        while to[x as usize] != x {
            x = to[x as usize];
        }
        x
    }
    let mut any = false;
    for t in &m.triangles {
        for k in 0..3 {
            let (a, b) = (root(&mut to, t[k]), root(&mut to, t[(k + 1) % 3]));
            if a != b && (m.positions[a as usize] - m.positions[b as usize]).len() < tol {
                to[a.max(b) as usize] = a.min(b);
                any = true;
            }
        }
    }
    if !any {
        return false;
    }
    let mut tris = Vec::with_capacity(m.triangles.len());
    let mut faces = Vec::with_capacity(m.triangles.len());
    for (t, f) in m.triangles.iter().zip(&m.face_of) {
        let r = t.map(|i| root(&mut to, i));
        if r[0] != r[1] && r[1] != r[2] && r[0] != r[2] {
            tris.push(r);
            faces.push(*f);
        }
    }
    m.triangles = tris;
    m.face_of = faces;
    true
}

/// Body boolean through the mesh path: polygons are fanned (tags kept), the result is turned
/// back into tagged faces and welded.
pub fn boolean_bodies(a: &Body, b: &Body, op: BoolOp, tol: Tol) -> Result<Body, MeshBoolError> {
    let r = mesh_boolean(&a.to_mesh(), &b.to_mesh(), op, tol)?;
    let m = &r.mesh;
    let polys = m
        .triangles
        .iter()
        .zip(&m.face_of)
        .filter_map(|(t, tag)| Poly::new(t.map(|i| m.positions[i as usize]).to_vec(), *tag))
        .collect();
    Ok(Body { polys }.welded(tol.length))
}
