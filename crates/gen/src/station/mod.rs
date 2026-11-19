// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An underground station built in an open box: the typology of a cut-and-cover station as
//! rules, not as a template. Diaphragm walls hold the excavation; inside sit the base slab
//! under the tracks, an island platform between two trackways, a concourse level above it and
//! the roof slab under the street cover. Banks of escalators with a fixed stair and lifts lead
//! from the platform to the concourse and on to the street through wells; ventilation shafts
//! rise at both ends. Every number comes from a `station.*` rule (`data/rules.json`), so a
//! norm pack, the project or the brief steers it, and every decision is explained by [`why`].
//! The result is schematic: valid geometry, no structural or fire design.

pub mod why;

use crate::project::rules::{self, RuleSet};
use crate::GenError;
use why::Why;

/// The rules the station reads, all with a built-in default.
pub const RULES: [&str; 46] = [
    "station.platform_length",
    "station.platform_width",
    "station.edge_lane",
    "station.trackway_width",
    "station.rail_depth",
    "station.platform_height",
    "station.track_bed",
    "station.cover",
    "station.roof_slab",
    "station.base_slab",
    "station.floor_slab",
    "station.concourse_height",
    "station.platform_clear",
    "station.end_zone",
    "station.dwall_min",
    "station.dwall_per_depth",
    "station.dwall_step",
    "station.dwall_toe",
    "station.peak_flow",
    "station.escalator_capacity",
    "station.escalator_banks",
    "station.escalator_standby",
    "station.escalator_angle",
    "station.escalator_width",
    "station.escalator_landing",
    "station.stair_width",
    "station.stair_riser",
    "station.stair_waist",
    "station.stair_gap",
    "station.stair_flow",
    "station.train_load",
    "station.evac_time",
    "station.psd",
    "station.psd_height",
    "station.psd_door_width",
    "station.train_door_pitch",
    "station.lifts",
    "station.lift_shaft",
    "station.lift_load",
    "station.vent_flow",
    "station.vent_velocity",
    "station.column_spacing",
    "station.vault_rise",
    "station.vault_spring",
    "station.vault_bay",
    "station.ground_margin",
];

/// The other rule of the column grid (kept apart: [`RULES`] is the list the brief reads in
/// order, the column size closes it).
pub const COLUMN_SIZE: &str = "station.column_size";

/// A level of the station, top down; the ground is the model's own level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lvl {
    Ground,
    Concourse,
    Platform,
    Track,
}

impl Lvl {
    pub const BELOW: [Lvl; 3] = [Lvl::Concourse, Lvl::Platform, Lvl::Track];

