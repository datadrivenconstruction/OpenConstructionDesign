// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The penalty of a built project, broken down by goal rule. Each `goal.*` rule measures one
//! quality on the built storeys, compares it with the rule's value, and costs
//! `weight x miss` in its tier, the miss normalised to 0..=1. Projects compare tier by tier,
//! strongest first, so weak goals never outweigh a stronger one. Every unit of penalty belongs
//! to one rule id.

use super::rules::{self, Tier};
use super::{GProject, PStorey, ProjectSpec, RoomKind};
use crate::{GWall, WallRole};
use serde_json::{json, Value as J};
use std::cmp::Ordering;

type P = (f64, f64);

/// What one goal costs.
#[derive(Clone, Debug, PartialEq)]
pub struct Cost {
    pub rule: String,
    pub tier: Tier,
    pub weight: f64,
    /// The rule's value in force.
    pub target: f64,
    /// What the build reached, in the rule's unit (a share of failing items for counted goals).
    pub measured: f64,
    /// 0 = met, 1 = missed completely.
    pub miss: f64,
    /// `weight x miss`.
    pub cost: f64,
    /// Items counted and failed, for counted goals.
    pub failed: usize,
    pub of: usize,
}

impl Cost {
    pub fn to_json(&self) -> J {
        let r = |x: f64| (x * 1000.0).round() / 1000.0;
        json!({"rule": self.rule, "tier": self.tier.name(), "weight": self.weight,
               "target": self.target, "measured": r(self.measured), "miss": r(self.miss),
               "cost": r(self.cost), "failed": self.failed, "of": self.of})
    }
}

/// Every goal's cost.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Score {
    pub costs: Vec<Cost>,
}

const EPS: f64 = 1e-9;

impl Score {
    /// Penalty per tier, strongest first.
    pub fn tiers(&self) -> [f64; 4] {
        let mut t = [0.0; 4];
        for c in &self.costs {
            t[c.tier.rank()] += c.cost;
        }
        t
    }

    /// The whole penalty: the sum of every rule's cost.
    pub fn total(&self) -> f64 {
        self.costs.iter().map(|c| c.cost).sum()
    }

    /// Goals of the hard tier that are missed.
    pub fn hard_misses(&self) -> Vec<&Cost> {
        self.costs.iter().filter(|c| c.tier == Tier::Hard && c.miss > EPS).collect()
    }

    /// The goals that cost something, dearest first within the strongest tier first.
    pub fn limiting(&self) -> Vec<&Cost> {
        let mut v: Vec<&Cost> = self.costs.iter().filter(|c| c.cost > EPS).collect();
        v.sort_by(|a, b| a.tier.cmp(&b.tier).then(b.cost.total_cmp(&a.cost)).then(a.rule.cmp(&b.rule)));
        v
    }

    pub fn cost_of(&self, rule: &str) -> f64 {
        self.costs.iter().filter(|c| c.rule == rule).map(|c| c.cost).sum()
    }

    /// Better is `Less`: the first tier that differs decides.
    pub fn compare(&self, other: &Score) -> Ordering {
        let (a, b) = (self.tiers(), other.tiers());
        for k in 0..4 {
            if (a[k] - b[k]).abs() > EPS {
                return a[k].total_cmp(&b[k]);
            }
        }
        Ordering::Equal
    }

    /// Per rule, what `other` costs more (positive) or less than this one; rules that cost the
    /// same are left out. "Why not B" reads this.
    pub fn contrast(&self, other: &Score) -> Vec<(String, Tier, f64)> {
        let mut out = vec![];
        for c in &other.costs {
            let d = c.cost - self.cost_of(&c.rule);
            if d.abs() > EPS {
                out.push((c.rule.clone(), c.tier, d));
            }
        }
        for c in &self.costs {
            if !other.costs.iter().any(|o| o.rule == c.rule) && c.cost > EPS {
                out.push((c.rule.clone(), c.tier, -c.cost));
            }
        }
        out.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.abs().total_cmp(&a.2.abs())));
        out
    }

    pub fn to_json(&self) -> J {
        let t = self.tiers();
        let r = |x: f64| (x * 1000.0).round() / 1000.0;
        json!({
            "penalty": r(self.total()),
            "tiers": {"hard": r(t[0]), "strong": r(t[1]), "medium": r(t[2]), "weak": r(t[3])},
            "goals": self.limiting().iter().map(|c| c.to_json()).collect::<Vec<_>>(),
        })
    }
}

