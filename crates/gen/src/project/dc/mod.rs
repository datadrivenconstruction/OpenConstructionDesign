// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Data centre fit-out of a built project: rack rows in hot and cold aisles sized from the IT
//! load, cooling units along the hall walls, chillers, UPS on two paths with their batteries,
//! generators in a yard, cable trays over the rows and a dual-path chilled water ring. Rooms
//! are found by canonical tag or by the words of their name (`data/datacenter.json`); every
//! unit is a library family; every count and clearance comes from a `dc.*` rule. Pure: the
//! project is not changed, the caller places what [`design`] returns.

mod hall;
mod plant;
pub mod why;

use std::collections::HashMap;
use std::sync::OnceLock;

use ddc_geom::component::{Family, Lod};
use ddc_geom::math::{v3, V3};
use serde_json::{json, Value as J};

use super::rules::{self, rule};
use super::svc;
use super::{GProject, ProjectSpec, RoomTags};
use crate::project::why::Why;

const LIBRARY: &str = include_str!("../../../../model/data/library.ddcf");
const DATA: &str = include_str!("../../../data/datacenter.json");
const EPS: f64 = 1e-9;

/// Kinds of units the fit-out places, keys of `units` in the data file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Rack,
    Crah,
    Chiller,
    Ups,
    Battery,
    Genset,
    Pdu,
    Door,
}

impl Kind {
    pub const ALL: &'static [Kind] = &[Kind::Rack, Kind::Crah, Kind::Chiller, Kind::Ups, Kind::Battery, Kind::Genset, Kind::Pdu, Kind::Door];

    pub fn key(self) -> &'static str {
        match self {
            Kind::Rack => "rack",
            Kind::Crah => "crah",
            Kind::Chiller => "chiller",
            Kind::Ups => "ups",
            Kind::Battery => "battery",
            Kind::Genset => "genset",
            Kind::Pdu => "pdu",
            Kind::Door => "door",
        }
    }
}

/// The networks the fit-out draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Net {
    ChwSupply,
    ChwReturn,
    PowerA,
    PowerB,
}

impl Net {
    pub const ALL: [Net; 4] = [Net::ChwSupply, Net::ChwReturn, Net::PowerA, Net::PowerB];

    /// Key of the network in `systems` of the data file.
    pub fn key(self) -> &'static str {
        match self {
            Net::ChwSupply => "chw_supply",
            Net::ChwReturn => "chw_return",
            Net::PowerA => "power_a",
            Net::PowerB => "power_b",
        }
    }

    /// The system's name in English or Russian.
    pub fn name(self, ru: bool) -> String {
        data()["systems"][self.key()][if ru { "ru" } else { "en" }].as_str().unwrap_or(self.key()).to_string()
    }

    pub fn kind(self) -> ddc_geom::mep::SystemKind {
        use ddc_geom::mep::SystemKind as K;
        match self {
            Net::ChwSupply | Net::ChwReturn => K::ChilledWater,
            Net::PowerA | Net::PowerB => K::Power,
        }
    }

    pub fn is_pipe(self) -> bool {
        matches!(self, Net::ChwSupply | Net::ChwReturn)
    }
}

/// A placed unit: a library family type standing on a storey.
#[derive(Clone, Debug, PartialEq)]
pub struct DcUnit {
    pub kind: Kind,
    pub family: String,
    pub ty: String,
    pub storey: usize,
    pub at: (f64, f64),
    /// Above the storey level.
    pub z: f64,
    pub rot: f64,
    /// Power path: 0 none, 1 A, 2 B.
    pub path: u8,
}

/// A pipe (`dn` > 0, steel) or a cable tray (`tray` width and height).
#[derive(Clone, Debug, PartialEq)]
pub struct DcRun {
    pub net: Net,
    pub storey: usize,
    /// Axis points, z above the project base.
    pub points: Vec<(f64, f64, f64)>,
    pub dn: u32,
    pub tray: (f64, f64),
    pub caps: (bool, bool),
}

