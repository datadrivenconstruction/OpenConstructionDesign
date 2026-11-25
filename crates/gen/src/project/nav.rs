// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan checks as rules: escape distance, daylight depth, accessible routes and doors, fire
//! compartment size, and the vertical skeleton (`stack.rs`). A finding names the rule it
//! breaks, is hard or soft as the rule set says, carries how far past the limit it is for the
//! score (`weight * excess`), and, where a rule change cures it, that change as a [`RuleFix`].
//! One finding per rule, at the storey where it first breaks, so a typical floor repeated ten
//! times is one finding and not ten.
//! Escape distance runs on a door-to-door graph. Rooms are taken as convex: a leg is the
//! straight line between two doors of one room, and a room's distance is from its farthest
//! corner through its best door.

use super::why::RuleFix;
use super::{GProject, PRole, PStorey, ProjectSpec, RoomKind};
use crate::{GLayout, GOpening, WallRole};
use serde_json::json;

type P = (f64, f64);

/// A door centre on a wall centre line lies on the outline of the rooms either side.
const ON_EDGE: f64 = 1e-6;
/// Room numbers a finding lists before it stops.
pub(super) const SHOWN: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub rule: &'static str,
    /// Storey from 0 where the rule first breaks.
    pub storey: Option<usize>,
    /// A hard rule fails the project; a soft one is advice that costs its weight.
    pub hard: bool,
    pub weight: f64,
    /// How far past the limit, as a share of it.
    pub excess: f64,
    pub msg: String,
    pub fix: Option<RuleFix>,
    /// The norm clause the limit comes from, when a country pack set it.
    pub cite: Option<String>,
}

impl Finding {
    /// The line a gate shows: where, the rule, what, and the norm clause behind the limit.
    pub fn line(&self) -> String {
        let at = match self.storey {
            Some(k) => format!("floor {}: {}: {}", k + 1, self.rule, self.msg),
            None => format!("{}: {}", self.rule, self.msg),
        };
        match &self.cite {
            Some(c) => format!("{at} [norm {c}]"),
            None => at,
        }
    }
}

pub(super) fn finding(
    spec: &ProjectSpec,
    rule: &'static str,
    storey: Option<usize>,
    excess: f64,
    msg: String,
    fix: Option<RuleFix>,
) -> Finding {
    Finding {
        rule,
        storey,
        hard: spec.rules.hard(rule),
        weight: spec.rules.weight(rule),
        excess,
        msg,
        fix,
        cite: spec.rules.cite(rule).map(str::to_string),
    }
}

/// Every check of a built project, one finding per broken rule.
pub fn check(spec: &ProjectSpec, p: &GProject) -> Vec<Finding> {
    let mut out = vec![];
    egress(spec, p, &mut out);
    daylight(spec, p, &mut out);
    access(spec, p, &mut out);
    super::fire::check(spec, p, &mut out);
    stairs(spec, p, &mut out);
    super::stack::check(spec, p, &mut out);
    super::assembly::check(spec, p, &mut out);
    out
}

/// Stair flights against the country's least width and highest riser; only a norm sets
/// these limits, so without a pack there is nothing to check.
fn stairs(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    let norm = |id: &str| (spec.rules.source(id) == Some(super::rules::Layer::Norm)).then(|| spec.rules.get(id));
    let width_rule = super::spec::stair_width_rule(spec.typology);
    let (width, riser) = (norm(width_rule), norm("core.max_riser"));
    let (mut narrow, mut steep): (Option<(usize, f64)>, Option<(usize, f64)>) = (None, None);
    for (k, st) in p.storeys.iter().enumerate() {
        for s in &st.layout.stairs {
            if width.is_some_and(|w| s.width < w - 1e-9) && narrow.is_none_or(|n| s.width < n.1) {
                narrow = Some((k, s.width));
            }
            if riser.is_some_and(|r| s.max_riser > r + 1e-9) && steep.is_none_or(|n| s.max_riser > n.1) {
                steep = Some((k, s.max_riser));
            }
        }
    }
    if let (Some((k, w)), Some(min)) = (narrow, width) {
        let msg = format!("a stair flight is {w:.2} m wide, less than {min} m");
        out.push(finding(spec, width_rule, Some(k), (min - w) / min, msg, None));
    }
    if let (Some((k, r)), Some(max)) = (steep, riser) {
        let msg = format!("stair risers up to {r:.3} m, more than {max} m");
        out.push(finding(spec, "core.max_riser", Some(k), (r - max) / max, msg, None));
    }
}

