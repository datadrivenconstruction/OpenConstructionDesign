//! Free-form modelling shapes: extrusion, revolve, sweep along a 2D/3D path, loft
//! between sections, sweep with transition; face push-pull, chamfers, fillets, shell,
//! face offset, face splitting, sections by storeys and by vertical planes.
//! All results are [`Body`]; invalid input yields [`MassError`] (no NaN and no empty bodies).
//!
//! Face tags: primitives produce local indices (0 = start/bottom, 1 = end/top, 2+… =
//! sides by the *stable index* of the profile edge; all facets of one surface share one index);
//! the caller maps them to [`ftag`] of its own operation (`Body::tagged`). Edge indices are assigned by
//! the history (`*_ids`): inserting a vertex does not shift the indices of other edges. Operations receive
//! `op` and tag new faces `ftag(op, k)`, where `k` comes from the [`FaceKeys`] keys (source face,
//! neighbouring face) rather than from the edge position in the outline; a moved face keeps its tag.

use crate::csg::{drop_collinear, ftag, Body, BodyError, Plane, Poly, LOCAL_BITS};
use crate::math::{v2, v3, Tol, V2, V3};
use crate::poly;

/// Maximum number of arc steps (revolve, fillet).
pub const MAX_SEG: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub enum MassError {
    NoFace(u32),
    /// The face is curved (revolve/loft facets): push-pull and elements on it are not built.
    NonPlanar(u32),
    NoEdge(u32, u32),
    /// The edge faces are coplanar: fillet/chamfer is undefined.
    Coplanar,
    BadParam(&'static str),
    /// The operation is defined only for convex bodies (or non-adjacent faces).
    NonConvex,
    /// Shell: the thickness consumes the whole body, leaving no cavity.
    EmptyCavity,
    Unbounded,
    /// The plane does not intersect the face.
    NoSplit,
    Empty,
    Body(BodyError),
    /// Push/pull target face is not parallel to the pushed face.
    NotParallel(u32, u32),
    /// The drawn region leaves the face.
    RegionOutside,
    /// Vertex where blended edges meet cannot be blended.
    Corner(&'static str),
    /// Blend size the faces cannot hold; `max` is the largest that fits.
    TooLarge { value: f64, max: f64 },
    /// The operation removes this face entirely.
    FaceLost(u32),
}

impl std::fmt::Display for MassError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            MassError::NoFace(t) => write!(f, "no face {t}"),
            MassError::NonPlanar(t) => write!(f, "face {t} is curved"),
            MassError::NoEdge(a, b) => write!(f, "faces {a} and {b} share no edge"),
            MassError::Coplanar => f.write_str("edge faces are coplanar"),
            MassError::BadParam(p) => write!(f, "bad parameter: {p}"),
            MassError::NonConvex => f.write_str("body is not convex"),
            MassError::EmptyCavity => f.write_str("shell thickness leaves no cavity"),
            MassError::Unbounded => f.write_str("planes do not bound a body"),
            MassError::NoSplit => f.write_str("plane does not cross the face"),
            MassError::Empty => f.write_str("result is empty"),
            MassError::Body(e) => write!(f, "invalid body: {e}"),
            MassError::NotParallel(a, b) => write!(f, "faces {a} and {b} are not parallel"),
            MassError::RegionOutside => f.write_str("region leaves the face"),
            MassError::Corner(m) => write!(f, "corner: {m}"),
            MassError::TooLarge { value, max } => {
                write!(f, "size {value} is too large: at most {max:.6}")
            }
            MassError::FaceLost(t) => write!(f, "face {t} would vanish"),
        }
    }
}

type R<T> = Result<T, MassError>;

#[path = "mass_edit.rs"]
mod edit;
pub use edit::{blend_edges, draft, push_distance, push_region, BlendKind, PushTarget};

/// Largest local face index in an operation.
const MAX_LOCAL: u32 = (1 << LOCAL_BITS) - 1;

/// Stable ring edge indices: given ones (one per edge, no repeats) or 0..n.
fn edge_ids(n: usize, ids: &[u32]) -> R<Vec<u32>> {
    if ids.is_empty() {
        return Ok((0..n as u32).collect());
    }
    let mut s = ids.to_vec();
    s.sort_unstable();
    s.dedup();
    if ids.len() != n || s.len() != n {
        return Err(MassError::BadParam("profile edge ids"));
    }
    Ok(ids.to_vec())
}

/// Side of the edge with index `id`: `2 + id` (outside the tag field it is an error, not overlapping faces).
fn plain_local(id: u32) -> R<u32> {
    id.checked_add(2)
        .filter(|l| *l <= MAX_LOCAL)
        .ok_or(MassError::BadParam("profile edge id"))
}

/// Side of edge `id`, sub-index `sub` (an edge piece or path segment, up to 8): `2 + 8·id + sub`.
fn sub_local(id: u32, sub: usize) -> R<u32> {
    let l = 2 + id as u64 * 8 + sub.min(7) as u64;
    if l > MAX_LOCAL as u64 {
        Err(MassError::BadParam("profile edge id"))
    } else {
        Ok(l as u32)
    }
}

/// Keys of the new faces of an operation, stored in the history: `[source face, neighbouring face,
/// occurrence, local index]`. New keys get the next free indices in key order,
/// so a face index does not depend on the edge position in the outline.
pub type FaceKeys = Vec<[u32; 4]>;

/// Local indices for the keys `want` (a missing key is appended with the next free index,
/// starting from `first`).
fn assign(keys: &mut FaceKeys, want: &[[u32; 3]], first: u32) -> R<Vec<u32>> {
    let has = |keys: &FaceKeys, w: &[u32; 3]| keys.iter().position(|k| k[..3] == w[..]);
    let mut fresh: Vec<[u32; 3]> = want
        .iter()
        .filter(|w| has(keys, w).is_none())
        .copied()
        .collect();
    fresh.sort_unstable();
    fresh.dedup();
    let mut next = keys
        .iter()
        .map(|k| k[3] + 1)
        .max()
        .unwrap_or(first)
        .max(first);
    for f in fresh {
        if next > MAX_LOCAL {
            return Err(MassError::BadParam("too many new faces in one operation"));
        }
        keys.push([f[0], f[1], f[2], next]);
        next += 1;
    }
    Ok(want
        .iter()
        .map(|w| has(keys, w).map_or(first, |i| keys[i][3]))
        .collect())
}

fn pos(v: f64, what: &'static str) -> R<f64> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(MassError::BadParam(what))
    }
}

fn orient_outward(mut b: Body) -> Body {
    if b.volume() < 0.0 {
        b = b.flipped();
    }
    b
}

/// Planar polygon (any simple one) → tagged convex pieces; `pts` are counter-clockwise
/// when viewed from the tip of normal `n`.
fn face(pts: &[V3], n: V3, tag: u32, out: &mut Vec<Poly>) {
    let pts = drop_collinear(pts.to_vec(), 1e-12);
    if pts.len() < 3 {
        return;
    }
    if is_convex(&pts, n) {
        if let Some(p) = Poly::new(pts, tag) {
            out.push(p);
        }
        return;
    }
    let (u, w) = basis(n);
    let o = pts[0];
    let ring: Vec<V2> = pts
        .iter()
        .map(|q| v2((*q - o).dot(u), (*q - o).dot(w)))
        .collect();
    for t in poly::triangulate(&ring) {
        if let Some(p) = Poly::new(vec![pts[t[0]], pts[t[1]], pts[t[2]]], tag) {
            out.push(p);
        }
    }
}

fn is_convex(pts: &[V3], n: V3) -> bool {
    let k = pts.len();
    (0..k).all(|i| {
        let (a, b, c) = (pts[i], pts[(i + 1) % k], pts[(i + 2) % k]);
        n.dot((b - a).cross(c - b)) >= -1e-12
    })
}

/// Orthonormal basis of the plane with normal `n` (u × w = n).
pub fn basis(n: V3) -> (V3, V3) {
    let n = n.normalized();
    let a = if n.x.abs() < 0.9 {
        v3(1., 0., 0.)
    } else {
        v3(0., 1., 0.)
    };
    let u = (a - n * a.dot(n)).normalized();
    (u, n.cross(u))
}

