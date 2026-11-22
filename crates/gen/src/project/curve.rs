// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Curved long facades: the south or north facade of a bar follows a circular arc through
//! its corners as one arc wall (split only where the wall role changes). Walls into the facade
//! run out to the arc, openings keep their place by arc length, rooms and the slab on it take
//! the extra area: their outlines follow the arc in short chords, while the model bounds the
//! rooms by the arc wall itself.

use super::GProject;
use crate::{GLayout, GWall, GenError, WallRole};
use std::collections::BTreeMap;
use std::f64::consts::{PI, TAU};

const EPS: f64 = 1e-6;
/// Longest chord of a room, slab or roof outline along the arc (sag under 5 mm from a radius
/// of 60 m).
const RING_CHORD: f64 = 1.5;

type P = (f64, f64);

/// The line y = `y0` from x 0 to `w`, pushed out by `s` at its middle; `out` is -1 for the
/// south facade, +1 for the north.
#[derive(Clone, Copy, Debug)]
struct Bow {
    y0: f64,
    w: f64,
    s: f64,
    out: f64,
}

impl Bow {
    fn radius(&self) -> f64 {
        let c = self.w / 2.0;
        (c * c + self.s * self.s) / (2.0 * self.s)
    }

    fn center(&self) -> P {
        (self.w / 2.0, self.y0 - self.out * (self.radius() - self.s))
    }

    fn y(&self, x: f64) -> f64 {
        let c = self.w / 2.0;
        let r = self.radius();
        let dx = (x - c).clamp(-c, c);
        self.y0 + self.out * ((r * r - dx * dx).max(0.0).sqrt() - (r - self.s))
    }

    fn on(&self, p: P) -> bool {
        (p.1 - self.y0).abs() < EPS
    }

    fn put(&self, p: P) -> P {
        if self.on(p) {
            (p.0, self.y(p.0))
        } else {
            p
        }
    }

    /// Bulge of the arc from x `x0` to `x1`: tan of a quarter of its signed sweep.
    fn bulge(&self, x0: f64, x1: f64) -> f64 {
        let c = self.center();
        let ang = |x: f64| (self.y(x) - c.1).atan2(x - c.0);
        let mut d = ang(x1) - ang(x0);
        if d > PI {
            d -= TAU;
        } else if d <= -PI {
            d += TAU;
        }
        (d / 4.0).tan()
    }
}

fn ring_area(r: &[P]) -> f64 {
    let n = r.len();
    ((0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>() / 2.0).abs()
}

/// The ring with its edges on the line replaced by the arc through `xs`, and the per-edge
/// slopes carried along: every chord keeps the slope of the straight edge it came from.
/// `xs` may come in any order (the slab may wind the other way round than the ring).
fn bend_roof(r: &[P], edges: &[Option<f64>], b: Bow, xs: &[f64]) -> (Vec<P>, Vec<Option<f64>>) {
    let n = r.len();
    let keep = edges.len() == n;
    let mut xs = xs.to_vec();
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, c| (*a - *c).abs() < EPS);
    let mut out = Vec::with_capacity(n + xs.len());
    let mut sl = Vec::with_capacity(n + xs.len());
    for i in 0..n {
        let (p, q) = (r[i], r[(i + 1) % n]);
        let e = if keep { edges[i] } else { None };
        out.push(b.put(p));
        sl.push(e);
        if b.on(p) && b.on(q) {
            let (lo, hi) = (p.0.min(q.0), p.0.max(q.0));
            let mut mid: Vec<f64> = xs.iter().copied().filter(|x| *x > lo + EPS && *x < hi - EPS).collect();
            if q.0 < p.0 {
                mid.reverse();
            }
            for x in mid {
                out.push((x, b.y(x)));
                sl.push(e);
            }
        }
    }
    (out, sl)
}

/// The ring alone, see [`bend_roof`].
fn bend_ring(r: &[P], b: Bow, xs: &[f64]) -> Vec<P> {
    bend_roof(r, &[], b, xs).0
}

/// x along the line of a point at `t` from the start of facade wall `w`.
fn x_at(w: &GWall, t: f64) -> f64 {
    if w.b.0 >= w.a.0 {
        w.a.0 + t
    } else {
        w.a.0 - t
    }
}

