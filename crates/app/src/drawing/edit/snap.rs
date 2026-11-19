//! Snap for drawing tools: ends, intersections, centres, midpoints and nearest points of
//! drawing lines and arcs, with the same kinds (and glyphs) as the model tools.

use std::f64::consts::TAU;

use ddc_geom::{v2, V2};

use super::modify::{cross_cc, cross_lc, cross_ll};
use super::{foot, Drawings};
use crate::drawing::seg_dist;
use crate::snap::SnapKind;

/// A line or an arc (counter-clockwise `a0..a1`, a full turn for a circle).
#[derive(Clone, Copy, Debug)]
pub(super) enum Prim {
    Line(V2, V2),
    Arc { c: V2, r: f64, a0: f64, a1: f64 },
}

/// Priority order: the index is the rank (lower wins).
const KINDS: [SnapKind; 5] = [
    SnapKind::Endpoint,
    SnapKind::Intersection,
    SnapKind::Center,
    SnapKind::Midpoint,
    SnapKind::Nearest,
];
const END: usize = 0;
const CROSS: usize = 1;
const CENTRE: usize = 2;
const MID: usize = 3;
const NEAR: usize = 4;

fn in_sweep(a0: f64, a1: f64, ang: f64) -> bool {
    a1 - a0 >= TAU - 1e-9 || (ang - a0).rem_euclid(TAU) <= a1 - a0 + 1e-9
}

fn at(c: V2, r: f64, a: f64) -> V2 {
    v2(c.x + r * a.cos(), c.y + r * a.sin())
}

impl Prim {
    /// Whether `x`, a point of the carrier line or circle, lies on the primitive itself.
    fn holds(&self, x: V2) -> bool {
        match *self {
            Prim::Line(a, b) => {
                let d = b - a;
                let l2 = d.dot(d);
                l2 > 0.0 && {
                    let t = (x - a).dot(d) / l2;
                    (-1e-9..=1.0 + 1e-9).contains(&t)
                }
            }
            Prim::Arc { c, a0, a1, .. } => in_sweep(a0, a1, (x.y - c.y).atan2(x.x - c.x)),
        }
    }

    /// Whether `p` is outside the primitive's box grown by `tol`.
    fn far(&self, p: V2, tol: f64) -> bool {
        let (lo, hi) = match *self {
            Prim::Line(a, b) => (v2(a.x.min(b.x), a.y.min(b.y)), v2(a.x.max(b.x), a.y.max(b.y))),
            Prim::Arc { c, r, .. } => (v2(c.x - r, c.y - r), v2(c.x + r, c.y + r)),
        };
        p.x < lo.x - tol || p.x > hi.x + tol || p.y < lo.y - tol || p.y > hi.y + tol
    }
}

/// Crossings of two primitives that lie on both; parallel lines have none.
fn crossings(a: &Prim, b: &Prim) -> Vec<V2> {
    let raw = match (*a, *b) {
        (Prim::Line(p, q), Prim::Line(r, s)) => cross_ll(p, q - p, r, s - r),
        (Prim::Line(p, q), Prim::Arc { c, r, .. }) | (Prim::Arc { c, r, .. }, Prim::Line(p, q)) => {
            cross_lc(p, q - p, c, r)
        }
        (Prim::Arc { c: c1, r: r1, .. }, Prim::Arc { c: c2, r: r2, .. }) => {
            cross_cc(c1, r1, c2, r2)
        }
    };
    raw.into_iter().filter(|x| a.holds(*x) && b.holds(*x)).collect()
}

type Best = [Option<(f64, V2)>; 5];

fn offer(best: &mut Best, p: V2, tol: f64, rank: usize, q: V2) {
    let d = (p - q).len();
    if d <= tol && best[rank].is_none_or(|b| d < b.0) {
        best[rank] = Some((d, q));
    }
}

