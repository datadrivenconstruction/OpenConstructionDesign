//! Shared by the electrical and the water services: the cited norm table, the connectors and
//! footprints of the families they work with, room frames, and the check of a finished
//! design against the rules the model applies when the runs are written (fittings fit, every
//! end meets something, one tree per system, approach, fall, size and load) plus clashes.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::{self, Domain, PipeMaterial, Section, Shape};
use serde_json::Value as J;

use super::rules::rule;
use super::{GProject, GRun, PRole, RoomKind, SvcSys};
use crate::{GFixture, GLayout};

/// x0, y0, x1, y1.
pub(crate) type R = [f64; 4];

pub(crate) const EPS: f64 = 1e-6;
/// Slab of a storey, below its level: the floor build-up the rules in force choose.
pub(crate) fn slab() -> f64 {
    super::assembly::floor_thickness()
}
/// Model joint tolerance and connector tolerance.
pub(crate) const TOL: f64 = 1e-3;
const PORT_TOL: f64 = 5e-3;
/// Least gap between two runs, or a run and anything else, m.
pub(crate) fn gap() -> f64 {
    rule("services.gap")
}
/// Ceiling void of a storey without air terminals: the face this far below the slab soffit.
pub(crate) fn void() -> f64 {
    rule("services.void")
}

/// Void layer table: the elevation of a system's axis relative to the storey level, from the rule
/// `mep.layer.<key>` as a depth under the slab soffit. `None` while `mep.layers` is off, then each
/// system keeps its own elevation.
pub(crate) fn layer(height: f64, key: &str) -> Option<f64> {
    depth(key).map(|d| height - slab() - d)
}
/// Axis depth under the soffit of a water main of outside diameter `od` while the layers are on:
/// its own band, and never closer than the gap to the underside of the tray band above it.
pub(crate) fn depth_water(od: f64) -> Option<f64> {
    let tray_h = rule("services.elec.tray_h");
    depth("water").map(|w| w.max(rule("mep.layer.tray") + tray_h / 2.0 + gap() + od / 2.0))
}
/// The depth of a system's axis under the slab soffit while the layers are on.
pub(crate) fn depth(key: &str) -> Option<f64> {
    (rule("mep.layers") > 0.5).then(|| rule(&format!("mep.layer.{key}")))
}

// ---- norms ----

pub(crate) struct Norms {
    j: J,
}

pub(crate) fn norms() -> &'static Norms {
    static N: OnceLock<Norms> = OnceLock::new();
    N.get_or_init(|| Norms {
        j: serde_json::from_str(include_str!("../../data/services_norms.json"))
            .expect("services_norms.json is valid"),
    })
}

/// A display string from `services_text.json` at `path`, in the project language.
pub(crate) fn text(path: &[&str], ru: bool) -> String {
    static T: OnceLock<J> = OnceLock::new();
    let t = T.get_or_init(|| {
        serde_json::from_str(include_str!("../../data/services_text.json"))
            .expect("services_text.json is valid")
    });
    let mut v = t;
    for p in path {
        v = &v[*p];
    }
    v[if ru { "ru" } else { "en" }]
        .as_str().unwrap_or_else(|| panic!("services_text.json: no text at {path:?}")).to_string()
}

pub(crate) fn kind_name(k: RoomKind) -> &'static str {
    use RoomKind::*;
    match k {
        Living => "living",
        Bedroom => "bedroom",
        Kitchen => "kitchen",
        Bath => "bath",
        Wc => "wc",
        Hall => "hall",
        Storage => "storage",
        Office => "office",
        Meeting => "meeting",
        Classroom => "classroom",
        Lab => "lab",
        Staff => "staff",
        Lobby => "lobby",
        Technical => "technical",
        Corridor => "corridor",
        Stair => "stair",
        Lift => "lift",
        Parking => "parking",
        Retail => "retail",
        Dining => "dining",
        Workshop => "workshop",
        Ward => "ward",
        Gym => "gym",
    }
}

impl Norms {
    fn f(&self, path: &[&str]) -> f64 {
        let mut v = &self.j;
        for p in path {
            v = &v[*p];
        }
        v.as_f64()
            .unwrap_or_else(|| panic!("services_norms.json: no number at {path:?}"))
    }

    /// Maintained illuminance of a room, lx: the dwelling row for residential rooms that
    /// have one, else the general row; `None` for a room kind without lighting (lift).
    pub fn lux(&self, k: RoomKind, res: bool) -> Option<f64> {
        let name = kind_name(k);
        let rows = self.j["lux"].as_array()?;
        let find = |r: bool| {
            rows.iter()
                .find(|x| x["room"] == name && x["res"] == r)
                .and_then(|x| x["lx"].as_f64())
        };
        let stair_as = |n: &str| rows.iter().find(|x| x["room"] == n && x["res"] == false);
        if res {
            if let Some(v) = find(true) {
                return Some(v);
            }
        }
        find(false).or_else(|| {
            (k == RoomKind::Stair)
                .then(|| stair_as("stair").and_then(|x| x["lx"].as_f64()))
                .flatten()
        })
    }

    /// Utilisation factor at room index `k`, linear between the cited points, held at the ends.
    pub fn uf(&self, k: f64) -> f64 {
        let pts: Vec<(f64, f64)> = self.j["lumen"]["uf"]
            .as_array()
            .expect("uf")
            .iter()
            .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
            .collect();
        if k <= pts[0].0 {
            return pts[0].1;
        }
        for w in pts.windows(2) {
            if k <= w[1].0 {
                let t = (k - w[0].0) / (w[1].0 - w[0].0);
                return w[0].1 + t * (w[1].1 - w[0].1);
            }
        }
        pts[pts.len() - 1].1
    }

    pub fn mf(&self) -> f64 {
        self.f(&["lumen", "mf"])
    }
    pub fn socket_h(&self) -> f64 {
        self.f(&["heights", "socket"])
    }
    pub fn switch_h(&self) -> f64 {
        self.f(&["heights", "switch"])
    }
    pub fn worktop_h(&self) -> f64 {
        self.f(&["heights", "worktop_socket"])
    }
    /// Centre of the vertical installation zone beside doors and corners.
    pub fn corner_zone(&self) -> f64 {
        let z = &self.j["heights"]["corner_zone"];
        (z[0].as_f64().unwrap() + z[1].as_f64().unwrap()) / 2.0
    }
    pub fn wall_reach(&self) -> f64 {
        self.f(&["sockets", "wall_reach"])
    }
    pub fn worktop_reach(&self) -> f64 {
        self.f(&["sockets", "worktop_reach"])
    }
    pub fn basin_reach(&self) -> f64 {
        self.f(&["sockets", "basin_reach"])
    }
    pub fn zone2(&self) -> f64 {
        self.f(&["sockets", "zone2"])
    }
    pub fn dedicated(&self, fam: &str) -> bool {
        self.j["dedicated"]["kinds"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x == fam))
    }
    pub fn du(&self, fam: &str) -> f64 {
        self.j["drain"]["du"][fam].as_f64().unwrap_or(0.0)
    }
    pub fn branch_dn(&self, fam: &str) -> u32 {
        self.j["drain"]["branch_dn"][fam].as_u64().unwrap_or(rule("services.branch_dn_fallback") as u64) as u32
    }
    /// Fall of a drain of this DN, %: the cited row of the next smaller DN.
    pub fn slope(&self, dn: u32) -> f64 {
        let o = self.j["drain"]["slope"].as_object().expect("slope");
        let mut rows: Vec<(u32, f64)> = o
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), v.as_f64().unwrap()))
            .collect();
        rows.sort_by_key(|r| r.0);
        rows.iter()
            .rev()
            .find(|r| r.0 <= dn)
            .unwrap_or(&rows[0])
            .1
    }
    pub fn k(&self, use_: &str) -> f64 {
        self.f(&["drain", "k", use_])
    }
    pub fn vent_above_roof(&self) -> f64 {
        self.f(&["drain", "vent_above_roof"])
    }
    pub fn lu(&self, fam: &str) -> f64 {
        self.j["supply"]["lu"][fam].as_f64().unwrap_or(0.0)
    }
}

// ---- families ----

/// A connector of a family the services join, z above the family's level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SvcPort {
    pub name: &'static str,
    pub sys: SvcSys,
    pub at: (f64, f64, f64),
    pub dir: (f64, f64, f64),
    /// Connector size, m.
    pub size: f64,
    /// Flow, l/s, or load, W.
    pub flow: f64,
    pub source: bool,
}

