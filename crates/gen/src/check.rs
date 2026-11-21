// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Validity of a layout without a model: every generator passes this before it returns.
//! The model's own integrity check is the second gate, in the tests of the app crate.

use crate::{GLayout, GRoom, GWall};

const EPS: f64 = 1e-6;
const MIN_WALL: f64 = 0.2;
/// Common area of two rooms, m2, below which they only touch.
const OVERLAP_AREA: f64 = 1e-3;
/// Least distance of an opening from either end of its wall, m.
pub(crate) const OPEN_END: f64 = 0.15;
/// Least clear gap between two openings on one wall, m.
pub(crate) const OPEN_GAP: f64 = 0.1;
const DEFAULT_STOREY: f64 = 3.0;
/// How far past its wall's face a hosted fixture's point may sit.
const HOST_OFF: f64 = 0.05;

type P = (f64, f64);

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}
fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}
fn len(a: P) -> f64 {
    dot(a, a).sqrt()
}
fn finite(p: P) -> bool {
    p.0.is_finite() && p.1.is_finite()
}

pub(crate) fn ring_area(r: &[P]) -> f64 {
    let n = r.len();
    (0..n).map(|i| cross(r[i], r[(i + 1) % n])).sum::<f64>() / 2.0
}

pub(crate) fn inside(r: &[P], p: P) -> bool {
    let n = r.len();
    let mut odd = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            odd = !odd;
        }
    }
    odd
}

pub(crate) fn dist_to_seg(p: P, a: P, b: P) -> f64 {
    let d = sub(b, a);
    let t = (dot(sub(p, a), d) / dot(d, d)).clamp(0.0, 1.0);
    len(sub(p, (a.0 + d.0 * t, a.1 + d.1 * t)))
}

fn bbox(r: &[P]) -> (P, P) {
    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    for p in r {
        lo = (lo.0.min(p.0), lo.1.min(p.1));
        hi = (hi.0.max(p.0), hi.1.max(p.1));
    }
    (lo, hi)
}

fn check_walls(walls: &[GWall]) -> Result<(), String> {
    for (i, w) in walls.iter().enumerate() {
        if !finite(w.a) || !finite(w.b) {
            return Err(format!("wall {i} is not finite"));
        }
        if !w.bulge.is_finite() || (w.bulge != 0.0 && w.circle().is_none()) {
            return Err(format!("wall {i} has a bad arc"));
        }
        if w.length() < MIN_WALL {
            return Err(format!("wall {i} is too short"));
        }
    }
    for i in 0..walls.len() {
        for j in i + 1..walls.len() {
            check_pair(i, &walls[i], j, &walls[j])?;
        }
    }
    if walls.len() > 1 {
        for (i, w) in walls.iter().enumerate() {
            for end in [w.a, w.b] {
                let touches = walls
                    .iter()
                    .enumerate()
                    .any(|(j, o)| j != i && o.dist(end) < EPS);
                if !touches {
                    return Err(format!("wall {i} has a free end"));
                }
            }
        }
    }
    Ok(())
}

/// `p` lies on the axis of `w`, clear of both its ends.
fn inner(w: &GWall, p: P) -> bool {
    let t = w.along(p);
    w.dist(p) < EPS && t > EPS && t < w.length() - EPS
}

/// Points where the full lines or circles of two walls meet (at least one an arc).
fn meets(a: &GWall, b: &GWall) -> Vec<P> {
    let line_circle = |l: &GWall, c: P, r: f64| -> Vec<P> {
        let d = sub(l.b, l.a);
        let f = sub(l.a, c);
        let (qa, qb, qc) = (dot(d, d), 2.0 * dot(f, d), dot(f, f) - r * r);
        let disc = qb * qb - 4.0 * qa * qc;
        if disc < 0.0 {
            return vec![];
        }
        let s = disc.sqrt();
        [(-qb - s) / (2.0 * qa), (-qb + s) / (2.0 * qa)]
            .into_iter()
            .map(|t| (l.a.0 + d.0 * t, l.a.1 + d.1 * t))
            .collect()
    };
    match (a.circle(), b.circle()) {
        (Some((c, r)), None) => line_circle(b, c, r),
        (None, Some((c, r))) => line_circle(a, c, r),
        (Some((c0, r0)), Some((c1, r1))) => {
            let d = sub(c1, c0);
            let dl = len(d);
            if dl < EPS || dl > r0 + r1 || dl < (r0 - r1).abs() {
                return vec![];
            }
            let x = (dl * dl + r0 * r0 - r1 * r1) / (2.0 * dl);
            let h = (r0 * r0 - x * x).max(0.0).sqrt();
            let (u, n) = ((d.0 / dl, d.1 / dl), (-d.1 / dl, d.0 / dl));
            let m = (c0.0 + u.0 * x, c0.1 + u.1 * x);
            vec![(m.0 + n.0 * h, m.1 + n.1 * h), (m.0 - n.0 * h, m.1 - n.1 * h)]
        }
        (None, None) => vec![],
    }
}

