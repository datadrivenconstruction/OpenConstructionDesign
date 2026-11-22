// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The lift family of a built bank: the car at the ground stop, a landing door on every
//! storey, the pit under the ground stop and the headroom over the top stop with its machine.
//! Car classes and speed bands are the catalogue `data/lift_rules.json`; the class follows the
//! rated capacity (`core.lift_capacity`), the band the rated speed (`core.lift_speed`). The
//! shaft is never smaller than the car plus its clearances, and [`check`] proves it.

use super::rules::rule;
use super::{slots, GProject};
use crate::GFixture;
use serde_json::Value as J;
use std::sync::OnceLock;

type P2 = (f64, f64);

/// One rated load of the catalogue.
pub struct Class {
    pub id: String,
    /// Rated load, kg: the class id.
    pub load: f64,
    pub persons: u32,
    /// Car width (across the door) and depth (towards it), m.
    pub car: P2,
    pub door: f64,
    car_ty: &'static str,
    door_ty: &'static str,
    /// Pit and headroom family types, one per speed band.
    pit_ty: Vec<&'static str>,
    over_ty: Vec<&'static str>,
}

/// What the rated speed asks of the pit and the head of the shaft.
pub struct Band {
    pub id: String,
    pub upto: f64,
    /// Pit depth under the lowest stop and headroom over the highest, m.
    pub pit: f64,
    pub head: f64,
    pub machine: (f64, f64, f64),
    machine_ty: &'static str,
}

/// Placement allowances of the fixtures, m: the car's gap to its door, the probe off a shaft
struct Table {
    gap: Gap,
    clear: P2,
    classes: Vec<Class>,
    bands: Vec<Band>,
}

struct Gap {
    car: f64,
    edge: f64,
    step: f64,
    reach: f64,
    machine: f64,
    hang: f64,
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn num(v: &J, what: &str) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("lift_rules.json: {what} is not a number"))
}

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let j: J = serde_json::from_str(include_str!("../../data/lift_rules.json")).expect("lift_rules.json");
        let bands: Vec<Band> = j["bands"]
            .as_array()
            .expect("bands")
            .iter()
            .map(|b| {
                let m = &b["machine"];
                let id = b["id"].as_str().expect("band id").to_string();
                Band {
                    machine_ty: leak(id.clone()),
                    id,
                    upto: num(&b["upto_speed"], "upto_speed"),
                    pit: num(&b["pit"], "pit"),
                    head: num(&b["head"], "head"),
                    machine: (num(&m[0], "machine"), num(&m[1], "machine"), num(&m[2], "machine")),
                }
            })
            .collect();
        let classes = j["classes"]
            .as_array()
            .expect("classes")
            .iter()
            .map(|c| {
                let id = c["id"].as_str().expect("class id").to_string();
                let door = num(&c["door"], "door");
                Class {
                    car_ty: leak(format!("{id} kg")),
                    door_ty: leak(format!("{}", (door * 1000.0).round() as i64)),
                    pit_ty: bands.iter().map(|b| leak(format!("{id} kg {}", b.id))).collect(),
                    over_ty: bands.iter().map(|b| leak(format!("{id} kg {}", b.id))).collect(),
                    load: id.parse().expect("class id is the rated load in kg"),
                    persons: c["persons"].as_u64().expect("persons") as u32,
                    car: (num(&c["car"][0], "car"), num(&c["car"][1], "car")),
                    door,
                    id,
                }
            })
            .collect();
        let g = |k: &str| num(&j["layout"][k], k);
        let gap = Gap { car: g("car_gap"), edge: g("edge_probe"), step: g("lobby_step"), reach: g("lobby_reach"),machine: g("machine_gap"), hang: g("machine_hang") };
        Table { gap, clear: (num(&j["clear_w"], "clear_w"), num(&j["clear_d"], "clear_d")), classes, bands }
    })
}

/// The smallest class that carries `persons`; the largest when none does.
pub fn class_for(persons: u32) -> &'static Class {
    let t = table();
    t.classes.iter().find(|c| c.persons >= persons).unwrap_or_else(|| t.classes.last().expect("classes"))
}