type Local = (&'static str, SvcSys, (f64, f64, f64), (f64, f64, f64), f64, f64);

/// The built-in library, as the model keeps it.
const LIBRARY: &str = include_str!("../../../model/data/library.ddcf");

/// What the services need of a library type at its default instance: the connectors in the
/// family frame with their flow (l/s) or load (W) from the type, whether the board is a
/// source, and the plan box of its body.
struct LibType {
    ports: Vec<Local>,
    /// Duct connectors that name their air system: (name, point, size).
    air: Vec<(&'static str, (f64, f64, f64), f64)>,
    source: bool,
    /// None for a ceiling terminal.
    plan: Option<R>,
}

/// The library type of a fixture, read from `library.ddcf` once per type.
fn lib_type(fam: &str, ty: &str) -> Option<&'static LibType> {
    use ddc_geom::component::{Family, Lod};
    use ddc_geom::mep::SystemKind as K;
    static FAMS: OnceLock<HashMap<String, Family>> = OnceLock::new();
    static TYPES: OnceLock<std::sync::Mutex<HashMap<(String, String), Option<&'static LibType>>>> = OnceLock::new();
    let fams = FAMS.get_or_init(|| {
        LIBRARY
            .split("\n---")
            .filter(|s| s.lines().any(|l| l.trim_start().starts_with("family ")))
            .filter_map(|s| Family::parse(s.trim_start_matches('\n')).ok())
            .map(|f| (f.name.clone(), f))
            .collect()
    });
    let mut types = TYPES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let key = (fam.to_string(), ty.to_string());
    if let Some(x) = types.get(&key) {
        return *x;
    }
    let made = (|| {
        let f = fams.get(fam)?;
        let r = f.resolve(&f.family_type(ty)?.values, &[]).ok()?;
        let b = f.build(&r, Lod::Fine).ok()?.bbox;
        let num = |n: &str| r.nums.get(n).map(|q| q.v).filter(|v| v.is_finite()).unwrap_or(0.0);
        let mut ports = vec![];
        let mut air = vec![];
        for p in f.ports(&r).ok()? {
            let (sys, flow) = match p.system {
                Some(K::SupplyAir | K::ReturnAir | K::ExhaustAir) => {
                    air.push((super::fit_rules::intern(&p.name), (p.at.x, p.at.y, p.at.z), p.size));
                    continue;
                }
                Some(K::ColdWater) => (SvcSys::Cold, num("FlowCold")),
                Some(K::HotWater) => (SvcSys::Hot, num("FlowHot")),
                Some(K::Sanitary) => (SvcSys::Sanitary, num("FlowWaste")),
                Some(K::Power) => (SvcSys::Power, num("Power")),
                Some(K::Sprinkler) => (SvcSys::Fire, num("Flow")),
                _ => continue,
            };
            let name = super::fit_rules::intern(&p.name);
            ports.push((name, sys, (p.at.x, p.at.y, p.at.z), (p.dir.x, p.dir.y, p.dir.z), p.size, flow));
        }
        // ceiling terminals take no floor or wall space
        let plan = (f.hosting != ddc_geom::component::Hosting::Ceiling).then_some([b[0].x, b[0].y, b[1].x, b[1].y]);
        let t = LibType { ports, air, source: num("Source") != 0.0, plan };
        Some(&*Box::leak(Box::new(t)))
    })();
    types.insert(key, made);
    made
}

/// Connectors of a library type in the family frame, and whether its `Main` is a source.
fn local_ports(fam: &str, ty: &str) -> (Vec<Local>, bool) {
    lib_type(fam, ty).map_or((vec![], false), |t| (t.ports.clone(), t.source))
}

fn rot(f: &GFixture, p: (f64, f64)) -> (f64, f64) {
    let (c, s) = (f.rotation.cos(), f.rotation.sin());
    (f.at.0 + c * p.0 - s * p.1, f.at.1 + s * p.0 + c * p.1)
}

/// Service connectors of a fixture, in plan with z above the fixture's level.
pub fn svc_ports(f: &GFixture) -> Vec<SvcPort> {
    let (loc, source) = local_ports(f.family, f.ty);
    let (c, s) = (f.rotation.cos(), f.rotation.sin());
    loc.into_iter()
        .map(|(name, sys, p, d, size, flow)| {
            let (x, y) = rot(f, (p.0, p.1));
            SvcPort {
                name,
                sys,
                at: (x, y, f.z + p.2),
                dir: (c * d.0 - s * d.1, s * d.0 + c * d.1, d.2),
                size,
                flow,
                source: source && name == "Main",
            }
        })
        .collect()
}

/// Duct connectors of a fixture that name their air system (a cooker hood's outlet), in plan
/// with z above the fixture's level: (connector, point, size).
pub(crate) fn air_outlets(f: &GFixture) -> Vec<(&'static str, (f64, f64, f64), f64)> {
    let Some(t) = lib_type(f.family, f.ty) else { return vec![] };
    t.air
        .iter()
        .map(|&(name, p, size)| {
            let (x, y) = rot(f, (p.0, p.1));
            (name, (x, y, f.z + p.2), size)
        })
        .collect()
}

/// Footprint of a fixture in plan: the plan box of its library type, set out by the fixture.
pub(crate) fn footprint(f: &GFixture) -> Option<R> {
    let r = lib_type(f.family, f.ty)?.plan?;
    let cs = [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])].map(|p| rot(f, p));
    Some(bbox(&cs))
}

// ---- plan geometry ----

pub(crate) fn bbox(p: &[(f64, f64)]) -> R {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for &(x, y) in p {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    b
}

pub(crate) fn hit(a: &R, b: &R, gap: f64) -> bool {
    a[0] < b[2] + gap - EPS && b[0] < a[2] + gap - EPS && a[1] < b[3] + gap - EPS && b[1] < a[3] + gap - EPS
}

pub(crate) fn inside(p: (f64, f64), r: &R) -> bool {
    p.0 > r[0] + EPS && p.0 < r[2] - EPS && p.1 > r[1] + EPS && p.1 < r[3] - EPS
}

pub(crate) fn grow(r: &R, d: f64) -> R {
    [r[0] - d, r[1] - d, r[2] + d, r[3] + d]
}

/// The wall on that axis line covering `a..b` along it.
pub(crate) fn wall_at(l: &GLayout, horiz: bool, line: f64, a: f64, b: f64) -> Option<usize> {
    let (lo, hi) = (a.min(b), a.max(b));
    l.walls.iter().position(|w| {
        let on = if horiz {
            (w.a.1 - line).abs() < EPS && (w.b.1 - line).abs() < EPS
        } else {
            (w.a.0 - line).abs() < EPS && (w.b.0 - line).abs() < EPS
        };
        let (p, q) = if horiz { (w.a.0, w.b.0) } else { (w.a.1, w.b.1) };
        on && p.min(q) <= lo + EPS && p.max(q) >= hi - EPS
    })
}

/// An opening on a side of a room, in the side's own `s` frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Gap {
    pub s0: f64,
    pub s1: f64,
    pub door: bool,
    pub sill: f64,
    pub head: f64,
}

/// A side of a room's clear rectangle walked counter-clockwise; `n` points into the room.
#[derive(Clone, Debug)]
pub(crate) struct Side {
    pub o: (f64, f64),
    pub t: (f64, f64),
    pub len: f64,
    pub horiz: bool,
    /// Wall axis line and the wall half thickness behind the side (0 without a wall).
    pub line: f64,
    pub half: f64,
    pub ext: bool,
    pub gaps: Vec<Gap>,
}