/// Crossing or overlap when one of the walls is an arc.
fn check_arc_pair(i: usize, a: &GWall, j: usize, b: &GWall) -> Result<(), String> {
    if let (Some((c0, r0)), Some((c1, r1))) = (a.circle(), b.circle()) {
        if len(sub(c1, c0)) < EPS && (r0 - r1).abs() < EPS {
            let (ma, mb) = (a.at(a.length() / 2.0), b.at(b.length() / 2.0));
            if [mb, b.a, b.b].iter().any(|p| inner(a, *p)) || inner(b, ma) {
                return Err(format!("walls {i} and {j} overlap"));
            }
            return Ok(());
        }
    }
    if meets(a, b).into_iter().any(|p| inner(a, p) && inner(b, p)) {
        return Err(format!("walls {i} and {j} cross"));
    }
    Ok(())
}

fn check_pair(i: usize, a: &GWall, j: usize, b: &GWall) -> Result<(), String> {
    if a.bulge != 0.0 || b.bulge != 0.0 {
        return check_arc_pair(i, a, j, b);
    }
    let (d1, d2) = (sub(a.b, a.a), sub(b.b, b.a));
    let (l1, l2) = (len(d1), len(d2));
    let den = cross(d1, d2);
    let off = sub(b.a, a.a);
    if den.abs() < EPS * l1 * l2 {
        if (cross(d1, off) / l1).abs() < EPS {
            let t0 = dot(off, d1) / l1;
            let t1 = dot(sub(b.b, a.a), d1) / l1;
            let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(l1));
            if hi - lo > EPS {
                return Err(format!("walls {i} and {j} overlap"));
            }
        }
        return Ok(());
    }
    let t = cross(off, d2) / den * l1;
    let u = cross(off, d1) / den * l2;
    if t > EPS && t < l1 - EPS && u > EPS && u < l2 - EPS {
        return Err(format!("walls {i} and {j} cross"));
    }
    Ok(())
}

/// How far from each axis end of wall `k` an opening must start so it does not cut the mitre
/// of a corner: half the wall's thickness over the tangent of half the angle to the wall it
/// meets there. Right and obtuse corners stay within the usual end margin.
pub fn miter_clear(walls: &[GWall], k: usize) -> (f64, f64) {
    let w = &walls[k];
    let half = w.role.thickness() / 2.0;
    let away = |x: &GWall, at_a: bool| {
        if at_a {
            x.tangent(0.0)
        } else {
            let u = x.tangent(x.length());
            (-u.0, -u.1)
        }
    };
    let end = |p: P, at_a: bool| {
        let u = away(w, at_a);
        let mut c: f64 = 0.0;
        for (j, o) in walls.iter().enumerate() {
            if j == k {
                continue;
            }
            for o_at_a in [true, false] {
                let q = if o_at_a { o.a } else { o.b };
                if (q.0 - p.0).hypot(q.1 - p.1) > EPS {
                    continue;
                }
                let v = away(o, o_at_a);
                let phi = (u.0 * v.0 + u.1 * v.1).clamp(-1.0, 1.0).acos().max(0.17);
                c = c.max(half / (phi / 2.0).tan());
            }
        }
        c
    };
    (end(w.a, true), end(w.b, false))
}

