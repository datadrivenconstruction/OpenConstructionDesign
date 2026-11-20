// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Direct modeling on mass bodies (docs/MODEL3.md): push/pull to an inferred target, push/pull
//! of a region drawn on a face (the face is split), fillets and chamfers on edge sets with
//! vertex blends at three-edge corners, draft of faces about a neutral plane.

use super::*;
use crate::csg::{drop_collinear, ftag, Body, Plane, Poly};
use crate::math::{v2, v3, Tol, V2, V3};

/// What a push/pull goes to: a distance, a parallel face, a point along the face normal, or
/// through the body (to its far side behind the face).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PushTarget {
    Dist(f64),
    Face(u32),
    Point(V3),
    Through,
}

/// Signed push/pull distance of face `tag` (> 0 outward) inferred from `to`.
pub fn push_distance(b: &Body, tag: u32, to: PushTarget, tol: Tol) -> R<f64> {
    let pl = planar_face(b, tag, tol.length)?;
    let d = match to {
        PushTarget::Dist(d) => d,
        PushTarget::Point(p) => pl.dist(p),
        PushTarget::Face(t) => {
            if t == tag {
                return Err(MassError::BadParam("target face is the pushed face"));
            }
            let q = planar_face(b, t, tol.length)?;
            if q.n.cross(pl.n).len() > 1e-9 {
                return Err(MassError::NotParallel(tag, t));
            }
            let p = b
                .polys
                .iter()
                .find(|p| p.tag == t)
                .map(|p| p.pts[0])
                .ok_or(MassError::NoFace(t))?;
            pl.dist(p)
        }
        PushTarget::Through => {
            let lo = b
                .polys
                .iter()
                .flat_map(|p| p.pts.iter())
                .map(|q| pl.dist(*q))
                .fold(0.0, f64::min);
            if lo > -tol.length {
                return Err(MassError::BadParam("nothing behind the face"));
            }
            lo
        }
    };
    if !d.is_finite() {
        return Err(MassError::BadParam("push/pull distance"));
    }
    Ok(d)
}

/// Solution of n_i · x = w_i (None for nearly parallel planes).
fn solve3(n: [V3; 3], w: [f64; 3]) -> Option<V3> {
    let det = n[0].dot(n[1].cross(n[2]));
    if det.abs() < 1e-9 {
        return None;
    }
    let x = (n[1].cross(n[2]) * w[0] + n[2].cross(n[0]) * w[1] + n[0].cross(n[1]) * w[2])
        * (1.0 / det);
    [x.x, x.y, x.z].iter().all(|c| c.is_finite()).then_some(x)
}

fn orient2(a: V2, b: V2, c: V2) -> f64 {
    (b - a).cross(c - a)
}

/// Segments cross at interior points of both (touching and collinear overlaps do not count).
fn proper_cross(a: V2, b: V2, c: V2, d: V2, eps: f64) -> bool {
    let e1 = eps * (b - a).len().max(1e-12);
    let e2 = eps * (d - c).len().max(1e-12);
    let (o1, o2) = (orient2(a, b, c), orient2(a, b, d));
    let (o3, o4) = (orient2(c, d, a), orient2(c, d, b));
    ((o1 > e1 && o2 < -e1) || (o1 < -e1 && o2 > e1))
        && ((o3 > e2 && o4 < -e2) || (o3 < -e2 && o4 > e2))
}

/// The drawn loop snapped onto the face plane, counter-clockwise about the face normal.
fn region_ring(pl: &Plane, region: &[V3], tol: Tol) -> R<Vec<V3>> {
    let bad = MassError::BadParam("region loop");
    if region
        .iter()
        .any(|q| ![q.x, q.y, q.z].iter().all(|c| c.is_finite()))
    {
        return Err(bad);
    }
    if region.iter().any(|q| pl.dist(*q).abs() > 1e-6) {
        return Err(MassError::BadParam("region loop is not on the face"));
    }
    let r: Vec<V3> = region.iter().map(|q| *q - pl.n * pl.dist(*q)).collect();
    let mut r = drop_collinear(drop_dups(r), tol.length);
    if r.len() < 3 {
        return Err(bad);
    }
    let (u, w) = basis(pl.n);
    let p: Vec<V2> = r.iter().map(|q| v2(q.dot(u), q.dot(w))).collect();
    let k = p.len();
    for i in 0..k {
        for j in i + 2..k {
            if i == 0 && j == k - 1 {
                continue;
            }
            if proper_cross(p[i], p[(i + 1) % k], p[j], p[(j + 1) % k], 1e-12) {
                return Err(MassError::BadParam("region loop crosses itself"));
            }
        }
    }
    let a = ring_normal(&r).dot(pl.n);
    if a.abs() * 0.5 <= tol.length * tol.length {
        return Err(bad);
    }
    if a < 0.0 {
        r.reverse();
    }
    Ok(r)
}

/// The loop lies on the face: vertices and edge samples on it, no edge crosses the outline.
fn inside_face(b: &Body, tag: u32, n: V3, ring: &[V3], tol: f64) -> bool {
    let on = |q: V3| b.polys.iter().any(|p| p.tag == tag && on_poly(p, q, tol));
    let k = ring.len();
    for i in 0..k {
        let (a, c) = (ring[i], ring[(i + 1) % k]);
        if ![0.0, 0.25, 0.5, 0.75].iter().all(|t| on(a + (c - a) * *t)) {
            return false;
        }
    }
    let (u, w) = basis(n);
    let f = |q: V3| v2(q.dot(u), q.dot(w));
    for o in b.face_outline(tag, tol) {
        let m = o.len();
        for i in 0..m {
            let (c, d) = (f(o[i]), f(o[(i + 1) % m]));
            for j in 0..k {
                if proper_cross(f(ring[j]), f(ring[(j + 1) % k]), c, d, 1e-9) {
                    return false;
                }
            }
        }
    }
    true
}