impl Side {
    pub fn n(&self) -> (f64, f64) {
        (-self.t.1, self.t.0)
    }
    /// Point `s` along the side and `d` into the room (negative: into the wall).
    pub fn at(&self, s: f64, d: f64) -> (f64, f64) {
        let n = self.n();
        (
            self.o.0 + self.t.0 * s + n.0 * d,
            self.o.1 + self.t.1 * s + n.1 * d,
        )
    }
    pub fn s_of(&self, p: (f64, f64)) -> f64 {
        (p.0 - self.o.0) * self.t.0 + (p.1 - self.o.1) * self.t.1
    }
    pub fn d_of(&self, p: (f64, f64)) -> f64 {
        let n = self.n();
        (p.0 - self.o.0) * n.0 + (p.1 - self.o.1) * n.1
    }
    /// Plan direction a wall-hosted family looks into the room with.
    pub fn wall_rot(&self) -> f64 {
        self.t.1.atan2(self.t.0)
    }
    /// Turn of a floor family whose back (local +y) faces this side's wall.
    pub fn back_rot(&self) -> f64 {
        (-self.t.1).atan2(-self.t.0)
    }
    /// No opening within `pad` of `s0..s1` reaching the height band `z0..z1`.
    pub fn free(&self, s0: f64, s1: f64, pad: f64, z0: f64, z1: f64) -> bool {
        self.gaps.iter().all(|g| {
            g.s1 + pad <= s0 + EPS || g.s0 - pad >= s1 - EPS || g.head <= z0 + EPS || g.sill >= z1 - EPS
        })
    }
}

/// A room with its clear rectangle and sides.
#[derive(Clone, Debug)]
pub(crate) struct Room {
    pub idx: usize,
    pub kind: RoomKind,
    pub owner: PRole,
    /// Outline on the wall axes.
    pub rect: R,
    pub clear: R,
    pub sides: [Side; 4],
}

impl Room {
    pub fn area(&self) -> f64 {
        (self.clear[2] - self.clear[0]) * (self.clear[3] - self.clear[1])
    }
}

/// Rooms of a storey whose outline is a rectangle, with their sides and openings.
pub(crate) fn rooms(st: &super::PStorey) -> Vec<Room> {
    let l = &st.layout;
    let mut out = vec![];
    for (i, (pr, gr)) in st.rooms.iter().zip(&l.rooms).enumerate() {
        let r = bbox(&gr.polygon);
        let a = (r[2] - r[0]) * (r[3] - r[1]);
        if gr.polygon.len() != 4 || (super::massing::ring_area(&gr.polygon) - a).abs() > 1e-3 {
            continue;
        }
        let half = |horiz: bool, line: f64, a: f64, b: f64| {
            let mid = (a + b) / 2.0;
            wall_at(l, horiz, line, mid, mid)
                .map(|k| (l.walls[k].role.thickness() / 2.0, l.walls[k].role == crate::WallRole::Exterior))
                .unwrap_or((0.0, false))
        };
        let (ts, es) = half(true, r[1], r[0], r[2]);
        let (te, ee) = half(false, r[2], r[1], r[3]);
        let (tn, en) = half(true, r[3], r[0], r[2]);
        let (tw, ew) = half(false, r[0], r[1], r[3]);
        let c = [r[0] + tw, r[1] + ts, r[2] - te, r[3] - tn];
        let (w, d) = (c[2] - c[0], c[3] - c[1]);
        let side = |o, t, len, horiz, line, half, ext| Side {
            o,
            t,
            len,
            horiz,
            line,
            half,
            ext,
            gaps: vec![],
        };
        let mut sides = [
            side((c[0], c[1]), (1.0, 0.0), w, true, r[1], ts, es),
            side((c[2], c[1]), (0.0, 1.0), d, false, r[2], te, ee),
            side((c[2], c[3]), (-1.0, 0.0), w, true, r[3], tn, en),
            side((c[0], c[3]), (0.0, -1.0), d, false, r[0], tw, ew),
        ];
        for o in &l.openings {
            let wall = &l.walls[o.wall];
            let (dx, dy) = (wall.b.0 - wall.a.0, wall.b.1 - wall.a.1);
            let len = dx.hypot(dy);
            if len < EPS {
                continue;
            }
            let (p0, p1) = (wall.at(o.along), wall.at(o.along + o.width));
            let horiz = dy.abs() < EPS;
            let line = if horiz { wall.a.1 } else { wall.a.0 };
            for sd in sides.iter_mut() {
                if sd.horiz != horiz || (sd.line - line).abs() > EPS {
                    continue;
                }
                let (a, b) = (sd.s_of(p0), sd.s_of(p1));
                let (s0, s1) = (a.min(b), a.max(b));
                if s1 <= EPS || s0 >= sd.len - EPS {
                    continue;
                }
                let sill = if o.door { 0.0 } else { o.sill };
                sd.gaps.push(Gap {
                    s0,
                    s1,
                    door: o.door,
                    sill,
                    head: sill + o.height,
                });
            }
        }
        out.push(Room {
            idx: i,
            kind: pr.kind,
            owner: pr.owner,
            rect: r,
            clear: c,
            sides,
        });
    }
    out
}

/// The ceiling face of a storey above its level: where its air terminals hang, else a void of
/// [`void`] under the slab soffit.
pub(crate) fn ceiling(p: &GProject, k: usize) -> f64 {
    let st = &p.storeys[k];
    st.layout
        .fixtures
        .iter()
        .filter(|f| matches!(f.family, "Supply Diffuser" | "Exhaust Grille"))
        .map(|f| f.z)
        .fold(st.height - slab() - void(), f64::min)
}

// ---- runs ----

pub(crate) fn pipe_run(sys: SvcSys, storey: usize, m: PipeMaterial, dn: u32, pts: Vec<V3>) -> GRun {
    let s = m.by_dn(dn).expect("catalogue DN");
    let mut sec = Section::new(Domain::Pipe, Shape::Round { d: s.od });
    sec.wall = s.wall;
    GRun {
        sys,
        storey,
        points: pts.iter().map(|p| (p.x, p.y, p.z)).collect(),
        section: sec,
        pipe: Some((m, dn)),
        bend: None,
        caps: (false, false),
    }
}

pub(crate) fn conduit_run(storey: usize, d: f64, pts: Vec<V3>) -> GRun {
    GRun {
        sys: SvcSys::Power,
        storey,
        points: pts.iter().map(|p| (p.x, p.y, p.z)).collect(),
        section: Section::new(Domain::Conduit, Shape::Round { d }),
        pipe: None,
        bend: None,
        caps: (false, false),
    }
}

pub(crate) fn tray_run(storey: usize, w: f64, h: f64, pts: Vec<V3>) -> GRun {
    GRun {
        sys: SvcSys::Power,
        storey,
        points: pts.iter().map(|p| (p.x, p.y, p.z)).collect(),
        section: Section::new(Domain::CableTray, Shape::Rect { w, h }),
        pipe: None,
        bend: None,
        caps: (false, false),
    }
}

/// Elbow radius of a run, as the model takes it.
pub(crate) fn radius(r: &GRun, normal_along_up: bool) -> f64 {
    if let Some(b) = r.bend {
        return b;
    }
    if let Some((m, dn)) = r.pipe {
        if let Some(s) = m.by_dn(dn) {
            return m.bend_radius(&s);
        }
    }
    mep::default_bend_radius(&r.section, normal_along_up)
}

/// Largest elbow radius of a run in either plane.
pub(crate) fn bend_max(r: &GRun) -> f64 {
    radius(r, true).max(radius(r, false))
}

pub(crate) fn pts(r: &GRun) -> Vec<V3> {
    r.points.iter().map(|q| v3(q.0, q.1, q.2)).collect()
}

pub(crate) fn len(p: &[V3]) -> f64 {
    p.windows(2).map(|w| (w[1] - w[0]).len()).sum()
}

fn nearest_on(pts: &[V3], q: V3) -> (V3, usize, f64) {
    let mut best = (pts[0], 0, f64::INFINITY);
    for k in 0..pts.len() - 1 {
        let (a, b) = (pts[k], pts[k + 1]);
        let d = b - a;
        let t = ((q - a).dot(d) / d.dot(d)).clamp(0.0, 1.0);
        let p = a + d * t;
        let dist = (q - p).len();
        if dist < best.2 {
            best = (p, k, dist);
        }
    }
    best
}

fn cell(p: V3, s: f64) -> (i64, i64) {
    ((p.x / s).floor() as i64, (p.y / s).floor() as i64)
}