fn area(poly: &[P]) -> f64 {
    let n = poly.len();
    let s: f64 = (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    s.abs() / 2.0
}

fn dist_seg(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

fn edges(poly: &[P]) -> impl Iterator<Item = (P, P)> + '_ {
    (0..poly.len()).map(move |i| (poly[i], poly[(i + 1) % poly.len()]))
}

fn dist_poly(p: P, poly: &[P]) -> f64 {
    edges(poly).map(|(a, b)| dist_seg(p, a, b)).fold(f64::MAX, f64::min)
}

/// Gap between two outlines: 0 when they touch.
fn gap(a: &[P], b: &[P]) -> f64 {
    let x = a.iter().map(|p| dist_poly(*p, b)).fold(f64::MAX, f64::min);
    let y = b.iter().map(|p| dist_poly(*p, a)).fold(f64::MAX, f64::min);
    x.min(y)
}

fn centroid(poly: &[P]) -> P {
    let n = poly.len().max(1) as f64;
    (poly.iter().map(|p| p.0).sum::<f64>() / n, poly.iter().map(|p| p.1).sum::<f64>() / n)
}

/// Short and long side of the tightest box along one of the outline's edges.
fn sides(poly: &[P]) -> (f64, f64) {
    let mut best = (0.0, 0.0, f64::MAX);
    for (a, b) in edges(poly) {
        let l = (b.0 - a.0).hypot(b.1 - a.1);
        if l < 1e-6 {
            continue;
        }
        let (ux, uy) = ((b.0 - a.0) / l, (b.1 - a.1) / l);
        let span = |f: &dyn Fn(P) -> f64| {
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for p in poly {
                lo = lo.min(f(*p));
                hi = hi.max(f(*p));
            }
            hi - lo
        };
        let (e1, e2) = (span(&|p| p.0 * ux + p.1 * uy), span(&|p| p.1 * ux - p.0 * uy));
        if e1 * e2 < best.2 {
            best = (e1.min(e2), e1.max(e2), e1 * e2);
        }
    }
    (best.0, best.1)
}

fn opening_centre(w: &GWall, along: f64, width: f64) -> P {
    let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
    let l = dx.hypot(dy).max(1e-9);
    let d = along + width / 2.0;
    (w.a.0 + dx / l * d, w.a.1 + dy / l * d)
}

/// Rooms of a storey: kind, the room record and its outline.
fn rooms(st: &PStorey) -> impl Iterator<Item = (RoomKind, &super::PRoom, &[P])> + '_ {
    st.layout.rooms.iter().zip(&st.rooms).map(|(g, r)| (r.kind, r, g.polygon.as_slice()))
}

fn share(failed: usize, of: usize) -> f64 {
    if of == 0 {
        0.0
    } else {
        failed as f64 / of as f64
    }
}

struct Ctx<'a> {
    spec: &'a ProjectSpec,
    p: &'a GProject,
}

