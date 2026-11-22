// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Orthogonal piping between the nozzles: each line a tree from its root nozzle, routed over the
//! rack tiers around the units, the walkway, the platforms and the other lines; the drain under
//! the floor with its fall to the sump. Then the pieces sized by the flow they carry, the
//! elbows, tees, reducers and flanges, the valves and instruments on the stubs, and the supports
//! at the rack bays, hangers, stands, guides and trench supports.

use std::collections::BTreeMap;

use ddc_geom::math::{v2, v3, V3};
use ddc_geom::mep::route3d::{self as r3, Index, Obstacle, Pass, Routed, RoutedIndex, Rules, Source, Terminal};

use crate::project::rules::rule;

use super::equip;
use super::layout::{Lay, STEEL_R, WALL};
use super::size::{self, ItemKind};
use super::topo::Topo;
use super::{End, EqKind, Equip, Line, Spot, Svc};

/// The router leaves a port along its direction by this share of the tangent before a turn.
const ESCAPE: f64 = 1.05;
/// Shortest straight before a tee as a share of the tangent: a tee never lands on a stub.
const TEE_K: f64 = 0.55;
/// Horizontal metres of a drain off its layer cost this many times more.
const DRAIN_OFF_ZONE: f64 = 20.0;
/// A height above anything in the hall: tops of flues and rods while routing.
const FAR: f64 = 100.0;
/// Shifts tried for a trapeze or a hanger that would clash, as shares of its bay or spacing.
const SHIFT: f64 = 0.05;
const SHIFTS: usize = 8;
/// Points closer than this are one point, m.
const EPS: f64 = 1e-6;
const ON: f64 = 1e-4;

/// Envelope radius of a piece of `dn` on line `l`: the pipe with its insulation, or a flange.
pub(super) fn env_dn(l: &Line, dn: u32) -> f64 {
    let p = size::pipe(l.spec, dn);
    (p.od / 2.0 + l.insul).max(size::flange_od(dn, l.pn) / 2.0)
}

pub(super) fn env_of(l: &Line) -> f64 {
    env_dn(l, l.dn)
}

/// Routing sizes of a line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Geo {
    pub env: f64,
    /// Envelope plus half the gap: two lines keep the sum of theirs.
    pub clear: f64,
    pub bend: f64,
    /// Straight from a nozzle face before the first elbow: the stack of valves and instruments,
    /// and the elbow clear of the unit.
    pub tangent: f64,
    pub min_leg: f64,
}

impl Geo {
    /// Length of the stub from a nozzle face to where the route may turn.
    pub fn reach(&self) -> f64 {
        ESCAPE * self.tangent
    }
}

pub(super) fn geo(l: &Line, eqs: &[Equip]) -> Geo {
    let env = env_of(l);
    let clear = env + rule("plant.pipe.gap") / 2.0;
    let bend = size::bend(l.spec, l.dn);
    let stack = l
        .all_ends()
        .iter()
        .map(|e| {
            let n = &eqs[e.eq].nozzles[e.nz];
            size::stack_len(n.dn, n.pipe_dn, &n.items, l.spec, l.pn)
        })
        .fold(0.0, f64::max);
    let tangent = (stack + bend).max(clear + bend);
    // the stack sits on the port's own straight; any other leg needs room for its two elbows
    Geo { env, clear, bend, tangent, min_leg: 2.0 * bend.max(clear) }
}

/// A straight piece of pipe.
#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub line: usize,
    pub a: V3,
    pub b: V3,
    pub dn: u32,
    pub env: f64,
    /// The nozzle whose stub this is: valves and instruments sit on it.
    pub stub: Option<End>,
}