/// Runs by the plan cells their points' box covers.
struct Grid {
    s: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn new(boxes: &[[f64; 6]], s: f64) -> Grid {
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, b) in boxes.iter().enumerate() {
            let (a, z) = (cell(v3(b[0], b[1], 0.0), s), cell(v3(b[3], b[4], 0.0), s));
            for x in a.0..=z.0 {
                for y in a.1..=z.1 {
                    cells.entry((x, y)).or_default().push(i);
                }
            }
        }
        Grid { s, cells }
    }
    fn near(&self, b: &[f64; 6]) -> Vec<usize> {
        let (a, z) = (cell(v3(b[0], b[1], 0.0), self.s), cell(v3(b[3], b[4], 0.0), self.s));
        let mut out = vec![];
        for x in a.0..=z.0 {
            for y in a.1..=z.1 {
                if let Some(v) = self.cells.get(&(x, y)) {
                    out.extend(v);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn pbox(p: &[V3], pad: f64) -> [f64; 6] {
    let mut b = [f64::MAX, f64::MAX, f64::MAX, f64::MIN, f64::MIN, f64::MIN];
    for q in p {
        b = [b[0].min(q.x), b[1].min(q.y), b[2].min(q.z), b[3].max(q.x), b[4].max(q.y), b[5].max(q.z)];
    }
    [b[0] - pad, b[1] - pad, b[2] - pad, b[3] + pad, b[4] + pad, b[5] + pad]
}

/// The network of one system as the model builds it: runs split where another run's end
/// meets their middle, nodes where ends meet.
pub(crate) struct Net {
    /// Run index, axis points, end nodes, and whether each end is the run's own end.
    pub edges: Vec<(usize, Vec<V3>, [usize; 2], [u8; 2])>,
    pub nodes: Vec<V3>,
    pub arms: Vec<Vec<(usize, usize)>>,
}

pub(crate) fn network(runs: &[&GRun]) -> Net {
    let pts: Vec<Vec<V3>> = runs.iter().map(|r| pts(r)).collect();
    let boxes: Vec<[f64; 6]> = pts.iter().map(|p| pbox(p, TOL)).collect();
    let grid = Grid::new(&boxes, 2.0);
    let mut splits: Vec<Vec<(usize, V3)>> = vec![vec![]; runs.len()];
    for (j, pj) in pts.iter().enumerate() {
        for e in [pj[0], *pj.last().unwrap()] {
            for i in grid.near(&pbox(&[e], TOL)) {
                if i == j {
                    continue;
                }
                let pi = &pts[i];
                let (q, k, d) = nearest_on(pi, e);
                let near_end = (q - pi[0]).len() < TOL || (q - *pi.last().unwrap()).len() < TOL;
                if d < TOL && !near_end && !splits[i].iter().any(|s| (s.1 - q).len() < TOL) {
                    splits[i].push((k, q));
                }
            }
        }
    }
    let mut edges = vec![];
    for (i, p) in pts.iter().enumerate() {
        let mut sp = splits[i].clone();
        sp.sort_by(|a, b| a.0.cmp(&b.0).then((a.1 - p[a.0]).len().total_cmp(&(b.1 - p[b.0]).len())));
        let mut cur = vec![p[0]];
        let mut tag0 = 0u8;
        let mut k = 0;
        for seg in 0..p.len() - 1 {
            while k < sp.len() && sp[k].0 == seg {
                cur.push(sp[k].1);
                edges.push((i, std::mem::take(&mut cur), [0, 0], [tag0, 2]));
                cur = vec![sp[k].1];
                tag0 = 2;
                k += 1;
            }
            cur.push(p[seg + 1]);
        }
        edges.push((i, cur, [0, 0], [tag0, 1]));
    }
    let mut nodes: Vec<V3> = vec![];
    let mut arms: Vec<Vec<(usize, usize)>> = vec![];
    let mut at: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    let key = |p: V3| ((p.x / TOL).floor() as i64, (p.y / TOL).floor() as i64, (p.z / TOL).floor() as i64);
    for ei in 0..edges.len() {
        for end in 0..2 {
            let p = if end == 0 { edges[ei].1[0] } else { *edges[ei].1.last().unwrap() };
            let (kx, ky, kz) = key(p);
            let mut found = None;
            'f: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for &n in at.get(&(kx + dx, ky + dy, kz + dz)).into_iter().flatten() {
                            if (nodes[n] - p).len() < TOL {
                                found = Some(n);
                                break 'f;
                            }
                        }
                    }
                }
            }
            let n = found.unwrap_or_else(|| {
                nodes.push(p);
                arms.push(vec![]);
                at.entry((kx, ky, kz)).or_default().push(nodes.len() - 1);
                nodes.len() - 1
            });
            edges[ei].2[end] = n;
            arms[n].push((ei, end));
        }
    }
    Net { edges, nodes, arms }
}

fn depart(p: &[V3], end: usize) -> V3 {
    if end == 0 {
        (p[1] - p[0]).normalized()
    } else {
        (p[p.len() - 2] - p[p.len() - 1]).normalized()
    }
}

/// Faults the model would refuse or report as errors when it writes these runs as one
/// system: fittings that do not fit, nodes of more than four branches, open ends, more than
/// one part, loops, connectors off the network.
pub(crate) fn net_faults(runs: &[&GRun], ports: &[V3]) -> Vec<String> {
    net_faults_at(runs, ports).into_iter().map(|f| f.1).collect()
}

/// [`net_faults`] with the point each fault is at.
pub(crate) fn net_faults_at(runs: &[&GRun], ports: &[V3]) -> Vec<(V3, String)> {
    let mut out: Vec<(V3, String)> = vec![];
    if runs.is_empty() {
        return out;
    }
    let net = network(runs);
    let port_at = |p: V3| ports.iter().any(|q| (*q - p).len() < PORT_TOL);
    let mut trim = vec![[0.0f64; 2]; net.edges.len()];
    for (n, a) in net.arms.iter().enumerate() {
        let at = net.nodes[n];
        let sec = |k: usize| runs[net.edges[a[k].0].0].section;
        match a.len() {
            1 => {
                let (e, end) = a[0];
                let r = runs[net.edges[e].0];
                let tag = net.edges[e].3[end];
                let capped = (tag == 0 && r.caps.0) || (tag == 1 && r.caps.1);
                if port_at(at) {
                } else if r.sys == SvcSys::Power {
                    // conduit and cable end at a board, a box or a device, never capped in the air
                    out.push((at, format!("dangling end at {:?}", (at.x, at.y, at.z))));
                } else if capped {
                    trim[e][end] += mep::cap_length(&r.section);
                } else {
                    out.push((at, format!("open end at {:?}", (at.x, at.y, at.z))));
                }
            }
            2 => {
                let d0 = depart(&net.edges[a[0].0].1, a[0].1);
                let d1 = depart(&net.edges[a[1].0].1, a[1].1);
                let (s0, s1) = (sec(0), sec(1));
                let turn = std::f64::consts::PI - d0.dot(d1).clamp(-1.0, 1.0).acos();
                let min = rule("services.bend_min").to_radians();
                let r_of = |k: usize, inc: V3, out_d: V3| {
                    let axis = inc.cross(out_d).normalized();
                    let fr = mep::start_frame(inc);
                    radius(runs[net.edges[a[k].0].0], axis.cross(fr.up).len() < 1e-6)
                };
                if s0 != s1 && turn > min && turn < std::f64::consts::PI - min {
                    let (ks, kb) = if s0.envelope().area() <= s1.envelope().area() { (0, 1) } else { (1, 0) };
                    let (ds, db) = if ks == 0 { (d0, d1) } else { (d1, d0) };
                    let r = r_of(ks, -ds, db);
                    let t = r * (turn / 2.0).tan();
                    let l = mep::transition_length(&sec(ks), &sec(kb));
                    trim[a[ks].0][a[ks].1] += t;
                    trim[a[kb].0][a[kb].1] += t + l;
                } else if s0 != s1 {
                    let l = mep::transition_length(&s0, &s1);
                    trim[a[0].0][a[0].1] += l / 2.0;
                    trim[a[1].0][a[1].1] += l / 2.0;
                } else if turn > 1e-6 {
                    let r = r_of(0, -d0, d1);
                    let t = r * (turn / 2.0).tan();
                    trim[a[0].0][a[0].1] += t;
                    trim[a[1].0][a[1].1] += t;
                }
            }
            k @ (3 | 4) => {
                let ds: Vec<V3> = a.iter().map(|&(e, end)| depart(&net.edges[e].1, end)).collect();
                let mut best = (0, 1, 2.0);
                for i in 0..k {
                    for j in i + 1..k {
                        let c = ds[i].dot(ds[j]);
                        if c < best.2 {
                            best = (i, j, c);
                        }
                    }
                }
                let leg = mep::junction_leg(&sec(best.0));
                for &(e, end) in a {
                    trim[e][end] += leg;
                }
            }
            k => out.push((at, format!("{k} branches at {:?}", (at.x, at.y, at.z)))),
        }
    }
    for (e, (ri, p, _, _)) in net.edges.iter().enumerate() {
        let r = runs[*ri];
        if let Err(x) = mep::route(p, &|u| radius(r, u), (trim[e][0], trim[e][1])) {
            let axis: Vec<(f64, f64, f64)> = p.iter().map(|q| (q.x, q.y, q.z)).collect();
            out.push((
                p[p.len() / 2],
                format!("run {ri} at {:?}: {x} (end trims {:.3} and {:.3} m on {axis:?})", (p[0].x, p[0].y, p[0].z), trim[e][0], trim[e][1]),
            ));
        }
    }
    for p in ports {
        if !net.nodes.iter().any(|q| (*q - *p).len() < PORT_TOL) {
            out.push((*p, format!("connector at {:?} off the network", (p.x, p.y, p.z))));
        }
    }
    let mut parent: Vec<usize> = (0..net.nodes.len()).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut loops = 0;
    for (_, _, n, _) in &net.edges {
        let (a, b) = (root(&mut parent, n[0]), root(&mut parent, n[1]));
        if a == b {
            loops += 1;
        } else {
            parent[a] = b;
        }
    }
    let parts: HashSet<usize> = (0..net.nodes.len()).map(|i| root(&mut parent, i)).collect();
    if parts.len() > 1 {
        out.push((net.nodes[0], format!("{} parts", parts.len())));
    }
    if loops > 0 {
        out.push((net.nodes[0], format!("{loops} loops")));
    }
    out
}