/// One goal's measure: (measured, miss, failed, of) for the rule's value `t`.
type Measure = fn(&Ctx<'_>, f64) -> (f64, f64, usize, usize);

fn counted(failed: usize, of: usize) -> (f64, f64, usize, usize) {
    let s = share(failed, of);
    (s, s, failed, of)
}

fn area_fit(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (mut failed, mut of) = (0, 0);
    for st in &c.p.storeys {
        for (_, r, poly) in rooms(st) {
            if let Some(asked) = r.asked.filter(|a| *a > 0.0) {
                of += 1;
                let a = area(poly);
                // a room that took spare length is allowed to be larger
                let over = if r.grown { 0.0 } else { (a - asked).max(0.0) };
                if (asked - a).max(over) > t * asked + 1e-6 {
                    failed += 1;
                }
            }
        }
    }
    counted(failed, of)
}

fn daylight(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (mut failed, mut of) = (0, 0);
    for st in &c.p.storeys {
        for (k, _, poly) in rooms(st) {
            if !k.habitable() {
                continue;
            }
            of += 1;
            let glass: f64 = st
                .layout
                .openings
                .iter()
                .filter(|o| !o.door)
                .filter_map(|o| st.layout.walls.get(o.wall).map(|w| (w, o)))
                .filter(|(w, o)| w.role == WallRole::Exterior && dist_poly(opening_centre(w, o.along, o.width), poly) < 0.3)
                .map(|(_, o)| o.width * o.height)
                .sum();
            if glass + 1e-6 < t * area(poly) {
                failed += 1;
            }
        }
    }
    counted(failed, of)
}

fn stair_reach(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (mut failed, mut of) = (0, 0);
    for st in &c.p.storeys {
        let stairs: Vec<P> = rooms(st).filter(|x| x.0 == RoomKind::Stair).map(|x| centroid(x.2)).collect();
        if stairs.is_empty() {
            continue;
        }
        for (k, _, poly) in rooms(st) {
            if !k.habitable() {
                continue;
            }
            of += 1;
            let m = centroid(poly);
            if stairs.iter().all(|s| (s.0 - m.0).hypot(s.1 - m.1) > t) {
                failed += 1;
            }
        }
    }
    counted(failed, of)
}

fn adjacency(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let pg = &c.spec.program;
    let (mut failed, mut of) = (0.0, 0.0);
    let (mut nf, mut n) = (0, 0);
    for st in &c.p.storeys {
        let rs: Vec<(RoomKind, &[P])> = rooms(st).map(|x| (x.0, x.2)).collect();
        let mut kinds: Vec<RoomKind> = rs.iter().map(|x| x.0).collect();
        kinds.sort_by_key(|k| k.key());
        kinds.dedup();
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i..] {
                let w = pg.adjacency(*a, *b);
                if w <= 0.0 {
                    continue;
                }
                for (i, (_, pa)) in rs.iter().enumerate().filter(|x| (x.1).0 == *a) {
                    let near = rs
                        .iter()
                        .enumerate()
                        .filter(|(j, x)| x.0 == *b && *j != i)
                        .any(|(_, x)| gap(pa, x.1) <= t + 1e-6);
                    of += w;
                    n += 1;
                    if !near {
                        failed += w;
                        nf += 1;
                    }
                }
            }
        }
    }
    let s = if of > 0.0 { failed / of } else { 0.0 };
    (s, s, nf, n)
}

/// Whether a room edge lies on an exterior wall.
fn on_facade(st: &PStorey, a: P, b: P) -> bool {
    let m = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    st.layout.walls.iter().any(|w| w.role == WallRole::Exterior && dist_seg(m, w.a, w.b) < 0.4)
}

fn light_depth(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (mut failed, mut of) = (0, 0);
    for st in &c.p.storeys {
        for (k, _, poly) in rooms(st) {
            if !k.habitable() {
                continue;
            }
            let facade: Vec<(P, P)> = edges(poly).filter(|(a, b)| on_facade(st, *a, *b)).collect();
            if facade.is_empty() {
                continue; // a room without a facade is the daylight goal's miss
            }
            of += 1;
            let deep = poly
                .iter()
                .map(|p| facade.iter().map(|(a, b)| dist_seg(*p, *a, *b)).fold(f64::MAX, f64::min))
                .fold(0.0, f64::max);
            if deep > t + 1e-6 {
                failed += 1;
            }
        }
    }
    counted(failed, of)
}

/// A room people only pass through: circulation, stair or lift.
fn moves(k: RoomKind) -> bool {
    k.circulation() || k == RoomKind::Stair || k == RoomKind::Lift
}

fn circulation_share(c: &Ctx<'_>) -> f64 {
    let (mut circ, mut all) = (0.0, 0.0);
    for st in &c.p.storeys {
        for (k, _, poly) in rooms(st) {
            let a = area(poly);
            all += a;
            if moves(k) {
                circ += a;
            }
        }
    }
    if all > 0.0 {
        circ / all
    } else {
        0.0
    }
}

fn circulation(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let r = circulation_share(c);
    (r, ((r - t) / t).clamp(0.0, 1.0), 0, 0)
}

