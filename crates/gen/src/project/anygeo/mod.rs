//! Any geometry on a plate building (R7). The structural grid follows the form: rotated or
//! skewed lines, rays and rings about a centre, or stations along a curved facade; columns
//! stand where every storey leaves them free, so they carry through. Curved facade runs are
//! one curved wall each ([`curve_walls`]). Rooms are measured in the grid's own axes against
//! the usable-rectangle rule, and every roof is built by the straight skeleton as a check.
//! Arms of any branched plan and terrace steps are cut from one planar arrangement, so L, U,
//! T, Y and ring plans are the same operation. Every decision number is a rule.

mod curves;
mod fitout;
pub(crate) mod grid;
mod roof;
pub(crate) mod shape;
mod usable;
pub(crate) mod vault;

pub use curves::{curve_walls, GCurve};
pub use usable::{usable_rect, Usable};

use super::{GProject, ProjectSpec};
use crate::GenError;

pub(crate) type P = (f64, f64);

pub(crate) fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
pub(crate) fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}
pub(crate) fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}
pub(crate) fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}
pub(crate) fn unit(a: P) -> P {
    let l = a.0.hypot(a.1);
    if l < 1e-12 {
        (1.0, 0.0)
    } else {
        (a.0 / l, a.1 / l)
    }
}
/// The left normal.
pub(crate) fn perp(a: P) -> P {
    (-a.1, a.0)
}
pub(crate) fn seg_dist(p: P, a: P, b: P) -> f64 {
    let d = sub(b, a);
    let l2 = dot(d, d);
    let t = if l2 < 1e-18 { 0.0 } else { (dot(sub(p, a), d) / l2).clamp(0.0, 1.0) };
    dist(p, (a.0 + d.0 * t, a.1 + d.1 * t))
}

/// The structural grid, its columns and the form measures of a plate project, in place.
pub(super) fn apply(spec: &ProjectSpec, p: &mut GProject) -> Result<(), GenError> {
    let Some(base) = p.storeys.first().map(|s| s.layout.slab.clone()) else {
        return Ok(());
    };
    let g = grid::choose(&base);
    let (grids, cols) = grid::place(spec, &g, p);
    if let Some(s) = p.storeys.first_mut() {
        s.layout.grids = grids;
    }
    let ground = cols.first().map_or(0, Vec::len);
    for (s, c) in p.storeys.iter_mut().zip(cols) {
        s.layout.columns = c;
    }
    let (fitted, people) = fitout::fit_out(spec, &g, p)?;
    let curved: usize = p.storeys.iter().map(|s| curve_walls(&s.layout).len()).sum();
    let (ok, all) = usable::count(&g, p);
    let roofs = roof::skeleton_ok(p);
    p.metrics.push(("fitted_rooms", if people == 0 { 100.0 } else { 100.0 * fitted as f64 / people as f64 }));
    p.metrics.push(("grid_form", g.form.index() as f64));
    p.metrics.push(("columns", ground as f64));
    p.metrics.push(("curved_walls", curved as f64));
    p.metrics.push(("usable_rooms", if all == 0 { 100.0 } else { 100.0 * ok as f64 / all as f64 }));
    p.metrics.push(("roof_skeleton", roofs as f64));
    Ok(())
}