/// The rule changes the findings propose, in finding order.
pub fn fixes(found: &[Finding]) -> Vec<RuleFix> {
    found.iter().filter_map(|f| f.fix.clone()).collect()
}

/// `weight * excess` summed over the findings of soft rules.
pub fn penalty(found: &[Finding]) -> f64 {
    found.iter().filter(|f| !f.hard).map(|f| f.weight * f.excess).sum()
}

pub(super) fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn seg_dist(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, (a.0 + t * dx, a.1 + t * dy))
}

/// Distance of `p` from the line through `a` and `b`.
fn line_dist(p: P, a: P, b: P) -> f64 {
    let len = dist(a, b);
    if len == 0.0 {
        return dist(p, a);
    }
    ((b.0 - a.0) * (a.1 - p.1) - (a.0 - p.0) * (b.1 - a.1)).abs() / len
}

fn on_outline(poly: &[P], p: P) -> bool {
    let n = poly.len();
    (0..n).any(|i| seg_dist(p, poly[i], poly[(i + 1) % n]) < ON_EDGE)
}

pub fn area(poly: &[P]) -> f64 {
    let n = poly.len();
    ((0..n)
        .map(|i| poly[i].0 * poly[(i + 1) % n].1 - poly[(i + 1) % n].0 * poly[i].1)
        .sum::<f64>()
        / 2.0)
        .abs()
}

fn perimeter(poly: &[P]) -> f64 {
    let n = poly.len();
    (0..n).map(|i| dist(poly[i], poly[(i + 1) % n])).sum()
}

/// Width of a room read as a strip of one width `w` along a centre line `c`: area `w * c`
/// and half the perimeter `c + w`. Exact for a rectangle and for an L of one width.
pub fn strip_width(poly: &[P]) -> f64 {
    let (a, s) = (area(poly), perimeter(poly) / 2.0);
    (s - (s * s - 2.0 * 2.0 * a).max(0.0).sqrt()) / 2.0
}

fn centre(l: &GLayout, o: &GOpening) -> Option<P> {
    let w = l.walls.get(o.wall)?;
    let len = dist(w.a, w.b);
    if len == 0.0 {
        return None;
    }
    let t = (o.along + o.width / 2.0) / len;
    Some((w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t))
}

pub(super) struct Door {
    pub(super) at: P,
    pub(super) width: f64,
    pub(super) exterior: bool,
    /// Rooms whose outline it lies on: the two it joins, one for an outside door.
    pub(super) rooms: Vec<usize>,
}

pub(super) fn doors(st: &PStorey) -> Vec<Door> {
    let l = &st.layout;
    l.openings
        .iter()
        .filter(|o| o.door)
        .filter_map(|o| {
            let at = centre(l, o)?;
            Some(Door {
                at,
                width: o.width,
                exterior: l.walls[o.wall].role == WallRole::Exterior,
                rooms: (0..l.rooms.len()).filter(|&i| on_outline(&l.rooms[i].polygon, at)).collect(),
            })
        })
        .collect()
}

/// Exits of storey `k`: the outside doors on the ground floor, the stair doors above.
pub(super) fn exits(st: &PStorey, doors: &[Door], k: usize) -> Vec<bool> {
    doors
        .iter()
        .map(|d| {
            if k == 0 {
                d.exterior
            } else {
                d.rooms
                    .iter()
                    .any(|&r| st.rooms.get(r).is_some_and(|m| m.kind == RoomKind::Stair))
            }
        })
        .collect()
}

/// Shortest way from every door to an exit, through any room but a lift.
pub(super) fn to_exit(st: &PStorey, doors: &[Door], exit: &[bool]) -> Vec<f64> {
    let n = doors.len();
    let mut d: Vec<f64> = exit.iter().map(|&e| if e { 0.0 } else { f64::INFINITY }).collect();
    let mut done = vec![false; n];
    while let Some(u) = (0..n)
        .filter(|&i| !done[i] && d[i].is_finite())
        .min_by(|&a, &b| d[a].total_cmp(&d[b]))
    {
        done[u] = true;
        for &r in &doors[u].rooms {
            if st.rooms.get(r).is_none_or(|m| m.kind == RoomKind::Lift) {
                continue;
            }
            for v in 0..n {
                if doors[v].rooms.contains(&r) {
                    let c = d[u] + dist(doors[u].at, doors[v].at);
                    if c < d[v] {
                        d[v] = c;
                    }
                }
            }
        }
    }
    d
}

