// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fire safety and egress. The checks: occupants of every room from its use and floor area,
//! the exits and exit width each storey and each crowded room needs, the stair width the
//! occupants of a storey need, dead-end corridors, the spacing of exits, and fire compartments
//! between rated walls. The generator's side: exit doors added before anything is reported,
//! fire walls at compartment boundaries of a bar, and rated stair enclosures. Every number is
//! a rule; a country pack sets them with their clauses.

use super::nav::{self, area, dist, finding, Finding};
use super::tile::{m, Cell, Mm};
use super::why::{s, RuleFix, Why};
use super::{GProject, GStairHead, PRoof, PRole, PRoom, PStorey, ProjectSpec, RoomKind};
use crate::{FireSep, GLayout, GOpening, GWall, WallFire, WallRole};
use serde_json::json;

type P = (f64, f64);

/// Geometric tolerance of points on wall centre lines.
const ON: f64 = 1e-6;
/// How far beside a wall a point is taken to find the room on its other side.
const PROBE: f64 = 0.05;

/// The occupant-area rule of a room: every room of a dwelling counts as residential, a room of
/// no use (wet rooms, circulation) holds nobody.
pub fn use_rule(kind: RoomKind, owner: PRole) -> Option<&'static str> {
    use RoomKind::*;
    if matches!(owner, PRole::Apartment(_)) {
        return Some("egress.occ_residential");
    }
    Some(match kind {
        Office | Staff | Lab => "egress.occ_office",
        Meeting | Dining => "egress.occ_assembly",
        Classroom => "egress.occ_classroom",
        Retail => "egress.occ_retail",
        Living | Bedroom | Kitchen => "egress.occ_residential",
        Ward => "egress.occ_ward",
        Workshop => "egress.occ_industrial",
        Storage | Technical | Parking => "egress.occ_storage",
        Gym => "egress.occ_exercise",
        Bath | Wc | Hall | Lobby | Corridor | Stair | Lift => return None,
    })
}

/// Occupants of a room of `a` m2: its area over the area per person, rounded up.
pub fn room_people(spec: &ProjectSpec, kind: RoomKind, owner: PRole, a: f64) -> u32 {
    match use_rule(kind, owner) {
        Some(id) => (a / spec.rules.get(id)).ceil().max(0.0) as u32,
        None => 0,
    }
}

/// Occupants of every room of a storey, in room order.
pub fn people_of(spec: &ProjectSpec, st: &PStorey) -> Vec<u32> {
    st.layout
        .rooms
        .iter()
        .zip(&st.rooms)
        .map(|(g, r)| room_people(spec, r.kind, r.owner, area(&g.polygon)))
        .collect()
}

/// Exits a room or storey of `people` needs: one up to `egress.single_exit_people`, two up to
/// `egress.three_exits_people`, three up to `egress.four_exits_people`, else four.
pub fn exits_needed(spec: &ProjectSpec, people: u32) -> usize {
    let p = f64::from(people);
    if p <= spec.rules.get("egress.single_exit_people") {
        1
    } else if p <= spec.rules.get("egress.three_exits_people") {
        2
    } else if p <= spec.rules.get("egress.four_exits_people") {
        3
    } else {
        4
    }
}

/// The rule whose occupant limit a storey with `need` exits passed.
fn exits_rule(need: usize) -> &'static str {
    match need {
        0..=2 => "egress.single_exit_people",
        3 => "egress.three_exits_people",
        _ => "egress.four_exits_people",
    }
}

/// What the fire rules measure on one storey.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreyFire {
    pub people: u32,
    /// Exits the occupants need.
    pub need: usize,
    /// Exits there are: outside doors on the ground, stairs above (each counted once).
    pub exits: usize,
    /// Clear width of those exits together.
    pub exit_width: f64,
    /// Flight width of the stairs that serve the storey from below; `None` on the ground.
    pub stair_width: Option<f64>,
    /// Longest corridor end with one way on.
    pub dead_end: f64,
    /// Farthest two exits apart, over the storey's diagonal.
    pub separation: Option<f64>,
    /// Rooms short of exits or exit width: (room, occupants, doors, door width).
    pub crowded: Vec<(usize, u32, usize, f64)>,
}

fn gate(spec: &ProjectSpec, o: &GOpening) -> bool {
    o.width >= spec.rules.get("facade.gate_w") - ON
}

pub fn centre(l: &GLayout, o: &GOpening) -> Option<P> {
    let w = l.walls.get(o.wall)?;
    let len = dist(w.a, w.b);
    (len > 0.0).then(|| {
        let t = (o.along + o.width / 2.0) / len;
        (w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t)
    })
}

fn seg_dist(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

pub fn on_outline(poly: &[P], p: P) -> bool {
    let n = poly.len();
    (0..n).any(|i| seg_dist(p, poly[i], poly[(i + 1) % n]) < ON)
}

fn inside(poly: &[P], p: P) -> bool {
    let n = poly.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
            c = !c;
        }
    }
    c
}

/// Door openings of a storey with their centres and the rooms whose outline holds them.
fn door_list(l: &GLayout) -> Vec<(usize, P, Vec<usize>)> {
    l.openings
        .iter()
        .enumerate()
        .filter(|(_, o)| o.door)
        .filter_map(|(i, o)| {
            let at = centre(l, o)?;
            let rooms = (0..l.rooms.len()).filter(|&r| on_outline(&l.rooms[r].polygon, at)).collect();
            Some((i, at, rooms))
        })
        .collect()
}

/// Exit doors of storey `k` (opening indices): outside doors but vehicle gates on the ground,
/// doors into a stair above.
fn exit_doors(spec: &ProjectSpec, st: &PStorey, k: usize) -> Vec<usize> {
    let l = &st.layout;
    door_list(l)
        .into_iter()
        .filter(|(i, _, rooms)| {
            let o = &l.openings[*i];
            if k == 0 {
                l.walls[o.wall].role == WallRole::Exterior && !gate(spec, o)
            } else {
                rooms.iter().any(|&r| st.rooms.get(r).is_some_and(|m| m.kind == RoomKind::Stair))
            }
        })
        .map(|(i, _, _)| i)
        .collect()
}

/// Exits as the occupants count them: each outside door on the ground, each stair above.
fn exit_count(st: &PStorey, k: usize, doors: &[usize]) -> usize {
    if k == 0 {
        return doors.len();
    }
    let l = &st.layout;
    let mut stairs: Vec<usize> = doors
        .iter()
        .filter_map(|&i| {
            let at = centre(l, &l.openings[i])?;
            (0..l.rooms.len()).find(|&r| st.rooms[r].kind == RoomKind::Stair && on_outline(&l.rooms[r].polygon, at))
        })
        .collect();
    stairs.sort_unstable();
    stairs.dedup();
    stairs.len()
}

fn bbox(poly: &[P]) -> (P, P) {
    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    for p in poly {
        lo = (lo.0.min(p.0), lo.1.min(p.1));
        hi = (hi.0.max(p.0), hi.1.max(p.1));
    }
    (lo, hi)
}

/// The storey outline: its slab, else the box of its rooms.
fn plate(l: &GLayout) -> (P, P) {
    if l.slab.len() >= 3 {
        bbox(&l.slab)
    } else {
        bbox(&l.rooms.iter().flat_map(|r| r.polygon.iter().copied()).collect::<Vec<_>>())
    }
}

/// Dead ends of every corridor room of storey `k`: (room, end at the low side?, length).
/// The ways on are the exits and the doors into other circulation; a corridor end beyond the
/// last of them leads one way only.
fn dead_ends(spec: &ProjectSpec, st: &PStorey, k: usize) -> Vec<(usize, bool, f64)> {
    let l = &st.layout;
    let exits = exit_doors(spec, st, k);
    let doors = door_list(l);
    let mut out = vec![];
    for (r, room) in st.rooms.iter().enumerate() {
        if room.kind != RoomKind::Corridor {
            continue;
        }
        let (lo, hi) = bbox(&l.rooms[r].polygon);
        let along_x = hi.0 - lo.0 >= hi.1 - lo.1;
        let t = |p: P| if along_x { p.0 } else { p.1 };
        let ways: Vec<f64> = doors
            .iter()
            .filter(|(_, _, rooms)| rooms.contains(&r))
            .filter(|(i, _, rooms)| {
                exits.contains(i)
                    || rooms.iter().any(|&q| {
                        q != r
                            && st.rooms.get(q).is_some_and(|m| {
                                matches!(m.kind, RoomKind::Corridor | RoomKind::Lobby)
                                    || (k > 0 && m.kind == RoomKind::Stair)
                            })
                    })
            })
            .map(|(_, at, _)| t(*at))
            .collect();
        let (a, b) = (t(lo), t(hi));
        let first = ways.iter().copied().fold(f64::INFINITY, f64::min);
        let last = ways.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if ways.is_empty() {
            out.push((r, true, b - a));
            continue;
        }
        out.push((r, true, first - a));
        out.push((r, false, b - last));
    }
    out
}

/// Measures every storey above ground and the ground floor.
pub fn measure(spec: &ProjectSpec, p: &GProject) -> Vec<StoreyFire> {
    p.storeys
        .iter()
        .enumerate()
        .map(|(k, st)| measure_storey(spec, st, k, (k > 0).then(|| stair_sum(&p.storeys[k - 1]))))
        .collect()
}

fn stair_sum(st: &PStorey) -> f64 {
    st.layout.stairs.iter().map(|s| s.width).sum()
}

/// The basements measured like the storeys above ground: their way out is a stair, and the
/// stairs they hold carry their occupants.
pub fn measure_basements(spec: &ProjectSpec, p: &GProject) -> Vec<StoreyFire> {
    p.basements.iter().map(|st| measure_storey(spec, st, 1, Some(stair_sum(st)))).collect()
}

fn measure_storey(spec: &ProjectSpec, st: &PStorey, k: usize, stair_width: Option<f64>) -> StoreyFire {
    let l = &st.layout;
    let each = people_of(spec, st);
    let people: u32 = each.iter().sum();
    let need = exits_needed(spec, people);
    let ex = exit_doors(spec, st, k);
    let exits = exit_count(st, k, &ex);
    let exit_width: f64 = ex.iter().map(|&i| l.openings[i].width).sum();
    let dead_end = if need >= 2 {
        dead_ends(spec, st, k).iter().map(|d| d.2).fold(0.0, f64::max)
    } else {
        0.0
    };
    let at: Vec<P> = ex.iter().filter_map(|&i| centre(l, &l.openings[i])).collect();
    let (lo, hi) = plate(l);
    let diag = dist(lo, hi);
    let separation = (need >= 2 && at.len() >= 2 && diag > 0.0).then(|| {
        let mut far = 0.0f64;
        for (i, a) in at.iter().enumerate() {
            for b in &at[i + 1..] {
                far = far.max(dist(*a, *b));
            }
        }
        far / diag
    });
    let doors = door_list(l);
    let pp = spec.rules.get("egress.door_per_person");
    let single = spec.rules.get("egress.single_exit_people");
    let crowded = each
        .iter()
        .enumerate()
        .filter(|(_, n)| f64::from(**n) > single)
        .filter_map(|(r, &n)| {
            let mine: Vec<&(usize, P, Vec<usize>)> = doors.iter().filter(|d| d.2.contains(&r)).collect();
            let w: f64 = mine.iter().map(|d| l.openings[d.0].width).sum();
            let short = mine.len() < exits_needed(spec, n) || w < f64::from(n) * pp - ON;
            short.then_some((r, n, mine.len(), w))
        })
        .collect();
    StoreyFire { people, need, exits, exit_width, stair_width, dead_end, separation, crowded }
}