/// Convex polygon clipped to m·x >= k (Sutherland-Hodgman with a length tolerance).
fn clip_ge(pts: &[V3], m: V3, k: f64, tol: f64) -> Vec<V3> {
    let d: Vec<f64> = pts.iter().map(|q| m.dot(*q) - k).collect();
    let mut out = vec![];
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        if d[i] >= -tol {
            out.push(pts[i]);
        }
        if (d[i] > tol && d[j] < -tol) || (d[i] < -tol && d[j] > tol) {
            out.push(pts[i] + (pts[j] - pts[i]) * (d[i] / (d[i] - d[j])));
        }
    }
    drop_dups(out)
}

/// Face `tag` split by the loop: the part inside gets `fresh`.
fn imprint(b: &Body, tag: u32, n: V3, ring: &[V3], fresh: u32, tol: f64) -> R<Body> {
    let mut cells = vec![];
    face(ring, n, 0, &mut cells);
    let mut polys = vec![];
    let mut hit = false;
    let keep = |pts: Vec<V3>, t: u32, plane: Plane, polys: &mut Vec<Poly>| -> bool {
        if pts.len() < 3 {
            return false;
        }
        match Poly::new(pts, t) {
            Some(mut q) if q.area() > tol * tol => {
                q.plane = plane;
                polys.push(q);
                true
            }
            _ => false,
        }
    };
    for p in &b.polys {
        if p.tag != tag {
            polys.push(p.clone());
            continue;
        }
        let mut rest = vec![p.pts.clone()];
        for q in &cells {
            let mut next = vec![];
            for r in rest {
                let mut cur = r;
                let k = q.pts.len();
                for i in 0..k {
                    let (a, c) = (q.pts[i], q.pts[(i + 1) % k]);
                    let m = n.cross(c - a).normalized();
                    let w = m.dot(a);
                    let out = clip_ge(&cur, -m, -w, tol);
                    if out.len() >= 3 {
                        next.push(out);
                    }
                    cur = clip_ge(&cur, m, w, tol);
                    if cur.len() < 3 {
                        break;
                    }
                }
                if cur.len() >= 3 {
                    hit |= keep(cur, fresh, p.plane, &mut polys);
                }
            }
            rest = next;
        }
        for r in rest {
            keep(r, tag, p.plane, &mut polys);
        }
    }
    if !hit {
        return Err(MassError::RegionOutside);
    }
    Ok(Body { polys }.welded(tol))
}

/// Push/pull of a region drawn on planar face `tag`: the face is split by the loop and only the
/// region moves by `d` (> 0 boss, < 0 pocket or hole; 0 imprints the region as a new face).
/// New faces: region cap `ftag(op, 1)`, side of loop edge i `ftag(op, 2 + i)`; a side flush
/// with a neighbour face continues that face.
pub fn push_region(b: &Body, tag: u32, region: &[V3], d: f64, op: u32, tol: Tol) -> R<Body> {
    let pl = planar_face(b, tag, tol.length)?;
    if !d.is_finite() {
        return Err(MassError::BadParam("push/pull distance"));
    }
    let ring = region_ring(&pl, region, tol)?;
    if ring.len() + 2 > MAX_LOCAL as usize {
        return Err(MassError::BadParam("region loop has too many edges"));
    }
    let lt = tol.length * 10.0;
    if !inside_face(b, tag, pl.n, &ring, lt) {
        return Err(MassError::RegionOutside);
    }
    if d.abs() <= tol.length {
        return imprint(b, tag, pl.n, &ring, ftag(op, 1), tol.length);
    }
    let n = pl.n;
    let sides: Vec<u32> = (0..ring.len() as u32).map(|i| ftag(op, 2 + i)).collect();
    let (base, cap0, cap1) = if d > 0.0 {
        (ring.clone(), ftag(op, 0), ftag(op, 1))
    } else {
        let r: Vec<V3> = ring.iter().map(|q| *q + n * d).collect();
        (r, ftag(op, 1), ftag(op, 0))
    };
    let mut tool = prism_tagged(&base, n * d.abs(), &sides, cap0, cap1);
    adopt(&mut tool, b, u32::MAX, tol.length);
    let out = if d > 0.0 {
        b.union(&tool, tol)
    } else {
        b.subtract(&tool, tol)
    };
    nonempty(out)
}

// ---------- fillets and chamfers on edge sets ----------

/// Edge blend: constant-radius fillet (`seg` arc steps) or equal-distance chamfer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BlendKind {
    Fillet { r: f64, seg: usize },
    Chamfer { d: f64 },
}

impl BlendKind {
    fn size(&self) -> f64 {
        match *self {
            BlendKind::Fillet { r, .. } => r,
            BlendKind::Chamfer { d } => d,
        }
    }
    fn with_size(&self, s: f64) -> BlendKind {
        match *self {
            BlendKind::Fillet { seg, .. } => BlendKind::Fillet { r: s, seg },
            BlendKind::Chamfer { .. } => BlendKind::Chamfer { d: s },
        }
    }
}

/// A selected edge with its frame and the trimmed parameter range of its tool.
#[derive(Clone, Debug)]
struct Sel {
    f: [u32; 2],
    a: V3,
    t: V3,
    len: f64,
    u: [V3; 2],
    n: [V3; 2],
    bis: V3,
    phi: f64,
    concave: bool,
    /// Distance of the tangent (or chamfer) lines from the edge.
    set: f64,
    lo: f64,
    hi: f64,
    /// Corner index at the start / end (tool ends exactly there, no extension).
    c_lo: Option<usize>,
    c_hi: Option<usize>,
    /// Start / end at a vertex shared with exactly one other selected edge (mitre).
    mitre: [bool; 2],
    /// Start / end is an interior cut of a split tool: no extension.
    split: [bool; 2],
}

/// A vertex where n >= 4 selected edges of one kind (or 3 not bounding a trihedral corner but
/// going all around it) meet: the edge tools stop at setbacks and an n-sided patch spans them.
#[derive(Clone, Debug)]
struct Setback {
    v: V3,
    /// (edge index, edge starts here) in cyclic order; edge i lies between faces i and i + 1.
    ends: Vec<(usize, bool)>,
    faces: Vec<u32>,
    /// Normals of the convex cone around the vertex (material when convex, void when concave).
    m: Vec<V3>,
    /// Outward axis of that cone.
    w: V3,
}

