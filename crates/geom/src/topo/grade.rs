// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Building pad: a contour at a given elevation, slopes out to the terrain (the zero-work line),
//! cut/fill volumes and the graded surface; smoothing of elevations inside the contour.
//!
//! The slope is given by its run ratio m (metres horizontal per 1 m vertical): the slope elevation is
//! z ± d / m, where d is the distance to the contour. Rays go outward from the contour (along edge normals,
//! fanned at convex corners, along the bisector at reflex ones) up to the point where
//! |terrain − z| = d / m, but no farther than their own zone: where another part of the contour is closer
//! (reflex corner, narrow bay), the ray stops on the medial axis and that point becomes a
//! slope vertex. This keeps rays from crossing and the zero-work line simple.

use super::{pos_neg, Tin, TinError};
use crate::math::{v2, v3, V2, V3};
use crate::predicates::{orient2d, point_in_ring, segments_meet};

/// Pad: plan contour, elevation, slope run ratio (0 means a vertical wall).
#[derive(Clone, Debug, PartialEq)]
pub struct Pad {
    pub outline: Vec<V2>,
    pub z: f64,
    pub slope: f64,
}

/// Graded surface and per-pad totals (in `pads` order).
#[derive(Clone, Debug)]
pub struct Graded {
    pub tin: Tin,
    /// Zero-work line of each pad (a simple counter-clockwise ring).
    pub daylight: Vec<Vec<V2>>,
    pub cut: Vec<f64>,
    pub fill: Vec<f64>,
    /// `tin` triangle inside the contour of pad k: k, otherwise -1.
    pub pad_tris: Vec<i32>,
}

/// Minimum run ratio: a "vertical" wall stays a surface (1 mm per 1 m).
const MIN_SLOPE: f64 = 1e-3;
const MIN_REACH: f64 = 1e-3;
/// Slope point coincidence (like vertex merging in a triangulation).
const SNAP: f64 = 1e-6;
const NO_RAY: usize = usize::MAX;

struct Ray {
    o: V2,
    d: V2,
    /// Growth of the distance to the contour per unit ray length in its own zone (bisector: cos).
    k: f64,
}

/// Ray end: point, elevation; `day` means on the ground, otherwise on the medial axis (slope elevation).
#[derive(Clone, Copy, Debug)]
struct End {
    p: V2,
    z: f64,
    day: bool,
}

/// Zero-work line vertex and the ray that produced it.
#[derive(Clone, Copy, Debug)]
struct Dp {
    p: V2,
    z: f64,
    ray: usize,
}

fn unit(v: V2) -> V2 {
    let l = v.len();
    if l > 0.0 {
        v * (1.0 / l)
    } else {
        v
    }
}

fn area(r: &[V2]) -> f64 {
    (0..r.len())
        .map(|i| r[i].cross(r[(i + 1) % r.len()]))
        .sum::<f64>()
        * 0.5
}

/// Counter-clockwise contour without adjacent repeats or a closing duplicate point.
fn clean(ring: &[V2]) -> Vec<V2> {
    let mut r: Vec<V2> = vec![];
    for &p in ring {
        if r.last().map_or(true, |q: &V2| (*q - p).len() > 1e-9) {
            r.push(p);
        }
    }
    while r.len() > 1 && (r[0] - r[r.len() - 1]).len() <= 1e-9 {
        r.pop();
    }
    if area(&r) < 0.0 {
        r.reverse();
    }
    r
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let ab = b - a;
    let l2 = ab.dot(ab);
    let t = if l2 > 0.0 {
        ((p - a).dot(ab) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + ab * t)).len()
}

/// Plan distance to the contour (ring).
fn ring_dist(ring: &[V2], p: V2) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| seg_dist(p, ring[i], ring[(i + 1) % n]))
        .fold(f64::INFINITY, f64::min)
}