// ---- check ----

/// World point of a connector of a fixture on storey `k` (z above the project base).
pub(crate) fn port_world(p: &GProject, k: usize, fixture: usize, name: &str) -> Option<SvcPort> {
    let st = &p.storeys[k];
    let f = st.layout.fixtures.get(fixture)?;
    svc_ports(f).into_iter().find(|c| c.name == name).map(|c| SvcPort {
        at: (c.at.0, c.at.1, st.elevation + c.at.2),
        ..c
    })
}

fn nominal_mm(r: &GRun) -> f64 {
    match r.pipe {
        Some((_, dn)) => dn as f64,
        None => {
            let (w, h) = r.section.shape.wh();
            w.min(h) * 1000.0
        }
    }
}

/// Half sizes of a straight piece's box across its axis: (x, y, z).
fn half(sec: &Section, a: V3, b: V3) -> (f64, f64, f64) {
    let (w, h) = sec.envelope().wh();
    let d = b - a;
    let plan = d.x.hypot(d.y);
    if d.z.abs() > plan {
        (w / 2.0, h / 2.0, 0.0)
    } else if d.x.abs() >= d.y.abs() {
        (0.0, w / 2.0, h / 2.0)
    } else {
        (w / 2.0, 0.0, h / 2.0)
    }
}

fn seg_box(sec: &Section, a: V3, b: V3) -> [f64; 6] {
    let (hx, hy, hz) = half(sec, a, b);
    [
        a.x.min(b.x) - hx,
        a.y.min(b.y) - hy,
        a.z.min(b.z) - hz,
        a.x.max(b.x) + hx,
        a.y.max(b.y) + hy,
        a.z.max(b.z) + hz,
    ]
}

fn overlap(a: &[f64; 6], b: &[f64; 6], gap: f64) -> bool {
    (0..3).all(|i| a[i] < b[i + 3] + gap - 2e-3 && b[i] < a[i + 3] + gap - 2e-3)
}

fn on_seg(p: V3, a: V3, b: V3) -> bool {
    nearest_on(&[a, b], p).2 < 2e-3
}

/// Shaft obstacle labels by what the shaft carries: bit 1 ducts, 2 pipes, 4 cables.
pub(crate) const SHAFTS: [&str; 8] = [
    "shaft",
    "shaft [duct]",
    "shaft [riser]",
    "shaft [duct riser]",
    "shaft [cable]",
    "shaft [duct cable]",
    "shaft [riser cable]",
    "shaft [duct riser cable]",
];

/// Whether a run of the system named (`Cold`, `Power`, `duct Supply`, ...) may stand in the
/// shaft the obstacle label names: only a shaft built for its kind takes it. A plain lift
/// shaft (mask 0) takes nothing.
pub(crate) fn shaft_takes(label: &str, sys: &str) -> bool {
    let Some(mask) = SHAFTS.iter().position(|l| *l == label) else { return false };
    let bit = match sys {
        "Cold" | "Hot" | "Sanitary" => 2,
        "Power" => 4,
        s if s.starts_with("duct") => 1,
        _ => 0,
    };
    mask & bit != 0
}

/// Obstacles the services keep clear of: columns, shafts, ducts and air terminals.
pub(crate) fn obstacles(p: &GProject) -> Vec<([f64; 6], &'static str)> {
    obstacles_with(p, true)
}

/// The obstacles, with the air ducts as boxes when `ducts` is set.
pub(crate) fn obstacles_with(p: &GProject, ducts: bool) -> Vec<([f64; 6], &'static str)> {
    let mut out = vec![];
    let top = p.roof.elevation;
    // the terminal catalogue of the ventilation design: face side, neck height, neck size
    let diff600 = (rule("services.hvac.supply_l_face"), rule("services.hvac.supply_l_neck_h"), rule("services.hvac.supply_l_neck_d"));
    let diff = (rule("services.hvac.supply_s_face"), rule("services.hvac.supply_s_neck_h"), rule("services.hvac.supply_s_neck_d"));
    let grille400 = (rule("services.hvac.extract_l_face"), rule("services.hvac.extract_l_neck_h"), rule("services.hvac.extract_l_neck_d"));
    let grille = (rule("services.hvac.extract_s_face"), rule("services.hvac.extract_s_neck_h"), rule("services.hvac.extract_s_neck_d"));
    let (below, above) = (rule("services.terminal_below"), rule("services.terminal_above"));
    for st in &p.storeys {
        let e = st.elevation;
        for c in &st.layout.columns {
            let (hx, hy) = (c.section.0 / 2.0, c.section.1 / 2.0);
            out.push(([c.at.0 - hx, c.at.1 - hy, e, c.at.0 + hx, c.at.1 + hy, e + st.height], "column"));
        }
        for f in &st.layout.fixtures {
            // face plate on the ceiling, the neck above it up to the duct
            let (size, neck, nd) = match (f.family, f.ty) {
                ("Supply Diffuser", "600x600") => diff600,
                ("Supply Diffuser", _) => diff,
                ("Exhaust Grille", "400x400") => grille400,
                ("Exhaust Grille", _) => grille,
                _ => continue,
            };
            let (h, n) = (size / 2.0, nd / 2.0);
            let z = e + f.z;
            out.push(([f.at.0 - h, f.at.1 - h, z - below, f.at.0 + h, f.at.1 + h, z + above], "air terminal"));
            out.push(([f.at.0 - n, f.at.1 - n, z, f.at.0 + n, f.at.1 + n, z + neck], "air terminal neck"));
        }
    }
    if let Some(st) = p.storeys.last() {
        // shafts of one outline are one obstacle, labelled by what they carry
        let mut seen: Vec<([f64; 4], usize)> = vec![];
        for s in &st.layout.shafts {
            let b = bbox(&s.ring);
            let key = [b[0], b[1], b[2], b[3]];
            let bit = match s.kind {
                crate::ShaftKind::Duct => 1,
                crate::ShaftKind::Riser => 2,
                crate::ShaftKind::Electrical => 4,
                crate::ShaftKind::Lift => 0,
            };
            match seen.iter_mut().find(|q| q.0 == key) {
                Some(q) => q.1 |= bit,
                None => seen.push((key, bit)),
            }
        }
        for (b, mask) in seen {
            out.push(([b[0], b[1], 0.0, b[2], b[3], top + rule("services.shaft_over")], SHAFTS[mask]));
        }
    }
    for f in &p.hvac.plant {
        let dims: Vec<f64> = f.ty.split('x').filter_map(|v| v.trim().parse::<f64>().ok()).collect();
        let (l, w) = match dims.as_slice() {
            [l, w, ..] => (l / 1000.0, w / 1000.0),
            _ => (rule("services.ahu_len"), rule("services.ahu_wid")),
        };
        let cs = [(-l / 2.0, -w / 2.0), (l / 2.0, -w / 2.0), (l / 2.0, w / 2.0), (-l / 2.0, w / 2.0)].map(|q| rot(f, q));
        let b = bbox(&cs);
        out.push(([b[0], b[1], top + f.z, b[2], b[3], top + f.z + rule("services.ahu_h")], "air handling unit"));
    }
    for d in p.hvac.ducts.iter().filter(|_| ducts) {
        let shape = if d.size.1 > 0.0 {
            Shape::Rect { w: d.size.0, h: d.size.1 }
        } else {
            Shape::Round { d: d.size.0 }
        };
        let sec = Section::new(Domain::Duct, shape);
        for w in d.points.windows(2) {
            let (a, b) = (v3(w[0].0, w[0].1, w[0].2), v3(w[1].0, w[1].1, w[1].2));
            out.push((seg_box(&sec, a, b), "duct"));
        }
    }
    out
}