fn ring_normal(r: &[V3]) -> V3 {
    let mut n = v3(0., 0., 0.);
    for i in 0..r.len() {
        n = n + r[i].cross(r[(i + 1) % r.len()]);
    }
    n
}

/// Rectangular cuboid.
pub fn cuboid(lo: V3, hi: V3) -> Body {
    let ring = [
        v2(lo.x, lo.y),
        v2(hi.x, lo.y),
        v2(hi.x, hi.y),
        v2(lo.x, hi.y),
    ];
    extrude(&ring, lo.z, hi.z - lo.z)
}

/// Extrusion of a plan outline (any simple one) from `z0` by height `h` along +Z.
/// Tags: 0 = bottom, 1 = top, 2+i = side of edge i.
pub fn extrude(ring: &[V2], z0: f64, h: f64) -> Body {
    let r: Vec<V3> = ring.iter().map(|p| v3(p.x, p.y, z0)).collect();
    prism(&r, v3(0., 0., h))
}

/// Extrusion with stable edge indices: the side of edge i is `2 + ids[i]` (empty means 0..n).
pub fn extrude_ids(ring: &[V2], ids: &[u32], z0: f64, h: f64) -> R<Body> {
    let sides = edge_ids(ring.len(), ids)?
        .into_iter()
        .map(plain_local)
        .collect::<R<Vec<u32>>>()?;
    let r: Vec<V3> = ring.iter().map(|p| v3(p.x, p.y, z0)).collect();
    Ok(prism_tagged(&r, v3(0., 0., h), &sides, 0, 1))
}

/// Extrusion of a planar (3D) outline along vector `d`: 0 = base, 1 = end cap, 2+i = side
/// of edge i of the original ring.
pub fn prism(ring: &[V3], d: V3) -> Body {
    let tags: Vec<u32> = (0..ring.len() as u32).map(|i| 2 + i).collect();
    prism_tagged(ring, d, &tags, 0, 1)
}

/// Prism with explicit tags for the sides (by original ring edges) and the end caps.
pub fn prism_tagged(ring: &[V3], d: V3, sides: &[u32], cap0: u32, cap1: u32) -> Body {
    let n0 = ring.len();
    let mut r = ring.to_vec();
    // edge i of the reversed ring (r[i] -> r[i+1]) is source edge n-2-i
    let flip = ring_normal(&r).dot(d) < 0.0;
    if flip {
        r.reverse();
    }
    let side = |i: usize| {
        let src = if flip { (2 * n0 - 2 - i) % n0 } else { i };
        sides.get(src).copied().unwrap_or(2 + src as u32)
    };
    let n = ring_normal(&r).normalized();
    let mut polys = vec![];
    let bottom: Vec<V3> = r.iter().rev().copied().collect();
    face(&bottom, -n, cap0, &mut polys);
    let top: Vec<V3> = r.iter().map(|p| *p + d).collect();
    face(&top, n, cap1, &mut polys);
    for i in 0..r.len() {
        let (a, b) = (r[i], r[(i + 1) % r.len()]);
        if let Some(p) = Poly::new(vec![a, b, b + d, a + d], side(i)) {
            polys.push(p);
        }
    }
    orient_outward(Body { polys })
}

/// Cylinder (n-gonal prism) of radius `r` about the vertical through `c`.
pub fn cylinder(c: V2, r: f64, z0: f64, h: f64, n: usize) -> Body {
    let ring: Vec<V2> = (0..n.clamp(3, MAX_SEG))
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            v2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect();
    extrude(&ring, z0, h)
}

/// Skin between rings of equal length; end caps from the first and last ring (0 and 1).
/// The side between rings s and s+1 along edge j gets tag `side(s, j)`; non-planar
/// quadrilaterals are split into triangles with the same tag.
pub fn skin(rings: &[Vec<V3>], caps: bool, side: &dyn Fn(usize, usize) -> u32) -> Body {
    let mut polys = vec![];
    let k = rings[0].len();
    for s in 0..rings.len() - 1 {
        let (r0, r1) = (&rings[s], &rings[s + 1]);
        for j in 0..k {
            let (a, b, c, d) = (r0[j], r0[(j + 1) % k], r1[(j + 1) % k], r1[j]);
            let q = vec![a, b, c, d];
            let planar = Plane::of(&q).is_some_and(|pl| q.iter().all(|p| pl.dist(*p).abs() < 1e-9));
            let tag = side(s, j);
            if planar {
                let q = drop_dups(q);
                if q.len() >= 3 {
                    if let Some(p) = Poly::new(q, tag) {
                        polys.push(p);
                    }
                }
            } else {
                for t in [vec![a, b, c], vec![a, c, d]] {
                    if let Some(p) = Poly::new(drop_dups(t), tag) {
                        polys.push(p);
                    }
                }
            }
        }
    }
    if caps {
        let first = &rings[0];
        let last = &rings[rings.len() - 1];
        let n0 = ring_normal(first);
        let rev: Vec<V3> = first.iter().rev().copied().collect();
        face(&rev, -n0, 0, &mut polys);
        face(last, ring_normal(last), 1, &mut polys);
    }
    orient_outward(Body { polys })
}

fn drop_dups(mut v: Vec<V3>) -> Vec<V3> {
    v.dedup_by(|a, b| (*a - *b).len() < 1e-12);
    while v.len() > 1 && (v[0] - v[v.len() - 1]).len() < 1e-12 {
        v.pop();
    }
    v
}

/// Revolve of a profile (r, z), r ≥ 0, about the Z axis by `angle` (rad) in `seg` steps
/// (3..=256). A full turn has no end caps. The side of profile edge j is a single surface `2+j`.
pub fn revolve(profile: &[V2], angle: f64, seg: usize) -> Body {
    revolve_with(profile, angle, seg, &|_s: usize, j: usize| 2 + j as u32)
}

/// Revolve with stable profile edge indices: the surface of edge j is `2 + ids[j]`.
pub fn revolve_ids(profile: &[V2], ids: &[u32], angle: f64, seg: usize) -> R<Body> {
    let locals = edge_ids(profile.len(), ids)?
        .into_iter()
        .map(plain_local)
        .collect::<R<Vec<u32>>>()?;
    Ok(revolve_with(profile, angle, seg, &|_s: usize, j: usize| {
        locals[j]
    }))
}

fn revolve_with(
    profile: &[V2],
    angle: f64,
    seg: usize,
    side: &dyn Fn(usize, usize) -> u32,
) -> Body {
    let full = (angle.abs() - std::f64::consts::TAU).abs() < 1e-12;
    let steps = seg.clamp(3, MAX_SEG);
    let count = if full { steps } else { steps + 1 };
    let rings: Vec<Vec<V3>> = (0..count)
        .map(|i| {
            let a = angle * i as f64 / steps as f64;
            let (s, c) = a.sin_cos();
            profile.iter().map(|p| v3(p.x * c, p.x * s, p.y)).collect()
        })
        .collect();
    if !full {
        return skin(&rings, true, side);
    }
    let mut closed = rings.clone();
    closed.push(rings[0].clone());
    skin(&closed, false, side)
}

/// Frames along a polyline: (tangent, normal, binormal) with parallel transport.
fn frames(path: &[V3]) -> Vec<(V3, V3, V3)> {
    let segs: Vec<V3> = path
        .windows(2)
        .map(|w| (w[1] - w[0]).normalized())
        .collect();
    let t0 = segs[0];
    let up = if t0.z.abs() < 0.9 {
        v3(0., 0., 1.)
    } else {
        v3(1., 0., 0.)
    };
    let mut nrm = (up - t0 * up.dot(t0)).normalized();
    let mut out = vec![(t0, nrm, t0.cross(nrm))];
    for w in segs.windows(2) {
        let (a, b) = (w[0], w[1]);
        let ax = a.cross(b);
        let s = ax.len();
        if s > 1e-12 {
            let k = ax * (1.0 / s);
            let c = a.dot(b);
            nrm = nrm * c + k.cross(nrm) * s + k * (k.dot(nrm) * (1.0 - c));
            nrm = (nrm - b * nrm.dot(b)).normalized();
        }
        out.push((b, nrm, b.cross(nrm)));
    }
    out
}