impl DcRun {
    /// Half the outer size: a pipe's outside diameter or a tray's larger side, over two.
    pub fn half(&self) -> f64 {
        if self.dn > 0 {
            ddc_geom::mep::PipeMaterial::Steel.by_dn(self.dn).map_or(0.0, |x| x.od) / 2.0
        } else {
            self.tray.0.max(self.tray.1) / 2.0
        }
    }
}

/// A unit connector joined to a network.
#[derive(Clone, Debug, PartialEq)]
pub struct DcLink {
    pub unit: usize,
    pub connector: String,
    pub net: Net,
}

/// What one hall holds.
#[derive(Clone, Debug, PartialEq)]
pub struct HallFact {
    pub name: String,
    pub storey: usize,
    pub racks: usize,
    pub rows: usize,
    pub it_kw: f64,
    pub crah: usize,
}

/// The fit-out: units, runs, links, the halls and the reasons.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GDc {
    pub units: Vec<DcUnit>,
    pub runs: Vec<DcRun>,
    pub links: Vec<DcLink>,
    pub halls: Vec<HallFact>,
    pub why: Vec<Why>,
    /// Redundancy records: system, duty units, built units (per hall or path for several).
    pub red: Vec<(&'static str, usize, usize)>,
}

impl GDc {
    pub fn is_empty(&self) -> bool {
        self.units.is_empty() && self.runs.is_empty()
    }

    pub fn count(&self, k: Kind) -> usize {
        self.units.iter().filter(|u| u.kind == k).count()
    }

    pub fn pipes(&self) -> usize {
        self.runs.iter().filter(|r| r.dn > 0).count()
    }

    pub fn trays(&self) -> usize {
        self.runs.iter().filter(|r| r.dn == 0).count()
    }