/// A trihedral vertex where three selected edges of one kind meet.
#[derive(Clone, Debug)]
struct Corner {
    v: V3,
    /// (edge index, edge starts here)
    ends: [(usize, bool); 3],
    faces: [u32; 3],
    /// Fillet: ball centre; chamfer: unused.
    c: V3,
    /// Point on each face where the blends of its two edges meet.
    q: [V3; 3],
    concave: bool,
}

struct Plan {
    sel: Vec<Sel>,
    corners: Vec<Corner>,
    setbacks: Vec<Setback>,
}

/// Why a plan failed: the size does not fit (bisection looks for the largest that does) or the
/// selection itself is invalid.
enum PlanErr {
    Size,
    Hard(MassError),
}

fn resolve(b: &Body, edges: &[(u32, u32)], tol: f64) -> R<Vec<Sel>> {
    if edges.is_empty() {
        return Err(MassError::BadParam("no edges"));
    }
    let mut out: Vec<Sel> = vec![];
    for (i, &(f1, f2)) in edges.iter().enumerate() {
        if edges[..i]
            .iter()
            .any(|&(x, y)| (x == f1 && y == f2) || (x == f2 && y == f1))
        {
            return Err(MassError::BadParam("edge listed twice"));
        }
        let e = edge_between(b, f1, f2, tol)?;
        let (t, u1, u2) = e.frame();
        let phi = u1.dot(u2).clamp(-1.0, 1.0).acos();
        if !(phi > 1e-6 && phi < std::f64::consts::PI - 1e-6) {
            return Err(MassError::Coplanar);
        }
        let len = (e.b - e.a).len();
        out.push(Sel {
            f: [f1, f2],
            a: e.a,
            t,
            len,
            u: [u1, u2],
            n: [e.n1, e.n2],
            bis: (u1 + u2).normalized(),
            phi,
            concave: e.concave(),
            set: 0.0,
            lo: 0.0,
            hi: len,
            c_lo: None,
            c_hi: None,
            mitre: [false; 2],
            split: [false; 2],
        });
    }
    Ok(out)
}

fn face_normal_of(s: &Sel, f: u32) -> Option<(V3, V3)> {
    (0..2).find(|k| s.f[*k] == f).map(|k| (s.n[k], s.u[k]))
}

fn plan(b: &Body, base: &[Sel], kind: BlendKind, tol: f64) -> Result<Plan, PlanErr> {
    let mut sel = base.to_vec();
    for s in &mut sel {
        s.set = match kind {
            BlendKind::Fillet { r, .. } => r / (s.phi / 2.0).tan(),
            BlendKind::Chamfer { d } => d,
        };
        s.lo = 0.0;
        s.hi = s.len;
        s.c_lo = None;
        s.c_hi = None;
        s.mitre = [false; 2];
        s.split = [false; 2];
    }
    // vertices shared by selected edges
    let mut verts: Vec<(V3, Vec<(usize, bool)>)> = vec![];
    for (i, s) in sel.iter().enumerate() {
        for (p, start) in [(s.a, true), (s.a + s.t * s.len, false)] {
            match verts.iter_mut().find(|v| (v.0 - p).len() < tol * 10.0) {
                Some(v) => v.1.push((i, start)),
                None => verts.push((p, vec![(i, start)])),
            }
        }
    }
    let mut corners = vec![];
    let mut setbacks: Vec<Setback> = vec![];
    for (v, ends) in verts {
        if ends.len() == 2 {
            for &(i, start) in &ends {
                sel[i].mitre[if start { 0 } else { 1 }] = true;
            }
        }
        if ends.len() < 3 {
            continue;
        }
        let concave = sel[ends[0].0].concave;
        if ends.iter().any(|e| sel[e.0].concave != concave) {
            return Err(PlanErr::Hard(MassError::Corner(
                "convex and concave edges meet at a blended vertex: blend them in separate \
                 steps, the convex edges first, then the concave ones",
            )));
        }
        let mut faces: Vec<u32> = ends.iter().flat_map(|e| sel[e.0].f).collect();
        faces.sort_unstable();
        faces.dedup();
        if ends.len() > 3 || faces.len() != 3 {
            let sb = setback_at(&mut sel, v, &ends, concave, setbacks.len())?;
            setbacks.push(sb);
            continue;
        }
        let faces = [faces[0], faces[1], faces[2]];
        let nrm = |f: u32| -> V3 {
            ends.iter()
                .find_map(|e| face_normal_of(&sel[e.0], f).map(|x| x.0))
                .unwrap_or(v3(0., 0., 1.))
        };
        let ns = [nrm(faces[0]), nrm(faces[1]), nrm(faces[2])];
        let ends3 = [ends[0], ends[1], ends[2]];
        let (c, q) = match kind {
            BlendKind::Fillet { r, .. } => {
                let s = if concave { r } else { -r };
                let w = [0, 1, 2].map(|k| ns[k].dot(v) + s);
                let Some(c) = solve3(ns, w) else {
                    return Err(PlanErr::Hard(MassError::Corner("degenerate corner")));
                };
                (c, [0, 1, 2].map(|k| c - ns[k] * s))
            }
            BlendKind::Chamfer { d } => {
                // on each face: the point at distance d from both of its edges
                let mut q = [v; 3];
                for k in 0..3 {
                    let us: Vec<V3> = ends3
                        .iter()
                        .filter_map(|e| face_normal_of(&sel[e.0], faces[k]).map(|x| x.1))
                        .collect();
                    if us.len() != 2 {
                        return Err(PlanErr::Hard(MassError::Corner("degenerate corner")));
                    }
                    let w = [ns[k].dot(v), us[0].dot(v) + d, us[1].dot(v) + d];
                    q[k] = solve3([ns[k], us[0], us[1]], w)
                        .ok_or(PlanErr::Hard(MassError::Corner("degenerate corner")))?;
                }
                (v, q)
            }
        };
        let ci = corners.len();
        if let BlendKind::Fillet { .. } = kind {
            // fillet tools end on the planes through the ball centre; chamfer tools meet in a
            // mitre and the flat blend is cut afterwards
            for &(i, start) in &ends3 {
                let s = &mut sel[i];
                let at = (c - s.a).dot(s.t);
                if start {
                    s.lo = at;
                    s.c_lo = Some(ci);
                } else {
                    s.hi = at;
                    s.c_hi = Some(ci);
                }
            }
        }
        corners.push(Corner {
            v,
            ends: ends3,
            faces,
            c,
            q,
            concave,
        });
    }
    // every tangent line stays on its face and clears the strips of the other selected edges
    // (edges sharing a vertex meet in a mitre or a vertex blend, their strips may overlap)
    let ends = |s: &Sel| [s.a, s.a + s.t * s.len];
    let touch = |x: &Sel, y: &Sel| {
        ends(x)
            .iter()
            .any(|p| ends(y).iter().any(|q| (*p - *q).len() < tol * 10.0))
    };
    for (i, s) in sel.iter().enumerate() {
        if !(s.hi - s.lo > tol * 10.0) || s.lo < -tol || s.hi > s.len + tol {
            return Err(PlanErr::Size);
        }
        // At a free end (no mitre, no vertex blend) the faces that meet there trim the strip, so
        // the tangent line may run off the face within two set-backs of the end.
        let span = s.hi - s.lo;
        let margin = (2.0 * s.set).min(0.2 * span);
        let free_lo = s.c_lo.is_none() && !s.mitre[0];
        let free_hi = s.c_hi.is_none() && !s.mitre[1];
        for k in 0..2 {
            let f = s.f[k];
            for frac in [0.02, 0.25, 0.5, 0.75, 0.98] {
                let mut x = s.lo + span * frac;
                if frac < 0.1 && free_lo {
                    x = x.max(s.lo + margin);
                }
                if frac > 0.9 && free_hi {
                    x = x.min(s.hi - margin);
                }
                let p = s.a + s.t * x + s.u[k] * s.set;
                if !b
                    .polys
                    .iter()
                    .any(|g| g.tag == f && on_poly(g, p, tol * 10.0))
                {
                    return Err(PlanErr::Size);
                }
                for (j, o) in sel.iter().enumerate() {
                    if j == i || touch(s, o) {
                        continue;
                    }
                    let Some((_, uo)) = face_normal_of(o, f) else {
                        continue;
                    };
                    let along = (p - o.a).dot(o.t);
                    if along > tol && along < o.len - tol && (p - o.a).dot(uo) < o.set - tol {
                        return Err(PlanErr::Size);
                    }
                }
            }
        }
    }
    // chamfer corner: the corner plane must not enter what is left of the faces
    if let BlendKind::Chamfer { .. } = kind {
        for c in &corners {
            let nc = (c.q[1] - c.q[0]).cross(c.q[2] - c.q[0]);
            let nc = if nc.dot(c.v - c.q[0]) < 0.0 { -nc } else { nc };
            if nc.dot(c.v - c.q[0]) <= tol * nc.len() {
                return Err(PlanErr::Size);
            }
            for &(i, start) in &c.ends {
                let s = &sel[i];
                let away = if start { s.t } else { -s.t };
                if nc.normalized().dot(away) > 1e-9 {
                    return Err(PlanErr::Hard(MassError::Corner(
                        "chamfer corner is too skewed for a flat blend",
                    )));
                }
            }
        }
    }
    for sb in &setbacks {
        if setback_patch(&sel, sb, kind, 0, 0, tol).is_none() {
            return Err(PlanErr::Size);
        }
    }
    Ok(Plan {
        sel,
        corners,
        setbacks,
    })
}