fn check_openings(l: &GLayout) -> Result<(), String> {
    let storey = if l.height > 0.0 {
        l.height
    } else {
        DEFAULT_STOREY
    };
    let mut by_wall: Vec<Vec<(f64, f64)>> = vec![vec![]; l.walls.len()];
    for (i, o) in l.openings.iter().enumerate() {
        let w = l
            .walls
            .get(o.wall)
            .ok_or_else(|| format!("opening {i} has no wall"))?;
        if ![o.along, o.width, o.height, o.sill]
            .iter()
            .all(|x| x.is_finite())
            || o.width <= 0.0
            || o.height <= 0.0
            || o.sill < 0.0
        {
            return Err(format!("opening {i} has a bad size"));
        }
        let wl = w.length();
        if o.along < OPEN_END - EPS || o.along + o.width > wl - OPEN_END + EPS {
            return Err(format!("opening {i} does not fit its wall"));
        }
        if o.sill + o.height > storey + EPS {
            return Err(format!("opening {i} is taller than the wall"));
        }
        by_wall[o.wall].push((o.along, o.along + o.width));
    }
    for (k, spans) in by_wall.iter_mut().enumerate() {
        if spans.is_empty() {
            continue;
        }
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        if spans.windows(2).any(|p| p[1].0 < p[0].1 + OPEN_GAP - EPS) {
            return Err(format!("openings overlap on wall {k}"));
        }
        let w = &l.walls[k];
        for (j, o) in l.walls.iter().enumerate() {
            if j == k {
                continue;
            }
            for end in [o.a, o.b] {
                if w.dist(end) < EPS {
                    let t = w.along(end);
                    if spans
                        .iter()
                        .any(|s| t > s.0 - OPEN_END + EPS && t < s.1 + OPEN_END - EPS)
                    {
                        return Err(format!("an opening on wall {k} meets wall {j}"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Every wall-hosted fixture stands on its own wall: its foot within the axis span, its point on
/// the wall's face. The model refuses a hosted component past its wall's end.
fn check_fixtures(l: &GLayout) -> Result<(), String> {
    for (i, f) in l.fixtures.iter().enumerate() {
        let Some(k) = f.wall else { continue };
        let what = || format!("fixture {i} ({} {}) at ({:.2}, {:.2})", f.family, f.ty, f.at.0, f.at.1);
        let w = l.walls.get(k).ok_or_else(|| format!("{} has no wall {k}", what()))?;
        if !finite(f.at) {
            return Err(format!("{} is not finite", what()));
        }
        let (s, wl) = (w.along(f.at), w.length());
        if s < -EPS || s > wl + EPS {
            return Err(format!("{} is off its wall {k}: {s:.3} m along a {wl:.3} m wall", what()));
        }
        let off = w.dist(f.at);
        if off > w.role.thickness() / 2.0 + HOST_OFF {
            return Err(format!("{} stands {off:.3} m off the axis of its wall {k}", what()));
        }
    }
    Ok(())
}

/// Each edge must lie on walls: a room whose outline is not walled off would not close in the model.
fn edge_on_walls(a: P, b: P, walls: &[GWall]) -> bool {
    let d = sub(b, a);
    let l = len(d);
    let mut spans: Vec<(f64, f64)> = vec![];
    for w in walls {
        if w.bulge != 0.0 {
            // a chord of an arc wall: the outline follows the arc between two of its points
            if w.dist(a) < EPS && w.dist(b) < EPS {
                return true;
            }
            continue;
        }
        if (cross(d, sub(w.a, a)) / l).abs() < EPS && (cross(d, sub(w.b, a)) / l).abs() < EPS {
            let (t0, t1) = (dot(sub(w.a, a), d) / l, dot(sub(w.b, a), d) / l);
            // a collinear wall elsewhere on the same line is not part of this edge
            if t0.max(t1) > EPS && t0.min(t1) < l - EPS {
                spans.push((t0.min(t1), t0.max(t1)));
            }
        }
    }
    spans.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut reach = 0.0;
    for s in spans {
        if s.0 > reach + EPS {
            return false;
        }
        reach = f64::max(reach, s.1);
    }
    reach >= l - EPS
}

fn check_rooms(l: &GLayout) -> Result<(), String> {
    for (i, r) in l.rooms.iter().enumerate() {
        if r.name.trim().is_empty() {
            return Err(format!("room {i} has no name"));
        }
        if r.polygon.len() < 3 || !r.polygon.iter().all(|p| finite(*p)) || !finite(r.point) {
            return Err(format!("room {i} has a bad outline"));
        }
        if ring_area(&r.polygon).abs() < 0.5 {
            return Err(format!("room {i} is too small"));
        }
        if !inside(&r.polygon, r.point) {
            return Err(format!("room {i}: the point is outside"));
        }
        let n = r.polygon.len();
        for k in 0..n {
            let (a, b) = (r.polygon[k], r.polygon[(k + 1) % n]);
            if dist_to_seg(r.point, a, b) < 0.05 {
                return Err(format!("room {i}: the point is on the outline"));
            }
            if !edge_on_walls(a, b, &l.walls) {
                return Err(format!("room {i} is not enclosed (edge {a:?}-{b:?})"));
            }
        }
    }
    for i in 0..l.rooms.len() {
        for j in i + 1..l.rooms.len() {
            if rooms_overlap(&l.rooms[i], &l.rooms[j]) {
                return Err(format!("rooms {i} and {j} overlap"));
            }
        }
    }
    Ok(())
}

fn rooms_overlap(a: &GRoom, b: &GRoom) -> bool {
    let ((alo, ahi), (blo, bhi)) = (bbox(&a.polygon), bbox(&b.polygon));
    let (x0, x1) = (alo.0.max(blo.0), ahi.0.min(bhi.0));
    let (y0, y1) = (alo.1.max(blo.1), ahi.1.min(bhi.1));
    if x1 - x0 <= EPS || y1 - y0 <= EPS {
        return false;
    }
    let c = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    if !(inside(&a.polygon, c) && inside(&b.polygon, c)) {
        return false;
    }
    // the probe may sit on a shared wall of two slanted rooms: measure the common area
    use ddc_geom::{poly, v2};
    let ring = |r: &[P]| -> Vec<ddc_geom::V2> { r.iter().map(|p| v2(p.0, p.1)).collect() };
    poly::overlap_area(&ring(&a.polygon), &poly::convex_pieces(&ring(&b.polygon))).abs() > OVERLAP_AREA
}

fn check_rings(l: &GLayout) -> Result<(), String> {
    if !l.slab.is_empty()
        && (l.slab.len() < 3
            || ring_area(&l.slab).abs() < 0.5
            || !l.slab.iter().all(|p| finite(*p)))
    {
        return Err("the slab has a bad outline".to_string());
    }
    let in_slab = |r: &[P]| l.slab.is_empty() || r.iter().all(|p| inside(&l.slab, *p));
    for (i, h) in l.slab_holes.iter().enumerate() {
        // a core-drilled sleeve for a small pipe is about 9 cm square: only a sliver is degenerate
        if h.len() < 3 || ring_area(h).abs() < 1e-3 || !in_slab(h) {
            return Err(format!("slab hole {i} is outside the slab"));
        }
    }
    for i in 0..l.slab_holes.len() {
        let (a, b) = bbox(&l.slab_holes[i]);
        for j in i + 1..l.slab_holes.len() {
            let (c, d) = bbox(&l.slab_holes[j]);
            if a.0 < d.0 && c.0 < b.0 && a.1 < d.1 && c.1 < b.1 {
                return Err(format!("slab holes {i} and {j} overlap"));
            }
        }
    }
    for (i, s) in l.shafts.iter().enumerate() {
        if s.ring.len() < 3 || ring_area(&s.ring).abs() < 0.01 || !in_slab(&s.ring) {
            return Err(format!("shaft {i} has a bad outline"));
        }
    }
    Ok(())
}

fn check_others(l: &GLayout) -> Result<(), String> {
    for (i, c) in l.columns.iter().enumerate() {
        if !finite(c.at) || !finite(c.section) || c.section.0 <= 0.0 || c.section.1 <= 0.0 {
            return Err(format!("column {i} has a bad size"));
        }
    }
    for (i, g) in l.grids.iter().enumerate() {
        if g.name.is_empty() || !finite(g.a) || !finite(g.b) || len(sub(g.b, g.a)) < MIN_WALL {
            return Err(format!("grid {i} is bad"));
        }
        if l.grids[..i].iter().any(|o| o.name == g.name) {
            return Err(format!("grid name {} repeats", g.name));
        }
    }
    for (i, s) in l.stairs.iter().enumerate() {
        s.def().layout().map_err(|e| format!("stair {i}: {e:?}"))?;
    }
    if let Some(m) = l.metrics.iter().find(|m| !m.1.is_finite()) {
        return Err(format!("metric {} is not finite", m.0));
    }
    Ok(())
}

/// `Err` names the first thing wrong; generators turn it into `GenError::Infeasible`.
pub fn validate_layout(l: &GLayout) -> Result<(), String> {
    check_walls(&l.walls)?;
    check_openings(l)?;
    check_fixtures(l)?;
    check_rooms(l)?;
    check_rings(l)?;
    check_others(l)
}