    /// Distinct pipe sizes, smallest first.
    pub fn sizes(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.runs.iter().map(|r| r.dn).filter(|d| *d > 0).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Duty and built units of a redundancy record; the worst of several.
    fn duty_built(&self, sys: &str) -> Option<(usize, usize)> {
        self.red
            .iter()
            .filter(|r| r.0 == sys)
            .map(|r| (r.1, r.2))
            .min_by_key(|(d, b)| *b as i64 - *d as i64)
    }

    /// What each system's built units give: `2N`, `N+k`, `N`, `short`, `dual` (two mains per
    /// hall into a ring) or `none`; the UI words them as `gen.dc.red.<system>` and
    /// `gen.dc.red.scheme.<code>`, `N+k` as `gen.dc.red.scheme.spare`.
    pub fn redundancy_codes(&self) -> Vec<(&'static str, String)> {
        let scheme = |x: Option<(usize, usize)>| match x {
            None => "none".to_string(),
            Some((d, b)) if b < d || d == 0 => "short".to_string(),
            Some((d, b)) if b == d => "N".to_string(),
            Some((d, b)) => format!("N+{}", b - d),
        };
        let paths: Vec<String> = ["ups_a", "ups_b"].iter().filter(|s| self.duty_built(s).is_some()).map(|s| scheme(self.duty_built(s))).collect();
        let ok = |c: &String| c != "short" && c != "none";
        let power = match paths.as_slice() {
            [a, b] if ok(a) && ok(b) => "2N".to_string(),
            [a, b] if ok(a) || ok(b) => if ok(a) { a.clone() } else { b.clone() },
            [a] => a.clone(),
            _ => "none".to_string(),
        };
        let chw = match self.duty_built("chw_main") {
            Some((d, b)) if d > 0 && b == d => "dual".to_string(),
            x => scheme(x),
        };
        vec![
            ("power", power),
            ("crah", scheme(self.duty_built("crah"))),
            ("chiller", scheme(self.duty_built("chiller"))),
            ("genset", scheme(self.duty_built("genset"))),
            ("chw", chw),
        ]
    }

    /// What is redundant and what is not, from what was built.
    pub fn redundancy(&self) -> String {
        let codes = self.redundancy_codes();
        let word = |s: &str| match s {
            "power" => "power (UPS paths)",
            "crah" => "cooling units per hall",
            "chiller" => "chillers",
            "genset" => "generators",
            _ => "chilled water mains",
        };
        let said = |c: &str| match c {
            "2N" => "2N: two UPS paths A and B, each carrying the whole IT load".to_string(),
            "dual" => "dual-path: two mains per hall into a ring".to_string(),
            "short" => "short of the duty units".to_string(),
            "none" => "not built".to_string(),
            "N" => "N, no spare".to_string(),
            x => x.to_string(),
        };
        let parts: Vec<String> = codes.iter().map(|(s, c)| format!("{} {}", word(s), said(c))).collect();
        let not: Vec<&str> = codes.iter().filter(|(s, c)| !(c.starts_with("N+") || c == "dual" || (*s == "power" && c == "2N"))).map(|(s, _)| word(s)).collect();
        let not = if not.is_empty() { "nothing".to_string() } else { not.join(", ") };
        format!("Redundancy: {}. Not redundant: {not}.", parts.join("; "))
    }

    /// The tool reply part.
    pub fn report(&self) -> J {
        let halls: Vec<J> = self
            .halls
            .iter()
            .map(|h| json!({"name": h.name, "storey": h.storey, "racks": h.racks, "rows": h.rows, "it_kw": h.it_kw, "cooling_units": h.crah}))
            .collect();
        json!({
            "halls": halls,
            "racks": self.count(Kind::Rack),
            "cooling_units": self.count(Kind::Crah),
            "chillers": self.count(Kind::Chiller),
            "ups": self.count(Kind::Ups),
            "battery_cabinets": self.count(Kind::Battery),
            "generators": self.count(Kind::Genset),
            "pdus": self.count(Kind::Pdu),
            "containment_doors": self.count(Kind::Door),
            "cable_trays": self.trays(),
            "chilled_water_pipes": self.pipes(),
            "pipe_dn": self.sizes(),
            "redundancy": self.redundancy(),
            "redundancy_codes": self.redundancy_codes().into_iter().collect::<std::collections::BTreeMap<_, _>>(),
            "why": self.why.iter().map(|w| w.to_json()).collect::<Vec<_>>(),
        })
    }
}

fn lib_of_unit(u: &DcUnit) -> Option<Lib> {
    types(u.kind).into_iter().find(|l| l.family == u.family && l.ty == u.ty)
}

/// Plan box of a unit's body: x0, y0, x1, y1.
pub fn plan_box(u: &DcUnit) -> Option<[f64; 4]> {
    lib_of_unit(u).map(|l| footprint(&l, u.at, u.rot, 0.0))
}

/// A unit's height from the library type.
pub fn unit_height(u: &DcUnit) -> Option<f64> {
    lib_of_unit(u).map(|l| l.h())
}

/// World point of a link's connector, z above the project base.
pub fn link_at(p: &GProject, dc: &GDc, k: &DcLink) -> Option<(f64, f64, f64)> {
    let u = dc.units.get(k.unit)?;
    let (q, _) = port_world(u, &lib_of_unit(u)?, &k.connector, p.storeys.get(u.storey)?.elevation)?;
    Some((q.x, q.y, q.z))
}

/// A rule's value as a whole count.
pub(crate) fn count_rule(id: &str) -> usize {
    rule(id).round().max(0.0) as usize
}

pub(crate) fn data() -> &'static J {
    static D: OnceLock<J> = OnceLock::new();
    D.get_or_init(|| serde_json::from_str(DATA).expect("datacenter.json"))
}

/// A library type as the layout needs it: plan box in local axes, height, capacity, ports.
#[derive(Clone, Debug)]
pub(crate) struct Lib {
    pub family: String,
    pub ty: String,
    /// Local box: min and max corner.
    pub lo: V3,
    pub hi: V3,
    pub cap: f64,
    /// Name, local point, local direction, size.
    pub ports: Vec<(String, V3, V3, f64)>,
}

