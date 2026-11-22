// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fit-out of rooms of any outline. A room on a turned, skewed or radial grid is fitted in
//! the grid's local frame: the storey is turned so the grid axis at the room is plan +x, the
//! room gives its usable rectangle (`usable_rect` inside the faces of its walls, its sides
//! that lie against a wall carried out to that wall's centre line so the fit-out sees the
//! wall, its doors and windows), and the door swings of the walls off the rectangle are kept
//! free. The pieces are then turned back. Trapezoids and wedges are furnished in the
//! rectangle the rule guarantees.

use super::grid::Grid;
use super::usable::{measured, usable_rect};
use super::{dot, perp, seg_dist, unit, P};
use crate::project::fit_rules::{rules_for, Rules};
use crate::project::footprint::{largest, Region};
use crate::project::furnish::{furnish, RoomIn};
use crate::project::rules::rule;
use crate::project::{offset, slots, GProject, ProjectSpec, RoomFit};
use crate::{GFixture, GLayout, GenError, WallRole};

/// Within this a point lies on a line, m.
const ON: f64 = 1e-6;

/// The frame of a grid axis: plan point to local and back.
#[derive(Clone, Copy)]
struct Frame {
    u: P,
    v: P,
}

impl Frame {
    fn new(axis: P) -> Frame {
        let u = unit(axis);
        Frame { u, v: perp(u) }
    }
    fn local(&self, p: P) -> P {
        (dot(p, self.u), dot(p, self.v))
    }
    fn plan(&self, q: P) -> P {
        (self.u.0 * q.0 + self.v.0 * q.1, self.u.1 * q.0 + self.v.1 * q.1)
    }
    fn angle(&self) -> f64 {
        self.u.1.atan2(self.u.0)
    }
}

/// The storey turned into the frame: walls, rooms, columns and shafts; openings keep their
/// walls by index.
fn turned(l: &GLayout, f: Frame) -> GLayout {
    let mut t = l.clone();
    for w in &mut t.walls {
        (w.a, w.b) = (f.local(w.a), f.local(w.b));
    }
    for r in &mut t.rooms {
        r.polygon.iter_mut().for_each(|p| *p = f.local(*p));
        r.point = f.local(r.point);
    }
    let (c, s) = (f.u.0.abs(), f.u.1.abs());
    for col in &mut t.columns {
        col.at = f.local(col.at);
        let (w, d) = col.section;
        col.section = (w * c + d * s, w * s + d * c);
    }
    for sh in &mut t.shafts {
        sh.ring.iter_mut().for_each(|p| *p = f.local(*p));
    }
    t.fixtures.clear();
    t
}

fn ccw(mut r: Vec<P>) -> Vec<P> {
    if crate::check::ring_area(&r) < 0.0 {
        r.reverse();
    }
    r
}

/// The room's usable rectangle in the turned storey: inside its wall faces, each side that
/// lies against a wall carried out to the wall's centre line.
fn rect(l: &GLayout, ring: &[P]) -> Option<[f64; 4]> {
    let half = WallRole::Exterior.thickness() / 2.0;
    let face = largest(offset::inset(&Region { outer: ccw(ring.to_vec()), holes: vec![] }, half))?;
    let u = usable_rect(&face.outer, (1.0, 0.0), rule("grid.min_usable"))?;
    let mut r = [u.lo.0, u.lo.1, u.lo.0 + u.w, u.lo.1 + u.h];
    // side k: (index into r, horizontal wall, outward sign)
    for (k, horiz, out) in [(1, true, -1.0), (2, false, 1.0), (3, true, 1.0), (0, false, -1.0)] {
        let (lo, hi) = if horiz { (r[0], r[2]) } else { (r[1], r[3]) };
        let line = |w: &crate::GWall| if horiz { (w.a.1, w.b.1, w.a.0, w.b.0) } else { (w.a.0, w.b.0, w.a.1, w.b.1) };
        let at = l
            .walls
            .iter()
            .map(line)
            .filter(|&(c0, c1, a, b)| (c0 - c1).abs() < ON && a.min(b) < hi - ON && a.max(b) > lo + ON)
            .map(|(c0, ..)| c0)
            .filter(|&c| {
                let gap = (c - r[k]) * out;
                gap > -ON && gap <= half + ON
            })
            .min_by(|a, b| (a - r[k]).abs().total_cmp(&(b - r[k]).abs()));
        if let Some(c) = at {
            r[k] = c;
        }
    }
    Some(r)
}