/// Profile rings along a path: profile `i` is placed at node `i`, mitred along the bisector.
fn place(profiles: &[Vec<V2>], path: &[V3]) -> Vec<Vec<V3>> {
    let fr = frames(path);
    let n = path.len();
    (0..n)
        .map(|i| {
            let seg = if i == 0 { 0 } else { i - 1 };
            let (t, nn, bb) = fr[seg];
            let miter = if i == 0 || i == n - 1 {
                t
            } else {
                (fr[i - 1].0 + fr[i].0).normalized()
            };
            profiles[i]
                .iter()
                .map(|p| {
                    let q = nn * p.x + bb * p.y;
                    // project along the incoming tangent onto the miter plane
                    let k = -q.dot(miter) / t.dot(miter);
                    path[i] + q + t * k
                })
                .collect()
        })
        .collect()
}

fn path_ok(path: &[V3]) -> bool {
    path.len() >= 2 && path.windows(2).all(|w| (w[1] - w[0]).len() > 1e-9)
}

/// Sweep of a profile (in the plane perpendicular to the path: x = normal, y = binormal) along a polyline.
pub fn sweep(profile: &[V2], path: &[V3]) -> R<Body> {
    sweep_ids(profile, &[], path)
}

/// Sweep with stable profile edge indices: the side of edge j on segment s is `2 + 8·ids[j] + s`
/// (segments after the seventh share one surface).
pub fn sweep_ids(profile: &[V2], ids: &[u32], path: &[V3]) -> R<Body> {
    if profile.len() < 3 || !path_ok(path) {
        return Err(MassError::BadParam("sweep profile/path"));
    }
    let ids = edge_ids(profile.len(), ids)?;
    let (c, src) = ccw_src(profile);
    let locals = (0..path.len() - 1)
        .map(|s| {
            src.iter()
                .map(|&e| sub_local(ids[e], s))
                .collect::<R<Vec<u32>>>()
        })
        .collect::<R<Vec<Vec<u32>>>>()?;
    let profiles = vec![c; path.len()];
    Ok(skin(
        &place(&profiles, path),
        true,
        &|s: usize, j: usize| locals[s][j],
    ))
}

/// Sweep along a 2D path in plan at elevation `z`.
pub fn sweep_plan(profile: &[V2], path: &[V2], z: f64) -> R<Body> {
    let p: Vec<V3> = path.iter().map(|q| v3(q.x, q.y, z)).collect();
    sweep(profile, &p)
}

/// Resamples a ring to `n` points evenly along the perimeter starting from vertex 0.
pub fn resample(r: &[V2], n: usize) -> Vec<V2> {
    if r.len() == n {
        return r.to_vec();
    }
    let k = r.len();
    let lens: Vec<f64> = (0..k).map(|i| (r[(i + 1) % k] - r[i]).len()).collect();
    let total: f64 = lens.iter().sum();
    let mut out = Vec::with_capacity(n);
    let (mut seg, mut acc) = (0usize, 0.0);
    for j in 0..n {
        let s = total * j as f64 / n as f64;
        while seg + 1 < k && acc + lens[seg] < s {
            acc += lens[seg];
            seg += 1;
        }
        let t = if lens[seg] > 0.0 {
            (s - acc) / lens[seg]
        } else {
            0.0
        };
        out.push(r[seg] + (r[(seg + 1) % k] - r[seg]) * t);
    }
    out
}

/// Up to `n` vertices by bisecting the longest edges: all corners of the original ring are preserved.
pub fn refine(r: &[V2], n: usize) -> Vec<V2> {
    let src: Vec<usize> = (0..r.len()).collect();
    refine_src(r, &src, n).0
}

/// [`refine`] with edge provenance: for result edge i, (source edge from `src`, sequence number
/// of the piece within that source).
fn refine_src(r: &[V2], src: &[usize], n: usize) -> (Vec<V2>, Vec<(usize, usize)>) {
    let mut out = r.to_vec();
    let mut from = src.to_vec();
    while out.len() < n {
        let k = out.len();
        let i = (0..k)
            .max_by(|a, b| {
                let la = (out[(a + 1) % k] - out[*a]).len();
                let lb = (out[(b + 1) % k] - out[*b]).len();
                la.total_cmp(&lb).then(b.cmp(a))
            })
            .unwrap_or(0);
        let m = (out[i] + out[(i + 1) % k]) * 0.5;
        out.insert(i + 1, m);
        from.insert(i + 1, from[i]);
    }
    let subs = (0..from.len())
        .map(|i| (from[i], from[..i].iter().filter(|e| **e == from[i]).count()))
        .collect();
    (out, subs)
}

/// Rotates the start of ring `r` to minimise the sum of squared distances to `to` (no twisting).
fn align_k(r: &[V2], to: &[V2]) -> usize {
    let n = r.len();
    (0..n)
        .min_by(|a, b| {
            let cost = |s: usize| -> f64 {
                (0..n)
                    .map(|i| {
                        let d = r[(i + s) % n] - to[i];
                        d.dot(d)
                    })
                    .sum()
            };
            cost(*a).total_cmp(&cost(*b))
        })
        .unwrap_or(0)
}

/// Ring made counter-clockwise, and the source edge of each of its edges (on reversal, edge i is
/// edge n−2−i of the original ring).
fn ccw_src(r: &[V2]) -> (Vec<V2>, Vec<usize>) {
    let n = r.len();
    if poly::area(r) < 0.0 {
        let rev: Vec<V2> = r.iter().rev().copied().collect();
        (rev, (0..n).map(|i| (2 * n - 2 - i) % n).collect())
    } else {
        (r.to_vec(), (0..n).collect())
    }
}

/// Sweep with transition: the profile changes smoothly from `p0` to `p1` along the path; differing
/// vertex counts are reconciled while preserving corners.
pub fn swept_blend(p0: &[V2], p1: &[V2], path: &[V3]) -> R<Body> {
    swept_blend_ids(p0, p1, &[], path)
}

/// Sweep with transition and stable edge indices of the start profile: the surface of piece
/// `sub` of edge j is `2 + 8·ids[j] + sub` along the whole path.
pub fn swept_blend_ids(p0: &[V2], p1: &[V2], ids: &[u32], path: &[V3]) -> R<Body> {
    if p0.len() < 3 || p1.len() < 3 || !path_ok(path) {
        return Err(MassError::BadParam("swept blend profiles/path"));
    }
    let ids = edge_ids(p0.len(), ids)?;
    let n = p0.len().max(p1.len());
    let (c0, s0) = ccw_src(p0);
    let (a, subs) = refine_src(&c0, &s0, n);
    let (c1, s1) = ccw_src(p1);
    let (mut b, _) = refine_src(&c1, &s1, n);
    let k = align_k(&b, &a);
    b.rotate_left(k);
    let locals = subs
        .iter()
        .map(|&(e, s)| sub_local(ids[e], s))
        .collect::<R<Vec<u32>>>()?;
    let lens: Vec<f64> = path.windows(2).map(|w| (w[1] - w[0]).len()).collect();
    let total: f64 = lens.iter().sum();
    let mut acc = 0.0;
    let profiles: Vec<Vec<V2>> = (0..path.len())
        .map(|i| {
            if i > 0 {
                acc += lens[i - 1];
            }
            let t = acc / total;
            a.iter().zip(&b).map(|(p, q)| *p + (*q - *p) * t).collect()
        })
        .collect();
    Ok(skin(
        &place(&profiles, path),
        true,
        &|_s: usize, j: usize| locals[j],
    ))
}

/// Loft between horizontal sections at elevations (outline, z), bottom to top. With differing vertex
/// counts, shorter rings are padded by splitting edges (corners preserved), and start vertices
/// are aligned for minimal twist.
pub fn blend(sections: &[(Vec<V2>, f64)]) -> R<Body> {
    blend_ids(sections, &[])
}