/// Whether one more stair would help a bar: a storey above ground short of exits, of stair
/// width, of exit spacing, with a corridor end too long, or a room too far from an exit.
pub(super) fn stairs_short(spec: &ProjectSpec, p: &GProject) -> bool {
    let travel = spec.rules.get("egress.max_travel");
    let far = |k: usize, st: &PStorey| {
        nav::exit_distances(st, k).into_iter().flatten().any(|t| t > travel + ON)
    };
    // a high-rise keeps two stairs on every storey whatever its occupants
    if high_rise(spec) && p.storeys.first().is_some_and(|s| s.layout.stairs.len() < 2) {
        return true;
    }
    let m = measure(spec, p);
    m.iter().enumerate().skip(1).any(|(k, f)| {
        f.exits < f.need
            || stair_short(spec, f).is_some()
            || f.dead_end > spec.rules.get("egress.dead_end") + ON
            || f.separation.is_some_and(|x| x < spec.rules.get("egress.exit_separation") - ON)
            || far(k, &p.storeys[k])
    })
}

/// Flight width the storey's occupants need, when the stairs below fall short of it.
fn stair_short(spec: &ProjectSpec, f: &StoreyFire) -> Option<f64> {
    let need = f64::from(f.people) * spec.rules.get("egress.stair_per_person");
    f.stair_width.filter(|&w| w > 0.0 && w < need - ON).map(|_| need)
}

/// Flight width each of the stairs needs for the most crowded storey above ground, rounded up
/// to a centimetre, when the stairs fall short.
pub(super) fn stair_need(spec: &ProjectSpec, p: &GProject) -> Option<f64> {
    let n = p.storeys.first().map_or(0, |s| s.layout.stairs.len());
    if n == 0 {
        return None;
    }
    measure(spec, p)
        .iter()
        .filter_map(|f| stair_short(spec, f))
        .reduce(f64::max)
        .map(|need| (need / n as f64 * 100.0).ceil() / 100.0)
}

/// Every fire and egress check of a built project, one finding per broken rule.
pub fn check(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    let mut worst: Vec<(&'static str, usize, f64, String, Option<RuleFix>, usize)> = vec![];
    let mut note = |rule: &'static str, k: usize, excess: f64, msg: String, fix: Option<RuleFix>| {
        match worst.iter_mut().find(|w| w.0 == rule) {
            Some(w) => {
                w.5 += 1;
                if excess > w.2 {
                    (w.1, w.2, w.3, w.4) = (k, excess, msg, fix);
                }
            }
            None => worst.push((rule, k, excess, msg, fix, 1)),
        }
    };
    let pp = spec.rules.get("egress.door_per_person");
    let dead = spec.rules.get("egress.dead_end");
    let sep = spec.rules.get("egress.exit_separation");
    let n_up = p.storeys.len();
    let levels: Vec<(usize, &PStorey, StoreyFire, usize)> = measure(spec, p)
        .into_iter()
        .enumerate()
        .map(|(k, f)| (k, &p.storeys[k], f, if k > 0 { p.storeys[k - 1].layout.stairs.len() } else { 0 }))
        .chain(
            measure_basements(spec, p)
                .into_iter()
                .enumerate()
                .map(|(b, f)| (n_up + b, &p.basements[b], f, p.basements[b].layout.stairs.len())),
        )
        .collect();
    for (k, st, f, stairs_below) in levels.iter().map(|(k, st, f, n)| (*k, *st, f, *n)) {
        if f.exits < f.need {
            let what = if k == 0 { "outside door(s)" } else { "stair(s)" };
            note(
                exits_rule(f.need),
                k,
                (f.need - f.exits) as f64 / f.need as f64,
                format!("{} occupants need {} exits, the storey has {} {what}", f.people, f.need, f.exits),
                None,
            );
        }
        let need_w = f64::from(f.people) * pp;
        if f.exits > 0 && f.exit_width < need_w - ON {
            note(
                "egress.door_per_person",
                k,
                (need_w - f.exit_width) / need_w,
                format!("{} occupants need {need_w:.2} m of exit doors, the exits are {:.2} m wide", f.people, f.exit_width),
                None,
            );
        }
        if let Some(need) = stair_short(spec, f) {
            let have = f.stair_width.unwrap_or(0.0);
            let n = stairs_below.max(1);
            let each = (need / n as f64 * 100.0).ceil() / 100.0;
            let id = super::spec::stair_width_rule(spec.typology);
            let fix = super::rules::def(id)
                .filter(|d| each <= d.max)
                .and_then(|_| RuleFix::keyed(id, json!(each), "fix_stair_capacity", vec![]).ok());
            note(
                "egress.stair_per_person",
                k,
                (need - have) / need,
                format!("{} occupants need {need:.2} m of stair flights, the stairs below give {have:.2} m", f.people),
                fix,
            );
        }
        if f.dead_end > dead + ON {
            note(
                "egress.dead_end",
                k,
                (f.dead_end - dead) / dead.max(ON),
                format!("a corridor ends {:.1} m beyond its last way on, more than {dead} m", f.dead_end),
                None,
            );
        }
        if let Some(x) = f.separation.filter(|&x| x < sep - ON) {
            note(
                "egress.exit_separation",
                k,
                (sep - x) / sep,
                format!("the exits stand {x:.2} of the storey's diagonal apart, less than {sep}"),
                None,
            );
        }
        if let Some(&(r, n, doors, w)) = f.crowded.iter().max_by_key(|c| c.1) {
            let number = st.rooms.get(r).map_or("", |m| m.number.as_str());
            let need = exits_needed(spec, n);
            let need_w = f64::from(n) * pp;
            let excess = ((need.saturating_sub(doors)) as f64 / need as f64).max((need_w - w) / need_w);
            note(
                "egress.door_per_person",
                k,
                excess,
                format!(
                    "room {number} holds {n} occupants: {need} door(s) {need_w:.2} m wide in all, it has {doors} door(s) {w:.2} m wide"
                ),
                None,
            );
        }
    }
    for (rule, k, excess, msg, fix, n) in worst {
        let msg = if n > 1 { format!("{msg}; {n} such case(s)") } else { msg };
        out.push(finding(spec, rule, Some(k), excess, msg, fix));
    }
    compartments(spec, p, out);
    if spec.rules.get("egress.protected_stairs") >= 2.0 {
        let bare: Vec<(usize, usize)> = p
            .storeys
            .iter()
            .chain(&p.basements)
            .enumerate()
            .map(|(k, st)| (k, bare_stairs(st).len()))
            .filter(|b| b.1 > 0)
            .collect();
        if let Some(&(k, _)) = bare.first() {
            let n: usize = bare.iter().map(|b| b.1).sum();
            let msg = format!("{n} stair room(s) have no room for a smoke lobby: a stair door opens straight from the corridor");
            out.push(finding(spec, "egress.protected_stairs", Some(k), n as f64, msg, None));
        }
    }
}

/// Rooms of a storey joined into fire compartments: two rooms meet unless a compartment wall
/// lies between them. Returns the compartment of every room.
pub fn compartments_of(l: &GLayout) -> Vec<usize> {
    let n = l.rooms.len();
    let fire_walls: Vec<&GWall> = l
        .walls
        .iter()
        .filter(|w| w.fire.is_some_and(|f| f.sep == FireSep::Compartment))
        .collect();
    let mut comp: Vec<usize> = (0..n).collect();
    fn find(c: &mut Vec<usize>, i: usize) -> usize {
        let mut i = i;
        while c[i] != i {
            c[i] = c[c[i]];
            i = c[i];
        }
        i
    }
    for i in 0..n {
        for j in i + 1..n {
            let Some(mid) = shared_mid(&l.rooms[i].polygon, &l.rooms[j].polygon) else {
                continue;
            };
            if fire_walls.iter().any(|w| seg_dist(mid, w.a, w.b) < ON) {
                continue;
            }
            let (a, b) = (find(&mut comp, i), find(&mut comp, j));
            comp[a] = b;
        }
    }
    (0..n).map(|i| find(&mut comp, i)).collect()
}

/// The middle of the longest stretch two room outlines share, if they share one.
fn shared_mid(a: &[P], b: &[P]) -> Option<P> {
    let mut best: Option<(f64, P)> = None;
    for i in 0..a.len() {
        let (p0, p1) = (a[i], a[(i + 1) % a.len()]);
        let len = dist(p0, p1);
        if len < ON {
            continue;
        }
        let d = ((p1.0 - p0.0) / len, (p1.1 - p0.1) / len);
        for j in 0..b.len() {
            let (q0, q1) = (b[j], b[(j + 1) % b.len()]);
            let cross = |q: P| ((q.0 - p0.0) * d.1 - (q.1 - p0.1) * d.0).abs();
            if cross(q0) > ON || cross(q1) > ON {
                continue;
            }
            let t = |q: P| (q.0 - p0.0) * d.0 + (q.1 - p0.1) * d.1;
            let (s0, s1) = (t(q0).min(t(q1)).max(0.0), t(q0).max(t(q1)).min(len));
            if s1 - s0 > ON && best.is_none_or(|x| s1 - s0 > x.0) {
                let sm = (s0 + s1) / 2.0;
                best = Some((s1 - s0, (p0.0 + d.0 * sm, p0.1 + d.1 * sm)));
            }
        }
    }
    best.map(|b| b.1)
}