impl Lib {
    pub fn w(&self) -> f64 {
        self.hi.x - self.lo.x
    }
    pub fn d(&self) -> f64 {
        self.hi.y - self.lo.y
    }
    pub fn h(&self) -> f64 {
        self.hi.z
    }
    pub fn port(&self, name: &str) -> Option<&(String, V3, V3, f64)> {
        self.ports.iter().find(|p| p.0 == name)
    }
}

/// The library's families by name, and the text of each block that did not parse (a family that
/// fails here would silently leave its units out of the fit-out).
fn library() -> &'static (HashMap<String, Family>, Vec<String>) {
    static F: OnceLock<(HashMap<String, Family>, Vec<String>)> = OnceLock::new();
    F.get_or_init(|| {
        let (mut ok, mut bad) = (HashMap::new(), vec![]);
        for s in LIBRARY
            .split("\n---")
            .filter(|s| s.lines().any(|l| l.trim_start().starts_with("family ")))
        {
            match Family::parse(s.trim_start_matches('\n')) {
                Ok(f) => {
                    ok.insert(f.name.clone(), f);
                }
                Err(e) => {
                    let head = s.lines().find(|l| l.trim_start().starts_with("family ")).unwrap_or("?");
                    bad.push(format!("{}: {e:?}", head.trim()));
                }
            }
        }
        (ok, bad)
    })
}

fn families() -> &'static HashMap<String, Family> {
    &library().0
}

#[cfg(test)]
mod library_tests {
    use super::*;

    #[test]
    fn every_library_family_parses_and_every_kind_has_its_types() {
        let bad = &library().1;
        assert!(bad.is_empty(), "library families that do not parse: {bad:?}");
        for k in Kind::ALL {
            assert!(!types(*k).is_empty(), "no library type resolves for {k:?}");
        }
    }
}

fn lib_of(family: &str, ty: &str, cap: &str) -> Option<Lib> {
    let f = families().get(family)?;
    let r = f.resolve(&f.family_type(ty)?.values, &[]).ok()?;
    let b = f.build(&r, Lod::Fine).ok()?.bbox;
    let ports = f.ports(&r).ok()?.into_iter().map(|p| (p.name, p.at, p.dir, p.size)).collect();
    let cap = r.nums.get(cap).map(|q| q.v).unwrap_or(0.0);
    Some(Lib { family: family.to_string(), ty: ty.to_string(), lo: b[0], hi: b[1], cap, ports })
}

/// The library types of a unit kind, in the data file's order.
pub(crate) fn types(k: Kind) -> Vec<Lib> {
    let u = &data()["units"][k.key()];
    let fam = u["family"].as_str().unwrap_or("");
    let cap = u["capacity"].as_str().unwrap_or("");
    u["types"].as_array().into_iter().flatten().filter_map(|t| lib_of(fam, t.as_str()?, cap)).collect()
}

/// Local to plan: turn by `rot` and move to `at`.
pub(crate) fn place(at: (f64, f64), rot: f64, p: V3) -> (f64, f64) {
    let (c, s) = (rot.cos(), rot.sin());
    (at.0 + p.x * c - p.y * s, at.1 + p.x * s + p.y * c)
}

pub(crate) fn turn(rot: f64, d: V3) -> V3 {
    let (c, s) = (rot.cos(), rot.sin());
    v3(d.x * c - d.y * s, d.x * s + d.y * c, d.z)
}

/// Plan corners of a unit's box, grown by `g`.
pub(crate) fn footprint(l: &Lib, at: (f64, f64), rot: f64, g: f64) -> svc_box::R {
    let cs = [(l.lo.x - g, l.lo.y - g), (l.hi.x + g, l.lo.y - g), (l.hi.x + g, l.hi.y + g), (l.lo.x - g, l.hi.y + g)]
        .map(|(x, y)| place(at, rot, v3(x, y, 0.0)));
    svc_box::bbox(&cs)
}

pub(crate) mod svc_box {
    pub type R = [f64; 4];
    pub fn bbox(p: &[(f64, f64)]) -> R {
        super::svc::bbox(p)
    }
    /// Boxes overlap by more than the tolerance.
    pub fn hit(a: &R, b: &R) -> bool {
        a[0] < b[2] - super::EPS && b[0] < a[2] - super::EPS && a[1] < b[3] - super::EPS && b[1] < a[3] - super::EPS
    }
}