/// Way out of room `r`: from its farthest corner through its best door; `None` when no door
/// of it leads out.
pub(super) fn travel(poly: &[P], doors: &[Door], d: &[f64], r: usize) -> Option<f64> {
    let mine: Vec<usize> = (0..doors.len())
        .filter(|&i| doors[i].rooms.contains(&r) && d[i].is_finite())
        .collect();
    if mine.is_empty() {
        return None;
    }
    poly.iter()
        .map(|&v| {
            mine.iter()
                .map(|&i| dist(v, doors[i].at) + d[i])
                .fold(f64::INFINITY, f64::min)
        })
        .reduce(f64::max)
}

/// Way out of every room of storey `k` (from 0), in room order: `None` for a stair, a lift,
/// a storey without an exit door, and a room no door of which leads out.
pub fn exit_distances(st: &PStorey, k: usize) -> Vec<Option<f64>> {
    let doors = doors(st);
    let exit = exits(st, &doors, k);
    let d = to_exit(st, &doors, &exit);
    st.layout
        .rooms
        .iter()
        .zip(&st.rooms)
        .enumerate()
        .map(|(r, (g, m))| {
            if matches!(m.kind, RoomKind::Stair | RoomKind::Lift) {
                None
            } else {
                travel(&g.polygon, &doors, &d, r)
            }
        })
        .collect()
}

/// The farthest room from an exit on each storey, against `egress.max_travel`.
fn egress(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    let limit = spec.rules.get("egress.max_travel");
    let (mut worst, mut first, mut floors): (Option<(f64, String)>, Option<usize>, usize) = (None, None, 0);
    for (k, st) in p.storeys.iter().enumerate() {
        let mut far: Option<(f64, &str)> = None;
        for (t, m) in exit_distances(st, k).into_iter().zip(&st.rooms) {
            if let Some(t) = t.filter(|&t| far.is_none_or(|f| t > f.0)) {
                far = Some((t, m.number.as_str()));
            }
        }
        if let Some((t, room)) = far.filter(|f| f.0 > limit + 1e-9) {
            floors += 1;
            first.get_or_insert(k);
            if worst.as_ref().is_none_or(|w| t > w.0) {
                worst = Some((t, room.to_string()));
            }
        }
    }
    if let Some((t, room)) = worst {
        out.push(finding(
            spec,
            "egress.max_travel",
            first,
            (t - limit) / limit,
            format!(
                "room {room} is {t:.1} m from the nearest exit, more than {limit} m, on {floors} floor(s): \
                 add a stair or shorten the plan"
            ),
            None,
        ));
    }
}

/// Lit rooms deeper from their window wall than `light.depth_ratio` times the window head.
fn daylight(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    let ratio = spec.rules.get("light.depth_ratio");
    let mut deep: Vec<String> = vec![];
    // the worst room: depth, limit, number
    let mut worst: Option<(f64, f64, String)> = None;
    let mut first = None;
    for (k, st) in p.storeys.iter().enumerate() {
        let l = &st.layout;
        for (g, m) in l.rooms.iter().zip(&st.rooms) {
            if !m.kind.habitable() {
                continue;
            }
            // the window wall the room is least deep from, for the window on it
            let mut best: Option<(f64, f64)> = None;
            for o in l.openings.iter().filter(|o| !o.door) {
                let Some(w) = l.walls.get(o.wall).filter(|w| w.role == WallRole::Exterior) else {
                    continue;
                };
                if !centre(l, o).is_some_and(|c| on_outline(&g.polygon, c)) {
                    continue;
                }
                let depth = g.polygon.iter().map(|&v| line_dist(v, w.a, w.b)).fold(0.0, f64::max);
                let lim = ratio * (o.sill + o.height);
                if lim > 0.0 && best.is_none_or(|b| depth / lim < b.0 / b.1) {
                    best = Some((depth, lim));
                }
            }
            let Some((depth, lim)) = best.filter(|b| b.0 > b.1 + 1e-9) else {
                continue;
            };
            first.get_or_insert(k);
            deep.push(m.number.clone());
            if worst.as_ref().is_none_or(|w| depth / lim > w.0 / w.1) {
                worst = Some((depth, lim, m.number.clone()));
            }
        }
    }
    let Some((depth, lim, room)) = worst else {
        return;
    };
    let shown: Vec<&str> = deep.iter().take(SHOWN).map(String::as_str).collect();
    let fix = if spec.traits.one_household || spec.plate.is_some() {
        None
    } else {
        // rooms as deep as the band: a band no deeper than the limit
        RuleFix::keyed("depth.band", json!(dm_floor(lim)), "fix_daylight_depth", vec![])
        .ok()
    };
    out.push(finding(
        spec,
        "light.depth_ratio",
        first,
        (depth - lim) / lim,
        format!(
            "{} lit room(s) deeper than {ratio} x window head ({}): room {room} is {depth:.1} m deep, limit {lim:.1} m",
            deep.len(),
            shown.join(", ")
        ),
        fix,
    ));
}