/// Every fault of the services of a project, empty when they are sound: the network rules
/// of the model per system, connector approach and size, fall of the drains (at least the
/// model's minimum, never rising away from the outlet), flow speed and conduit load, and
/// clashes with columns, shafts, ducts, air terminals, slabs outside their holes and
/// between runs.
pub fn services_check(p: &GProject) -> Vec<String> {
    let s = &p.services;
    let mut out = vec![];
    let all: Vec<&GRun> = s.water.iter().chain(&s.elec).collect();
    for sys in SvcSys::ALL {
        let runs: Vec<&GRun> = all.iter().copied().filter(|r| r.sys == sys).collect();
        let links: Vec<SvcPort> = s
            .links
            .iter()
            .filter(|l| l.sys == sys)
            .filter_map(|l| port_world(p, l.storey, l.fixture, l.connector))
            .collect();
        if runs.is_empty() {
            if !links.is_empty() {
                out.push(format!("{sys:?}: connectors without runs"));
            }
            continue;
        }
        let ports: Vec<V3> = links.iter().map(|c| v3(c.at.0, c.at.1, c.at.2)).collect();
        out.extend(net_faults(&runs, &ports).into_iter().map(|f| format!("{sys:?}: {f}")));
        check_flows(sys, &runs, &links, &mut out);
    }
    clashes(p, &all, &mut out);
    out.extend(s.issues.iter().map(|i| format!("Power: {INFO}{i}")));
    out
}

/// Marks a services check line that informs (a device left out, told with its reason) rather
/// than an error in the runs.
pub const INFO: &str = "info: ";

/// The services check without its informing lines: what is wrong in the runs themselves.
pub fn services_errors(p: &GProject) -> Vec<String> {
    services_check(p).into_iter().filter(|f| !is_info(f)).collect()
}

/// Whether a services check line informs rather than reports an error.
pub fn is_info(f: &str) -> bool {
    f.split_once(": ").is_some_and(|(_, rest)| rest.starts_with(INFO))
}

/// Section of an air duct as the model writes it.
pub(crate) fn duct_section(size: (f64, f64)) -> Section {
    let shape = if size.1 > 0.0 { Shape::Rect { w: size.0, h: size.1 } } else { Shape::Round { d: size.0 } };
    let mut sec = Section::new(Domain::Duct, shape);
    sec.wall = rule("services.hvac.duct_wall");
    sec
}

/// Ducts of each air system that the model cannot route: a segment shorter than the elbows,
/// tees, crosses and reducers at its two ends, a reversal, a node of more than four branches.
pub fn duct_faults(p: &GProject) -> Vec<String> {
    let mut out = vec![];
    let nets = 1 + p.hvac.nets.len();
    for (sys, net) in [super::AirSys::Supply, super::AirSys::Extract].into_iter().flat_map(|s| (0..nets).map(move |n| (s, n))) {
        let idx: Vec<usize> =
            (0..p.hvac.ducts.len()).filter(|&i| p.hvac.ducts[i].sys == sys && usize::from(p.hvac.ducts[i].net) == net).collect();
        let runs: Vec<GRun> = idx
            .iter()
            .map(|&i| {
                let d = &p.hvac.ducts[i];
                GRun {
                    sys: SvcSys::Power,
                    storey: d.storey,
                    points: d.points.clone(),
                    section: duct_section(d.size),
                    pipe: None,
                    bend: d.bend,
                    caps: (false, false),
                }
            })
            .collect();
        let refs: Vec<&GRun> = runs.iter().collect();
        if refs.is_empty() {
            continue;
        }
        for (at, f) in net_faults_at(&refs, &[]) {
            // open ends are the terminals and the unit, joined by links
            if f.starts_with("open end") || f.starts_with("dangling end") || f.ends_with("parts") {
                continue;
            }
            let f = match f.strip_prefix("run ").and_then(|r| r.split_once(' ')) {
                Some((n, rest)) => match n.parse::<usize>() {
                    Ok(k) => format!("duct {} {rest}", idx[k]),
                    Err(_) => f,
                },
                None => format!("{f} near {:?}", (at.x, at.y, at.z)),
            };
            out.push(format!("{sys:?}: {f}"));
        }
    }
    out
}