/// Runs of the facade from west to east: x span and role, one per change of role.
fn runs(l: &GLayout, facade: &[usize]) -> Vec<(f64, f64, WallRole)> {
    let span = |i: usize| {
        let w = &l.walls[i];
        (w.a.0.min(w.b.0), w.a.0.max(w.b.0), w.role)
    };
    let mut pieces: Vec<(f64, f64, WallRole)> = facade.iter().map(|&i| span(i)).collect();
    pieces.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut out: Vec<(f64, f64, WallRole)> = vec![];
    for (x0, x1, role) in pieces {
        match out.last_mut() {
            Some(r) if r.2 == role && (r.1 - x0).abs() < EPS => r.1 = r.1.max(x1),
            _ => out.push((x0, x1, role)),
        }
    }
    out
}

/// Bends one storey; returns the rooms that grew.
fn bend(l: &mut GLayout, b: Bow) -> Result<Vec<usize>, GenError> {
    let facade: Vec<usize> = (0..l.walls.len()).filter(|&i| b.on(l.walls[i].a) && b.on(l.walls[i].b)).collect();
    if facade.is_empty() {
        return Ok(vec![]);
    }
    let runs = runs(l, &facade);
    // outline points on the arc: the wall junctions, then chords of at most RING_CHORD
    let mut xs: Vec<f64> = vec![0.0, b.w];
    for w in &l.walls {
        xs.extend([w.a, w.b].into_iter().filter(|p| b.on(*p)).map(|p| p.0));
    }
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, c| (*a - *c).abs() < EPS);
    let mut more = vec![];
    for p in xs.windows(2) {
        let len = p[1] - p[0];
        let n = (len / RING_CHORD).ceil() as usize;
        more.extend((1..n).map(|k| p[0] + len * k as f64 / n as f64));
    }
    xs.extend(more);
    xs.sort_by(f64::total_cmp);
    // the facade walls give way to one arc wall per run, the other walls keep their order
    let mut map = BTreeMap::new();
    let mut walls = vec![];
    for (i, w) in l.walls.iter().enumerate() {
        if !facade.contains(&i) {
            map.insert(i, walls.len());
            walls.push(GWall {
                a: b.put(w.a),
                b: b.put(w.b),
                role: w.role,
                bulge: w.bulge,
                fire: w.fire,
            });
        }
    }
    let first = walls.len();
    for &(x0, x1, role) in &runs {
        walls.push(GWall {
            a: (x0, b.y(x0)),
            b: (x1, b.y(x1)),
            role,
            bulge: Some(b.bulge(x0, x1)).filter(|k| k.abs() >= ddc_geom::arc::MIN_BULGE).unwrap_or(0.0),
            fire: None,
        });
    }
    let run_of = |x: f64| runs.iter().position(|r| r.0 - EPS <= x && x <= r.1 + EPS);
    for o in &mut l.openings {
        if facade.contains(&o.wall) {
            let x = x_at(&l.walls[o.wall], o.along + o.width / 2.0);
            let k = run_of(x).ok_or_else(|| GenError::Infeasible("an opening left the curved facade".into()))?;
            o.along = walls[first + k].along((x, b.y(x))) - o.width / 2.0;
            o.wall = first + k;
        } else {
            // a wall that runs out to the arc grows at its facade end: an opening measured
            // from that end keeps its place in the plan, not its distance from the end
            let (old, new) = (&l.walls[o.wall], &walls[map[&o.wall]]);
            o.along = new.along(old.at(o.along));
            o.wall = map[&o.wall];
        }
    }
    for f in &mut l.fixtures {
        let Some(wi) = f.wall else { continue };
        if facade.contains(&wi) {
            let x = f.at.0;
            let k = run_of(x).ok_or_else(|| GenError::Infeasible("a fixture left the curved facade".into()))?;
            let w = &walls[first + k];
            let u = w.tangent(w.along((x, b.y(x))));
            // the offset off the straight facade turns with the arc at that point
            let (d, turn) = (f.at.1 - b.y0, u.1.atan2(u.0));
            f.at = (x - d * turn.sin(), b.y(x) + d * turn.cos());
            f.rotation += turn;
            f.wall = Some(first + k);
        } else {
            f.wall = Some(map[&wi]);
        }
    }
    l.walls = walls;
    let mut grew = vec![];
    for (i, r) in l.rooms.iter_mut().enumerate() {
        if r.polygon.iter().any(|p| b.on(*p)) {
            r.polygon = bend_ring(&r.polygon, b, &xs);
            grew.push(i);
        }
    }
    l.slab = bend_ring(&l.slab, b, &xs);
    Ok(grew)
}