fn rays(ring: &[V2], spacing: f64) -> Vec<Ray> {
    let n = ring.len();
    let mut out = vec![];
    for i in 0..n {
        let (pv, p, nx) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
        let (e0, e1) = (p - pv, nx - p);
        let (n0, n1) = (unit(v2(e0.y, -e0.x)), unit(v2(e1.y, -e1.x)));
        if e0.cross(e1) > 0.0 {
            // convex corner: fan from the incoming edge normal to the outgoing edge normal
            let a0 = n0.y.atan2(n0.x);
            let mut a1 = n1.y.atan2(n1.x);
            while a1 < a0 {
                a1 += std::f64::consts::TAU;
            }
            let steps = ((a1 - a0) / (std::f64::consts::PI / 16.0)).ceil().max(1.0) as usize;
            for k in 0..=steps {
                let a = a0 + (a1 - a0) * k as f64 / steps as f64;
                out.push(Ray {
                    o: p,
                    d: v2(a.cos(), a.sin()),
                    k: 1.0,
                });
            }
        } else {
            // reflex (or straight) corner: bisector, distance grows as cos of the half-angle
            let b = n0 + n1;
            let (d, k) = if b.len() > 1e-9 {
                let d = unit(b);
                (d, d.dot(n0).max(1e-6))
            } else {
                (n1, 1.0)
            };
            out.push(Ray { o: p, d, k });
        }
        let m = ((e1.len() / spacing).ceil() as usize).clamp(1, 200);
        for j in 1..m {
            out.push(Ray {
                o: p + e1 * (j as f64 / m as f64),
                d: n1,
                k: 1.0,
            });
        }
    }
    out
}

/// Slope end along a ray: to the ground or to the boundary of its own zone (medial axis).
fn reach(ex: &Tin, ring: &[V2], r: &Ray, z: f64, m: f64, step: f64, dmax: f64) -> End {
    let pt = |s: f64| r.o + r.d * s;
    let dist = |s: f64| ring_dist(ring, pt(s));
    // in its own zone the nearest part of the contour is the one the ray left from
    let own = |s: f64| dist(s) >= r.k * s - 1e-9 * (1.0 + s);
    // > 0 means the slope has not yet reached the ground
    let h = |s: f64| ex.z_at(pt(s)).map(|e| (e - z).abs() - dist(s) / m);
    let Some(e0) = ex.z_at(pt(MIN_REACH)) else {
        return End {
            p: pt(MIN_REACH),
            z,
            day: true,
        };
    };
    if h(MIN_REACH).map_or(true, |v| v <= 0.0) {
        return End {
            p: pt(MIN_REACH),
            z: e0,
            day: true,
        };
    }
    let side = if e0 >= z { 1.0 } else { -1.0 };
    let design = |s: f64| z + side * dist(s) / m;
    let mut s = MIN_REACH;
    let mut bracket = None;
    while s < dmax {
        let mut s1 = (s + step).min(dmax);
        let mut blocked = false;
        if !own(s1) {
            let (mut a, mut b) = (s, s1);
            for _ in 0..60 {
                let mid = 0.5 * (a + b);
                if own(mid) {
                    a = mid;
                } else {
                    b = mid;
                }
            }
            s1 = a;
            blocked = true;
        }
        match h(s1) {
            None => break,
            Some(v) if v <= 0.0 => {
                bracket = Some((s, s1));
                break;
            }
            Some(_) if blocked => {
                return End {
                    p: pt(s1),
                    z: design(s1),
                    day: false,
                };
            }
            Some(_) => s = s1,
        }
    }
    let Some((mut lo, mut hi)) = bracket else {
        // terrain ended first: the slope stops at the surface edge
        return End {
            p: pt(s),
            z: design(s),
            day: true,
        };
    };
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        match h(mid) {
            Some(v) if v > 0.0 => lo = mid,
            _ => hi = mid,
        }
    }
    End {
        p: pt(hi),
        z: ex.z_at(pt(hi)).unwrap_or_else(|| design(hi)),
        day: true,
    }
}

fn opposite(a: f64, b: f64) -> bool {
    (a > 0.0 && b < 0.0) || (a < 0.0 && b > 0.0)
}

/// First pair of properly crossing non-adjacent edges of a closed polyline (i < j).
pub(crate) fn first_crossing(r: &[V2]) -> Option<(usize, usize)> {
    let n = r.len();
    if n < 4 {
        return None;
    }
    let mut segs: Vec<(f64, f64, usize)> = (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            (a.x.min(b.x), a.x.max(b.x), i)
        })
        .collect();
    segs.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (k, &(_, x1, i)) in segs.iter().enumerate() {
        let (a, b) = (r[i], r[(i + 1) % n]);
        for &(x0, _, j) in &segs[k + 1..] {
            if x0 > x1 {
                break;
            }
            if (j + 1) % n == i || (i + 1) % n == j {
                continue;
            }
            let (c, d) = (r[j], r[(j + 1) % n]);
            if a.y.max(b.y) < c.y.min(d.y) || c.y.max(d.y) < a.y.min(b.y) {
                continue;
            }
            if opposite(orient2d(a, b, c), orient2d(a, b, d))
                && opposite(orient2d(c, d, a), orient2d(c, d, b))
            {
                return Some((i.min(j), i.max(j)));
            }
        }
    }
    None
}

