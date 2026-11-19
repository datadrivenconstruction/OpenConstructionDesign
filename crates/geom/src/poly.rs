// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Planar polygons and mesh sections: convex clipping, ear-clipping triangulation,
//! segment intervals inside a contour, mesh-plane section, assembling segments into contours.

use crate::math::{v2, V2, V3};
use crate::mesh::Mesh;
use crate::predicates::{point_in_ring, segments_meet};

/// Simple contour: at least 3 vertices, edges not shorter than `eps`, non-adjacent edges do not touch.
pub fn simple_ring(r: &[V2], eps: f64) -> Result<(), &'static str> {
    let n = r.len();
    if n < 3 {
        return Err("ring needs at least 3 vertices");
    }
    for i in 0..n {
        if (r[(i + 1) % n] - r[i]).len() < eps {
            return Err("ring has a zero-length edge");
        }
    }
    for i in 0..n {
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            if segments_meet(r[i], r[(i + 1) % n], r[j], r[(j + 1) % n]) {
                return Err("ring self-intersects");
            }
        }
    }
    if area(r).abs() < eps * eps {
        return Err("ring has zero area");
    }
    Ok(())
}

pub fn area(r: &[V2]) -> f64 {
    crate::profile::signed_area(r)
}

pub fn centroid(r: &[V2]) -> V2 {
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..r.len() {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        let c = p.cross(q);
        a += c;
        cx += (p.x + q.x) * c;
        cy += (p.y + q.y) * c;
    }
    if a.abs() < 1e-300 {
        return r.first().copied().unwrap_or_default();
    }
    v2(cx / (3.0 * a), cy / (3.0 * a))
}

pub fn perimeter(r: &[V2]) -> f64 {
    (0..r.len())
        .map(|i| (r[(i + 1) % r.len()] - r[i]).len())
        .sum()
}

/// Clip by the half-plane `n·p <= c` (Sutherland-Hodgman, single side).
pub fn clip_half(subject: &[V2], n: V2, c: f64) -> Vec<V2> {
    let mut out = Vec::with_capacity(subject.len() + 2);
    let k = subject.len();
    for i in 0..k {
        let (p, q) = (subject[i], subject[(i + 1) % k]);
        let (dp, dq) = (n.dot(p) - c, n.dot(q) - c);
        if dp <= 0.0 {
            out.push(p);
        }
        if (dp < 0.0 && dq > 0.0) || (dp > 0.0 && dq < 0.0) {
            out.push(p + (q - p) * (dp / (dp - dq)));
        }
    }
    out
}

/// Part of `subject` (any simple contour) inside the convex `convex` (counter-clockwise).
/// The result area is exact even for a non-convex `subject`: degenerate edges cancel each other.
pub fn clip_convex(subject: &[V2], convex: &[V2]) -> Vec<V2> {
    let mut out = subject.to_vec();
    let k = convex.len();
    let s = if area(convex) < 0.0 { -1.0 } else { 1.0 };
    for i in 0..k {
        if out.is_empty() {
            break;
        }
        let (a, b) = (convex[i], convex[(i + 1) % k]);
        let d = (b - a) * s;
        let n = v2(d.y, -d.x); // outward from the convex polygon
        out = clip_half(&out, n, n.dot(a));
    }
    out
}

/// Area of the intersection of contour `ring` (sign preserved) with the union of convex pieces.
pub fn overlap_area(ring: &[V2], pieces: &[Vec<V2>]) -> f64 {
    pieces.iter().map(|c| area(&clip_convex(ring, c))).sum()
}