/// Loft with stable section edge indices (one index space for all sections;
/// empty means consecutive across sections): the side between sections s and s+1 along piece `sub` of edge e of section s is
/// `2 + 8·id(e) + sub`. Inserting a vertex into a section does not change the indices of other edges.
pub fn blend_ids(sections: &[(Vec<V2>, f64)], ids: &[Vec<u32>]) -> R<Body> {
    if sections.len() < 2
        || sections.iter().any(|s| s.0.len() < 3 || !s.1.is_finite())
        || sections.windows(2).any(|w| w[1].1 <= w[0].1)
        || !(ids.is_empty() || ids.len() == sections.len())
    {
        return Err(MassError::BadParam("blend sections"));
    }
    let mut all: Vec<Vec<u32>> = vec![];
    let mut off = 0u32;
    for (i, s) in sections.iter().enumerate() {
        let k = s.0.len() as u32;
        all.push(match ids.get(i).filter(|x| !x.is_empty()) {
            Some(x) => edge_ids(s.0.len(), x)?,
            None => (off..off + k).collect(),
        });
        off += k;
    }
    let mut flat = all.concat();
    let total = flat.len();
    flat.sort_unstable();
    flat.dedup();
    if flat.len() != total {
        return Err(MassError::BadParam("profile edge ids"));
    }
    let n = sections.iter().map(|s| s.0.len()).max().unwrap_or(3);
    let mut rings: Vec<Vec<V2>> = vec![];
    let mut subs: Vec<Vec<(usize, usize)>> = vec![];
    for s in sections {
        let (c, src) = ccw_src(&s.0);
        let (r, sb) = refine_src(&c, &src, n);
        rings.push(r);
        subs.push(sb);
    }
    for i in 1..rings.len() {
        let k = align_k(&rings[i], &rings[i - 1]);
        rings[i].rotate_left(k);
        subs[i].rotate_left(k);
    }
    let locals = subs
        .iter()
        .zip(&all)
        .map(|(sb, ids)| {
            sb.iter()
                .map(|&(e, s)| sub_local(ids[e], s))
                .collect::<R<Vec<u32>>>()
        })
        .collect::<R<Vec<Vec<u32>>>>()?;
    let r3: Vec<Vec<V3>> = rings
        .iter()
        .zip(sections)
        .map(|(r, s)| r.iter().map(|p| v3(p.x, p.y, s.1)).collect())
        .collect();
    Ok(skin(&r3, true, &|s: usize, j: usize| locals[s][j]))
}

// ---------- operations ----------

/// Normal and pieces of the tagged face.
pub fn face_of(b: &Body, tag: u32) -> Option<(V3, Vec<Vec<V3>>)> {
    let ps: Vec<&Poly> = b.polys.iter().filter(|p| p.tag == tag).collect();
    let n = ps.first()?.plane.n;
    Some((n, ps.iter().map(|p| p.pts.clone()).collect()))
}

/// Planar face or an error (no such face / curved).
pub fn planar_face(b: &Body, tag: u32, tol: f64) -> R<Plane> {
    if !b.polys.iter().any(|p| p.tag == tag) {
        return Err(MassError::NoFace(tag));
    }
    b.face_plane(tag, tol.max(1e-9) * 10.0)
        .ok_or(MassError::NonPlanar(tag))
}

/// Point on a planar convex piece (with tolerance, boundary included).
fn on_poly(p: &Poly, q: V3, tol: f64) -> bool {
    if p.plane.dist(q).abs() > tol {
        return false;
    }
    let k = p.pts.len();
    (0..k).all(|i| {
        let (a, c) = (p.pts[i], p.pts[(i + 1) % k]);
        let e = c - a;
        p.plane.n.dot(e.cross(q - a)) >= -tol * e.len().max(1.0)
    })
}

/// Outlines of face `tag` with edges split by neighbouring faces: (ring, neighbouring tag of ring
/// edge i; `None` means no neighbour).
fn outline_nb(b: &Body, tag: u32, tol: f64) -> Vec<(Vec<V3>, Vec<Option<u32>>)> {
    let others: Vec<&Poly> = b.polys.iter().filter(|p| p.tag != tag).collect();
    b.face_outline(tag, tol)
        .into_iter()
        .map(|r| {
            let (mut ring, mut nb) = (vec![], vec![]);
            for i in 0..r.len() {
                let (a, c) = (r[i], r[(i + 1) % r.len()]);
                let e = c - a;
                let len = e.len();
                let mut ts: Vec<f64> = others
                    .iter()
                    .flat_map(|p| p.pts.iter())
                    .filter_map(|q| {
                        let t = (*q - a).dot(e) / (len * len);
                        let near = (a + e * t - *q).len() < tol;
                        (near && t * len > tol && (1.0 - t) * len > tol).then_some(t)
                    })
                    .collect();
                ts.push(0.0);
                ts.sort_by(f64::total_cmp);
                ts.dedup_by(|x, y| (*x - *y).abs() * len < tol);
                ts.push(1.0);
                for w in ts.windows(2) {
                    let m = a + e * ((w[0] + w[1]) * 0.5);
                    let n = others.iter().find(|p| on_poly(p, m, tol)).map(|p| p.tag);
                    if w[0] > 0.0 && nb.last() == Some(&n) {
                        continue;
                    }
                    ring.push(a + e * w[0]);
                    nb.push(n);
                }
            }
            (ring, nb)
        })
        .collect()
}

/// Faces of the new body lying in the plane of a face of the old body (same orientation) continue it.
fn adopt(tool: &mut Body, b: &Body, skip: u32, tol: f64) {
    for p in &mut tool.polys {
        if p.tag == skip {
            continue;
        }
        if let Some(q) = b.polys.iter().find(|q| {
            q.tag != skip
                && q.plane.n.dot(p.plane.n) > 1.0 - 1e-9
                && (q.plane.w - p.plane.w).abs() < tol
        }) {
            p.tag = q.tag;
        }
    }
}

/// Push-pull: planar face `tag` moves along its normal by `d` (> 0 is outward).
/// The moved face keeps its tag; a side flush with a neighbouring face continues it. Holes
/// of the face remain holes.
pub fn push_pull(b: &Body, tag: u32, d: f64, op: u32, tol: Tol) -> R<Body> {
    push_pull_keyed(b, tag, d, op, &mut vec![], tol)
}

/// [`push_pull`] with persistent side indices: the side along an edge shared with neighbouring face N gets
/// the index of key (face, N, occurrence) from `keys`; new keys are appended.
pub fn push_pull_keyed(
    b: &Body,
    tag: u32,
    d: f64,
    op: u32,
    keys: &mut FaceKeys,
    tol: Tol,
) -> R<Body> {
    let pl = planar_face(b, tag, tol.length)?;
    if !d.is_finite() {
        return Err(MassError::BadParam("push/pull distance"));
    }
    if d.abs() <= tol.length {
        return Ok(b.clone());
    }
    let n = pl.n;
    let rings = outline_nb(b, tag, tol.length);
    if rings.is_empty() {
        return Err(MassError::NoFace(tag));
    }
    let mut want = vec![];
    let mut seen: Vec<u32> = vec![];
    for x in rings.iter().flat_map(|r| r.1.iter()) {
        let nb = x.unwrap_or(u32::MAX);
        let occ = seen.iter().filter(|s| **s == nb).count() as u32;
        seen.push(nb);
        want.push([tag, nb, occ]);
    }
    let mut locals = assign(keys, &want, 2)?.into_iter();
    let (cap0, cap1) = if d > 0.0 {
        (ftag(op, 0), tag)
    } else {
        (tag, ftag(op, 1))
    };
    let (mut outer, mut holes) = (Body::default(), Body::default());
    for (r, _) in &rings {
        let base: Vec<V3> = if d > 0.0 {
            r.to_vec()
        } else {
            r.iter().map(|q| *q + n * d).collect()
        };
        let sides: Vec<u32> = (0..r.len())
            .map(|_| ftag(op, locals.next().unwrap_or(2)))
            .collect();
        let p = prism_tagged(&base, n * d.abs(), &sides, cap0, cap1);
        if ring_normal(r).dot(n) >= 0.0 {
            outer = outer.union(&p, tol);
        } else {
            holes = holes.union(&p, tol);
        }
    }
    let mut tool = outer.subtract(&holes, tol);
    adopt(&mut tool, b, tag, tol.length);
    let out = if d > 0.0 {
        b.union(&tool, tol)
    } else {
        b.subtract(&tool, tol)
    };
    nonempty(out)
}

fn nonempty(b: Body) -> R<Body> {
    if b.is_empty() || b.volume().abs() < 1e-15 {
        Err(MassError::Empty)
    } else {
        Ok(b)
    }
}

/// Body edge between faces: endpoints (in face 1 winding order) and unit outward normals.
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub a: V3,
    pub b: V3,
    pub n1: V3,
    pub n2: V3,
}

