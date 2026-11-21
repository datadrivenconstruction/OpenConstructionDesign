// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Mechanical ventilation of a project. Each room gets the air its kind and size ask for:
//! habitable rooms a supply diffuser, wet rooms and kitchens an extract grille (rooms of
//! non-residential buildings both, balanced). Mains run in the corridor ceiling void, supply
//! above extract so that branches cross; risers stand in a ventilation shaft beside the
//! corridor; the air handling unit sits on the roof (in the attic under a pitched roof).
//! Ducts are sized by equal friction, and runs meet only end to end, so each system builds as
//! one tree from the unit to every terminal.

use super::rules::rule;
use super::svc;
use ddc_geom::mep;
use super::{AirSys, GAirLink, GDuct, GHvac, GProject, ProjectSpec, RoofKind, RoomKind, Typology};
use crate::{GFixture, GShaft, GenError};
use ddc_geom::mep::duct_sizing::{
    friction_rect, pick_rect, pick_round, velocity, MAX_ASPECT, ROUND_MM,
};
use std::f64::consts::PI;

/// The design numbers the layout reads on its hot paths, read once per design from the rules.
#[derive(Clone, Copy, Debug)]
struct Dn {
    /// Equal-friction rate, Pa/m, and velocity caps, m/s.
    friction: f64,
    v_main: f64,
    v_branch: f64,
    v_riser: f64,
    /// Between the main's top and the slab soffit.
    top_gap: f64,
    /// Between the supply and the extract layer.
    layer_gap: f64,
    /// Between a drop elbow and the terminal neck.
    drop_gap: f64,
    /// Unit base above the roof level: a plinth on a flat roof, joists in an attic.
    plinth: f64,
    /// Clearance round a riser in the shaft.
    shaft_clear: f64,
    /// Elbow radius of a branch drop to its terminal, by diameter.
    drop_bend: f64,
    /// Elbow radius of a round duct by its diameter: the duct default.
    bend: f64,
    margin: f64,
    /// Terminal centre kept off the room walls.
    wall_keep: f64,
    /// Duct size step, m.
    step: f64,
    /// Reducer half-angle, degrees, and the least reducer length.
    red_angle: f64,
    red_min: f64,
    /// Terminal flows round up by this, m3/h.
    flow_step: f64,
    deepen_step: f64,
    deepen_steps: usize,
    riser_gap: f64,
    shaft_off: f64,
    fixture_keep: f64,
    corr_end: f64,
    unit_clear: f64,
    roof_edge: f64,
    pref_weight: f64,
    low_clear: f64,
    riser_sep: f64,
    unit_riser_clear: f64,
    plant_gap: f64,
    plant_layer_gap: f64,
    lift_step: f64,
    roof_clear: f64,
    tall_storey: f64,
    h_max_tall: f64,
    h_max_low: f64,
    /// Unit port offset from its centre, as a fraction of its length.
    port_frac: f64,
    /// Main lane offset from the corridor axis, as a fraction of the corridor width.
    lane_frac: f64,
}