/// Self-intersections of the zero-work line (a safeguard on abrupt terrain): the loop with the smaller
/// area is cut off at the intersection point.
fn untangle(dl: &mut Vec<Dp>, ex: &Tin) {
    for _ in 0..64 {
        if dl.len() < 4 {
            return;
        }
        let ring: Vec<V2> = dl.iter().map(|d| d.p).collect();
        let Some((i, j)) = first_crossing(&ring) else {
            return;
        };
        let n = dl.len();
        let (a, b, c, d) = (ring[i], ring[i + 1], ring[j], ring[(j + 1) % n]);
        let den = (b - a).cross(d - c);
        let t = if den != 0.0 {
            ((c - a).cross(d - c) / den).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let x = a + (b - a) * t;
        let zi = dl[i].z + (dl[i + 1].z - dl[i].z) * t;
        let xp = Dp {
            p: x,
            z: ex.z_at(x).unwrap_or(zi),
            ray: NO_RAY,
        };
        let mut inner = vec![xp];
        inner.extend_from_slice(&dl[i + 1..=j]);
        let mut outer = vec![xp];
        outer.extend_from_slice(&dl[j + 1..]);
        outer.extend_from_slice(&dl[..=i]);
        let ar = |v: &[Dp]| area(&v.iter().map(|d| d.p).collect::<Vec<V2>>());
        *dl = if ar(&inner) > ar(&outer) {
            inner
        } else {
            outer
        };
    }
}

/// ∫(terrain − design)⁺ and ∫(design − terrain)⁺ over design triangles; terrain is sampled
/// on a grid inside the triangle (step ≤ h), linear between nodes with exact splitting at zero.
fn integrate(ex: &Tin, tris: &[[V3; 3]], h: f64) -> (f64, f64) {
    let (mut cut, mut fill) = (0.0, 0.0);
    for t in tris {
        let [a, b, c] = *t;
        let (ab, ac) = (b - a, c - a);
        let ar = 0.5 * (ab.x * ac.y - ab.y * ac.x).abs();
        if ar < 1e-12 {
            continue;
        }
        let lmax = [(b - a), (c - b), (a - c)]
            .iter()
            .map(|e| e.x.hypot(e.y))
            .fold(0.0, f64::max);
        let n = ((lmax / h).ceil() as usize).clamp(1, 64);
        let nf = n as f64;
        let row = |i: usize| i * (2 * n + 3 - i) / 2;
        let mut f = vec![0.0; (n + 1) * (n + 2) / 2];
        for i in 0..=n {
            for j in 0..=n - i {
                let p = a + ab * (i as f64 / nf) + ac * (j as f64 / nf);
                f[row(i) + j] = ex.z_at(v2(p.x, p.y)).map_or(0.0, |e| e - p.z);
            }
        }
        let sub = ar / (nf * nf);
        let g = |i: usize, j: usize| f[row(i) + j];
        for i in 0..n {
            for j in 0..n - i {
                let (p, q) = pos_neg([g(i, j), g(i + 1, j), g(i, j + 1)], sub);
                cut += p;
                fill += q;
                if i + j + 1 < n {
                    let (p, q) = pos_neg([g(i + 1, j), g(i + 1, j + 1), g(i, j + 1)], sub);
                    cut += p;
                    fill += q;
                }
            }
        }
    }
    (cut, fill)
}

fn bbox(r: &[V2]) -> (V2, V2) {
    r.iter().fold(
        (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN)),
        |(lo, hi), p| {
            (
                v2(lo.x.min(p.x), lo.y.min(p.y)),
                v2(hi.x.max(p.x), hi.y.max(p.y)),
            )
        },
    )
}

fn in_box(p: V2, (lo, hi): (V2, V2)) -> bool {
    p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y
}

