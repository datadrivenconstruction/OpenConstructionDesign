// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rooms: faces of the planar subdivision of wall footprint contours (KERNEL.md §5).
//!
//! Nodes closer than `Tol::length` are merged (input order picks the representative), edges are split
//! at nodes and intersection points, and faces are traced along half-edges. A bounded face whose
//! interior point lies in no footprint (exact `point_in_ring`) is a room.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use crate::grid::{bbox_of, BoxGrid};
use crate::math::{v2, Tol, V2};
use crate::predicates::{orient2d, point_in_ring};
use crate::profile::signed_area;

/// Closed region: outer contour counter-clockwise, holes clockwise.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub outer: Vec<V2>,
    pub holes: Vec<Vec<V2>>,
    /// Area net of holes.
    pub area: f64,
    /// Length of all contours (outer + holes).
    pub perimeter: f64,
}

impl Region {
    /// Point strictly inside (not on the boundary, not in a hole).
    pub fn contains(&self, p: V2) -> bool {
        point_in_ring(p, &self.outer) > 0 && self.holes.iter().all(|h| point_in_ring(p, h) < 0)
    }
}

#[derive(Debug, Default)]
pub struct Arrangement {
    /// Rooms, ascending by first (lexicographically smallest) vertex.
    pub rooms: Vec<Region>,
    /// Outer contours of wall groups (counter-clockwise): the basis of a "slab by contour".
    pub outlines: Vec<Vec<V2>>,
}

struct Snap {
    pts: Vec<V2>,
    grid: HashMap<(i64, i64), Vec<u32>>,
    tol: f64,
}

impl Snap {
    fn key(&self, p: V2) -> (i64, i64) {
        (
            (p.x / self.tol).floor() as i64,
            (p.y / self.tol).floor() as i64,
        )
    }
    fn id(&mut self, p: V2) -> u32 {
        let (kx, ky) = self.key(p);
        let mut best: Option<(f64, u32)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &i in self.grid.get(&(kx + dx, ky + dy)).into_iter().flatten() {
                    let d = (self.pts[i as usize] - p).len();
                    if d <= self.tol && best.is_none_or(|b| (d, i) < b) {
                        best = Some((d, i));
                    }
                }
            }
        }
        if let Some((_, i)) = best {
            return i;
        }
        let i = self.pts.len() as u32;
        self.pts.push(p);
        self.grid.entry((kx, ky)).or_default().push(i);
        i
    }
}

fn ring_len(r: &[V2]) -> f64 {
    (0..r.len())
        .map(|i| (r[(i + 1) % r.len()] - r[i]).len())
        .sum()
}

/// Removes vertices lying on the line through their neighbours within tolerance, and starts from the smallest vertex.
fn clean(mut r: Vec<V2>, tol: f64) -> Vec<V2> {
    let mut changed = true;
    while changed && r.len() > 3 {
        changed = false;
        for i in 0..r.len() {
            let n = r.len();
            let (a, p, b) = (r[(i + n - 1) % n], r[i], r[(i + 1) % n]);
            let ab = b - a;
            let l = ab.len();
            if l > 0.0 && (ab.cross(p - a) / l).abs() <= tol && (p - a).dot(ab) > 0.0 {
                r.remove(i);
                changed = true;
                break;
            }
        }
    }
    let k = (0..r.len()).min_by(|&i, &j| lex(r[i], r[j])).unwrap_or(0);
    r.rotate_left(k);
    r
}

fn lex(a: V2, b: V2) -> Ordering {
    a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y))
}

/// Point strictly inside a face with holes: the midpoint of the widest span of a horizontal line
/// drawn through the middle of the largest gap between vertex ordinates.
pub fn interior_point(rings: &[&[V2]]) -> Option<V2> {
    let mut ys: Vec<f64> = rings.iter().flat_map(|r| r.iter().map(|p| p.y)).collect();
    ys.sort_by(f64::total_cmp);
    ys.dedup();
    let (mut gap, mut y) = (0.0, None);
    for w in ys.windows(2) {
        if w[1] - w[0] > gap {
            gap = w[1] - w[0];
            y = Some((w[0] + w[1]) / 2.0);
        }
    }
    let y = y?;
    let mut xs = vec![];
    for r in rings {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            if (a.y < y) != (b.y < y) {
                xs.push(a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x));
            }
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.chunks_exact(2)
        .max_by(|p, q| (p[1] - p[0]).total_cmp(&(q[1] - q[0])))
        .map(|p| v2((p[0] + p[1]) / 2.0, y))
}