/// The best snap near `p` within `tol`: end > intersection > centre > mid > nearest, else `p`.
/// Only primitives whose box reaches `p` are examined; `own` are points of the line being drawn.
pub(super) fn pick(
    prims: impl Iterator<Item = Prim>,
    own: &[V2],
    p: V2,
    tol: f64,
) -> (V2, SnapKind) {
    let mut best: Best = [None; 5];
    for q in own {
        offer(&mut best, p, tol, END, *q);
    }
    let mut near: Vec<Prim> = vec![];
    for pr in prims {
        if pr.far(p, tol) {
            continue;
        }
        match pr {
            Prim::Line(a, b) => {
                offer(&mut best, p, tol, END, a);
                offer(&mut best, p, tol, END, b);
                offer(&mut best, p, tol, MID, (a + b) * 0.5);
                if seg_dist(p, a, b) <= tol {
                    offer(&mut best, p, tol, NEAR, foot(p, a, b));
                    near.push(pr);
                }
            }
            Prim::Arc { c, r, a0, a1 } => {
                offer(&mut best, p, tol, END, at(c, r, a0));
                offer(&mut best, p, tol, END, at(c, r, a1));
                offer(&mut best, p, tol, CENTRE, c);
                offer(&mut best, p, tol, MID, at(c, r, (a0 + a1) * 0.5));
                let d = p - c;
                let l = d.len();
                if l > 1e-12 && in_sweep(a0, a1, d.y.atan2(d.x)) && (l - r).abs() <= tol {
                    offer(&mut best, p, tol, NEAR, c + d * (r / l));
                    near.push(pr);
                }
            }
        }
    }
    for i in 0..near.len() {
        for j in i + 1..near.len() {
            for x in crossings(&near[i], &near[j]) {
                offer(&mut best, p, tol, CROSS, x);
            }
        }
    }
    best.iter()
        .zip(KINDS)
        .find_map(|(b, k)| b.map(|b| (b.1, k)))
        .unwrap_or((p, SnapKind::Free))
}

impl Drawings {
    /// Snap for drawing: an end of a drawing line or arc or of the line being drawn, else a
    /// crossing, an arc centre, a midpoint or the nearest point, all within `tol`; else `p`
    /// itself with [`SnapKind::Free`].
    pub fn snap_point(&self, level: i64, p: V2, tol: f64) -> (V2, SnapKind) {
        let own = self.edit.draw.as_deref().unwrap_or(&[]);
        pick(self.prims(level), own, p, tol)
    }

