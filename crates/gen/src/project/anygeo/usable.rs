//! The usable-rectangle rule. A room on a turned, skewed or radial grid may be a trapezoid or
//! a wedge; it is usable when it holds a rectangle in the grid's local axes whose sides are
//! both at least `grid.min_usable` and whose area is at least `grid.usable_share` of the
//! room. Measured exactly: between two vertex abscissae every boundary is straight, so the
//! free height of a strip is the common part of its cross-sections at those abscissae.

use super::grid::Grid;
use super::{dot, perp, unit, P};
use crate::project::rules::rule;
use crate::project::{GProject, RoomKind};

/// Extra strip edges between the vertices, so a long side is tried in parts too.
const SAMPLES: usize = 8;

/// The largest rectangle of a room in a local frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usable {
    /// Sides along the axis and across it, m.
    pub w: f64,
    pub h: f64,
    /// Its share of the room's area.
    pub share: f64,
    /// Its corner of least coordinates in the local frame: along the axis, then across.
    pub lo: P,
}

/// Inside intervals of the vertical line `x` through a ring of local points.
fn section(r: &[P], x: f64) -> Vec<(f64, f64)> {
    let n = r.len();
    let mut ys = vec![];
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.0 <= x && x < b.0) || (b.0 <= x && x < a.0) {
            ys.push(a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0));
        }
    }
    ys.sort_by(f64::total_cmp);
    ys.chunks_exact(2).map(|c| (c[0], c[1])).collect()
}

fn meet(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = vec![];
    for x in a {
        for y in b {
            let (lo, hi) = (x.0.max(y.0), x.1.min(y.1));
            if hi > lo {
                out.push((lo, hi));
            }
        }
    }
    out
}

/// The largest rectangle with both sides at least `min` in `ring`, its sides along `axis`
/// and across it; `None` when no such rectangle fits.
pub fn usable_rect(ring: &[P], axis: P, min: f64) -> Option<Usable> {
    let u = unit(axis);
    let v = perp(u);
    let loc: Vec<P> = ring.iter().map(|p| (dot(*p, u), dot(*p, v))).collect();
    let area = crate::check::ring_area(&loc).abs();
    if loc.len() < 3 || area < 1e-9 {
        return None;
    }
    let mut xs: Vec<f64> = loc.iter().map(|p| p.0).collect();
    let (x0, x1) = xs.iter().fold((f64::MAX, f64::MIN), |r, x| (r.0.min(*x), r.1.max(*x)));
    xs.extend((1..SAMPLES).map(|i| x0 + (x1 - x0) * i as f64 / SAMPLES as f64));
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-7);
    let eps = 1e-7;
    let mut best: Option<Usable> = None;
    for i in 0..xs.len() {
        for j in i + 1..xs.len() {
            let w = xs[j] - xs[i];
            if w < min - 1e-9 {
                continue;
            }
            // cross-sections just inside the strip ends and at every abscissa between
            let mut free = section(&loc, xs[i] + eps);
            for x in xs[i + 1..j].iter().copied().chain([xs[j] - eps]) {
                if free.is_empty() {
                    break;
                }
                free = meet(&free, &section(&loc, x));
            }
            for (lo, hi) in free {
                let h = hi - lo;
                if h >= min - 1e-9 && best.is_none_or(|b| w * h > b.w * b.h + 1e-12) {
                    best = Some(Usable { w, h, share: w * h / area, lo: (xs[i], lo) });
                }
            }
        }
    }
    best
}

/// Rooms the rule applies to: asked rooms people use, not circulation or the core.
pub(crate) fn measured(kind: RoomKind) -> bool {
    !(kind.circulation() || matches!(kind, RoomKind::Stair | RoomKind::Lift))
}

/// How many of the measured rooms pass the rule, of how many.
pub(crate) fn count(g: &Grid, p: &GProject) -> (usize, usize) {
    let (min, share) = (rule("grid.min_usable"), rule("grid.usable_share"));
    let (mut ok, mut all) = (0, 0);
    for s in &p.storeys {
        for (gr, pr) in s.layout.rooms.iter().zip(&s.rooms) {
            if pr.asked.is_none() || !measured(pr.kind) {
                continue;
            }
            all += 1;
            let axis = g.form.axis_at(crate::project::footprint::centroid(&gr.polygon));
            ok += usize::from(usable_rect(&gr.polygon, axis, min).is_some_and(|u| u.share >= share - 1e-9));
        }
    }
    (ok, all)
}