/// Approach, size, fall and load the way the model reports them.
fn check_flows(sys: SvcSys, runs: &[&GRun], links: &[SvcPort], out: &mut Vec<String>) {
    let net = network(runs);
    let n = net.nodes.len();
    let node_of = |p: (f64, f64, f64)| net.nodes.iter().position(|q| (*q - v3(p.0, p.1, p.2)).len() < PORT_TOL);
    let mut load = vec![0.0; n];
    let mut source = vec![false; n];
    let (approach_cos, approach_tilt) = (rule("services.approach_cos"), rule("services.approach_tilt"));
    for c in links {
        let Some(k) = node_of(c.at) else { continue };
        if c.source {
            source[k] = true;
        } else {
            load[k] += c.flow;
        }
        let Some(&(e, end)) = net.arms[k].first() else { continue };
        let r = runs[net.edges[e].0];
        if c.size * 1000.0 > nominal_mm(r) + 1.0 {
            out.push(format!("{sys:?}: connector {} {:.0} mm on a {:.0} mm run", c.name, c.size * 1000.0, nominal_mm(r)));
        }
        if sys != SvcSys::Power {
            let p = pts(r);
            let tag = net.edges[e].3[end];
            let leg = if tag == 0 { p[1] - p[0] } else if tag == 1 { p[p.len() - 2] - p[p.len() - 1] } else { continue };
            let skew = leg.normalized().dot(v3(c.dir.0, c.dir.1, c.dir.2)) < approach_cos
                || p.windows(2).any(|w| {
                    let plan = (w[1].x - w[0].x).hypot(w[1].y - w[0].y);
                    let dz = (w[1].z - w[0].z).abs();
                    dz > plan * approach_tilt && dz * approach_tilt < plan
                });
            if skew {
                out.push(format!("{sys:?}: {} at {:?} not approached along its axis", c.name, c.at));
            }
        }
    }
    // root: a source, else the largest open end, for drains the lowest of them
    let score = |i: usize| -> (u8, f64, f64) {
        if source[i] {
            return (2, 0.0, 0.0);
        }
        let open = net.arms[i].len() == 1 && links.iter().all(|c| (v3(c.at.0, c.at.1, c.at.2) - net.nodes[i]).len() >= PORT_TOL);
        let area = net.arms[i].iter().map(|a| runs[net.edges[a.0].0].section.shape.area()).fold(0.0, f64::max);
        let low = if sys == SvcSys::Sanitary { -net.nodes[i].z } else { 0.0 };
        (u8::from(open), area, low)
    };
    let mut best = 0;
    for i in 1..n {
        if score(i) > score(best) {
            best = i;
        }
    }
    let mut adj: Vec<Vec<(usize, usize)>> = vec![vec![]; n];
    for (e, (_, _, nd, _)) in net.edges.iter().enumerate() {
        adj[nd[0]].push((e, nd[1]));
        adj[nd[1]].push((e, nd[0]));
    }
    let mut order = vec![];
    let mut parent: Vec<Option<(usize, usize)>> = vec![None; n];
    let mut seen = vec![false; n];
    seen[best] = true;
    let mut stack = vec![best];
    while let Some(u) = stack.pop() {
        order.push(u);
        for &(e, w) in &adj[u] {
            if !seen[w] {
                seen[w] = true;
                parent[w] = Some((e, u));
                stack.push(w);
            }
        }
    }
    let mut sum = load.clone();
    let mut flow = vec![0.0; net.edges.len()];
    for &u in order.iter().rev() {
        if let Some((e, par)) = parent[u] {
            flow[e] = sum[u];
            sum[par] += sum[u];
            if sys == SvcSys::Sanitary {
                let p = &net.edges[e].1;
                let from_par = (p[0] - net.nodes[par]).len() < TOL;
                let path: Vec<V3> = if from_par { p.clone() } else { p.iter().rev().copied().collect() };
                if path.windows(2).any(|w| w[1].z < w[0].z - 1e-4) {
                    out.push(format!("Sanitary: run falls away from the outlet at {:?}", (p[0].x, p[0].y, p[0].z)));
                }
            }
        }
    }
    let mut run_flow = vec![0.0f64; runs.len()];
    for (e, (ri, _, _, _)) in net.edges.iter().enumerate() {
        run_flow[*ri] = run_flow[*ri].max(flow[e]);
    }
    let (volt, speed) = (rule("services.elec.voltage"), rule("services.cold_speed"));
    let (amp_small, amp_large, small_d) = (rule("services.conduit_small_a"), rule("services.conduit_large_a"), rule("services.conduit_small_d"));
    let (fall_small, fall_large, small_dn) = (rule("services.fall_small"), rule("services.fall_large"), rule("services.sanitary_small_d"));
    let fall_run = rule("services.fall_check_run");
    for (i, r) in runs.iter().enumerate() {
        let area = r.section.inner().area();
        match sys {
            SvcSys::Power if r.section.domain == Domain::Conduit => {
                let limit = if r.section.shape.wh().0 < small_d { amp_small } else { amp_large };
                if run_flow[i] / volt > limit + 1e-9 {
                    out.push(format!("Power: conduit carries {:.1} A, limit {limit}", run_flow[i] / volt));
                }
            }
            SvcSys::Cold | SvcSys::Hot if run_flow[i] / 1000.0 / area > speed + 1e-9 => {
                out.push(format!("{sys:?}: {:.2} m/s in DN{:?}", run_flow[i] / 1000.0 / area, r.pipe.map(|x| x.1)));
            }
            SvcSys::Sanitary => {
                let min = if r.section.inner().wh().0 < small_dn { fall_small } else { fall_large };
                for w in r.points.windows(2) {
                    let run = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
                    let dz = (w[1].2 - w[0].2).abs();
                    if run >= fall_run && dz < run && dz / run * 100.0 + 1e-6 < min {
                        out.push(format!("Sanitary: fall {:.2} % under {min} % at {:?}", dz / run * 100.0, w[0]));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Clashes of the service runs with the building and each other.
fn clashes(p: &GProject, runs: &[&GRun], out: &mut Vec<String>) {
    let mut segs: Vec<(usize, V3, V3, [f64; 6])> = vec![];
    for (i, r) in runs.iter().enumerate() {
        let q = pts(r);
        for w in q.windows(2) {
            segs.push((i, w[0], w[1], seg_box(&r.section, w[0], w[1])));
        }
    }
    let boxes: Vec<[f64; 6]> = segs.iter().map(|s| s.3).collect();
    let grid = Grid::new(&boxes, 2.0);
    let joined = |a: &(usize, V3, V3, [f64; 6]), b: &(usize, V3, V3, [f64; 6])| {
        runs[a.0].sys == runs[b.0].sys
            && (on_seg(a.1, b.1, b.2) || on_seg(a.2, b.1, b.2) || on_seg(b.1, a.1, a.2) || on_seg(b.2, a.1, a.2))
    };
    let mut n = 0;
    let cap = rule("services.clash_report") as usize;
    for (i, a) in segs.iter().enumerate() {
        for j in grid.near(&a.3) {
            if j <= i {
                continue;
            }
            let b = &segs[j];
            if overlap(&a.3, &b.3, 0.0) && !joined(a, b) {
                n += 1;
                if n <= cap {
                    out.push(format!(
                        "clash {:?} {:?}-{:?} with {:?} {:?}-{:?}",
                        runs[a.0].sys, (a.1.x, a.1.y, a.1.z), (a.2.x, a.2.y, a.2.z),
                        runs[b.0].sys, (b.1.x, b.1.y, b.1.z), (b.2.x, b.2.y, b.2.z)
                    ));
                }
            }
        }
    }
    for (o, what) in obstacles(p) {
        for j in grid.near(&o) {
            let b = &segs[j];
            if overlap(&o, &b.3, 0.0) && !shaft_takes(what, &format!("{:?}", runs[b.0].sys)) {
                n += 1;
                if n <= cap {
                    out.push(format!("clash {:?} {:?}-{:?} with a {what}", runs[b.0].sys, (b.1.x, b.1.y, b.1.z), (b.2.x, b.2.y, b.2.z)));
                }
            }
        }
    }
    // slabs: a run crosses a slab only upright and through a hole of that slab
    for (k, st) in p.storeys.iter().enumerate() {
        let (z0, z1) = (st.elevation - slab(), st.elevation);
        let slab = bbox(&st.layout.slab);
        for s in &segs {
            let (lo, hi) = (s.3[2], s.3[5]);
            if hi <= z0 + 2e-3 || lo >= z1 - 2e-3 || !hit(&[s.3[0], s.3[1], s.3[3], s.3[4]], &slab, 0.0) {
                continue;
            }
            let upright = (s.1.x - s.2.x).abs() < 1e-9 && (s.1.y - s.2.y).abs() < 1e-9;
            let holed = upright
                && st.layout.slab_holes.iter().any(|h| {
                    let b = bbox(h);
                    s.3[0] >= b[0] - 1e-6 && s.3[1] >= b[1] - 1e-6 && s.3[3] <= b[2] + 1e-6 && s.3[4] <= b[3] + 1e-6
                });
            if !holed {
                n += 1;
                if n <= cap {
                    out.push(format!("clash {:?} {:?}-{:?} with the slab of storey {k}", runs[s.0].sys, (s.1.x, s.1.y, s.1.z), (s.2.x, s.2.y, s.2.z)));
                }
            }
        }
    }
    if n > cap {
        out.push(format!("{n} clashes in all"));
    }
}

/// Free of the obstacles, the slabs outside `holes` and the runs already placed: what a
/// builder asks before it keeps a candidate route. `holes` are the slab holes it will add.
pub(crate) struct Space<'a> {
    obst: Vec<([f64; 6], &'static str)>,
    p: &'a GProject,
    segs: Vec<(SvcSys, V3, V3, [f64; 6])>,
    grid: HashMap<(i64, i64), Vec<usize>>,
}

impl<'a> Space<'a> {
    pub fn new(p: &'a GProject) -> Space<'a> {
        let mut s = Space {
            obst: obstacles(p),
            p,
            segs: vec![],
            grid: HashMap::new(),
        };
        for r in p.services.water.iter().chain(&p.services.elec) {
            s.add(r);
        }
        s
    }

    pub fn add(&mut self, r: &GRun) {
        let q = pts(r);
        for w in q.windows(2) {
            let b = seg_box(&r.section, w[0], w[1]);
            let i = self.segs.len();
            self.segs.push((r.sys, w[0], w[1], b));
            let (a, z) = (cell(v3(b[0], b[1], 0.0), 2.0), cell(v3(b[3], b[4], 0.0), 2.0));
            for x in a.0..=z.0 {
                for y in a.1..=z.1 {
                    self.grid.entry((x, y)).or_default().push(i);
                }
            }
        }
    }

    pub fn mark(&self) -> usize {
        self.segs.len()
    }

    pub fn rollback(&mut self, mark: usize) {
        if self.segs.len() > mark {
            self.segs.truncate(mark);
            for v in self.grid.values_mut() {
                v.retain(|&i| i < mark);
            }
        }
    }

    /// Adds the runs when every one fits, each against the others too; else leaves the space.
    pub fn try_add(&mut self, runs: &[GRun], gap: f64) -> bool {
        let m = self.mark();
        for r in runs {
            if !self.fits(r, gap) {
                self.rollback(m);
                return false;
            }
            self.add(r);
        }
        true
    }

    /// Whether the run fits: clear of obstacles by `gap`, of other systems' runs by `gap`,
    /// of its own system's runs unless joined to them, and crossing slabs only upright.
    pub fn fits(&self, r: &GRun, gap: f64) -> bool {
        let q = pts(r);
        for w in q.windows(2) {
            let b = seg_box(&r.section, w[0], w[1]);
            let sys = format!("{:?}", r.sys);
            if self.obst.iter().any(|o| overlap(&o.0, &b, gap) && !shaft_takes(o.1, &sys)) {
                return false;
            }
            let upright = (w[0].x - w[1].x).abs() < 1e-9 && (w[0].y - w[1].y).abs() < 1e-9;
            if !upright {
                for st in &self.p.storeys {
                    let (z0, z1) = (st.elevation - slab(), st.elevation);
                    if b[5] > z0 - gap + 2e-3 && b[2] < z1 + gap - 2e-3 {
                        return false;
                    }
                }
            }
            let (a, z) = (cell(v3(b[0], b[1], 0.0), 2.0), cell(v3(b[3], b[4], 0.0), 2.0));
            let mut seen = HashSet::new();
            for x in a.0..=z.0 {
                for y in a.1..=z.1 {
                    for &i in self.grid.get(&(x, y)).into_iter().flatten() {
                        if !seen.insert(i) {
                            continue;
                        }
                        let s = &self.segs[i];
                        let g = if s.0 == r.sys { 0.0 } else { gap };
                        if overlap(&s.3, &b, g) {
                            let joined = s.0 == r.sys
                                && (on_seg(s.1, w[0], w[1]) || on_seg(s.2, w[0], w[1]) || on_seg(w[0], s.1, s.2) || on_seg(w[1], s.1, s.2));
                            if !joined {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        true
    }
}

/// A square hole ring around an upright run of outer size `d`, with the annular gap.
pub(crate) fn hole(x: f64, y: f64, d: f64) -> R {
    let h = d / 2.0 + rule("services.hole_gap");
    [x - h, y - h, x + h, y + h]
}

/// Merges overlapping holes until none overlap, and adds them to the storeys' slabs.
pub(crate) fn add_holes(p: &mut GProject, holes: &[(usize, R)]) {
    for k in 0..p.storeys.len() {
        let mut hs: Vec<R> = holes.iter().filter(|h| h.0 == k).map(|h| h.1).collect();
        if hs.is_empty() {
            continue;
        }
        loop {
            let mut merged = false;
            'outer: for i in 0..hs.len() {
                for j in i + 1..hs.len() {
                    if hit(&hs[i], &hs[j], 0.0) {
                        let (a, b) = (hs[i], hs[j]);
                        hs[i] = [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])];
                        hs.remove(j);
                        merged = true;
                        break 'outer;
                    }
                }
            }
            if !merged {
                break;
            }
        }
        let l = &mut p.storeys[k].layout;
        for h in hs {
            l.slab_holes.push(vec![(h[0], h[1]), (h[2], h[1]), (h[2], h[3]), (h[0], h[3])]);
        }
    }
}

// ---- rings and sizes ----

/// A rectangle around a room `off` inside its clear rectangle (negative: inside the walls),
/// walked counter-clockwise from the south-west corner by the length `u` along it.
pub(crate) struct Ring {
    pub corners: [(f64, f64); 4],
    cum: [f64; 5],
    pub off: f64,
}

const DIRS: [(f64, f64); 4] = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)];

impl Ring {
    pub fn new(room: &Room, off: f64) -> Ring {
        let c = room.clear;
        let corners = [
            (c[0] + off, c[1] + off),
            (c[2] - off, c[1] + off),
            (c[2] - off, c[3] - off),
            (c[0] + off, c[3] - off),
        ];
        let (w, d) = (c[2] - c[0] - 2.0 * off, c[3] - c[1] - 2.0 * off);
        Ring {
            corners,
            cum: [0.0, w, w + d, 2.0 * w + d, 2.0 * (w + d)],
            off,
        }
    }
    pub fn per(&self) -> f64 {
        self.cum[4]
    }
    /// Ring length at `s` along side `j` of the clear rectangle.
    pub fn u(&self, j: usize, s: f64) -> f64 {
        self.cum[j] + s - self.off
    }
    pub fn at(&self, u: f64) -> (f64, f64) {
        let u = u.rem_euclid(self.per());
        let j = (0..4).rev().find(|&j| u >= self.cum[j] - EPS).unwrap_or(0);
        let (c, t) = (self.corners[j], DIRS[j]);
        let s = u - self.cum[j];
        (c.0 + t.0 * s, c.1 + t.1 * s)
    }
    /// The ring from `u0` to `u1`, forward when `u1 > u0`, with the corners between.
    pub fn path(&self, u0: f64, u1: f64) -> Vec<(f64, f64)> {
        let per = self.per();
        let mut ks: Vec<f64> = (-1..=2)
            .flat_map(|m| (0..4).map(move |j| (j, m)))
            .map(|(j, m)| self.cum[j] + m as f64 * per)
            .filter(|&k| k > u0.min(u1) + EPS && k < u0.max(u1) - EPS)
            .collect();
        ks.sort_by(f64::total_cmp);
        if u1 < u0 {
            ks.reverse();
        }
        let mut out = vec![self.at(u0)];
        out.extend(ks.into_iter().map(|k| self.at(k)));
        out.push(self.at(u1));
        out
    }
}

/// Pipe of the supply mains for a flow, l/s: the smallest plastic size within the speed the
/// model allows, steel beyond the plastic range.
pub(crate) fn supply_pipe(q: f64) -> (PipeMaterial, u32) {
    let (speed, dn_min) = (rule("services.cold_speed"), rule("services.main_dn_min") as u32);
    for m in [PipeMaterial::Pp, PipeMaterial::Steel] {
        let mut s = m.sizes();
        s.sort_by_key(|x| x.dn);
        if let Some(x) = s.iter().find(|x| x.dn >= dn_min && q / 1000.0 <= speed * std::f64::consts::PI * x.id().powi(2) / 4.0 - 1e-12) {
            return (m, x.dn);
        }
    }
    (PipeMaterial::Steel, rule("services.main_dn_max") as u32)
}

/// Manning roughness of PVC pipe.
const N_PVC: f64 = 0.013;

/// Half-full capacity of a drain by Manning with the model's roughness, l/s.
pub(crate) fn drain_capacity(id: f64, pct: f64) -> f64 {
    let a = std::f64::consts::PI * id * id / 8.0;
    a * (id / 4.0).powf(2.0 / 3.0) * (pct / 100.0).sqrt() / N_PVC * 1000.0
}

/// Drain DN for a design flow at its fall: the smallest plastic size from `min` up that runs
/// it half full.
pub(crate) fn drain_dn(q: f64, min: u32) -> u32 {
    let mut s = PipeMaterial::Pvc.sizes();
    s.sort_by_key(|x| x.dn);
    s.iter()
        .filter(|x| x.dn >= min)
        .find(|x| drain_capacity(x.id(), norms().slope(x.dn)) >= q)
        .or(s.last())
        .map_or(min, |x| x.dn)
}

pub(crate) fn od(m: PipeMaterial, dn: u32) -> f64 {
    m.by_dn(dn).map_or(rule("services.od_fallback"), |s| s.od)
}

pub(crate) fn bend_of(m: PipeMaterial, dn: u32) -> f64 {
    m.by_dn(dn).map_or(rule("services.bend_fallback"), |s| m.bend_radius(&s))
}

#[cfg(test)]
mod layer_tests {
    use super::*;
    use crate::project::rules::{scoped, Layer, RuleSet};

    #[test]
    fn a_water_main_of_any_dn_clears_the_tray_band() {
        let mut rs = RuleSet::new();
        rs.apply_patch(Layer::Preset, &serde_json::json!({"mep.layers": true})).unwrap();
        scoped(&rs, || {
            let (tray, th) = (rule("mep.layer.tray"), rule("services.elec.tray_h"));
            // depths run down from the soffit: the sprinkler row (0.15) sits above the tray band (0.25), under the ducts' top band
            assert!(rule("mep.layer.sprinkler") + 0.05 < tray - th / 2.0);
            for dn in [15, 20, 25, 32, 40, 50, 65, 80, 100, 125, 150, 200] {
                let od = od(ddc_geom::mep::PipeMaterial::Pvc, dn);
                let d = depth_water(od).expect("layers on");
                // pipe top (depth d - od/2) at least the gap below the tray bottom (tray + th/2)
                assert!(d - od / 2.0 - gap() >= tray + th / 2.0 - 1e-9, "DN{dn}: axis {d}, od {od}");
            }
        });
        assert_eq!(depth_water(0.1), None, "layers off: no table");
    }
}