/// Cyclic order, cone and setbacks of a vertex blend; trims the edge tools at the setbacks.
fn setback_at(
    sel: &mut [Sel],
    v: V3,
    ends: &[(usize, bool)],
    concave: bool,
    k: usize,
) -> Result<Setback, PlanErr> {
    let around = || {
        PlanErr::Hard(MassError::Corner(
            "blended edges at a vertex must go all around it: pick every edge there, or \
             blend at most two of them in one step",
        ))
    };
    let degenerate = || PlanErr::Hard(MassError::Corner("degenerate corner"));
    let n = ends.len();
    // each face of the vertex bounds exactly two of its edges
    let mut order = vec![ends[0]];
    let mut faces = vec![sel[ends[0].0].f[0]];
    let mut cur = sel[ends[0].0].f[1];
    while order.len() < n {
        faces.push(cur);
        let next = ends
            .iter()
            .find(|e| !order.iter().any(|o| o.0 == e.0) && sel[e.0].f.contains(&cur))
            .copied();
        let Some(e) = next else {
            return Err(around());
        };
        order.push(e);
        let f = sel[e.0].f;
        cur = if f[0] == cur { f[1] } else { f[0] };
    }
    let mut distinct = faces.clone();
    distinct.sort_unstable();
    distinct.dedup();
    if cur != faces[0] || distinct.len() != n {
        return Err(around());
    }
    let sign = if concave { -1.0 } else { 1.0 };
    let mut nf = vec![];
    for (i, f) in faces.iter().enumerate() {
        let (nrm, _) = face_normal_of(&sel[order[i].0], *f).ok_or_else(degenerate)?;
        nf.push(nrm);
    }
    let m: Vec<V3> = nf.iter().map(|x| *x * sign).collect();
    let w = m.iter().fold(v3(0., 0., 0.), |a, x| a + *x);
    if w.len() < 1e-9 {
        return Err(degenerate());
    }
    let w = w.normalized();
    if m.iter().any(|x| x.dot(w) < 0.05) {
        return Err(PlanErr::Hard(MassError::Corner(
            "the vertex is too sharp for a vertex blend: blend fewer of its edges in one step",
        )));
    }
    // on face i: where the blend lines of its two edges cross
    let mut q = vec![];
    for i in 0..n {
        let (ea, eb) = (&sel[order[(i + n - 1) % n].0], &sel[order[i].0]);
        let (ua, ub) = match (face_normal_of(ea, faces[i]), face_normal_of(eb, faces[i])) {
            (Some(a), Some(b)) => (a.1, b.1),
            _ => return Err(degenerate()),
        };
        let w3 = [nf[i].dot(v), ua.dot(v) + ea.set, ub.dot(v) + eb.set];
        q.push(solve3([nf[i], ua, ub], w3).ok_or_else(degenerate)?);
    }
    for i in 0..n {
        let (e, start) = order[i];
        let s = &mut sel[e];
        let d = if start { s.t } else { -s.t };
        let a = (q[i] - v).dot(d).max((q[(i + 1) % n] - v).dot(d)).max(0.0) + 0.5 * s.set;
        if start {
            s.lo = a;
            s.c_lo = Some(k);
        } else {
            s.hi = s.len - a;
            s.c_hi = Some(k);
        }
    }
    Ok(Setback {
        v,
        ends: order,
        faces,
        m,
        w,
    })
}