impl Edge {
    /// Directions into the faces from the edge: (tangent, into face 1, into face 2).
    pub fn frame(&self) -> (V3, V3, V3) {
        let t = (self.b - self.a).normalized();
        (
            t,
            self.n1.cross(t).normalized(),
            t.cross(self.n2).normalized(),
        )
    }

    /// Concave edge: face 1 extends toward the outward normal of face 2.
    pub fn concave(&self) -> bool {
        self.frame().1.dot(self.n2) > 0.0
    }
}

/// Shared edge of faces `t1` and `t2` (the longest one). An error if a face is missing, they are curved at the edge,
/// there is no shared edge, or the faces are coplanar.
pub fn edge_between(b: &Body, t1: u32, t2: u32, tol: f64) -> R<Edge> {
    let n1 = planar_face(b, t1, tol)?.n;
    let n2 = planar_face(b, t2, tol)?.n;
    if n1.cross(n2).len() < 1e-9 {
        return Err(MassError::Coplanar);
    }
    let r1 = b.face_outline(t1, tol);
    let r2 = b.face_outline(t2, tol);
    let mut best: Option<(f64, V3, V3)> = None;
    for r in &r1 {
        for i in 0..r.len() {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            for s in &r2 {
                for j in 0..s.len() {
                    let (u, v) = (s[j], s[(j + 1) % s.len()]);
                    if (p - v).len() < tol && (q - u).len() < tol {
                        let l = (q - p).len();
                        if best.is_none_or(|bb| l > bb.0) {
                            best = Some((l, p, q));
                        }
                    }
                }
            }
        }
    }
    best.map(|(_, a, b)| Edge { a, b, n1, n2 })
        .ok_or(MassError::NoEdge(t1, t2))
}

/// Profile `sec` (in the plane perpendicular to the edge through `e.a`) is swept along the edge.
/// The end is extended by `margin` only if the extension does not touch neighbouring faces:
/// outside the body for a cut, inside for a build-up.
fn edge_tool(
    b: &Body,
    e: &Edge,
    sec: &[V3],
    probe: V3,
    margin: f64,
    sides: &[u32],
    op: u32,
) -> Body {
    let t = (e.b - e.a).normalized();
    let len = (e.b - e.a).len();
    let add = e.concave();
    let ext = |at: V3, dir: V3| -> f64 {
        let inside = b.contains(at + dir * (margin * 0.5) + (probe - e.a));
        if inside == add {
            margin
        } else {
            0.0
        }
    };
    let (m0, m1) = (ext(e.a, -t), ext(e.b, t));
    let base: Vec<V3> = sec.iter().map(|p| *p - t * m0).collect();
    prism_tagged(&base, t * (len + m0 + m1), sides, ftag(op, 0), ftag(op, 1))
}

fn edge_apply(b: &Body, e: &Edge, tool: &Body, tol: Tol) -> R<Body> {
    let out = if e.concave() {
        b.union(tool, tol)
    } else {
        b.subtract(tool, tol)
    };
    nonempty(out)
}

/// Chamfer with legs `d` along edge `e`: a convex edge is cut away, a concave one is filled.
/// The chamfer face is `ftag(op, 2)`.
pub fn chamfer(b: &Body, e: &Edge, d: f64, op: u32, tol: Tol) -> R<Body> {
    let d = pos(d, "chamfer distance")?;
    let (_, u1, u2) = e.frame();
    let bis = (u1 + u2).normalized();
    if !(bis.len() > 0.5) {
        return Err(MassError::Coplanar);
    }
    let sec = [e.a - bis * d, e.a + u1 * d, e.a + u2 * d];
    // sides: extra→p1, p1→p2 (chamfer face), p2→extra
    let sides = [ftag(op, 3), ftag(op, 2), ftag(op, 4)];
    let tool = edge_tool(b, e, &sec, e.a + bis * (d * 0.25), d, &sides, op);
    edge_apply(b, e, &tool, tol)
}

/// Fillet of radius `r` along an edge (`seg` arc segments, 1..=256): a convex edge
/// is rounded by a cut, a concave one by a build-up. The fillet surface is `ftag(op, 2)`.
pub fn fillet(b: &Body, e: &Edge, r: f64, seg: usize, op: u32, tol: Tol) -> R<Body> {
    let r = pos(r, "fillet radius")?;
    let seg = seg.clamp(1, MAX_SEG);
    let (_, u1, u2) = e.frame();
    let phi = u1.dot(u2).clamp(-1.0, 1.0).acos(); // wedge angle at the edge
    if !(phi > 1e-6 && phi < std::f64::consts::PI - 1e-6) {
        return Err(MassError::Coplanar);
    }
    let tan_d = r / (phi / 2.0).tan();
    let bis = (u1 + u2).normalized();
    let c = e.a + bis * (r / (phi / 2.0).sin());
    let (p1, p2) = (e.a + u1 * tan_d, e.a + u2 * tan_d);
    let sweep = std::f64::consts::PI - phi;
    let (v1, v2) = (p1 - c, p2 - c);
    let ax = v1.cross(v2).normalized();
    let mut sec = vec![e.a - bis * r, p1];
    for k in 1..seg {
        let a = sweep * k as f64 / seg as f64;
        let (s, co) = a.sin_cos();
        let v = v1 * co + ax.cross(v1) * s + ax * (ax.dot(v1) * (1.0 - co));
        sec.push(c + v);
    }
    sec.push(p2);
    // side 0: extra→p1, sides 1..=seg: the arc, last: p2→extra
    let mut sides = vec![ftag(op, 3)];
    sides.extend(std::iter::repeat_n(ftag(op, 2), seg));
    sides.push(ftag(op, 4));
    let tool = edge_tool(b, e, &sec, e.a + bis * (tan_d * 0.25), r, &sides, op);
    edge_apply(b, e, &tool, tol)
}

/// Face planes by tag (for a convex body, one per plane), in order of first
/// appearance.
fn planes(b: &Body) -> Vec<(Vec<u32>, V3, f64)> {
    let mut out: Vec<(Vec<u32>, V3, f64)> = vec![];
    for p in &b.polys {
        match out
            .iter_mut()
            .find(|(_, n, w)| n.dot(p.plane.n) > 1.0 - 1e-9 && (w - p.plane.w).abs() < 1e-9)
        {
            Some(e) => {
                if !e.0.contains(&p.tag) {
                    e.0.push(p.tag);
                }
            }
            None => out.push((vec![p.tag], p.plane.n, p.plane.w)),
        }
    }
    out
}

const BIG_TAG: u32 = u32::MAX - 1;

/// Convex body as an intersection of half-spaces n·x ≤ w.
pub fn convex_from_planes(pl: &[(u32, V3, f64)], tol: Tol) -> R<Body> {
    let big = 1e4;
    let mut b = cuboid(v3(-big, -big, -big), v3(big, big, big));
    b.polys.iter_mut().for_each(|p| p.tag = BIG_TAG);
    for &(tag, n, w) in pl {
        b = clip_half(&b, n, w, tag, tol.length);
        if b.is_empty() {
            return Err(MassError::Empty);
        }
    }
    if b.polys.iter().any(|p| p.tag == BIG_TAG) {
        return Err(MassError::Unbounded);
    }
    nonempty(b)
}

