// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Typed openings. The layout makes every door and window a hole with a size; this pass, run
//! once on the finished project, gives each one a type of the catalogue (`data/opening_types.json`),
//! a mark, a leaf swing and its ratings. Every choice comes from a rule of the `opening` group:
//! the fire class from the wall the opening sits in, the swing from the escape direction, the
//! clear width from the accessible route, the sound and thermal classes from the exterior.
//! Marks are one per distinct type (catalogue type and size), the same on every opening of it, so
//! the schedules list types with counts.

use super::rules::RuleSet;
use super::why::s;
use super::why_text::Args;
use super::{GProject, PRole, PStorey, ProjectSpec, RoomKind};
use crate::{GLayout, GOpening, GOpeningType, GSwing, WallRole};
use serde_json::Value as J;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const DATA: &str = include_str!("../../data/opening_types.json");
pub const SCHEMA: &str = "gen_openings.v1";
/// Distance from the wall axis at which the rooms either side of an opening are read, m.
const PROBE: f64 = 0.15;
/// Float tolerance on widths and ratings.
const TOL: f64 = 1e-9;
/// Millimetres in a metre: sizes key a type to the millimetre.
const MM: f64 = 1000.0;
/// Functions an opening serves; a catalogue type lists those it fits.
pub const FUNCTIONS: [&str; 10] =
    ["room", "wet", "unit", "stair", "shaft", "lift", "compartment", "exit", "exterior", "window"];

/// One type of the catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct CatType {
    pub id: String,
    pub door: bool,
    pub name: String,
    pub uses: Vec<String>,
    pub leaves: u8,
    pub operation: &'static str,
    pub min_w: f64,
    pub max_w: f64,
    pub fire: u32,
    pub rw: u32,
    pub u: f64,
    pub threshold: f64,
}

fn operation(s: &str) -> Option<&'static str> {
    ["swing", "sliding", "casement", "fixed"].into_iter().find(|o| *o == s)
}

fn parse(text: &str) -> Result<Vec<CatType>, String> {
    let v: J = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if v["schema"] != SCHEMA {
        return Err(format!("schema must be {SCHEMA}"));
    }
    let list = v["types"].as_array().ok_or("types must be a list")?;
    let mut out: Vec<CatType> = vec![];
    for t in list {
        let id = t["id"].as_str().ok_or("a type without id")?.to_string();
        let num = |k: &str| t[k].as_f64().ok_or(format!("{id}: no {k}"));
        let txt = |k: &str| t[k].as_str().map(str::to_string).ok_or(format!("{id}: no {k}"));
        let door = match txt("kind")?.as_str() {
            "door" => true,
            "window" => false,
            _ => return Err(format!("{id}: kind must be door or window")),
        };
        let uses: Vec<String> = t["uses"].as_array().into_iter().flatten().filter_map(|u| u.as_str().map(str::to_string)).collect();
        if uses.is_empty() || uses.iter().any(|u| !FUNCTIONS.contains(&u.as_str())) {
            return Err(format!("{id}: uses must name functions of {FUNCTIONS:?}"));
        }
        let op = operation(&txt("operation")?).ok_or(format!("{id}: unknown operation"))?;
        let c = CatType {
            name: txt("name")?,
            door,
            uses,
            leaves: num("leaves")? as u8,
            operation: op,
            min_w: num("min_w")?,
            max_w: num("max_w")?,
            fire: num("fire")? as u32,
            rw: num("rw")? as u32,
            u: num("u")?,
            threshold: num("threshold")?,
            id,
        };
        if c.min_w <= 0.0 || c.min_w > c.max_w {
            return Err(format!("{}: width range", c.id));
        }
        if out.iter().any(|o| o.id == c.id) {
            return Err(format!("{}: twice", c.id));
        }
        out.push(c);
    }
    Ok(out)
}

/// The built-in catalogue, in file order.
pub fn catalogue() -> &'static [CatType] {
    static C: OnceLock<Vec<CatType>> = OnceLock::new();
    C.get_or_init(|| parse(DATA).unwrap_or_else(|e| panic!("opening_types.json: {e}")))
}