fn put_tri(out: &mut Vec<Poly>, a: V3, b: V3, c: V3, flip: bool, tag: u32) {
    let pts = if flip { vec![a, c, b] } else { vec![a, b, c] };
    if let Some(p) = Poly::new(pts, tag) {
        out.push(p);
    }
}

/// Setback vertex blend: an n-sided patch over the ends of the edge blends and the face lines
/// between them (rings toward a centre lifted into a dome), closed by a fan from a point
/// beyond the vertex. None when the patch would leave the cone around the vertex.
fn setback_patch(
    sel: &[Sel],
    sb: &Setback,
    kind: BlendKind,
    tag: u32,
    op: u32,
    tol: f64,
) -> Option<Vec<Poly>> {
    let mut ring: Vec<V3> = vec![];
    let mut cap: Vec<V3> = vec![];
    for (i, &(e, start)) in sb.ends.iter().enumerate() {
        let s = &sel[e];
        let sec = section(s, if start { s.lo } else { s.hi }, kind);
        let mut arc = sec[1..].to_vec();
        if s.f[0] != sb.faces[i] {
            arc.reverse();
        }
        cap.extend([arc[0], sec[0], arc[arc.len() - 1]]);
        ring.extend(arc);
    }
    let cp = ring.iter().fold(v3(0., 0., 0.), |a, p| a + *p) * (1.0 / ring.len() as f64);
    let inside = |p: V3, slack: f64| sb.m.iter().all(|m| m.dot(p - sb.v) <= slack);
    if !inside(cp, -tol) {
        return None;
    }
    let turn = |pts: &[V3]| -> f64 {
        (0..pts.len())
            .map(|k| (pts[k] - cp).cross(pts[(k + 1) % pts.len()] - cp).dot(sb.w))
            .sum()
    };
    let (ccw_ring, ccw_cap) = (turn(&ring) > 0.0, turn(&cap) > 0.0);
    let (rows, mut h) = match kind {
        BlendKind::Fillet { seg, .. } => ((seg / 2).max(2), 0.5 * (sb.v - cp).dot(sb.w)),
        BlendKind::Chamfer { .. } => (1, 0.0),
    };
    let (rings, top) = loop {
        let rings: Vec<Vec<V3>> = (0..rows)
            .map(|j| {
                let s = 1.0 - j as f64 / rows as f64;
                ring.iter()
                    .map(|p| cp + (*p - cp) * s + sb.w * (h * (1.0 - s * s)))
                    .collect()
            })
            .collect();
        let top = cp + sb.w * h;
        if rings[1..].iter().flatten().all(|p| inside(*p, 0.0)) && inside(top, -tol) {
            break (rings, top);
        }
        if h <= 0.0 {
            return None;
        }
        h = if h < tol { 0.0 } else { h * 0.5 };
    };
    let mut out = vec![];
    // patch faces the kept side (against w), the fan faces away from it
    let m = ring.len();
    for j in 0..rings.len() {
        let o = &rings[j];
        for k in 0..m {
            let k1 = (k + 1) % m;
            if let Some(i) = rings.get(j + 1) {
                put_tri(&mut out, o[k], o[k1], i[k1], ccw_ring, tag);
                put_tri(&mut out, o[k], i[k1], i[k], ccw_ring, tag);
            } else {
                put_tri(&mut out, top, o[k], o[k1], ccw_ring, tag);
            }
        }
    }
    let apex = sb.v + sb.w * (kind.size() + (sb.v - cp).len());
    for k in 0..cap.len() {
        let k1 = (k + 1) % cap.len();
        put_tri(&mut out, apex, cap[k], cap[k1], !ccw_cap, ftag(op, 0));
    }
    Some(out)
}

/// Points of the arc around `c` from `p` to `q` (`n` steps, ends exact).
fn arc(c: V3, p: V3, q: V3, n: usize) -> Vec<V3> {
    let (v1, v2) = (p - c, q - c);
    let ang = (v1.dot(v2) / (v1.len() * v2.len())).clamp(-1.0, 1.0).acos();
    let ax = v1.cross(v2);
    let mut out = vec![p];
    if ax.len() > 1e-15 && n > 1 {
        let ax = ax.normalized();
        for k in 1..n {
            let a = ang * k as f64 / n as f64;
            let (s, co) = a.sin_cos();
            out.push(c + v1 * co + ax.cross(v1) * s + ax * (ax.dot(v1) * (1.0 - co)));
        }
    } else {
        for k in 1..n {
            out.push(p + (q - p) * (k as f64 / n as f64));
        }
    }
    out.push(q);
    out
}

const CAP_LO: u32 = u32::MAX - 3;
const CAP_HI: u32 = u32::MAX - 4;

/// Edge section at parameter `at`: [extra point outside the wedge, tangent point on face 1,
/// arc or chamfer line, tangent point on face 2].
fn section(s: &Sel, at: f64, kind: BlendKind) -> Vec<V3> {
    let o = s.a + s.t * at;
    let (p1, p2) = (o + s.u[0] * s.set, o + s.u[1] * s.set);
    let x = o - s.bis * kind.size();
    match kind {
        BlendKind::Fillet { r, seg } => {
            let c = o + s.bis * (r / (s.phi / 2.0).sin());
            let mut v = vec![x];
            v.extend(arc(c, p1, p2, seg));
            v
        }
        BlendKind::Chamfer { .. } => vec![x, p1, p2],
    }
}