/// Fix values in whole decimetres, rounded towards the safe side.
fn dm_floor(x: f64) -> f64 {
    (x * 10.0 + 1e-9).floor() / 10.0
}

fn dm_ceil(x: f64) -> f64 {
    (x * 10.0 - 1e-9).ceil() / 10.0
}

/// Common routes (public corridors, lobbies and halls) and the doors off them, against the
/// accessibility rules. A house is one household and has no common route.
fn access(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    if spec.traits.one_household {
        return;
    }
    let route = spec.rules.get("access.route_width");
    let door_w = spec.rules.get("access.door_width");
    let turn = spec.rules.get("access.turning");
    // narrowest route (width, room), storeys with no turning place, narrowest door
    let mut narrow: Option<(f64, String, usize)> = None;
    let mut no_turn: Vec<usize> = vec![];
    let mut widest_short = f64::INFINITY;
    let mut door: Option<(f64, usize, usize)> = None;
    for (k, st) in p.storeys.iter().enumerate() {
        let l = &st.layout;
        let routes: Vec<usize> = (0..l.rooms.len().min(st.rooms.len()))
            .filter(|&i| st.rooms[i].kind.circulation() && st.rooms[i].owner == PRole::Public)
            .collect();
        if routes.is_empty() {
            continue;
        }
        let mut widest = 0.0f64;
        for &i in &routes {
            let w = strip_width(&l.rooms[i].polygon);
            widest = widest.max(w);
            if w < route - 1e-9 && narrow.as_ref().is_none_or(|n| w < n.0) {
                narrow = Some((w, st.rooms[i].number.clone(), k));
            }
        }
        if widest < turn - 1e-9 {
            no_turn.push(k);
            widest_short = widest_short.min(widest);
        }
        let mut few = 0;
        let mut min_w = f64::INFINITY;
        for d in doors(st) {
            if d.rooms.iter().any(|r| routes.contains(r)) && d.width < door_w - 1e-9 {
                few += 1;
                min_w = min_w.min(d.width);
            }
        }
        if few > 0 && door.is_none_or(|x| min_w < x.0) {
            door = Some((min_w, few, k));
        }
    }
    let widen = |to: f64, key: &'static str| RuleFix::keyed("corridor.width", json!(dm_ceil(to)), key, vec![]).ok();
    if let Some((w, room, k)) = narrow {
        out.push(finding(
            spec,
            "access.route_width",
            Some(k),
            (route - w) / route,
            format!("{room} is {w:.2} m wide, a common route needs {route} m"),
            widen(route, "fix_route_width"),
        ));
    }
    if let Some(&k) = no_turn.first() {
        out.push(finding(
            spec,
            "access.turning",
            Some(k),
            (turn - widest_short) / turn,
            format!(
                "no place {turn} m wide to turn a wheelchair on the common route of {} floor(s); the widest is {widest_short:.2} m",
                no_turn.len()
            ),
            widen(turn, "fix_turning"),
        ));
    }
    if let Some((w, n, k)) = door {
        // the door rule that made the narrowest door
        let id = if (w - spec.rules.get("room.door_wet")).abs() < 1e-9 { "room.door_wet" } else { "room.door" };
        out.push(finding(
            spec,
            "access.door_width",
            Some(k),
            (door_w - w) / door_w,
            format!("{n} door(s) off the common route are {w:.2} m wide, less than {door_w} m"),
            RuleFix::keyed(id, json!(door_w), "fix_door_width", vec![]).ok(),
        ));
    }
}
