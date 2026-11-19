// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Checks of the designed plant against its own rules: every nozzle on exactly one line and
//! reached by pipe; pipes clear of each other, of the units, of the steel; maintenance zones,
//! the walkway and the wall clearances free; every hand-operated valve within reach of the
//! floor or a platform; drains falling to the sump.

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::route3d::seg_seg_dist;
use serde_json::{json, Value as J};

use crate::project::rules::rule;

use super::layout::{Lay, STEEL_R};
use super::route::{Piping, Placed, Seg};
use super::topo::Topo;
use super::{EqKind, End, Svc};

/// Tolerance of a clearance, m: rounding of the routed coordinates.
const TOL: f64 = 1e-6;
/// Steps of the search for the closest point of a segment to a box.
const STEPS: usize = 60;

/// One finding.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub check: &'static str,
    pub what: String,
}

/// What was checked and what failed.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub ok: bool,
    pub issues: Vec<Issue>,
    pub nozzles: usize,
    pub connected: usize,
    /// Pairs of pipes checked, and the least margin over the required gap, m.
    pub pipe_pairs: usize,
    pub min_margin: f64,
    pub valves: usize,
    pub valves_in_reach: usize,
}

impl Report {
    pub fn to_json(&self) -> J {
        json!({"ok": self.ok, "nozzles": self.nozzles, "connected": self.connected,
               "pipe_pairs": self.pipe_pairs, "min_margin_m": (self.min_margin * 1000.0).round() / 1000.0,
               "valves": self.valves, "valves_in_reach": self.valves_in_reach,
               "issues": self.issues.iter().map(|i| json!({"check": i.check, "what": i.what})).collect::<Vec<_>>()})
    }
}

fn point_box(p: V3, lo: V3, hi: V3) -> f64 {
    let d = |x: f64, a: f64, b: f64| (a - x).max(0.0).max(x - b);
    v3(d(p.x, lo.x, hi.x), d(p.y, lo.y, hi.y), d(p.z, lo.z, hi.z)).len()
}

/// Distance of segment `a..b` to the box: the distance is convex along the segment.
pub(super) fn seg_box(a: V3, b: V3, lo: V3, hi: V3) -> f64 {
    let f = |t: f64| point_box(a + (b - a) * t, lo, hi);
    let (mut l, mut h) = (0.0, 1.0);
    for _ in 0..STEPS {
        let m1 = l + (h - l) / 3.0;
        let m2 = h - (h - l) / 3.0;
        if f(m1) <= f(m2) {
            h = m2;
        } else {
            l = m1;
        }
    }
    f((l + h) / 2.0).min(f(0.0)).min(f(1.0))
}

/// Boxes overlap with some volume.
fn overlap(a: (V3, V3), b: (V3, V3)) -> bool {
    let o = |a0: f64, a1: f64, b0: f64, b1: f64| a1.min(b1) - a0.max(b0) > TOL;
    o(a.0.x, a.1.x, b.0.x, b.1.x) && o(a.0.y, a.1.y, b.0.y, b.1.y) && o(a.0.z, a.1.z, b.0.z, b.1.z)
}

/// Where an operated item is worked from: `floor`, a platform tag, or None out of reach.
pub(super) fn access_of(i: &Placed, lay: &Lay) -> Option<String> {
    let reach = rule("plant.access.reach");
    if i.op.z <= reach + TOL {
        return Some("floor".into());
    }
    let rh = rule("plant.access.reach_h");
    lay.platforms
        .iter()
        .find(|p| {
            let up = i.op.z - p.deck;
            let plan = point_box(v3(i.op.x, i.op.y, 0.0), v3(p.lo.x, p.lo.y, 0.0), v3(p.hi.x, p.hi.y, 0.0));
            up >= -TOL && up <= reach + TOL && plan <= rh + TOL
        })
        .map(|p| p.tag.clone())
}