    /// Shown lines and arcs of the drawings on `level`.
    pub(super) fn prims(&self, level: i64) -> impl Iterator<Item = Prim> + '_ {
        self.on_level(level).flat_map(|(_, d)| {
            let lines = d
                .u
                .lines
                .iter()
                .filter(|l| d.shown(&l.layer))
                .map(|l| Prim::Line(v2(l.a.0, l.a.1), v2(l.b.0, l.b.1)));
            let arcs = d.u.arcs.iter().filter(|a| d.shown(&a.layer)).map(|a| Prim::Arc {
                c: v2(a.c.0, a.c.1),
                r: a.r,
                a0: a.a0,
                a1: a.a1,
            });
            lines.chain(arcs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ln(ax: f64, ay: f64, bx: f64, by: f64) -> Prim {
        Prim::Line(v2(ax, ay), v2(bx, by))
    }

    #[test]
    fn drawing_snap_intersection_beats_nearest() {
        let ps = [ln(-5.0, 0.0, 5.0, 0.0), ln(0.0, -5.0, 0.0, 5.0)];
        let (q, k) = pick(ps.into_iter(), &[], v2(0.02, 0.01), 0.1);
        assert_eq!(k, SnapKind::Intersection);
        assert!(q.len() < 1e-12);
    }

    #[test]
    fn line_arc_intersection() {
        let ps = [
            ln(-5.0, 0.0, 5.0, 0.0),
            Prim::Arc { c: v2(0.0, 0.0), r: 2.0, a0: -1.0, a1: 1.0 },
        ];
        let (q, k) = pick(ps.into_iter(), &[], v2(2.01, 0.02), 0.1);
        assert_eq!(k, SnapKind::Intersection);
        assert!((q - v2(2.0, 0.0)).len() < 1e-9);
    }

    #[test]
    fn drawing_snap_arc_centre() {
        let ps = [Prim::Arc { c: v2(1.0, 1.0), r: 3.0, a0: 0.0, a1: TAU }];
        let (q, k) = pick(ps.into_iter(), &[], v2(1.03, 0.98), 0.1);
        assert_eq!(k, SnapKind::Center);
        assert!((q - v2(1.0, 1.0)).len() < 1e-12);
    }

    #[test]
    fn arc_end_snap() {
        let ps = [Prim::Arc { c: v2(0.0, 0.0), r: 2.0, a0: 0.0, a1: 1.0 }];
        let e = at(v2(0.0, 0.0), 2.0, 1.0);
        let (q, k) = pick(ps.into_iter(), &[], e + v2(0.02, 0.0), 0.1);
        assert_eq!(k, SnapKind::Endpoint);
        assert!((q - e).len() < 1e-12);
    }

    #[test]
    fn parallel_lines_give_no_intersection() {
        let ps = [ln(-5.0, 0.0, 5.0, 0.0), ln(-5.0, 0.05, 5.0, 0.05)];
        assert!(crossings(&ps[0], &ps[1]).is_empty());
        let (_, k) = pick(ps.into_iter(), &[], v2(1.0, 0.02), 0.1);
        assert_eq!(k, SnapKind::Nearest);
    }

    #[test]
    fn end_beats_intersection_when_coincident() {
        let ps = [ln(0.0, 0.0, 5.0, 0.0), ln(0.0, 0.0, 0.0, 5.0)];
        let (q, k) = pick(ps.into_iter(), &[], v2(0.01, 0.01), 0.1);
        assert_eq!(k, SnapKind::Endpoint);
        assert!(q.len() < 1e-12);
    }

    #[test]
    fn midpoint_snap() {
        let ps = [ln(0.0, 0.0, 6.0, 0.0)];
        let (q, k) = pick(ps.into_iter(), &[], v2(3.02, 0.03), 0.1);
        assert_eq!(k, SnapKind::Midpoint);
        assert!((q - v2(3.0, 0.0)).len() < 1e-12);
    }

    #[test]
    fn nearest_snap_on_a_line() {
        let ps = [ln(0.0, 0.0, 6.0, 0.0)];
        let (q, k) = pick(ps.into_iter(), &[], v2(1.5, 0.05), 0.1);
        assert_eq!(k, SnapKind::Nearest);
        assert!((q - v2(1.5, 0.0)).len() < 1e-12);
    }

    #[test]
    fn arc_arc_intersection() {
        let ps = [
            Prim::Arc { c: v2(0.0, 0.0), r: 2.0, a0: 0.0, a1: TAU },
            Prim::Arc { c: v2(2.0, 0.0), r: 2.0, a0: 0.0, a1: TAU },
        ];
        let x = v2(1.0, 3.0_f64.sqrt());
        let (q, k) = pick(ps.into_iter(), &[], x + v2(0.02, -0.01), 0.1);
        assert_eq!(k, SnapKind::Intersection);
        assert!((q - x).len() < 1e-9);
    }

    #[test]
    fn crossing_outside_the_segment_is_not_snapped() {
        // the carriers meet at (10, 0), beyond the end of the first line
        let ps = [ln(0.0, 0.0, 4.0, 0.0), ln(10.0, -3.0, 10.0, 3.0)];
        assert!(crossings(&ps[0], &ps[1]).is_empty());
    }

    #[test]
    fn point_of_the_line_being_drawn_snaps() {
        let own = [v2(7.0, 7.0)];
        let (q, k) = pick(std::iter::empty(), &own, v2(7.02, 7.01), 0.1);
        assert_eq!(k, SnapKind::Endpoint);
        assert!((q - own[0]).len() < 1e-12);
    }

    #[test]
    fn nothing_near_is_free() {
        let ps = [ln(0.0, 0.0, 5.0, 0.0)];
        let p = v2(2.0, 3.0);
        let (q, k) = pick(ps.into_iter(), &[], p, 0.1);
        assert_eq!((q, k), (p, SnapKind::Free));
    }
}