/// Ear-clipping triangulation of a simple contour (O(n²)); vertex indices, counter-clockwise orientation.
pub fn triangulate(r: &[V2]) -> Vec<[usize; 3]> {
    let n = r.len();
    let mut idx: Vec<usize> = (0..n).collect();
    if area(r) < 0.0 {
        idx.reverse();
    }
    let mut out = Vec::with_capacity(n.saturating_sub(2));
    let cross = |a: V2, b: V2, c: V2| (b - a).cross(c - a);
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n + 10 {
        guard += 1;
        let m = idx.len();
        let mut cut = None;
        for i in 0..m {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (r[ia], r[ib], r[ic]);
            if cross(a, b, c) <= 0.0 {
                continue;
            }
            let inside = idx.iter().any(|&j| {
                if j == ia || j == ib || j == ic {
                    return false;
                }
                let p = r[j];
                cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0
            });
            if !inside {
                cut = Some(i);
                break;
            }
        }
        // degenerate remainder (collinear vertices): clip any ear
        let i = cut.unwrap_or(0);
        let m = idx.len();
        let t = [idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]];
        if cross(r[t[0]], r[t[1]], r[t[2]]) > 0.0 {
            out.push(t);
        }
        idx.remove(i);
    }
    if idx.len() == 3 && cross(r[idx[0]], r[idx[1]], r[idx[2]]) > 0.0 {
        out.push([idx[0], idx[1], idx[2]]);
    }
    out
}

/// Convex pieces (triangles) of the contour.
pub fn convex_pieces(r: &[V2]) -> Vec<Vec<V2>> {
    triangulate(r)
        .into_iter()
        .map(|t| vec![r[t[0]], r[t[1]], r[t[2]]])
        .collect()
}

pub fn contains(rings: &[Vec<V2>], p: V2) -> bool {
    let Some((outer, holes)) = rings.split_first() else {
        return false;
    };
    point_in_ring(p, outer) >= 0 && holes.iter().all(|h| point_in_ring(p, h) <= 0)
}

/// Parameters t in [0,1] of intersections of segment a→b with contour edges (ascending, including 0 and 1).
pub fn crossings(a: V2, b: V2, rings: &[Vec<V2>]) -> Vec<f64> {
    let d = b - a;
    let mut ts = vec![0.0, 1.0];
    for r in rings {
        for i in 0..r.len() {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            let e = q - p;
            let den = d.cross(e);
            if den.abs() < 1e-15 {
                continue;
            }
            let t = (p - a).cross(e) / den;
            let s = (p - a).cross(d) / den;
            if (-1e-12..=1.0 + 1e-12).contains(&s) && t > 0.0 && t < 1.0 {
                ts.push(t);
            }
        }
    }
    ts.sort_by(f64::total_cmp);
    ts.dedup_by(|x, y| (*x - *y).abs() < 1e-12);
    ts
}

/// Intervals (parameters) of segment a→b inside the contour region.
pub fn inside_intervals(a: V2, b: V2, rings: &[Vec<V2>]) -> Vec<(f64, f64)> {
    let ts = crossings(a, b, rings);
    let mut out: Vec<(f64, f64)> = vec![];
    for w in ts.windows(2) {
        let m = (w[0] + w[1]) * 0.5;
        if contains(rings, a + (b - a) * m) {
            match out.last_mut() {
                Some(l) if (l.1 - w[0]).abs() < 1e-12 => l.1 = w[1],
                _ => out.push((w[0], w[1])),
            }
        }
    }
    out
}

/// Section of a mesh by plane `n·p = c`: segments per triangle.
pub fn slice(m: &Mesh, n: V3, c: f64) -> Vec<(V3, V3)> {
    let mut out = vec![];
    for t in &m.triangles {
        let p = t.map(|i| m.positions[i as usize]);
        let d = p.map(|q| n.dot(q) - c);
        let mut pts = vec![];
        for k in 0..3 {
            let (a, b) = (k, (k + 1) % 3);
            if (d[a] < 0.0 && d[b] >= 0.0) || (d[a] >= 0.0 && d[b] < 0.0) {
                pts.push(p[a] + (p[b] - p[a]) * (d[a] / (d[a] - d[b])));
            }
        }
        if pts.len() == 2 && (pts[0] - pts[1]).len() > 1e-12 {
            out.push((pts[0], pts[1]));
        }
    }
    out
}