/// What the rules ask of the openings.
struct Cfg {
    egress: usize,
    out_area: f64,
    corridor_clear: f64,
    loss: f64,
    clear_access: f64,
    clear_exit: f64,
    rw_unit: u32,
    rw_ext: u32,
    u_max: f64,
    edge: f64,
    fire: [(&'static str, u32); 4],
}

impl Cfg {
    fn read(rs: &RuleSet) -> Cfg {
        let n = |id: &str| rs.get(id);
        Cfg {
            egress: n("opening.egress_swing") as usize,
            out_area: n("opening.out_swing_area"),
            corridor_clear: n("opening.corridor_clear"),
            loss: n("opening.leaf_loss"),
            clear_access: n("opening.clear_access"),
            clear_exit: n("opening.clear_exit"),
            rw_unit: n("opening.rw_unit") as u32,
            rw_ext: n("opening.rw_exterior") as u32,
            u_max: n("opening.u_max"),
            edge: n("facade.edge_clear"),
            fire: [
                ("stair", n("opening.fire_stair") as u32),
                ("unit", n("opening.fire_unit") as u32),
                ("compartment", n("opening.fire_compartment") as u32),
                ("shaft", n("opening.fire_shaft") as u32),
            ],
        }
    }

    /// EI minutes the wall of a function carries, and the rule behind it.
    fn fire_of(&self, function: &str) -> (u32, Option<&'static str>) {
        match function {
            "stair" => (self.fire[0].1, Some("opening.fire_stair")),
            "unit" => (self.fire[1].1, Some("opening.fire_unit")),
            "compartment" => (self.fire[2].1, Some("opening.fire_compartment")),
            "shaft" | "lift" => (self.fire[3].1, Some("opening.fire_shaft")),
            _ => (0, None),
        }
    }
}

type P = (f64, f64);

fn inside(poly: &[P], p: P) -> bool {
    let n = poly.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

fn room_at(l: &GLayout, p: P) -> Option<usize> {
    l.rooms.iter().position(|r| inside(&r.polygon, p))
}

/// The rooms to the left and to the right of the wall axis a -> b at the opening's centre.
fn sides(l: &GLayout, o: &GOpening) -> (Option<usize>, Option<usize>) {
    let Some(w) = l.walls.get(o.wall) else {
        return (None, None);
    };
    let t = o.along + o.width / 2.0;
    let (c, u) = (w.at(t), w.tangent(t));
    let n = (-u.1, u.0);
    let at = |k: f64| room_at(l, (c.0 + n.0 * k * PROBE, c.1 + n.1 * k * PROBE));
    (at(1.0), at(-1.0))
}

fn kind(st: &PStorey, r: Option<usize>) -> Option<RoomKind> {
    r.and_then(|r| st.rooms.get(r)).map(|m| m.kind)
}

fn owner(st: &PStorey, r: Option<usize>) -> Option<PRole> {
    r.and_then(|r| st.rooms.get(r)).map(|m| m.owner)
}

fn circulation(k: Option<RoomKind>) -> bool {
    matches!(k, Some(RoomKind::Corridor | RoomKind::Hall | RoomKind::Lobby))
}

/// What an opening does: the function the rules key on.
fn function_of(st: &PStorey, o: &GOpening, sd: (Option<usize>, Option<usize>), ground: bool) -> &'static str {
    let l = &st.layout;
    let role = l.walls.get(o.wall).map(|w| w.role);
    if !o.door {
        return "window";
    }
    if role == Some(WallRole::Exterior) {
        return if ground { "exit" } else { "exterior" };
    }
    let kinds = [kind(st, sd.0), kind(st, sd.1)];
    let any = |k: RoomKind| kinds.contains(&Some(k));
    if any(RoomKind::Stair) {
        return "stair";
    }
    if any(RoomKind::Lift) {
        return "lift";
    }
    if any(RoomKind::Technical) {
        return "shaft";
    }
    match (owner(st, sd.0), owner(st, sd.1)) {
        (Some(PRole::Apartment(a)), Some(PRole::Apartment(b))) if a != b => return "compartment",
        (Some(PRole::Apartment(_)), Some(PRole::Public)) | (Some(PRole::Public), Some(PRole::Apartment(_))) => {
            return "unit";
        }
        _ => {}
    }
    if role == Some(WallRole::Party) {
        return "compartment";
    }
    if any(RoomKind::Bath) || any(RoomKind::Wc) {
        return "wet";
    }
    "room"
}

/// Everything decided about an opening before its size is final.
struct Draft {
    function: &'static str,
    wall_fire: u32,
    need_clear: f64,
    accessible: bool,
    exterior: bool,
    /// Side the leaf swings into: false left of the axis a -> b, true right.
    into: bool,
    exit: bool,
    rules: Vec<&'static str>,
}

fn decide(cfg: &Cfg, st: &PStorey, ground: bool, i: usize) -> Draft {
    let l = &st.layout;
    let o = &l.openings[i];
    let sd = sides(l, o);
    let function = function_of(st, o, sd, ground);
    let exterior = l.walls.get(o.wall).is_some_and(|w| w.role == WallRole::Exterior);
    let mut rules: Vec<&'static str> = vec!["opening.leaf_loss"];
    let (wall_fire, fire_rule) = cfg.fire_of(function);
    if let Some(r) = fire_rule.filter(|_| wall_fire > 0) {
        rules.push(r);
    }
    // accessible route: entrances, flat doors, lifts and the doors of public rooms
    let apartment = [owner(st, sd.0), owner(st, sd.1)].iter().any(|r| matches!(r, Some(PRole::Apartment(_))));
    let accessible = o.door
        && match function {
            "exit" | "unit" | "lift" => true,
            "room" | "wet" => !apartment,
            _ => false,
        };
    let access_need = if accessible { cfg.clear_access } else { 0.0 };
    let exit_need = if matches!(function, "exit" | "stair") { cfg.clear_exit } else { 0.0 };
    let need_clear = access_need.max(exit_need);
    if need_clear > 0.0 {
        rules.push(if exit_need >= access_need { "opening.clear_exit" } else { "opening.clear_access" });
    }
    // default: into the room that is served, never into a corridor or hall
    let weight = |r: Option<usize>| -> (u8, u8, f64) {
        match r.and_then(|r| st.rooms.get(r).map(|m| (r, m.kind))) {
            None => (2, 0, 0.0),
            Some((r, k)) => (0, u8::from(circulation(Some(k))), super::nav::area(&l.rooms[r].polygon)),
        }
    };
    let (wl, wr) = (weight(sd.0), weight(sd.1));
    let mut into = wr.partial_cmp(&wl).is_some_and(|c| c.is_lt());
    let mut exit = false;
    if o.door && cfg.egress >= 1 {
        let kl = kind(st, sd.0);
        match function {
            "exit" => {
                // out of the building, to the side with no room
                if sd.0.is_none() != sd.1.is_none() {
                    into = sd.0.is_some();
                    exit = true;
                    rules.push("opening.egress_swing");
                }
            }
            "stair" => {
                // above the ground floor the stair is the exit: into it; on the ground floor
                // the way out leads from it
                let stair_left = kl == Some(RoomKind::Stair);
                into = if ground { stair_left } else { !stair_left };
                exit = true;
                rules.push("opening.egress_swing");
            }
            "room" | "wet" | "compartment" if cfg.egress >= 2 => {
                // a large public room opens towards the route, if the corridor keeps its width
                let served = if circulation(kl) { sd.1 } else { sd.0 };
                let route = if circulation(kl) { sd.0 } else { sd.1 };
                let public = matches!(owner(st, served), Some(PRole::Public));
                let big = served.is_some_and(|r| super::nav::area(&l.rooms[r].polygon) >= cfg.out_area - TOL);
                if public && big && circulation(kind(st, route)) && !circulation(kind(st, served)) {
                    let corridor_room = route.filter(|&r| kind(st, Some(r)) == Some(RoomKind::Corridor));
                    let left_free = corridor_room.map(|r| super::nav::strip_width(&l.rooms[r].polygon) - o.width);
                    if left_free.is_some_and(|f| f < cfg.corridor_clear - TOL) {
                        rules.push("opening.corridor_clear");
                    } else {
                        into = route != sd.0;
                        exit = true;
                        rules.push("opening.out_swing_area");
                    }
                }
            }
            _ => {}
        }
    }
    Draft { function, wall_fire, need_clear, accessible, exterior, into, exit, rules }
}

/// A width of `want` about the centre of opening `i` where the wall and its neighbours leave
/// room: `(along, width)`; `None` when they do not.
fn widen(l: &GLayout, i: usize, want: f64, edge: f64) -> Option<(f64, f64)> {
    let o = &l.openings[i];
    let len = l.walls.get(o.wall)?.length();
    let c = o.along + o.width / 2.0;
    let mut lo_free = edge;
    let mut hi_free = len - edge;
    for (j, q) in l.openings.iter().enumerate() {
        if j == i || q.wall != o.wall {
            continue;
        }
        let (a, b) = (q.along, q.along + q.width);
        if b <= c {
            lo_free = lo_free.max(b + edge);
        } else if a >= c {
            hi_free = hi_free.min(a - edge);
        } else {
            return None;
        }
    }
    if hi_free - lo_free < want - TOL {
        return None;
    }
    let along = (c - want / 2.0).clamp(lo_free, hi_free - want);
    Some((along, want))
}

/// What the catalogue must give a door or window.
struct Need {
    fire: u32,
    rw: u32,
    u: Option<f64>,
}

/// The first type that serves the function at this width and meets the need. When none does,
/// the need gives way one step at a time: sound, then thermal, then fire, then the width.
fn choose(door: bool, function: &str, width: f64, need: &Need) -> usize {
    let cat = catalogue();
    let uses: Vec<usize> = (0..cat.len()).filter(|&i| cat[i].door == door && cat[i].uses.iter().any(|u| u == function)).collect();
    let fits = |c: &CatType| width >= c.min_w - TOL && width <= c.max_w + TOL;
    let fire_ok = |c: &CatType| c.fire >= need.fire;
    let rw_ok = |c: &CatType| c.rw >= need.rw;
    let u_ok = |c: &CatType| need.u.is_none_or(|u| c.u > 0.0 && c.u <= u + TOL);
    for level in 0..3 {
        let hit = uses.iter().copied().find(|&i| {
            let c = &cat[i];
            fits(c) && fire_ok(c) && (level >= 1 || rw_ok(c)) && (level >= 2 || u_ok(c))
        });
        if let Some(i) = hit {
            return i;
        }
    }
    let by_fire = uses.iter().copied().filter(|&i| fits(&cat[i])).max_by_key(|&i| cat[i].fire);
    let near = |i: &usize| {
        let c = &cat[*i];
        ((c.min_w - width).max(width - c.max_w)).max(0.0)
    };
    by_fire
        .or_else(|| uses.iter().copied().min_by(|a, b| near(a).total_cmp(&near(b))))
        .or_else(|| (0..cat.len()).find(|&i| cat[i].door == door))
        .unwrap_or(0)
}

fn type_storey(cfg: &Cfg, st: &mut PStorey, ground: bool) {
    let n = st.layout.openings.len();
    let drafts: Vec<Draft> = (0..n).map(|i| decide(cfg, st, ground, i)).collect();
    for (i, d) in drafts.into_iter().enumerate() {
        let mut widened = false;
        let o = &st.layout.openings[i];
        if o.door && d.need_clear > 0.0 && o.width - cfg.loss < d.need_clear - TOL {
            if let Some((along, width)) = widen(&st.layout, i, d.need_clear + cfg.loss, cfg.edge) {
                let o = &mut st.layout.openings[i];
                (o.along, o.width) = (along, width);
                widened = true;
            }
        }
        let wall_len = st.layout.walls.get(st.layout.openings[i].wall).map_or(0.0, |w| w.length());
        let mut rules = d.rules;
        let o = &mut st.layout.openings[i];
        let ext = d.exterior || matches!(d.function, "exit" | "exterior");
        let need = Need {
            fire: d.wall_fire,
            rw: match d.function {
                "unit" => cfg.rw_unit,
                _ if ext => cfg.rw_ext,
                _ => 0,
            },
            u: ext.then_some(cfg.u_max),
        };
        if need.rw > 0 {
            rules.push(if d.function == "unit" { "opening.rw_unit" } else { "opening.rw_exterior" });
        }
        if need.u.is_some() {
            rules.push("opening.u_max");
        }
        let c = &catalogue()[choose(o.door, d.function, o.width, &need)];
        // the leaf hangs at the end of the opening nearer the wall end, so it folds flat
        let hinge = o.along > wall_len - o.along - o.width;
        let sliding = c.operation != "swing" && c.operation != "casement";
        let swing = if sliding { GSwing { side: d.into, hinge: false } } else { GSwing { side: d.into, hinge } };
        o.ty = Some(GOpeningType {
            catalogue: c.id.clone(),
            name: c.name.clone(),
            mark: String::new(),
            function: d.function,
            leaves: c.leaves,
            operation: c.operation,
            swing,
            fire: c.fire,
            wall_fire: d.wall_fire,
            rw: c.rw,
            u: c.u,
            clear: o.width - cfg.loss,
            threshold: c.threshold,
            exit: d.exit,
            accessible: d.accessible,
            widened,
            rules,
        });
    }
}

/// Sizes of a type to the millimetre: the key of a mark.
fn key(catalogue: usize, o: &GOpening) -> (usize, i64, i64, i64) {
    let mm = |x: f64| (x * MM).round() as i64;
    (catalogue, mm(o.width), mm(o.height), mm(o.sill))
}

fn cat_index(id: &str) -> usize {
    catalogue().iter().position(|c| c.id == id).unwrap_or(usize::MAX)
}

/// Marks: `D01`, `W01`, ... one per distinct type in catalogue order and size, so the same
/// project always gets the same marks whatever storey is read first.
fn number(p: &mut GProject) {
    let mut keys: BTreeMap<(usize, i64, i64, i64), String> = BTreeMap::new();
    let all = |p: &GProject| -> Vec<(usize, i64, i64, i64)> {
        p.blocks()
            .flat_map(|(_, st)| st.layout.openings.iter())
            .filter_map(|o| o.ty.as_ref().map(|t| key(cat_index(&t.catalogue), o)))
            .collect()
    };
    for k in all(p) {
        keys.insert(k, String::new());
    }
    let (mut doors, mut windows) = (0, 0);
    for (k, mark) in keys.iter_mut() {
        let door = catalogue().get(k.0).is_none_or(|c| c.door);
        let n = if door { &mut doors } else { &mut windows };
        *n += 1;
        *mark = format!("{}{:02}", if door { 'D' } else { 'W' }, n);
    }
    for st in p.storeys.iter_mut().chain(p.basements.iter_mut()) {
        for o in &mut st.layout.openings {
            let k = o.ty.as_ref().map(|t| key(cat_index(&t.catalogue), o));
            if let (Some(k), Some(t)) = (k, o.ty.as_mut()) {
                t.mark = keys[&k].clone();
            }
        }
    }
}

/// Types every opening of the project. Runs inside the rule scope of the build.
pub(super) fn apply(spec: &ProjectSpec, p: &mut GProject) {
    let cfg = Cfg::read(&spec.rules);
    for (k, st) in p.storeys.iter_mut().enumerate() {
        type_storey(&cfg, st, k == 0);
    }
    for st in p.basements.iter_mut() {
        type_storey(&cfg, st, false);
    }
    number(p);
}

/// One line of a door or window schedule: a type at its size and how many there are.
#[derive(Clone, Debug, PartialEq)]
pub struct SchedRow {
    pub mark: String,
    pub door: bool,
    pub name: String,
    pub catalogue: String,
    pub width: f64,
    pub height: f64,
    pub sill: f64,
    pub leaves: u8,
    pub fire: u32,
    pub rw: u32,
    pub u: f64,
    pub clear: f64,
    pub count: usize,
    /// Storeys (above and below ground) that hold at least one.
    pub storeys: usize,
}

/// The door and window schedules of the project: doors first, each by mark.
pub fn schedule(p: &GProject) -> Vec<SchedRow> {
    let mut rows: BTreeMap<String, (SchedRow, Vec<u32>)> = BTreeMap::new();
    for (blk, st) in p.blocks() {
        for o in &st.layout.openings {
            let Some(t) = &o.ty else { continue };
            let e = rows.entry(t.mark.clone()).or_insert_with(|| {
                let door = o.door;
                let row = SchedRow {
                    mark: t.mark.clone(),
                    door,
                    name: t.name.clone(),
                    catalogue: t.catalogue.clone(),
                    width: o.width,
                    height: o.height,
                    sill: o.sill,
                    leaves: t.leaves,
                    fire: t.fire,
                    rw: t.rw,
                    u: t.u,
                    clear: t.clear,
                    count: 0,
                    storeys: 0,
                };
                (row, vec![])
            });
            e.0.count += 1;
            if !e.1.contains(&blk) {
                e.1.push(blk);
            }
        }
    }
    let mut out: Vec<SchedRow> = rows
        .into_values()
        .map(|(mut r, st)| {
            r.storeys = st.len();
            r
        })
        .collect();
    out.sort_by(|a, b| b.door.cmp(&a.door).then(a.mark.cmp(&b.mark)));
    out
}

/// The reasons of the typing as `(rule, value, key, args)` for the explanation list.
pub(super) fn whys(spec: &ProjectSpec, p: &GProject) -> Vec<(&'static str, f64, &'static str, Args)> {
    let rs = &spec.rules;
    let all: Vec<(&GOpening, &GOpeningType)> =
        p.blocks().flat_map(|(_, st)| st.layout.openings.iter()).filter_map(|o| o.ty.as_ref().map(|t| (o, t))).collect();
    if all.is_empty() {
        return vec![];
    }
    let has = |t: &GOpeningType, r: &str| t.rules.iter().any(|x| *x == r);
    let count = |f: &dyn Fn(&GOpening, &GOpeningType) -> bool| all.iter().filter(|(o, t)| f(o, t)).count();
    let mut out: Vec<(&'static str, f64, &'static str, Args)> = vec![];
    let marks = |door: bool| -> Vec<&str> {
        let mut m: Vec<&str> = all.iter().filter(|(o, _)| o.door == door).map(|(_, t)| t.mark.as_str()).collect();
        m.sort_unstable();
        m.dedup();
        m
    };
    let (dm, wm) = (marks(true), marks(false));
    let first = dm.first().or(wm.first()).copied().unwrap_or("");
    let last = wm.last().or(dm.last()).copied().unwrap_or("");
    out.push((
        "opening.leaf_loss",
        rs.get("opening.leaf_loss"),
        "open_types",
        vec![
            ("n", s(all.len())),
            ("doors", s(dm.len())),
            ("windows", s(wm.len())),
            ("first", first.to_string()),
            ("last", last.to_string()),
            ("loss", s(rs.get("opening.leaf_loss"))),
        ],
    ));
    let exits = count(&|o, t| o.door && t.exit && has(t, "opening.egress_swing"));
    if exits > 0 {
        let outward = count(&|o, t| o.door && t.function == "exit" && t.exit);
        let stair = count(&|o, t| o.door && t.function == "stair" && t.exit);
        out.push(("opening.egress_swing", rs.get("opening.egress_swing"), "open_swing_exit", vec![
            ("n", s(exits)),
            ("out", s(outward)),
            ("stair", s(stair)),
        ]));
    }
    let route = count(&|_, t| has(t, "opening.out_swing_area"));
    if route > 0 {
        let area = rs.get("opening.out_swing_area");
        out.push(("opening.out_swing_area", area, "open_swing_route", vec![("n", s(route)), ("area", s(area))]));
    }
    let kept = count(&|_, t| has(t, "opening.corridor_clear"));
    if kept > 0 {
        let clear = rs.get("opening.corridor_clear");
        out.push(("opening.corridor_clear", clear, "open_swing_room", vec![("n", s(kept)), ("clear", s(clear))]));
    }
    for (rule, key, function) in [
        ("opening.fire_stair", "open_fire_stair", "stair"),
        ("opening.fire_unit", "open_fire_unit", "unit"),
        ("opening.fire_shaft", "open_fire_shaft", "shaft"),
        ("opening.fire_compartment", "open_fire_compartment", "compartment"),
    ] {
        let n = count(&|o, t| o.door && has(t, rule) && (t.function == function || (function == "shaft" && t.function == "lift")));
        if n > 0 {
            out.push((rule, rs.get(rule), key, vec![("n", s(n)), ("min", s(rs.get(rule)))]));
        }
    }
    for (rule, key) in [("opening.clear_access", "open_clear_access"), ("opening.clear_exit", "open_clear_exit")] {
        let n = count(&|_, t| has(t, rule));
        if n > 0 {
            let v = rs.get(rule);
            let widened = count(&|_, t| has(t, rule) && t.widened);
            let short = count(&|_, t| has(t, rule) && t.clear < v - TOL);
            out.push((rule, v, key, vec![("n", s(n)), ("clear", s(v)), ("widened", s(widened)), ("short", s(short))]));
        }
    }
    let unit = count(&|_, t| has(t, "opening.rw_unit"));
    if unit > 0 {
        let rw = rs.get("opening.rw_unit");
        out.push(("opening.rw_unit", rw, "open_rw_unit", vec![("n", s(unit)), ("rw", s(rw))]));
    }
    let ext = count(&|_, t| has(t, "opening.rw_exterior"));
    if ext > 0 {
        let rw = rs.get("opening.rw_exterior");
        out.push(("opening.rw_exterior", rw, "open_rw_exterior", vec![("n", s(ext)), ("rw", s(rw))]));
    }
    let thermal = count(&|_, t| has(t, "opening.u_max"));
    if thermal > 0 {
        let u = rs.get("opening.u_max");
        out.push(("opening.u_max", u, "open_u", vec![("n", s(thermal)), ("u", s(u))]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_parses_and_serves_every_function() {
        let c = catalogue();
        assert!(c.len() >= 20);
        for f in FUNCTIONS {
            let door = f != "window";
            assert!(c.iter().any(|t| t.door == door && t.uses.iter().any(|u| u == f)), "{f}");
        }
    }

    #[test]
    fn a_fire_need_picks_the_plainest_type_that_meets_it() {
        let need = |fire| Need { fire, rw: 0, u: None };
        let id = |w, n: &Need| catalogue()[choose(true, "stair", w, n)].id.clone();
        assert_eq!(id(0.9, &need(0)), "door.int.1");
        assert_eq!(id(0.9, &need(30)), "door.ei30.1");
        assert_eq!(id(0.9, &need(60)), "door.ei60.1");
        assert_eq!(id(1.8, &need(60)), "door.ei60.2");
    }

    #[test]
    fn a_thermal_need_picks_the_triple_glazed_type() {
        let std = Need { fire: 0, rw: 28, u: Some(1.6) };
        let strict = Need { fire: 0, rw: 28, u: Some(1.0) };
        assert_eq!(catalogue()[choose(false, "window", 1.5, &std)].id, "win.cas.2");
        assert_eq!(catalogue()[choose(false, "window", 1.5, &strict)].id, "win.cas.2.hp");
    }
}