/// The world point and direction of a unit's port.
pub(crate) fn port_world(u: &DcUnit, l: &Lib, name: &str, elev: f64) -> Option<(V3, V3)> {
    let p = l.port(name)?;
    let xy = place(u.at, u.rot, p.1);
    Some((v3(xy.0, xy.1, elev + u.z + p.1.z), turn(u.rot, p.2)))
}

/// What a room is for in the data centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Hall,
    Ups,
    Battery,
    Cooling,
}

/// A room the fit-out uses: its storey and the clear rectangle with sides.
pub(crate) struct DcRoom {
    pub storey: usize,
    pub name: String,
    pub room: svc::Room,
}

/// Whether the data centre fit-out serves this room (hall, UPS, battery, cooling plant): its
/// heat leaves on the chilled water of the cooling units, so the building's air system leaves
/// the room alone.
pub(crate) fn serves(spec: &ProjectSpec, kind: super::RoomKind, name: &str) -> bool {
    role_of(spec, kind, name).is_some()
}

fn role_of(spec: &ProjectSpec, kind: super::RoomKind, name: &str) -> Option<Role> {
    let tags = spec.room_tags.of(kind, name);
    let low = name.to_lowercase();
    let roles = data()["roles"].as_array()?;
    let by = |f: &dyn Fn(&J) -> bool| {
        roles.iter().find(|r| f(r)).and_then(|r| match r["role"].as_str()? {
            "hall" => Some(Role::Hall),
            "ups" => Some(Role::Ups),
            "battery" => Some(Role::Battery),
            "cooling" => Some(Role::Cooling),
            _ => None,
        })
    };
    by(&|r| r["tag"].as_str().and_then(RoomTags::parse).is_some_and(|t| tags.meets(t))).or_else(|| {
        by(&|r| r["name_has"].as_array().into_iter().flatten().filter_map(|w| w.as_str()).any(|w| low.contains(w)))
    })
}

/// The brief asks for a data hall: a room of its kind, with its tag, or named as one.
pub(crate) fn brief_has_hall(v: &J) -> bool {
    let roles = data()["roles"].as_array();
    let Some(hall) = roles.into_iter().flatten().find(|r| r["role"].as_str() == Some("hall")) else {
        return false;
    };
    let tag = hall["tag"].as_str().unwrap_or("data_hall");
    let words: Vec<&str> = hall["name_has"].as_array().into_iter().flatten().filter_map(|w| w.as_str()).collect();
    v["rooms"].as_array().into_iter().flatten().any(|r| {
        let low = r["name"].as_str().unwrap_or_default().to_lowercase();
        r["kind"].as_str() == Some(tag)
            || r["tags"].as_array().into_iter().flatten().any(|t| t.as_str() == Some(tag))
            || words.iter().any(|w| low.contains(w))
    })
}

/// Rooms of the storeys above ground by role, in storey and room order.
pub(crate) fn rooms_by_role(spec: &ProjectSpec, p: &GProject) -> HashMap<&'static str, Vec<DcRoom>> {
    let mut out: HashMap<&'static str, Vec<DcRoom>> = HashMap::new();
    for (k, st) in p.storeys.iter().enumerate() {
        for r in svc::rooms(st) {
            let name = st.layout.rooms[r.idx].name.clone();
            let Some(role) = role_of(spec, r.kind, &name) else { continue };
            let key = match role {
                Role::Hall => "hall",
                Role::Ups => "ups",
                Role::Battery => "battery",
                Role::Cooling => "cooling",
            };
            out.entry(key).or_default().push(DcRoom { storey: k, name, room: r });
        }
    }
    out
}

/// Plan boxes of the columns of a storey.
pub(crate) fn columns(p: &GProject, k: usize) -> Vec<svc_box::R> {
    p.storeys[k]
        .layout
        .columns
        .iter()
        .map(|c| {
            let (hx, hy) = (c.section.0 / 2.0, c.section.1 / 2.0);
            [c.at.0 - hx, c.at.1 - hy, c.at.0 + hx, c.at.1 + hy]
        })
        .collect()
}

