// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Native forms recognised in a triangle mesh (world coordinates): a planar vertical prism
//! (slab), the floor outline of a body (space), a stack of box or round steps (footing).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::{cross, dot, snap, tess, Why, V3};

/// Grid of the plan and height comparisons, m.
const EPS: f64 = 1e-5;

type K = (i64, i64);
type Ring = Vec<(f64, f64)>;

fn key(x: f64, y: f64) -> K {
    ((x / EPS).round() as i64, (y / EPS).round() as i64)
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn tris(m: &tess::Tri) -> impl Iterator<Item = [V3; 3]> + '_ {
    m.t.iter()
        .map(|t| [m.p[t[0] as usize], m.p[t[1] as usize], m.p[t[2] as usize]])
}

/// Enclosed volume; the winding may run either way round.
pub(super) fn volume(m: &tess::Tri) -> f64 {
    tris(m).map(|[a, b, c]| dot(a, cross(b, c))).sum::<f64>().abs() / 6.0
}

/// Distinct heights of the used vertices, ascending, merged within `EPS`.
fn levels(m: &tess::Tri) -> Vec<f64> {
    let mut z: Vec<f64> = m.t.iter().flatten().map(|&i| m.p[i as usize][2]).collect();
    z.sort_by(f64::total_cmp);
    let mut out: Vec<f64> = vec![];
    for v in z {
        if out.last().is_none_or(|l| v - l > EPS) {
            out.push(v);
        }
    }
    out
}