/// Each compartment against `fire.compartment_area` (its share of the slab) and its length
/// against `fire.wall_spacing`, unless the norm sets no limit.
fn compartments(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    let area_free = spec.rules.unlimited("fire.compartment_area").is_some();
    let len_free = spec.rules.unlimited("fire.wall_spacing").is_some();
    let (limit, spacing) = (spec.rules.get("fire.compartment_area"), spec.rules.get("fire.wall_spacing"));
    let (mut big, mut first, mut most) = (0usize, None, 0.0f64);
    let (mut long, mut first_long, mut longest) = (0usize, None, 0.0f64);
    let mut rated = false;
    for (k, st) in p.storeys.iter().chain(&p.basements).enumerate() {
        let l = &st.layout;
        let comp = compartments_of(l);
        rated |= l.walls.iter().any(|w| w.fire.is_some_and(|f| f.sep == FireSep::Compartment));
        let rooms: f64 = l.rooms.iter().map(|r| area(&r.polygon)).sum();
        let gross = if l.slab.len() >= 3 { area(&l.slab) } else { rooms };
        let scale = if rooms > 0.0 { gross / rooms } else { 1.0 };
        let mut ids = comp.clone();
        ids.sort_unstable();
        ids.dedup();
        for c in ids {
            let members: Vec<usize> = (0..comp.len()).filter(|&r| comp[r] == c).collect();
            let a: f64 = members.iter().map(|&r| area(&l.rooms[r].polygon)).sum::<f64>() * scale;
            let pts: Vec<P> = members.iter().flat_map(|&r| l.rooms[r].polygon.iter().copied()).collect();
            let (lo, hi) = bbox(&pts);
            let len = (hi.0 - lo.0).max(hi.1 - lo.1);
            if !area_free && a > limit + 1e-9 {
                big += 1;
                first.get_or_insert(k);
                most = most.max(a);
            }
            if !len_free && len > spacing + 1e-9 {
                long += 1;
                first_long.get_or_insert(k);
                longest = longest.max(len);
            }
        }
    }
    if big > 0 {
        let depth = p.footprint.1;
        let fix = if spec.design.massing == super::Massing::Bar && depth > 0.0 && !rated {
            RuleFix::keyed("massing.width", json!((limit / depth * 10.0).floor() / 10.0), "fix_compartment", vec![]).ok()
        } else {
            None
        };
        out.push(finding(
            spec,
            "fire.compartment_area",
            first,
            (most - limit) / limit,
            format!(
                "{big} compartment(s) up to {most:.0} m2, more than {limit} m2: no room boundary runs across \
                 the plate for a fire wall; add fire walls or a smaller plate"
            ),
            fix,
        ));
    }
    if long > 0 {
        out.push(finding(
            spec,
            "fire.wall_spacing",
            first_long,
            (longest - spacing) / spacing,
            format!("{long} compartment(s) up to {longest:.1} m long, more than {spacing} m between fire walls"),
            None,
        ));
    }
}

// ---- the generator's side -------------------------------------------------------------------

/// Free stretches of wall `wi` (from its start) between `lo` and `hi`, clear of its openings
/// by `gap`.
fn free_spans(l: &GLayout, wi: usize, lo: f64, hi: f64, gap: f64, skip: Option<usize>) -> Vec<(f64, f64)> {
    let mut spans = vec![(lo, hi)];
    for (i, o) in l.openings.iter().enumerate() {
        if o.wall != wi || Some(i) == skip {
            continue;
        }
        let (a, b) = (o.along - gap, o.along + o.width + gap);
        spans = spans
            .into_iter()
            .flat_map(|(s0, s1)| {
                if b <= s0 || a >= s1 {
                    vec![(s0, s1)]
                } else {
                    [(s0, a), (b, s1)].into_iter().filter(|x| x.1 > x.0).collect()
                }
            })
            .collect();
    }
    spans
}

/// Where a new door of room `r` may go: every straight wall along an edge of its outline whose
/// other side `ok` accepts (`None` is outside), as (wall, from, to) along the wall.
fn door_spans(
    l: &GLayout,
    r: usize,
    clear: f64,
    ok: &dyn Fn(Option<usize>, &GWall, (P, P)) -> bool,
) -> Vec<(usize, f64, f64)> {
    let poly = &l.rooms[r].polygon;
    let mut out = vec![];
    for i in 0..poly.len() {
        let (e0, e1) = (poly[i], poly[(i + 1) % poly.len()]);
        for (wi, w) in l.walls.iter().enumerate() {
            if w.bulge != 0.0 {
                continue;
            }
            let len = dist(w.a, w.b);
            if len < ON || seg_dist(e0, w.a, w.b) > ON && seg_dist(w.a, e0, e1) > ON {
                continue;
            }
            let d = ((w.b.0 - w.a.0) / len, (w.b.1 - w.a.1) / len);
            let cross = |q: P| ((q.0 - w.a.0) * d.1 - (q.1 - w.a.1) * d.0).abs();
            if cross(e0) > ON || cross(e1) > ON {
                continue;
            }
            let t = |q: P| (q.0 - w.a.0) * d.0 + (q.1 - w.a.1) * d.1;
            let (s0, s1) = (t(e0).min(t(e1)).max(0.0), t(e0).max(t(e1)).min(len));
            if s1 - s0 < 2.0 * clear {
                continue;
            }
            let mid = (w.a.0 + d.0 * (s0 + s1) / 2.0, w.a.1 + d.1 * (s0 + s1) / 2.0);
            let normal = (-d.1, d.0);
            let side = |k: f64| (mid.0 + normal.0 * k, mid.1 + normal.1 * k);
            let mine = inside(poly, side(PROBE));
            let other_pt = if mine { side(-PROBE) } else { side(PROBE) };
            let other = if w.role == WallRole::Exterior {
                None
            } else {
                (0..l.rooms.len()).find(|&q| q != r && inside(&l.rooms[q].polygon, other_pt))
            };
            if w.role != WallRole::Exterior && other.is_none() {
                continue;
            }
            if ok(other, w, (e0, e1)) {
                out.push((wi, s0 + clear, s1 - clear));
            }
        }
    }
    out
}

/// Puts a door `width` wide into one of `spans`, as far as it can from the points `away`;
/// false when none has room.
fn put_door(l: &mut GLayout, spans: &[(usize, f64, f64)], width: f64, away: &[P], clear: f64) -> bool {
    let mut best: Option<(f64, usize, f64)> = None;
    for &(wi, lo, hi) in spans {
        let w = &l.walls[wi];
        let len = dist(w.a, w.b);
        for (a, b) in free_spans(l, wi, lo, hi, clear, None) {
            if b - a < width - ON {
                continue;
            }
            for along in [a, (a + b - width) / 2.0, b - width] {
                let t = (along + width / 2.0) / len;
                let at = (w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t);
                let score = away.iter().map(|&q| dist(q, at)).fold(f64::INFINITY, f64::min);
                let score = if score.is_finite() { score } else { 0.0 };
                if best.is_none_or(|x| score > x.0 + ON) {
                    best = Some((score, wi, along));
                }
            }
        }
    }
    let Some((_, wall, along)) = best else {
        return false;
    };
    l.openings.push(GOpening { wall, along, door: true, width, height: crate::layout::DOOR_H, sill: 0.0, ty: None });
    true
}

fn room_doors(l: &GLayout, r: usize) -> Vec<(usize, P)> {
    door_list(l).into_iter().filter(|d| d.2.contains(&r)).map(|d| (d.0, d.1)).collect()
}

/// Width of the doors the generator adds, never under the least exit width.
fn exit_door(spec: &ProjectSpec) -> f64 {
    spec.rules.get("egress.exit_door").max(spec.rules.get("egress.exit_door_min"))
}

/// Exit doors of storey `k`, added before anything is reported: a crowded room gets doors onto
/// the public circulation (or outside on the ground) until it has the exits and width its
/// occupants need; the ground floor gets outside doors from its corridors, lobby and halls
/// until the storey has them too, and at corridor ends that lead one way only; the stair doors
/// above widen to their share of the exit width. Returns the doors added.
pub(super) fn open_exits(spec: &ProjectSpec, l: &mut GLayout, rooms: &[PRoom], k: usize) -> usize {
    let mut st = PStorey {
        name: String::new(),
        elevation: 0.0,
        height: 0.0,
        layout: std::mem::take(l),
        rooms: rooms.to_vec(),
        stair_room: None,
        fit: vec![],
        curtains: vec![],
        railings: vec![],
    };
    let added = open_storey(spec, &mut st, k);
    *l = st.layout;
    added
}

fn open_storey(spec: &ProjectSpec, st: &mut PStorey, k: usize) -> usize {
    let clear = spec.rules.get("facade.edge_clear");
    let pp = spec.rules.get("egress.door_per_person");
    let width = exit_door(spec);
    let rooms = st.rooms.clone();
    let public_way = |q: Option<usize>| {
        q.and_then(|q| rooms.get(q))
            .is_some_and(|m| m.owner == PRole::Public && matches!(m.kind, RoomKind::Corridor | RoomKind::Lobby | RoomKind::Hall))
    };
    let mut added = 0;
    // crowded rooms
    let each = people_of(spec, st);
    for (r, &n) in each.iter().enumerate() {
        if f64::from(n) <= spec.rules.get("egress.single_exit_people") {
            continue;
        }
        let need = exits_needed(spec, n);
        let need_w = f64::from(n) * pp;
        for _ in 0..need + 2 {
            let mine = room_doors(&st.layout, r);
            let w: f64 = mine.iter().map(|d| st.layout.openings[d.0].width).sum();
            if mine.len() >= need && w >= need_w - ON {
                break;
            }
            let away: Vec<P> = mine.iter().map(|d| d.1).collect();
            let ok = |q: Option<usize>, _: &GWall, _: (P, P)| public_way(q) || (k == 0 && q.is_none());
            let spans = door_spans(&st.layout, r, clear, &ok);
            if !put_door(&mut st.layout, &spans, width, &away, clear) {
                break;
            }
            added += 1;
        }
    }
    let people: u32 = each.iter().sum();
    let need = exits_needed(spec, people);
    let need_w = f64::from(people) * pp;
    if k == 0 {
        // outside doors, from the circulation first, the most crowded room last
        let mut order: Vec<usize> = (0..rooms.len())
            .filter(|&r| rooms[r].owner == PRole::Public && matches!(rooms[r].kind, RoomKind::Corridor | RoomKind::Lobby | RoomKind::Hall))
            .collect();
        let mut big: Vec<usize> = (0..rooms.len()).filter(|&r| each[r] > 0 && rooms[r].owner == PRole::Public).collect();
        big.sort_by_key(|&r| std::cmp::Reverse(each[r]));
        order.extend(big);
        let outside = |_: Option<usize>, w: &GWall, _: (P, P)| w.role == WallRole::Exterior;
        for _ in 0..need + 2 {
            let ex = exit_doors(spec, st, 0);
            let w: f64 = ex.iter().map(|&i| st.layout.openings[i].width).sum();
            if ex.len() >= need && w >= need_w - ON {
                break;
            }
            let away: Vec<P> = ex.iter().filter_map(|&i| centre(&st.layout, &st.layout.openings[i])).collect();
            let placed = order.iter().any(|&r| {
                let spans = door_spans(&st.layout, r, clear, &outside);
                put_door(&mut st.layout, &spans, width, &away, clear)
            });
            if !placed {
                break;
            }
            added += 1;
        }
        if need >= 2 {
            added += open_dead_ends(spec, st, clear, width);
        }
    } else {
        // the stair doors share the exit width
        let ex = exit_doors(spec, st, k);
        let have: f64 = ex.iter().map(|&i| st.layout.openings[i].width).sum();
        if !ex.is_empty() && have < need_w - ON {
            let each_w = (need_w / ex.len() as f64 * 100.0).ceil() / 100.0;
            for &i in &ex {
                widen(&mut st.layout, i, each_w, clear);
            }
        }
    }
    added
}