    /// Name in English and Russian.
    pub fn names(self) -> (&'static str, &'static str) {
        match self {
            Lvl::Ground => ("Ground", crate::lang::ru("Ground")),
            Lvl::Concourse => ("Concourse level", crate::lang::ru("Concourse level")),
            Lvl::Platform => ("Platform level", crate::lang::ru("Platform level")),
            Lvl::Track => ("Track level", crate::lang::ru("Track level")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallKind {
    /// A band of the diaphragm wall between two levels.
    Diaphragm,
    /// A cross wall closing an end zone.
    Partition,
    /// A panel of the platform screen along a platform edge, between two doors.
    ScreenDoor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SWall {
    pub level: Lvl,
    pub a: (f64, f64),
    pub b: (f64, f64),
    /// Of the base, above the level.
    pub base_offset: f64,
    pub height: f64,
    pub kind: WallKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlabKind {
    Base,
    Platform,
    Concourse,
    Roof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SSlab {
    pub level: Lvl,
    /// Of the top, above the level.
    pub offset: f64,
    pub thickness: f64,
    pub ring: Vec<(f64, f64)>,
    pub kind: SlabKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaftKind {
    EscalatorWell,
    StairWell,
    EntranceWell,
    Lift,
    Vent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SShaft {
    pub level: Lvl,
    pub top: Lvl,
    pub ring: Vec<(f64, f64)>,
    pub kind: ShaftKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SStair {
    pub level: Lvl,
    pub top: Lvl,
    /// Middle of the first riser line.
    pub origin: (f64, f64),
    /// Radians, the direction of the climb.
    pub angle: f64,
    pub shape: ddc_geom::stair::Shape,
    pub width: f64,
    pub max_riser: f64,
    pub landing: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SColumn {
    pub level: Lvl,
    /// `None`: up to `top_offset` above its own level.
    pub top: Option<Lvl>,
    pub top_offset: f64,
    pub at: (f64, f64),
    pub size: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SRoom {
    pub level: Lvl,
    pub class: &'static str,
    pub name: (&'static str, &'static str),
    pub point: (f64, f64),
    pub height: f64,
}

/// One escalator: its axis from the bottom landing to the top one.
#[derive(Clone, Debug, PartialEq)]
pub struct SEscalator {
    pub bank: usize,
    pub bottom: (f64, f64, f64),
    pub top: (f64, f64, f64),
    pub width: f64,
}

/// One barrel vault segment over a trackway or the platform: the arch `profile` as (u, z) over
/// the span a to b, drawn `depth` to the left of a to b, `offset` above its level.
#[derive(Clone, Debug, PartialEq)]
pub struct SVault {
    pub level: Lvl,
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub profile: Vec<(f64, f64)>,
    pub depth: f64,
    pub offset: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GStation {
    /// Levels below ground with their elevations, top down.
    pub levels: Vec<(Lvl, f64)>,
    /// Inside of the box: length along the tracks, width across.
    pub inner: (f64, f64),
    pub walls: Vec<SWall>,
    pub slabs: Vec<SSlab>,
    pub shafts: Vec<SShaft>,
    pub stairs: Vec<SStair>,
    pub columns: Vec<SColumn>,
    pub rooms: Vec<SRoom>,
    pub escalators: Vec<SEscalator>,
    /// Barrel vaults of the tunnel sections: per bay, over each trackway and the platform.
    pub vaults: Vec<SVault>,
    /// The ground at street level as four plane strips round the box, outside its walls: the box
    /// is a hole in the ground, so the earth bodies never overlap it. West, east, south, north.
    pub ground: Vec<Vec<(f64, f64)>>,
    pub dwall_thickness: f64,
    pub metrics: Vec<(&'static str, f64)>,
    pub why: Vec<Why>,
    /// Checks the station misses: it is still built, the reply names them.
    pub issues: Vec<String>,
}

impl GStation {
    pub fn elevation(&self, l: Lvl) -> f64 {
        self.levels.iter().find(|x| x.0 == l).map_or(0.0, |x| x.1)
    }

    pub fn metric(&self, k: &str) -> Option<f64> {
        self.metrics.iter().find(|m| m.0 == k).map(|m| m.1)
    }
}

/// Axis-aligned rectangle `(x0, y0, x1, y1)`.
pub type Rect = (f64, f64, f64, f64);

pub fn ring(r: Rect) -> Vec<(f64, f64)> {
    vec![(r.0, r.1), (r.2, r.1), (r.2, r.3), (r.0, r.3)]
}

/// Bounding rectangle of a ring.
pub fn bounds(ring: &[(f64, f64)]) -> Rect {
    ring.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, p| {
        (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1))
    })
}

fn up(x: f64, step: f64) -> f64 {
    (x / step - 1e-9).ceil() * step
}

/// A number as the texts print it: at most two decimals.
fn n(x: f64) -> String {
    let s = format!("{x:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

fn infeasible(s: String) -> GenError {
    GenError::Infeasible(s)
}

struct Ctx<'a> {
    rs: &'a RuleSet,
    why: Vec<Why>,
}

impl Ctx<'_> {
    fn r(&self, id: &str) -> f64 {
        self.rs.get(id)
    }

    fn say(&mut self, target: &'static str, rule: &'static str, key: &'static str, args: Vec<(&'static str, String)>) {
        let value = self.rs.get(rule);
        let source = self.rs.source(rule).map_or("builtin", |l| l.name());
        let effect = why::effect(key, &args);
        self.why.push(Why { target, rule, value, source, key, args, effect });
    }
}

/// The station under `rs`. Errors name the rules to change when the parts do not fit.
pub fn plan(rs: &RuleSet) -> Result<GStation, GenError> {
    for id in RULES.iter().chain([&COLUMN_SIZE]) {
        if rules::def(id).and_then(|d| d.default).is_none() {
            return Err(infeasible(format!("rule {id} is missing from the rule set")));
        }
    }
    let mut cx = Ctx { rs, why: vec![] };
    let pl = cx.r("station.platform_length");
    let pw = cx.r("station.platform_width");
    let tw = cx.r("station.trackway_width");
    let ez = cx.r("station.end_zone");
    let lane = cx.r("station.edge_lane");
    let (len, wid) = (pl + 2.0 * ez, pw + 2.0 * tw);
    let mid = wid * 0.5;
    cx.say("box", "station.platform_length", "station_box", vec![
        ("l", n(len)), ("w", n(wid)), ("pl", n(pl)), ("ez", n(ez)), ("pw", n(pw)), ("tw", n(tw)),
    ]);

    // elevations, ground at 0
    let rail = -cx.r("station.rail_depth");
    let pz = rail + cx.r("station.platform_height");
    let bz = rail - cx.r("station.track_bed");
    let base_bot = bz - cx.r("station.base_slab");
    let roof_top = -cx.r("station.cover");
    let roof_bot = roof_top - cx.r("station.roof_slab");
    let cz = roof_bot - cx.r("station.concourse_height");
    let floor = cx.r("station.floor_slab");
    let clear = cz - floor - pz;
    let need = cx.r("station.platform_clear");
    if clear < need {
        return Err(infeasible(format!(
            "the concourse leaves {} m over the platform, station.platform_clear wants {} m: \
             raise station.rail_depth, or lower station.cover, station.concourse_height or the slabs",
            n(clear),
            n(need)
        )));
    }
    cx.say("levels", "station.rail_depth", "station_levels", vec![
        ("rail", n(-rail)), ("pz", n(pz)), ("cz", n(cz)), ("bz", n(bz)),
    ]);
    cx.say("levels", "station.platform_clear", "station_clear", vec![("h", n(clear)), ("min", n(need))]);

    // diaphragm walls: thicker with depth, a toe below the formation
    let exc = -base_bot;
    let step = cx.r("station.dwall_step");
    let t = up(cx.r("station.dwall_min").max(exc * cx.r("station.dwall_per_depth")), step);
    let toe = exc * cx.r("station.dwall_toe");
    let bottom = base_bot - toe;
    cx.say("walls", "station.dwall_per_depth", "station_dwall", vec![
        ("t", n(t)), ("bot", n(bottom)), ("exc", n(exc)), ("toe", n(toe)),
    ]);
    let h = t * 0.5;
    let corners = [(-h, -h), (len + h, -h), (len + h, wid + h), (-h, wid + h)];
    let mut walls = vec![];
    // one band per level, so each level is closed for its rooms; the lowest takes the toe
    let bands = [(Lvl::Track, bz, bottom, pz), (Lvl::Platform, pz, pz, cz), (Lvl::Concourse, cz, cz, 0.0)];
    for (level, at, from, to) in bands {
        for i in 0..corners.len() {
            walls.push(SWall {
                level,
                a: corners[i],
                b: corners[(i + 1) % corners.len()],
                base_offset: from - at,
                height: to - from,
                kind: WallKind::Diaphragm,
            });
        }
    }
    for x in [ez, len - ez] {
        walls.push(SWall { level: Lvl::Platform, a: (x, tw), b: (x, tw + pw), base_offset: 0.0, height: clear, kind: WallKind::Partition });
        walls.push(SWall { level: Lvl::Concourse, a: (x, 0.0), b: (x, wid), base_offset: 0.0, height: roof_bot - cz, kind: WallKind::Partition });
    }

    // platform screen doors: a screen along each platform edge with a door at every train door
    let psd = cx.r("station.psd") >= 1.0;
    let (psd_h, psd_w, pitch) = (cx.r("station.psd_height"), cx.r("station.psd_door_width"), cx.r("station.train_door_pitch"));
    let doors_per_edge = if psd { (pl / pitch).floor().max(1.0) } else { 0.0 };
    if psd {
        let count = doors_per_edge as usize;
        // doors centred in equal bays of the platform, the screen between them
        let bay = pl / count as f64;
        for y in [tw, tw + pw] {
            let mut from = ez;
            for k in 0..count {
                let c = ez + bay * (k as f64 + 0.5);
                if c - psd_w / 2.0 - from > 1e-9 {
                    walls.push(SWall { level: Lvl::Platform, a: (from, y), b: (c - psd_w / 2.0, y), base_offset: 0.0, height: psd_h, kind: WallKind::ScreenDoor });
                }
                from = c + psd_w / 2.0;
            }
            if ez + pl - from > 1e-9 {
                walls.push(SWall { level: Lvl::Platform, a: (from, y), b: (ez + pl, y), base_offset: 0.0, height: psd_h, kind: WallKind::ScreenDoor });
            }
        }
        cx.say("psd", "station.psd", "station_psd", vec![("n", count.to_string()), ("h", n(psd_h)), ("w", n(psd_w)), ("pitch", n(pitch))]);
    }

    let inner = (0.0, 0.0, len, wid);
    let slabs = vec![
        SSlab { level: Lvl::Track, offset: 0.0, thickness: bz - base_bot, ring: ring(inner), kind: SlabKind::Base },
        SSlab { level: Lvl::Platform, offset: 0.0, thickness: floor, ring: ring((0.0, tw, len, tw + pw)), kind: SlabKind::Platform },
        SSlab { level: Lvl::Concourse, offset: 0.0, thickness: floor, ring: ring(inner), kind: SlabKind::Concourse },
        SSlab { level: Lvl::Concourse, offset: roof_top - cz, thickness: roof_top - roof_bot, ring: ring(inner), kind: SlabKind::Roof },
    ];

    // escalators: the peak flow over the banks, plus standby units
    let banks = cx.r("station.escalator_banks") as usize;
    let cap = cx.r("station.escalator_capacity");
    let flow = cx.r("station.peak_flow");
    let standby = cx.r("station.escalator_standby");
    let per_bank = ((flow / banks as f64 / cap) - 1e-9).ceil().max(1.0) + standby;
    let ew = cx.r("station.escalator_width");
    let land = cx.r("station.escalator_landing");
    let tan = cx.r("station.escalator_angle").to_radians().tan();
    let rise = cz - pz;
    let well_len = rise / tan + 2.0 * land;
    let well_wid = per_bank * ew;
    if well_wid > pw - 2.0 * lane {
        return Err(infeasible(format!(
            "{} escalators side by side take {} m, the platform leaves {} m between its edge lanes: \
             widen station.platform_width, add station.escalator_banks or lower station.peak_flow",
            per_bank, n(well_wid), n(pw - 2.0 * lane)
        )));
    }
    cx.say("escalators", "station.peak_flow", "station_escalators", vec![
        ("n", n(per_bank * banks as f64)), ("banks", banks.to_string()), ("flow", n(flow)), ("cap", n(cap)), ("sb", n(standby)),
    ]);
    cx.say("escalators", "station.escalator_angle", "station_well", vec![
        ("len", n(well_len)), ("wid", n(well_wid)), ("rise", n(rise)), ("angle", n(cx.r("station.escalator_angle"))), ("land", n(land)),
    ]);

    // the fixed stair beside each bank: two flights turning on a landing, so a deep rise
    // still fits the platform; its footprint is measured by the stair layout itself
    let sw = cx.r("station.stair_width");
    let riser = cx.r("station.stair_riser");
    let stair_landing = lane;
    let shape = ddc_geom::stair::Shape::U { left: true, first: None, gap: cx.r("station.stair_gap") };
    let def = ddc_geom::stair::StairDef {
        origin: ddc_geom::v2(0.0, 0.0),
        angle: 0.0,
        base_z: 0.0,
        height: rise,
        width: sw,
        max_riser: riser,
        tread: None,
        landing: stair_landing,
        thickness: cx.r("station.stair_waist"),
        stringer: None,
        shape,
    };
    let st = def.layout().map_err(|e| infeasible(format!("stair: {e:?}")))?;
    let pts: Vec<(f64, f64)> = st
        .flights
        .iter()
        .flat_map(|f| f.ring(st.tread))
        .chain(st.landings.iter().flat_map(|l| l.ring.iter().copied()))
        .map(|p| (p.x, p.y))
        .collect();
    let foot = bounds(&pts);
    let (stair_len, stair_span) = (foot.2 - foot.0, foot.3 - foot.1);
    if stair_span > pw - 2.0 * lane {
        return Err(infeasible(format!(
            "the stair takes {} m across, the platform leaves {} m between its edge lanes:              widen station.platform_width or lower station.stair_width",
            n(stair_span), n(pw - 2.0 * lane)
        )));
    }
    cx.say("stairs", "station.stair_width", "station_stairs", vec![
        ("n", banks.to_string()), ("w", n(sw)), ("len", n(stair_len)),
    ]);

    let lifts = cx.r("station.lifts") as usize;
    let ls = cx.r("station.lift_shaft");
    if ls > pw - 2.0 * lane {
        return Err(infeasible(format!(
            "a lift shaft of {} m does not fit between the edge lanes: widen station.platform_width or lower station.lift_shaft",
            n(ls)
        )));
    }
    cx.say("lifts", "station.lifts", "station_lifts", vec![("n", lifts.to_string()), ("s", n(ls))]);

    let mut shafts = vec![];
    let mut stairs = vec![];
    let mut escalators = vec![];
    let mut busy: Vec<(Lvl, Lvl, Rect)> = vec![];
    let entrance_wid = well_wid;
    if lane + entrance_wid > mid - ls * 0.5 {
        return Err(infeasible(format!(
            "an entrance well of {} m does not fit beside the lifts: widen station.trackway_width or station.platform_width",
            n(entrance_wid)
        )));
    }
    let entrance_len = -cz / tan + 2.0 * land;
    for b in 0..banks {
        let own = (0..lifts).filter(|k| k % banks == b).count() as f64;
        let total = own * (ls + lane) + well_len + lane + stair_len;
        let span = pl / banks as f64;
        if total > span {
            return Err(infeasible(format!(
                "bank {} needs {} m of platform, it has {} m: lengthen station.platform_length or lower station.escalator_banks",
                b + 1, n(total), n(span)
            )));
        }
        let mut x = ez + span * (b as f64 + 0.5) - total * 0.5;
        for _ in 0..own as usize {
            let r = (x, mid - ls * 0.5, x + ls, mid + ls * 0.5);
            shafts.push(SShaft { level: Lvl::Platform, top: Lvl::Ground, ring: ring(r), kind: ShaftKind::Lift });
            busy.push((Lvl::Platform, Lvl::Ground, r));
            x += ls + lane;
        }
        let well = (x, mid - well_wid * 0.5, x + well_len, mid + well_wid * 0.5);
        shafts.push(SShaft { level: Lvl::Platform, top: Lvl::Concourse, ring: ring(well), kind: ShaftKind::EscalatorWell });
        busy.push((Lvl::Platform, Lvl::Concourse, well));
        for k in 0..per_bank as usize {
            let y = well.1 + ew * (k as f64 + 0.5);
            escalators.push(SEscalator { bank: b, bottom: (well.0 + land, y, pz), top: (well.2 - land, y, cz), width: ew });
        }
        x = well.2 + lane;
        let sr = (x, mid - stair_span * 0.5, x + stair_len, mid + stair_span * 0.5);
        shafts.push(SShaft { level: Lvl::Platform, top: Lvl::Concourse, ring: ring(sr), kind: ShaftKind::StairWell });
        busy.push((Lvl::Platform, Lvl::Concourse, sr));
        // the layout's origin moved so its footprint fills the well
        let origin = (x - foot.0, mid - (foot.1 + foot.3) * 0.5);
        stairs.push(SStair { level: Lvl::Platform, top: Lvl::Concourse, origin, angle: 0.0, shape, width: sw, max_riser: riser, landing: stair_landing });
        // the way on to the street: a bank of escalators at the side of the concourse
        let c = ez + span * (b as f64 + 0.5);
        let (y0, y1) = if b % 2 == 0 { (lane, lane + entrance_wid) } else { (wid - lane - entrance_wid, wid - lane) };
        let e = (c - entrance_len * 0.5, y0, c + entrance_len * 0.5, y1);
        shafts.push(SShaft { level: Lvl::Concourse, top: Lvl::Ground, ring: ring(e), kind: ShaftKind::EntranceWell });
        busy.push((Lvl::Concourse, Lvl::Ground, e));
        for k in 0..per_bank as usize {
            let y = y0 + ew * (k as f64 + 0.5);
            escalators.push(SEscalator { bank: b, bottom: (e.0 + land, y, cz), top: (e.2 - land, y, 0.0), width: ew });
        }
    }

    // ventilation: a shaft in each end zone over the platform strip, clear of the trackways the
    // trains run through; its section from the duty and the face velocity
    let vf = cx.r("station.vent_flow");
    let vv = cx.r("station.vent_velocity");
    let side = up((vf / vv).sqrt(), step);
    if lane + side > ez || side > pw - 2.0 * lane {
        return Err(infeasible(format!(
            "a ventilation shaft of {} m does not fit the end zone between the trackways: lower station.vent_flow, raise station.vent_velocity, lengthen station.end_zone or widen station.platform_width",
            n(side)
        )));
    }
    cx.say("vent", "station.vent_flow", "station_vent", vec![("n", n(2.0)), ("s", n(side)), ("flow", n(vf)), ("v", n(vv))]);
    for x in [lane, len - lane - side] {
        let r = (x, mid - side * 0.5, x + side, mid + side * 0.5);
        shafts.push(SShaft { level: Lvl::Track, top: Lvl::Ground, ring: ring(r), kind: ShaftKind::Vent });
        busy.push((Lvl::Track, Lvl::Ground, r));
    }

    // columns on the centreline: under the concourse slab and under the roof, clear of the wells
    let csz = cx.r(COLUMN_SIZE);
    let sp = cx.r("station.column_spacing");
    let mut columns = vec![];
    let bays = (pl / sp - 1e-9).ceil();
    let pitch = pl / bays;
    // a column stands on the floors from its base up to, not into, its top; a shaft opens every
    // floor from its base to its top, the top floor's slab included
    let spans = |lo: Lvl, hi: Lvl, s: &(Lvl, Lvl, Rect)| (depth(hi) + 1).max(depth(s.1)) <= depth(lo).min(depth(s.0));
    for k in 1..bays as usize {
        let at = (ez + pitch * k as f64, mid);
        let free = |lo: Lvl, hi: Lvl| {
            busy.iter().filter(|s| spans(lo, hi, s)).all(|s| {
                let r = s.2;
                at.0 + csz < r.0 || at.0 - csz > r.2 || at.1 + csz < r.1 || at.1 - csz > r.3
            })
        };
        if free(Lvl::Platform, Lvl::Concourse) {
            columns.push(SColumn { level: Lvl::Platform, top: Some(Lvl::Concourse), top_offset: -floor, at, size: csz });
        }
        if free(Lvl::Concourse, Lvl::Ground) {
            columns.push(SColumn { level: Lvl::Concourse, top: None, top_offset: roof_bot - cz, at, size: csz });
        }
    }
    cx.say("columns", "station.column_spacing", "station_columns", vec![
        ("n", columns.len().to_string()), ("c", n(csz)), ("sp", n(sp)),
    ]);

    // evacuation: two full trains on the platform, up every escalator but one out of service,
    // and up the stairs
    let people = 2.0 * cx.r("station.train_load");
    let running = (per_bank * banks as f64 - 1.0).max(1.0);
    let per_min = running * cap / 60.0 + banks as f64 * sw * cx.r("station.stair_flow");
    let evac = people / per_min;
    let lim = cx.r("station.evac_time");
    cx.say("evac", "station.evac_time", "station_evac", vec![
        ("t", n(evac)), ("people", n(people)), ("cap", n(per_min)), ("lim", n(lim)),
    ]);
    let mut issues = vec![];

    // barrel vaults over the trackways and the platform, one segment per bay along the box
    let (rise_k, bay, spring) = (cx.r("station.vault_rise"), cx.r("station.vault_bay"), cx.r("station.vault_spring"));
    let nb = (len / bay - 1e-9).ceil().max(1.0) as usize;
    let seg = len / nb as f64;
    let mut vaults = vec![];
    let mut flat = false;
    for (y0, y1) in [(0.0, tw), (tw, tw + pw), (tw + pw, wid)] {
        let span = y1 - y0;
        let wanted = rise_k * span;
        let rise = wanted.min(clear - spring).max(0.0);
        flat |= rise < wanted - 1e-9;
        let profile: Vec<(f64, f64)> = (0..=8)
            .map(|i| {
                let t = i as f64 / 8.0;
                (span * t, rise * 4.0 * t * (1.0 - t))
            })
            .collect();
        for k in 0..nb {
            let x_end = seg * (k as f64 + 1.0);
            vaults.push(SVault { level: Lvl::Platform, a: (x_end, y0), b: (x_end, y1), profile: profile.clone(), depth: seg, offset: clear - rise });
        }
    }
    cx.say("vaults", "station.vault_rise", "station_vaults", vec![("n", vaults.len().to_string()), ("k", n(rise_k)), ("bay", n(seg))]);
    if flat {
        issues.push("a vault is flatter than station.vault_rise asks: the platform clear height leaves no room for the full rise; raise station.rail_depth or lower station.vault_rise".to_string());
    }
    let margin = cx.r("station.ground_margin");
    // the hole is the box with its walls; the strips run `margin` beyond it
    let hole = (-t, -t, len + t, wid + t);
    let out = (hole.0 - margin, hole.1 - margin, hole.2 + margin, hole.3 + margin);
    let ground = vec![
        ring((out.0, hole.1, hole.0, hole.3)),
        ring((hole.2, hole.1, out.2, hole.3)),
        ring((out.0, out.1, out.2, hole.1)),
        ring((out.0, hole.3, out.2, out.3)),
    ];
    cx.say("ground", "station.ground_margin", "station_ground", vec![("m", n(margin))]);
    if evac > lim {
        issues.push(format!(
            "the platform clears in {} min, station.evac_time allows {} min: add escalators (station.peak_flow, station.escalator_banks) or widen station.stair_width",
            n(evac), n(lim)
        ));
    }

    let ch = clear;
    let rooms = vec![
        SRoom { level: Lvl::Platform, class: "platform", name: ("Platform", crate::lang::ru("Platform")), point: (len * 0.5, mid), height: ch },
        SRoom { level: Lvl::Platform, class: "platform_services", name: ("Platform services room", crate::lang::ru("Platform services room")), point: (ez * 0.5, mid), height: ch },
        SRoom { level: Lvl::Platform, class: "platform_services", name: ("Platform services room", crate::lang::ru("Platform services room")), point: (len - ez * 0.5, mid), height: ch },
        SRoom { level: Lvl::Concourse, class: "concourse", name: ("Concourse and ticket hall", crate::lang::ru("Concourse and ticket hall")), point: (len * 0.5, mid), height: roof_bot - cz },
        SRoom { level: Lvl::Concourse, class: "fan_room", name: ("Tunnel ventilation plant room", crate::lang::ru("Tunnel ventilation plant room")), point: (ez * 0.5, mid), height: roof_bot - cz },
        SRoom { level: Lvl::Concourse, class: "substation", name: ("Traction substation and plant", crate::lang::ru("Traction substation and plant")), point: (len - ez * 0.5, mid), height: roof_bot - cz },
    ];

    let metrics = vec![
        ("box_length", len),
        ("box_width", wid),
        ("excavation_depth", exc),
        ("dwall_thickness", t),
        ("lowest_z", bottom),
        ("escalators", per_bank * banks as f64),
        ("entrance_escalators", (escalators.len() as f64) - per_bank * banks as f64),
        ("lifts", lifts as f64),
        ("lift_load", cx.r("station.lift_load")),
        ("shafts", shafts.len() as f64),
        ("vent_shaft_side", side),
        ("evac_min", evac),
        ("screen_doors", doors_per_edge * 2.0),
        ("platform_clear", clear),
        ("vaults", vaults.len() as f64),
    ];
    Ok(GStation {
        levels: vec![(Lvl::Concourse, cz), (Lvl::Platform, pz), (Lvl::Track, bz)],
        inner: (len, wid),
        walls,
        slabs,
        shafts,
        stairs,
        columns,
        rooms,
        escalators,
        vaults,
        ground,
        dwall_thickness: t,
        metrics,
        why: cx.why,
        issues,
    })
}

/// Order of the levels from the street down.
fn depth(l: Lvl) -> usize {
    match l {
        Lvl::Ground => 0,
        Lvl::Concourse => 1,
        Lvl::Platform => 2,
        Lvl::Track => 3,
    }
}