/// Whether `persons` is more than the largest car of the catalogue carries.
pub fn exceeds(persons: u32) -> bool {
    table().classes.last().is_some_and(|c| c.persons < persons)
}

/// Height of the top occupied storey above the ground floor, m: what the fire brigade climbs.
pub fn serve_height(spec: &super::ProjectSpec) -> f64 {
    spec.storey_elevation(spec.floors.saturating_sub(1) as usize)
}

/// The car class of the bank: the rated capacity's, and in a building over
/// `fire.fighting_lift_height` at least the class of `fire.fighting_lift_load`, so every car
/// can serve as the firefighting lift.
pub fn bank_class(height: f64) -> &'static Class {
    let base = class_for(rule("core.lift_capacity") as u32);
    if height < rule("fire.fighting_lift_height") {
        return base;
    }
    let t = table();
    let load = rule("fire.fighting_lift_load");
    let fire = t.classes.iter().find(|c| c.load + 1e-9 >= load).unwrap_or_else(|| t.classes.last().expect("classes"));
    if fire.persons > base.persons {
        fire
    } else {
        base
    }
}

/// The band of a rated speed, m/s.
pub fn band_for(speed: f64) -> &'static Band {
    let t = table();
    t.bands.iter().find(|b| speed <= b.upto + 1e-9).unwrap_or_else(|| t.bands.last().expect("bands"))
}

/// Width and depth the car needs in its shaft: the car and its guide and door clearances.
pub fn envelope(c: &Class) -> P2 {
    let t = table();
    (c.car.0 + t.clear.0, c.car.1 + t.clear.1)
}

/// The lift of the rules in force, as the why and the checks describe it.
pub struct Summary {
    pub class: &'static str,
    pub persons: u32,
    pub car: P2,
    pub envelope: P2,
    pub pit: f64,
    pub head: f64,
}

pub fn summary(height: f64) -> Summary {
    let c = bank_class(height);
    let b = band_for(rule("core.lift_speed"));
    Summary { class: &c.id, persons: c.persons, car: c.car, envelope: envelope(c), pit: b.pit, head: b.head }
}

/// A shaft read for its door: centre, the unit direction its door faces, and its extents
/// along that direction and across it.
struct Geom {
    c: P2,
    n: P2,
    along: f64,
    across: f64,
}

fn inside(poly: &[P2], q: P2) -> bool {
    poly.len() > 2 && crate::check::inside(poly, q)
}

/// The side of the shaft that opens on the most free room: the lift lobby. A ring read from a
/// rotated plate gives its door side the same way, so no axis is assumed.
fn geom(rooms: &[crate::GRoom], bank: &[Vec<P2>], ring: &[P2], env: P2) -> Option<Geom> {
    if ring.len() < 3 {
        return None;
    }
    let gap = &table().gap;
    let k = ring.len() as f64;
    let c = (ring.iter().map(|q| q.0).sum::<f64>() / k, ring.iter().map(|q| q.1).sum::<f64>() / k);
    let room = rooms.iter().find(|r| inside(&r.polygon, c));
    let mut best: Option<(bool, i32, P2)> = None;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        if len < 1e-6 {
            continue;
        }
        let t = ((b.0 - a.0) / len, (b.1 - a.1) / len);
        let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let mut n = (t.1, -t.0);
        if inside(ring, (mid.0 + n.0 * gap.edge, mid.1 + n.1 * gap.edge)) {
            n = (-n.0, -n.1);
        }
        // metres of the room free in front of this side, in 0.3 m steps
        let free = room.map_or(0, |r| {
            (1..=(gap.reach / gap.step).round() as i32)
                .take_while(|s| {
                    let q = (mid.0 + n.0 * gap.step * f64::from(*s), mid.1 + n.1 * gap.step * f64::from(*s));
                    inside(&r.polygon, q) && !bank.iter().any(|o| inside(o, q))
                })
                .count() as i32
        });
        // a side the car fits behind wins over one it does not, once there is room before it:
        // an end shaft has free room beside it too, and the car's own depth tells the door side
        let fit = free > 0 && fits(&geom_of(ring, c, n), env);
        if best.is_none_or(|b| (fit, free) > (b.0, b.1)) {
            best = Some((fit, free, n));
        }
    }
    let (_, _, n) = best?;
    Some(geom_of(ring, c, n))
}