/// Corridor ends of the ground floor that lead one way only get an outside door at the end.
fn open_dead_ends(spec: &ProjectSpec, st: &mut PStorey, clear: f64, width: f64) -> usize {
    let limit = spec.rules.get("egress.dead_end");
    let mut added = 0;
    for (r, low, len) in dead_ends(spec, st, 0) {
        if len <= limit + ON {
            continue;
        }
        let (lo, hi) = bbox(&st.layout.rooms[r].polygon);
        let along_x = hi.0 - lo.0 >= hi.1 - lo.1;
        let end = if low { if along_x { lo.0 } else { lo.1 } } else if along_x { hi.0 } else { hi.1 };
        let at_end = move |_: Option<usize>, w: &GWall, e: (P, P)| {
            let (p, q) = e;
            w.role == WallRole::Exterior
                && if along_x {
                    (p.0 - end).abs() < ON && (q.0 - end).abs() < ON
                } else {
                    (p.1 - end).abs() < ON && (q.1 - end).abs() < ON
                }
        };
        let spans = door_spans(&st.layout, r, clear, &at_end);
        if put_door(&mut st.layout, &spans, width, &[], clear) {
            added += 1;
        }
    }
    added
}

/// Widens door `i` towards `width`, about its centre, as far as its wall leaves room.
fn widen(l: &mut GLayout, i: usize, width: f64, clear: f64) {
    let o = l.openings[i].clone();
    if o.width >= width - ON {
        return;
    }
    let w = &l.walls[o.wall];
    let len = dist(w.a, w.b);
    let c = o.along + o.width / 2.0;
    let Some((a, b)) = free_spans(l, o.wall, clear, len - clear, clear, Some(i))
        .into_iter()
        .find(|s| s.0 <= o.along + ON && o.along + o.width <= s.1 + ON)
    else {
        return;
    };
    let nw = width.min(b - a);
    let along = (c - nw / 2.0).clamp(a, b - nw);
    let x = &mut l.openings[i];
    x.width = nw;
    x.along = along;
}

/// Compartment cuts of a bar's floor: lines across the plate where no room straddles either
/// band, chosen greedily so each part stays within `fire.compartment_area` and
/// `fire.wall_spacing`. The corridor cell is split at each cut.
pub(super) fn cut_cells(spec: &ProjectSpec, cells: &mut Vec<Cell>, d: Mm, ds: Mm, yn: Mm) -> Vec<Mm> {
    if spec.rules.get("fire.walls") < 0.5 {
        return vec![];
    }
    let limit = if spec.rules.unlimited("fire.compartment_area").is_some() {
        f64::INFINITY
    } else {
        spec.rules.get("fire.compartment_area")
    };
    let spacing = if spec.rules.unlimited("fire.wall_spacing").is_some() {
        f64::INFINITY
    } else {
        spec.rules.get("fire.wall_spacing")
    };
    let len = cells.iter().map(|c| c.x1).max().unwrap_or(0);
    let fits = |a: Mm, b: Mm| m(b - a) * m(d) <= limit + 1e-9 && m(b - a) <= spacing + 1e-9;
    if fits(0, len) {
        return vec![];
    }
    let south = |c: &&Cell| c.y1 <= ds;
    let north = |c: &&Cell| c.y0 >= yn;
    let clean = |x: Mm, band: &dyn Fn(&&Cell) -> bool| {
        cells.iter().filter(band).any(|c| c.x0 == x || c.x1 == x)
            && !cells.iter().filter(band).any(|c| c.x0 < x && x < c.x1)
    };
    let mut cand: Vec<Mm> = cells
        .iter()
        .flat_map(|c| [c.x0, c.x1])
        .filter(|&x| x > 0 && x < len && clean(x, &south) && clean(x, &north))
        .collect();
    cand.sort_unstable();
    cand.dedup();
    let mut cuts = vec![];
    let mut start = 0;
    while !fits(start, len) {
        let next = cand
            .iter()
            .copied()
            .filter(|&x| x > start && fits(start, x))
            .max()
            .or_else(|| cand.iter().copied().find(|&x| x > start));
        let Some(x) = next else { break };
        cuts.push(x);
        start = x;
    }
    for &x in &cuts {
        if let Some(i) = cells.iter().position(|c| c.kind == RoomKind::Corridor && c.x0 < x && x < c.x1) {
            let mut east = cells[i].clone();
            east.x0 = x;
            cells[i].x1 = x;
            cells.insert(i + 1, east);
        }
    }
    cuts
}

/// Marks the inner walls on each cut line as compartment walls of `fire.wall_rating` and puts
/// a door through the corridor there. Returns the doors added.
pub(super) fn fire_walls(spec: &ProjectSpec, l: &mut GLayout, cuts: &[f64], corridor: (f64, f64)) -> usize {
    let wall = WallFire { sep: FireSep::Compartment, minutes: spec.rules.get("fire.wall_rating").round() as u32 };
    let clear = spec.rules.get("facade.edge_clear");
    let mut added = 0;
    for &x in cuts {
        for w in l.walls.iter_mut() {
            if w.role != WallRole::Exterior && (w.a.0 - x).abs() < ON && (w.b.0 - x).abs() < ON {
                w.fire = Some(wall);
            }
        }
        let mid = (x, (corridor.0 + corridor.1) / 2.0);
        let Some(wi) = (0..l.walls.len()).find(|&i| {
            let w = &l.walls[i];
            w.fire.is_some() && (w.a.0 - x).abs() < ON && seg_dist(mid, w.a, w.b) < ON
        }) else {
            continue;
        };
        let w = &l.walls[wi];
        let along_of = |y: f64| (y - w.a.1).abs();
        let (s0, s1) = (along_of(corridor.0).min(along_of(corridor.1)), along_of(corridor.0).max(along_of(corridor.1)));
        let width = exit_door(spec).min(s1 - s0 - 2.0 * clear);
        if width < spec.rules.get("egress.exit_door_min") - ON {
            continue;
        }
        let along = (s0 + s1 - width) / 2.0;
        l.openings.push(GOpening { wall: wi, along, door: true, width, height: crate::layout::DOOR_H, sill: 0.0, ty: None });
        added += 1;
    }
    added
}

/// Name of the vestibule room between a corridor and a smokeproof stair.
pub const SMOKE_LOBBY: &str = "Smoke lobby";

/// Smoke lobbies of the project as (level, room): the storeys first, then the basements. The
/// hook for pressurisation: each is a closed room with two doors, the one to pressurise.
pub fn smoke_lobbies(p: &GProject) -> Vec<(usize, usize)> {
    p.storeys
        .iter()
        .chain(&p.basements)
        .enumerate()
        .flat_map(|(k, st)| {
            (0..st.layout.rooms.len()).filter(|&r| st.layout.rooms[r].name == SMOKE_LOBBY).map(move |r| (k, r))
        })
        .collect()
}

fn rect(lo: P, hi: P) -> Vec<P> {
    vec![lo, (hi.0, lo.1), hi, (lo.0, hi.1)]
}

/// Stair rooms of a storey with no smoke lobby: no door joins them to one.
fn bare_stairs(st: &PStorey) -> Vec<usize> {
    let doors = door_list(&st.layout);
    (0..st.rooms.len().min(st.layout.rooms.len()))
        .filter(|&r| st.rooms[r].kind == RoomKind::Stair)
        .filter(|&r| {
            !doors.iter().any(|(_, _, rs)| rs.contains(&r) && rs.iter().any(|&q| st.layout.rooms[q].name == SMOKE_LOBBY))
        })
        .collect()
}

/// Cuts a smoke lobby off every stair room of `st` that opens onto circulation: a strip of the
/// stair room along the corridor door, `egress.smoke_lobby_depth` deep and at least
/// `egress.smoke_lobby_area` large, closed by a new wall with its own door, so the way into the
/// stair passes two doors. The strip must leave the flights where they are and hold no other
/// opening. Returns the lobbies built.
fn smoke_lobbies_of(spec: &ProjectSpec, st: &mut PStorey) -> usize {
    let depth = spec.rules.get("egress.smoke_lobby_depth");
    let want_area = spec.rules.get("egress.smoke_lobby_area");
    let mut built = 0;
    for s in bare_stairs(st) {
        let poly = st.layout.rooms[s].polygon.clone();
        let (lo, hi) = bbox(&poly);
        if (area(&poly) - (hi.0 - lo.0) * (hi.1 - lo.1)).abs() > 1e-4 {
            continue;
        }
        let mut foot: Option<(P, P)> = None;
        for gs in &st.layout.stairs {
            let Ok(lay) = gs.def().layout() else { continue };
            let pts: Vec<P> = lay
                .flights
                .iter()
                .flat_map(|f| f.ring(lay.tread))
                .chain(lay.landings.iter().flat_map(|l| l.ring.clone()))
                .map(|v| (v.x, v.y))
                .collect();
            let (a, b) = bbox(&pts);
            if inside(&poly, ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)) {
                foot = Some(match foot {
                    Some((fa, fb)) => ((fa.0.min(a.0), fa.1.min(a.1)), (fb.0.max(b.0), fb.1.max(b.1))),
                    None => (a, b),
                });
            }
        }
        let doors = door_list(&st.layout);
        let tries: Vec<usize> = doors
            .iter()
            .filter(|(_, _, rs)| rs.contains(&s))
            .filter(|(_, _, rs)| {
                rs.iter().any(|&q| {
                    q != s && matches!(st.rooms[q].kind, RoomKind::Corridor | RoomKind::Lobby | RoomKind::Hall)
                })
            })
            .map(|(i, _, _)| *i)
            .collect();
        for di in tries {
            let o = st.layout.openings[di].clone();
            let w = st.layout.walls[o.wall].clone();
            // the side of the stair room the door wall lies on, and the way inward
            let (horizontal, at) = if (w.a.1 - w.b.1).abs() < ON {
                (true, w.a.1)
            } else if (w.a.0 - w.b.0).abs() < ON {
                (false, w.a.0)
            } else {
                continue;
            };
            let (l0, h0) = if horizontal { (lo.1, hi.1) } else { (lo.0, hi.0) };
            let side = if horizontal { hi.0 - lo.0 } else { hi.1 - lo.1 };
            let inward = if (at - l0).abs() < ON {
                1.0
            } else if (at - h0).abs() < ON {
                -1.0
            } else {
                continue;
            };
            let (flo, fhi) = foot.map_or((f64::MAX, f64::MIN), |(a, b)| if horizontal { (a.1, b.1) } else { (a.0, b.0) });
            let clear = 0.1;
            // the wished depth first, then the least depth that still gives the lobby its area
            let least = (want_area / side.max(0.1)).max(1.2);
            let fits = |d: f64| {
                let cut = at + inward * d;
                let free = if foot.is_none() {
                    (h0 - l0) - d >= 1.0
                } else if inward > 0.0 {
                    flo >= cut + clear
                } else {
                    fhi <= cut - clear
                };
                free && cut > l0 + ON && cut < h0 - ON && d * side >= want_area - ON
            };
            let Some(d) = [depth.max(least), least].into_iter().find(|&d| fits(d)) else {
                continue;
            };
            let cut = at + inward * d;
            let (slo, shi, rlo, rhi) = if horizontal {
                if inward > 0.0 {
                    (lo, (hi.0, cut), (lo.0, cut), hi)
                } else {
                    ((lo.0, cut), hi, lo, (hi.0, cut))
                }
            } else if inward > 0.0 {
                (lo, (cut, hi.1), (cut, lo.1), hi)
            } else {
                ((cut, lo.1), hi, lo, (cut, hi.1))
            };
            let blocked = st.layout.openings.iter().enumerate().any(|(i, x)| {
                i != di
                    && x.wall != o.wall
                    && centre(&st.layout, x)
                        .is_some_and(|c| c.0 > slo.0 - ON && c.0 < shi.0 + ON && c.1 > slo.1 - ON && c.1 < shi.1 + ON)
            });
            if blocked {
                continue;
            }
            let (strip, rest) = (rect(slo, shi), rect(rlo, rhi));
            let (n0, n1) = if horizontal { ((lo.0, cut), (hi.0, cut)) } else { ((cut, lo.1), (cut, hi.1)) };
            let wi = st.layout.walls.len();
            st.layout.walls.push(GWall::line(n0, n1, WallRole::Partition));
            let len = dist(n0, n1);
            let wd = o.width.min(len - 0.4).max(0.5);
            st.layout.openings.push(GOpening {
                wall: wi,
                along: (len - wd) / 2.0,
                door: true,
                width: wd,
                height: o.height,
                sill: o.sill,
                ty: None,
            });
            st.layout.rooms[s].polygon = rest;
            st.layout.rooms[s].point = ((rlo.0 + rhi.0) / 2.0, (rlo.1 + rhi.1) / 2.0);
            st.layout.rooms.push(crate::GRoom {
                name: SMOKE_LOBBY.into(),
                apartment: None,
                point: ((slo.0 + shi.0) / 2.0, (slo.1 + shi.1) / 2.0),
                polygon: strip,
            });
            let number = format!("{}L", st.rooms[s].number);
            st.rooms.push(PRoom { kind: RoomKind::Lobby, number, owner: PRole::Public, asked: None, grown: false });
            built += 1;
            break;
        }
    }
    built
}