fn efficiency(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (mut net, mut gross) = (0.0, 0.0);
    for st in &c.p.storeys {
        let holes: f64 = st.layout.slab_holes.iter().map(|h| area(h)).sum();
        gross += (area(&st.layout.slab) - holes).max(0.0);
        // usable area: corridors, stairs and lifts are gross, not net
        net += rooms(st).filter(|x| !moves(x.0)).map(|x| area(x.2)).sum::<f64>();
    }
    let e = if gross > 0.0 { (net / gross).min(1.0) } else { 1.0 };
    (e, ((t - e) / t).clamp(0.0, 1.0), 0, 0)
}

fn wet_stack(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let wet = |st: &PStorey| -> Vec<P> {
        rooms(st).filter(|x| matches!(x.0, RoomKind::Bath | RoomKind::Wc)).map(|x| centroid(x.2)).collect()
    };
    let (mut failed, mut of) = (0, 0);
    for k in 1..c.p.storeys.len() {
        let below = wet(&c.p.storeys[k - 1]);
        for m in wet(&c.p.storeys[k]) {
            of += 1;
            if below.iter().all(|b| (b.0 - m.0).hypot(b.1 - m.1) > t) {
                failed += 1;
            }
        }
    }
    counted(failed, of)
}

fn room_aspect(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (mut failed, mut of) = (0, 0);
    for st in &c.p.storeys {
        for (k, _, poly) in rooms(st) {
            if k.habitable() {
                of += 1;
                let (s, l) = sides(poly);
                if s <= 0.0 || l / s > t + 1e-6 {
                    failed += 1;
                }
            }
        }
    }
    counted(failed, of)
}

fn room_width(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (mut failed, mut of) = (0, 0);
    for st in &c.p.storeys {
        for (k, _, poly) in rooms(st) {
            if k.habitable() {
                of += 1;
                if sides(poly).0 + 1e-6 < t {
                    failed += 1;
                }
            }
        }
    }
    counted(failed, of)
}

fn leftover(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (mut left, mut all) = (0.0, 0.0);
    for st in &c.p.storeys {
        for (k, r, poly) in rooms(st) {
            let a = area(poly);
            all += a;
            if k == RoomKind::Storage && r.asked.is_none() {
                left += a;
            }
        }
    }
    let s = if all > 0.0 { left / all } else { 0.0 };
    (s, ((s - t) / t.max(0.05)).clamp(0.0, 1.0), 0, 0)
}

fn compact(c: &Ctx<'_>, t: f64) -> (f64, f64, usize, usize) {
    let (w, d) = c.p.footprint;
    let r = if w.min(d) > 0.0 { w.max(d) / w.min(d) } else { 1.0 };
    (r, ((r - t) / t).clamp(0.0, 1.0), 0, 0)
}

/// The goals and their measures, in the order of the built-in file.
const GOALS: [(&str, Measure); 12] = [
    ("goal.area_fit", area_fit),
    ("goal.daylight", daylight),
    ("goal.stair_reach", stair_reach),
    ("goal.adjacency", adjacency),
    ("goal.light_depth", light_depth),
    ("goal.circulation", circulation),
    ("goal.efficiency", efficiency),
    ("goal.wet_stack", wet_stack),
    ("goal.room_aspect", room_aspect),
    ("goal.room_width", room_width),
    ("goal.leftover", leftover),
    ("goal.compact", compact),
];

/// The goal ids the scorer measures.
pub fn goal_ids() -> Vec<&'static str> {
    GOALS.iter().map(|g| g.0).collect()
}

/// Every goal's cost for a built project under the spec's rules.
pub fn score(spec: &ProjectSpec, p: &GProject) -> Score {
    let c = Ctx { spec, p };
    let costs = GOALS
        .iter()
        .filter_map(|(id, f)| {
            let d = rules::def(id)?;
            let target = spec.rules.value(id).or(d.default)?;
            let tier = spec.rules.tier(id).unwrap_or(Tier::Medium);
            let weight = spec.rules.weight(id);
            let (measured, miss, failed, of) = f(&c, target);
            let miss = miss.clamp(0.0, 1.0);
            Some(Cost {
                rule: id.to_string(),
                tier,
                weight,
                target,
                measured,
                miss,
                cost: weight * miss,
                failed,
                of,
            })
        })
        .collect();
    Score { costs }
}
