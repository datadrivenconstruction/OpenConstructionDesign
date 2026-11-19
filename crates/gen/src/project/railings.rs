//! Guard rails where a floor drops away: both sides of every stair flight, the unwalled edges
//! of every floor void that is not a stair or shaft well (atria, galleries), and the deck of a
//! flat roof whose parapet is lower than the guard. Balconies carry their own glass guard.

use super::rules::rule;
use super::{slots, GProject, ProjectSpec, RoofKind, PARAPET};
use crate::{GLayout, GenError};

type P = (f64, f64);

/// What a guard runs along.
#[derive(Clone, Debug, PartialEq)]
pub enum RailOn {
    /// Side 0 left, 1 right, going up stair `stair` of the storey's layout.
    Stair { stair: usize, side: u8 },
    /// Points in the storey frame, z above the storey level.
    Path(Vec<(f64, f64, f64)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct GRailing {
    pub on: RailOn,
    pub height: f64,
}

/// A wall closer than this to a void edge guards it.
const WALLED: f64 = 0.35;
/// Guard set back from the roof edge, inside the parapet.
const ROOF_SETBACK: f64 = 0.45;

/// The guards of every storey, by the `egress.*` rules.
pub(super) fn apply(spec: &ProjectSpec, p: &mut GProject) -> Result<(), GenError> {
    let on = rule("egress.railings") > 0.5;
    let h = rule("egress.railing_height");
    let n = p.storeys.len();
    for k in 0..n {
        let mut out = vec![];
        if on {
            let below = if k > 0 { p.storeys[k - 1].layout.stairs.iter().map(|s| s.origin).collect() } else { vec![] };
            let l = &p.storeys[k].layout;
            for i in 0..l.stairs.len() {
                for side in [0, 1] {
                    out.push(GRailing { on: RailOn::Stair { stair: i, side }, height: h });
                }
            }
            for run in void_edges(l, &below, rule("egress.void_min")) {
                out.push(GRailing { on: RailOn::Path(run.iter().map(|q| (q.0, q.1, 0.0)).collect()), height: h });
            }
        }
        let roof = on
            && k + 1 == n
            && spec.roof == RoofKind::Flat
            && p.roof.vault.is_none()
            && p.shell.is_none()
            && rule("egress.roof_guard") > 0.5
            && h > PARAPET;
        if roof {
            let z = p.roof.elevation - p.storeys[k].elevation;
            let mut ring = inset(&p.roof.ring, ROOF_SETBACK);
            if let Some(&first) = ring.first() {
                ring.push(first);
                out.push(GRailing { on: RailOn::Path(ring.iter().map(|q| (q.0, q.1, z)).collect()), height: h });
            }
        }
        let cap = slots::RAILINGS_MAX as usize;
        if out.len() > cap {
            return Err(GenError::Infeasible(format!(
                "{} guard rails on floor {}; at most {cap}: split the building",
                out.len(),
                k + 1
            )));
        }
        p.storeys[k].railings = out;
    }
    Ok(())
}

/// Open runs along the edges of the voids in `l` that no wall lines; a hole that holds a stair
/// (of this storey or the one below, `below`) or a shaft is a well and keeps its walls, one
/// narrower than `min_w` a service penetration that its cover closes.
pub(super) fn void_edges(l: &GLayout, below: &[P], min_w: f64) -> Vec<Vec<P>> {
    let mut runs = vec![];
    for h in &l.slab_holes {
        if h.len() < 3 {
            continue;
        }
        let (lo, hi) = bbox(h);
        if (hi.0 - lo.0).min(hi.1 - lo.1) < min_w {
            continue;
        }
        let near = |q: P| q.0 > lo.0 - 0.5 && q.0 < hi.0 + 0.5 && q.1 > lo.1 - 0.5 && q.1 < hi.1 + 0.5;
        let well = l.stairs.iter().map(|s| s.origin).chain(below.iter().copied()).any(near)
            || l.shafts.iter().any(|s| !s.ring.is_empty() && near(centroid(&s.ring)));
        if well {
            continue;
        }
        let n = h.len();
        let open: Vec<bool> = (0..n).map(|i| !walled(l, h[i], h[(i + 1) % n])).collect();
        if open.iter().all(|o| *o) {
            let mut r = h.clone();
            r.push(h[0]);
            runs.push(r);
            continue;
        }
        // start after a walled edge so a run never wraps
        let s0 = (0..n).find(|&i| !open[i]).unwrap_or(0) + 1;
        let mut cur: Vec<P> = vec![];
        for j in 0..n {
            let i = (s0 + j) % n;
            if open[i] {
                if cur.is_empty() {
                    cur.push(h[i]);
                }
                cur.push(h[(i + 1) % n]);
            } else if cur.len() >= 2 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
        if cur.len() >= 2 {
            runs.push(cur);
        }
    }
    runs
}

fn walled(l: &GLayout, a: P, b: P) -> bool {
    [0.2, 0.5, 0.8].iter().all(|t| {
        let q = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        l.walls.iter().any(|w| crate::check::dist_to_seg(q, w.a, w.b) <= WALLED)
    })
}

fn bbox(r: &[P]) -> (P, P) {
    r.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), q| {
        ((lo.0.min(q.0), lo.1.min(q.1)), (hi.0.max(q.0), hi.1.max(q.1)))
    })
}

fn centroid(r: &[P]) -> P {
    let n = r.len() as f64;
    let s = r.iter().fold((0.0, 0.0), |a, q| (a.0 + q.0, a.1 + q.1));
    (s.0 / n, s.1 / n)
}

/// `ring` moved `d` inwards: each corner where its two moved edges meet.
pub(super) fn inset(ring: &[P], d: f64) -> Vec<P> {
    let n = ring.len();
    if n < 3 {
        return vec![];
    }
    let area: f64 = (0..n).map(|i| ring[i].0 * ring[(i + 1) % n].1 - ring[(i + 1) % n].0 * ring[i].1).sum();
    let s = if area > 0.0 { 1.0 } else { -1.0 };
    // inward normal of edge i (left of a counter-clockwise ring)
    let edge = |i: usize| {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt().max(1e-12);
        let nn = (-dy / len * s, dx / len * s);
        ((a.0 + nn.0 * d, a.1 + nn.1 * d), (dx, dy))
    };
    (0..n)
        .map(|i| {
            let (p0, d0) = edge((i + n - 1) % n);
            let (p1, d1) = edge(i);
            let den = d0.0 * d1.1 - d0.1 * d1.0;
            if den.abs() < 1e-9 {
                return p1;
            }
            let t = ((p1.0 - p0.0) * d1.1 - (p1.1 - p0.1) * d1.0) / den;
            (p0.0 + d0.0 * t, p0.1 + d0.1 * t)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_square_moves_in_on_every_side() {
        let r = inset(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], 0.5);
        assert_eq!(r.len(), 4);
        for (q, e) in r.iter().zip([(0.5, 0.5), (9.5, 0.5), (9.5, 9.5), (0.5, 9.5)]) {
            assert!((q.0 - e.0).abs() < 1e-9 && (q.1 - e.1).abs() < 1e-9, "{q:?}");
        }
        let cw = inset(&[(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0)], 0.5);
        assert!((cw[0].0 - 0.5).abs() < 1e-9 && (cw[0].1 - 0.5).abs() < 1e-9, "{cw:?}");
    }

    #[test]
    fn a_void_is_guarded_on_its_open_edges_only() {
        let mut l = GLayout::default();
        l.slab_holes.push(vec![(10.0, 10.0), (20.0, 10.0), (20.0, 16.0), (10.0, 16.0)]);
        assert_eq!(void_edges(&l, &[], 1.0), vec![vec![(10.0, 10.0), (20.0, 10.0), (20.0, 16.0), (10.0, 16.0), (10.0, 10.0)]]);
        // a wall along the north edge: the guard runs the other three
        l.walls.push(crate::GWall { a: (8.0, 16.1), b: (22.0, 16.1), role: crate::WallRole::Exterior, bulge: 0.0, fire: None });
        assert_eq!(void_edges(&l, &[], 1.0), vec![vec![(10.0, 16.0), (10.0, 10.0), (20.0, 10.0), (20.0, 16.0)]]);
        // a stair rising from the storey below makes it a well
        assert!(void_edges(&l, &[(12.0, 11.0)], 1.0).is_empty());
        // a riser penetration is no void
        let mut r = GLayout::default();
        r.slab_holes.push(vec![(0.0, 0.0), (0.6, 0.0), (0.6, 2.0), (0.0, 2.0)]);
        assert!(void_edges(&r, &[], 1.0).is_empty());
    }
}
