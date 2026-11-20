// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Connection plates of lattice joints: rivet groups laid out from the member ends, the plate
//! outline as the convex hull of every rivet's edge-distance disc, and an independent check that
//! each rivet sits inside its plate with the edge distance and clear of every other rivet.

use super::rivet::{self, RivetGroup};
use crate::math::V3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlateKind {
    /// Lattice node: one plate in a face plane, a rivet group per member end.
    Gusset,
    /// Lacing bar riveted straight onto its chord: the outline is the bar end's contact patch.
    Lacing,
    /// Two diagonals crossing: one rivet through both.
    Crossing,
    /// Cover plate joining two chord pieces.
    Splice,
    /// Column foot with anchor rods.
    Base,
}

impl PlateKind {
    pub const ALL: [PlateKind; 5] = [PlateKind::Gusset, PlateKind::Lacing, PlateKind::Crossing, PlateKind::Splice, PlateKind::Base];
    pub fn key(self) -> &'static str {
        match self {
            PlateKind::Gusset => "gusset",
            PlateKind::Lacing => "lacing",
            PlateKind::Crossing => "crossing",
            PlateKind::Splice => "splice",
            PlateKind::Base => "base",
        }
    }
}

/// A plate in its own plane: origin, in-plane axes, thickness, convex outline and fastener
/// centres in (e1, e2) coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Plate {
    pub kind: PlateKind,
    pub o: V3,
    pub e1: V3,
    pub e2: V3,
    pub t: f64,
    pub outline: Vec<(f64, f64)>,
    /// Fastener and hole diameter.
    pub d: f64,
    pub d0: f64,
    pub rivets: Vec<(f64, f64)>,
}

/// One member end on a plate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct End {
    /// Direction from the node along the member (in the plate plane).
    pub u: V3,
    /// Distance from the node to where the member's rivets may start.
    pub start: f64,
    pub n: usize,
    /// Thinner ply and total grip.
    pub t_ply: f64,
    pub grip: f64,
}