impl Seg {
    pub fn len(&self) -> f64 {
        (self.b - self.a).len()
    }
    pub fn vertical(&self) -> bool {
        let d = self.b - self.a;
        d.z.abs() > d.x.abs().max(d.y.abs())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FitKind {
    Elbow,
    Tee,
    Reducer,
    EccReducer,
    Flange,
}

impl FitKind {
    pub const ALL: [FitKind; 5] = [FitKind::Elbow, FitKind::Tee, FitKind::Reducer, FitKind::EccReducer, FitKind::Flange];
    pub fn name(self) -> &'static str {
        match self {
            FitKind::Elbow => "elbow 90",
            FitKind::Tee => "tee",
            FitKind::Reducer => "concentric reducer",
            FitKind::EccReducer => "eccentric reducer",
            FitKind::Flange => "flange",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fitting {
    pub kind: FitKind,
    pub line: usize,
    pub at: V3,
    pub dn: u32,
    /// Branch or small end of a tee or reducer; `dn` otherwise.
    pub dn2: u32,
}

/// A valve or instrument on a stub.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub tag: String,
    pub item: ItemKind,
    pub line: usize,
    pub end: End,
    pub at: V3,
    pub dir: V3,
    pub dn: u32,
    /// Where it is worked from.
    pub op: V3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SupportKind {
    Trapeze,
    Hanger,
    Stand,
    Guide,
    Trench,
}

impl SupportKind {
    pub const ALL: [SupportKind; 5] = [SupportKind::Trapeze, SupportKind::Hanger, SupportKind::Stand, SupportKind::Guide, SupportKind::Trench];
    pub fn name(self) -> &'static str {
        match self {
            SupportKind::Trapeze => "rack trapeze",
            SupportKind::Hanger => "rod hanger",
            SupportKind::Stand => "pipe stand",
            SupportKind::Guide => "riser guide",
            SupportKind::Trench => "trench support",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Support {
    pub kind: SupportKind,
    pub line: usize,
    pub at: V3,
    pub dn: u32,
    pub env: f64,
}

/// A rack trapeze: beams across the rack at one bay, hung from the roof on two rods.
#[derive(Clone, Debug, PartialEq)]
pub struct Trapeze {
    pub x: f64,
    pub y0: f64,
    pub y1: f64,
    pub beams: Vec<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Piping {
    pub segs: Vec<Seg>,
    pub fittings: Vec<Fitting>,
    pub items: Vec<Placed>,
    pub supports: Vec<Support>,
    pub trapezes: Vec<Trapeze>,
    /// Ends no route reached.
    pub unrouted: Vec<End>,
    pub tiers: Vec<f64>,
    pub bay: f64,
    /// Fall of the drain from its far end to the sump, m.
    pub drain_fall: f64,
}

/// One branch of a routed line: points from its end's face to the join.
#[derive(Clone, Debug)]
struct RBranch {
    end: End,
    points: Vec<V3>,
    parent: Option<usize>,
    join: V3,
}

#[derive(Clone, Debug, Default)]
struct Raw {
    lines: BTreeMap<usize, Vec<RBranch>>,
    unrouted: Vec<End>,
    tiers: Vec<f64>,
    fall: f64,
}

pub(super) fn route(t: &mut Topo, lay: &Lay) -> Piping {
    let max = (rule("plant.route.max_tiers") as usize).max(1);
    let mut best: Option<Raw> = None;
    for n in 1..=max {
        let raw = route_all(t, lay, &lay.tiers(n));
        let done = raw.unrouted.is_empty();
        if best.as_ref().is_none_or(|b| raw.unrouted.len() < b.unrouted.len()) {
            best = Some(raw);
        }
        if done {
            break;
        }
    }
    let raw = best.unwrap_or_default();
    if let Some(i) = t.eqs.iter().position(|e| e.kind == EqKind::Sump) {
        t.eqs[i].dims.l = raw.fall;
        equip::shape(&mut t.eqs, i, &t.lines);
    }
    build(t, lay, raw)
}

fn face(eqs: &[Equip], e: End) -> (V3, V3) {
    eqs[e.eq].nozzle_world(e.nz)
}

/// Everything every line keeps clear of, and the front zones by unit.
fn obstacles(t: &Topo, lay: &Lay, tiers: &[f64]) -> (Vec<Obstacle>, Vec<(usize, Obstacle)>) {
    let mut out = vec![];
    for (i, e) in t.eqs.iter().enumerate() {
        for (lo, hi) in e.body() {
            out.push(Obstacle::aabb(lo, hi, Pass::Block, i as i64));
        }
    }
    let h = &lay.hall;
    let walls = [
        (v3(h.x0 - WALL, h.y0 - WALL, -FAR), v3(h.x0, h.y1 + WALL, FAR)),
        (v3(h.x1, h.y0 - WALL, -FAR), v3(h.x1 + WALL, h.y1 + WALL, FAR)),
        (v3(h.x0, h.y0 - WALL, -FAR), v3(h.x1, h.y0, FAR)),
        (v3(h.x0, h.y1, -FAR), v3(h.x1, h.y1 + WALL, FAR)),
    ];
    if !t.outdoor {
        for (lo, hi) in walls {
            out.push(Obstacle::aabb(lo, hi, Pass::Block, -1));
        }
    }
    // the posts and beams of an outdoor rack stand before any pipe
    if let Some(r) = &lay.rack {
        let beams = r.beams(tiers);
        let top = beams.iter().copied().fold(0.0, f64::max);
        for &x in &r.xs {
            for y in [r.y0, r.y1] {
                out.push(Obstacle::aabb(v3(x - STEEL_R, y - STEEL_R, 0.0), v3(x + STEEL_R, y + STEEL_R, top + STEEL_R), Pass::Block, -6));
            }
            for &zb in &beams {
                out.push(Obstacle::aabb(v3(x - STEEL_R, r.y0, zb - STEEL_R), v3(x + STEEL_R, r.y1, zb + STEEL_R), Pass::Block, -6));
            }
        }
        if let (Some(&x0), Some(&x1)) = (r.xs.first(), r.xs.last()) {
            for y in [r.y0, r.y1] {
                out.push(Obstacle::aabb(v3(x0, y - STEEL_R, top - STEEL_R), v3(x1, y + STEEL_R, top + STEEL_R), Pass::Block, -6));
            }
        }
    }
    for z in lay.zones.iter().filter(|z| z.kind == "walkway") {
        out.push(Obstacle::aabb(z.lo, z.hi, Pass::Block, -2));
    }
    // platforms and access with their steel on the envelope
    let pad = v3(STEEL_R, STEEL_R, STEEL_R);
    for p in &lay.platforms {
        out.push(Obstacle::aabb(v3(p.lo.x, p.lo.y, 0.0) - pad, p.hi + pad, Pass::Block, -3));
        for a in std::iter::once(&p.access).chain(&p.extra) {
            out.push(Obstacle::aabb(a.lo - pad, a.hi + pad, Pass::Block, -3));
        }
    }
    for f in &lay.flues {
        let r = v3(f.d / 2.0, f.d / 2.0, 0.0);
        out.push(Obstacle::aabb(f.at - r, f.at + r + v3(0.0, 0.0, FAR), Pass::Block, -4));
    }
    let tag: BTreeMap<&str, usize> = t.eqs.iter().enumerate().map(|(i, e)| (e.tag.as_str(), i)).collect();
    let fronts = lay
        .zones
        .iter()
        .filter(|z| z.kind == "front")
        .filter_map(|z| tag.get(z.tag.as_str()).map(|&i| (i, Obstacle::aabb(z.lo, z.hi, Pass::Block, -5))))
        .collect();
    (out, fronts)
}

fn route_all(t: &Topo, lay: &Lay, tiers: &[f64]) -> Raw {
    let (base, fronts) = obstacles(t, lay, tiers);
    let h = &lay.hall;
    let bounds = (v2(h.x0, h.y0), v2(h.x1, h.y1));
    let geos: Vec<Geo> = t.lines.iter().map(|l| geo(l, &t.eqs)).collect();
    let mut order: Vec<usize> = (0..t.lines.len()).collect();
    order.sort_by_key(|&i| (t.lines[i].svc == Svc::Dr, std::cmp::Reverse(t.lines[i].dn), i));
    let mut raw = Raw { tiers: tiers.to_vec(), ..Raw::default() };
    let mut done: Vec<Routed> = vec![];
    let walk_mid = (h.walk.0 + h.walk.1) / 2.0;
    for (k, &li) in order.iter().enumerate() {
        let l = &t.lines[li];
        let g = geos[li];
        let ends = l.all_ends();
        let mut obs = base.clone();
        obs.extend(fronts.iter().filter(|(i, _)| !ends.iter().any(|e| e.eq == *i)).map(|(_, o)| o.clone()));
        let ix = Index::new(obs);
        let mut others = done.clone();
        for &oj in &order[k + 1..] {
            let ol = &t.lines[oj];
            for e in ol.all_ends() {
                let (a, d) = face(&t.eqs, e);
                others.push(Routed { a, b: a + d * geos[oj].reach(), clearance: geos[oj].clear });
            }
        }
        let drain = l.svc == Svc::Dr;
        // a pump's suction stays low at its own level: short, level, no pocket over the rack
        let mut suction: Vec<f64> =
            ends.iter().filter(|e| t.eqs[e.eq].nozzles[e.nz].spot == Spot::Suction).map(|&e| face(&t.eqs, e).0.z).collect();
        suction.sort_by(f64::total_cmp);
        suction.dedup_by(|a, b| (*a - *b).abs() < ON);
        let layers = if drain {
            vec![h.drain_z]
        } else if t.outdoor && !suction.is_empty() {
            suction
        } else {
            tiers.to_vec()
        };
        let mut rules = Rules::new(g.clear, g.min_leg, layers.clone(), bounds);
        rules.step = rule("plant.route.step");
        rules.tangent = g.tangent;
        rules.tee_leg = (TEE_K * g.tangent).min(g.min_leg);
        rules.bend_cost = rule("plant.route.bend_cost");
        rules.axes = (vec![], vec![walk_mid]);
        rules.axis_gain = rule("plant.rack.lane_gain");
        if drain {
            rules.off_zone = DRAIN_OFF_ZONE;
        }
        let (sa, sd) = face(&t.eqs, l.root);
        let src = Source::Terminal(Terminal { at: sa, dir: Some(sd) });
        let targets: Vec<Terminal> = l
            .ends
            .iter()
            .map(|&e| {
                let (a, d) = face(&t.eqs, e);
                Terminal { at: a, dir: Some(d) }
            })
            .collect();
        let mut ri = RoutedIndex::new(others);
        let mut tree = r3::route_tree(&ix, &rules, &src, &targets, &mut ri);
        if drain {
            let zd = h.drain_z;
            let on: Vec<Vec<bool>> = tree.branches.iter().map(|b| b.points.iter().map(|p| (p.z - zd).abs() < ON).collect()).collect();
            let join_on: Vec<bool> = tree.branches.iter().map(|b| (b.join.z - zd).abs() < ON).collect();
            r3::apply_fall(&mut tree, &layers, rule("plant.pipe.drain_slope"));
            let mut fall: f64 = 0.0;
            for (b, on) in tree.branches.iter().zip(&on) {
                for (p, o) in b.points.iter().zip(on) {
                    if *o {
                        fall = fall.max(p.z - zd);
                    }
                }
            }
            for (bi, b) in tree.branches.iter_mut().enumerate() {
                for (p, o) in b.points.iter_mut().zip(&on[bi]) {
                    if *o {
                        p.z -= fall;
                    }
                }
                if join_on[bi] {
                    b.join.z -= fall;
                }
            }
            raw.fall = fall;
        }
        for &f in &tree.failed {
            raw.unrouted.push(l.ends[f]);
        }
        let mut branches = vec![];
        for b in &tree.branches {
            for w in b.points.windows(2) {
                done.push(Routed { a: w[0], b: w[1], clearance: g.clear });
            }
            branches.push(RBranch { end: l.ends[b.target], points: b.points.clone(), parent: b.parent, join: b.join });
        }
        if tree.branches.is_empty() && !l.ends.is_empty() && tree.failed.is_empty() {
            raw.unrouted.extend(l.ends.iter().copied());
        }
        raw.lines.insert(li, branches);
    }
    raw.unrouted.sort();
    raw
}

/// Steepest drift off a stub's direction still taken as the same straight: a drain's fall tilts it.
const STUB_SLOPE: f64 = 0.1;

/// Splits `p` (from a face along `dir`) so the stub to `reach` is its own segment.
fn split_stub(a: V3, b: V3, dir: V3, reach: f64) -> Vec<(V3, V3)> {
    let d = b - a;
    let along = d.dot(dir);
    if along > reach + EPS && (d - dir * along).len() <= STUB_SLOPE * along + EPS {
        let m = a + d * (reach / along);
        vec![(a, m), (m, b)]
    } else {
        vec![(a, b)]
    }
}

/// Distance along a polyline segment `a..b` to `p` if `p` lies on it.
fn on_seg(p: V3, a: V3, b: V3) -> Option<f64> {
    (r3::seg_seg_dist(p, p, a, b) < ON).then(|| (p - a).len())
}

fn span(dn: u32) -> f64 {
    (rule("plant.support.span") * (dn as f64 / 25.0).sqrt()).min(rule("plant.support.span_max"))
}

fn build(t: &Topo, lay: &Lay, raw: Raw) -> Piping {
    let mut p = Piping { tiers: raw.tiers.clone(), drain_fall: raw.fall, unrouted: raw.unrouted.clone(), ..Piping::default() };
    for (&li, branches) in &raw.lines {
        let l = &t.lines[li];
        let g = geo(l, &t.eqs);
        let v = rule(l.v_rule);
        let drain_dn = size::pipe(l.spec, rule("plant.pipe.drain_dn") as u32).dn;
        let size_of = |q: f64| {
            let dn = size::dn_for(l.spec, q.min(l.flow), v);
            if l.svc == Svc::Dr {
                dn.max(drain_dn)
            } else {
                dn
            }
        };
        let flow_of = |e: End| t.eqs[e.eq].nozzles[e.nz].flow;
        let n = branches.len();
        let mut total: Vec<f64> = branches.iter().map(|b| flow_of(b.end)).collect();
        for i in (0..n).rev() {
            if let Some(par) = branches[i].parent {
                if par < i {
                    total[par] += total[i];
                }
            }
        }
        let (root_face, root_dir) = face(&t.eqs, l.root);
        for (bi, b) in branches.iter().enumerate() {
            let pts = &b.points;
            if pts.len() < 2 {
                continue;
            }
            let children: Vec<usize> = (0..n).filter(|&c| branches[c].parent == Some(bi)).collect();
            let mut flow = flow_of(b.end);
            let mut dn_prev = t.eqs[b.end.eq].nozzles[b.end.nz].pipe_dn;
            let last = pts.len() - 2;
            for (k, w) in pts.windows(2).enumerate() {
                let (a, bb) = (w[0], w[1]);
                // the stub at the end's face, and at the root for a branch ending on the source
                let mut parts: Vec<(V3, V3, Option<End>)> = vec![];
                let (ef, ed) = face(&t.eqs, b.end);
                if k == 0 && (a - ef).len() < EPS {
                    for (j, (x, y)) in split_stub(a, bb, ed, g.reach()).into_iter().enumerate() {
                        parts.push((x, y, (j == 0).then_some(b.end)));
                    }
                } else if k == last && b.parent.is_none() && (bb - root_face).len() < EPS {
                    // split from the root face outward, then walked toward it
                    for (j, (x, y)) in split_stub(bb, a, root_dir, g.reach()).into_iter().enumerate().rev() {
                        parts.push((y, x, (j == 0).then_some(l.root)));
                    }
                } else {
                    parts.push((a, bb, None));
                }
                for (x, y, stub) in parts {
                    // joins of children on this part, nearest first
                    let mut js: Vec<(f64, usize)> = children
                        .iter()
                        .filter_map(|&c| on_seg(branches[c].join, x, y).map(|s| (s, c)))
                        .filter(|(s, _)| *s > EPS)
                        .collect();
                    js.sort_by(|u, w| u.0.total_cmp(&w.0).then(u.1.cmp(&w.1)));
                    let mut from = x;
                    for (s, c) in js.into_iter().chain(std::iter::once((f64::INFINITY, usize::MAX))) {
                        let to = if c == usize::MAX { y } else { x + (y - x).normalized() * s };
                        let dn = match stub {
                            Some(e) => t.eqs[e.eq].nozzles[e.nz].pipe_dn,
                            None => size_of(flow),
                        };
                        if (to - from).len() > EPS {
                            if dn != dn_prev {
                                p.fittings.push(Fitting { kind: FitKind::Reducer, line: li, at: from, dn: dn.max(dn_prev), dn2: dn.min(dn_prev) });
                            }
                            p.segs.push(Seg { line: li, a: from, b: to, dn, env: env_dn(l, dn), stub });
                            dn_prev = dn;
                        }
                        if c != usize::MAX {
                            let cdn = size_of(total[c]);
                            flow += total[c];
                            let main = dn_prev.max(size_of(flow));
                            p.fittings.push(Fitting { kind: FitKind::Tee, line: li, at: to, dn: main, dn2: cdn.min(main) });
                        }
                        from = to;
                    }
                }
                // an elbow where this segment turns into the next
                if k + 1 < pts.len() - 1 {
                    let d1 = (pts[k + 1] - pts[k]).normalized();
                    let d2 = (pts[k + 2] - pts[k + 1]).normalized();
                    if d1.cross(d2).len() > EPS {
                        p.fittings.push(Fitting { kind: FitKind::Elbow, line: li, at: pts[k + 1], dn: dn_prev, dn2: dn_prev });
                    }
                }
            }
        }
        // what sits on each nozzle: flange, reducer, valves and instruments
        for e in l.all_ends() {
            let nz = &t.eqs[e.eq].nozzles[e.nz];
            let (fa, fd) = face(&t.eqs, e);
            p.fittings.push(Fitting { kind: FitKind::Flange, line: li, at: fa, dn: nz.dn, dn2: nz.dn });
            let fl = size::flange_od(nz.pipe_dn, l.pn);
            for s in size::stack(nz.dn, nz.pipe_dn, &nz.items, l.spec, l.pn) {
                let at = fa + fd * (s.at + s.len / 2.0);
                match s.item {
                    None => {
                        let kind = if nz.spot == Spot::Suction { FitKind::EccReducer } else { FitKind::Reducer };
                        p.fittings.push(Fitting { kind, line: li, at, dn: nz.pipe_dn, dn2: nz.dn });
                    }
                    Some(k) => {
                        for f in 0..k.flanges() {
                            let off = if f == 0 { s.at } else { s.at + s.len };
                            p.fittings.push(Fitting { kind: FitKind::Flange, line: li, at: fa + fd * off, dn: nz.pipe_dn, dn2: nz.pipe_dn });
                        }
                        let n_same = p.items.iter().filter(|q| q.item.code() == k.code()).count() + 1;
                        p.items.push(Placed {
                            tag: format!("{}-{:03}", k.code(), n_same),
                            item: k,
                            line: li,
                            end: e,
                            at,
                            dir: fd,
                            dn: nz.pipe_dn,
                            op: at + v3(0.0, 0.0, equip::op_rise(k, fd, fl)),
                        });
                    }
                }
            }
        }
    }
    supports(&mut p, lay);
    p
}

/// A vertical steel member from `z0` up through `x, y` keeps clear of every pipe but those of
/// `skip`.
fn rod_clear(segs: &[Seg], x: f64, y: f64, z0: f64, z1: f64, skip: Option<usize>) -> bool {
    let (a, b) = (v3(x, y, z0), v3(x, y, z1));
    segs.iter().filter(|s| Some(s.line) != skip).all(|s| r3::seg_seg_dist(a, b, s.a, s.b) >= s.env + STEEL_R - EPS)
}

fn supports(p: &mut Piping, lay: &Lay) {
    let headroom = rule("plant.access.headroom");
    let tiers = p.tiers.clone();
    let on_tier = |z: f64| tiers.iter().any(|t| (t - z).abs() < ON);
    let frame = lay.rack.as_ref();
    let within = |y: f64| frame.is_none_or(|r| y > r.y0 + EPS && y < r.y1 - EPS);
    let rack = |s: &Seg| !s.vertical() && on_tier(s.a.z) && (s.b - s.a).x.abs() > (s.b - s.a).y.abs() && s.stub.is_none() && within(s.a.y);
    let bay = p.segs.iter().filter(|s| rack(s)).map(|s| span(s.dn)).fold(rule("plant.rack.bay"), f64::min);
    p.bay = bay;
    let (x0, x1) = (lay.hall.x0, lay.hall.x1);
    let n_bays = if frame.is_some() { 0 } else { ((x1 - x0) / bay).floor() as usize };
    let segs = p.segs.clone();
    // outdoors: a portal at every bay of the fixed frame carries the pipes that cross it
    if let Some(r) = frame {
        p.bay = r.xs.windows(2).map(|w| w[1] - w[0]).fold(rule("plant.rack.bay"), f64::min);
        for &x in &r.xs {
            for s in segs.iter().filter(|s| rack(s) && s.a.x.min(s.b.x) < x - EPS && s.a.x.max(s.b.x) > x + EPS) {
                p.supports.push(Support { kind: SupportKind::Trapeze, line: s.line, at: v3(x, s.a.y, s.a.z), dn: s.dn, env: s.env });
            }
            p.trapezes.push(Trapeze { x, y0: r.y0, y1: r.y1, beams: r.beams(&tiers) });
        }
    }
    // trapezes: each bay line, shifted off any riser its rods or beams would hit
    for k in 0..n_bays {
        let base = x0 + bay * (k as f64 + 0.5);
        let mut chosen = None;
        for j in 0..=SHIFTS {
            let sh = bay * SHIFT * j.div_ceil(2) as f64 * if j % 2 == 0 { 1.0 } else { -1.0 };
            let x = base + sh;
            let on: Vec<&Seg> = segs.iter().filter(|s| rack(s) && s.a.x.min(s.b.x) < x - EPS && s.a.x.max(s.b.x) > x + EPS).collect();
            if on.is_empty() {
                break;
            }
            let y0 = on.iter().map(|s| s.a.y - s.env).fold(f64::INFINITY, f64::min) - STEEL_R - rule("plant.pipe.gap");
            let y1 = on.iter().map(|s| s.a.y + s.env).fold(f64::NEG_INFINITY, f64::max) + STEEL_R + rule("plant.pipe.gap");
            let mut beams: Vec<f64> = vec![];
            for &tz in &tiers {
                let env = on.iter().filter(|s| (s.a.z - tz).abs() < ON).map(|s| s.env).fold(f64::NEG_INFINITY, f64::max);
                if env.is_finite() {
                    beams.push(tz - env - STEEL_R);
                }
            }
            let low = beams.iter().copied().fold(f64::INFINITY, f64::min);
            let rods = rod_clear(&segs, x, y0, low, FAR, None) && rod_clear(&segs, x, y1, low, FAR, None);
            let beams_ok = beams.iter().all(|&zb| {
                let (a, b) = (v3(x, y0, zb), v3(x, y1, zb));
                segs.iter().all(|s| r3::seg_seg_dist(a, b, s.a, s.b) >= s.env + STEEL_R - EPS)
            });
            if rods && beams_ok {
                chosen = Some((x, y0, y1, beams, on.iter().map(|s| (s.line, s.a.y, s.a.z, s.dn, s.env)).collect::<Vec<_>>()));
                break;
            }
            if chosen.is_none() && j == SHIFTS {
                chosen = Some((x, y0, y1, beams, on.iter().map(|s| (s.line, s.a.y, s.a.z, s.dn, s.env)).collect::<Vec<_>>()));
            }
        }
        if let Some((x, y0, y1, beams, on)) = chosen {
            for (line, y, z, dn, env) in on {
                p.supports.push(Support { kind: SupportKind::Trapeze, line, at: v3(x, y, z), dn, env });
            }
            p.trapezes.push(Trapeze { x, y0, y1, beams });
        }
    }
    // the rest: guides on risers, trench supports under the floor, hangers high, stands low
    for s in &segs {
        if s.stub.is_some() || rack(s) {
            continue;
        }
        let len = s.len();
        let sp = span(s.dn);
        if s.vertical() {
            let m = (len / sp).ceil() as usize;
            for k in 1..m {
                let at = s.a + (s.b - s.a) * (k as f64 / m as f64);
                p.supports.push(Support { kind: SupportKind::Guide, line: s.line, at, dn: s.dn, env: s.env });
            }
            continue;
        }
        let m = ((len / sp).ceil() as usize).max(1);
        let z = s.a.z.max(s.b.z);
        let kind = if z < 0.0 {
            SupportKind::Trench
        } else if z >= headroom && frame.is_none() {
            SupportKind::Hanger
        } else {
            SupportKind::Stand
        };
        for k in 0..m {
            let f0 = (k as f64 + 0.5) / m as f64;
            let mut at = s.a + (s.b - s.a) * f0;
            if kind != SupportKind::Trench {
                for j in 0..=SHIFTS {
                    let f = f0 + SHIFT / m as f64 * j.div_ceil(2) as f64 * if j % 2 == 0 { 1.0 } else { -1.0 };
                    let q = s.a + (s.b - s.a) * f.clamp(0.0, 1.0);
                    let ok = if kind == SupportKind::Hanger {
                        rod_clear(&segs, q.x, q.y, q.z + s.env, FAR, Some(s.line))
                    } else {
                        rod_clear(&segs, q.x, q.y, 0.0, q.z - s.env, Some(s.line))
                    };
                    if ok {
                        at = q;
                        break;
                    }
                }
            }
            p.supports.push(Support { kind, line: s.line, at, dn: s.dn, env: s.env });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stub_on_a_falling_drain_is_split_at_its_reach() {
        // the root straight of a drain after its fall: 1.92 m long, 19 mm lower at the face
        let face = v3(13.8175, 1.35, -0.607);
        let far = v3(11.9009, 1.35, -0.5879);
        let parts = split_stub(face, far, v3(-1.0, 0.0, 0.0), 0.25326);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!((parts[0].0 - face).len() < EPS && (parts[1].1 - far).len() < EPS);
        assert!(((parts[0].1 - face).x.abs() - 0.25326).abs() < 1e-9, "{parts:?}");
        // a level straight splits the same way, a turn does not
        assert_eq!(split_stub(face, v3(11.9, 1.35, -0.607), v3(-1.0, 0.0, 0.0), 0.25326).len(), 2);
        assert_eq!(split_stub(face, v3(13.8175, 3.0, -0.607), v3(-1.0, 0.0, 0.0), 0.25326).len(), 1);
    }
}
