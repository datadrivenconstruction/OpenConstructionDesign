//! Room heating and cooling loads at the design conditions of the rules (a country pack sets
//! the climate and the envelope through their norm keys). Heating: transmission through the
//! exterior walls, windows, roof and ground floor plus infiltration. Cooling, sensible:
//! walls and roof at their sol-air excess, window conduction, solar gain through the glass by
//! the facade's orientation, people, lighting and equipment; latent: people. Outdoor air per
//! person and per floor area.

use super::super::rules::rule;
use super::super::{GProject, ProjectSpec, RoomKind, Typology};
use super::geo::{area, bbox2, centroid, inside};
use crate::WallRole;
use ddc_geom::mep::duct_sizing::RHO;
use ddc_geom::mep::fittings::CP_AIR;

/// W per (m3/h K) of air.
pub fn air_wh() -> f64 {
    RHO * CP_AIR / 3600.0
}

/// One exterior wall of a room: compass azimuth of its outward normal (0 north, 90 east),
/// opaque and glazed area.
#[derive(Clone, Debug, PartialEq)]
pub struct Facade {
    pub azimuth: f64,
    pub wall: f64,
    pub glass: f64,
}

pub const HEAT_PARTS: [&str; 5] = ["wall", "window", "roof", "ground", "infiltration"];
pub const COOL_PARTS: [&str; 7] = ["wall", "window", "solar", "roof", "people", "lighting", "equipment"];

/// Loads of one room, W, and its outdoor air, m3/h.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomLoad {
    pub storey: usize,
    pub room: usize,
    pub kind: RoomKind,
    pub number: String,
    pub area: f64,
    pub volume: f64,
    pub people: f64,
    pub facades: Vec<Facade>,
    /// Roof and ground floor area over and under the room.
    pub roof: f64,
    pub ground: f64,
    pub heat: [f64; 5],
    pub cool: [f64; 7],
    pub latent: f64,
    pub oa: f64,
}

impl RoomLoad {
    pub fn heating(&self) -> f64 {
        self.heat.iter().sum()
    }
    pub fn sensible(&self) -> f64 {
        self.cool.iter().sum()
    }
}

pub fn residential(t: Typology) -> bool {
    matches!(t, Typology::House | Typology::ApartmentSection)
}

/// Rooms kept at the design temperatures.
pub fn conditioned(k: RoomKind) -> bool {
    !matches!(k, RoomKind::Stair | RoomKind::Lift | RoomKind::Parking | RoomKind::Technical)
}

/// Floor area per person, m2; zero for rooms nobody stays in.
fn per_person(k: RoomKind, res: bool) -> f64 {
    use RoomKind::*;
    if !k.habitable() || k == Kitchen && res {
        return 0.0;
    }
    if res {
        return rule("mep.hvac.occ_res");
    }
    match k {
        Office => rule("mep.hvac.occ_office"),
        Meeting => rule("mep.hvac.occ_meeting"),
        Classroom => rule("mep.hvac.occ_classroom"),
        Dining => rule("mep.hvac.occ_dining"),
        Retail => rule("mep.hvac.occ_retail"),
        _ => rule("mep.hvac.occ_other"),
    }
}

/// Equipment gain, W/m2.
fn equipment(k: RoomKind, res: bool) -> f64 {
    use RoomKind::*;
    match k {
        Kitchen => rule("mep.hvac.equip_kitchen"),
        _ if !k.habitable() => 0.0,
        _ if res => rule("mep.hvac.equip_res"),
        Office | Lab | Meeting | Classroom | Staff | Workshop => rule("mep.hvac.equip_office"),
        _ => rule("mep.hvac.equip_other"),
    }
}

/// Peak solar irradiance on a vertical wall facing `az` degrees, W/m2: the four compass
/// values of the rules, linear between them.
pub fn solar(az: f64) -> f64 {
    let v = [
        rule("mep.hvac.solar_n"),
        rule("mep.hvac.solar_e"),
        rule("mep.hvac.solar_s"),
        rule("mep.hvac.solar_w"),
    ];
    let a = az.rem_euclid(360.0) / 90.0;
    let i = a.floor() as usize % 4;
    let t = a - a.floor();
    v[i] * (1.0 - t) + v[(i + 1) % 4] * t
}