/// Clipping of a convex body by half-space n·x ≤ w; the new face gets `tag`.
fn clip_half(b: &Body, n: V3, w: f64, tag: u32, tol: f64) -> Body {
    // a supporting (redundant) plane must not add a duplicate face
    if b.polys
        .iter()
        .flat_map(|p| p.pts.iter())
        .all(|q| n.dot(*q) - w <= tol)
    {
        return b.clone();
    }
    let mut polys = vec![];
    let mut cut: Vec<V3> = vec![];
    for p in &b.polys {
        let d: Vec<f64> = p.pts.iter().map(|q| n.dot(*q) - w).collect();
        let mut keep = vec![];
        for i in 0..p.pts.len() {
            let j = (i + 1) % p.pts.len();
            if d[i] <= tol {
                keep.push(p.pts[i]);
            }
            if d[i].abs() <= tol {
                cut.push(p.pts[i]);
            }
            if (d[i] > tol && d[j] < -tol) || (d[i] < -tol && d[j] > tol) {
                let x = p.pts[i] + (p.pts[j] - p.pts[i]) * (d[i] / (d[i] - d[j]));
                keep.push(x);
                cut.push(x);
            }
        }
        let keep = drop_dups(keep);
        if keep.len() >= 3 {
            if let Some(q) = Poly::new(keep, p.tag) {
                polys.push(q);
            }
        }
    }
    if cut.len() >= 3 {
        let (u, v) = basis(n);
        let c = cut.iter().fold(v3(0., 0., 0.), |s, q| s + *q) * (1.0 / cut.len() as f64);
        let mut ang: Vec<(f64, V3)> = cut
            .iter()
            .map(|q| ((*q - c).dot(v).atan2((*q - c).dot(u)), *q))
            .collect();
        ang.sort_by(|a, b| a.0.total_cmp(&b.0));
        let ring = drop_dups(ang.into_iter().map(|a| a.1).collect());
        let ring = drop_collinear(ring, tol);
        if ring.len() >= 3 {
            if let Some(q) = Poly::new(ring, tag) {
                polys.push(q);
            }
        }
    }
    Body { polys }.welded(tol)
}

fn check_faces(b: &Body, faces: &[u32], tol: f64) -> R<()> {
    for &t in faces {
        planar_face(b, t, tol)?;
    }
    Ok(())
}

/// Direction without repeats (unit).
fn push_dir(v: &mut Vec<V3>, d: V3) {
    if d.len() > 1e-12 {
        let d = d.normalized();
        if !v.iter().any(|x| x.dot(d) > 1.0 - 1e-9) {
            v.push(d);
        }
    }
}

/// Axis without repeats (a direction up to sign).
fn push_axis(v: &mut Vec<V3>, d: V3) {
    if d.len() > 1e-12 {
        let d = d.normalized();
        if !v.iter().any(|x| x.dot(d).abs() > 1.0 - 1e-9) {
            v.push(d);
        }
    }
}

/// Convex polyhedron K = {y: u·y ≤ c} for Minkowski sums: vertices, normals
/// (candidate faces of the sum) and edge axes.
struct KPoly {
    verts: Vec<V3>,
    norms: Vec<V3>,
    edges: Vec<V3>,
}

impl KPoly {
    /// −K (for an inward cut).
    fn mirrored(self) -> KPoly {
        KPoly {
            verts: self.verts.into_iter().map(|v| -v).collect(),
            norms: self.norms.into_iter().map(|v| -v).collect(),
            edges: self.edges,
        }
    }
}

/// K with volume, built by clipping with half-spaces.
fn kpoly_planes(cons: &[(V3, f64)], tol: Tol) -> R<KPoly> {
    let pl: Vec<(u32, V3, f64)> = cons.iter().map(|(u, c)| (0, *u, *c)).collect();
    let body = convex_from_planes(&pl, tol)?;
    let (mut verts, mut norms, mut edges) = (vec![], vec![], vec![]);
    for p in &body.polys {
        push_dir(&mut norms, p.plane.n);
        for i in 0..p.pts.len() {
            let (a, c) = (p.pts[i], p.pts[(i + 1) % p.pts.len()]);
            if !verts.iter().any(|v: &V3| (*v - a).len() < tol.length) {
                verts.push(a);
            }
            push_axis(&mut edges, c - a);
        }
    }
    Ok(KPoly {
        verts,
        norms,
        edges,
    })
}

/// K of any dimension (segment, polygon, solid): vertices are intersections of plane triples
/// inside all constraints; an edge is a pair of vertices on two shared planes.
fn kpoly_any(cons: &[(V3, f64)], tol: f64) -> R<KPoly> {
    let m = cons.len();
    if m > 64 {
        return Err(MassError::BadParam("too many face directions"));
    }
    let mut verts: Vec<V3> = vec![];
    for i in 0..m {
        for j in i + 1..m {
            for k in j + 1..m {
                let ((a, p), (b, q), (c, r)) = (cons[i], cons[j], cons[k]);
                let det = a.dot(b.cross(c));
                if det.abs() < 1e-12 {
                    continue;
                }
                let x = (b.cross(c) * p + c.cross(a) * q + a.cross(b) * r) * (1.0 / det);
                if cons.iter().all(|(u, w)| u.dot(x) <= w + tol)
                    && !verts.iter().any(|v| (*v - x).len() < tol)
                {
                    verts.push(x);
                }
            }
        }
    }
    if verts.is_empty() {
        return Err(MassError::Unbounded);
    }
    let tight: Vec<Vec<usize>> = verts
        .iter()
        .map(|v| {
            (0..m)
                .filter(|&i| (cons[i].0.dot(*v) - cons[i].1).abs() <= tol)
                .collect()
        })
        .collect();
    let mut edges = vec![];
    for i in 0..verts.len() {
        for j in i + 1..verts.len() {
            // an edge lies on two shared planes that are not parallel
            let sh: Vec<V3> = tight[i]
                .iter()
                .filter(|x| tight[j].contains(x))
                .map(|x| cons[*x].0)
                .collect();
            let line = sh
                .iter()
                .enumerate()
                .any(|(a, u)| sh[a + 1..].iter().any(|w| u.cross(*w).len() > 1e-9));
            if line {
                push_axis(&mut edges, verts[j] - verts[i]);
            }
        }
    }
    let mut norms = vec![];
    for (u, _) in cons {
        push_dir(&mut norms, *u);
    }
    // flat K: its plane normal (both senses) bounds the sum
    if verts.len() >= 3 {
        let o = verts[0];
        let mut nn = None;
        'f: for i in 1..verts.len() {
            for j in i + 1..verts.len() {
                let c = (verts[i] - o).cross(verts[j] - o);
                if c.len() > 1e-9 {
                    nn = Some(c.normalized());
                    break 'f;
                }
            }
        }
        if let Some(nn) = nn.filter(|n| verts.iter().all(|v| n.dot(*v - o).abs() < tol)) {
            push_dir(&mut norms, nn);
            push_dir(&mut norms, -nn);
        }
    }
    Ok(KPoly {
        verts,
        norms,
        edges,
    })
}

/// Cutter: the Minkowski sum of a convex planar piece `p` (normal `np`) and K, intersected with
/// half-spaces `cut` (tag, n, w: n·x ≤ w). The face with normal `face.0` gets tag
/// `face.1`, the others get `other`. Face planes of the sum are among K normals, ±np and edge × edge.
#[allow(clippy::too_many_arguments)]
fn mink(
    p: &[V3],
    np: V3,
    k: &KPoly,
    cut: &[(u32, V3, f64)],
    face: (V3, u32),
    other: u32,
    tol: Tol,
) -> R<Body> {
    let mut dirs = k.norms.clone();
    push_dir(&mut dirs, np);
    push_dir(&mut dirs, -np);
    for i in 0..p.len() {
        let e = p[(i + 1) % p.len()] - p[i];
        for f in &k.edges {
            let c = e.cross(*f);
            if c.len() > 1e-9 * e.len() {
                push_dir(&mut dirs, c);
                push_dir(&mut dirs, -c);
            }
        }
    }
    let sup = |u: V3, pts: &[V3]| pts.iter().map(|q| u.dot(*q)).fold(f64::MIN, f64::max);
    let mut pl: Vec<(u32, V3, f64)> = cut.to_vec();
    pl.extend(dirs.iter().map(|u| {
        let tag = if u.dot(face.0) > 1.0 - 1e-9 {
            face.1
        } else {
            other
        };
        (tag, *u, sup(*u, p) + sup(*u, &k.verts))
    }));
    convex_from_planes(&pl, tol)
}

/// Convex pieces of a face: the whole face if it is convex and without holes, otherwise its body pieces.
fn pieces(b: &Body, tag: u32, tol: f64) -> Vec<Vec<V3>> {
    let rings = b.face_outline(tag, tol);
    if let (1, Some(pl)) = (rings.len(), b.face_plane(tag, tol * 10.0)) {
        if is_convex(&rings[0], pl.n) {
            return rings;
        }
    }
    b.polys
        .iter()
        .filter(|p| p.tag == tag)
        .map(|p| p.pts.clone())
        .collect()
}