/// The extents of `ring` along its door direction `n` and across it.
fn geom_of(ring: &[P2], c: P2, n: P2) -> Geom {
    let u = (-n.1, n.0);
    let ext = |d: P2| {
        let v: Vec<f64> = ring.iter().map(|q| q.0 * d.0 + q.1 * d.1).collect();
        v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min)
    };
    Geom { c, n, along: ext(n), across: ext(u) }
}

/// Turn of a family whose local +y is the door side, for a door facing `n`.
fn turn(n: P2) -> f64 {
    n.1.atan2(n.0) - std::f64::consts::FRAC_PI_2
}

/// A car fits when its envelope lies within the shaft's two extents.
fn fits(g: &Geom, env: P2) -> bool {
    env.1 <= g.along + 1e-6 && env.0 <= g.across + 1e-6
}

/// The first `cars` lift shafts of one storey, in layout order: the core bank is built before
/// any other lift shaft (a split level's), and service shafts are of another kind.
fn bank(l: &crate::GLayout, cars: usize) -> Vec<Vec<P2>> {
    l.shafts.iter().filter(|s| s.kind == crate::ShaftKind::Lift).take(cars).map(|s| s.ring.clone()).collect()
}

/// The lift shafts of the ground storey.
pub(super) fn rings(p: &GProject) -> Vec<Vec<P2>> {
    bank(&p.storeys[0].layout, p.lifts.cars as usize)
}

/// The door side and extents of each shaft of the bank, read on the ground storey where the lift
/// room stands: every storey and basement above or below it keeps the same shafts.
fn geoms(p: &GProject, env: P2) -> Vec<Option<Geom>> {
    let l = &p.storeys[0].layout;
    let rs = bank(l, p.lifts.cars as usize);
    rs.iter().map(|r| geom(&l.rooms, &rs, r, env)).collect()
}

/// Every storey and basement holds the bank's shafts, and each holds the car of the bank's
/// class with its clearances. The class is read under the spec's own rules, as it was built.
pub fn check(spec: &super::ProjectSpec, p: &GProject) -> Result<(), String> {
    super::rules::scoped(&spec.rules, || check_ruled(spec, p))
}

fn check_ruled(spec: &super::ProjectSpec, p: &GProject) -> Result<(), String> {
    let cars = p.lifts.cars as usize;
    if cars == 0 || p.storeys.is_empty() {
        return Ok(());
    }
    let env = envelope(bank_class(serve_height(spec)));
    for (i, g) in geoms(p, env).iter().enumerate() {
        let g = g.as_ref().ok_or(format!("lift shaft {i} has no outline"))?;
        if !fits(g, env) {
            return Err(format!(
                "lift shaft {i} is {:.2} x {:.2} m, the car with its clearances needs {:.2} x {:.2} m",
                g.across, g.along, env.0, env.1
            ));
        }
    }
    for (k, s) in p.basements.iter().chain(&p.storeys).enumerate() {
        let n = bank(&s.layout, cars).len();
        if n != cars {
            return Err(format!("storey {k} has {n} lift shafts, the bank has {cars}"));
        }
    }
    Ok(())
}

/// Adds one fixture; false when the storey's fixture slots are full.
fn put(l: &mut crate::GLayout, family: &'static str, ty: &'static str, at: P2, rotation: f64, z: f64) -> bool {
    let room = l.fixtures.len() < slots::FIXTURES_MAX as usize;
    if room {
        l.fixtures.push(GFixture { scale: 1.0, min_clear: 0.0, family, ty, at, rotation, wall: None, z });
    }
    room
}

/// What one storey takes of the lift family.
#[derive(Clone, Copy)]
struct Part {
    car: bool,
    pit: bool,
    head: bool,
}

struct Kit {
    cls: &'static Class,
    band: &'static Band,
    bi: usize,
}