/// Bows the south facade by `design.curve` and the north by `design.curve_north`.
pub(super) fn apply(south: f64, north: f64, p: &mut GProject) -> Result<(), GenError> {
    let (w, d) = p.footprint;
    for (s, y0, out) in [(south, 0.0, -1.0), (north, d, 1.0)] {
        if s <= 0.0 {
            continue;
        }
        if s > w / 4.0 {
            return Err(GenError::Infeasible(format!(
                "a facade {w:.1} m long bows out at most {:.1} m",
                w / 4.0
            )));
        }
        let b = Bow { y0, w, s, out };
        let mut added = 0.0;
        let mut top_xs = vec![];
        for st in &mut p.storeys {
            let before = ring_area(&st.layout.slab);
            for i in bend(&mut st.layout, b)? {
                st.rooms[i].grown = true;
            }
            added += ring_area(&st.layout.slab) - before;
            top_xs = st.layout.slab.iter().filter(|q| (q.1 - b.y(q.0)).abs() < EPS).map(|q| q.0).collect();
        }
        for r in std::iter::once(&mut p.roof).chain(&mut p.extra_roofs).filter(|r| r.vault.is_none()) {
            let (ring, edges) = bend_roof(&r.ring, &r.edges, b, &top_xs);
            r.edges = if r.edges.len() == r.ring.len() { edges } else { vec![] };
            r.ring = ring;
        }
        for (k, v) in &mut p.metrics {
            if *k == "gross_floor_area" {
                *v += added;
            }
        }
        p.metrics.push(if out < 0.0 { ("curve_south", s) } else { ("curve_north", s) });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GOpening, GRoom};

    #[test]
    fn the_arc_meets_the_corners_and_bows_out_by_the_sagitta() {
        let b = Bow { y0: 0.0, w: 40.0, s: 3.0, out: -1.0 };
        assert!(b.y(0.0).abs() < 1e-9 && b.y(40.0).abs() < 1e-9);
        assert!((b.y(20.0) + 3.0).abs() < 1e-9);
        let r = bend_ring(&[(0.0, 0.0), (40.0, 0.0), (40.0, 12.0), (0.0, 12.0)], b, &[0.0, 10.0, 30.0, 40.0]);
        assert_eq!(r.len(), 6);
        assert!(ring_area(&r) > 480.0 && ring_area(&r) < 480.0 + 2.0 / 3.0 * 40.0 * 3.0);
    }

    #[test]
    fn the_roof_ring_closes_whatever_way_the_slab_and_the_ring_wind() {
        use ddc_geom::{roof::Footprint, v2};
        let b = Bow { y0: 0.0, w: 40.0, s: 5.0, out: -1.0 };
        let ccw = [(0.0, 0.0), (40.0, 0.0), (40.0, 12.0), (0.0, 12.0)];
        let cw: Vec<P> = ccw.iter().rev().copied().collect();
        let asc = [10.0, 20.0, 30.0];
        let desc = [30.0, 20.0, 10.0];
        for ring in [&ccw[..], &cw[..]] {
            for xs in [&asc[..], &desc[..], &[20.0, 10.0, 30.0, 20.0][..]] {
                let (r, sl) = bend_roof(ring, &[Some(30.0); 4], b, xs);
                assert_eq!((r.len(), sl.len()), (7, 7));
                let f = Footprint { ring: r.iter().map(|q| v2(q.0, q.1)).collect(), slopes: sl, base_z: 0.0, overhang: 0.5, mansard: None };
                assert!(f.build().is_ok_and(|x| !x.faces.is_empty()), "{ring:?} {xs:?}");
            }
        }
    }

    fn bar() -> GLayout {
        // a 40 x 12 bar: south facade in two pieces at the cross wall x = 20, a window on each
        let ext = WallRole::Exterior;
        let walls = vec![
            GWall::line((0.0, 0.0), (20.0, 0.0), ext),
            GWall::line((20.0, 0.0), (40.0, 0.0), ext),
            GWall::line((40.0, 0.0), (40.0, 12.0), ext),
            GWall::line((40.0, 12.0), (0.0, 12.0), ext),
            GWall::line((0.0, 12.0), (0.0, 0.0), ext),
            GWall::line((20.0, 0.0), (20.0, 12.0), WallRole::Partition),
        ];
        let win = |wall: usize, along: f64| GOpening { wall, along, door: false, width: 2.0, height: 1.5, sill: 0.9, ty: None };
        let room = |name: &str, x0: f64, x1: f64| GRoom {
            name: name.into(),
            polygon: vec![(x0, 0.0), (x1, 0.0), (x1, 12.0), (x0, 12.0)],
            point: ((x0 + x1) / 2.0, 6.0),
            apartment: None,
        };
        GLayout {
            walls,
            openings: vec![win(0, 9.0), win(1, 9.0)],
            rooms: vec![room("A", 0.0, 20.0), room("B", 20.0, 40.0)],
            slab: vec![(0.0, 0.0), (40.0, 0.0), (40.0, 12.0), (0.0, 12.0)],
            ..GLayout::default()
        }
    }

    #[test]
    fn the_facade_becomes_one_arc_wall_with_the_sagitta_and_openings_by_arc_length() {
        let mut l = bar();
        crate::check::validate_layout(&l).unwrap();
        let b = Bow { y0: 0.0, w: 40.0, s: 3.0, out: -1.0 };
        let grew = bend(&mut l, b).unwrap();
        assert_eq!(grew, vec![0, 1]);
        let arcs: Vec<&GWall> = l.walls.iter().filter(|w| w.bulge != 0.0).collect();
        assert_eq!(arcs.len(), 1, "{:?}", l.walls);
        let a = arcs[0];
        assert_eq!((a.a.0, a.b.0), (0.0, 40.0));
        assert!(a.a.1.abs() < 1e-9 && a.b.1.abs() < 1e-9);
        // bulge = 2 s / w for the whole arc; its middle lies the sagitta out
        assert!((a.bulge - 2.0 * 3.0 / 40.0).abs() < 1e-9, "{}", a.bulge);
        let mid = a.at(a.length() / 2.0);
        assert!((mid.0 - 20.0).abs() < 1e-9 && (mid.1 + 3.0).abs() < 1e-9, "{mid:?}");
        // arc length ~ chord + 8 s^2 / (3 w) = 40.6
        assert!(a.length() > 40.59 && a.length() < 40.61, "{}", a.length());
        // the cross wall runs out to the arc
        let cross = l.walls.iter().find(|w| w.role == WallRole::Partition).unwrap();
        assert!(a.dist(cross.a) < 1e-9 && (cross.a.1 + 3.0).abs() < 1e-9);
        // both windows sit on the arc wall, centred where they were in x, within its length
        let k = l.walls.iter().position(|w| w.bulge != 0.0).unwrap();
        for (o, x) in l.openings.iter().zip([10.0, 30.0]) {
            assert_eq!(o.wall, k);
            assert!(o.along > 0.0 && o.along + o.width < a.length());
            let c = a.at(o.along + o.width / 2.0);
            assert!((c.0 - x).abs() < 1e-6, "{c:?}");
        }
        crate::check::validate_layout(&l).unwrap();
    }

    #[test]
    fn a_door_in_a_wall_that_runs_out_to_the_arc_keeps_its_place() {
        let mut l = bar();
        l.openings.push(GOpening { wall: 5, along: 8.0, door: true, width: 0.9, height: 2.1, sill: 0.0, ty: None });
        bend(&mut l, Bow { y0: 0.0, w: 40.0, s: 3.0, out: -1.0 }).unwrap();
        let o = l.openings.last().unwrap();
        let c = l.walls[o.wall].at(o.along + o.width / 2.0);
        assert!((c.0 - 20.0).abs() < 1e-9 && (c.1 - 8.45).abs() < 1e-9, "{c:?}");
    }

    #[test]
    fn the_validator_sees_a_wall_through_the_arc() {
        let mut l = bar();
        bend(&mut l, Bow { y0: 0.0, w: 40.0, s: 3.0, out: -1.0 }).unwrap();
        let p = l.walls.iter().position(|w| w.role == WallRole::Partition).unwrap();
        l.walls[p].a = (20.0, -5.0);
        let e = crate::check::validate_layout(&l).unwrap_err();
        assert!(e.contains("cross"), "{e}");
    }

    #[test]
    fn a_role_change_splits_the_arc_on_one_circle() {
        let mut l = bar();
        l.walls[1].role = WallRole::Party;
        let b = Bow { y0: 0.0, w: 40.0, s: 3.0, out: -1.0 };
        bend(&mut l, b).unwrap();
        let arcs: Vec<&GWall> = l.walls.iter().filter(|w| w.bulge != 0.0).collect();
        assert_eq!(arcs.len(), 2);
        let (c0, r0) = arcs[0].circle().unwrap();
        let (c1, r1) = arcs[1].circle().unwrap();
        assert!((c0.0 - c1.0).abs() < 1e-6 && (c0.1 - c1.1).abs() < 1e-6 && (r0 - r1).abs() < 1e-6);
        assert!((arcs[0].b.0 - 20.0).abs() < 1e-9 && arcs[0].b == arcs[1].a);
        crate::check::validate_layout(&l).unwrap();
    }
}