/// Planes of unselected faces touching the piece (by edge or vertex), on the piece side:
/// growing/cutting the piece does not cross them, so the neighbouring face stays in place. A plane
/// that cuts through the piece itself is not a constraint.
fn restrictions(b: &Body, piece: &[V3], sel: &[u32], tol: f64) -> Vec<(u32, V3, f64)> {
    let k = piece.len();
    let mut probes: Vec<V3> = piece.to_vec();
    probes.extend((0..k).map(|i| (piece[i] + piece[(i + 1) % k]) * 0.5));
    let mut out: Vec<(u32, V3, f64)> = vec![];
    for g in b.polys.iter().filter(|g| !sel.contains(&g.tag)) {
        if !probes.iter().any(|q| on_poly(g, *q, tol)) {
            continue;
        }
        let s: Vec<f64> = piece.iter().map(|q| g.plane.dist(*q)).collect();
        let (n, w) = if s.iter().all(|x| *x <= tol) && s.iter().any(|x| *x < -tol) {
            (g.plane.n, g.plane.w)
        } else if s.iter().all(|x| *x >= -tol) && s.iter().any(|x| *x > tol) {
            (-g.plane.n, -g.plane.w)
        } else {
            continue;
        };
        if !out
            .iter()
            .any(|o| o.1.dot(n) > 1.0 - 1e-9 && (o.2 - w).abs() < tol)
        {
            out.push((g.tag, n, w));
        }
    }
    out
}

/// Pieces tagged from `from` that lie in the target plane (n, w) receive its tag: a face
/// assembled from several cutters remains a single face.
fn retag(body: &mut Body, from: &[u32], targets: &[(V3, f64, u32)], tol: f64) {
    for p in &mut body.polys {
        if !from.contains(&p.tag) {
            continue;
        }
        if let Some(t) = targets
            .iter()
            .find(|(n, w, _)| n.dot(p.plane.n) > 1.0 - 1e-9 && (w - p.plane.w).abs() < tol)
        {
            p.tag = t.2;
        }
    }
}

/// Face directions of the body (without repeats).
fn face_dirs(b: &Body) -> Vec<V3> {
    let mut dirs = vec![];
    for p in &b.polys {
        push_dir(&mut dirs, p.plane.n);
    }
    dirs
}

/// Face offset by `d` (> 0 is outward). Convex body: all selected planes (or all of them,
/// if empty) are shifted at once and the body is rebuilt. Non-convex: a true offset:
/// each convex piece of a selected face is Minkowski-summed with K (shift of |d| along the
/// directions of the selected faces, 0 along the others); the cutter does not cross the planes of unselected
/// neighbouring faces; growing is a union, cutting is a subtraction. A moved face keeps its
/// tag, new faces (bevels at edges) are tagged by the source-face key.
pub fn offset_faces(b: &Body, faces: &[u32], d: f64, op: u32, tol: Tol) -> R<Body> {
    offset_faces_keyed(b, faces, d, op, &mut vec![], tol)
}

/// [`offset_faces`] with persistent indices for the new faces (`keys`).
pub fn offset_faces_keyed(
    b: &Body,
    faces: &[u32],
    d: f64,
    op: u32,
    keys: &mut FaceKeys,
    tol: Tol,
) -> R<Body> {
    if !d.is_finite() || d.abs() <= tol.length {
        return Err(MassError::BadParam("offset distance"));
    }
    check_faces(b, faces, tol.length)?;
    let pl = planes(b);
    let whole = |tags: &Vec<u32>| tags.iter().all(|t| faces.contains(t));
    let partial = pl
        .iter()
        .any(|(t, _, _)| t.iter().any(|x| faces.contains(x)) && !whole(t));
    if b.is_convex(tol.length * 10.0) && !partial {
        let moved: Vec<(u32, V3, f64)> = pl
            .iter()
            .map(|(t, n, w)| {
                let k = if faces.is_empty() || whole(t) {
                    w + d
                } else {
                    *w
                };
                (t[0], *n, k)
            })
            .collect();
        return convex_from_planes(&moved, tol);
    }
    let all = faces.is_empty();
    let sel: Vec<u32> = if all { b.tags() } else { faces.to_vec() };
    let lt = tol.length;
    let r = d.abs();
    let cons: Vec<(V3, f64)> = face_dirs(b)
        .into_iter()
        .map(|u| {
            let moved = all
                || b.polys
                    .iter()
                    .any(|p| sel.contains(&p.tag) && p.plane.n.dot(u) > 1.0 - 1e-9);
            (u, if moved { r } else { 0.0 })
        })
        .collect();
    let k = match kpoly_planes(&cons, tol) {
        Ok(k) => k,
        Err(_) => kpoly_any(&cons, lt * 10.0)?,
    };
    let k = if d < 0.0 { k.mirrored() } else { k };
    let want: Vec<[u32; 3]> = sel.iter().map(|t| [*t, 1, 0]).collect();
    let bevel = assign(keys, &want, 2)?;
    let mut out = b.clone();
    for (i, &t) in sel.iter().enumerate() {
        let other = ftag(op, bevel[i]);
        for p in pieces(b, t, lt) {
            let Some(pp) = Poly::new(p.clone(), t) else {
                continue;
            };
            let (np, wp) = (pp.plane.n, pp.plane.w);
            let mut cut = vec![];
            if !all {
                cut = restrictions(b, &p, &sel, lt * 10.0);
                // keep the tool on the moving side of its own face
                cut.push(if d > 0.0 {
                    (other, -np, -wp + lt * 10.0)
                } else {
                    (other, np, wp + lt * 10.0)
                });
            }
            let tool = mink(&p, np, &k, &cut, (np * d.signum(), t), other, tol)?;
            out = if d > 0.0 {
                out.union(&tool, tol)
            } else {
                out.subtract(&tool, tol)
            };
        }
    }
    let from: Vec<u32> = bevel.iter().map(|l| ftag(op, *l)).collect();
    let mut targets: Vec<(V3, f64, u32)> = b
        .polys
        .iter()
        .filter(|q| sel.contains(&q.tag))
        .map(|q| (q.plane.n, q.plane.w + d, q.tag))
        .collect();
    targets.extend(
        b.polys
            .iter()
            .filter(|q| !sel.contains(&q.tag))
            .map(|q| (q.plane.n, q.plane.w, q.tag)),
    );
    retag(&mut out, &from, &targets, lt * 10.0);
    nonempty(out)
}

/// Shell of thickness `t` inward; the `open` faces are removed (the cavity opens outward). A convex
/// body is the intersection of shifted half-spaces; a non-convex one uses erosion: the Minkowski
/// sums of the closed-face pieces with −K are subtracted from the body (K is the polyhedron of face directions of radius
/// `t`), and at open faces the cavity is extended outward. The inner face opposite face G is tagged
/// by key G. A thickness leaving no cavity yields `EmptyCavity`.
pub fn shell(b: &Body, t: f64, open: &[u32], op: u32, tol: Tol) -> R<Body> {
    shell_keyed(b, t, open, op, &mut vec![], tol)
}