pub(super) fn run(t: &Topo, lay: &Lay, p: &Piping) -> Report {
    let mut issues = vec![];
    let mut bad = |check: &'static str, what: String| issues.push(Issue { check, what });
    let gap = rule("plant.pipe.gap");
    let num = |s: &Seg| t.lines[s.line].number();

    // every nozzle on one line, and its stub routed
    let (mut nozzles, mut connected) = (0, 0);
    for (ei, e) in t.eqs.iter().enumerate() {
        for (ni, n) in e.nozzles.iter().enumerate() {
            nozzles += 1;
            let end = End { eq: ei, nz: ni };
            let on = t.lines.get(n.line).map_or(0, |l| l.all_ends().iter().filter(|x| **x == end).count());
            let routed = p.segs.iter().any(|s| s.stub == Some(end));
            if on == 1 && routed {
                connected += 1;
            } else {
                bad("connect", format!("{}.{} is on {on} lines, routed: {routed}", e.tag, n.name));
            }
        }
    }
    for u in &p.unrouted {
        bad("route", format!("no route reached {}.{}", t.eqs[u.eq].tag, t.eqs[u.eq].nozzles[u.nz].name));
    }

    // pipe to pipe
    let mut pairs = 0;
    let mut min_margin = f64::INFINITY;
    for (i, a) in p.segs.iter().enumerate() {
        for b in &p.segs[i + 1..] {
            if a.line == b.line {
                continue;
            }
            pairs += 1;
            let m = seg_seg_dist(a.a, a.b, b.a, b.b) - (a.env + b.env + gap);
            min_margin = min_margin.min(m);
            if m < -TOL {
                bad("pipe_pipe", format!("{} and {} are {:.0} mm too close", num(a), num(b), -m * 1000.0));
            }
        }
    }

    // pipe to units and flues
    let need = |s: &Seg| s.env + gap / 2.0 - TOL;
    for s in &p.segs {
        for (ei, e) in t.eqs.iter().enumerate() {
            if s.stub.is_some_and(|x| x.eq == ei) {
                continue;
            }
            for (lo, hi) in e.body() {
                if seg_box(s.a, s.b, lo, hi) < need(s) {
                    bad("pipe_unit", format!("{} runs into {}", num(s), e.tag));
                    break;
                }
            }
        }
        for f in &lay.flues {
            let r = v3(f.d / 2.0, f.d / 2.0, 0.0);
            if seg_box(s.a, s.b, f.at - r, v3(f.at.x, f.at.y, f.top) + r) < need(s) {
                bad("pipe_unit", format!("{} runs into the flue of {}", num(s), f.tag));
            }
        }
        for st in &lay.steel {
            if st.line == Some(s.line) {
                continue;
            }
            if seg_seg_dist(s.a, s.b, st.a, st.b) < s.env + STEEL_R - TOL {
                bad("pipe_steel", format!("{} runs into a {} of {}", num(s), st.kind.name(), st.of));
            }
        }
    }

    // maintenance zones, walkway, walls
    let headroom = rule("plant.access.headroom");
    let walk = lay.zones.iter().find(|z| z.kind == "walkway");
    for z in &lay.zones {
        for e in t.eqs.iter().filter(|e| e.tag != z.tag && e.kind != EqKind::Sump) {
            if e.body().into_iter().any(|b| overlap(b, (z.lo, z.hi))) {
                bad("zone", format!("{} stands in the {} zone {}", e.tag, z.kind, z.tag));
            }
        }
        if z.kind == "front" || z.kind == "walkway" {
            for s in p.segs.iter().filter(|s| s.a.z.min(s.b.z) < headroom && s.a.z.max(s.b.z) > 0.0) {
                let own = t.lines[s.line].all_ends().iter().any(|x| t.eqs[x.eq].tag == z.tag);
                if !own && seg_box(s.a, s.b, z.lo, z.hi) < s.env - TOL {
                    bad("zone", format!("{} runs low through the {} zone {}", num(s), z.kind, z.tag));
                }
            }
        }
    }
    if let Some(w) = walk {
        for pf in &lay.platforms {
            if overlap((pf.lo, pf.hi), (w.lo, w.hi))
                || std::iter::once(&pf.access).chain(&pf.extra).any(|a| overlap((a.lo, a.hi), (w.lo, w.hi)))
            {
                bad("zone", format!("{} or its {} stands in the walkway", pf.tag, pf.access.kind));
            }
        }
    }
    // nothing but the tanks stands inside their bund, and the posts of the rack clear the units
    if let Some(b) = &lay.bund {
        let inside = (v3(b.lo.x, b.lo.y, -1.0), v3(b.hi.x, b.hi.y, 100.0));
        for e in t.eqs.iter().filter(|e| e.kind.in_row() && !b.tanks.contains(&e.tag)) {
            if e.body().into_iter().any(|x| overlap(x, inside)) {
                bad("bund", format!("{} stands inside the bund of {}", e.tag, b.tanks.join(", ")));
            }
        }
        if let Some(w) = walk {
            if overlap(inside, (w.lo, w.hi)) {
                bad("bund", "the bund wall crosses the walkway".to_string());
            }
        }
    }
    for st in lay.steel.iter().filter(|s| s.of == "rack" && s.kind == super::SteelKind::Post) {
        for e in t.eqs.iter().filter(|e| e.kind.in_row()) {
            if e.body().into_iter().any(|(lo, hi)| seg_box(st.a, st.b, lo, hi) < STEEL_R - TOL) {
                bad("pipe_steel", format!("a rack post stands in {}", e.tag));
            }
        }
    }
    let wall = rule("plant.clear.wall");
    let h = &lay.hall;
    for e in t.eqs.iter().filter(|e| e.kind.in_row()) {
        for (lo, hi) in e.body() {
            let d = (lo.x - h.x0).min(h.x1 - hi.x).min(lo.y - h.y0).min(h.y1 - hi.y);
            if d < wall - TOL {
                bad("wall", format!("{} is {:.2} m from a wall", e.tag, d));
                break;
            }
        }
    }

    // valves within reach
    let ops: Vec<&Placed> = p.items.iter().filter(|i| i.item.operated()).collect();
    let mut in_reach = 0;
    for i in &ops {
        if access_of(i, lay).is_some() {
            in_reach += 1;
        } else {
            bad("reach", format!("{} at {:.2} m on {} is out of reach", i.tag, i.op.z, t.lines[i.line].number()));
        }
    }

    // drains fall
    let slope = rule("plant.pipe.drain_slope") / 100.0;
    for s in p.segs.iter().filter(|s| t.lines[s.line].svc == Svc::Dr && s.stub.is_none() && !s.vertical()) {
        let d = s.b - s.a;
        let plan = (d.x * d.x + d.y * d.y).sqrt();
        if d.z.abs() < slope * plan - TOL {
            bad("drain", format!("{} falls {:.2} % only", num(s), d.z.abs() / plan * 100.0));
        }
    }

    Report {
        ok: issues.is_empty(),
        issues,
        nozzles,
        connected,
        pipe_pairs: pairs,
        min_margin: if min_margin.is_finite() { min_margin } else { 0.0 },
        valves: ops.len(),
        valves_in_reach: in_reach,
    }
}