/// Convex hull, counter-clockwise (Andrew's monotone chain).
pub fn hull(mut p: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    p.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    p.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
    if p.len() < 3 {
        return p;
    }
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut h: Vec<(f64, f64)> = vec![];
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &(f64, f64)>> = if pass == 0 { Box::new(p.iter()) } else { Box::new(p.iter().rev()) };
        for &q in it {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    h
}

/// Smallest signed distance from `p` to the edges of a counter-clockwise convex outline
/// (positive inside).
pub fn margin(outline: &[(f64, f64)], p: (f64, f64)) -> f64 {
    let n = outline.len();
    if n < 3 {
        return f64::NEG_INFINITY;
    }
    (0..n)
        .map(|i| {
            let (a, b) = (outline[i], outline[(i + 1) % n]);
            let (ex, ey) = (b.0 - a.0, b.1 - a.1);
            let l = (ex * ex + ey * ey).sqrt().max(1e-15);
            (ex * (p.1 - a.1) - ey * (p.0 - a.0)) / l
        })
        .fold(f64::INFINITY, f64::min)
}

/// Octagon around a disc of radius `r` (the disc lies inside it).
fn disc(c: (f64, f64), r: f64, out: &mut Vec<(f64, f64)>) {
    let rr = r / (std::f64::consts::PI / 8.0).cos();
    for k in 0..8 {
        let a = std::f64::consts::PI / 8.0 + k as f64 * std::f64::consts::FRAC_PI_4;
        out.push((c.0 + rr * a.cos(), c.1 + rr * a.sin()));
    }
}

fn proj(e1: V3, e2: V3, u: V3) -> (f64, f64) {
    let (x, y) = (u.dot(e1), u.dot(e2));
    let l = (x * x + y * y).sqrt().max(1e-15);
    (x / l, y / l)
}

/// Lays out one rivet group per member end, keeping groups of members at an angle clear of
/// each other, and closes the outline around all rivets and the node. Returns the plate and the
/// number of rule findings of the groups.
pub fn build(kind: PlateKind, o: V3, e1: V3, e2: V3, t: f64, ends: &[End]) -> (Plate, usize) {
    let cat = rivet::catalog();
    let t_min = ends.iter().map(|e| e.t_ply).fold(f64::INFINITY, f64::min);
    let t_min = if t_min.is_finite() { t_min } else { t };
    let grip = ends.iter().map(|e| e.grip).fold(0.0, f64::max);
    let groups: Vec<RivetGroup> = ends.iter().map(|e| RivetGroup::layout(e.n.max(1), if e.n > 6 { 2 } else { 1 }, t_min, grip)).collect();
    let mut findings: usize = groups.iter().map(|g| g.check().len()).sum();
    let size = cat.for_ply(t_min);
    let (d, d0) = (size.d, size.d0);
    let dirs: Vec<(f64, f64)> = ends.iter().map(|e| proj(e1, e2, e.u)).collect();
    let across: Vec<f64> = groups.iter().map(|g| g.gauge * (g.lines - 1) as f64).collect();
    let clear = cat.rules.p_min * d0;
    let mut rivets = vec![];
    let mut r_edge: f64 = 0.0;
    // along-extent of each group past its first rivet, and where each placed group starts
    let reach: Vec<f64> = groups.iter().map(|g| g.local_points().iter().map(|p| p.0 - g.end).fold(0.0, f64::max)).collect();
    let mut starts: Vec<f64> = vec![];
    for (i, g) in groups.iter().enumerate() {
        let mut s = ends[i].start;
        for j in 0..ends.len() {
            if i == j {
                continue;
            }
            let c = (dirs[i].0 * dirs[j].0 + dirs[i].1 * dirs[j].1).clamp(-1.0, 1.0);
            let need = clear + (across[i] + across[j]) / 2.0;
            if c <= 0.0 {
                // 90 degrees and more apart: the nearest rivets are both first rivets
                s = s.max(need / (2.0 * ((1.0 - c) / 2.0).sqrt()));
                continue;
            }
            let sin = (1.0 - c * c).max(0.0).sqrt();
            if sin < 1e-3 {
                // two members along one line on one side of the node: the later group goes past
                // the earlier one
                if j < i {
                    // the lines are only nearly parallel: a rivet of j off its axis by up to
                    // across/2 leans toward i by that times sin, so the gap grows by it
                    s = s.max(starts[j] + reach[j] + clear + across[j] / 2.0 * sin);
                }
                continue;
            }
            s = s.max(need / sin);
        }
        starts.push(s);
        let (u, v) = (dirs[i], (-dirs[i].1, dirs[i].0));
        for (a, c) in g.local_points() {
            let along = s + (a - g.end);
            let off = c - g.edge - across[i] / 2.0;
            rivets.push((u.0 * along + v.0 * off, u.1 * along + v.1 * off));
        }
        r_edge = r_edge.max(g.end.max(g.edge));
    }
    if r_edge == 0.0 {
        r_edge = cat.rules.e_min * d0;
    }
    let mut pts = vec![];
    disc((0.0, 0.0), r_edge, &mut pts);
    for &p in &rivets {
        disc(p, r_edge, &mut pts);
    }
    let plate = Plate { kind, o, e1, e2, t, outline: hull(pts), d, d0, rivets };
    findings += check(&plate).len();
    (plate, findings)
}

/// One rivet through two crossing bars.
pub fn crossing(o: V3, e1: V3, e2: V3, t_ply: f64) -> Plate {
    let cat = rivet::catalog();
    let s = cat.for_ply(t_ply);
    let mut pts = vec![];
    disc((0.0, 0.0), cat.rules.e_min * s.d0, &mut pts);
    Plate { kind: PlateKind::Crossing, o, e1, e2, t: 0.0, outline: hull(pts), d: s.d, d0: s.d0, rivets: vec![(0.0, 0.0)] }
}

/// Base plate under a member foot `size` wide: anchor rods from the connection catalogue on a
/// square around the foot, edge distance by the catalogue's layout rule.
pub fn base(o: V3, size: f64, t: f64) -> Plate {
    let c = super::catalog::catalog();
    let (d, n) = c.anchors_for(size);
    let d0 = c.bolt(d).map_or(d + 0.003, |b| b.anchor_hole);
    let e = c.e_layout(d0);
    let a = size / 2.0 + e;
    let mut rivets = vec![(a, a), (-a, a), (-a, -a), (a, -a)];
    if n > 4 {
        rivets.extend([(a, 0.0), (0.0, a), (-a, 0.0), (0.0, -a)]);
    }
    let mut pts = vec![];
    for &p in &rivets {
        disc(p, e, &mut pts);
    }
    let x = crate::math::v3(1.0, 0.0, 0.0);
    let y = crate::math::v3(0.0, 1.0, 0.0);
    Plate { kind: PlateKind::Base, o, e1: x, e2: y, t, outline: hull(pts), d, d0, rivets }
}

/// Every fastener inside the outline with the minimum edge distance, and fasteners in a plate
/// at least the minimum pitch apart. Empty when the plate is valid.
pub fn check(p: &Plate) -> Vec<String> {
    let r = &rivet::catalog().rules;
    let (edge, pitch) = match p.kind {
        PlateKind::Base => (0.0, 0.0),
        _ => (r.e2_min * p.d0, r.p_min * p.d0),
    };
    let mut out = vec![];
    for (i, &q) in p.rivets.iter().enumerate() {
        let m = margin(&p.outline, q);
        let need = if p.kind == PlateKind::Base { p.d0 / 2.0 } else { edge };
        if m + 1e-9 < need {
            out.push(format!("fastener {i} at ({:.3}, {:.3}): {:.1} mm from the plate edge, {:.1} mm needed", q.0, q.1, m * 1e3, need * 1e3));
        }
        for (j, &w) in p.rivets.iter().enumerate().skip(i + 1) {
            let dd = ((q.0 - w.0).powi(2) + (q.1 - w.1).powi(2)).sqrt();
            if dd + 1e-9 < pitch {
                out.push(format!("fasteners {i} and {j}: {:.1} mm apart, {:.1} mm needed", dd * 1e3, pitch * 1e3));
            }
        }
    }
    out
}