/// [`shell`] with persistent indices for the inner faces (`keys`).
pub fn shell_keyed(
    b: &Body,
    t: f64,
    open: &[u32],
    op: u32,
    keys: &mut FaceKeys,
    tol: Tol,
) -> R<Body> {
    let t = pos(t, "shell thickness")?;
    for &o in open {
        if !b.polys.iter().any(|p| p.tag == o) {
            return Err(MassError::NoFace(o));
        }
    }
    let lt = tol.length;
    if b.is_convex(lt * 10.0) {
        let groups = planes(b);
        let want: Vec<[u32; 3]> = groups.iter().map(|g| [g.0[0], 0, 0]).collect();
        let loc = assign(keys, &want, 2)?;
        let pl: Vec<(u32, V3, f64)> = groups
            .into_iter()
            .zip(loc)
            .map(|((tags, n, w), l)| {
                let k = if tags.iter().any(|x| open.contains(x)) {
                    w + t
                } else {
                    w - t
                };
                (ftag(op, l), n, k)
            })
            .collect();
        let inner = match convex_from_planes(&pl, tol) {
            Ok(x) if x.volume() > lt => x,
            _ => return Err(MassError::EmptyCavity),
        };
        return nonempty(b.subtract(&inner, tol));
    }
    let cons: Vec<(V3, f64)> = face_dirs(b).into_iter().map(|u| (u, t)).collect();
    let k = kpoly_planes(&cons, tol)?.mirrored();
    let tags: Vec<u32> = b.tags();
    let want: Vec<[u32; 3]> = tags.iter().flat_map(|x| [[*x, 0, 0], [*x, 1, 0]]).collect();
    let loc = assign(keys, &want, 2)?;
    let mut inner = b.clone();
    for (i, &tg) in tags.iter().enumerate() {
        if open.contains(&tg) {
            continue;
        }
        let (face, other) = (ftag(op, loc[2 * i]), ftag(op, loc[2 * i + 1]));
        for p in pieces(b, tg, lt) {
            let Some(pp) = Poly::new(p.clone(), tg) else {
                continue;
            };
            let tool = mink(&p, pp.plane.n, &k, &[], (-pp.plane.n, face), other, tol)?;
            inner = inner.subtract(&tool, tol);
            if inner.is_empty() {
                return Err(MassError::EmptyCavity);
            }
        }
    }
    if inner.volume() <= lt {
        return Err(MassError::EmptyCavity);
    }
    // the cavity breaks through each open face: its part on the open plane is pushed out
    let mut ext = Body::default();
    for p in &inner.polys {
        let Some(o) = b.polys.iter().find(|q| {
            open.contains(&q.tag)
                && q.plane.n.dot(p.plane.n) > 1.0 - 1e-9
                && (q.plane.w - p.plane.w).abs() < lt * 10.0
        }) else {
            continue;
        };
        let other = loc[2 * tags.iter().position(|x| *x == o.tag).unwrap_or(0) + 1];
        let n = p.plane.n;
        let base: Vec<V3> = p.pts.iter().map(|q| *q - n * (t * 0.5)).collect();
        let sides = vec![ftag(op, other); base.len()];
        let cap = ftag(op, other);
        ext = ext.union(&prism_tagged(&base, n * (t * 1.5), &sides, cap, cap), tol);
    }
    if !ext.is_empty() {
        inner = inner.union(&ext, tol);
    }
    let from: Vec<u32> = (0..tags.len()).map(|i| ftag(op, loc[2 * i + 1])).collect();
    let targets: Vec<(V3, f64, u32)> = b
        .polys
        .iter()
        .filter(|q| !open.contains(&q.tag))
        .filter_map(|q| {
            let i = tags.iter().position(|x| *x == q.tag)?;
            Some((q.plane.n, q.plane.w - t, ftag(op, loc[2 * i])))
        })
        .collect();
    retag(&mut inner, &from, &targets, lt * 10.0);
    nonempty(b.subtract(&inner, tol))
}

/// Splitting face `tag` by a plane (point, normal): the front part becomes `ftag(op, 1)`.
pub fn split_face(b: &Body, tag: u32, at: V3, n: V3, op: u32, tol: Tol) -> R<Body> {
    if !b.polys.iter().any(|p| p.tag == tag) {
        return Err(MassError::NoFace(tag));
    }
    if !(n.len() > 1e-12) || ![at.x, at.y, at.z].iter().all(|v| v.is_finite()) {
        return Err(MassError::BadParam("split plane"));
    }
    let fresh = ftag(op, 1);
    let n = n.normalized();
    let w = n.dot(at);
    let mut polys = vec![];
    let (mut front, mut back) = (false, false);
    for p in &b.polys {
        if p.tag != tag {
            polys.push(p.clone());
            continue;
        }
        let d: Vec<f64> = p.pts.iter().map(|q| n.dot(*q) - w).collect();
        let (mut f, mut k) = (vec![], vec![]);
        for i in 0..p.pts.len() {
            let j = (i + 1) % p.pts.len();
            if d[i] >= -tol.length {
                f.push(p.pts[i]);
            }
            if d[i] <= tol.length {
                k.push(p.pts[i]);
            }
            if (d[i] > tol.length && d[j] < -tol.length)
                || (d[i] < -tol.length && d[j] > tol.length)
            {
                let x = p.pts[i] + (p.pts[j] - p.pts[i]) * (d[i] / (d[i] - d[j]));
                f.push(x);
                k.push(x);
            }
        }
        for (r, t) in [(f, fresh), (k, tag)] {
            let r = drop_dups(r);
            if r.len() >= 3 {
                if let Some(mut q) = Poly::new(r, t) {
                    if q.area() > tol.length * tol.length {
                        q.plane = p.plane;
                        if t == fresh {
                            front = true;
                        } else {
                            back = true;
                        }
                        polys.push(q);
                    }
                }
            }
        }
    }
    if !(front && back) {
        return Err(MassError::NoSplit);
    }
    Ok(Body { polys }.welded(tol.length))
}

/// Oriented section segments by plane `n·x = w` (direction is n × face normal).
fn slice(b: &Body, n: V3, w: f64) -> Vec<(V3, V3)> {
    let mut out = vec![];
    for p in &b.polys {
        let d: Vec<f64> = p.pts.iter().map(|q| n.dot(*q) - w).collect();
        let mut x = vec![];
        for i in 0..p.pts.len() {
            let j = (i + 1) % p.pts.len();
            if (d[i] < 0.0) != (d[j] < 0.0) {
                x.push(p.pts[i] + (p.pts[j] - p.pts[i]) * (d[i] / (d[i] - d[j])));
            }
        }
        if x.len() != 2 {
            continue;
        }
        let dir = n.cross(p.plane.n);
        let (s, e) = if (x[1] - x[0]).dot(dir) < 0.0 {
            (x[1], x[0])
        } else {
            (x[0], x[1])
        };
        out.push((s, e));
    }
    out
}

/// Section by horizontal plane z: (area, perimeter). Segment orientation comes from the
/// outward face normals, so no outline assembly is needed.
pub fn section(b: &Body, z: f64) -> (f64, f64) {
    let (mut area2, mut per) = (0.0, 0.0);
    for (s, e) in slice(b, v3(0., 0., 1.), z) {
        let (s, e) = (v2(s.x, s.y), v2(e.x, e.y));
        area2 += s.cross(e);
        per += (e - s).len();
    }
    (area2 / 2.0, per)
}

/// Section outlines at elevation z (for floor slabs per storey of the form).
pub fn section_rings(b: &Body, z: f64) -> Vec<Vec<V2>> {
    let segs: Vec<(V2, V2)> = slice(b, v3(0., 0., 1.), z)
        .into_iter()
        .map(|(s, e)| (v2(s.x, s.y), v2(e.x, e.y)))
        .collect();
    clean_rings(poly::chain(&segs, 1e-7))
}

fn clean_rings(rings: Vec<Vec<V2>>) -> Vec<Vec<V2>> {
    rings
        .into_iter()
        .map(|r| {
            let r3 = drop_collinear(r.iter().map(|p| v3(p.x, p.y, 0.0)).collect(), 1e-7);
            r3.iter().map(|p| v2(p.x, p.y)).collect::<Vec<V2>>()
        })
        .filter(|r| r.len() >= 3 && poly::area(r).abs() > 1e-12)
        .collect()
}

/// Section by a vertical plane through `o` along unit `u`: outlines in coordinates
/// (s along u, z).
pub fn vertical_rings(b: &Body, o: V2, u: V2) -> Vec<Vec<V2>> {
    let n = v3(-u.y, u.x, 0.0);
    let w = n.dot(v3(o.x, o.y, 0.0));
    let to = |p: V3| v2((v2(p.x, p.y) - o).dot(u), p.z);
    let segs: Vec<(V2, V2)> = slice(b, n, w)
        .into_iter()
        .map(|(s, e)| (to(s), to(e)))
        .collect();
    clean_rings(poly::chain(&segs, 1e-7))
}

/// Storeys of the form: (elevation, area, perimeter) for elevations inside the body.
pub fn floors(b: &Body, elevations: &[f64]) -> Vec<(f64, f64, f64)> {
    let Some((lo, hi)) = b.bbox() else {
        return vec![];
    };
    elevations
        .iter()
        .filter(|z| **z >= lo.z - 1e-9 && **z < hi.z - 1e-9)
        .map(|&z| {
            let (a, p) = section(b, z + 1e-9);
            (z, a, p)
        })
        .filter(|f| f.1 > 1e-9)
        .collect()
}