fn surf_tags(i: usize, n_side: usize, op: u32) -> Vec<u32> {
    let base = 2 + 3 * i as u32;
    let mut v = vec![ftag(op, base + 1)];
    v.extend(std::iter::repeat_n(ftag(op, base), n_side));
    v.push(ftag(op, base + 2));
    v
}

/// Tool of one edge between its trimmed ends; free ends are extended past the body when the
/// extension stays clear of it (cut: outside, fill: inside).
fn edge_prism(b: &Body, s: &Sel, kind: BlendKind, op: u32, i: usize) -> Body {
    let margin = 2.0 * kind.size().max(s.set);
    let probe = s.bis * (s.set * 0.25);
    let ext = |at: f64, dir: V3| -> f64 {
        let p = s.a + s.t * at + dir * (margin * 0.5) + probe;
        if b.contains(p) == s.concave {
            margin
        } else {
            0.0
        }
    };
    let m0 = if s.c_lo.is_some() || s.split[0] {
        0.0
    } else {
        ext(s.lo, -s.t)
    };
    let m1 = if s.c_hi.is_some() || s.split[1] {
        0.0
    } else {
        ext(s.hi, s.t)
    };
    let sec = section(s, s.lo - m0, kind);
    let n_side = sec.len() - 2;
    let sides = surf_tags(i, n_side, op);
    let d = s.t * (s.hi - s.lo + m0 + m1);
    let mut p = prism_tagged(&sec, d, &sides, CAP_LO, CAP_HI);
    // the end at a corner joins the corner tool directly
    p.polys.retain(|q| {
        !((q.tag == CAP_LO && s.c_lo.is_some()) || (q.tag == CAP_HI && s.c_hi.is_some()))
    });
    for q in &mut p.polys {
        if q.tag == CAP_LO || q.tag == CAP_HI {
            q.tag = ftag(op, 0);
        }
    }
    p
}

fn tri(a: V3, b: V3, c: V3, toward: V3, tag: u32, out: &mut Vec<Poly>) {
    let n = (b - a).cross(c - a);
    let cen = (a + b + c) * (1.0 / 3.0);
    let pts = if n.dot(toward - cen) >= 0.0 {
        vec![a, b, c]
    } else {
        vec![a, c, b]
    };
    if let Some(p) = Poly::new(pts, tag) {
        out.push(p);
    }
}

/// Spherical vertex blend: the sphere patch between the three edge arcs (outward = toward the
/// ball centre) closed by a fan from a point beyond the vertex through the edges' extra points.
fn fillet_corner(pl: &Plan, k: usize, kind: BlendKind, tag: u32, op: u32) -> Vec<Poly> {
    let seg = match kind {
        BlendKind::Fillet { seg, .. } => seg,
        BlendKind::Chamfer { .. } => 1,
    };
    let c = &pl.corners[k];
    let mut out = vec![];
    let (qa, qb, qc) = (c.q[0], c.q[1], c.q[2]);
    let rows: Vec<Vec<V3>> = {
        let ea = arc(c.c, qc, qa, seg);
        let eb = arc(c.c, qc, qb, seg);
        (0..=seg)
            .map(|i| {
                if i == 0 {
                    vec![qc]
                } else {
                    arc(c.c, ea[i], eb[i], i)
                }
            })
            .collect()
    };
    for i in 0..seg {
        let (r0, r1) = (&rows[i], &rows[i + 1]);
        for j in 0..=i {
            tri(r1[j], r1[j + 1], r0[j], c.c, tag, &mut out);
        }
        for j in 0..i {
            tri(r0[j], r1[j + 1], r0[j + 1], c.c, tag, &mut out);
        }
    }
    // hexagon q_f, x(edge f-g), q_g, ... around the corner
    let x_of = |f: u32, g: u32| -> V3 {
        let (i, start) = *c
            .ends
            .iter()
            .find(|e| {
                let s = &pl.sel[e.0];
                (s.f[0] == f && s.f[1] == g) || (s.f[0] == g && s.f[1] == f)
            })
            .expect("corner edges bound its faces");
        let s = &pl.sel[i];
        let at = if start { s.lo } else { s.hi };
        s.a + s.t * at - s.bis * kind.size()
    };
    let f = c.faces;
    let mut hex = vec![
        c.q[0],
        x_of(f[0], f[1]),
        c.q[1],
        x_of(f[1], f[2]),
        c.q[2],
        x_of(f[2], f[0]),
    ];
    let apex = c.v + (c.v - c.c);
    let n0 = (hex[0] - apex).cross(hex[1] - apex);
    let cen = (apex + hex[0] + hex[1]) * (1.0 / 3.0);
    if n0.dot(cen - c.c) < 0.0 {
        hex.reverse();
    }
    for i in 0..6 {
        if let Some(p) = Poly::new(vec![apex, hex[i], hex[(i + 1) % 6]], ftag(op, 0)) {
            out.push(p);
        }
    }
    out
}