/// The exterior walls of room `room` on storey `k`: a wall bounds the room where a room edge
/// runs along it within the wall's thickness.
pub fn facades(p: &GProject, k: usize, room: usize) -> Vec<Facade> {
    let st = &p.storeys[k];
    let l = &st.layout;
    let poly = &l.rooms[room].polygon;
    let c = centroid(poly);
    let reach = WallRole::Exterior.thickness();
    let mut out = vec![];
    for (wi, w) in l.walls.iter().enumerate() {
        if w.role != WallRole::Exterior {
            continue;
        }
        let (a, b) = (w.a, w.b);
        let wl = (b.0 - a.0).hypot(b.1 - a.1);
        if wl < 1e-9 {
            continue;
        }
        let u = ((b.0 - a.0) / wl, (b.1 - a.1) / wl);
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for i in 0..poly.len() {
            let (p0, p1) = (poly[i], poly[(i + 1) % poly.len()]);
            let e = (p1.0 - p0.0, p1.1 - p0.1);
            let el = e.0.hypot(e.1);
            if el < 1e-9 || (u.0 * e.1 - u.1 * e.0).abs() / el > 1e-3 {
                continue;
            }
            let off = |q: (f64, f64)| ((q.0 - a.0) * -u.1 + (q.1 - a.1) * u.0).abs();
            if off(p0).max(off(p1)) > reach {
                continue;
            }
            let t = |q: (f64, f64)| (q.0 - a.0) * u.0 + (q.1 - a.1) * u.1;
            let (t0, t1) = (t(p0).min(t(p1)).max(0.0), t(p0).max(t(p1)).min(wl));
            if t1 - t0 > 1e-6 {
                lo = lo.min(t0);
                hi = hi.max(t1);
            }
        }
        if hi <= lo {
            continue;
        }
        let mut n = (u.1, -u.0);
        if (c.0 - a.0) * n.0 + (c.1 - a.1) * n.1 > 0.0 {
            n = (-n.0, -n.1);
        }
        let glass: f64 = l
            .openings
            .iter()
            .filter(|o| o.wall == wi && !o.door)
            .filter(|o| {
                let m = o.along + o.width / 2.0;
                m >= lo && m <= hi
            })
            .map(|o| o.width * o.height)
            .sum();
        let az = (n.0.atan2(n.1).to_degrees() + rule("mep.hvac.north_deg")).rem_euclid(360.0);
        out.push(Facade { azimuth: az, wall: ((hi - lo) * st.height - glass).max(0.0), glass });
    }
    out
}

/// Loads of every conditioned room above ground, but the rooms of the data centre fit-out.
pub fn room_loads(spec: &ProjectSpec, res: bool, p: &GProject) -> Vec<RoomLoad> {
    let (ti, te) = (rule("mep.hvac.t_in_winter"), rule("mep.hvac.t_out_winter"));
    let (tc, ts) = (rule("mep.hvac.t_in_summer"), rule("mep.hvac.t_out_summer"));
    let tg = rule("mep.hvac.t_ground");
    let (uw, uf, ur, ug) = (
        rule("mep.hvac.u_wall"),
        rule("mep.hvac.u_window"),
        rule("mep.hvac.u_roof"),
        rule("mep.hvac.u_floor"),
    );
    let gain = rule("mep.hvac.g_window") * rule("mep.hvac.shade");
    let n = p.storeys.len();
    let mut out = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        let clear = st.height - super::super::svc::slab();
        for (i, (pr, gr)) in st.rooms.iter().zip(&st.layout.rooms).enumerate() {
            if !conditioned(pr.kind) || super::super::dc::serves(spec, pr.kind, &gr.name) {
                continue;
            }
            let a = area(&gr.polygon);
            let c = centroid(&gr.polygon);
            let covered = k + 1 < n && inside(c, &p.storeys[k + 1].layout.slab);
            let roof = if covered { 0.0 } else { a };
            let ground = if k == 0 { a } else { 0.0 };
            let fac = facades(p, k, i);
            let (wall, glass): (f64, f64) = (fac.iter().map(|f| f.wall).sum(), fac.iter().map(|f| f.glass).sum());
            let dh = ti - te;
            let infil = if fac.is_empty() { 0.0 } else { air_wh() * rule("mep.hvac.infiltration") * a * clear * dh };
            let heat = [uw * wall * dh, uf * glass * dh, ur * roof * dh, ug * ground * (ti - tg), infil];
            let per = per_person(pr.kind, res);
            let people = if per > 0.0 { (a / per).ceil() } else { 0.0 };
            let sun: f64 = fac.iter().map(|f| f.glass * gain * solar(f.azimuth)).sum();
            let dc = ts - tc;
            let cool = [
                uw * wall * (dc + rule("mep.hvac.cltd_wall")),
                uf * glass * dc,
                sun,
                ur * roof * (dc + rule("mep.hvac.cltd_roof")),
                people * rule("mep.hvac.person_sens"),
                rule("mep.hvac.light") * a,
                equipment(pr.kind, res) * a,
            ];
            let oa = if pr.kind.habitable() {
                rule("mep.hvac.oa_person") * people + rule("mep.hvac.oa_area") * a
            } else {
                0.0
            };
            out.push(RoomLoad {
                storey: k,
                room: i,
                kind: pr.kind,
                number: pr.number.clone(),
                area: a,
                volume: a * clear,
                people,
                facades: fac,
                roof,
                ground,
                heat: heat.map(|x| x.max(0.0)),
                cool: cool.map(|x| x.max(0.0)),
                latent: people * rule("mep.hvac.person_lat"),
                oa,
            });
        }
    }
    out
}

/// Plan box of a room, for placing its units.
pub fn room_box(p: &GProject, k: usize, room: usize) -> [f64; 4] {
    bbox2(&p.storeys[k].layout.rooms[room].polygon)
}