/// Encloses the stair rooms per `egress.protected_stairs`: `none` leaves them open, `enclosed`
/// rates the walls around every stair room (`fire.stair_rating`, or `fire.stair_rating_tall`
/// from `fire.tall_storeys` storeys on), `smokeproof` always takes the tall rating. A
/// compartment wall keeps its kind and takes the higher rating. `smokeproof` also cuts a smoke
/// lobby with two doors off every stair room ([`smoke_lobbies`]); basements are included.
pub fn protect_stairs(spec: &ProjectSpec, p: &mut GProject) {
    let level = spec.rules.get("egress.protected_stairs").round() as usize;
    if level == 0 && slab_rating(spec).is_none() {
        return;
    }
    if level >= 2 {
        for st in p.storeys.iter_mut().chain(p.basements.iter_mut()) {
            smoke_lobbies_of(spec, st);
        }
    }
    let storeys = p.storeys.len() + p.basements.len();
    let id = if level >= 2 || storeys as f64 >= spec.rules.get("fire.tall_storeys") {
        "fire.stair_rating_tall"
    } else {
        "fire.stair_rating"
    };
    let minutes = spec.rules.get(id).round() as u32;
    if let Some(minutes) = slab_rating(spec) {
        for st in p.storeys.iter_mut().chain(p.basements.iter_mut()) {
            let shafts: Vec<Vec<P>> = st.rooms.iter().zip(&st.layout.rooms).filter(|(m, _)| m.kind == RoomKind::Lift).map(|(_, g)| g.polygon.clone()).collect();
            for w in st.layout.walls.iter_mut() {
                if w.role == WallRole::Exterior || w.bulge != 0.0 {
                    continue;
                }
                let mid = ((w.a.0 + w.b.0) / 2.0, (w.a.1 + w.b.1) / 2.0);
                let near = |q: P| shafts.iter().any(|s| on_outline(s, q));
                if near(mid) && near(w.a) && near(w.b) {
                    w.fire = Some(match w.fire {
                        Some(f) => WallFire { sep: f.sep, minutes: f.minutes.max(minutes) },
                        None => WallFire { sep: FireSep::Compartment, minutes },
                    });
                }
            }
        }
    }
    if level == 0 {
        return;
    }
    for st in p.storeys.iter_mut().chain(p.basements.iter_mut()) {
        let stairs: Vec<Vec<P>> = st
            .rooms
            .iter()
            .zip(&st.layout.rooms)
            .filter(|(m, g)| m.kind == RoomKind::Stair || g.name == SMOKE_LOBBY)
            .map(|(_, g)| g.polygon.clone())
            .collect();
        for w in st.layout.walls.iter_mut() {
            if w.role == WallRole::Exterior || w.bulge != 0.0 {
                continue;
            }
            let mid = ((w.a.0 + w.b.0) / 2.0, (w.a.1 + w.b.1) / 2.0);
            let near = |q: P| stairs.iter().any(|s| on_outline(s, q));
            let encloses = near(mid) && near(w.a) && near(w.b);
            if !encloses {
                continue;
            }
            w.fire = Some(match w.fire {
                Some(f) => WallFire { sep: f.sep, minutes: f.minutes.max(minutes) },
                None => WallFire { sep: FireSep::Stair, minutes },
            });
        }
    }
}

/// The reasons of the fire decisions: exits and occupants, the stairs a bar took, the fire
/// walls and the stair enclosures.
pub fn explain(spec: &ProjectSpec, p: &GProject) -> Vec<Why> {
    let src = |id: &str| super::why::source(spec, id);
    let mut out = vec![];
    let mut why = |target: String, rule: &str, value: serde_json::Value, key: &'static str, args: super::why_text::Args| {
        out.push(Why::new(target, rule, value, src(rule), key, args));
    };
    let pp = spec.rules.get("egress.door_per_person");
    for (k, f) in measure(spec, p).iter().enumerate() {
        if f.people == 0 {
            continue;
        }
        why(
            "egress".into(),
            exits_rule(f.need),
            json!(spec.rules.get(exits_rule(f.need))),
            "fire_exits",
            vec![
                ("k", s(k)),
                ("people", s(f.people)),
                ("n", s(f.need)),
                ("w", s((f64::from(f.people) * pp * 100.0).ceil() / 100.0)),
                ("have", s(f.exits)),
            ],
        );
    }
    let stairs = p.storeys.first().map_or(0, |s| s.layout.stairs.len());
    if stairs > 1 {
        if spec.plate.is_some() {
            let people = measure(spec, p).iter().map(|f| f.people).max().unwrap_or(0);
            why("core".into(), "egress.core_pair", json!(spec.rules.get("egress.core_pair")), "core_pair", vec![("n", s(stairs)), ("people", s(people))]);
        } else {
            why("core".into(), "egress.stairs_max", json!(stairs), "end_stairs", vec![("n", s(stairs))]);
        }
    }
    let walls = |sep: FireSep| {
        p.storeys
            .iter()
            .chain(&p.basements)
            .flat_map(|st| &st.layout.walls)
            .filter(|w| w.fire.is_some_and(|f| f.sep == sep))
            .map(|w| w.fire.map_or(0, |f| f.minutes))
            .collect::<Vec<u32>>()
    };
    let fw = walls(FireSep::Compartment);
    if let Some(&min) = fw.iter().max() {
        let a = spec.rules.get("fire.compartment_area");
        why(
            "fire".into(),
            "fire.wall_rating",
            json!(min),
            "fire_walls",
            vec![("n", s(fw.len())), ("min", s(min)), ("area", s(a))],
        );
    }
    let sw = walls(FireSep::Stair);
    if let Some(&min) = sw.iter().max() {
        let id = if min as f64 >= spec.rules.get("fire.stair_rating_tall") - ON
            && spec.rules.get("fire.stair_rating_tall") > spec.rules.get("fire.stair_rating")
        {
            "fire.stair_rating_tall"
        } else {
            "fire.stair_rating"
        };
        why("fire".into(), id, json!(min), "stair_enclosure", vec![("n", s(sw.len())), ("min", s(min))]);
    }
    out
}

/// The escape stairs run on from the top storey to the roof: `fire.stair_to_roof` in a
/// building of `fire.tall_storeys` storeys or more, or whose top floor stands
/// `fire.tall_height_m` or more above the ground.
/// Whether the top storey stands `egress.two_stairs_above_height` or more above the ground:
/// two protected stairs on every storey and every floor its own rated compartment.
pub fn high_rise(spec: &ProjectSpec) -> bool {
    let top = f64::from(spec.floors.saturating_sub(1)) * spec.floor_height;
    top >= spec.rules.get("egress.two_stairs_above_height") - ON
}

/// Fire resistance of the floor slabs, minutes, in a high-rise.
pub fn slab_rating(spec: &ProjectSpec) -> Option<u32> {
    high_rise(spec).then(|| spec.rules.get("fire.slab_rating_tall").round() as u32)
}

pub fn stairs_to_roof(spec: &ProjectSpec) -> bool {
    let top = f64::from(spec.floors.saturating_sub(1)) * spec.floor_height;
    spec.rules.get("fire.stair_to_roof") >= 0.5
        && (f64::from(spec.floors) >= spec.rules.get("fire.tall_storeys") || top >= spec.rules.get("fire.tall_height_m") - ON)
}