/// Assembles segments into contours by matching endpoints (tolerance `eps`); open tails are dropped.
pub fn chain(segs: &[(V2, V2)], eps: f64) -> Vec<Vec<V2>> {
    let mut used = vec![false; segs.len()];
    let mut out = vec![];
    for s in 0..segs.len() {
        if used[s] {
            continue;
        }
        used[s] = true;
        let start = segs[s].0;
        let mut ring = vec![segs[s].0];
        let mut cur = segs[s].1;
        let mut closed = false;
        for _ in 0..segs.len() {
            if (cur - start).len() <= eps {
                closed = true;
                break;
            }
            ring.push(cur);
            let next = (0..segs.len()).find(|&j| {
                !used[j] && ((segs[j].0 - cur).len() <= eps || (segs[j].1 - cur).len() <= eps)
            });
            let Some(j) = next else { break };
            used[j] = true;
            cur = if (segs[j].0 - cur).len() <= eps {
                segs[j].1
            } else {
                segs[j].0
            };
        }
        if closed && ring.len() >= 3 {
            out.push(ring);
        }
    }
    out
}

/// Mesh edges between non-parallel triangles (and boundary edges): view lines.
pub fn feature_edges(m: &Mesh, cos_tol: f64) -> Vec<(V3, V3)> {
    use std::collections::HashMap;
    let mut map: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    let normal = |t: &[u32; 3]| {
        let [a, b, c] = t.map(|i| m.positions[i as usize]);
        let n = (b - a).cross(c - a);
        let l = n.len();
        if l > 0.0 {
            n * (1.0 / l)
        } else {
            n
        }
    };
    let key = |p: V3| {
        let q = |v: f64| (v * 1e7).round() as i64;
        (q(p.x), q(p.y), q(p.z))
    };
    // vertices may be duplicated: merge by coordinates
    let mut ids: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let canon: Vec<u32> = m
        .positions
        .iter()
        .enumerate()
        .map(|(i, &p)| *ids.entry(key(p)).or_insert(i as u32))
        .collect();
    for (ti, t) in m.triangles.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (canon[t[k] as usize], canon[t[(k + 1) % 3] as usize]);
            map.entry((a.min(b), a.max(b))).or_default().push(ti);
        }
    }
    let mut out = vec![];
    for ((a, b), ts) in map {
        let keep = ts.len() != 2 || {
            let (n0, n1) = (normal(&m.triangles[ts[0]]), normal(&m.triangles[ts[1]]));
            n0.dot(n1) < cos_tol
        };
        if keep {
            out.push((m.positions[a as usize], m.positions[b as usize]));
        }
    }
    out.sort_by(|x, y| {
        (x.0.x, x.0.y, x.0.z, x.1.x)
            .partial_cmp(&(y.0.x, y.0.y, y.0.z, y.1.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Region `host` ([outer, holes...]) minus the union of simple contours `cuts`:
/// pieces (outer counter-clockwise, holes clockwise). A cut through an edge yields a notch,
/// a cut across yields several pieces. None if contour assembly did not close (degenerate input).
pub fn subtract(host: &[Vec<V2>], cuts: &[Vec<V2>]) -> Option<Vec<(Vec<V2>, Vec<Vec<V2>>)>> {
    use std::collections::{BTreeSet, HashMap};
    let orient = |r: &[V2], ccw: bool| {
        let mut v = r.to_vec();
        if (area(&v) > 0.0) != ccw {
            v.reverse();
        }
        v
    };
    let (o, hs) = host.split_first()?;
    let mut rings = vec![orient(o, true)];
    rings.extend(hs.iter().map(|h| orient(h, false)));
    let nh = rings.len();
    rings.extend(
        cuts.iter()
            .filter(|c| c.len() >= 3)
            .map(|c| orient(c, true)),
    );
    if rings.iter().any(|r| r.len() < 3) {
        return None;
    }
    let inside =
        |p: V2| contains(&rings[..nh], p) && rings[nh..].iter().all(|c| point_in_ring(p, c) < 0);
    // edges (a, b, ring) and split-point parameters
    let mut edges: Vec<(V2, V2, usize)> = vec![];
    for (k, r) in rings.iter().enumerate() {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            if (b - a).len() > 1e-12 {
                edges.push((a, b, k));
            }
        }
    }
    let mut ts: Vec<Vec<f64>> = vec![vec![0.0, 1.0]; edges.len()];
    let inr = |t: f64| (-1e-9..=1.0 + 1e-9).contains(&t);
    for i in 0..edges.len() {
        for j in i + 1..edges.len() {
            let ((a, b, ki), (c, d, kj)) = (edges[i], edges[j]);
            // host contours are valid and each contour is simple: they intersect only with cuts
            if ki == kj || (ki < nh && kj < nh) {
                continue;
            }
            let (r, q) = (b - a, d - c);
            let den = r.cross(q);
            if den.abs() > 1e-12 * r.len() * q.len() {
                let t = (c - a).cross(q) / den;
                let s = (c - a).cross(r) / den;
                if inr(t) && inr(s) {
                    ts[i].push(t.clamp(0.0, 1.0));
                    ts[j].push(s.clamp(0.0, 1.0));
                }
            } else if (c - a).cross(r).abs() <= 1e-9 * r.len() {
                // overlap on one line: the endpoints of one are split points of the other
                let (rr, qq) = (r.dot(r), q.dot(q));
                for p in [c, d] {
                    let t = (p - a).dot(r) / rr;
                    if inr(t) {
                        ts[i].push(t.clamp(0.0, 1.0));
                    }
                }
                for p in [a, b] {
                    let s = (p - c).dot(q) / qq;
                    if inr(s) {
                        ts[j].push(s.clamp(0.0, 1.0));
                    }
                }
            }
        }
    }
    // vertices with merging of close ones (cell 1e-6, tolerance 1e-7)
    let mut vs: Vec<V2> = vec![];
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    let mut vid = |p: V2, vs: &mut Vec<V2>| -> usize {
        let key = ((p.x / 1e-6).floor() as i64, (p.y / 1e-6).floor() as i64);
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(c) = grid.get(&(key.0 + dx, key.1 + dy)) {
                    if let Some(&k) = c.iter().find(|&&k| (vs[k] - p).len() <= 1e-7) {
                        return k;
                    }
                }
            }
        }
        vs.push(p);
        grid.entry(key).or_default().push(vs.len() - 1);
        vs.len() - 1
    };
    let mut segs: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (e, t) in edges.iter().zip(ts.iter_mut()) {
        t.sort_by(f64::total_cmp);
        let mut ids: Vec<usize> = Vec::with_capacity(t.len());
        for &t in t.iter() {
            let p = if t <= 0.0 {
                e.0
            } else if t >= 1.0 {
                e.1
            } else {
                e.0 + (e.1 - e.0) * t
            };
            ids.push(vid(p, &mut vs));
        }
        for w in ids.windows(2) {
            if w[0] != w[1] {
                segs.insert((w[0].min(w[1]), w[0].max(w[1])));
            }
        }
    }
    let off = |a: V2, b: V2| {
        let len = (b - a).len();
        let d = (b - a) * (1.0 / len);
        (
            (a + b) * 0.5,
            v2(-d.y, d.x) * (len * 1e-3).clamp(1e-9, 1e-5),
        )
    };
    // result boundary, region on the left
    let mut de: Vec<(usize, usize)> = vec![];
    for &(u, v) in &segs {
        let (m, n) = off(vs[u], vs[v]);
        match (inside(m + n), inside(m - n)) {
            (true, false) => de.push((u, v)),
            (false, true) => de.push((v, u)),
            _ => {}
        }
    }
    let mut out_of: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, e) in de.iter().enumerate() {
        out_of.entry(e.0).or_default().push(k);
    }
    let mut used = vec![false; de.len()];
    let mut loops: Vec<Vec<V2>> = vec![];
    for s in 0..de.len() {
        if used[s] {
            continue;
        }
        used[s] = true;
        let mut ring = vec![vs[de[s].0]];
        let mut cur = s;
        loop {
            let (u, v) = de[cur];
            if v == de[s].0 {
                break;
            }
            if ring.len() > de.len() {
                return None;
            }
            ring.push(vs[v]);
            // first edge clockwise from the reverse direction belongs to the same face
            let back = vs[u] - vs[v];
            let ab = back.y.atan2(back.x);
            let cw = |k: usize| {
                let d = vs[de[k].1] - vs[v];
                let mut a = ab - d.y.atan2(d.x);
                while a <= 1e-12 {
                    a += std::f64::consts::TAU;
                }
                a
            };
            let next = out_of
                .get(&v)?
                .iter()
                .copied()
                .filter(|&k| !used[k])
                .min_by(|&x, &y| cw(x).total_cmp(&cw(y)))?;
            used[next] = true;
            cur = next;
        }
        // split points lying on a straight line are removed
        let k = ring.len();
        let mut r: Vec<V2> = vec![];
        for i in 0..k {
            let (p, c, q) = (ring[(i + k - 1) % k], ring[i], ring[(i + 1) % k]);
            let (e0, e1) = (c - p, q - c);
            if e0.cross(e1).abs() > 1e-12 * e0.len() * e1.len() || e0.dot(e1) < 0.0 {
                r.push(c);
            }
        }
        if r.len() >= 3 && area(&r).abs() > 1e-12 {
            loops.push(r);
        }
    }
    let (outs, holes): (Vec<Vec<V2>>, Vec<Vec<V2>>) =
        loops.into_iter().partition(|r| area(r) > 0.0);
    let mut pieces: Vec<(Vec<V2>, Vec<Vec<V2>>)> = outs.into_iter().map(|o| (o, vec![])).collect();
    for h in holes {
        let (m, n) = off(h[0], h[1]);
        let probe = m + n;
        let at = pieces
            .iter()
            .enumerate()
            .filter(|(_, p)| point_in_ring(probe, &p.0) > 0)
            .min_by(|x, y| area(&x.1 .0).total_cmp(&area(&y.1 .0)))
            .map(|(i, _)| i)?;
        pieces[at].1.push(h);
    }
    Some(pieces)
}