/// The lift family on one storey: a landing door on each shaft, and the car, pit, headroom and
/// machine where `part` asks. True when the fixture slots ran out.
fn dress(l: &mut crate::GLayout, gs: &[Option<Geom>], kit: &Kit, part: Part) -> bool {
    let t = table();
    let (cls, band) = (kit.cls, kit.band);
    let (pit, head) = (cls.pit_ty[kit.bi], cls.over_ty[kit.bi]);
    let mut full = false;
    for g in gs.iter().flatten() {
        let rot = turn(g.n);
        let face = (g.c.0 + g.n.0 * g.along / 2.0, g.c.1 + g.n.1 * g.along / 2.0);
        full |= !put(l, "Lift Landing Door", cls.door_ty, face, rot, 0.0);
        if part.car {
            let back = cls.car.1 / 2.0 + t.gap.car;
            full |= !put(l, "Lift Car", cls.car_ty, (face.0 - g.n.0 * back, face.1 - g.n.1 * back), rot, 0.0);
        }
        if part.pit {
            full |= !put(l, "Lift Pit", pit, g.c, rot, -band.pit);
        }
        if part.head {
            full |= !put(l, "Lift Overrun", head, g.c, rot, 0.0);
            let (_, md, mh) = band.machine;
            let back = (envelope(cls).1 / 2.0 - md / 2.0 - t.gap.machine).max(0.0);
            full |= !put(l, "Lift Machine", band.machine_ty, (g.c.0 - g.n.0 * back, g.c.1 - g.n.1 * back), rot, band.head - mh - t.gap.hang);
        }
    }
    full
}

/// Car, doors, pit, headroom and machine of every shaft of the bank. The car stands at the
/// ground stop; a landing door closes each shaft on every storey, basements included; the pit
/// lies under the lowest stop (the lowest basement, else the ground floor); the headroom
/// enclosure and the machine stand over the top stop. What cannot be placed is reported in
/// `lifts.issues`, never skipped silently.
pub(super) fn furnish(spec: &super::ProjectSpec, p: &mut GProject) {
    let cars = p.lifts.cars as usize;
    if cars == 0 || p.storeys.is_empty() {
        return;
    }
    let t = table();
    let height = serve_height(spec);
    let mut issues = vec![];
    let want = rule("core.lift_capacity") as u32;
    if exceeds(want) {
        issues.push(format!("{want} persons per car is more than the largest car of the catalogue carries; the largest is used"));
    }
    let cls = bank_class(height);
    let speed = rule("core.lift_speed");
    let bi = t.bands.iter().position(|b| speed <= b.upto + 1e-9).unwrap_or(t.bands.len() - 1);
    let kit = Kit { cls, band: &t.bands[bi], bi };
    let env = envelope(cls);
    let n0 = bank(&p.storeys[0].layout, cars).len();
    if n0 != cars {
        issues.push(format!("the bank has {cars} cars, the ground storey {n0} lift shafts"));
    }
    let mut gs = geoms(p, env);
    for (i, g) in gs.iter_mut().enumerate() {
        match g {
            None => issues.push(format!("lift shaft {i} has no outline for its door")),
            Some(x) if !fits(x, env) => {
                issues.push(format!("the lift car needs {:.2} x {:.2} m, shaft {i} gives {:.2} x {:.2} m", env.0, env.1, x.across, x.along));
                *g = None;
            }
            Some(_) => {}
        }
    }
    let nb = p.basements.len();
    let top = p.storeys.len() - 1;
    let mut full = false;
    for (k, b) in p.basements.iter_mut().enumerate() {
        full |= dress(&mut b.layout, &gs, &kit, Part { car: false, pit: k + 1 == nb, head: false });
    }
    for (k, s) in p.storeys.iter_mut().enumerate() {
        full |= dress(&mut s.layout, &gs, &kit, Part { car: k == 0, pit: k == 0 && nb == 0, head: k == top });
    }
    if full {
        issues.push("the fixture slots of a storey ran out: some lift doors or cars are missing".to_string());
    }
    if height >= rule("fire.fighting_lift_height") {
        p.lifts.fighting = 1;
        if cars < 2 {
            issues.push("the only lift is also the firefighting lift: it has no shaft of its own".to_string());
        }
    }
    p.lifts.issues.extend(issues);
}