/// A head on the roof round every flight of the top storey when the stairs run on to the roof:
/// the box of its flights and landings in the stair's own axes, rated as a tall stair, the
/// exit door (`egress.exit_door` wide) on the side nearest where the last flight arrives. The
/// head's height, clearance and door margins are the `fire.stair_head.*` rules. A head whose
/// side cannot take a door of `egress.exit_door_min` is refused and reported.
pub(super) fn stair_heads(spec: &ProjectSpec, p: &mut GProject) {
    let Some(top) = p.storeys.last().filter(|_| stairs_to_roof(spec)) else {
        return;
    };
    let least = spec.rules.get("egress.exit_door_min");
    let mut issues = vec![];
    let fire = spec.rules.get("fire.stair_rating_tall") as u32;
    let want = spec.rules.get("egress.exit_door").max(least);
    let clear = spec.rules.get("fire.stair_head.roof_clear");
    let (height, gap, margin) = (
        spec.rules.get("fire.stair_head.height"),
        spec.rules.get("fire.stair_head.gap"),
        spec.rules.get("fire.stair_head.door_margin"),
    );
    let mut out = vec![];
    for (i, g) in top.layout.stairs.iter().enumerate() {
        let Some((ring, end)) = head_box(g, gap) else { continue };
        let edge = (0..4)
            .min_by(|&a, &b| {
                let d = |j: usize| crate::check::dist_to_seg(end, ring[j], ring[(j + 1) % 4]);
                d(a).total_cmp(&d(b))
            })
            .unwrap_or(0);
        let (a, b) = (ring[edge], ring[(edge + 1) % 4]);
        let len = dist(a, b);
        let width = want.min(len - 2.0 * margin).max(0.0);
        if width < least - ON {
            issues.push(format!(
                "stair {i}: its head on the roof has {width:.2} m for the exit door, egress.exit_door_min asks {least:.2} m; no way out on the roof"
            ));
            continue;
        }
        let t = ((end.0 - a.0) * (b.0 - a.0) + (end.1 - a.1) * (b.1 - a.1)) / len.max(ON);
        let along = (t - width / 2.0).clamp(margin, (len - width - margin).max(margin));
        let height = height.max(cap_rise(p, &ring) + clear);
        out.push(GStairHead { stair: i, ring, height, door: (edge, along, width, crate::layout::DOOR_H), fire });
    }
    let room = (super::slots::HEADS_MAX / super::slots::HEAD_SLOTS) as usize;
    if out.len() > room {
        let gone: Vec<String> = out[room..].iter().map(|h| h.stair.to_string()).collect();
        issues.push(format!(
            "stairs {}: the roof holds {room} stair heads, these stairs reach the roof with no head and no way out on it",
            gone.join(", ")
        ));
        out.truncate(room);
    }
    p.fire.stair_heads = out;
    p.fire.head_issues.extend(issues);
}

/// The walls of a head round stair `g`, counter-clockwise: the box of its flights and landings
/// in the stair's own axes grown by `gap`, and the point where its last flight arrives.
fn head_box(g: &crate::GStair, gap: f64) -> Option<(Vec<P>, P)> {
    let st = g.def().layout().ok()?;
    let last = st.flights.last()?;
    let (c, sn, o) = (g.angle.cos(), g.angle.sin(), g.origin);
    let local = |q: P| ((q.0 - o.0) * c + (q.1 - o.1) * sn, -(q.0 - o.0) * sn + (q.1 - o.1) * c);
    let world = |q: P| (o.0 + q.0 * c - q.1 * sn, o.1 + q.0 * sn + q.1 * c);
    let rings = st.flights.iter().flat_map(|f| f.ring(st.tread)).chain(st.landings.iter().flat_map(|l| l.ring.clone()));
    let (mut lo, mut hi) = ((f64::INFINITY, f64::INFINITY), (f64::NEG_INFINITY, f64::NEG_INFINITY));
    for q in rings.map(|q| local((q.x, q.y))) {
        (lo, hi) = ((lo.0.min(q.0), lo.1.min(q.1)), (hi.0.max(q.0), hi.1.max(q.1)));
    }
    let (lo, hi) = ((lo.0 - gap, lo.1 - gap), (hi.0 + gap, hi.1 + gap));
    let ring: Vec<P> = [lo, (hi.0, lo.1), hi, (lo.0, hi.1)].into_iter().map(world).collect();
    let end = last.start + last.dir * (st.tread * last.treads() as f64) + last.right() * (last.width * 0.5);
    Some((ring, (end.x, end.y)))
}

/// Plan boxes (min, max) the roof plant keeps out of: every stair head the stairs will get
/// (`stair_heads`), grown by `fire.stair_head.plant_clear`. Empty while the stairs end on the
/// top storey. Read before the heads are placed, from the top storey's flights.
pub(super) fn head_keepout(spec: &ProjectSpec, p: &GProject) -> Vec<(P, P)> {
    let Some(top) = p.storeys.last().filter(|_| stairs_to_roof(spec)) else {
        return vec![];
    };
    let (gap, clear) = (spec.rules.get("fire.stair_head.gap"), spec.rules.get("fire.stair_head.plant_clear"));
    top.layout
        .stairs
        .iter()
        .filter_map(|g| head_box(g, gap))
        .map(|(r, _)| {
            let lo = r.iter().fold((f64::INFINITY, f64::INFINITY), |a, q| (a.0.min(q.0), a.1.min(q.1)));
            let hi = r.iter().fold((f64::NEG_INFINITY, f64::NEG_INFINITY), |a, q| (a.0.max(q.0), a.1.max(q.1)));
            ((lo.0 - clear, lo.1 - clear), (hi.0 + clear, hi.1 + clear))
        })
        .collect()
}

/// Height of the roof surface over the plan point `q`, above the roof's elevation: a drawn
/// vault by its profile, a pitched roof by the lowest of its sloped edge planes (a vertical
/// gable edge sets none), a flat roof 0.
fn rise_at(r: &PRoof, q: P) -> f64 {
    if let Some(v) = &r.vault {
        let (Some(f), Some(l)) = (v.profile.first(), v.profile.last()) else { return 0.0 };
        let d = (v.b.0 - v.a.0, v.b.1 - v.a.1);
        let len = d.0.hypot(d.1).max(ON);
        let u = (((q.0 - v.a.0) * d.0 + (q.1 - v.a.1) * d.1) / len).clamp(f.0.min(l.0), f.0.max(l.0));
        return v
            .profile
            .windows(2)
            .find(|w| u >= w[0].0.min(w[1].0) - ON && u <= w[0].0.max(w[1].0) + ON)
            .map_or(0.0, |w| {
                let t = if (w[1].0 - w[0].0).abs() < ON { 0.0 } else { (u - w[0].0) / (w[1].0 - w[0].0) };
                w[0].1 + (w[1].1 - w[0].1) * t
            });
    }
    if !super::assembly::pitched(r.slope) {
        return 0.0;
    }
    if let Some((a, b)) = r.eave {
        let len = dist(a, b).max(ON);
        let d = ((b.0 - a.0) * (q.1 - a.1) - (b.1 - a.1) * (q.0 - a.0)) / len;
        return d.max(0.0) * r.slope.to_radians().tan();
    }
    let n = r.ring.len();
    let mut z = f64::INFINITY;
    for j in 0..n {
        let s = if r.edges.is_empty() { Some(r.slope) } else { r.edges.get(j).copied().flatten() };
        let Some(s) = s else { continue };
        let (a, b) = (r.ring[j], r.ring[(j + 1) % n]);
        let len = dist(a, b).max(ON);
        let d = ((b.0 - a.0) * (q.1 - a.1) - (b.1 - a.1) * (q.0 - a.0)).abs() / len;
        z = z.min(d * s.to_radians().tan());
    }
    if z.is_finite() { z } else { 0.0 }
}

/// Plan points over `ring`: its corners and a grid inside it.
fn samples(ring: &[P]) -> Vec<P> {
    let (lo, hi) = ring.iter().fold(((f64::INFINITY, f64::INFINITY), (f64::NEG_INFINITY, f64::NEG_INFINITY)), |a, q| {
        ((a.0 .0.min(q.0), a.0 .1.min(q.1)), (a.1 .0.max(q.0), a.1 .1.max(q.1)))
    });
    let mut out = ring.to_vec();
    for i in 0..=10 {
        for k in 0..=10 {
            let q = (lo.0 + (hi.0 - lo.0) * i as f64 / 10.0, lo.1 + (hi.1 - lo.1) * k as f64 / 10.0);
            if crate::check::inside(ring, q) {
                out.push(q);
            }
        }
    }
    out
}

/// The highest the roof (main or wing) is over the head footprint `ring`, above the roof level.
fn cap_rise(p: &GProject, ring: &[P]) -> f64 {
    let n = ring.len().max(1) as f64;
    let c = ring.iter().fold((0.0, 0.0), |a, q| (a.0 + q.0 / n, a.1 + q.1 / n));
    std::iter::once(&p.roof)
        .chain(&p.extra_roofs)
        .find(|r| crate::check::inside(&r.ring, c))
        .map_or(0.0, |r| samples(ring).into_iter().map(|q| rise_at(r, q)).fold(0.0, f64::max))
}

/// The roof opens over every stair head: the model's roof holds no holes, so the roof the head
/// stands on is parted into the pieces round it (outside one edge of the head, inside the edges
/// before it). The first piece keeps the roof's place, the others follow the wings' roofs. A flat
/// roof parts plainly. A pitched or vaulted roof parts with its old edges keeping their slopes and
/// the cuts left vertical, so the head goes through it on a cap above its highest point; a
/// pitched roof whose pieces would not keep its surface (an edge that held it up is gone from a
/// piece) is not opened: the head is refused and reported.
pub(super) fn open_roof(p: &mut GProject) {
    use ddc_geom::{poly, v2};
    for h in p.fire.stair_heads.clone() {
        let c = h.ring.iter().fold((0.0, 0.0), |a, q| (a.0 + q.0 / h.ring.len() as f64, a.1 + q.1 / h.ring.len() as f64));
        let roofs = std::iter::once(&p.roof).chain(&p.extra_roofs);
        let Some(i) = roofs.clone().position(|r| crate::check::inside(&r.ring, c)) else { continue };
        let r = if i == 0 { p.roof.clone() } else { p.extra_roofs[i - 1].clone() };
        let shaped = super::assembly::pitched(r.slope) || r.vault.is_some();
        let parts: Vec<PRoof> = if shaped && r.vault.is_none() {
            match plane_pieces(&r, &h.ring) {
                Some(v) => v,
                None => {
                    p.fire.head_issues.push(format!(
                        "stair head at ({:.1}, {:.1}): the pitched roof over it cannot be opened into planar pieces; the head is not built",
                        c.0, c.1
                    ));
                    p.fire.stair_heads.retain(|g| g.stair != h.stair);
                    continue;
                }
            }
        } else {
            let ring: Vec<_> = r.ring.iter().map(|q| v2(q.0, q.1)).collect();
            let n = h.ring.len();
            // outward normal of head edge j and its offset: outside is n.p >= c
            let edge = |j: usize| {
                let (a, b) = (h.ring[j], h.ring[(j + 1) % n]);
                let nn = v2(b.1 - a.1, a.0 - b.0);
                (nn, nn.dot(v2(a.0, a.1)))
            };
            let mut pieces = vec![];
            for j in 0..n {
                let (nn, k) = edge(j);
                let mut piece = poly::clip_half(&ring, nn * -1.0, -k);
                for e in 0..j {
                    let (ne, ke) = edge(e);
                    piece = poly::clip_half(&piece, ne, ke);
                }
                if piece.len() >= 3 && poly::area(&piece).abs() > 1e-3 {
                    pieces.push(piece.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>());
                }
            }
            pieces.into_iter().map(|ring| if shaped { cut_piece(&r, ring) } else { PRoof { ring, edges: vec![], eave: None, ..r.clone() } }).collect()
        };
        let mut parts = parts.into_iter();
        let Some(first) = parts.next() else { continue };
        if i == 0 {
            p.roof = first;
        } else {
            p.extra_roofs[i - 1] = first;
        }
        p.extra_roofs.extend(parts);
    }
}