/// Segment a–b touches the ring (with bounding box `bb`).
fn touches_ring(a: V2, b: V2, r: &[V2], bb: (V2, V2)) -> bool {
    let n = r.len();
    if n < 3
        || a.x.max(b.x) < bb.0.x
        || a.x.min(b.x) > bb.1.x
        || a.y.max(b.y) < bb.0.y
        || a.y.min(b.y) > bb.1.y
    {
        return false;
    }
    (0..n).any(|i| segments_meet(a, b, r[i], r[(i + 1) % n]))
}

/// Pads on terrain: the graded surface and volumes relative to the original.
/// Pads must not overlap (their slopes are summed independently).
pub fn grade(ex: &Tin, pads: &[Pad]) -> Result<Graded, TinError> {
    if pads.is_empty() {
        return Ok(Graded {
            pad_tris: vec![-1; ex.tris.len()],
            tin: ex.clone(),
            daylight: vec![],
            cut: vec![],
            fill: vec![],
        });
    }
    let spacing = ex.spacing();
    let (lo, hi) = ex.bounds().ok_or(TinError::TooFewPoints)?;
    let mut lines: Vec<Vec<V3>> = vec![];
    let mut out = Graded {
        tin: Tin::default(),
        daylight: vec![],
        cut: vec![0.0; pads.len()],
        fill: vec![0.0; pads.len()],
        pad_tris: vec![],
    };
    let mut outlines: Vec<Vec<V2>> = vec![];
    for pad in pads {
        let ring = clean(&pad.outline);
        if ring.len() < 3 || area(&ring) <= 0.0 {
            out.daylight.push(vec![]);
            outlines.push(vec![]);
            continue;
        }
        let m = pad.slope.max(MIN_SLOPE);
        let dmax =
            m * ((hi.z - lo.z) + (pad.z - lo.z).abs() + (pad.z - hi.z).abs()) + 2.0 * spacing;
        let rs = rays(&ring, spacing);
        let ends: Vec<End> = rs
            .iter()
            .map(|r| reach(ex, &ring, r, pad.z, m, spacing * 0.5, dmax))
            .collect();
        let at_z = |p: V2| v3(p.x, p.y, pad.z);
        // zero-work line: the ends of rays that reached the ground
        let mut dl: Vec<Dp> = vec![];
        for (j, e) in ends.iter().enumerate() {
            if e.day && dl.last().map_or(true, |q: &Dp| (q.p - e.p).len() > SNAP) {
                dl.push(Dp {
                    p: e.p,
                    z: e.z,
                    ray: j,
                });
            }
        }
        while dl.len() > 1 && (dl[0].p - dl[dl.len() - 1].p).len() <= SNAP {
            dl.pop();
        }
        untangle(&mut dl, ex);
        let dl_ring: Vec<V2> = dl.iter().map(|d| d.p).collect();
        if dl_ring.len() < 3 {
            out.daylight.push(vec![]);
            outlines.push(vec![]);
            continue;
        }
        // medial-axis points inside the zero-work line; coincident ones are merged
        let mut mids: Vec<(usize, V2, f64)> = vec![];
        for (j, e) in ends.iter().enumerate() {
            if e.day || point_in_ring(e.p, &dl_ring) <= 0 {
                continue;
            }
            let (p, z) = mids
                .iter()
                .find(|q| (q.1 - e.p).len() <= SNAP)
                .map_or((e.p, e.z), |q| (q.1, q.2));
            mids.push((j, p, z));
        }
        // rays are slope edges (elevation is linear along a ray); axis points on a ray are its vertices
        let mut segs: Vec<(usize, V2, f64)> = dl
            .iter()
            .filter(|d| d.ray != NO_RAY)
            .map(|d| (d.ray, d.p, d.z))
            .collect();
        segs.extend(mids.iter().copied());
        for &(j, e, ez) in &segs {
            let o = rs[j].o;
            let len = (e - o).len();
            if len <= SNAP {
                continue;
            }
            let dir = (e - o) * (1.0 / len);
            let mut on: Vec<(f64, V3)> = mids
                .iter()
                .filter_map(|&(_, q, qz)| {
                    let t = (q - o).dot(dir);
                    let off = (q - o).cross(dir).abs();
                    (t > SNAP && t < len - SNAP && off <= SNAP).then(|| (t, v3(q.x, q.y, qz)))
                })
                .collect();
            on.sort_by(|a, b| a.0.total_cmp(&b.0));
            on.dedup_by(|a, b| (a.0 - b.0).abs() <= SNAP);
            let mut line = vec![at_z(o)];
            line.extend(on.into_iter().map(|q| q.1));
            line.push(v3(e.x, e.y, ez));
            lines.push(line);
        }
        let mut origins: Vec<V2> = vec![];
        for r in &rs {
            if origins.last().map_or(true, |q: &V2| *q != r.o) {
                origins.push(r.o);
            }
        }
        if origins.len() > 1 && origins[0] == origins[origins.len() - 1] {
            origins.pop();
        }
        let mut o3: Vec<V3> = origins.iter().map(|&p| at_z(p)).collect();
        o3.push(o3[0]);
        lines.push(o3);
        let mut d3: Vec<V3> = dl.iter().map(|d| v3(d.p.x, d.p.y, d.z)).collect();
        d3.push(d3[0]);
        lines.push(d3);
        out.daylight.push(dl_ring);
        outlines.push(ring);
    }
    let rings: Vec<&Vec<V2>> = out.daylight.iter().chain(&outlines).collect();
    let boxes: Vec<(V2, V2)> = rings.iter().map(|r| bbox(r)).collect();
    let gone = |p: V3| {
        let q = v2(p.x, p.y);
        rings
            .iter()
            .zip(&boxes)
            .any(|(r, &bb)| r.len() >= 3 && in_box(q, bb) && point_in_ring(q, r) >= 0)
    };
    let keep: Vec<bool> = ex.pts.iter().map(|&p| !gone(p)).collect();
    let points: Vec<V3> = ex
        .pts
        .iter()
        .zip(&keep)
        .filter(|&(_, &k)| k)
        .map(|(&p, _)| p)
        .collect();
    for e in &ex.fixed {
        if !keep[e[0] as usize] || !keep[e[1] as usize] {
            continue;
        }
        let (a, b) = (ex.pts[e[0] as usize], ex.pts[e[1] as usize]);
        // an old breakline across the pad or slope would pull them toward the terrain
        let (pa, pb) = (v2(a.x, a.y), v2(b.x, b.y));
        if rings
            .iter()
            .zip(&boxes)
            .any(|(r, &bb)| touches_ring(pa, pb, r, bb))
        {
            continue;
        }
        lines.push(vec![a, b]);
    }
    let outer = ex.outer_ring();
    if let Some(o) = &outer {
        lines.push(ex.drape(o));
    }
    let tin = Tin::pass(&points, &lines, outer.as_deref())?;
    out.pad_tris = tin
        .tris
        .iter()
        .map(|t| {
            let m = tin.centroid(t);
            outlines
                .iter()
                .position(|r| r.len() >= 3 && point_in_ring(m, r) > 0)
                .map_or(-1, |k| k as i32)
        })
        .collect();
    // volumes come from the built surface inside the zero-work line
    for (k, dl) in out.daylight.iter().enumerate() {
        if dl.len() < 3 {
            continue;
        }
        let bb = bbox(dl);
        let tris: Vec<[V3; 3]> = tin
            .tris
            .iter()
            .filter(|t| {
                let c = tin.centroid(t);
                in_box(c, bb) && point_in_ring(c, dl) > 0
            })
            .map(|t| t.map(|i| tin.pts[i as usize]))
            .collect();
        let (cut, fill) = integrate(ex, &tris, spacing * 0.5);
        out.cut[k] = cut;
        out.fill[k] = fill;
    }
    out.tin = tin;
    Ok(out)
}

/// Smoothing: vertices inside `ring` are moved toward the neighbour average `passes` times
/// (half step); returns the elevations of all vertices.
pub fn smooth(tin: &Tin, ring: &[V2], passes: u32) -> Vec<f64> {
    let nb = tin.neighbors();
    let inside: Vec<bool> = tin
        .pts
        .iter()
        .map(|p| ring.len() >= 3 && point_in_ring(v2(p.x, p.y), ring) > 0)
        .collect();
    let mut z: Vec<f64> = tin.pts.iter().map(|p| p.z).collect();
    for _ in 0..passes {
        let prev = z.clone();
        for v in 0..z.len() {
            if inside[v] && !nb[v].is_empty() {
                let avg = nb[v].iter().map(|&u| prev[u as usize]).sum::<f64>() / nb[v].len() as f64;
                z[v] = 0.5 * (prev[v] + avg);
            }
        }
    }
    z
}
