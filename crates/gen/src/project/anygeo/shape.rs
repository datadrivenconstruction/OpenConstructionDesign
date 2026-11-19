// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan shapes from one planar arrangement: the union of the arms of a branched plan (L, U,
//! T, Y, X, H, a closed ring of arms around a court are all the same call) and the cut of a
//! plate by a straight terrace line. No shape is a special case.

use super::{dot, perp, seg_dist, sub, unit, P};
use crate::project::arrange::Arrangement;
use crate::project::footprint::{self, Region};

/// One arm: its axis from `a` to `b` and its width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Arm {
    pub a: P,
    pub b: P,
    pub w: f64,
}

/// Inside a convex ring given counter-clockwise.
fn in_convex(r: &[P], p: P) -> bool {
    let n = r.len();
    (0..n).all(|i| super::cross(sub(r[(i + 1) % n], r[i]), sub(p, r[i])) >= -1e-9)
}

fn ring_segs(r: &[P], out: &mut Vec<(P, P)>) {
    let n = r.len();
    out.extend((0..n).map(|i| (r[i], r[(i + 1) % n])));
}

/// The arm's rectangle, counter-clockwise; an end inside another arm reaches half its own
/// width further, so arms meeting at an angle close their corner.
fn body(arms: &[Arm], i: usize) -> Vec<P> {
    let Arm { a, b, w } = arms[i];
    let u = unit(sub(b, a));
    let n = perp(u);
    let h = w / 2.0;
    let joined = |p: P| arms.iter().enumerate().any(|(j, o)| j != i && seg_dist(p, o.a, o.b) <= o.w / 2.0 + 1e-9);
    let a2 = if joined(a) { (a.0 - u.0 * h, a.1 - u.1 * h) } else { a };
    let b2 = if joined(b) { (b.0 + u.0 * h, b.1 + u.1 * h) } else { b };
    vec![
        (a2.0 - n.0 * h, a2.1 - n.1 * h),
        (b2.0 - n.0 * h, b2.1 - n.1 * h),
        (b2.0 + n.0 * h, b2.1 + n.1 * h),
        (a2.0 + n.0 * h, a2.1 + n.1 * h),
    ]
}

/// The outline of the arms together and the courts they close.
pub(crate) fn union_arms(arms: &[Arm]) -> Result<(Vec<P>, Vec<Vec<P>>), String> {
    for (i, a) in arms.iter().enumerate() {
        if super::dist(a.a, a.b) < a.w / 2.0 {
            return Err(format!("wing {i} is shorter than half its width"));
        }
    }
    let bodies: Vec<Vec<P>> = (0..arms.len()).map(|i| body(arms, i)).collect();
    let mut segs = vec![];
    for b in &bodies {
        ring_segs(b, &mut segs);
    }
    let ar = Arrangement::new(&segs);
    let labels = ar.label(|p| bodies.iter().any(|b| in_convex(b, p)).then_some(1u8));
    let (mut outs, holes) = ar.region(&labels, 1u8);
    if outs.len() != 1 {
        return Err(format!("the wings make {} separate buildings; every wing must touch another", outs.len()));
    }
    Ok((outs.remove(0), holes))
}

/// The part of `r` on the side of the line `dot(p, dir) = limit` where `dot(p, dir) <= limit`:
/// the largest piece, with the courts left inside it.
pub(crate) fn clip_half(r: &Region, dir: P, limit: f64) -> Option<Region> {
    let mut segs = vec![];
    for ring in std::iter::once(&r.outer).chain(&r.holes) {
        ring_segs(ring, &mut segs);
    }
    let ((x0, y0), (x1, y1)) = footprint::bbox(&r.outer);
    let reach = (x1 - x0).hypot(y1 - y0) + 1.0;
    let c = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let t = perp(dir);
    // the line through the point of the line nearest the box centre
    let o = {
        let s = limit - dot(c, dir);
        (c.0 + dir.0 * s, c.1 + dir.1 * s)
    };
    segs.push(((o.0 - t.0 * reach, o.1 - t.1 * reach), (o.0 + t.0 * reach, o.1 + t.1 * reach)));
    let ar = Arrangement::new(&segs);
    let labels = ar.label(|p| (crate::project::offset::contains(r, p) && dot(p, dir) <= limit).then_some(1u8));
    let (outs, holes) = ar.region(&labels, 1u8);
    let outer = outs.into_iter().max_by(|a, b| crate::check::ring_area(a).abs().total_cmp(&crate::check::ring_area(b).abs()))?;
    let holes = holes.into_iter().filter(|h| h.first().is_some_and(|q| crate::check::inside(&outer, *q))).collect();
    Some(Region { outer, holes })
}