fn find(uf: &mut [u32], mut i: u32) -> u32 {
    while uf[i as usize] != i {
        uf[i as usize] = uf[uf[i as usize] as usize];
        i = uf[i as usize];
    }
    i
}

fn clearance(p: V2, rings: &[&[V2]]) -> f64 {
    let mut best = f64::INFINITY;
    for r in rings {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l2 = dx * dx + dy * dy;
            let u = if l2 > 0.0 {
                (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            best = best.min((p.x - a.x - u * dx).hypot(p.y - a.y - u * dy));
        }
    }
    best
}

/// Plan subdivision by footprint contours (`walls` are rings of any orientation). Deterministic.
pub fn arrange(walls: &[Vec<V2>], tol: Tol) -> Arrangement {
    arrange_with_lines(walls, &[], tol)
}

/// Like [`arrange`], plus separators: zero-thickness segments that close a room where there is no
/// wall. Separator ends that do not reach a boundary (dangling edges) are dropped.
pub fn arrange_with_lines(walls: &[Vec<V2>], lines: &[(V2, V2)], tol: Tol) -> Arrangement {
    let t = tol.length;
    let walls: Vec<&Vec<V2>> = walls
        .iter()
        .filter(|w| w.len() >= 3 && w.iter().all(|p| p.x.is_finite() && p.y.is_finite()))
        .collect();
    let mut sn = Snap {
        pts: vec![],
        grid: HashMap::new(),
        tol: t,
    };
    let mut raw: Vec<(u32, u32)> = vec![];
    for w in &walls {
        let ids: Vec<u32> = w.iter().map(|&p| sn.id(p)).collect();
        for i in 0..ids.len() {
            let (a, b) = (ids[i], ids[(i + 1) % ids.len()]);
            if a != b {
                raw.push((a, b));
            }
        }
    }
    for &(p, q) in lines {
        if [p.x, p.y, q.x, q.y].iter().all(|v| v.is_finite()) {
            let (a, b) = (sn.id(p), sn.id(q));
            if a != b {
                raw.push((a, b));
            }
        }
    }
    // Edge self-intersections (footprints should not overlap, but input is sometimes imperfect).
    let boxes: Vec<_> = raw
        .iter()
        .map(|&(a, b)| bbox_of(&[sn.pts[a as usize], sn.pts[b as usize]], t))
        .collect();
    let grid = BoxGrid::new(&boxes);
    let mut cand = vec![];
    let mut cross_pts = vec![];
    for (i, &(a, b)) in raw.iter().enumerate() {
        grid.query(&boxes[i], &mut cand);
        let (pa, pb) = (sn.pts[a as usize], sn.pts[b as usize]);
        for &j in cand.iter().filter(|&&j| j > i) {
            let (c, d) = raw[j];
            let (pc, pd) = (sn.pts[c as usize], sn.pts[d as usize]);
            let (o1, o2) = (orient2d(pa, pb, pc), orient2d(pa, pb, pd));
            let (o3, o4) = (orient2d(pc, pd, pa), orient2d(pc, pd, pb));
            if o1 * o2 < 0.0 && o3 * o4 < 0.0 {
                let s = o3 / (o3 - o4);
                cross_pts.push(pa + (pb - pa) * s);
            }
        }
    }
    for p in cross_pts {
        sn.id(p);
    }
    // Split edges at every node lying on them within tolerance.
    let mut split: BTreeSet<(u32, u32)> = BTreeSet::new();
    let pt_box: Vec<_> = sn.pts.iter().map(|&p| bbox_of(&[p], t)).collect();
    let pgrid = BoxGrid::new(&pt_box);
    for (i, &(a, b)) in raw.iter().enumerate() {
        pgrid.query(&boxes[i], &mut cand);
        let (pa, pb) = (sn.pts[a as usize], sn.pts[b as usize]);
        let d = pb - pa;
        let l2 = d.dot(d);
        let mut on: Vec<(f64, u32)> = vec![(0.0, a), (1.0, b)];
        for &k in &cand {
            let k = k as u32;
            if k == a || k == b {
                continue;
            }
            let p = sn.pts[k as usize];
            let s = (p - pa).dot(d) / l2;
            if s > 0.0 && s < 1.0 && (d.cross(p - pa)).abs() / l2.sqrt() <= t {
                on.push((s, k));
            }
        }
        on.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        for w in on.windows(2) {
            let (u, v) = (w[0].1, w[1].1);
            if u != v {
                split.insert((u.min(v), u.max(v)));
            }
        }
    }
    let mut edges: Vec<(u32, u32)> = split.into_iter().collect();
    if !lines.is_empty() {
        prune_spurs(&mut edges);
    }
    faces(&sn.pts, &edges, &walls, t)
}

/// Removes edges with a dangling end (degree 1) until stable.
fn prune_spurs(edges: &mut Vec<(u32, u32)>) {
    loop {
        let mut deg: HashMap<u32, u32> = HashMap::new();
        for &(a, b) in edges.iter() {
            *deg.entry(a).or_default() += 1;
            *deg.entry(b).or_default() += 1;
        }
        let n = edges.len();
        edges.retain(|&(a, b)| deg[&a] > 1 && deg[&b] > 1);
        if edges.len() == n {
            return;
        }
    }
}

fn faces(pts: &[V2], edges: &[(u32, u32)], walls: &[&Vec<V2>], t: f64) -> Arrangement {
    // Half-edge 2k: a→b, 2k+1: b→a.
    let from = |h: usize| {
        if h % 2 == 0 {
            edges[h / 2].0
        } else {
            edges[h / 2].1
        }
    };
    let to = |h: usize| from(h ^ 1);
    let mut out: Vec<Vec<usize>> = vec![vec![]; pts.len()];
    for h in 0..edges.len() * 2 {
        out[from(h) as usize].push(h);
    }
    let upper = |d: V2| d.y > 0.0 || (d.y == 0.0 && d.x > 0.0);
    for (v, hs) in out.iter_mut().enumerate() {
        let o = pts[v];
        hs.sort_by(|&p, &q| {
            let (a, b) = (pts[to(p) as usize], pts[to(q) as usize]);
            let (ua, ub) = (upper(a - o), upper(b - o));
            if ua != ub {
                return ub.cmp(&ua);
            }
            let s = orient2d(o, a, b);
            if s > 0.0 {
                Ordering::Less
            } else if s < 0.0 {
                Ordering::Greater
            } else {
                p.cmp(&q)
            }
        });
    }
    let mut pos = vec![0usize; edges.len() * 2];
    for hs in &out {
        for (k, &h) in hs.iter().enumerate() {
            pos[h] = k;
        }
    }
    let next = |h: usize| {
        let hs = &out[to(h) as usize];
        hs[(pos[h ^ 1] + hs.len() - 1) % hs.len()]
    };
    let mut uf: Vec<u32> = (0..pts.len() as u32).collect();
    for &(a, b) in edges {
        let (ra, rb) = (find(&mut uf, a), find(&mut uf, b));
        uf[ra.max(rb) as usize] = ra.min(rb);
    }
    let mut seen = vec![false; edges.len() * 2];
    let mut pos_faces: Vec<(Vec<V2>, u32)> = vec![];
    let mut neg: Vec<(Vec<V2>, u32)> = vec![];
    for h0 in 0..edges.len() * 2 {
        if seen[h0] {
            continue;
        }
        let mut ring = vec![];
        let mut h = h0;
        while !seen[h] {
            seen[h] = true;
            ring.push(pts[from(h) as usize]);
            h = next(h);
        }
        let comp = find(&mut uf, from(h0));
        if signed_area(&ring) > 0.0 {
            pos_faces.push((ring, comp));
        } else {
            neg.push((ring, comp));
        }
    }
    // A component's boundary is a hole of the smallest bounded face of another component surrounding it.
    let mut holes: Vec<Vec<usize>> = vec![vec![]; pos_faces.len()];
    let mut outlines = vec![];
    for (k, (ring, comp)) in neg.iter().enumerate() {
        let host = pos_faces
            .iter()
            .enumerate()
            .filter(|(_, f)| f.1 != *comp && point_in_ring(ring[0], &f.0) > 0)
            .min_by(|a, b| signed_area(&a.1 .0).total_cmp(&signed_area(&b.1 .0)))
            .map(|(i, _)| i);
        match host {
            Some(i) => holes[i].push(k),
            None if signed_area(ring) < 0.0 => {
                let mut r = ring.clone();
                r.reverse();
                outlines.push(clean(r, t));
            }
            None => {}
        }
    }
    let wboxes: Vec<_> = walls.iter().map(|w| bbox_of(w, 0.0)).collect();
    let wgrid = BoxGrid::new(&wboxes);
    let mut cand = vec![];
    let mut rooms = vec![];
    for (i, (ring, _)) in pos_faces.iter().enumerate() {
        let mut rings: Vec<&[V2]> = vec![ring];
        rings.extend(holes[i].iter().map(|&k| neg[k].0.as_slice()));
        let Some(p) = interior_point(&rings) else {
            continue;
        };
        wgrid.query(&bbox_of(&[p], 0.0), &mut cand);
        if cand.iter().any(|&w| point_in_ring(p, walls[w]) > 0) {
            continue;
        }
        let outer = clean(ring.clone(), t);
        let hs: Vec<Vec<V2>> = holes[i]
            .iter()
            .map(|&k| clean(neg[k].0.clone(), t))
            .collect();
        let area = signed_area(&outer) + hs.iter().map(|h| signed_area(h)).sum::<f64>();
        let perimeter = ring_len(&outer) + hs.iter().map(|h| ring_len(h)).sum::<f64>();
        // Sliver from snapped nodes: mean width 2A/P or the test point's clearance below tolerance.
        if area.is_nan() || area <= 0.0 || 2.0 * area < t * perimeter || clearance(p, &rings) < t {
            continue;
        }
        rooms.push(Region {
            outer,
            holes: hs,
            area,
            perimeter,
        });
    }
    rooms.sort_by(|a, b| lex(a.outer[0], b.outer[0]));
    outlines.sort_by(|a, b| lex(a[0], b[0]));
    Arrangement { rooms, outlines }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layered::{join, layer_footprints, Layer, Strip};

    #[test]
    fn separator_splits_region_and_spur_is_ignored() {
        // four 0.1 thick walls around a 6x4 room
        let walls: Vec<Vec<V2>> = vec![
            vec![v2(-0.1, -0.1), v2(6.1, -0.1), v2(6.1, 0.0), v2(-0.1, 0.0)],
            vec![v2(6.0, 0.0), v2(6.1, 0.0), v2(6.1, 4.0), v2(6.0, 4.0)],
            vec![v2(-0.1, 4.0), v2(6.1, 4.0), v2(6.1, 4.1), v2(-0.1, 4.1)],
            vec![v2(-0.1, 0.0), v2(0.0, 0.0), v2(0.0, 4.0), v2(-0.1, 4.0)],
        ];
        let tol = Tol::default();
        let a = arrange(&walls, tol);
        assert_eq!(a.rooms.len(), 1);
        assert!((a.rooms[0].area - 24.0).abs() < 1e-9);
        let cut = [(v2(3., 0.), v2(3., 4.))];
        let b = arrange_with_lines(&walls, &cut, tol);
        assert_eq!(b.rooms.len(), 2);
        assert!((b.rooms[0].area - 12.0).abs() < 1e-9 && (b.rooms[1].area - 12.0).abs() < 1e-9);
        assert!((b.rooms[0].perimeter - 14.0).abs() < 1e-9);
        // a free-standing segment and a spur change nothing
        let spur = [(v2(3., 0.), v2(3., 1.)), (v2(1., 1.), v2(2., 1.))];
        let c = arrange_with_lines(&walls, &spur, tol);
        assert_eq!(c.rooms.len(), 1);
        assert!((c.rooms[0].perimeter - a.rooms[0].perimeter).abs() < 1e-9);
        assert_eq!(arrange_with_lines(&walls, &cut, tol).rooms, b.rooms);
    }

    fn footprints(axes: &[((f64, f64), (f64, f64))], th: f64) -> Vec<Vec<V2>> {
        let tol = Tol::default();
        let strips: Vec<Strip> = axes
            .iter()
            .map(|&(a, b)| Strip {
                a: v2(a.0, a.1),
                b: v2(b.0, b.1),
                layers: vec![Layer::new(th)],
                offset0: -th / 2.0,
                base: 0.0,
                height: 3.0,
            })
            .collect();
        let j = join(&strips, tol).unwrap();
        let mut out = vec![];
        for (i, s) in strips.iter().enumerate() {
            for l in layer_footprints(s, &j.pieces[i], i, tol).unwrap() {
                out.extend(l.into_iter().map(|p| p.outer));
            }
        }
        out.extend(j.fillers);
        out
    }

    fn rect(w: f64, h: f64) -> Vec<((f64, f64), (f64, f64))> {
        vec![
            ((0., 0.), (w, 0.)),
            ((w, 0.), (w, h)),
            ((w, h), (0., h)),
            ((0., h), (0., 0.)),
        ]
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn four_walls_one_room() {
        let a = arrange(&footprints(&rect(6., 4.), 0.3), Tol::default());
        assert_eq!(a.rooms.len(), 1);
        let r = &a.rooms[0];
        near(r.area, 21.09);
        near(r.perimeter, 18.8);
        assert_eq!(r.outer.len(), 4);
        assert!(r.contains(v2(3., 2.)) && !r.contains(v2(0., 0.)));
        assert_eq!(a.outlines.len(), 1);
        near(signed_area(&a.outlines[0]), 6.3 * 4.3);
    }

    #[test]
    fn l_plan_with_internal_wall_two_rooms() {
        let p = [(0., 0.), (8., 0.), (8., 4.), (4., 4.), (4., 8.), (0., 8.)];
        let mut axes: Vec<_> = (0..6).map(|i| (p[i], p[(i + 1) % 6])).collect();
        axes.push(((4., 0.), (4., 4.)));
        let a = arrange(&footprints(&axes, 0.2), Tol::default());
        assert_eq!(a.rooms.len(), 2, "{:?}", a.rooms);
        near(a.rooms[0].area, 29.64);
        near(a.rooms[0].perimeter, 23.2);
        near(a.rooms[1].area, 14.44);
        near(a.rooms[1].perimeter, 15.2);
    }

    #[test]
    fn sliver_between_offset_footprints_is_not_a_room() {
        let t = Tol::default().length;
        let bx = |x0: f64, x1: f64, y0: f64, y1: f64| {
            vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
        };
        let frame = |gap: f64| {
            vec![
                bx(0., 3., 0., 4.),
                bx(3. + gap, 6., 0., 4.),
                bx(0., 6., 4., 5.),
                bx(0., 6., -1., 0.),
            ]
        };
        let a = arrange(&frame(0.5 * t), Tol::default());
        assert!(a.rooms.is_empty(), "{:?}", a.rooms);
        let b = arrange(&frame(0.4), Tol::default());
        assert_eq!(b.rooms.len(), 1);
    }

    #[test]
    fn gap_gives_no_room() {
        let mut axes = rect(6., 4.);
        axes[0] = ((0., 0.), (5.5, 0.));
        let a = arrange(&footprints(&axes, 0.3), Tol::default());
        assert!(a.rooms.is_empty(), "{:?}", a.rooms);
    }

    #[test]
    fn column_inside_room_is_hole() {
        let mut f = footprints(&rect(6., 4.), 0.3);
        f.push(vec![v2(2., 1.), v2(3., 1.), v2(3., 2.), v2(2., 2.)]);
        let a = arrange(&f, Tol::default());
        assert_eq!(a.rooms.len(), 1);
        near(a.rooms[0].area, 20.09);
        near(a.rooms[0].perimeter, 22.8);
        assert!(!a.rooms[0].contains(v2(2.5, 1.5)));
    }

    #[test]
    fn deterministic_under_input_order() {
        let mut f = footprints(&rect(6., 4.), 0.3);
        let a = arrange(&f, Tol::default());
        f.reverse();
        let b = arrange(&f, Tol::default());
        assert_eq!(a.rooms, b.rooms);
    }

    #[test]
    fn crossing_input_is_split() {
        // Two overlapping squares: the intersection yields a separate face, with no free area.
        let s = |o: f64| vec![v2(o, o), v2(o + 2., o), v2(o + 2., o + 2.), v2(o, o + 2.)];
        let a = arrange(&[s(0.), s(1.)], Tol::default());
        assert!(a.rooms.is_empty());
        assert_eq!(a.outlines.len(), 1);
        near(signed_area(&a.outlines[0]), 7.0);
    }
}