/// Smallest steel DN whose bore keeps the flow (m3/s) at most `dc.chw_v`; the largest when
/// none does. Returns the DN and the velocity.
pub(crate) fn dn_for(flow: f64) -> (u32, f64) {
    let sizes = ddc_geom::mep::PipeMaterial::Steel.sizes();
    let vmax = rule("dc.chw_v");
    let vel = |s: &ddc_geom::mep::PipeSize| {
        let d = s.od - s.wall * 2.0;
        flow / (std::f64::consts::PI * d * d / 4.0)
    };
    let pick = sizes.iter().find(|s| vel(s) <= vmax + EPS).or(sizes.last());
    pick.map_or((0, 0.0), |s| (s.dn, vel(s)))
}

/// Chilled water flow in m3/s carrying `kw` at the design rise `dc.chw_dt`.
pub(crate) fn flow_of(kw: f64) -> f64 {
    let w = &data()["water"];
    let (cp, rho) = (w["cp"].as_f64().unwrap_or(f64::NAN), w["rho"].as_f64().unwrap_or(f64::NAN));
    kw / (cp * rule("dc.chw_dt") * rho)
}

pub(crate) fn pipe(net: Net, storey: usize, dn: u32, pts: &[V3]) -> DcRun {
    DcRun { net, storey, points: pts.iter().map(|p| (p.x, p.y, p.z)).collect(), dn, tray: (0.0, 0.0), caps: (false, false) }
}

pub(crate) fn tray(net: Net, storey: usize, pts: &[V3], caps: (bool, bool)) -> DcRun {
    DcRun {
        net,
        storey,
        points: pts.iter().map(|p| (p.x, p.y, p.z)).collect(),
        dn: 0,
        tray: (rule("dc.tray_w"), rule("dc.tray_h")),
        caps,
    }
}

/// Racks of a hall's clear rectangle by the rules, for each row direction: all and per row.
pub fn capacity(w: f64, d: f64) -> [(usize, usize); 2] {
    hall::capacity(w, d)
}

/// Floor area to ask for a hall of `racks`, from `dc.hall_m2_rack`.
pub fn hall_area(racks: usize) -> f64 {
    racks as f64 * rule("dc.hall_m2_rack")
}

/// The data centre fit-out of a built project; empty when no room is a data hall.
pub fn design(spec: &ProjectSpec, p: &GProject) -> GDc {
    rules::scoped(&spec.rules, || {
        let mut rooms = rooms_by_role(spec, p);
        let halls = rooms.remove("hall").unwrap_or_default();
        if halls.is_empty() {
            return GDc::default();
        }
        let mut dc = GDc::default();
        let mut w = why::Whys::new(spec);
        let laid = hall::lay_halls(p, &halls, &mut dc, &mut w);
        plant::plant(p, &rooms, &laid, &mut dc, &mut w);
        dc.why = w.out;
        dc.red = w.red;
        dc
    })
}

/// Brief features the fit-out answers: (feature, built).
pub fn features(spec: &ProjectSpec, dc: &GDc) -> Vec<(&'static str, bool)> {
    rules::scoped(&spec.rules, || vec![
        ("data_hall", !dc.halls.is_empty()),
        ("hot_cold_aisle", dc.count(Kind::Rack) > 0),
        ("raised_floor", dc.count(Kind::Rack) > 0 && rule("dc.raised_floor") > 0.0),
        ("crah", dc.count(Kind::Crah) > 0),
        ("chilled_water", dc.pipes() > 0),
        ("ups", dc.count(Kind::Ups) > 0),
        ("generators_outside", dc.count(Kind::Genset) > 0),
        ("cable_tray", dc.trays() > 0),
        ("aisle_containment", dc.count(Kind::Door) > 0),
        ("pdu", dc.count(Kind::Pdu) > 0),
    ])
}