impl Dn {
    /// Housing takes the lower velocity caps of its quieter rooms.
    fn read(res: bool) -> Dn {
        let v = |id: &str| rule(&format!("services.hvac.{id}{}", if res { "_res" } else { "" }));
        Dn {
            friction: rule("services.hvac.friction"),
            v_main: v("v_main"),
            v_branch: v("v_branch"),
            v_riser: v("v_riser"),
            top_gap: rule("services.hvac.top_gap"),
            layer_gap: rule("services.hvac.layer_gap"),
            drop_gap: rule("services.hvac.drop_gap"),
            plinth: rule("services.hvac.plinth"),
            shaft_clear: rule("services.hvac.shaft_clear"),
            drop_bend: rule("services.hvac.drop_bend"),
            bend: rule("services.hvac.round_bend"),
            margin: rule("services.hvac.margin"),
            wall_keep: rule("services.hvac.wall_keep"),
            step: rule("services.hvac.size_step"),
            red_angle: rule("services.hvac.reducer_angle"),
            red_min: rule("services.hvac.reducer_min"),
            flow_step: rule("services.hvac.flow_step"),
            deepen_step: rule("services.hvac.deepen_step"),
            deepen_steps: rule("services.hvac.deepen_steps") as usize,
            riser_gap: rule("services.hvac.riser_gap"),
            shaft_off: rule("services.hvac.shaft_off"),
            fixture_keep: rule("services.hvac.fixture_keep"),
            corr_end: rule("services.hvac.corr_end"),
            unit_clear: rule("services.hvac.unit_clear"),
            roof_edge: rule("services.hvac.roof_edge"),
            pref_weight: rule("services.hvac.pref_weight"),
            low_clear: rule("services.hvac.low_clear"),
            riser_sep: rule("services.hvac.riser_sep"),
            unit_riser_clear: rule("services.hvac.unit_riser_clear"),
            plant_gap: rule("services.hvac.plant_gap"),
            plant_layer_gap: rule("services.hvac.plant_layer_gap"),
            lift_step: rule("services.hvac.lift_step"),
            roof_clear: rule("services.hvac.roof_clear"),
            tall_storey: rule("services.hvac.tall_storey"),
            h_max_tall: rule("services.hvac.h_max_tall"),
            h_max_low: rule("services.hvac.h_max_low"),
            port_frac: rule("services.hvac.port_frac"),
            lane_frac: rule("services.hvac.lane_frac"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Term {
    fam: &'static str,
    ty: &'static str,
    neck_d: f64,
    neck_h: f64,
    cap: f64,
}

fn supply_terms() -> [Term; 2] {
    [
        Term {
            fam: "Supply Diffuser",
            ty: "300x300",
            neck_d: rule("services.hvac.supply_s_neck_d"),
            neck_h: rule("services.hvac.supply_s_neck_h"),
            cap: rule("services.hvac.supply_s_cap"),
        },
        Term {
            fam: "Supply Diffuser",
            ty: "600x600",
            neck_d: rule("services.hvac.supply_l_neck_d"),
            neck_h: rule("services.hvac.supply_l_neck_h"),
            cap: rule("services.hvac.supply_l_cap"),
        },
    ]
}

fn extract_terms() -> [Term; 2] {
    [
        Term {
            fam: "Exhaust Grille",
            ty: "200x200",
            neck_d: rule("services.hvac.extract_s_neck_d"),
            neck_h: rule("services.hvac.extract_s_neck_h"),
            cap: rule("services.hvac.extract_s_cap"),
        },
        Term {
            fam: "Exhaust Grille",
            ty: "400x400",
            neck_d: rule("services.hvac.extract_l_neck_d"),
            neck_h: rule("services.hvac.extract_l_neck_h"),
            cap: rule("services.hvac.extract_l_cap"),
        },
    ]
}

/// Air handling unit types of the library: length, width, height, port, airflow it serves.
#[derive(Clone, Copy, Debug)]
struct Unit {
    ty: &'static str,
    l: f64,
    w: f64,
    h: f64,
    cap: f64,
}

const UNIT_FAMILY: &str = "Air Handling Unit";
fn units() -> [Unit; 4] {
    [
        Unit {
            ty: "900x600",
            l: rule("services.hvac.unit_s_l"),
            w: rule("services.hvac.unit_s_w"),
            h: rule("services.hvac.unit_s_h"),
            cap: rule("services.hvac.unit_s_cap"),
        },
        Unit {
            ty: "2000x1000",
            l: rule("services.hvac.unit_m_l"),
            w: rule("services.hvac.unit_m_w"),
            h: rule("services.hvac.unit_m_h"),
            cap: rule("services.hvac.unit_m_cap"),
        },
        Unit {
            ty: "3000x1400",
            l: rule("services.hvac.unit_l_l"),
            w: rule("services.hvac.unit_l_w"),
            h: rule("services.hvac.unit_l_h"),
            cap: rule("services.hvac.unit_l_cap"),
        },
        Unit {
            ty: "6000x2200",
            l: rule("services.hvac.unit_x_l"),
            w: rule("services.hvac.unit_x_w"),
            h: rule("services.hvac.unit_x_h"),
            cap: rule("services.hvac.unit_x_cap"),
        },
    ]
}

fn terms(s: usize) -> [Term; 2] {
    if s == 0 {
        supply_terms()
    } else {
        extract_terms()
    }
}

fn term_for(s: usize, q: f64) -> Term {
    let t = terms(s);
    if q <= t[0].cap {
        t[0]
    } else {
        t[1]
    }
}

const SYS: [AirSys; 2] = [AirSys::Supply, AirSys::Extract];

/// Air connectors of a family the ventilation places, in plan with z above the family's
/// level: (connector, point).
pub fn air_ports(f: &GFixture) -> Vec<(&'static str, (f64, f64, f64))> {
    for t in supply_terms().into_iter().chain(extract_terms()) {
        if t.fam == f.family && t.ty == f.ty {
            return vec![("Air", (f.at.0, f.at.1, f.z + t.neck_h))];
        }
    }
    if f.family == UNIT_FAMILY {
        if let Some(u) = units().into_iter().find(|u| u.ty == f.ty) {
            let (c, s) = (f.rotation.cos(), f.rotation.sin());
            let at = |dx: f64| (f.at.0 + c * dx, f.at.1 + s * dx, f.z + u.h);
            let off = u.l * rule("services.hvac.port_frac");
            return vec![("Supply", at(off)), ("Return", at(-off))];
        }
    }
    super::svc::air_outlets(f).into_iter().map(|(name, at, _)| (name, at)).collect()
}

/// Each appliance with an air outlet (a cooker hood) ducted to the extract terminal of its
/// room: it tees into the terminal's own branch where a level straight of the branch has room
/// for the tee, so the terminal's neck stays a plain connector; up from the outlet to that
/// straight's level, then across to it, and the branch is split there as the mains are at
/// their taps. Without such room it joins the neck.
fn duct_outlets(p: &mut GProject) {
    use ddc_geom::math::{v3, V3};
    let mut ducts = vec![];
    let mut links = vec![];
    // (branch duct, segment, tee point)
    let mut cuts: Vec<(usize, usize, V3)> = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        let fx = &st.layout.fixtures;
        for (i, f) in fx.iter().enumerate() {
            for (name, at, size) in super::svc::air_outlets(f) {
                let room = st.layout.rooms.iter().map(|r| super::svc::bbox(&r.polygon)).find(|b| super::svc::inside((at.0, at.1), b));
                let neck = p.hvac.links.iter().filter(|l| l.storey == k && l.sys == AirSys::Extract).find_map(|l| {
                    let t = fx.get(l.fixture)?;
                    let (_, q) = air_ports(t).into_iter().find(|c| c.0 == l.connector)?;
                    room.is_some_and(|b| super::svc::inside((q.0, q.1), &b)).then_some(q)
                });
                let Some(q) = neck else { continue };
                // the terminal's own branch tells the air the room extracts
                let z = st.elevation + q.2;
                let bi = p
                    .hvac
                    .ducts
                    .iter()
                    .position(|d| d.points.last().is_some_and(|e| (e.0 - q.0).hypot(e.1 - q.1) < svc::EPS && (e.2 - z).abs() < svc::EPS))
                    .filter(|b| !cuts.iter().any(|c| c.0 == *b));
                let branch = bi.map(|b| &p.hvac.ducts[b]);
                let flow = branch.map_or(0.0, |d| d.flow);
                let sec = svc::duct_section((size, 0.0));
                let r = |u| mep::default_bend_radius(&sec, u);
                let leg = mep::junction_leg(&sec);
                let from = v3(at.0, at.1, st.elevation + at.2);
                // the level straight of the branch whose tee window lies nearest the outlet
                let tee = branch.and_then(|d| {
                    let bs = svc::duct_section(d.size);
                    let rb = d.bend.unwrap_or_else(|| mep::default_bend_radius(&bs, false).max(mep::default_bend_radius(&bs, true)));
                    let m = mep::junction_leg(&bs).max(rb) + mep::junction_leg(&bs) + leg;
                    let pts: Vec<V3> = d.points.iter().map(|e| v3(e.0, e.1, e.2)).collect();
                    pts.windows(2)
                        .enumerate()
                        .filter(|(_, w)| (w[1].z - w[0].z).abs() < svc::EPS && (w[1] - w[0]).len() > 2.0 * m)
                        .map(|(si, w)| {
                            let dir = (w[1] - w[0]).normalized();
                            let u = (from - w[0]).dot(dir);
                            let j = w[0] + dir * u.clamp(m, (w[1] - w[0]).len() - m);
                            (j, dir, u - (j - w[0]).dot(dir), si)
                        })
                        .min_by(|a, b| (a.0 - from).len().total_cmp(&(b.0 - from).len()))
                });
                let mut pts: Vec<V3> = match tee {
                    // up to the straight's level, along it into its window, then square across
                    Some((j, dir, off, si)) => {
                        cuts.push((bi.expect("a tee is on a branch"), si, j));
                        let up = v3(from.x, from.y, j.z);
                        vec![from, up, up - dir * off, j]
                    }
                    None => vec![from, v3(at.0, at.1, z), v3(q.0, at.1, z), v3(q.0, q.1, z)],
                };
                pts.dedup_by(|a, b| (*a - *b).len() < svc::EPS);
                let end = if tee.is_some() { leg } else { 0.0 };
                mep::fit_bends(&mut pts, &r, (0.0, end));
                let net = branch.map_or(0, |d| d.net);
                ducts.push(GDuct {
                    net,
                    sys: AirSys::Extract,
                    storey: k,
                    points: pts.iter().map(|q| (q.x, q.y, q.z)).collect(),
                    size: (size, 0.0),
                    flow,
                    bend: None,
                });
                links.push(GAirLink { storey: k, fixture: i, connector: name, sys: AirSys::Extract, net });
            }
        }
    }
    // each tee splits its branch: the part up to the tee and the part on to the terminal
    cuts.sort_by(|a, b| b.0.cmp(&a.0));
    for (bi, si, j) in cuts {
        let d = p.hvac.ducts[bi].clone();
        let jt = (j.x, j.y, j.z);
        let mut head: Vec<(f64, f64, f64)> = d.points[..=si].to_vec();
        head.push(jt);
        let mut tail = vec![jt];
        tail.extend_from_slice(&d.points[si + 1..]);
        p.hvac.ducts[bi].points = head;
        p.hvac.ducts.insert(bi + 1, GDuct { points: tail, ..d });
    }
    p.hvac.ducts.extend(ducts);
    p.hvac.links.extend(links);
}

fn residential(t: Typology) -> bool {
    matches!(t, Typology::House | Typology::ApartmentSection)
}

fn up5(q: f64) -> f64 {
    let step = rule("services.hvac.flow_step");
    (q / step).ceil() * step
}

/// Supply and extract air of a room, m3/h: residential rooms by the floor-area rate with at
/// least one air change, wet rooms and kitchens by the fixed extract rates; other rooms by
/// the occupancy rate of their kind with at least three air changes, balanced.
fn demand(k: RoomKind, area: f64, clear: f64, res: bool, tags: super::RoomTags) -> (f64, f64) {
    use RoomKind::*;
    if let Some((s, e)) = super::care::air(k, tags, area, clear) {
        return (up5(s), up5(e));
    }
    let vol = area * clear;
    if res {
        return match k {
            Kitchen => (0.0, rule("services.hvac.res_kitchen_extract")),
            Bath | Wc => (0.0, rule("services.hvac.res_wet_extract")),
            _ if k.habitable() => (up5((rule("services.hvac.res_rate") * area).max(vol)), 0.0),
            _ => (0.0, 0.0),
        };
    }
    match k {
        Kitchen => {
            let e = up5((rule("services.hvac.kitchen_rate") * area).max(rule("services.hvac.kitchen_ach") * vol));
            (up5(rule("services.hvac.kitchen_supply_ratio") * e), e)
        }
        Wc => (0.0, rule("services.hvac.wc_extract")),
        Bath => (0.0, rule("services.hvac.bath_extract")),
        Lobby | Hall => (up5(rule("services.hvac.hall_rate") * area), 0.0),
        _ if k.habitable() => {
            let rate = match k {
                Office => rule("services.hvac.rate_office"),
                Meeting => rule("services.hvac.rate_meeting"),
                Classroom => rule("services.hvac.rate_classroom"),
                Lab => rule("services.hvac.rate_lab"),
                Staff => rule("services.hvac.rate_staff"),
                Retail => rule("services.hvac.rate_retail"),
                Dining => rule("services.hvac.rate_dining"),
                Workshop => rule("services.hvac.rate_workshop"),
                Ward => rule("services.hvac.rate_ward"),
                Gym => rule("services.hvac.rate_gym"),
                _ => rule("services.hvac.rate_other"),
            };
            let q = up5((rate * area).max(rule("services.hvac.min_ach") * vol));
            (q, q)
        }
        _ => (0.0, 0.0),
    }
}

type Rect = (f64, f64, f64, f64);

fn bbox(p: &[(f64, f64)]) -> Rect {
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in p {
        b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
    }
    b
}

fn inside(a: Rect, b: Rect) -> bool {
    a.0 >= b.0 - 1e-6 && a.1 >= b.1 - 1e-6 && a.2 <= b.2 + 1e-6 && a.3 <= b.3 + 1e-6
}

fn round(k: &Dn, q: f64, v: f64) -> f64 {
    pick_round(q, k.friction, v, k.step, &ROUND_MM)
}

fn main_size(k: &Dn, q: f64, h_max: f64) -> (f64, f64) {
    pick_rect(q, k.friction, k.v_main, h_max, k.step)
}

/// Whether one main no taller than `h_max` carries `q` within the friction and speed limits
/// and the aspect limit (the sizing falls back to a wider section when none fits).
fn main_fits(k: &Dn, q: f64, h_max: f64) -> bool {
    let (w, h) = main_size(k, q, h_max);
    w <= h * MAX_ASPECT + 1e-9
        && friction_rect(q, w, h) <= k.friction + 1e-9
        && velocity(q, w * h) <= k.v_main + 1e-9
}

/// The widest main a floor carrying `q` gets: its own size when one main carries it, else
/// the largest that fits, as the floor then splits at the riser.
fn main_widest(k: &Dn, q: f64, h_max: f64) -> (f64, f64) {
    if main_fits(k, q, h_max) {
        return main_size(k, q, h_max);
    }
    let (mut lo, mut hi) = (0.0, q);
    for _ in 0..40 {
        let m = (lo + hi) / 2.0;
        if main_fits(k, m, h_max) {
            lo = m;
        } else {
            hi = m;
        }
    }
    main_size(k, lo, h_max)
}

/// Length a fitting may take from a run end at a node joining these sections: the largest
/// elbow or tee leg and a reducer between the largest and the smallest section.
fn node_trim(k: &Dn, secs: &[(f64, f64)]) -> f64 {
    let dims = |s: &(f64, f64)| (s.0, if s.1 > 0.0 { s.1 } else { s.0 });
    let big = secs
        .iter()
        .map(|s| {
            let (w, h) = dims(s);
            if s.1 > 0.0 {
                w.max(h)
            } else {
                k.bend * w
            }
        })
        .fold(0.0, f64::max);
    let mut red: f64 = 0.0;
    for a in secs {
        for b in secs {
            let (aw, ah) = dims(a);
            let (bw, bh) = dims(b);
            let d = (aw - bw).abs().max((ah - bh).abs());
            red = red.max((d / 2.0 / k.red_angle.to_radians().tan()).max(k.red_min));
        }
    }
    big + red
}

/// Length a tee or cross takes from each run end it joins, as the model cuts it: the outer
/// size of the duct through it.
fn tee_leg(s: (f64, f64)) -> f64 {
    mep::junction_leg(&svc::duct_section(s))
}

/// The smallest unit that carries `q`, else the largest.
fn pick_unit(q: f64) -> Unit {
    let us = units();
    *us.iter().find(|u| q <= u.cap).unwrap_or(&us[us.len() - 1])
}

/// A room the ventilation serves.
/// One terminal on a branch: its centre across the corridor, air, type, neck diameter and room.
#[derive(Clone, Copy)]
struct Drop {
    y: f64,
    q: f64,
    term: Term,
    d: f64,
    room: usize,
}

struct Served {
    room: usize,
    r: Rect,
    /// +1 north of the corridor, -1 south.
    side: f64,
    /// The room edge next to the corridor and the far one.
    near: f64,
    far: f64,
    q: [f64; 2],
}

struct Floor {
    elev: f64,
    corr: Rect,
    rooms: Vec<Served>,
    total: [f64; 2],
}

/// A terminal before its tap is placed.
#[derive(Clone)]
struct Want {
    floor: usize,
    room: usize,
    sys: usize,
    side: f64,
    x: f64,
    lo: f64,
    hi: f64,
    /// First terminal centre across the corridor and its distance from the main: for a supply
    /// branch the nearest drop, for an extract branch the farthest reach.
    y: f64,
    dist: f64,
    /// Air of the branch, of each of its terminals, and how many terminals it feeds.
    q: f64,
    qt: f64,
    ny: usize,
    /// Terminals along the branch, nearest the main first.
    drops: Vec<Drop>,
    term: Term,
    /// The room has a hood outlet that tees into this branch.
    hood: bool,
    /// Branch and terminal drop diameters.
    d: f64,
    dd: f64,
}

/// A tap on a main: position, side, flow, and the terminal it feeds (`None` for the riser).
#[derive(Clone)]
struct Tap {
    x: f64,
    side: f64,
    q: f64,
    want: Option<usize>,
}

/// Loose furniture a shaft may displace: the chase takes that corner of the room. Plumbing,
/// wall-hung and built-in fixtures stay and refuse the position instead.
const MOVABLE: [&str; 12] = [
    "Bed",
    "Chair",
    "Desk",
    "Sofa",
    "Table",
    "Wardrobe",
    "Shelf Unit",
    "Locker Bank",
    "Storage Rack",
    "Workbench",
    "Lab Bench",
    "Treadmill",
];

/// Whether a plan point lies in a ring's bounding box.
pub(super) fn in_rect(ring: &[(f64, f64)], q: (f64, f64)) -> bool {
    in_rect_r(&bbox(ring), q)
}

pub(super) fn in_rect_r(r: &Rect, q: (f64, f64)) -> bool {
    q.0 >= r.0 - 1e-6 && q.0 <= r.2 + 1e-6 && q.1 >= r.1 - 1e-6 && q.1 <= r.3 + 1e-6
}

/// Bays of `kind` that stand at the same place on every storey of the building.
pub(super) fn core_bays(p: &GProject, kind: crate::ShaftKind) -> Vec<Rect> {
    let of = |k: usize| -> Vec<Vec<(f64, f64)>> {
        p.storeys[k].layout.shafts.iter().filter(|s| s.kind == kind).map(|s| s.ring.clone()).collect()
    };
    if p.storeys.is_empty() {
        return vec![];
    }
    of(0).into_iter()
        .filter(|r| (1..p.storeys.len()).all(|k| of(k).contains(r)))
        .map(|r| bbox(&r))
        .collect()
}

/// The shaft and the plant above it.
struct Shaft {
    ring: Rect,
    side: f64,
    /// Furniture the shaft displaces: storey, fixture index on it.
    clear: Vec<(usize, usize)>,
    /// Riser axes: supply, extract.
    xr: [f64; 2],
    yr: f64,
    unit: Unit,
    /// Unit centre in plan and its rotation (0 or PI).
    at: (f64, f64),
    rot: f64,
    /// Plan points of each low run over the top slab after its riser, and its height above
    /// the roof level; no points where the riser climbs straight to the plant.
    lift: [Vec<(f64, f64)>; 2],
    zlo: [f64; 2],
    /// Mains across the corridor: supply, extract.
    ym: [f64; 2],
    /// A floor too big for one main splits at the riser: the second branch runs along the
    /// shaft to this x and feeds the mains beyond it.
    legs: [Option<f64>; 2],
    /// The larger flow of the two mains of a split floor, the floor's flow otherwise.
    lq: [f64; 2],
}

/// Where the unit stands and how the plant ducts reach it.
struct Plant {
    at: (f64, f64),
    rot: f64,
    lift: [Vec<(f64, f64)>; 2],
    zlo: [f64; 2],
}

/// One ventilation system of the project: which rooms it takes, how far its mains sit below
/// the general ones, which network and which roof unit it is.
#[derive(Clone, Copy)]
struct Pass {
    /// Rooms carrying one of these tags are the dedicated system's; none: one system for all.
    tags: super::RoomTags,
    /// This pass serves the tagged rooms, otherwise all the others.
    dedicated: bool,
    /// Depth of its layers under the general ones, m.
    drop: f64,
    net: u8,
    /// Index of its unit in the plant list.
    plant_ix: usize,
    /// Stretch of the corridor this zone takes: rooms by their centre, taps and shaft inside.
    win: Option<(f64, f64)>,
}

impl Pass {
    fn all() -> Pass {
        Pass { tags: super::RoomTags::default(), dedicated: false, drop: 0.0, net: 0, plant_ix: 0, win: None }
    }

    fn skips(&self, tags: super::RoomTags) -> bool {
        !self.tags.is_empty() && tags.meets(self.tags) != self.dedicated
    }
}

struct Ctx<'a> {
    p: &'a GProject,
    floors: Vec<Option<Floor>>,
    has: [bool; 2],
    grand: [f64; 2],
    riser_d: [f64; 2],
    /// Main layers and terminal faces above the storey level.
    zm: [f64; 2],
    face: [f64; 2],
    neck: [f64; 2],
    pitched: Option<f64>,
    /// Why shaft positions were refused, by reason; reported when none is left.
    refused: std::cell::RefCell<[usize; REFUSALS.len()]>,
    /// Plan boxes of the stair heads with their clearance: the unit stands outside them.
    heads: Vec<((f64, f64), (f64, f64))>,
    k: Dn,
    /// The air handling unit for the grand flow.
    unit_t: Unit,
    /// Roof plan taken by the units and plant ducts of the systems already built.
    taken: Vec<Rect>,
    /// Duct shafts already standing, with the side of the corridor they are on.
    rings: Vec<(Rect, f64)>,
    /// The zone's stretch less the gap to the next zone: its taps, shaft and mains stay inside.
    win: Option<(f64, f64)>,
    /// The zone's own stretch, uncut: the shaft beside the corridor may reach up to its ends.
    zone: Option<(f64, f64)>,
}

/// Reasons a shaft position is refused, as the metrics name them.
const REFUSALS: [&str; 7] = [
    "hvac_refused_no_service_room",
    "hvac_refused_fixture",
    "hvac_refused_shaft",
    "hvac_refused_main",
    "hvac_refused_outside",
    "hvac_refused_roof",
    "hvac_refused_stair_head",
];
/// Index of `hvac_refused_stair_head`: the unit would stand on a stair head or its clearance.
const ON_HEAD: usize = 6;

/// Rooms that may hold the shaft, best first; work rooms are the last resort (a chase in a
/// corner) when no service room is wide enough.
const SHAFT_ROOMS: [RoomKind; 16] = [
    RoomKind::Technical,
    RoomKind::Storage,
    RoomKind::Hall,
    RoomKind::Wc,
    RoomKind::Bath,
    RoomKind::Lobby,
    RoomKind::Staff,
    RoomKind::Office,
    RoomKind::Meeting,
    RoomKind::Classroom,
    RoomKind::Lab,
    RoomKind::Ward,
    RoomKind::Workshop,
    RoomKind::Retail,
    RoomKind::Dining,
    RoomKind::Gym,
];

impl Ctx<'_> {
    fn corridor(&self) -> Option<Rect> {
        self.floors.iter().flatten().map(|f| f.corr).next()
    }

    /// Clear height under a hip roof above its level at a plan point: the top plane rises from
    /// the wall line (the overhang hangs below the level), the roof's thickness comes off.
    fn roof_room(&self, x: f64, y: f64) -> f64 {
        let Some(slope) = self.pitched else {
            return f64::MAX;
        };
        let b = bbox(&self.p.roof.ring);
        let e = (x - b.0).min(b.2 - x).min(y - b.1).min(b.3 - y);
        let a = slope.to_radians();
        e * a.tan() - super::roof_thickness(slope) / a.cos()
    }

    fn unit(&self) -> Unit {
        self.unit_t
    }

    /// The shaft at `x`, deepened step by step until the mains clear the risers: wide flat
    /// mains of a tall block need metres between the corridor and the riser. Only the mains
    /// get better with depth, so any other refusal ends the search; each position counts
    /// once in the refusal metrics.
    /// The least preferred of the host rooms a ring spans across their partitions on one
    /// storey: every room it overlaps a host, each as deep as the ring, the gaps walls.
    fn straddle(&self, st: &super::PStorey, ring: Rect) -> Option<usize> {
        let wall = rule("services.hvac.straddle_wall");
        let mut spans = vec![];
        for (i, (pr, gr)) in st.rooms.iter().zip(&st.layout.rooms).enumerate() {
            let b = bbox(&gr.polygon);
            if b.0 >= ring.2 - 1e-6 || b.2 <= ring.0 + 1e-6 || b.1 >= ring.3 - 1e-6 || b.3 <= ring.1 + 1e-6 {
                continue;
            }
            if !SHAFT_ROOMS.contains(&pr.kind) || b.1 > ring.1 + 1e-6 || b.3 < ring.3 - 1e-6 {
                return None;
            }
            spans.push((b.0, b.2, i));
        }
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut reach = ring.0;
        for &(x0, x1, _) in &spans {
            if x0 > reach + wall {
                return None;
            }
            reach = reach.max(x1);
        }
        (spans.len() > 1 && reach >= ring.2 - 1e-6).then_some(())?;
        spans.into_iter().map(|s| s.2).max_by_key(|&i| SHAFT_ROOMS.iter().position(|k| *k == st.rooms[i].kind))
    }

    fn try_shaft(&self, side: f64, x: f64, fixed: Option<Rect>) -> Option<(f64, Shaft)> {
        // a stepped block can hang the shaft off any storey's corridor: off the podium's it
        // may land outside the tower, off the tower's it clears the podium mains
        let mut anchors: Vec<f64> = vec![];
        for k in (0..self.floors.len()).filter(|&k| self.floors[k].is_some()) {
            let a = self.shift(k);
            if !anchors.iter().any(|b| (a - b).abs() < 1e-6) {
                anchors.push(a);
            }
        }
        let mut why = None;
        let mut best: Option<(f64, Shaft)> = None;
        for a in anchors {
            match self.deepen(side, x, a, fixed) {
                Ok(s) => {
                    if best.as_ref().is_none_or(|b| s.0 < b.0) {
                        best = Some(s);
                    }
                }
                Err(i) => {
                    why.get_or_insert(i);
                }
            }
        }
        if best.is_none() {
            self.refused.borrow_mut()[why.unwrap_or(0)] += 1;
        }
        best
    }

    fn deepen(&self, side: f64, x: f64, anchor: f64, fixed: Option<Rect>) -> Result<(f64, Shaft), usize> {
        const MAIN: usize = 3;
        for k in 0..self.k.deepen_steps {
            match self.shaft_at(side, x, self.k.deepen_step * k as f64, anchor, fixed) {
                Ok(s) => return Ok(s),
                Err(MAIN) => continue,
                // past the first step the mains were the reason it had to go deeper
                Err(i) => return Err(if k > 0 { MAIN } else { i }),
            }
        }
        Err(MAIN)
    }

    /// How much taller storey `k` is than the lowest: the duct band `zm` is measured under the
    /// lowest ceiling, a taller level hangs it that much higher, under its own ceiling.
    fn lift(&self, k: usize) -> f64 {
        let low = self.p.storeys.iter().map(|s| s.height).fold(f64::MAX, f64::min);
        self.p.storeys.get(k).map_or(0.0, |s| s.height - low)
    }

    /// Offset of storey `k`'s corridor axis from the reference corridor: a stepped block's
    /// upper floors run their mains along their own corridor.
    fn shift(&self, k: usize) -> f64 {
        match (self.corridor(), self.floors.get(k).and_then(|f| f.as_ref())) {
            (Some(c), Some(f)) => (f.corr.1 + f.corr.3 - c.1 - c.3) / 2.0,
            _ => 0.0,
        }
    }

    /// `fixed` is a core bay of the building: the risers stand in it, whatever the scan offers.
    fn shaft_at(&self, side: f64, x: f64, extra: f64, anchor: f64, fixed: Option<Rect>) -> Result<(f64, Shaft), usize> {
        let c = self.corridor().ok_or(0usize)?;
        let (ds, de) = (self.riser_d[0], self.riser_d[1]);
        let dn = &self.k;
        let unit = self.unit();
        let gap = match self.has {
            [true, true] => (ds / 2.0 + de / 2.0 + dn.riser_gap)
                .max(2.0 * dn.bend * de + dn.margin - unit.l / 2.0),
            _ => 0.0,
        };
        let dmax = [ds, de]
            .iter()
            .zip(self.has)
            .filter(|(_, h)| *h)
            .map(|(d, _)| *d)
            .fold(0.0, f64::max);
        let x = fixed.map_or(x, |r| r.0 + dn.shaft_clear + if self.has[0] { ds } else { de } / 2.0);
        let xr = [x, x + gap];
        let left = if self.has[0] { x - ds / 2.0 } else { x - de / 2.0 };
        let right = if self.has[1] { x + gap + de / 2.0 } else { x + ds / 2.0 };
        let depth = dmax + 2.0 * dn.shaft_clear + extra;
        let (y0, y1) = if side > 0.0 {
            (c.3 + anchor + dn.shaft_off, c.3 + anchor + dn.shaft_off + depth)
        } else {
            (c.1 + anchor - dn.shaft_off - depth, c.1 + anchor - dn.shaft_off)
        };
        let mut ring = (left - dn.shaft_clear, y0, right + dn.shaft_clear, y1);
        if let Some(r) = fixed {
            // a bay too small for its risers is refused as an overlap with the walls round it
            if ring.2 > r.2 + 1e-9 || dmax + 2.0 * dn.shaft_clear > r.3 - r.1 + 1e-9 {
                return Err(2);
            }
            ring = r;
        }
        let (y0, y1) = (ring.1, ring.3);
        let yr = (y0 + y1) / 2.0;
        let mut pref = 0usize;
        let mut straddles = 0usize;
        let mut clear = vec![];
        for (k, st) in self.p.storeys.iter().enumerate() {
            let l = &st.layout;
            if fixed.is_none() {
                let host = st.rooms.iter().zip(&l.rooms).position(|(pr, gr)| {
                    SHAFT_ROOMS.contains(&pr.kind) && inside(ring, bbox(&gr.polygon))
                });
                // or across a partition between two hosts: the shaft wall takes that stretch of it
                let host = match host {
                    Some(h) => h,
                    None => {
                        let Some(h) = self.straddle(st, ring) else {
                            return Err(0);
                        };
                        straddles += 1;
                        h
                    }
                };
                pref = pref.max(SHAFT_ROOMS.iter().position(|k| *k == st.rooms[host].kind).ok_or(0usize)?);
            }
            let keep = dn.fixture_keep;
            for (i, f) in l.fixtures.iter().enumerate() {
                let near = f.at.0 > ring.0 - keep
                    && f.at.0 < ring.2 + keep
                    && f.at.1 > ring.1 - keep
                    && f.at.1 < ring.3 + keep;
                if !near {
                    continue;
                }
                if f.wall.is_some() || !MOVABLE.contains(&f.family) {
                    return Err(1);
                }
                clear.push((k, i));
            }
            if fixed.is_none() && l.shafts.iter().any(|s| {
                let b = bbox(&s.ring);
                b.0 < ring.2 && ring.0 < b.2 && b.1 < ring.3 && ring.1 < b.3
            }) {
                return Err(2);
            }
        }
        // mains across the corridor: supply off the shaft side, extract as near the shaft as
        // its riser connection allows
        let mid = (c.1 + c.3) / 2.0;
        let cw = c.3 - c.1;
        let lane = side * cw * dn.lane_frac;
        let mut ym = [mid - lane, mid];
        let mut legs = [None; 2];
        let mut lq = [0.0; 2];
        let h = self.h_max();
        for s in 0..2 {
            if !self.has[s] {
                continue;
            }
            let riser = (self.riser_d[s], 0.0);
            let fmax = self.floors_max(s);
            lq[s] = fmax;
            let need = if main_fits(dn, fmax, h) {
                let conn = main_size(dn, fmax, h);
                node_trim(dn, &[riser, conn]) + node_trim(dn, &[conn, conn]) + dn.margin
            } else {
                // two branches at the riser: the straight one feeds the mains towards the
                // other riser, the far one runs along the shaft first, away from it
                let dir = if s == 0 { -1.0 } else { 1.0 };
                let cap = terms(s)[1].cap;
                let (mut qn, mut qf) = (0.0f64, 0.0f64);
                for f in self.floors.iter().flatten() {
                    let (mut n, mut fa) = (0.0, 0.0);
                    for r in f.rooms.iter().filter(|r| r.q[s] > 0.0) {
                        // terminal flows round up by 5 m3/h each
                        let q = r.q[s] + dn.flow_step * (r.q[s] / cap).ceil().max(1.0);
                        let (a, b) = ((r.r.0 - xr[s]) * dir, (r.r.2 - xr[s]) * dir);
                        if a.max(b) > 0.0 {
                            fa += q;
                        }
                        if a.min(b) <= 0.0 {
                            n += q;
                        }
                    }
                    qn = qn.max(n);
                    qf = qf.max(fa);
                }
                if !main_fits(dn, qn, h) || !main_fits(dn, qf, h) {
                    return Err(3);
                }
                let (cn, cf) = (main_size(dn, qn, h), main_size(dn, qf, h));
                let tee = node_trim(dn, &[riser, cn, cf]);
                let xa = xr[s] + dir * (tee + node_trim(dn, &[cf]) + dn.margin);
                if self
                    .floors
                    .iter()
                    .flatten()
                    .any(|f| xa < f.corr.0 + dn.corr_end || xa > f.corr.2 - dn.corr_end)
                {
                    return Err(4);
                }
                legs[s] = Some(xa);
                lq[s] = qn.max(qf);
                (tee + node_trim(dn, &[cn, cn]))
                    .max(node_trim(dn, &[cf]) + node_trim(dn, &[cf, cf]))
                    + dn.margin
            };
            let cands = if s == 0 {
                vec![mid - lane]
            } else {
                vec![mid + lane, mid, mid - lane]
            };
            // every storey's main, moved with its corridor, keeps room for the riser tee
            let clear = |y: f64| {
                (0..self.floors.len())
                    .filter(|&k| self.floors[k].is_some())
                    .all(|k| (yr - y - self.shift(k)).abs() >= need)
            };
            let Some(y) = cands.iter().find(|y| clear(**y)) else {
                return Err(3);
            };
            ym[s] = *y;
        }
        // the unit beside the shaft, towards the corridor: its supply port above the riser
        let rs = dn.bend * ds;
        let re = dn.bend * de;
        let delta = (2.0 * rs + dn.margin)
            .max(2.0 * re + dn.margin)
            .max(unit.w / 2.0 + dmax / 2.0 + dn.unit_clear);
        // the extract riser jogs over to the unit's far end between two elbows
        let jog = if self.has[1] {
            (2.0 * re + dn.margin - (xr[1] - x + unit.l / 2.0)).max(0.0)
        } else {
            0.0
        };
        let (ox, fw) = (self.p.roof.ring[0].0, self.p.footprint.0);
        let m = dn.roof_edge;
        if ring.0 < ox + m || ring.2 > ox + fw - m {
            return Err(4);
        }
        if let (Some((wl, wh)), Some((zl, zh))) = (self.win, self.zone) {
            if ring.0 < zl || ring.2 > zh || legs.iter().flatten().any(|&xa| xa < wl || xa > wh) {
                return Err(4);
            }
        }
        let y_at = yr - side * delta;
        let top = self.p.storeys.last().map_or(0.0, |s| s.elevation) - self.p.roof.elevation;
        let pl = self.plant(xr, yr, y_at, side, jog, unit, self.zm.map(|z| top + z))?;
        let b = bbox(&self.p.roof.ring);
        let centre = ((x - (b.0 + b.2) / 2.0).abs() + (yr - (b.1 + b.3) / 2.0).abs()) / fw;
        // a clear corner beats one that displaces furniture
        let score = pref as f64 * dn.pref_weight + centre + clear.len() as f64 + straddles as f64 * dn.pref_weight;
        Ok((
            score,
            Shaft {
                ring,
                side,
                clear,
                xr,
                yr,
                unit,
                at: pl.at,
                rot: pl.rot,
                lift: pl.lift,
                zlo: pl.zlo,
                ym,
                legs,
                lq,
            },
        ))
    }

    /// The unit at `y_at` and the plant ducts up to it. The risers climb straight to the plant
    /// layers when the roof allows; under a hip roof too low there they stop just over the top
    /// slab and run towards the middle, rising where the slopes leave room. The run that would
    /// pass the other riser steps aside first, away from the corridor.
    #[allow(clippy::too_many_arguments)]
    fn plant(
        &self,
        xr: [f64; 2],
        yr: f64,
        y_at: f64,
        side: f64,
        jog: f64,
        u: Unit,
        ztop: [f64; 2],
    ) -> Result<Plant, usize> {
        // (rotation, unit centre x); under a hip roof the unit may also turn round or sit past
        // the extract riser, whichever keeps the plant clear of the slopes
        let q = u.l * self.k.port_frac;
        let spots = |r: [f64; 2]| {
            let mut c = vec![(0.0, r[0] - q - jog)];
            // a pitched roof, or a stair head in the way, lets the unit turn or pass the riser
            if self.pitched.is_some() || !self.heads.is_empty() {
                c.extend([(PI, r[0] + q), (PI, r[1] - q), (0.0, r[1] + q)]);
            }
            c
        };
        let mut why = None;
        for (rot, ax) in spots(xr) {
            match self.plant_fits([(xr[0], yr), (xr[1], yr)], (ax, y_at), rot, u) {
                Ok(()) => {
                    return Ok(Plant {
                        at: (ax, y_at),
                        rot,
                        lift: [vec![], vec![]],
                        zlo: [0.0; 2],
                    })
                }
                Err(i) => {
                    why.get_or_insert(i);
                }
            }
        }
        if self.pitched.is_none() {
            return Err(why.unwrap_or(5));
        }
        let d = self.riser_d;
        let has = self.has;
        let dn = &self.k;
        let bend = |s: usize| 2.0 * dn.bend * d[s] + dn.margin;
        // over the top storey's tee on the riser, then the elbow into the low run
        let zlo = [0, 1].map(|s| {
            (d[s] / 2.0 + dn.low_clear).max(ztop[s] + tee_leg((d[s], 0.0)) + dn.bend * d[s] + dn.margin)
        });
        let b = bbox(&self.p.roof.ring);
        let dir = if (b.0 + b.2) / 2.0 >= xr[0] { 1.0 } else { -1.0 };
        let trail = if dir > 0.0 { 0 } else { 1 };
        let both = has[0] && has[1];
        let sep = d[0] / 2.0 + d[1] / 2.0 + dn.riser_sep;
        let (ox, oy) = (self.p.roof.ring[0].0, self.p.roof.ring[0].1);
        let (fw, fd) = self.p.footprint;
        let m = dn.roof_edge;
        let mut t = (0..2).filter(|&s| has[s]).map(bend).fold(0.0, f64::max);
        loop {
            let r = xr.map(|x| x + dir * t);
            if !(has[0] || has[1]) || (0..2).any(|s| has[s] && (r[s] < ox + m || r[s] > ox + fw - m)) {
                break;
            }
            let mut lift = [vec![], vec![]];
            let mut o = [(r[0], yr), (r[1], yr)];
            for s in (0..2).filter(|&s| has[s]) {
                let lane = if both && s == trail {
                    yr + side * sep.max(bend(s))
                } else {
                    yr
                };
                if (lane - yr).abs() > 1e-9 {
                    lift[s].push((xr[s], lane));
                }
                lift[s].push((r[s], lane));
                o[s] = (r[s], lane);
            }
            let inside = lift
                .iter()
                .flatten()
                .all(|&(_, y)| y >= oy + m && y <= oy + fd - m);
            if inside && self.low_fits(xr, yr, &lift, zlo) {
                for (rot, ax) in spots(r) {
                    match self.plant_fits(o, (ax, y_at), rot, u) {
                        Ok(()) if self.clear_of_unit(xr, yr, &lift, (ax, y_at), u) => {
                            return Ok(Plant {
                                at: (ax, y_at),
                                rot,
                                lift,
                                zlo,
                            });
                        }
                        Ok(()) => {
                            why.get_or_insert(4);
                        }
                        Err(i) => {
                            why.get_or_insert(i);
                        }
                    }
                }
            }
            t += dn.lift_step;
        }
        Err(why.unwrap_or(5))
    }

    /// The low runs over the top slab, from each riser at (`xr`, `yr`) through `lift`: under
    /// the roof, and each vertical clear of the other system's run.
    fn low_fits(&self, xr: [f64; 2], yr: f64, lift: &[Vec<(f64, f64)>; 2], zlo: [f64; 2]) -> bool {
        let d = self.riser_d;
        let path = |s: usize| {
            let mut p = vec![(xr[s], yr)];
            p.extend(lift[s].iter().copied());
            p
        };
        for s in (0..2).filter(|&s| self.has[s]) {
            let r = d[s] / 2.0;
            let room = path(s).iter().all(|&(x, y)| {
                [(-r, -r), (r, r), (-r, r), (r, -r)]
                    .iter()
                    .all(|(dx, dy)| self.roof_room(x + dx, y + dy) >= zlo[s] + r + self.k.roof_clear)
            });
            if !room {
                return false;
            }
        }
        if self.has[0] && self.has[1] {
            let sep = d[0] / 2.0 + d[1] / 2.0 + self.k.riser_sep;
            for s in 0..2 {
                let other = path(1 - s);
                let p = path(s);
                let ups = [p[0], *p.last().expect("a run")];
                if ups
                    .iter()
                    .any(|&q| other.windows(2).any(|w| seg_dist(q, w[0], w[1]) < sep))
                {
                    return false;
                }
            }
        }
        true
    }

    /// The low runs stay out from under the unit.
    fn clear_of_unit(
        &self,
        xr: [f64; 2],
        yr: f64,
        lift: &[Vec<(f64, f64)>; 2],
        at: (f64, f64),
        u: Unit,
    ) -> bool {
        let (hl, hw) = (u.l / 2.0, u.w / 2.0);
        (0..2).filter(|&s| self.has[s]).all(|s| {
            let r = self.riser_d[s] / 2.0 + self.k.unit_riser_clear;
            let mut p = vec![(xr[s], yr)];
            p.extend(lift[s].iter().copied());
            p.windows(2).all(|w| {
                let (x0, x1) = (w[0].0.min(w[1].0) - r, w[0].0.max(w[1].0) + r);
                let (y0, y1) = (w[0].1.min(w[1].1) - r, w[0].1.max(w[1].1) + r);
                x1 <= at.0 - hl || x0 >= at.0 + hl || y1 <= at.1 - hw || y0 >= at.1 + hw
            })
        })
    }

    /// Plan x of the unit's supply and return ports.
    fn ports(dn: &Dn, at: (f64, f64), rot: f64, u: Unit) -> [f64; 2] {
        let c = rot.cos() * u.l * dn.port_frac;
        [at.0 + c, at.0 - c]
    }

    /// Whether the unit at `at` turned by `rot` fits the plant ducts climbing at `o`: inside
    /// the footprint, every leg either straight above its port or a two-elbow jog away, the
    /// supply legs clear of the extract climb, and under a pitched roof each duct and the unit
    /// below the slopes. Errors: 4 off the footprint or no room for the legs, 5 under the roof.
    fn plant_fits(&self, o: [(f64, f64); 2], at: (f64, f64), rot: f64, u: Unit) -> Result<(), usize> {
        let (ox, oy) = (self.p.roof.ring[0].0, self.p.roof.ring[0].1);
        let (fw, fd) = self.p.footprint;
        let dn = &self.k;
        let m = dn.roof_edge;
        let ub =(at.0 - u.l / 2.0, at.1 - u.w / 2.0, at.0 + u.l / 2.0, at.1 + u.w / 2.0);
        if ub.0 < ox + m || ub.2 > ox + fw - m || ub.1 < oy + m || ub.3 > oy + fd - m {
            return Err(4);
        }
        if self.heads.iter().any(|(lo, hi)| ub.0 < hi.0 && ub.2 > lo.0 && ub.1 < hi.1 && ub.3 > lo.1) {
            return Err(ON_HEAD);
        }
        let d = self.riser_d;
        let p = Self::ports(dn, at, rot, u);
        let hit = |a: Rect| self.taken.iter().any(|t| a.0 < t.2 && t.0 < a.2 && a.1 < t.3 && t.1 < a.3);
        if hit(ub) {
            return Err(4);
        }
        for s in (0..2).filter(|&s| self.has[s]) {
            let r = d[s] / 2.0;
            let legs = [(o[s], (p[s], o[s].1)), ((p[s], o[s].1), (p[s], at.1))];
            if legs.iter().any(|(a, b)| hit((a.0.min(b.0) - r, a.1.min(b.1) - r, a.0.max(b.0) + r, a.1.max(b.1) + r))) {
                return Err(4);
            }
            let leg = (p[s] - o[s].0).abs();
            if leg > 1e-9 && leg < 2.0 * dn.bend * d[s] + dn.margin - 1e-9 {
                return Err(4);
            }
        }
        if self.has[0] && self.has[1] {
            // the supply runs at the lower layer, which the extract climb passes through
            let sep = d[0] / 2.0 + d[1] / 2.0 + dn.riser_sep;
            let bend = (p[0], o[0].1);
            if seg_dist(o[1], o[0], bend) < sep || seg_dist(o[1], bend, (p[0], at.1)) < sep {
                return Err(4);
            }
        }
        let (z1, z2) = self.plant_z(u);
        let need = [z1 + d[0] / 2.0, z2 + d[1] / 2.0];
        let under = |x: f64, y: f64, r: f64, top: f64| {
            [(-r, -r), (r, r), (-r, r), (r, -r)]
                .iter()
                .all(|(dx, dy)| self.roof_room(x + dx, y + dy) >= top + dn.roof_clear)
        };
        let unit_ok = [(ub.0, ub.1), (ub.2, ub.3), (ub.0, ub.3), (ub.2, ub.1)]
            .iter()
            .all(|&(x, y)| under(x, y, 0.0, dn.plinth + u.h));
        let ducts_ok = (0..2).filter(|&s| self.has[s]).all(|s| {
            [o[s], (p[s], o[s].1), (p[s], at.1)]
                .iter()
                .all(|&(x, y)| under(x, y, d[s] / 2.0, need[s]))
        });
        if unit_ok && ducts_ok {
            Ok(())
        } else {
            Err(5)
        }
    }

    fn h_max(&self) -> f64 {
        if self.p.storeys.first().is_some_and(|s| s.height >= self.k.tall_storey) {
            self.k.h_max_tall
        } else {
            self.k.h_max_low
        }
    }

    fn floors_max(&self, s: usize) -> f64 {
        self.floors
            .iter()
            .flatten()
            .map(|f| f.total[s])
            .fold(0.0, f64::max)
    }

    /// Plant ducts above the roof level: supply and extract layers.
    fn plant_z(&self, u: Unit) -> (f64, f64) {
        let dn = &self.k;
        let pz = dn.plinth + u.h;
        let (ds, de) = (self.riser_d[0], self.riser_d[1]);
        let z1 = pz + dn.bend * ds + dn.margin + dn.plant_gap;
        let mut z2 = pz + dn.bend * de + dn.margin + dn.plant_gap;
        if self.has[0] {
            z2 = z2.max(z1 + ds / 2.0 + de / 2.0 + dn.plant_layer_gap);
        }
        (z1, z2)
    }
}

/// Places taps on one main; `fixed` are the riser tap and taps of another system to keep
/// clear of. A tap joins one already placed only across the main, as a cross.
fn place(
    wants: &[Want],
    order: &[usize],
    taps: &mut Vec<Tap>,
    spacing: f64,
    floor: f64,
    blocked: &dyn Fn(&Want, f64) -> bool,
) -> Vec<usize> {
    let stacked_ratio = rule("services.hvac.stacked_ratio");
    let grid_step = rule("services.hvac.tap_grid");
    // whether want `i` may take a tap at `x`, ignoring the tap at index `skip`
    let fits = |taps: &Vec<Tap>, i: usize, x: f64, skip: Option<usize>| {
        let w = &wants[i];
        if x < w.lo - 1e-9 || x > w.hi + 1e-9 || blocked(w, x) {
            return false;
        }
        let others = || taps.iter().enumerate().filter(|(k, _)| Some(*k) != skip).map(|(_, t)| t);
        others().all(|t| {
            let dx = (t.x - x).abs();
            if dx < 1e-9 {
                t.side != w.side && others().filter(|o| (o.x - x).abs() < 1e-9).count() == 1
            } else {
                // rooms stacked across the corridor sit side by side on a shared window:
                // their branches run parallel, so the fittings may be closer
                let stacked = t
                    .want
                    .is_some_and(|j| wants[j].lo == w.lo && wants[j].hi == w.hi);
                // never closer than the two tees' legs: the main between them would vanish
                dx >= (if stacked { stacked_ratio * spacing } else { spacing }).max(floor) - 1e-9
            }
        })
    };
    let grid = |lo: f64, hi: f64| {
        let steps = ((hi - lo) / grid_step).floor() as usize;
        (0..=steps).map(move |k| lo + grid_step * k as f64).chain(std::iter::once(hi))
    };
    let mut lost = vec![];
    for &i in order {
        let w = &wants[i];
        let mut cands = vec![w.x.clamp(w.lo, w.hi)];
        for t in taps.iter() {
            let sp = spacing.max(floor);
            cands.extend([t.x, t.x + sp, t.x - sp]);
        }
        // anywhere else in the window, for windows narrowed by the shaft and its neighbours
        cands.extend(grid(w.lo, w.hi));
        cands.retain(|x| fits(taps, i, *x, None));
        cands.sort_by(|a, b| (a - w.x).abs().total_cmp(&(b - w.x).abs()));
        if let Some(&x) = cands.first() {
            taps.push(Tap {
                x,
                side: w.side,
                q: w.q,
                want: Some(i),
            });
            continue;
        }
        // no room of its own: a lone tap across the main whose window overlaps this one moves
        // into the overlap, and the two share a cross
        let mut moves: Vec<(usize, f64)> = vec![];
        for (k, t) in taps.iter().enumerate() {
            let Some(j) = t.want else { continue };
            if t.side == w.side || taps.iter().filter(|o| (o.x - t.x).abs() < 1e-9).count() != 1 {
                continue;
            }
            let (lo, hi) = (w.lo.max(wants[j].lo), w.hi.min(wants[j].hi));
            if hi < lo - 1e-9 {
                continue;
            }
            moves.extend(grid(lo, hi).filter(|&x| fits(taps, j, x, Some(k))).filter(|&x| {
                let mut moved = taps.clone();
                moved[k].x = x;
                fits(&moved, i, x, None)
            }).map(|x| (k, x)));
        }
        let cost = |&(k, x): &(usize, f64)| (taps[k].x - x).abs() + (w.x - x).abs();
        moves.sort_by(|a, b| cost(a).total_cmp(&cost(b)));
        match moves.first() {
            Some(&(k, x)) => {
                taps[k].x = x;
                taps.push(Tap {
                    x,
                    side: w.side,
                    q: w.q,
                    want: Some(i),
                });
            }
            None => lost.push(i),
        }
    }
    lost
}

/// Plan distance from `q` to the segment `a`–`b`.
fn seg_dist(q: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((q.0 - a.0 - t * dx).powi(2) + (q.1 - a.1 - t * dy).powi(2)).sqrt()
}

fn len(p: &[(f64, f64, f64)]) -> f64 {
    p.windows(2)
        .map(|w| {
            let d = (w[1].0 - w[0].0, w[1].1 - w[0].1, w[1].2 - w[0].2);
            (d.0 * d.0 + d.1 * d.1 + d.2 * d.2).sqrt()
        })
        .sum()
}

/// What the ventilation adds to a project.
#[derive(Clone)]
struct Design {
    /// Depth under the storey top that the next system's layers start from.
    span: f64,
    fixtures: Vec<Vec<GFixture>>,
    /// Furniture the shaft displaces: storey, fixture index.
    clear: Vec<(usize, usize)>,
    ring: Vec<(f64, f64)>,
    hvac: GHvac,
    metrics: Vec<(&'static str, f64)>,
    /// Shaft positions that were possible.
    count: usize,
    /// Ducts over their velocity cap: the zone carries too much air for one shaft.
    fast: usize,
    /// Air the placed terminals deliver: supply, extract.
    placed: [f64; 2],
}

/// Plan the units and plant ducts on the roof take, with the clearance round them.
fn roof_taken(p: &GProject) -> Vec<Rect> {
    let n = p.storeys.len();
    let pad = rule("services.hvac.unit_clear");
    let mut out = vec![];
    for f in &p.hvac.plant {
        if let Some(u) = units().into_iter().find(|u| u.ty == f.ty) {
            out.push((f.at.0 - u.l / 2.0 - pad, f.at.1 - u.w / 2.0 - pad, f.at.0 + u.l / 2.0 + pad, f.at.1 + u.w / 2.0 + pad));
        }
    }
    for d in p.hvac.ducts.iter().filter(|d| d.storey >= n) {
        let r = d.size.0.max(d.size.1) / 2.0 + pad;
        for w in d.points.windows(2) {
            out.push((w[0].0.min(w[1].0) - r, w[0].1.min(w[1].1) - r, w[0].0.max(w[1].0) + r, w[0].1.max(w[1].1) + r));
        }
    }
    out
}

/// Duct shafts already in the plan, each with its side of the corridor.
fn existing_rings(cx: &Ctx) -> Vec<(Rect, f64)> {
    let mid = cx.corridor().map_or(0.0, |c| (c.1 + c.3) / 2.0);
    cx.p.storeys
        .first()
        .map(|st| {
            st.layout
                .shafts
                .iter()
                .filter(|s| s.kind == crate::ShaftKind::Duct)
                .map(|s| {
                    let b = bbox(&s.ring);
                    (b, if (b.1 + b.3) / 2.0 > mid { 1.0 } else { -1.0 })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn unserved(d: &Design) -> f64 {
    d.metrics
        .iter()
        .find(|m| m.0 == "hvac_unserved" || m.0 == "hvac_dedicated_unserved")
        .map_or(0.0, |m| m.1)
}

/// The first shaft position that serves every terminal, else the one serving most.
/// `why` gets the refusal counts when no shaft position is left.
fn best_design(
    spec: &ProjectSpec,
    p: &GProject,
    pass: &Pass,
    why: &mut Vec<(&'static str, f64)>,
) -> Result<Option<Design>, GenError> {
    let mut best: Option<Design> = None;
    // the tap spacing in fitting lengths: roomy first, tighter only when a terminal would
    // be left out
    for tee in [
        rule("services.hvac.tee_roomy"),
        rule("services.hvac.tee_mid"),
        rule("services.hvac.tee_tight"),
    ] {
        let mut rank = 0;
        while let Some(d) = design(spec, p, pass, rank, tee, why)? {
            let (done, count) = (unserved(&d) == 0.0, d.count);
            if done || best.as_ref().is_none_or(|b| unserved(&d) < unserved(b)) {
                best = Some(d);
            }
            if done {
                return Ok(best);
            }
            // at most ~60 positions, spread over the ranking
            rank += (count / rule("services.hvac.rank_max") as usize).max(1);
            if rank >= count {
                break;
            }
        }
        // the shaft positions do not depend on the spacing: none for one, none for any
        if best.is_none() {
            break;
        }
    }
    Ok(best)
}

/// Adds the ventilation to a built project: terminals to the storeys' fixtures, the shaft to
/// every storey, the plant and the ducts. A project without a corridor or without a place for
/// the shaft is left without ventilation; the metrics say so.
pub(crate) fn ventilate(spec: &ProjectSpec, p: &mut GProject) -> Result<(), GenError> {
    if !spec.mep_auto || spec.services.hvac == Some(false) {
        return Ok(());
    }
    let tags = dedicated_tags();
    let mut why = vec![];
    if !tags.is_empty() && has_tagged(spec, p, tags) {
        // the general zones without the tagged rooms, then their own system under them; when
        // the second one has no room, the zones serve all, as without the rule
        if let Some((mut q, span)) = zoned(spec, p, tags, &mut Vec::new())? {
            let own = Pass { tags, dedicated: true, drop: span, net: q.hvac.nets.len() as u8 + 1, plant_ix: q.hvac.plant.len(), win: None };
            if let Some(dd) = best_design(spec, &q, &own, &mut Vec::new())?.filter(|dd| unserved(dd) == 0.0) {
                place_design(&mut q, dd)?;
                finish(&mut q)?;
                *p = q;
                return Ok(());
            }
        }
    }
    let Some((mut q, _)) = zoned(spec, p, super::RoomTags::default(), &mut why)? else {
        p.metrics.push(("hvac_unserved", 1.0));
        p.metrics.extend(why);
        return Ok(());
    };
    finish(&mut q)?;
    *p = q;
    Ok(())
}

fn finish(p: &mut GProject) -> Result<(), GenError> {
    duct_outlets(p);
    // the zones report one sum per metric
    let mut out: Vec<(&'static str, f64)> = vec![];
    for (k, v) in p.metrics.drain(..) {
        match out.iter_mut().find(|m| m.0 == k && k.starts_with("hvac_")) {
            Some(m) => m.1 += v,
            None => out.push((k, v)),
        }
    }
    p.metrics = out;
    check_ducts(p)
}

/// Tallest main section the storeys allow, as `Ctx::h_max` reads it.
fn floors_h_max(p: &GProject) -> f64 {
    if p.storeys.first().is_some_and(|s| s.height >= rule("services.hvac.tall_storey")) {
        rule("services.hvac.h_max_tall")
    } else {
        rule("services.hvac.h_max_low")
    }
}

/// Whether a zone's design is one to build: every terminal placed, every duct within its
/// velocity cap, supply and extract balanced.
fn sound(d: &Design) -> bool {
    let [s, e] = d.placed;
    let cap = units().iter().map(|u| u.cap).fold(0.0, f64::max);
    unserved(d) == 0.0 && d.fast == 0 && s.max(e) <= cap + 1e-6 && (s - e).abs() <= rule("services.hvac.balance_max") * s.max(e)
}

/// The general system split into zones along the corridor, as few as carry the air within the
/// unit, riser and main limits: each zone its own shaft, unit and network. Returns the project
/// with them and the depth they take under the slab; `None` when no split serves every zone.
fn zoned(
    spec: &ProjectSpec,
    p: &GProject,
    tags: super::RoomTags,
    why: &mut Vec<(&'static str, f64)>,
) -> Result<Option<(GProject, f64)>, GenError> {
    let all = Pass { tags, ..Pass::all() };
    let floors = served(spec, p, &all);
    let dn = Dn::read(residential(spec.typology));
    let h_max = floors_h_max(p);
    let big = |f: &Floor| f.total[0].max(f.total[1]);
    let grand = [0, 1].map(|s| floors.iter().flatten().map(|f| f.total[s]).sum::<f64>()).into_iter().fold(0.0, f64::max);
    let floor_max = floors.iter().flatten().map(big).fold(0.0, f64::max);
    // a floor splits at its riser into two mains at most
    let main_cap = {
        let (mut lo, mut hi) = (0.0, floor_max.max(1.0));
        for _ in 0..40 {
            let m = (lo + hi) / 2.0;
            if main_fits(&dn, m, h_max) { lo = m } else { hi = m }
        }
        2.0 * lo
    };
    let need = |q: f64, cap: f64| if cap > 0.0 { (q / cap).ceil() as usize } else { 1 };
    let zone_cap = rule("services.hvac.zone_cap").min(units().iter().map(|u| u.cap).fold(0.0, f64::max));
    let n_min = need(grand, zone_cap).max(need(floor_max, main_cap)).max(1);
    let n_max = (rule("services.hvac.zones_max") as usize).max(n_min);
    // rooms by their centre along the corridor, with the air they move
    let mut along: Vec<(f64, f64)> =
        floors.iter().flatten().flat_map(|f| f.rooms.iter().map(|r| ((r.r.0 + r.r.2) / 2.0, r.q[0].max(r.q[1])))).collect();
    along.sort_by(|a, b| a.0.total_cmp(&b.0));
    let rects: Vec<Rect> = floors.iter().flatten().flat_map(|f| f.rooms.iter().map(|r| r.r)).collect();
    let (x0, x1) = floors.iter().flatten().flat_map(|f| f.rooms.iter().map(|r| (r.r.0, r.r.2))).fold((f64::MAX, f64::MIN), |a, c| (a.0.min(c.0), a.1.max(c.1)));
    let mut best: Option<(usize, GProject, f64)> = None;
    for n in n_min..=n_max {
        // cuts at equal lengths of the corridor, moved to the nearby wall line that splits
        // the fewest rooms on any storey: a split room keeps a sliver of its zone for terminals
        let mut cuts = vec![f64::MIN];
        for i in 1..n {
            let ideal = x0 + (x1 - x0) * i as f64 / n as f64;
            let prev = *cuts.last().expect("a cut");
            let cost = |c: f64| {
                let split: f64 = rects
                    .iter()
                    .filter(|r| r.0 < c - 1e-6 && r.2 > c + 1e-6)
                    .map(|r| (c - r.0).min(r.2 - c))
                    .sum();
                (c - ideal).abs() + rule("services.hvac.zone_split_weight") * split
            };
            let pick = rects
                .iter()
                .flat_map(|r| [r.0, r.2])
                .chain(along.windows(2).map(|w| (w[0].0 + w[1].0) / 2.0))
                .filter(|&c| (c - ideal).abs() <= (x1 - x0) / n as f64 / 3.0)
                .filter(|&c| along.iter().any(|a| a.0 > prev && a.0 < c) && along.iter().any(|a| a.0 > c))
                .min_by(|a, b| cost(*a).total_cmp(&cost(*b)));
            if let Some(c) = pick {
                cuts.push(c);
            }
        }
        if cuts.len() < n {
            break;
        }
        cuts.push(f64::MAX);
        // a zone that finds no shaft (a stretch of lobbies and stairs) joins its neighbour
        let (q, span, bad, whole, zone_why) = loop {
            let mut q = p.clone();
            let (mut span, mut bad, mut failed) = (0.0f64, 0usize, None);
            let mut zone_why = vec![];
            let m = cuts.len() - 1;
            for i in 0..m {
                let win = (m > 1).then(|| (cuts[i], cuts[i + 1]));
                let pass = Pass { tags, net: i as u8, plant_ix: q.hvac.plant.len(), win, ..Pass::all() };
                let Some(d) = best_design(spec, &q, &pass, &mut zone_why)? else {
                    failed = Some(i);
                    break;
                };
                if !sound(&d) {
                    bad += 1 + unserved(&d) as usize + d.fast;
                }
                span = span.max(d.span);
                place_design(&mut q, d)?;
            }
            match failed {
                None => break (q, span, bad, true, zone_why),
                Some(_) if m == 1 => break (q, span, bad, false, zone_why),
                Some(i) => {
                    cuts.remove(if i + 1 < m { i + 1 } else { i });
                }
            }
        };
        if !whole {
            if n == n_min {
                why.extend(zone_why);
            }
            continue;
        }
        if bad == 0 {
            return Ok(Some((q, span)));
        }
        if best.as_ref().is_none_or(|b| bad < b.0) {
            best = Some((bad, q, span));
        }
    }
    Ok(best.map(|b| (b.1, b.2)))
}

/// Tags of the rooms the rules give their own outdoor air system.
fn dedicated_tags() -> super::RoomTags {
    let tag = |id| super::RoomTags::parse(id).unwrap_or_default();
    match rule("mep.hvac.dedicated_oa") as usize {
        1 => tag("operating"),
        2 => tag("isolation"),
        3 => tag("operating").with(tag("isolation")),
        _ => super::RoomTags::default(),
    }
}

/// Whether a room that needs air carries one of the tags.
fn has_tagged(spec: &ProjectSpec, p: &GProject, tags: super::RoomTags) -> bool {
    p.storeys.iter().any(|st| {
        st.rooms.iter().zip(&st.layout.rooms).any(|(pr, gr)| spec.room_tags.of(pr.kind, &gr.name).meets(tags))
    })
}

/// The design's terminals, shaft, metrics and network added to the project; the furniture the
/// shaft displaces leaves, and the links of systems placed earlier follow their fixtures.
fn place_design(p: &mut GProject, mut d: Design) -> Result<(), GenError> {
    d.clear.sort_unstable();
    for &(k, i) in d.clear.iter().rev() {
        p.storeys[k].layout.fixtures.remove(i);
    }
    for l in &mut p.hvac.links {
        if l.storey < p.storeys.len() {
            l.fixture -= d.clear.iter().filter(|c| c.0 == l.storey && c.1 < l.fixture).count();
        }
    }
    for (k, st) in p.storeys.iter_mut().enumerate() {
        st.layout.fixtures.append(&mut d.fixtures[k]);
        if !st.layout.shafts.iter().any(|s| s.kind == crate::ShaftKind::Duct && s.ring == d.ring) {
            st.layout.shafts.push(GShaft::new(d.ring.clone(), crate::ShaftKind::Duct));
        }
        if st.layout.fixtures.len() > super::slots::FIXTURES_MAX as usize {
            return Err(GenError::Infeasible(format!(
                "{} fixtures and air terminals on floor {}; at most {}: split the building",
                st.layout.fixtures.len(),
                k + 1,
                super::slots::FIXTURES_MAX
            )));
        }
    }
    p.metrics.extend(d.metrics);
    if p.hvac.plant.is_empty() {
        p.hvac = d.hvac;
    } else {
        p.hvac.plant.extend(d.hvac.plant);
        p.hvac.ducts.extend(d.hvac.ducts);
        p.hvac.links.extend(d.hvac.links);
        p.hvac.nets.push(d.hvac.names);
    }
    Ok(())
}

fn check_ducts(p: &GProject) -> Result<(), GenError> {
    if p.hvac.ducts.len() > super::slots::DUCTS_MAX as usize {
        return Err(GenError::Infeasible(format!(
            "{} air ducts; at most {}: split the building",
            p.hvac.ducts.len(),
            super::slots::DUCTS_MAX
        )));
    }
    Ok(())
}

/// The rooms of each storey the pass serves, with their air; `None` for a storey without a
/// corridor. A zone takes the rooms whose centre lies in its stretch.
fn served(spec: &ProjectSpec, p: &GProject, pass: &Pass) -> Vec<Option<Floor>> {
    let res = residential(spec.typology);
    let mut floors = vec![];
    let air = super::mech::AirPlan::new(spec, p);
    for (sk, st) in p.storeys.iter().enumerate() {
        let clear = st.height - svc::slab();
        let corr = st
            .rooms
            .iter()
            .position(|r| r.kind == RoomKind::Corridor)
            .map(|i| bbox(&st.layout.rooms[i].polygon));
        let Some(corr) = corr else {
            floors.push(None);
            continue;
        };
        let mid = (corr.1 + corr.3) / 2.0;
        let mut rooms = vec![];
        let mut total = [0.0; 2];
        for (i, (pr, gr)) in st.rooms.iter().zip(&st.layout.rooms).enumerate() {
            let r = bbox(&gr.polygon);
            let area = (r.2 - r.0) * (r.3 - r.1);
            let tags = spec.room_tags.of(pr.kind, &gr.name);
            if pass.skips(tags) || pass.win.is_some_and(|w| !((r.0 + r.2) / 2.0 >= w.0 && (r.0 + r.2) / 2.0 < w.1)) {
                continue;
            }
            let (qs, qe) = spec
                .model
                .as_ref()
                .and_then(|m| m.air(pr.kind, &gr.name, area, clear, &spec.rules))
                .unwrap_or_else(|| air.room(sk, i, demand(pr.kind, area, clear, res, tags)));
            if qs + qe <= 0.0 || (r.1 < corr.3 - 1e-6 && r.3 > corr.1 + 1e-6) {
                continue;
            }
            let side = if (r.1 + r.3) / 2.0 > mid { 1.0 } else { -1.0 };
            let (near, far) = if side > 0.0 { (r.1, r.3) } else { (r.3, r.1) };
            total[0] += qs;
            total[1] += qe;
            rooms.push(Served {
                room: i,
                r,
                side,
                near,
                far,
                q: [qs, qe],
            });
        }
        floors.push(Some(Floor {
            elev: st.elevation,
            corr,
            rooms,
            total,
        }));
    }
    balance(&mut floors, pass.dedicated);
    floors
}

/// Raises the smaller of a zone's supply and extract to the larger, spread over the rooms that
/// have it, so the unit moves as much air in as out; a dedicated system keeps its rooms'
/// pressure but within the cascade limit.
fn balance(floors: &mut [Option<Floor>], dedicated: bool) {
    let tot = [0, 1].map(|s| floors.iter().flatten().map(|f| f.total[s]).sum::<f64>());
    let big = tot[0].max(tot[1]);
    let small = if tot[0] < tot[1] { 0 } else { 1 };
    let want = if dedicated { big * (1.0 - rule("services.hvac.cascade_max")) } else { big };
    if tot[small] <= 0.0 || tot[small] >= want - 1e-9 {
        return;
    }
    let k = want / tot[small];
    for f in floors.iter_mut().flatten() {
        for r in &mut f.rooms {
            r.q[small] = up5(r.q[small] * k);
        }
        f.total[small] = f.rooms.iter().map(|r| r.q[small]).sum();
    }
}

/// The `rank`-th best shaft position; `None` when there are fewer.
fn design(
    spec: &ProjectSpec,
    p: &GProject,
    pass: &Pass,
    rank: usize,
    tee: f64,
    why: &mut Vec<(&'static str, f64)>,
) -> Result<Option<Design>, GenError> {
    let res = residential(spec.typology);
    let floors = served(spec, p, pass);
    let grand = [0, 1].map(|s| floors.iter().flatten().map(|f| f.total[s]).sum::<f64>());
    let has = grand.map(|g| g > 0.0);
    if (pass.dedicated || pass.win.is_some()) && !has[0] && !has[1] {
        return Ok(None);
    }
    let pitched = (spec.roof == RoofKind::Pitched).then_some(p.roof.slope);
    let dn = Dn::read(res);
    let mut cx = Ctx {
        p,
        floors,
        has,
        grand,
        riser_d: grand.map(|g| if g > 0.0 { round(&dn, g, dn.v_riser) } else { 0.0 }),
        zm: [0.0; 2],
        face: [0.0; 2],
        neck: [rule("services.hvac.neck_supply"), rule("services.hvac.neck_extract")],
        pitched,
        refused: Default::default(),
        heads: super::fire::head_keepout(spec, p),
        k: dn,
        unit_t: pick_unit(grand[0].max(grand[1])),
        taken: roof_taken(p),
        rings: vec![],
        win: pass.win.map(|w| (w.0 + rule("services.hvac.zone_gap"), w.1 - rule("services.hvac.zone_gap"))),
        zone: pass.win,
    };
    let tap_extra = rule("services.hvac.tap_extra");
    let h_max = cx.h_max();
    let spacing = tee * node_trim(&dn, &[main_widest(&dn, cx.floors_max(0).max(cx.floors_max(1)), h_max)])
        + tap_extra;
    // terminals first: their branch sizes set the layers
    let mut wants: Vec<Want> = vec![];
    let mut skipped = 0usize;
    for (k, f) in cx.floors.iter().enumerate() {
        let Some(f) = f else { continue };
        for sv in &f.rooms {
            let both = sv.q[0] > 0.0 && sv.q[1] > 0.0;
            for s in 0..2 {
                let q = sv.q[s];
                if q <= 0.0 {
                    continue;
                }
                let (wl, wh) = cx.win.unwrap_or((f64::MIN, f64::MAX));
                let lo = (sv.r.0 + dn.wall_keep).max(f.corr.0 + dn.corr_end).max(wl);
                let hi = (sv.r.2 - dn.wall_keep).min(f.corr.2 - dn.corr_end).min(wh);
                if hi < lo {
                    skipped += 1;
                    continue;
                }
                // terminals spread along each branch across the room first: one take-off feeds
                // a row of them, so a deep room does not crowd the main with taps
                let total = ((q / terms(s)[1].cap).ceil() as usize).max(1);
                let depth = (sv.far - sv.near).abs() - 2.0 * dn.wall_keep;
                let d0 = round(&dn, q, dn.v_branch);
                let row = row_pitch(&dn, d0);
                // a hood's outlet tees into the room's one extract branch
                let hood = s == 1 && {
                    let st = &cx.p.storeys[k];
                    st.layout.fixtures.iter().any(|fx| !super::svc::air_outlets(fx).is_empty() && super::svc::inside(fx.at, &[sv.r.0, sv.r.1, sv.r.2, sv.r.3]))
                };
                let ny = if hood { 1 } else { total.min(1 + (depth.max(0.0) / row).floor() as usize).max(1) };
                let fit = 1 + ((hi - lo) / spacing).floor() as usize;
                let n = total.div_ceil(ny).clamp(1, fit);
                let qt = up5(q / (n * ny) as f64);
                let term = term_for(s, qt);
                let dd = round(&dn, qt, dn.v_branch).max(term.neck_d);
                let d = round(&dn, qt * ny as f64, dn.v_branch).max(dd);
                let frac = match (both, s) {
                    (false, _) => 0.5,
                    (true, 0) => rule("services.hvac.frac_supply_both"),
                    _ => rule("services.hvac.frac_extract_both"),
                };
                let y = sv.near + frac * (sv.far - sv.near);
                for i in 0..n {
                    let x = if n == 1 {
                        ((sv.r.0 + sv.r.2) / 2.0).clamp(lo, hi)
                    } else {
                        lo + (hi - lo) * i as f64 / (n - 1) as f64
                    };
                    wants.push(Want {
                        floor: k,
                        room: sv.room,
                        sys: s,
                        side: sv.side,
                        x,
                        lo,
                        hi,
                        y,
                        dist: 0.0,
                        q: qt * ny as f64,
                        qt,
                        ny,
                        drops: vec![],
                        hood,
                        term,
                        d,
                        dd,
                    });
                }
            }
        }
    }
    // rooms stacked across the corridor share one x window: spread their terminals over it
    for i in 0..wants.len() {
        let (f, s, sd, lo, hi) = (wants[i].floor, wants[i].sys, wants[i].side, wants[i].lo, wants[i].hi);
        let mut group: Vec<usize> = (0..wants.len())
            .filter(|&j| {
                let w = &wants[j];
                (w.floor, w.sys, w.side, w.lo, w.hi) == (f, s, sd, lo, hi)
            })
            .collect();
        if group.len() < 2 || group[0] != i {
            continue;
        }
        group.sort_by_key(|&j| wants[j].room);
        let m = group.len();
        for (k, &j) in group.iter().enumerate() {
            wants[j].x = lo + (hi - lo) * k as f64 / (m - 1) as f64;
        }
    }
    // the terminals round their flows up: the risers and the unit carry what they deliver
    cx.grand = [0, 1].map(|s| wants.iter().filter(|w| w.sys == s).map(|w| w.q).sum::<f64>());
    cx.riser_d = cx.grand.map(|g| if g > 0.0 { round(&dn, g, dn.v_riser) } else { 0.0 });
    cx.unit_t = pick_unit(cx.grand[0].max(cx.grand[1]));
    let dmax = [0, 1].map(|s| {
        wants
            .iter()
            .filter(|w| w.sys == s)
            .map(|w| w.d)
            .fold(0.0, f64::max)
    });
    // the shared duct band hangs under the lowest ceiling so it clears every storey
    let h = cx.p.storeys.iter().map(|s| s.height).reduce(f64::min).unwrap_or(rule("services.hvac.storey_height"));
    let top = h - svc::slab() - dn.top_gap - pass.drop;
    let band = [0, 1].map(|s| h_max.max(dmax[s]));
    cx.zm[0] = top - band[0] / 2.0;
    cx.zm[1] = if cx.has[0] {
        cx.zm[0] - band[0] / 2.0 - dn.layer_gap - band[1] / 2.0
    } else {
        top - band[1] / 2.0
    };
    for s in 0..2 {
        cx.face[s] = cx.zm[s] - dn.drop_bend * dmax[s] - dn.drop_gap - cx.neck[s];
    }
    // every terminal sits on one ceiling plane, the lowest any system needs
    let ceil = (0..2)
        .filter(|&s| cx.has[s])
        .map(|s| cx.face[s])
        .fold(f64::INFINITY, f64::min);
    if ceil.is_finite() {
        cx.face = [ceil; 2];
    }
    // what the next system under this one starts from: the bottom of the lowest main
    let span = top + pass.drop
        - (0..2).filter(|&s| cx.has[s]).map(|s| cx.zm[s] - band[s] / 2.0).fold(f64::INFINITY, f64::min)
        + dn.layer_gap;
    if pass.dedicated && ceil.is_finite() && ceil < rule("mep.hvac.dedicated_min_face") {
        why.push(("hvac_dedicated_low_ceiling", 1.0));
        return Ok(None);
    }
    cx.rings = existing_rings(&cx);
    // the shaft: beside the corridor, in a service room on every storey, plant fitting above
    let mut found: Vec<(f64, Shaft)> = vec![];
    let scan = rule("services.hvac.shaft_scan");
    if let Some(c) = cx.corridor() {
        // the building's own duct bays, on every storey alike, take the risers first
        let (wl, wh) = cx.zone.unwrap_or((f64::MIN, f64::MAX));
        let mut bays = core_bays(p, crate::ShaftKind::Duct);
        bays.retain(|r| r.0 >= wl && r.2 <= wh);
        for r in &bays {
            let side = if (r.1 + r.3) / 2.0 > (c.1 + c.3) / 2.0 { 1.0 } else { -1.0 };
            found.extend(cx.try_shaft(side, r.0, Some(*r)));
        }
        if found.is_empty() {
            if !bays.is_empty() {
                why.push(("hvac_core_bay_unfit", bays.len() as f64));
            }
            for side in [1.0, -1.0] {
                let mut x = c.0.max(wl) + 0.5;
                while x <= c.2.min(wh) - 0.5 {
                    found.extend(cx.try_shaft(side, x, None));
                    x += scan;
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    let count = found.len();
    let Some((_, sh)) = found.into_iter().nth(rank) else {
        if count == 0 {
            if cx.corridor().is_none() {
                why.push(("hvac_refused_no_corridor", 1.0));
            }
            let n = cx.refused.borrow();
            why.extend(REFUSALS.iter().zip(n.iter()).filter(|(_, c)| **c > 0).map(|(r, c)| (*r, *c as f64)));
        }
        return Ok(None);
    };
    // terminal positions across the corridor, kept clear of the main for the tee and drop
    let ym = sh.ym;
    let mut dropped = vec![];
    for (i, w) in wants.iter_mut().enumerate() {
        let f = cx.floors[w.floor].as_ref().expect("a served floor");
        let sv_far = f
            .rooms
            .iter()
            .find(|r| r.room == w.room)
            .map(|r| (r.near, r.far));
        let need = node_trim(&dn, &[main_widest(&dn, f.total[w.sys], h_max), (w.d, 0.0)])
            + dn.drop_bend * w.d
            + dn.margin;
        let ymw = ym[w.sys] + cx.shift(w.floor);
        let min_y = ymw + w.side * need;
        let mut y = w.y;
        if (y - ymw) * w.side < need {
            y = min_y;
        }
        let mut ys = vec![y];
        if let Some((near, far)) = sv_far {
            let lo = near.min(far) + dn.wall_keep;
            let hi = near.max(far) - dn.wall_keep;
            if y < lo - 1e-9 || y > hi + 1e-9 {
                dropped.push(i);
            } else if w.ny > 1 {
                // the row from the first drop to the far wall, as many as its pitch allows
                let last = far - w.side * dn.wall_keep;
                let fit = 1 + ((last - y) * w.side / row_pitch(&dn, w.d)).max(0.0).floor() as usize;
                let m = w.ny.min(fit);
                if m > 1 {
                    ys = (0..m).map(|j| y + (last - y) * j as f64 / (m - 1) as f64).collect();
                }
                if m != w.ny {
                    w.qt = up5(w.q / m as f64);
                    w.q = w.qt * m as f64;
                    w.term = term_for(w.sys, w.qt);
                    w.dd = round(&dn, w.qt, dn.v_branch).max(w.term.neck_d);
                    w.d = round(&dn, w.q, dn.v_branch).max(w.dd);
                    w.ny = m;
                }
            }
        }
        w.y = y;
        w.drops = ys.into_iter().map(|y| Drop { y, q: w.qt, term: w.term, d: w.dd, room: w.room }).collect();
    }
    // rooms stacked away from the corridor in one window, more than its taps fit: one branch
    // runs out through the near rooms and drops into each in turn
    let mut absorbed: Vec<usize> = vec![];
    for i in 0..wants.len() {
        if dropped.contains(&i) || absorbed.contains(&i) {
            continue;
        }
        let w = wants[i].clone();
        let key = (w.floor, w.sys, w.side, w.lo, w.hi);
        let mut group: Vec<usize> = (i..wants.len())
            .filter(|&j| !dropped.contains(&j) && !absorbed.contains(&j))
            .filter(|&j| (wants[j].floor, wants[j].sys, wants[j].side, wants[j].lo, wants[j].hi) == key)
            .collect();
        let fit = 1 + ((w.hi - w.lo) / spacing).floor() as usize;
        group.retain(|&j| !wants[j].hood);
        if group.len() <= fit {
            continue;
        }
        let ymw = ym[w.sys] + cx.shift(w.floor);
        group.sort_by(|a, b| (wants[*a].y - ymw).abs().total_cmp(&(wants[*b].y - ymw).abs()));
        for c in 0..fit {
            let chain: Vec<usize> = group.iter().copied().skip(c).step_by(fit).collect();
            let head = chain[0];
            let mut drops: Vec<Drop> = chain.iter().flat_map(|&j| wants[j].drops.clone()).collect();
            drops.sort_by(|a, b| (a.y - ymw).abs().total_cmp(&(b.y - ymw).abs()));
            let q: f64 = drops.iter().map(|d| d.q).sum();
            let x = if fit == 1 { (w.lo + w.hi) / 2.0 } else { w.lo + (w.hi - w.lo) * c as f64 / (fit - 1) as f64 };
            let h = &mut wants[head];
            h.q = q;
            h.d = round(&dn, q, dn.v_branch).max(drops.iter().map(|d| d.d).fold(0.0, f64::max));
            h.y = drops[0].y;
            h.x = x;
            h.drops = drops;
            absorbed.extend(chain.into_iter().skip(1));
        }
    }
    for w in &mut wants {
        let ymw = ym[w.sys] + cx.shift(w.floor);
        let reach = w.drops.iter().map(|d| (d.y - ymw).abs());
        w.dist = if w.sys == 0 { reach.fold(f64::MAX, f64::min) } else { reach.fold(0.0, f64::max) };
    }
    // taps: extract first, then supply clear of extract branches it would cross
    let excl_pad = rule("services.hvac.shaft_excl");
    let leg_clear = rule("services.hvac.leg_clear");
    let leg_sep = rule("services.hvac.leg_sep");
    let leg_slack = rule("services.hvac.leg_slack");
    let leg_off = rule("services.hvac.leg_off");
    let branch_clear = rule("services.hvac.branch_clear");
    let excl = (sh.ring.0 - excl_pad, sh.ring.2 + excl_pad);
    // far branches: system, x span along the shaft line, width, x of the run to the main
    let legs: Vec<(usize, f64, f64, f64, f64)> = (0..2)
        .filter_map(|s| {
            let w = main_widest(&dn, cx.floors_max(s), h_max).0;
            sh.legs[s].map(|xa| (s, sh.xr[s].min(xa), sh.xr[s].max(xa), w, xa))
        })
        .collect();
    // a supply drop on the line of the extract far branch moves off it, toward the main
    for w in wants.iter_mut().filter(|w| w.sys == 0 && w.side == sh.side) {
        let Some(c) = legs
            .iter()
            .filter(|l| l.0 == 1 && w.hi > l.1 - w.d / 2.0 - l.3 / 2.0 - leg_clear && w.lo < l.2 + w.d / 2.0 + l.3 / 2.0 + leg_clear)
            .map(|l| w.d / 2.0 + l.3 / 2.0 + leg_sep)
            .reduce(f64::max)
            .filter(|c| (w.y - sh.yr).abs() < *c)
        else {
            continue;
        };
        let f = cx.floors[w.floor].as_ref().expect("a served floor");
        let Some(r) = f.rooms.iter().find(|r| r.room == w.room) else { continue };
        let need = node_trim(&dn, &[main_widest(&dn, f.total[0], h_max), (w.d, 0.0)]) + dn.drop_bend * w.d + dn.margin;
        let ymw = sh.ym[0] + cx.shift(w.floor);
        let y = sh.yr - sh.side * (c + leg_off);
        if y >= r.near.min(r.far) + dn.wall_keep - 1e-9
            && y <= r.near.max(r.far) - dn.wall_keep + 1e-9
            && (y - ymw) * sh.side >= need - 1e-9
        {
            w.y = y;
            w.dist = (y - ymw).abs();
        }
    }
    let mut floor_taps: Vec<[Vec<Tap>; 2]> = (0..cx.floors.len()).map(|_| [vec![], vec![]]).collect();
    for (k, f) in cx.floors.iter().enumerate() {
        let Some(f) = f else { continue };
        for s in 0..2 {
            if f.total[s] > 0.0 {
                for x in std::iter::once(sh.xr[s]).chain(sh.legs[s]) {
                    floor_taps[k][s].push(Tap {
                        x,
                        side: sh.side,
                        q: 0.0,
                        want: None,
                    });
                }
            }
        }
    }
    for s in [1, 0] {
        for k in 0..cx.floors.len() {
            let mut order: Vec<usize> = (0..wants.len())
                .filter(|&i| wants[i].floor == k && wants[i].sys == s && !dropped.contains(&i) && !absorbed.contains(&i))
                .collect();
            // narrow windows first: they cannot move out of the way of a wide one
            order.sort_by(|a, b| {
                let width = |i: usize| wants[i].hi - wants[i].lo;
                width(*a)
                    .total_cmp(&width(*b))
                    .then(wants[*a].x.total_cmp(&wants[*b].x))
            });
            let ext: Vec<(f64, f64, f64, f64)> = floor_taps[k][1]
                .iter()
                .filter_map(|t| t.want.map(|i| (t.x, t.side, wants[i].dist, wants[i].d)))
                .collect();
            let blocked = |w: &Want, x: f64| {
                if w.side == sh.side && x > excl.0 - w.d / 2.0 && x < excl.1 + w.d / 2.0 {
                    return true;
                }
                if cx.rings.iter().any(|(r, sd)| w.side == *sd && x > r.0 - excl_pad - w.d / 2.0 && x < r.2 + excl_pad + w.d / 2.0) {
                    return true;
                }
                // a far branch runs along the shaft line, then across to its main at `xa`: a
                // branch of its own layer may not reach that line, and a supply drop, which
                // passes the extract layer, may not stand on it
                if w.side == sh.side
                    && legs.iter().any(|&(ls, lo, hi, lw, xa)| {
                        let c = w.d / 2.0 + lw / 2.0 + leg_sep;
                        let along = x > lo - c - leg_slack && x < hi + c + leg_slack;
                        let beyond = (w.y - sh.yr) * sh.side > -c;
                        if ls == w.sys {
                            along && beyond
                        } else if w.sys == 0 {
                            let across = (x - xa).abs() < c && beyond;
                            (along && (w.y - sh.yr).abs() < c) || across
                        } else {
                            false
                        }
                    })
                {
                    return true;
                }
                w.sys == 0
                    && ext.iter().any(|&(ex, es, ed, edd)| {
                        es == w.side && (ex - x).abs() < (w.d + edd) / 2.0 + branch_clear && ed > w.dist
                    })
            };
            let mut taps = std::mem::take(&mut floor_taps[k][s]);
            // a split floor's mains are narrower than one main for the floor: so are the tees
            let sp = spacing.min(tee * node_trim(&dn, &[main_widest(&dn, sh.lq[s], h_max)]) + tap_extra);
            // a main end is an elbow and reducer into the last drop: that may take more
            let main = main_widest(&dn, sh.lq[s], h_max);
            let end = wants
                .iter()
                .filter(|w| w.sys == s)
                .map(|w| dn.drop_bend * w.d + mep::transition_length(&svc::duct_section(main), &svc::duct_section((w.d, 0.0))))
                .fold(0.0, f64::max);
            let floor = tee_leg(main) + tee_leg(main).max(end) + dn.margin;
            let lost = place(&wants, &order, &mut taps, sp, floor, &blocked);
            dropped.extend(lost);
            floor_taps[k][s] = taps;
        }
    }
    // the network
    let n = cx.p.storeys.len();
    let mut ducts: Vec<GDuct> = vec![];
    let mut links: Vec<GAirLink> = vec![];
    let mut terminals = 0usize;
    let mut fixtures: Vec<Vec<GFixture>> = vec![vec![]; n];
    for (k, f) in cx.floors.iter().enumerate() {
        let Some(f) = f else { continue };
        for s in 0..2 {
            let taps = &floor_taps[k][s];
            if taps.iter().all(|t| t.want.is_none()) {
                continue;
            }
            let z = f.elev + cx.zm[s] + cx.lift(k);
            let y = ym[s] + cx.shift(k);
            let xr = sh.xr[s];
            for t in taps {
                let Some(i) = t.want else { continue };
                let w = &wants[i];
                // the displaced furniture leaves before the terminals are appended
                let gone = sh.clear.iter().filter(|c| c.0 == k).count();
                let m = w.drops.len();
                let mut from = y;
                for (j, dr) in w.drops.iter().enumerate() {
                    let room_both = f.rooms.iter().find(|r| r.room == dr.room).is_some_and(|r| r.q[0] > 0.0 && r.q[1] > 0.0);
                    let face = if room_both { cx.face[0].min(cx.face[1]) } else { cx.face[s] };
                    let neck = f.elev + face + dr.term.neck_h;
                    let last = j + 1 == m;
                    let q: f64 = w.drops[j..].iter().map(|d| d.q).sum();
                    let dj = if j == 0 { w.d } else if last { dr.d } else { round(&dn, q, dn.v_branch).max(dr.d) };
                    // a drop before the branch end stands off it, towards the middle of its room
                    let xd = if last {
                        t.x
                    } else {
                        let stub = node_trim(&dn, &[(dj, 0.0), (dr.d, 0.0)]) + dn.drop_bend * dr.d + dn.margin;
                        let mid = f.rooms.iter().find(|r| r.room == dr.room).map_or(t.x, |r| (r.r.0 + r.r.2) / 2.0);
                        if mid >= t.x { t.x + stub } else { t.x - stub }
                    };
                    fixtures[k].push(GFixture { scale: 1.0, min_clear: 0.0,
                        family: dr.term.fam,
                        ty: dr.term.ty,
                        at: (xd, dr.y),
                        rotation: 0.0,
                        wall: None,
                        z: face + cx.lift(k),
                    });
                    let idx = cx.p.storeys[k].layout.fixtures.len() - gone + fixtures[k].len() - 1;
                    links.push(GAirLink {
                        storey: k,
                        fixture: idx,
                        connector: "Air",
                        sys: SYS[s],
                        net: pass.net,
                    });
                    terminals += 1;
                    let mut points = vec![(t.x, from, z), (t.x, dr.y, z)];
                    if last {
                        points.push((t.x, dr.y, neck));
                    }
                    ducts.push(GDuct {
                        net: pass.net,
                        sys: SYS[s],
                        storey: k,
                        points,
                        size: (dj, 0.0),
                        flow: q,
                        bend: last.then_some(dn.drop_bend * dj),
                    });
                    if !last {
                        // a side stub off the branch tee, then the elbow down to the neck
                        ducts.push(GDuct {
                            net: pass.net,
                            sys: SYS[s],
                            storey: k,
                            points: vec![(t.x, dr.y, z), (xd, dr.y, z), (xd, dr.y, neck)],
                            size: (dr.d, 0.0),
                            flow: dr.q,
                            bend: Some(dn.drop_bend * dr.d),
                        });
                    }
                    from = dr.y;
                }
            }
            // main segments between tap positions, each carrying what lies beyond its feed:
            // the riser, or for a split floor the far branch's end for the taps past it
            let leg = sh.legs[s];
            let far = |x: f64| leg.is_some_and(|xa| (x - xr) * (xa - xr) > 1e-9);
            for (group, feed) in [(false, xr), (true, leg.unwrap_or(xr))] {
                if group && leg.is_none() {
                    continue;
                }
                let g: Vec<&Tap> = taps.iter().filter(|t| far(t.x) == group).collect();
                if g.iter().all(|t| t.want.is_none()) {
                    continue;
                }
                let mut xs: Vec<f64> = g.iter().map(|t| t.x).collect();
                xs.sort_by(f64::total_cmp);
                xs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
                for p in xs.windows(2) {
                    let (a, b) = (p[0], p[1]);
                    let q: f64 = if a >= feed - 1e-9 {
                        g.iter().filter(|t| t.x >= b - 1e-9).map(|t| t.q).sum()
                    } else {
                        g.iter().filter(|t| t.x <= a + 1e-9).map(|t| t.q).sum()
                    };
                    ducts.push(GDuct {
                net: pass.net,
                        sys: SYS[s],
                        storey: k,
                        points: vec![(a, y, z), (b, y, z)],
                        size: main_size(&dn, q, h_max),
                        flow: q,
                        bend: None,
                    });
                }
                let q: f64 = g.iter().map(|t| t.q).sum();
                let mut points = vec![(xr, sh.yr, z)];
                if group {
                    points.push((feed, sh.yr, z));
                }
                points.push((feed, y, z));
                ducts.push(GDuct {
                net: pass.net,
                    sys: SYS[s],
                    storey: k,
                    points,
                    size: main_size(&dn, q, h_max),
                    flow: q,
                    bend: None,
                });
            }
        }
    }
    // risers: from the lowest served storey up to the plant, carrying what the taps deliver
    let fq: Vec<[f64; 2]> = floor_taps.iter().map(|t| [0, 1].map(|s| t[s].iter().filter(|t| t.want.is_some()).map(|t| t.q).sum())).collect();
    let placed = [0, 1].map(|s| fq.iter().map(|q| q[s]).sum::<f64>());
    let roof = cx.p.roof.elevation;
    let unit = sh.unit;
    let (z1, z2) = cx.plant_z(unit);
    let pz = roof + dn.plinth + unit.h;
    for s in 0..2 {
        let served: Vec<usize> = (0..cx.floors.len())
            .filter(|&k| floor_taps[k][s].iter().any(|t| t.want.is_some()))
            .collect();
        let Some(&k0) = served.first() else { continue };
        let xr = sh.xr[s];
        let node = |k: usize| cx.p.storeys[k].elevation + cx.zm[s] + cx.lift(k);
        let mut below = 0.0;
        for k in k0..n - 1 {
            below += fq[k][s];
            ducts.push(GDuct {
                net: pass.net,
                sys: SYS[s],
                storey: k,
                points: vec![(xr, sh.yr, node(k)), (xr, sh.yr, node(k + 1))],
                size: (round(&dn, below, dn.v_riser), 0.0),
                flow: below,
                bend: None,
            });
        }
        let all = placed[s];
        let ztop = node(n - 1);
        // up the riser, a jog along the shaft when the port is not above it, over to the unit
        let zl = roof + if s == 0 { z1 } else { z2 };
        let mut px = Ctx::ports(&dn, sh.at, sh.rot, unit)[s];
        let mut points = vec![(xr, sh.yr, ztop)];
        let mut o = (xr, sh.yr);
        if !sh.lift[s].is_empty() {
            let zlo = roof + sh.zlo[s];
            points.push((xr, sh.yr, zlo));
            points.extend(sh.lift[s].iter().map(|&(x, y)| (x, y, zlo)));
            o = *sh.lift[s].last().expect("a low run");
        }
        points.push((o.0, o.1, zl));
        if (px - o.0).abs() > 1e-9 {
            points.push((px, o.1, zl));
        } else {
            px = o.0;
        }
        points.extend([(px, sh.at.1, zl), (px, sh.at.1, pz)]);
        ducts.push(GDuct {
                net: pass.net,
            sys: SYS[s],
            storey: n,
            points,
            size: (round(&dn, all, dn.v_riser), 0.0),
            flow: all,
            bend: None,
        });
        links.push(GAirLink {
            storey: n,
            fixture: pass.plant_ix,
            connector: if s == 0 { "Supply" } else { "Return" },
            sys: SYS[s],
            net: pass.net,
        });
    }
    let plant = GFixture { scale: 1.0, min_clear: 0.0,
        family: UNIT_FAMILY,
        ty: unit.ty,
        at: sh.at,
        rotation: sh.rot,
        wall: None,
        z: dn.plinth,
    };
    let ring = crate::layout::rect(sh.ring.0, sh.ring.1, sh.ring.2, sh.ring.3);
    let metres: f64 = ducts.iter().map(|d| len(&d.points)).sum();
    let fast = ducts.iter().filter(|d| velocity(d.flow, duct_area(d.size)) > cap_of(&dn, d, n) + 0.05).count();
    let no = pass.net + 1;
    let names = if spec.ru {
        (format!("\u{41f}{no}"), format!("\u{412}{no}"))
    } else {
        (format!("S{no}"), format!("E{no}"))
    };
    let metrics = if pass.dedicated {
        vec![
            ("hvac_dedicated_supply_m3h", placed[0]),
            ("hvac_dedicated_extract_m3h", placed[1]),
            ("hvac_dedicated_terminals", terminals as f64),
            ("hvac_dedicated_unserved", (dropped.len() + skipped) as f64),
            ("hvac_dedicated_duct_m", (metres * 10.0).round() / 10.0),
        ]
    } else {
        vec![
            ("hvac_supply_m3h", placed[0]),
            ("hvac_extract_m3h", placed[1]),
            ("hvac_terminals", terminals as f64),
            ("hvac_unserved", (dropped.len() + skipped) as f64),
            ("hvac_duct_m", (metres * 10.0).round() / 10.0),
            ("hvac_units", 1.0),
            ("hvac_furniture_moved", sh.clear.len() as f64),
            ("hvac_fast_ducts", fast as f64),
        ]
    };
    Ok(Some(Design {
        span,
        fixtures,
        clear: sh.clear,
        ring,
        hvac: GHvac {
            plant: vec![plant],
            ducts,
            links,
            names,
            nets: vec![],
            flow: (placed[0], placed[1]),
        },
        metrics,
        count,
        fast,
        placed,
    }))
}

/// Least distance between two drops along a branch of diameter `d`: a tee each, a straight
/// between them.
fn row_pitch(dn: &Dn, d: f64) -> f64 {
    2.0 * tee_leg((d, 0.0)) + dn.drop_bend * d + dn.margin + rule("services.hvac.row_pitch_min")
}

fn duct_area(s: (f64, f64)) -> f64 {
    if s.1 > 0.0 {
        s.0 * s.1
    } else {
        PI * s.0 * s.0 / 4.0
    }
}

/// The velocity cap of a duct by its place: a drop to a terminal, a riser or plant run, a main.
fn cap_of(dn: &Dn, d: &GDuct, n: usize) -> f64 {
    let vertical = d.points.windows(2).all(|w| (w[0].0 - w[1].0).abs() < 1e-6 && (w[0].1 - w[1].1).abs() < 1e-6);
    if d.bend.is_some() {
        dn.v_branch
    } else if vertical || d.storey >= n {
        dn.v_riser
    } else {
        dn.v_main
    }
}