/// Door swings, as boxes of the turned storey, of the doors on the room's outline.
fn swings(l: &GLayout, ring: &[P], gap: f64) -> Vec<[f64; 4]> {
    let n = ring.len();
    let on_ring = |p: P| (0..n).any(|i| seg_dist(p, ring[i], ring[(i + 1) % n]) < 1e-3);
    let inside = |p: P| crate::check::inside(ring, p);
    let mut out = vec![];
    for o in l.openings.iter().filter(|o| o.door) {
        let Some(w) = l.walls.get(o.wall) else { continue };
        let d = unit((w.b.0 - w.a.0, w.b.1 - w.a.1));
        let at = |s: f64| (w.a.0 + d.0 * s, w.a.1 + d.1 * s);
        let (p0, p1) = (at(o.along - gap), at(o.along + o.width + gap));
        let mid = at(o.along + o.width / 2.0);
        if !on_ring(mid) {
            continue;
        }
        let mut nv = perp(d);
        let deep = o.width + gap;
        if !inside((mid.0 + nv.0 * 0.5 * deep, mid.1 + nv.1 * 0.5 * deep)) {
            nv = (-nv.0, -nv.1);
        }
        let q = [p0, p1, (p1.0 + nv.0 * deep, p1.1 + nv.1 * deep), (p0.0 + nv.0 * deep, p0.1 + nv.1 * deep)];
        let xs = q.iter().map(|p| p.0);
        let ys = q.iter().map(|p| p.1);
        out.push([
            xs.clone().fold(f64::MAX, f64::min),
            ys.clone().fold(f64::MAX, f64::min),
            xs.fold(f64::MIN, f64::max),
            ys.fold(f64::MIN, f64::max),
        ]);
    }
    out
}

/// Furnishes every room people use on every storey of a plate project in its grid frame.
/// Returns how many of those rooms took a fit-out without an unmet need, of how many.
pub(super) fn fit_out(spec: &ProjectSpec, g: &Grid, p: &mut GProject) -> Result<(usize, usize), GenError> {
    let rules: std::sync::Arc<Rules> = rules_for(spec.furnish.as_ref())
        .map_err(|e| GenError::Infeasible(format!("fit-out rules: {}", e.join("; "))))?;
    let (mut done, mut all) = (0, 0);
    for s in &mut p.storeys {
        let l = &s.layout;
        // what stands outside every room (fins, canopies, trees, balconies) is not fit-out
        let mut fixtures: Vec<GFixture> = l
            .fixtures
            .iter()
            .filter(|x| !l.rooms.iter().any(|r| crate::check::inside(&r.polygon, x.at)))
            .cloned()
            .collect();
        let mut fits: Vec<RoomFit> = vec![];
        for (i, (gr, pr)) in l.rooms.iter().zip(&s.rooms).enumerate() {
            if !measured(pr.kind) || pr.asked.is_none() {
                continue;
            }
            all += 1;
            let f = Frame::new(g.form.axis_at(crate::project::footprint::centroid(&gr.polygon)));
            let tl = turned(l, f);
            let ring = &tl.rooms[i].polygon;
            let Some(r) = rect(&tl, ring) else { continue };
            let room = RoomIn {
                rect: r,
                kind: pr.kind,
                owner: pr.owner,
                name: &gr.name,
                tags: spec.room_tags.of(pr.kind, &gr.name),
                keep: swings(&tl, ring, rules.door_gap),
            };
            let (fx, fit) = furnish(&tl, std::slice::from_ref(&room), s.height, spec.typology, &rules, &mut Default::default());
            if fixtures.len() + fx.len() > slots::FIXTURES_MAX as usize {
                continue;
            }
            fixtures.extend(fx.into_iter().map(|x| GFixture { at: f.plan(x.at), rotation: x.rotation + f.angle(), ..x }));
            for mut ft in fit {
                ft.room = i;
                ft.stack = ft.stack.map(|q| f.plan(q));
                ft.turning.iter_mut().for_each(|t| t.0 = f.plan(t.0));
                done += usize::from(ft.issues.is_empty());
                fits.push(ft);
            }
        }
        s.layout.fixtures = fixtures;
        s.fit = fits;
    }
    Ok((done, all))
}