/// A piece of the roof `r` cut to `ring`: each edge that lies on an old edge keeps its slope,
/// a cut edge is a vertical gable.
fn cut_piece(r: &PRoof, ring: Vec<P>) -> PRoof {
    let n = r.ring.len();
    let edges = (0..ring.len())
        .map(|j| {
            let (a, b) = (ring[j], ring[(j + 1) % ring.len()]);
            let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
            (0..n)
                .find(|&k| crate::check::dist_to_seg(mid, r.ring[k], r.ring[(k + 1) % n]) < 1e-6)
                .and_then(|k| if r.edges.is_empty() { Some(r.slope) } else { r.edges.get(k).copied().flatten() })
        })
        .collect();
    PRoof { ring, edges, eave: None, ..r.clone() }
}

/// Smallest strip kept, m2: the area tolerance the cut tests cover the roof with.
const SLIVER: f64 = 1e-6;

/// Area of `r` less `cut` with holes counted out; `None` when the boolean fails.
fn poly_area_minus(r: &[ddc_geom::V2], cut: &[ddc_geom::V2]) -> Option<f64> {
    let parts = ddc_geom::poly::subtract(&[r.to_vec()], &[cut.to_vec()])?;
    Some(parts.iter().map(|q| ddc_geom::poly::area(&q.0).abs() - q.1.iter().map(|h| ddc_geom::poly::area(h).abs()).sum::<f64>()).sum())
}

/// `r` without `cut`, as simple outlines; `None` when the cut leaves a hole or the boolean fails.
fn minus(r: &[ddc_geom::V2], cut: &[ddc_geom::V2]) -> Option<Vec<Vec<ddc_geom::V2>>> {
    let parts = ddc_geom::poly::subtract(&[r.to_vec()], &[cut.to_vec()])?;
    if parts.iter().any(|q| !q.1.is_empty()) {
        return None;
    }
    Some(parts.into_iter().map(|q| q.0).collect())
}