/// Signed plan area (counter-clockwise positive).
pub(super) fn area(r: &[(f64, f64)]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

/// Drops vertices lying on the line through their neighbours (triangulation splits).
fn simplify(mut v: Ring) -> Ring {
    let mut k = 0;
    while v.len() > 3 && k < v.len() {
        let n = v.len();
        let (a, b, c) = (v[(k + n - 1) % n], v[k], v[(k + 1) % n]);
        let ac = (c.0 - a.0, c.1 - a.1);
        let l = ac.0.hypot(ac.1);
        let off = if l < EPS {
            0.0
        } else {
            ((b.0 - a.0) * ac.1 - (b.1 - a.1) * ac.0).abs() / l
        };
        if off <= EPS {
            v.remove(k);
            k = k.saturating_sub(1);
        } else {
            k += 1;
        }
    }
    v
}

/// Plan outline of the horizontal faces at height `z`: outer ring counter-clockwise first,
/// then the holes clockwise. One connected region only.
pub(super) fn plan_rings(m: &tess::Tri, z: f64) -> Result<Vec<Ring>, Why> {
    plan_rings_of(m, &|t: &[V3; 3]| t.iter().all(|p| (p[2] - z).abs() <= EPS))
}

/// Plan outline of the faces `on` accepts, as `plan_rings`.
fn plan_rings_of(m: &tess::Tri, on: &dyn Fn(&[V3; 3]) -> bool) -> Result<Vec<Ring>, Why> {
    const BAD: Why = "outline not simple";
    let mut count: HashMap<(K, K), usize> = HashMap::new();
    let mut at: HashMap<K, (f64, f64)> = HashMap::new();
    let mut faces = 0.0;
    for t in tris(m) {
        if !on(&t) {
            continue;
        }
        let pa = ((t[1][0] - t[0][0]) * (t[2][1] - t[0][1])
            - (t[1][1] - t[0][1]) * (t[2][0] - t[0][0]))
            / 2.0;
        if pa.abs() <= 1e-12 {
            continue;
        }
        faces += pa.abs();
        let ks: Vec<K> = t
            .iter()
            .map(|p| {
                let k = key(p[0], p[1]);
                at.entry(k).or_insert((p[0], p[1]));
                k
            })
            .collect();
        for i in 0..3 {
            let (a, b) = (ks[i], ks[(i + 1) % 3]);
            if a != b {
                *count.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
    }
    if faces <= 0.0 {
        return Err("no horizontal faces");
    }
    let mut adj: BTreeMap<K, Vec<K>> = BTreeMap::new();
    for (&(a, b), &n) in &count {
        match n {
            1 => {
                adj.entry(a).or_default().push(b);
                adj.entry(b).or_default().push(a);
            }
            2 => {}
            _ => return Err(BAD),
        }
    }
    if adj.values().any(|v| v.len() != 2) {
        return Err(BAD);
    }
    let mut left: BTreeSet<K> = adj.keys().copied().collect();
    let mut rings: Vec<Ring> = vec![];
    while let Some(start) = left.pop_first() {
        let mut ring = vec![start];
        let (mut prev, mut cur) = (start, adj[&start][0]);
        while cur != start {
            if !left.remove(&cur) {
                return Err(BAD);
            }
            ring.push(cur);
            let nb = &adj[&cur];
            let next = if nb[0] == prev { nb[1] } else { nb[0] };
            (prev, cur) = (cur, next);
        }
        let r = simplify(ring.iter().map(|k| at[k]).collect());
        if r.len() < 3 {
            return Err(BAD);
        }
        rings.push(r.into_iter().map(|p| (snap(p.0), snap(p.1))).collect());
    }
    if rings.is_empty() {
        return Err(BAD);
    }
    rings.sort_by(|a, b| area(b).abs().total_cmp(&area(a).abs()));
    for (i, r) in rings.iter_mut().enumerate() {
        if (area(r) > 0.0) != (i == 0) {
            r.reverse();
        }
    }
    let outer = &rings[0];
    if !rings[1..]
        .iter()
        .all(|h| h.iter().all(|p| tess::inside2(outer, *p)))
    {
        return Err("several separate regions");
    }
    // nested islands or overlapping faces would not add up
    let net = net_area(&rings);
    if (net - faces).abs() > 1e-6 * faces.max(1.0) {
        return Err(BAD);
    }
    Ok(rings)
}

fn net_area(rings: &[Ring]) -> f64 {
    rings.iter().map(|r| area(r)).sum()
}

/// Plan rings and z range of a body that is a vertical prism with flat top and bottom.
pub(super) fn prism(m: &tess::Tri) -> Result<(Vec<Ring>, f64, f64), Why> {
    const NOT: Why = "tessellated body is not a planar prism";
    let &[z0, z1] = levels(m).as_slice() else {
        return Err(NOT);
    };
    let bottom = plan_rings(m, z0).map_err(|_| NOT)?;
    let top = plan_rings(m, z1).map_err(|_| NOT)?;
    let keys = |rs: &[Ring]| -> BTreeSet<K> {
        rs.iter().flatten().map(|p| key(p.0, p.1)).collect()
    };
    if keys(&bottom) != keys(&top) {
        return Err(NOT);
    }
    let v = net_area(&top) * (z1 - z0);
    if (volume(m) - v).abs() > 1e-4 * v + 1e-9 {
        return Err(NOT);
    }
    Ok((top, z0, z1))
}

/// A plane z = z0 + g . (x - p) through a face, from its (non-vertical) normal.
fn face_plane(t: &[V3; 3]) -> Option<((f64, f64), f64, (f64, f64))> {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    let l = dot(n, n).sqrt();
    if !(l > 1e-12) || n[2].abs() / l < 1e-6 {
        return None;
    }
    Some(((t[0][0], t[0][1]), t[0][2], (-n[0] / n[2], -n[1] / n[2])))
}

/// A body that is a vertical prism between two parallel planes, flat or sloped (a slab or a
/// roof part): plan rings, bottom and top z at the first ring vertex, and the (point,
/// gradient) of the planes when they slope; like `Rd::sloped_prism` for an extrusion.
#[allow(clippy::type_complexity)]
pub(super) fn sloped_prism(
    m: &tess::Tri,
) -> Result<(Vec<Ring>, f64, f64, Option<((f64, f64), (f64, f64))>), Why> {
    const NOT: Why = "tessellated body is not a planar prism";
    // the largest upward and downward faces give the two planes
    let mut best: [Option<(f64, ((f64, f64), f64, (f64, f64)))>; 2] = [None, None];
    for t in tris(m) {
        let Some(pl) = face_plane(&t) else { continue };
        let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
        let k = usize::from(n[2] < 0.0);
        let a = dot(n, n).sqrt();
        if best[k].is_none_or(|b| a > b.0) {
            best[k] = Some((a, pl));
        }
    }
    let (Some((_, top)), Some((_, bot))) = (best[0], best[1]) else {
        return Err(NOT);
    };
    let g = top.2;
    if (bot.2 .0 - g.0).abs() > 1e-6 || (bot.2 .1 - g.1).abs() > 1e-6 {
        return Err(NOT);
    }
    let z_of = |pl: &((f64, f64), f64, (f64, f64)), x: f64, y: f64| {
        pl.1 + pl.2 .0 * (x - pl.0 .0) + pl.2 .1 * (y - pl.0 .1)
    };
    let on = |pl: ((f64, f64), f64, (f64, f64))| {
        move |t: &[V3; 3]| t.iter().all(|p| (p[2] - z_of(&pl, p[0], p[1])).abs() <= EPS)
    };
    // every face lies on one of the planes or is vertical
    for t in tris(m) {
        if face_plane(&t).is_some() && !on(top)(&t) && !on(bot)(&t) {
            return Err(NOT);
        }
    }
    let up = plan_rings_of(m, &on(top)).map_err(|_| NOT)?;
    let down = plan_rings_of(m, &on(bot)).map_err(|_| NOT)?;
    let keys = |rs: &[Ring]| -> BTreeSet<K> {
        rs.iter().flatten().map(|p| key(p.0, p.1)).collect()
    };
    if keys(&up) != keys(&down) {
        return Err(NOT);
    }
    let p = up[0][0];
    let (z0, z1) = (z_of(&bot, p.0, p.1), z_of(&top, p.0, p.1));
    let v = net_area(&up) * (z1 - z0);
    if !(z1 - z0 > EPS) || (volume(m) - v).abs() > 1e-4 * v + 1e-9 {
        return Err(NOT);
    }
    let slope = (g.0.hypot(g.1) > 1e-9).then_some((p, g));
    Ok((up, z0, z1, slope))
}

/// Floor outline and z range of a body with vertical sides (any top): every vertex stands on
/// the floor outline.
pub(super) fn footprint(m: &tess::Tri) -> Result<(Vec<Ring>, f64, f64), Why> {
    const NOT: Why = "tessellated body has no vertical sides";
    let (rings, z0, z1) = floor(m).map_err(|_| NOT)?;
    let on = |q: (f64, f64)| {
        rings.iter().any(|r| {
            (0..r.len()).any(|i| {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let l2 = dx * dx + dy * dy;
                let t = (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / l2.max(1e-18)).clamp(0.0, 1.0);
                (a.0 + t * dx - q.0).hypot(a.1 + t * dy - q.1) <= EPS
            })
        })
    };
    let inner = |p: &V3| (p[2] - z0).abs() <= EPS;
    if m.t.iter().flatten().map(|&i| &m.p[i as usize]).any(|p| !inner(p) && !on((p[0], p[1]))) {
        return Err(NOT);
    }
    Ok((rings, z0, z1))
}

/// Floor outline (horizontal faces at the lowest height) and z range of any body.
pub(super) fn floor(m: &tess::Tri) -> Result<(Vec<Ring>, f64, f64), Why> {
    let zs = levels(m);
    let (Some(&z0), Some(&z1)) = (zs.first(), zs.last()) else {
        return Err("no body");
    };
    if z1 - z0 <= EPS {
        return Err("zero height");
    }
    let r = plan_rings(m, z0).map_err(|_| "floor outline not found")?;
    Ok((r, z0, z1))
}

/// Isolated footing: steps bottom first (w along the rotated x, l along y; round: w diameter).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Steps {
    pub center: (f64, f64),
    pub rotation: f64,
    pub round: bool,
    pub steps: Vec<(f64, f64, f64)>,
    pub top: f64,
}

/// Stack of concentric box (or round) steps; checked against the enclosed volume.
pub(super) fn steps(m: &tess::Tri) -> Result<Steps, Why> {
    const NOT: Why = "body is not a stack of box or round steps";
    let zs = levels(m);
    if zs.len() < 2 || zs.len() > 17 {
        return Err(NOT);
    }
    let at = |z: f64| -> BTreeMap<K, (f64, f64)> {
        m.t.iter()
            .flatten()
            .map(|&i| m.p[i as usize])
            .filter(|p| (p[2] - z).abs() <= EPS)
            .map(|p| (key(p[0], p[1]), (p[0], p[1])))
            .collect()
    };
    // a step's corners are the points found at both its bottom and its top
    let mut bands: Vec<(Ring, (f64, f64), f64)> = vec![];
    for w in zs.windows(2) {
        let (lo, hi) = (at(w[0]), at(w[1]));
        let mut pts: Ring = lo
            .iter()
            .filter(|(k, _)| hi.contains_key(k))
            .map(|(_, p)| *p)
            .collect();
        if pts.len() < 3 {
            return Err(NOT);
        }
        let n = pts.len() as f64;
        let c = (
            pts.iter().map(|p| p.0).sum::<f64>() / n,
            pts.iter().map(|p| p.1).sum::<f64>() / n,
        );
        let ang = |p: &(f64, f64)| (p.1 - c.1).atan2(p.0 - c.0);
        pts.sort_by(|a, b| ang(a).total_cmp(&ang(b)));
        bands.push((pts, c, w[1] - w[0]));
    }
    let c0 = bands[0].1;
    if bands
        .iter()
        .any(|b| (b.1 .0 - c0.0).hypot(b.1 .1 - c0.1) > 1e-4)
    {
        return Err(NOT);
    }
    let round = bands[0].0.len() >= 12;
    let mut out = vec![];
    let mut rotation = 0.0;
    let mut v = 0.0;
    if round {
        for (pts, c, h) in &bands {
            let r: Vec<f64> = pts.iter().map(|p| (p.0 - c.0).hypot(p.1 - c.1)).collect();
            let r0 = r[0];
            if pts.len() < 12 || r.iter().any(|x| (x - r0).abs() > 1e-4 * r0.max(1.0)) {
                return Err(NOT);
            }
            out.push((snap(2.0 * r0), snap(2.0 * r0), snap(*h)));
            v += area(pts) * h;
        }
    } else {
        let p = &bands[0].0;
        rotation = (p[1].1 - p[0].1)
            .atan2(p[1].0 - p[0].0)
            .rem_euclid(std::f64::consts::FRAC_PI_2);
        let (cs, sn) = (rotation.cos(), rotation.sin());
        for (pts, c, h) in &bands {
            if pts.len() != 4 {
                return Err(NOT);
            }
            let uv: Vec<(f64, f64)> = pts
                .iter()
                .map(|p| {
                    let (x, y) = (p.0 - c.0, p.1 - c.1);
                    (x * cs + y * sn, -x * sn + y * cs)
                })
                .collect();
            let (hw, hl) = (uv[0].0.abs(), uv[0].1.abs());
            // corners of an axis-aligned rectangle in the step's own frame
            if uv
                .iter()
                .any(|q| (q.0.abs() - hw).abs() > 1e-4 || (q.1.abs() - hl).abs() > 1e-4)
            {
                return Err(NOT);
            }
            out.push((snap(2.0 * hw), snap(2.0 * hl), snap(*h)));
            v += 4.0 * hw * hl * h;
        }
    }
    if (volume(m) - v).abs() > 1e-3 * v {
        return Err(NOT);
    }
    Ok(Steps {
        center: (snap(c0.0), snap(c0.1)),
        rotation,
        round,
        steps: out,
        top: zs[zs.len() - 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed box prism over the ring (counter-clockwise), from z0 to z1.
    fn prism_mesh(m: &mut tess::Tri, r: &[(f64, f64)], z0: f64, z1: f64) {
        let n = r.len();
        let b: Vec<V3> = r.iter().map(|p| [p.0, p.1, z0]).collect();
        let t: Vec<V3> = r.iter().map(|p| [p.0, p.1, z1]).collect();
        for i in 1..n - 1 {
            m.tri(b[0], b[i + 1], b[i]);
            m.tri(t[0], t[i], t[i + 1]);
        }
        for i in 0..n {
            let j = (i + 1) % n;
            m.quad(b[i], b[j], t[j], t[i]);
        }
    }

    fn rect(cx: f64, cy: f64, w: f64, l: f64) -> Ring {
        vec![
            (cx - w / 2.0, cy - l / 2.0),
            (cx + w / 2.0, cy - l / 2.0),
            (cx + w / 2.0, cy + l / 2.0),
            (cx - w / 2.0, cy + l / 2.0),
        ]
    }

    #[test]
    fn an_l_shaped_prism_gives_its_outline_and_a_wedge_does_not() {
        let l = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 2.0), (2.0, 2.0), (2.0, 5.0), (0.0, 5.0)];
        let mut m = tess::Tri::default();
        prism_mesh(&mut m, &l, -0.2, 0.0);
        let (rings, z0, z1) = prism(&m).unwrap();
        assert_eq!((z0, z1), (-0.2, 0.0));
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 6);
        assert!((area(&rings[0]) - 14.0).abs() < 1e-9);
        let mut w = tess::Tri::default();
        let (a, b, c, d) = ([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        w.tri(a, c, b);
        w.tri(a, b, d);
        w.tri(a, d, c);
        w.tri(b, c, d);
        assert!(prism(&w).is_err());
    }

    #[test]
    fn a_rotated_stepped_footing_is_read_back() {
        let mut m = tess::Tri::default();
        let turn = |r: Ring, a: f64| -> Ring {
            r.into_iter()
                .map(|p| (p.0 * a.cos() - p.1 * a.sin() + 3.0, p.0 * a.sin() + p.1 * a.cos() + 1.0))
                .collect()
        };
        prism_mesh(&mut m, &turn(rect(0.0, 0.0, 1.8, 1.2), 0.3), -1.0, -0.7);
        prism_mesh(&mut m, &turn(rect(0.0, 0.0, 1.0, 0.6), 0.3), -0.7, -0.4);
        let s = steps(&m).unwrap();
        assert!(!s.round);
        assert!((s.rotation - 0.3).abs() < 1e-9, "{}", s.rotation);
        assert!((s.center.0 - 3.0).abs() < 1e-9 && (s.center.1 - 1.0).abs() < 1e-9);
        assert_eq!(s.steps, vec![(1.8, 1.2, 0.3), (1.0, 0.6, 0.3)]);
        assert!((s.top + 0.4).abs() < 1e-9);
    }
}