/// Fillet or chamfer of the edges between face pairs `edges`, one operation. Convex edges are
/// cut, concave edges filled. Two blended edges at a vertex meet in a mitre; three of one kind
/// at a trihedral vertex get a vertex blend (fillet: sphere patch, chamfer: flat triangle).
/// Tags: blend surface of edge i `ftag(op, 2 + 3i)`, vertex blend k `ftag(op, 2 + 3n + k)`.
/// A size the faces cannot hold: `TooLarge { value, max }`.
pub fn blend_edges(
    b: &Body,
    edges: &[(u32, u32)],
    kind: BlendKind,
    op: u32,
    tol: Tol,
) -> R<Body> {
    let what = match kind {
        BlendKind::Fillet { .. } => "fillet radius",
        BlendKind::Chamfer { .. } => "chamfer distance",
    };
    let size = pos(kind.size(), what)?;
    let kind = match kind {
        BlendKind::Fillet { r, seg } => BlendKind::Fillet {
            r,
            seg: seg.clamp(1, MAX_SEG),
        },
        k => k,
    };
    let lt = tol.length;
    let base = resolve(b, edges, lt)?;
    if 2 + 4 * base.len() as u64 > MAX_LOCAL as u64 {
        return Err(MassError::BadParam("too many edges in one blend"));
    }
    let pl = match plan(b, &base, kind, lt) {
        Ok(p) => p,
        Err(PlanErr::Hard(e)) => return Err(e),
        Err(PlanErr::Size) => {
            let (mut lo, mut hi) = (0.0, size);
            for _ in 0..48 {
                let mid = 0.5 * (lo + hi);
                if plan(b, &base, kind.with_size(mid), lt).is_ok() {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            return Err(MassError::TooLarge {
                value: size,
                max: lo,
            });
        }
    };
    let n = pl.sel.len();
    // edges joined through fillet vertex blends form one tool assembled without booleans
    let mut comp: Vec<usize> = (0..n).collect();
    fn root(c: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while c[i] != i {
            c[i] = c[c[i]];
            i = c[i];
        }
        i
    }
    let blended = matches!(kind, BlendKind::Fillet { .. });
    let joins = pl
        .corners
        .iter()
        .filter(|_| blended)
        .map(|c| c.ends.to_vec())
        .chain(pl.setbacks.iter().map(|s| s.ends.clone()));
    for ends in joins {
        let r0 = root(&mut comp, ends[0].0);
        for e in &ends[1..] {
            let r = root(&mut comp, e.0);
            if r != r0 {
                comp[r] = r0;
            }
        }
    }
    let mut parts: Vec<(usize, Vec<Poly>, bool)> = vec![];
    for (i, s) in pl.sel.iter().enumerate() {
        let r = root(&mut comp, i);
        let mut pieces = vec![(r, s.clone())];
        if (s.c_lo.is_some() && s.mitre[1]) || (s.c_hi.is_some() && s.mitre[0]) {
            // a tool welded to a vertex blend must not overlap its mitre partner inside one
            // part: the half at the mitre is a separate tool
            let mid = 0.5 * (s.lo + s.hi);
            let (mut lo, mut hi) = (s.clone(), s.clone());
            lo.hi = mid;
            lo.c_hi = None;
            lo.split[1] = true;
            hi.lo = mid;
            hi.c_lo = None;
            hi.split[0] = true;
            let (near, far) = if s.c_lo.is_some() { (lo, hi) } else { (hi, lo) };
            pieces = vec![(r, near), (usize::MAX / 2 + i, far)];
        }
        for (key, piece) in pieces {
            let polys = edge_prism(b, &piece, kind, op, i).polys;
            match parts.iter_mut().find(|p| p.0 == key) {
                Some(p) => p.1.extend(polys),
                None => parts.push((key, polys, s.concave)),
            }
        }
    }
    let n_corner = pl.corners.len() as u32;
    for (k, sb) in pl.setbacks.iter().enumerate() {
        let tag = ftag(op, 2 + 3 * n as u32 + n_corner + k as u32);
        let polys = setback_patch(&pl.sel, sb, kind, tag, op, lt).ok_or(MassError::Corner(
            "degenerate corner",
        ))?;
        let r = root(&mut comp, sb.ends[0].0);
        if let Some(p) = parts.iter_mut().find(|p| p.0 == r) {
            p.1.extend(polys);
        }
    }
    for (k, c) in pl.corners.iter().enumerate() {
        let tag = ftag(op, 2 + 3 * n as u32 + k as u32);
        if blended {
            let r = root(&mut comp, c.ends[0].0);
            let polys = fillet_corner(&pl, k, kind, tag, op);
            if let Some(p) = parts.iter_mut().find(|p| p.0 == r) {
                p.1.extend(polys);
            }
        } else {
            // flat vertex blend: the corner on the vertex side of the plane through the three
            // face points is cut (convex) or filled (concave)
            let nc = (c.q[1] - c.q[0]).cross(c.q[2] - c.q[0]).normalized();
            let nc = if nc.dot(c.v - c.q[0]) < 0.0 { -nc } else { nc };
            let side = if c.concave { -1.0 } else { 1.0 };
            let mut planes = vec![(tag, -nc, -nc.dot(c.q[0]))];
            for f in &c.faces {
                let nf = pl
                    .sel
                    .iter()
                    .find_map(|e| face_normal_of(e, *f).map(|x| x.0))
                    .unwrap_or(nc)
                    * side;
                planes.push((ftag(op, 0), nf, nf.dot(c.v) + size));
            }
            let tool = convex_from_planes(&planes, tol)?;
            parts.push((usize::MAX - k, tool.polys, c.concave));
        }
    }
    let (mut cut, mut fill) = (Body::default(), Body::default());
    for (_, polys, concave) in parts {
        let tool = Body { polys }.welded(lt * 10.0);
        tool.validate(tol).map_err(MassError::Body)?;
        if concave {
            fill = fill.union(&tool, tol);
        } else {
            cut = cut.union(&tool, tol);
        }
    }
    let mut out = b.clone();
    if !cut.is_empty() {
        out = out.subtract(&cut, tol);
    }
    if !fill.is_empty() {
        out = out.union(&fill, tol);
    }
    nonempty(out)
}

// ---------- draft ----------

/// One drafted face: old plane (n, w) and the turned plane (n2, w2).
#[derive(Clone, Copy, Debug)]
struct Drafted {
    tag: u32,
    n: V3,
    w: f64,
    n2: V3,
    w2: f64,
}

/// Turned planes: each face turns about its line on the neutral plane (through `at`, normal
/// `pull`) by `angle`; `angle > 0` makes the body narrower along +pull.
fn draft_planes(
    b: &Body,
    faces: &[u32],
    at: V3,
    pull: V3,
    angle: f64,
    tol: f64,
) -> R<Vec<Drafted>> {
    if faces.is_empty() {
        return Err(MassError::BadParam("no faces to draft"));
    }
    let lim = 89.0_f64.to_radians();
    if !angle.is_finite() || angle == 0.0 || angle.abs() > lim {
        return Err(MassError::BadParam("draft angle"));
    }
    if !(pull.len() > 1e-12) || ![at.x, at.y, at.z].iter().all(|c| c.is_finite()) {
        return Err(MassError::BadParam("draft pull direction"));
    }
    let p = pull.normalized();
    let mut out: Vec<Drafted> = vec![];
    for &f in faces {
        if out.iter().any(|d| d.tag == f) {
            return Err(MassError::BadParam("face listed twice"));
        }
        let pl = planar_face(b, f, tol)?;
        let pp = p - pl.n * pl.n.dot(p);
        if pp.len() < 1e-6 {
            return Err(MassError::BadParam("face is normal to the pull direction"));
        }
        let pp = pp.normalized();
        let (s, c) = angle.sin_cos();
        let n2 = (pl.n * c + pp * s).normalized();
        let h = pl.n.cross(pp).normalized();
        let pts: Vec<V3> = b
            .polys
            .iter()
            .filter(|q| q.tag == f)
            .flat_map(|q| q.pts.iter().copied())
            .collect();
        let cen = pts.iter().fold(v3(0., 0., 0.), |a, q| a + *q) * (1.0 / pts.len().max(1) as f64);
        let x0 = solve3([pl.n, p, h], [pl.w, p.dot(at), h.dot(cen)])
            .ok_or(MassError::BadParam("face is normal to the pull direction"))?;
        out.push(Drafted {
            tag: f,
            n: pl.n,
            w: pl.w,
            n2,
            w2: n2.dot(x0),
        });
    }
    Ok(out)
}

/// Draft: faces turn about their lines on the neutral plane (point `at`, normal `pull`) by
/// `angle` (radians, 0 < |angle| <= 89 deg; > 0 narrows the body along +pull). Each face keeps
/// its tag; neighbours stretch or shrink to meet it. A face that vanishes: `FaceLost`.
pub fn draft(
    b: &Body,
    faces: &[u32],
    at: V3,
    pull: V3,
    angle: f64,
    op: u32,
    tol: Tol,
) -> R<Body> {
    let lt = tol.length;
    let dr = draft_planes(b, faces, at, pull, angle, lt)?;
    let groups = planes(b);
    let whole = |tags: &Vec<u32>| tags.iter().all(|t| faces.contains(t));
    let partial = groups
        .iter()
        .any(|(t, _, _)| t.iter().any(|x| faces.contains(x)) && !whole(t));
    let out = if b.is_convex(lt * 10.0) && !partial {
        let moved: Vec<(u32, V3, f64)> = groups
            .iter()
            .map(|(t, n, w)| match dr.iter().find(|d| t.contains(&d.tag)) {
                Some(d) => (d.tag, d.n2, d.w2),
                None => (t[0], *n, *w),
            })
            .collect();
        let out = convex_from_planes(&moved, tol)?;
        for (t, _, _) in &groups {
            let keep = dr
                .iter()
                .find(|d| t.contains(&d.tag))
                .map_or(t[0], |d| d.tag);
            if out.tag_area(keep) <= lt * lt {
                return Err(MassError::FaceLost(keep));
            }
        }
        out
    } else {
        draft_wedges(b, &dr, op, tol)?
    };
    for d in &dr {
        if out.tag_area(d.tag) <= lt * lt {
            return Err(MassError::FaceLost(d.tag));
        }
    }
    nonempty(out)
}

/// Non-convex draft: per convex piece of a face, the wedge inside the old plane and outside the
/// new one is cut, the opposite wedge filled. Wedges are bounded by the neighbour planes (for a
/// drafted neighbour the looser of its old and new plane at the wedge) and by the planes of
/// internal piece edges, so adjacent pieces tile the face.
fn draft_wedges(b: &Body, dr: &[Drafted], op: u32, tol: Tol) -> R<Body> {
    let lt = tol.length;
    let inner = ftag(op, 2);
    let (mut cut, mut fill) = (Body::default(), Body::default());
    for d in dr {
        for piece in pieces(b, d.tag, lt) {
            let rs = restrictions(b, &piece, &[d.tag], lt * 10.0);
            let k = piece.len();
            let mut edge_planes = vec![];
            for i in 0..k {
                let (a, c) = (piece[i], piece[(i + 1) % k]);
                let m = (a + c) * 0.5;
                let shared = b
                    .polys
                    .iter()
                    .any(|g| g.tag != d.tag && on_poly(g, m, lt * 10.0));
                if !shared {
                    let nn = (c - a).cross(d.n).normalized();
                    edge_planes.push((inner, nn, nn.dot(a)));
                }
            }
            for removing in [true, false] {
                // the piece vertex deepest on the wedge side of the new plane
                let side = |q: &V3| {
                    let s = d.n2.dot(*q) - d.w2;
                    if removing {
                        s
                    } else {
                        -s
                    }
                };
                let probe = piece
                    .iter()
                    .copied()
                    .max_by(|x, y| side(x).total_cmp(&side(y)))
                    .unwrap_or(piece[0]);
                if side(&probe) <= lt {
                    continue;
                }
                let mut pl: Vec<(u32, V3, f64)> = if removing {
                    vec![(inner, d.n, d.w), (d.tag, -d.n2, -d.w2)]
                } else {
                    vec![(inner, -d.n, -d.w), (d.tag, d.n2, d.w2)]
                };
                for &(g, n, w) in &rs {
                    let mut best = (g, n, w);
                    if let Some(o) = dr.iter().find(|o| o.tag == g) {
                        let s = if n.dot(o.n) > 0.0 { 1.0 } else { -1.0 };
                        let (n2, w2) = (o.n2 * s, o.w2 * s);
                        if n2.dot(probe) - w2 < n.dot(probe) - w {
                            best = (g, n2, w2);
                        }
                    }
                    pl.push(best);
                }
                pl.extend(edge_planes.iter().copied());
                let wedge = match convex_from_planes(&pl, tol) {
                    Ok(x) => x,
                    Err(MassError::Empty) => continue,
                    Err(e) => return Err(e),
                };
                if wedge.volume() <= lt * lt * lt {
                    continue;
                }
                if removing {
                    cut = cut.union(&wedge, tol);
                } else {
                    fill = fill.union(&wedge, tol);
                }
            }
        }
    }
    let mut out = b.clone();
    if !cut.is_empty() {
        out = out.subtract(&cut, tol);
    }
    if !fill.is_empty() {
        out = out.union(&fill, tol);
    }
    Ok(out)
}