/// A pitched roof parted round the head ring into planar pieces: the roof's own faces (its
/// skeleton, so a concave outline is right too) are each cut by the head polygon itself into
/// strips (below it, above it, and the band either side of it, in the face's own axes), and
/// every strip is a roof of its own on the face's plane, hung on its lowest edge parallel to the
/// eave (`lift` is that edge's height). A strip edge on the old outline keeps the overhang: it
/// stands out by it on the same plane; the cuts stay put and the pieces carry none. `None` when
/// the skeleton fails or a strip has no such edge.
pub fn plane_pieces(r: &PRoof, head: &[(f64, f64)]) -> Option<Vec<PRoof>> {
    use ddc_geom::{poly, roof::Footprint, v2, V2};
    let n = r.ring.len();
    let slopes: Vec<Option<f64>> =
        (0..n).map(|j| if r.edges.is_empty() { Some(r.slope) } else { r.edges.get(j).copied().flatten() }).collect();
    let fp = Footprint { ring: r.ring.iter().map(|q| v2(q.0, q.1)).collect(), slopes, base_z: 0.0, overhang: 0.0, mansard: None };
    let roof = fp.build().ok()?;
    // a head corner within the tolerance of a face's corner or edge (valley, hip, ridge, outline)
    // lies on it, so the cut leaves no sliver along that line
    let tol = ddc_geom::Tol::default().length;
    let segs: Vec<(P, P)> = roof
        .faces
        .iter()
        .filter(|f| f.sloped)
        .flat_map(|f| {
            let k = f.pts.len();
            (0..k).map(move |i| ((f.pts[i].x, f.pts[i].y), (f.pts[(i + 1) % k].x, f.pts[(i + 1) % k].y)))
        })
        .collect();
    let nearest = |q: P, c: &mut dyn Iterator<Item = P>| c.filter(|&x| dist(q, x) < tol).min_by(|x, y| dist(q, *x).total_cmp(&dist(q, *y)));
    let snap = |q: P| {
        nearest(q, &mut segs.iter().map(|s| s.0))
            .or_else(|| {
                nearest(
                    q,
                    &mut segs.iter().filter_map(|&(a, b)| {
                        let l2 = (b.0 - a.0).powi(2) + (b.1 - a.1).powi(2);
                        let t = (((q.0 - a.0) * (b.0 - a.0) + (q.1 - a.1) * (b.1 - a.1)) / l2).clamp(0.0, 1.0);
                        (l2 > ON * ON).then(|| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t))
                    }),
                )
            })
            .unwrap_or(q)
    };
    let head: Vec<P> = head.iter().map(|&q| snap(q)).collect();
    let head = &head[..];
    let head_ring: Vec<V2> = head.iter().map(|q| v2(q.0, q.1)).collect();
    let mut out = vec![];
    for f in roof.faces.iter().filter(|f| f.sloped) {
        let zmin = f.pts.iter().map(|q| q.z).fold(f64::INFINITY, f64::min);
        let low: Vec<P> = f.pts.iter().filter(|q| q.z <= zmin + 1e-6).map(|q| (q.x, q.y)).collect();
        // the eave: the two lowest corners furthest apart
        let (mut a, mut b, mut far) = (low[0], low[0], 0.0);
        for &p in &low {
            for &q in &low {
                if dist(p, q) > far {
                    (a, b, far) = (p, q, dist(p, q));
                }
            }
        }
        if far < ON {
            continue;
        }
        let d = ((b.0 - a.0) / far, (b.1 - a.1) / far);
        let top = f.pts.iter().fold(f.pts[0], |m, q| if q.z > m.z { *q } else { m });
        let mut nn = (-d.1, d.0);
        if (top.x - a.0) * nn.0 + (top.y - a.1) * nn.1 < 0.0 {
            nn = (-nn.0, -nn.1);
        }
        let rise = (top.x - a.0) * nn.0 + (top.y - a.1) * nn.1;
        if rise < ON {
            continue;
        }
        let (t, s) = ((top.z - zmin) / rise, ((top.z - zmin) / rise).atan().to_degrees());
        let uv = |q: P| (d.0 * (q.0 - a.0) + d.1 * (q.1 - a.1), nn.0 * (q.0 - a.0) + nn.1 * (q.1 - a.1));
        let w = |u: f64, v: f64| v2(a.0 + d.0 * u + nn.0 * v, a.1 + d.1 * u + nn.1 * v);
        let mut face: Vec<V2> = f.pts.iter().map(|q| v2(q.x, q.y)).collect();
        if poly::area(&face) < 0.0 {
            face.reverse();
        }
        let (vlo, vhi) = head.iter().map(|&q| uv(q).1).fold((f64::INFINITY, f64::NEG_INFINITY), |m, v| (m.0.min(v), m.1.max(v)));
        let (ulo, uhi) = head.iter().map(|&q| uv(q).0).fold((f64::INFINITY, f64::NEG_INFINITY), |m, v| (m.0.min(v), m.1.max(v)));
        let big = 1e4;
        let above = |v0: f64| vec![w(-big, v0), w(big, v0), w(big, big), w(-big, big)];
        let below = |v0: f64| vec![w(-big, -big), w(big, -big), w(big, v0), w(-big, v0)];
        let right_of = |u0: f64| vec![w(u0, -big), w(big, -big), w(big, big), w(u0, big)];
        let left_of = |u0: f64| vec![w(-big, -big), w(u0, -big), w(u0, big), w(-big, big)];
        // the face less the head, holes counted out: a head inside the face is no failure
        let kept = poly_area_minus(&face, &head_ring)?;
        let hit = poly::area(&face).abs() - kept;
        // a head monotone across the slope parts the band by its own left and right chains:
        // each side piece then keeps the band's bottom edge, parallel to the eave
        let chains = side_chains(&head.iter().map(|&q| uv(q)).collect::<Vec<_>>()).map(|(lc, rc)| {
            let (l0, lk, r0, rk) = (lc[0], lc[lc.len() - 1], rc[0], rc[rc.len() - 1]);
            let mut cut_r = vec![w(l0.0, -big), w(big, -big), w(big, big), w(lk.0, big)];
            cut_r.extend(lc.iter().rev().map(|q| w(q.0, q.1)));
            let mut cut_l = vec![w(-big, -big), w(r0.0, -big)];
            cut_l.extend(rc.iter().map(|q| w(q.0, q.1)));
            cut_l.extend([w(rk.0, big), w(-big, big)]);
            (cut_r, cut_l)
        });
        let strips: Vec<Vec<V2>> = if hit < SLIVER {
            vec![face.clone()]
        } else {
            // below, above, then the band across the head's v-range: left and right of the
            // head's chains, or else left, right and middle slices (the middle is the head's
            // box, so cutting the head out leaves no hole)
            let mut v = minus(&face, &above(vlo))?;
            v.extend(minus(&face, &below(vhi))?);
            for band in minus(&face, &above(vhi))? {
                for m in minus(&band, &below(vlo))? {
                    if let Some((cut_r, cut_l)) = &chains {
                        v.extend(minus(&m, cut_r)?);
                        v.extend(minus(&m, cut_l)?);
                        continue;
                    }
                    v.extend(minus(&m, &right_of(ulo))?);
                    v.extend(minus(&m, &left_of(uhi))?);
                    for a in minus(&m, &left_of(ulo))? {
                        for b in minus(&a, &right_of(uhi))? {
                            v.extend(minus(&b, &head_ring)?);
                        }
                    }
                }
            }
            v
        };
        for strip in strips.into_iter().filter(|q| q.len() >= 3 && poly::area(q).abs() > SLIVER) {
            let mut pts: Vec<P> = strip.iter().map(|q| (q.x, q.y)).collect();
            if poly::area(&strip) < 0.0 {
                pts.reverse();
            }
            let vmin = pts.iter().map(|&q| uv(q).1).fold(f64::INFINITY, f64::min);
            let m = pts.len();
            let edges: Vec<Option<f64>> = (0..m)
                .map(|e| ((uv(pts[e]).1 - vmin).abs() < 1e-5 && (uv(pts[(e + 1) % m]).1 - vmin).abs() < 1e-5).then_some(s))
                .collect();
            let outer: Vec<bool> = (0..m)
                .map(|e| (0..n).any(|k| on_seg(pts[e], r.ring[k], r.ring[(k + 1) % n]) && on_seg(pts[(e + 1) % m], r.ring[k], r.ring[(k + 1) % n])))
                .collect();
            let eave_out = (0..m).any(|e| edges[e].is_some() && outer[e]);
            let ring = offset_edges(&pts, &outer, r.overhang, 1.0);
            if edges.iter().all(|e| e.is_none()) {
                // the strip meets its eave line at a corner only (cut through a valley or hip
                // corner): it hangs on that line, off its ring, and every edge is a gable
                let low = ring.iter().copied().fold(ring[0], |m, q| if uv(q).1 < uv(m).1 { q } else { m });
                let eave = Some((low, (low.0 + nn.1, low.1 - nn.0)));
                let k = ring.len();
                out.push(PRoof { ring, edges: vec![None; k], overhang: 0.0, slope: s, lift: r.lift + uv(low).1 * t, eave, ..r.clone() });
                continue;
            }
            let lift = r.lift + (vmin - if eave_out { r.overhang } else { 0.0 }) * t;
            out.push(PRoof { ring, edges, overhang: 0.0, lift, eave: None, ..r.clone() });
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// The left and right chains of the ring `hv` given in (u, v), each from its lowest corner to
/// its highest; `None` when a chain turns back down (the ring is not monotone in v).
fn side_chains(hv: &[P]) -> Option<(Vec<P>, Vec<P>)> {
    let k = hv.len();
    if k < 3 {
        return None;
    }
    let mut r = hv.to_vec();
    let a2: f64 = (0..k).map(|i| r[i].0 * r[(i + 1) % k].1 - r[(i + 1) % k].0 * r[i].1).sum();
    if a2 < 0.0 {
        r.reverse();
    }
    let vmin = r.iter().map(|q| q.1).fold(f64::INFINITY, f64::min);
    let vmax = r.iter().map(|q| q.1).fold(f64::NEG_INFINITY, f64::max);
    if vmax - vmin < ON {
        return None;
    }
    // the corner at height `at` furthest along `sg` (-1 leftmost, 1 rightmost)
    let ext = |at: f64, sg: f64| (0..k).filter(|&i| (r[i].1 - at).abs() < 1e-9).max_by(|&i, &j| (sg * r[i].0).total_cmp(&(sg * r[j].0)));
    let (lo_l, lo_r, hi_l, hi_r) = (ext(vmin, -1.0)?, ext(vmin, 1.0)?, ext(vmax, -1.0)?, ext(vmax, 1.0)?);
    // counter-clockwise: up the right chain, down the left one
    let walk = |from: usize, to: usize| {
        let (mut c, mut i) = (vec![r[from]], from);
        while i != to {
            i = (i + 1) % k;
            c.push(r[i]);
        }
        c
    };
    let right = walk(lo_r, hi_r);
    let mut left = walk(hi_l, lo_l);
    left.reverse();
    let up = |c: &[P]| c.windows(2).all(|p| p[1].1 >= p[0].1 - 1e-9);
    (up(&left) && up(&right)).then_some((left, right))
}

fn on_seg(q: P, a: P, b: P) -> bool {
    crate::check::dist_to_seg(q, a, b) < 1e-6
}

/// The polygon `pts` (counter-clockwise when `sg` is 1) with the edges flagged in `shift` moved
/// outward by `by`, the corners where the moved lines meet.
fn offset_edges(pts: &[P], shift: &[bool], by: f64, sg: f64) -> Vec<P> {
    let m = pts.len();
    let line = |e: usize| {
        let (a, b) = (pts[e], pts[(e + 1) % m]);
        let len = dist(a, b).max(ON);
        let d = ((b.0 - a.0) / len, (b.1 - a.1) / len);
        let out = (d.1 * sg, -d.0 * sg);
        let o = if shift[e] { by } else { 0.0 };
        ((a.0 + out.0 * o, a.1 + out.1 * o), d)
    };
    (0..m)
        .map(|i| {
            let (p0, d0) = line((i + m - 1) % m);
            let (p1, d1) = line(i);
            let cross = d0.0 * d1.1 - d0.1 * d1.0;
            if cross.abs() < 1e-9 {
                return p1;
            }
            let s = ((p1.0 - p0.0) * d1.1 - (p1.1 - p0.1) * d1.0) / cross;
            (p0.0 + d0.0 * s, p0.1 + d0.1 * s)
        })
        .collect()
}

#[cfg(test)]
mod roof_cut_tests {
    use super::*;
    use ddc_geom::{poly, roof::Footprint, v2};

    fn roof(ring: Vec<P>, edges: Vec<Option<f64>>, slope: f64, overhang: f64) -> PRoof {
        PRoof { name: String::new(), elevation: 0.0, ring, slope, overhang, edges, vault: None, lift: 0.0, eave: None }
    }

    /// The pieces of `r` cut by `head` tile the roof minus the head exactly, and each stands on the
    /// roof's own surface (with the overhang off, so the outline is the old one).
    fn check_cut(r: &PRoof, head: &[P]) -> Vec<PRoof> {
        let bare = PRoof { overhang: 0.0, ..r.clone() };
        let parts = plane_pieces(&bare, head).expect("the roof opens");
        let v = |q: &[P]| q.iter().map(|x| v2(x.0, x.1)).collect::<Vec<_>>();
        let want = poly::area(&v(&r.ring)).abs() - poly::area(&v(head)).abs();
        let got: f64 = parts.iter().map(|q| poly::area(&v(&q.ring)).abs()).sum();
        assert!((want - got).abs() < 1e-6, "the pieces cover {got} m2, the roof less the head is {want}");
        let n = bare.ring.len();
        let slopes = (0..n).map(|j| if bare.edges.is_empty() { Some(bare.slope) } else { bare.edges[j] }).collect();
        let surface = Footprint { ring: v(&bare.ring), slopes, base_z: 0.0, overhang: 0.0, mansard: None }.build().unwrap();
        for q in &parts {
            for x in samples(&q.ring) {
                if crate::check::inside(head, x) || !crate::check::inside(&bare.ring, x) {
                    continue;
                }
                let want = surface.top_at(v2(x.0, x.1)).unwrap();
                assert!((q.lift + rise_at(q, x) - want).abs() < 1e-6, "piece {:?} off the roof at {x:?}", q.ring);
            }
        }
        for q in &parts {
            let c = q.ring.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / q.ring.len() as f64, a.1 + p.1 / q.ring.len() as f64));
            let hc = head.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / head.len() as f64, a.1 + p.1 / head.len() as f64));
            assert!(!crate::check::inside(&q.ring, hc) || crate::check::inside(&q.ring, c) && !crate::check::inside(head, c), "a piece over the head");
        }
        parts
    }

    /// A hip roof over an L (a concave outline): faces come from the skeleton, no spurious edges.
    #[test]
    fn an_l_shaped_hip_roof_opens_over_a_head() {
        let ring = vec![(0.0, 0.0), (20.0, 0.0), (20.0, 8.0), (8.0, 8.0), (8.0, 20.0), (0.0, 20.0)];
        let r = roof(ring, vec![], 30.0, 0.5);
        for head in [
            vec![(2.0, 2.0), (5.0, 2.0), (5.0, 5.0), (2.0, 5.0)],
            vec![(2.0, 12.0), (5.0, 12.0), (5.0, 16.0), (2.0, 16.0)],
            vec![(6.0, 3.0), (10.0, 3.0), (10.0, 6.0), (6.0, 6.0)],
        ] {
            check_cut(&r, &head);
        }
        let with = plane_pieces(&r, &[(2.0, 2.0), (5.0, 2.0), (5.0, 5.0), (2.0, 5.0)]).unwrap();
        assert!(with.iter().all(|q| q.overhang == 0.0 && q.edges.iter().any(|e| e.is_some())));
    }

    /// A head corner on the valley, or within the tolerance either side of it, opens the roof all
    /// the same: the corner counts as on the line, the face's leftover corner hangs on its own
    /// eave line off its ring, and no sliver is left along the valley.
    #[test]
    fn a_head_corner_at_a_valley_within_tolerance_opens_without_slivers() {
        let ring = vec![(0.0, 0.0), (20.0, 0.0), (20.0, 8.0), (8.0, 8.0), (8.0, 20.0), (0.0, 20.0)];
        let r = roof(ring.clone(), vec![], 30.0, 0.0);
        let tol = ddc_geom::Tol::default().length;
        let v = |q: &[P]| q.iter().map(|x| v2(x.0, x.1)).collect::<Vec<_>>();
        let per = |q: &[P]| (0..q.len()).map(|i| dist(q[i], q[(i + 1) % q.len()])).sum::<f64>();
        let surface = Footprint { ring: v(&ring), slopes: vec![Some(30.0); 6], base_z: 0.0, overhang: 0.0, mansard: None }.build().unwrap();
        for off in [0.0, tol / 2.0, -tol / 2.0] {
            // (6, 6) on the valley y = x, moved across it by `off`
            let k = off / 2f64.sqrt();
            let head = vec![(6.0, 3.0), (10.0, 3.0), (10.0, 6.0), (6.0 + k, 6.0 - k)];
            let parts = plane_pieces(&r, &head).unwrap_or_else(|| panic!("the roof opens with the corner {off} m off the valley"));
            let want = poly::area(&v(&ring)).abs() - poly::area(&v(&head)).abs();
            let got: f64 = parts.iter().map(|q| poly::area(&v(&q.ring)).abs()).sum();
            assert!((want - got).abs() <= tol * per(&head), "off {off}: the pieces cover {got} m2, the roof less the head is {want}");
            assert!(parts.iter().any(|q| q.eave.is_some()), "off {off}: the valley corner hangs on its own eave line");
            for q in &parts {
                let a = poly::area(&v(&q.ring)).abs();
                assert!(2.0 * a / per(&q.ring) > tol, "off {off}: a sliver piece {:?}", q.ring);
                for x in samples(&q.ring) {
                    if crate::check::inside(&head, x) || !crate::check::inside(&ring, x) {
                        continue;
                    }
                    let want = surface.top_at(v2(x.0, x.1)).unwrap();
                    assert!((q.lift + rise_at(q, x) - want).abs() < 1e-6, "off {off}: piece {:?} off the roof at {x:?}", q.ring);
                }
            }
        }
    }

    /// A head wholly inside one face of a gable roof: left, right and middle pieces, no hole.
    #[test]
    fn a_head_inside_one_gable_face_leaves_pieces_all_round() {
        let ring = vec![(0.0, 0.0), (30.0, 0.0), (30.0, 20.0), (0.0, 20.0)];
        let r = roof(ring, vec![Some(25.0), None, Some(25.0), None], 25.0, 0.5);
        let head = vec![(12.0, 3.0), (18.0, 3.0), (18.0, 7.0), (12.0, 7.0)];
        let parts = check_cut(&r, &head);
        assert!(parts.len() >= 4, "south face opened round the head: {} pieces", parts.len());
    }

    /// A head turned against the slope opens the roof by its own outline, not by its bounding box.
    #[test]
    fn a_head_turned_against_the_slope_cuts_by_its_outline() {
        let ring = vec![(0.0, 0.0), (30.0, 0.0), (30.0, 20.0), (0.0, 20.0)];
        let r = roof(ring, vec![Some(25.0), None, Some(25.0), None], 25.0, 0.5);
        let (c, s) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
        let head: Vec<P> = [(-3.0, -2.0), (3.0, -2.0), (3.0, 2.0), (-3.0, 2.0)].iter().map(|q| (15.0 + q.0 * c - q.1 * s, 7.0 + q.0 * s + q.1 * c)).collect();
        check_cut(&r, &head);
    }
}