/// Area of the pieces (outer minus holes).
pub fn pieces_area(pieces: &[(Vec<V2>, Vec<Vec<V2>>)]) -> f64 {
    pieces
        .iter()
        .map(|(o, hs)| area(o).abs() - hs.iter().map(|h| area(h).abs()).sum::<f64>())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
        vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
    }

    #[test]
    fn subtract_inner_edge_overlap_and_split() {
        let host = vec![rect(0.0, 0.0, 6.0, 4.0)];
        // inside: a hole
        let p = subtract(&host, &[rect(1.0, 1.0, 2.0, 2.0)]).unwrap();
        assert_eq!((p.len(), p[0].1.len()), (1, 1));
        assert!((pieces_area(&p) - 23.0).abs() < 1e-9);
        // through an edge: a notch, no holes
        let p = subtract(&host, &[rect(5.0, 1.0, 7.0, 2.0)]).unwrap();
        assert_eq!((p.len(), p[0].1.len()), (1, 0));
        assert!((pieces_area(&p) - 23.0).abs() < 1e-9);
        // touching the edge from inside (shared edge): also a notch
        let p = subtract(&host, &[rect(5.0, 1.0, 6.0, 2.0)]).unwrap();
        assert_eq!((p.len(), p[0].1.len()), (1, 0));
        assert!((pieces_area(&p) - 23.0).abs() < 1e-9);
        // two overlapping cuts: union, no double subtraction
        let p = subtract(&host, &[rect(1.0, 1.0, 3.0, 3.0), rect(2.0, 2.0, 4.0, 3.5)]).unwrap();
        assert_eq!((p.len(), p[0].1.len()), (1, 1));
        assert!((pieces_area(&p) - (24.0 - 4.0 - 3.0 + 1.0)).abs() < 1e-9);
        // across: two pieces
        let p = subtract(&host, &[rect(2.0, -1.0, 3.0, 5.0)]).unwrap();
        assert_eq!(p.len(), 2);
        assert!((pieces_area(&p) - 20.0).abs() < 1e-9);
        // a cut touching a host hole merges with it
        let host = vec![rect(0.0, 0.0, 6.0, 4.0), rect(1.0, 1.0, 2.0, 2.0)];
        let p = subtract(&host, &[rect(1.5, 1.5, 3.0, 3.0)]).unwrap();
        assert_eq!((p.len(), p[0].1.len()), (1, 1));
        assert!((pieces_area(&p) - (24.0 - 1.0 - 2.25 + 0.25)).abs() < 1e-9);
        // outside: host unchanged
        let p = subtract(&host, &[rect(10.0, 10.0, 11.0, 11.0)]).unwrap();
        assert!((pieces_area(&p) - 23.0).abs() < 1e-9);
    }

    fn sq(x0: f64, y0: f64, s: f64) -> Vec<V2> {
        vec![
            v2(x0, y0),
            v2(x0 + s, y0),
            v2(x0 + s, y0 + s),
            v2(x0, y0 + s),
        ]
    }

    #[test]
    fn simple_ring_rejects_bowtie_and_spikes() {
        let sq = [v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 1.0), v2(0.0, 1.0)];
        assert!(simple_ring(&sq, 1e-6).is_ok());
        let bow = [v2(0.0, 0.0), v2(1.0, 1.0), v2(1.0, 0.0), v2(0.0, 1.0)];
        assert_eq!(simple_ring(&bow, 1e-6), Err("ring self-intersects"));
        let dup = [v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 0.0), v2(0.0, 1.0)];
        assert_eq!(simple_ring(&dup, 1e-6), Err("ring has a zero-length edge"));
    }

    #[test]
    fn clip_and_triangulate_exact() {
        let a = sq(0.0, 0.0, 2.0);
        let b = sq(1.0, 1.0, 2.0);
        assert!((area(&clip_convex(&a, &b)) - 1.0).abs() < 1e-12);
        // non-convex L against a square
        let l = vec![
            v2(0.0, 0.0),
            v2(3.0, 0.0),
            v2(3.0, 1.0),
            v2(1.0, 1.0),
            v2(1.0, 3.0),
            v2(0.0, 3.0),
        ];
        assert!((area(&clip_convex(&l, &sq(0.0, 0.0, 2.0))) - 3.0).abs() < 1e-12);
        let t = triangulate(&l);
        assert_eq!(t.len(), 4);
        let s: f64 = convex_pieces(&l).iter().map(|p| area(p)).sum();
        assert!((s - 5.0).abs() < 1e-12);
        assert!((overlap_area(&sq(0.0, 0.0, 2.0), &convex_pieces(&l)) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn intervals_through_l() {
        let l = vec![
            v2(0.0, 0.0),
            v2(3.0, 0.0),
            v2(3.0, 1.0),
            v2(1.0, 1.0),
            v2(1.0, 3.0),
            v2(0.0, 3.0),
        ];
        let iv = inside_intervals(v2(-1.0, 2.0), v2(4.0, 2.0), &[l]);
        assert_eq!(iv.len(), 1);
        assert!((iv[0].0 - 0.2).abs() < 1e-12 && (iv[0].1 - 0.4).abs() < 1e-12);
    }
}
