// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Power and lighting: a board per floor over a cable tray riser (the main board, the source,
//! on the ground floor), a board in each apartment hall; trays along a wall of the
//! circulation rooms and a conduit ring in the ceiling void of every other room, fed across a
//! shared wall from its parent room in a tree from the board. Luminaires by the lumen method
//! hang from rows of conduit off the ring; sockets, switches and appliances take a drop down
//! the wall from it. Circuits per board by use, sized by the app from load and route length.

use std::collections::{HashMap, VecDeque};

use ddc_geom::math::{v3, V3};

use super::rules::rule;
use super::svc::{self, norms, Ring, Room, Space, SvcPort, EPS};
use super::{Boards, CircuitUse, GCircuit, GProject, GRun, GSvcLink, PRole, ProjectSpec, RoomKind, RoomTags, SvcSys, Typology};
use crate::GFixture;

/// Ring of conduit this far inside the walls, m: clear of air terminal necks kept 0.4 m out.
fn d_ring() -> f64 {
    rule("services.elec.d_ring")
}
/// Tray along a wall of a circulation room, axis from the wall face, m.
fn d_spine() -> f64 {
    rule("services.elec.d_spine")
}
/// Conduits and trays this far above the ceiling face, m: never less than a luminaire drop
/// needs for the elbow at its top, of the largest section a luminaire row runs in.
fn over() -> f64 {
    rule("services.elec.over").max(lum_drop_min())
}

/// Least drop from the ring level to a ceiling connector: the elbow radius of the row's
/// section (conduits bend at 6·D), the tee leg for a middle luminaire.
fn lum_drop_min() -> f64 {
    [conduit_s(), rule("services.elec.conduit_l")]
        .into_iter()
        .map(|d| {
            let r = svc::conduit_run(0, d, vec![]);
            svc::bend_max(&r).max(ddc_geom::mep::junction_leg(&r.section))
        })
        .chain(std::iter::once(svc::bend_max(&svc::tray_run(0, tray().0, tray().1, vec![]))))
        .fold(0.0, f64::max)
        + EPS
}
/// Cable tray: width and height, m.
/// The cable level of storey `k`: `z`, else the lowest level above it whose band meets no duct
/// main of the storey and still fits under the slab; `z` when none does.
fn over_ducts(p: &GProject, ducts: &[[f64; 6]], k: usize, z: f64) -> f64 {
    let st = &p.storeys[k];
    let (half, top) = (tray().1 / 2.0 + svc::gap(), st.elevation + st.height - svc::slab());
    // the level only steps over the mains that lie along the void; a riser or a drop is a post
    // the runs go round
    let lies = |d: &[f64; 6]| (d[3] - d[0]).max(d[4] - d[1]) > d[5] - d[2];
    let here: Vec<&[f64; 6]> = ducts.iter().filter(|d| d[2] > st.elevation && d[5] < top + EPS && lies(d)).collect();
    let free = |c: f64| !here.iter().any(|d| d[2] < c + half - EPS && d[5] > c - half + EPS);
    let mut cs: Vec<f64> = here.iter().map(|d| d[5] + half).filter(|c| *c > z && *c + half <= top + EPS).collect();
    cs.sort_by(f64::total_cmp);
    std::iter::once(z).chain(cs).find(|c| free(*c)).unwrap_or(z)
}
fn tray() -> (f64, f64) {
    (rule("services.elec.tray_w"), rule("services.elec.tray_h"))
}
/// Board bottom above the floor: its top at the door head.
fn board_z() -> f64 {
    rule("services.elec.board_z")
}
/// Least distance between two tees on a carrier, m.
fn spacing() -> f64 {
    rule("services.elec.spacing")
}
/// Product data of the library luminaires: lumens, watts and diameter.
fn light_s() -> (&'static str, f64, f64, f64) {
    ("Round 300", rule("services.elec.light_s_lm"), rule("services.elec.light_s_w"), rule("services.elec.light_s_d"))
}
fn light_l() -> (&'static str, f64, f64, f64) {
    ("Round 600", rule("services.elec.light_l_lm"), rule("services.elec.light_l_w"), rule("services.elec.light_l_d"))
}
/// Top of the wall stretch checked for openings: above any storey, so the whole height.
const WALL_TOP: f64 = 100.0;
/// Appliance circuit keys: fixture index plus storey times this, unique while a storey has
/// fewer fixtures.
const KEY_STRIDE: usize = 1_000_000;
/// Mains voltage, V.
fn volts() -> f64 {
    rule("services.elec.voltage")
}
/// Conduit limits the model checks: 16 A under 25 mm, 32 A above, at mains voltage.
fn w20() -> f64 {
    rule("services.elec.conduit_a_small") * volts()
}
fn w32() -> f64 {
    rule("services.elec.conduit_a_large") * volts()
}

fn circulation(k: RoomKind) -> bool {
    matches!(k, RoomKind::Corridor | RoomKind::Hall | RoomKind::Lobby | RoomKind::Stair)
}

/// Rooms that take power: every lit room, and stores and plant rooms for their socket.
fn wired(k: RoomKind) -> bool {
    norms().lux(k, true).is_some() || matches!(k, RoomKind::Storage | RoomKind::Technical)
}

/// Every how many luminaires of a room one is an emergency luminaire on its own circuit: every
/// one in an operating room, every n-th in a ward, a clinical room or a room on backup power.
pub(crate) fn emergency_every(spec: &ProjectSpec, k: RoomKind, name: &str) -> Option<usize> {
    let tags = spec.room_tags.of(k, name);
    let tag = |t: &str| RoomTags::parse(t).is_some_and(|x| tags.meets(x));
    if tag("operating") {
        return Some(rule("services.elec.em_every_critical").max(1.0) as usize);
    }
    let clinical = k == RoomKind::Ward || tag("clinical") || tag("isolation") || on_backup(spec, k, name);
    clinical.then(|| rule("services.elec.em_every").max(1.0) as usize)
}

/// A room the typology puts on backup power: its sockets go on the essential supply.
pub(crate) fn on_backup(spec: &ProjectSpec, k: RoomKind, name: &str) -> bool {
    spec.model.as_ref().and_then(|m| m.of(k, name)).is_some_and(|s| s.demand.backup_power)
}

/// The library's emergency version of a luminaire type: its own battery and test switch.
fn emergency_type(ty: &'static str) -> &'static str {
    match ty {
        "Round 600" => "Round 600 EM",
        _ => "Round 300 EM",
    }
}

fn residential(t: Typology) -> bool {
    matches!(t, Typology::House | Typology::ApartmentSection)
}

/// Small conduit diameter, m: also the connector size of a socket or luminaire.
fn conduit_s() -> f64 {
    rule("services.elec.conduit_s")
}

/// Section for a downstream load: conduit within its limit, else tray.
fn section_for(load: f64) -> (f64, Option<f64>) {
    if load <= w20() {
        (conduit_s(), None)
    } else if load <= w32() {
        (rule("services.elec.conduit_l"), None)
    } else {
        (tray().0, Some(tray().1))
    }
}

fn run_for(k: usize, load: f64, pts: Vec<V3>) -> GRun {
    match section_for(load) {
        (d, None) => svc::conduit_run(k, d, pts),
        (w, Some(h)) => svc::tray_run(k, w, h, pts),
    }
}

/// A wall device: socket, switch or appliance connector on side `j` at `s` of the clear
/// rectangle, its family origin `z` above the level.
#[derive(Clone, Debug)]
struct Dev {
    fam: &'static str,
    ty: &'static str,
    j: usize,
    s: f64,
    z: f64,
    load: f64,
    use_: CircuitUse,
    /// An appliance placed before: its fixture index.
    existing: Option<usize>,
    /// Its connector this far into the room from the wall face, m; a new device sits on the face.
    off: f64,
}

/// A row of luminaires square to side `j` at `s`, at `ds` into the room.
#[derive(Clone, Debug)]
struct Col {
    j: usize,
    s: f64,
    ds: Vec<f64>,
    ty: (&'static str, f64, f64, f64),
}

#[derive(Clone, Debug)]
struct Plan {
    room: Room,
    devs: Vec<Dev>,
    cols: Vec<Col>,
    /// Lux reached against the target, for the metric.
    lux: (f64, f64),
}

impl Plan {
    fn load(&self) -> f64 {
        self.devs.iter().map(|d| d.load).sum::<f64>() + self.cols.iter().map(|c| c.ds.len() as f64 * c.ty.2).sum::<f64>()
    }
}

/// Room dimensions along and across side `j`.
fn dims(room: &Room, j: usize) -> (f64, f64) {
    let (w, d) = (room.clear[2] - room.clear[0], room.clear[3] - room.clear[1]);
    if j % 2 == 0 {
        (w, d)
    } else {
        (d, w)
    }
}

/// Footprints of the floor and wall fixtures on a storey, without ceiling terminals.
fn footprints(fx: &[GFixture]) -> Vec<svc::R> {
    fx.iter()
        .filter(|f| !matches!(f.family, "Supply Diffuser" | "Exhaust Grille" | "Ceiling Light"))
        .filter_map(svc::footprint)
        .collect()
}

/// Whether a wall device fits at `s` on side `j` at height `z`: on a plain stretch of wall
/// whose drop from the ceiling meets no opening, clear of the furniture in front, away from
/// the corners.
fn wall_ok(room: &Room, j: usize, s: f64, z: f64, top: f64, fps: &[svc::R], ignore_fp: bool) -> bool {
    let sd = &room.sides[j];
    let corner = rule("services.elec.wall_corner");
    if sd.half <= 0.0 || s < d_ring() + corner || s > sd.len - d_ring() - corner {
        return false;
    }
    let (half, depth) = (rule("services.elec.wall_dev_half"), rule("services.elec.wall_dev_depth"));
    let (chalf, below, above) = (rule("services.elec.wall_clear_half"), rule("services.elec.wall_clear_below"), rule("services.elec.wall_clear_above"));
    if !sd.free(s - half, s + half, depth, z, top) || !sd.free(s - chalf, s + chalf, 0.0, z - below, z + above) {
        return false;
    }
    if ignore_fp {
        return true;
    }
    let p = sd.at(s, depth);
    let bx = rule("services.elec.wall_dev_box");
    let b = [p.0 - bx, p.1 - bx, p.0 + bx, p.1 + bx];
    !fps.iter().any(|f| svc::hit(f, &b, 0.0))
}

/// Whether a tee at `s` on side `j` comes nearer than the spacing to a taken one: on the same
/// side, or in a spine room across it, where the drops to the far side span the room.
fn crowded(room: &Room, spine: Option<usize>, taken: &[(usize, f64)], j: usize, s: f64) -> bool {
    let along = |jj: usize, ss: f64| room.sides[j].s_of(room.sides[jj].at(ss, 0.0));
    let least = spacing() + rule("services.elec.spacing_slack");
    taken.iter().any(|&(jj, ss)| {
        let parallel = jj == j || (spine.is_some() && jj == (j + 2) % 4);
        parallel && (along(jj, ss) - s).abs() < least
    })
}

/// Devices and luminaires a room needs.
#[allow(clippy::too_many_arguments)]
fn plan(spec: &ProjectSpec, p: &GProject, k: usize, room: &Room, f_rel: f64, fps: &[svc::R], spine: Option<usize>, reserved: &[(usize, f64)]) -> Plan {
    let n = norms();
    let st = &p.storeys[k];
    let res = residential(spec.typology) && room.owner != PRole::Public || spec.typology == Typology::House;
    let mut devs: Vec<Dev> = vec![];
    let mut cols: Vec<Col> = vec![];
    let occupied = |devs: &[Dev], cols: &[Col], j: usize, s: f64| {
        let marks: Vec<(usize, f64)> = devs.iter().map(|d| (d.j, d.s)).chain(cols.iter().map(|c| (c.j, c.s))).chain(reserved.iter().copied()).collect();
        crowded(room, spine, &marks, j, s)
    };
    // sides a device may use: every side of a ring room, the far side of a spine room
    let usable: Vec<usize> = match spine {
        Some(j) => vec![(j + 2) % 4],
        None => (0..4).collect(),
    };
    use RoomKind::*;
    let general = matches!(
        room.kind,
        Living | Bedroom | Kitchen | Office | Meeting | Classroom | Lab | Staff | Ward | Dining | Retail | Workshop | Gym | Lobby
    );
    let one = matches!(room.kind, Hall | Storage | Technical);
    let double = matches!(room.kind, Office | Meeting | Classroom | Lab | Kitchen | Workshop);
    let (w_double, w_single) = (rule("services.elec.sock_double_w"), rule("services.elec.sock_single_w"));
    let drop = rule("services.elec.plate_drop");
    let sock = if double { ("Double", w_double) } else { ("Single", w_single) };
    let zs = n.socket_h() - drop;
    // appliances already in the room: their connector is on the wall face behind them
    for (i, f) in st.layout.fixtures.iter().enumerate() {
        if !n.dedicated(f.family) {
            continue;
        }
        for c in svc::svc_ports(f).into_iter().filter(|c| c.sys == SvcSys::Power) {
            let q = (c.at.0, c.at.1);
            // the nearest usable wall within the reach water connectors are served from; a
            // connector behind the wall face is the next room's
            let reach = rule("services.water.port_reach");
            let wall = usable
                .iter()
                .copied()
                .filter(|&j| {
                    let sd = &room.sides[j];
                    sd.d_of(q) > -EPS && sd.d_of(q) < reach && sd.s_of(q) > 0.0 && sd.s_of(q) < sd.len
                })
                .min_by(|&x, &y| room.sides[x].d_of(q).total_cmp(&room.sides[y].d_of(q)));
            if let Some(j) = wall {
                let s = room.sides[j].s_of(q);
                devs.push(Dev {
                    fam: f.family,
                    ty: f.ty,
                    j,
                    s,
                    z: c.at.2 - rule("services.elec.appliance_drop"),
                    load: c.flow,
                    use_: CircuitUse::Appliance,
                    existing: Some(i),
                    off: room.sides[j].d_of(q),
                });
            }
        }
    }
    // general sockets: every stretch of plain wall, none farther than the reach from one
    let top = f_rel;
    if general || one {
        let reach = 2.0 * n.wall_reach();
        // offsets tried along the wall when the even spot is taken
        let (shift_1, shift_3) = (rule("services.elec.socket_shift"), rule("services.elec.socket_shift_far"));
        'sides: for &j in &usable {
            let sd = &room.sides[j];
            if sd.half <= 0.0 {
                continue;
            }
            // stretches between the doors
            let mut cuts: Vec<(f64, f64)> = sd.gaps.iter().filter(|g| g.door).map(|g| (g.s0, g.s1)).collect();
            cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut a = 0.0;
            let mut stretches = vec![];
            let stretch_min = rule("services.elec.stretch_min");
            for (c0, c1) in cuts.iter().copied().chain([(sd.len, sd.len)]) {
                if c0 - a > stretch_min {
                    stretches.push((a, c0));
                }
                a = a.max(c1);
            }
            for (a, b) in stretches {
                let cnt = ((b - a) / reach).ceil().max(1.0) as usize;
                for i in 0..cnt {
                    let s0 = a + (b - a) * (i as f64 + 0.5) / cnt as f64;
                    let ok = [0.0, shift_1, -shift_1, 2.0 * shift_1, -2.0 * shift_1, shift_3, -shift_3]
                        .iter()
                        .map(|d| s0 + d)
                        .find(|&s| wall_ok(room, j, s, zs, top, fps, false) && !occupied(&devs, &cols, j, s));
                    if let Some(s) = ok {
                        devs.push(Dev { fam: "Socket Outlet", ty: sock.0, j, s, z: zs, load: sock.1, use_: CircuitUse::Sockets, existing: None, off: 0.0 });
                        if one {
                            break 'sides;
                        }
                    }
                }
            }
        }
    }
    // worktop sockets over each counter run, clear of the sink
    if room.kind == Kitchen {
        let zw = n.worktop_h() - drop;
        let (touch, sink_along, sink_across) = (rule("services.elec.wall_touch"), rule("services.elec.sink_along"), rule("services.elec.sink_across"));
        let sinks: Vec<(f64, f64)> = st.layout.fixtures.iter().filter(|f| f.family == "Sink").map(|f| f.at).collect();
        let step = 2.0 * n.worktop_reach();
        for f in st.layout.fixtures.iter().filter(|f| f.family == "Kitchen Counter") {
            let Some(fp) = svc::footprint(f) else { continue };
            if !svc::hit(&fp, &room.clear, -rule("services.elec.counter_overlap")) {
                continue;
            }
            for &j in &usable {
                let sd = &room.sides[j];
                let (p0, p1) = (sd.s_of((fp[0], fp[1])), sd.s_of((fp[2], fp[3])));
                let (a, b) = (p0.min(p1), p0.max(p1));
                let dmin = sd.d_of((fp[0], fp[1])).min(sd.d_of((fp[2], fp[3])));
                if dmin.abs() > touch {
                    continue;
                }
                let cnt = ((b - a) / step).ceil().max(1.0) as usize;
                for i in 0..cnt {
                    let s = a + (b - a) * (i as f64 + 0.5) / cnt as f64;
                    let near_sink = sinks.iter().any(|q| (sd.s_of(*q) - s).abs() < sink_along && sd.d_of(*q) < sink_across);
                    if !near_sink && wall_ok(room, j, s, zw, top, fps, true) && !occupied(&devs, &cols, j, s) {
                        devs.push(Dev { fam: "Socket Outlet", ty: "Double", j, s, z: zw, load: w_double, use_: CircuitUse::Worktop, existing: None, off: 0.0 });
                    }
                }
            }
        }
    }
    // a socket beside the basin of a bath room, outside zone 2 of the bath and shower
    if room.kind == Bath {
        let zw = n.worktop_h() - drop;
        let touch = rule("services.elec.wall_touch");
        let (basin_near, basin_far) = (rule("services.elec.basin_near"), rule("services.elec.basin_far"));
        let wet: Vec<svc::R> = st
            .layout
            .fixtures
            .iter()
            .filter(|f| matches!(f.family, "Bathtub" | "Shower Tray"))
            .filter_map(svc::footprint)
            .collect();
        'basin: for f in st.layout.fixtures.iter().filter(|f| f.family == "Washbasin" && svc::inside(f.at, &svc::grow(&room.clear, touch))) {
            for &j in &usable {
                let sd = &room.sides[j];
                if sd.d_of(f.at).abs() > touch {
                    continue;
                }
                let sb = sd.s_of(f.at);
                for ds in [basin_near, -basin_near, basin_far, -basin_far] {
                    let s = sb + ds;
                    let q = sd.at(s, 0.0);
                    let zone2 = wet.iter().any(|w| svc::hit(&svc::grow(w, n.zone2()), &[q.0, q.1, q.0, q.1], 0.0));
                    if !zone2 && ds.abs() <= n.basin_reach() && wall_ok(room, j, s, zw, top, fps, false) && !occupied(&devs, &cols, j, s) {
                        devs.push(Dev { fam: "Socket Outlet", ty: "Single", j, s, z: zw, load: w_single, use_: CircuitUse::Wet, existing: None, off: 0.0 });
                        break 'basin;
                    }
                }
            }
        }
    }
    // luminaires by the lumen method over the clear area
    let target = n.lux(room.kind, res);
    let mut lux = (0.0, target.unwrap_or(0.0));
    if let Some(e) = target {
        let area = room.area();
        let ty = if area >= rule("services.elec.big_room_area") { light_l() } else { light_s() };
        let (j, (len, depth)) = match spine {
            Some(j) => (j, dims(room, j)),
            None => {
                let j = if room.clear[2] - room.clear[0] >= room.clear[3] - room.clear[1] { 0 } else { 1 };
                (j, dims(room, j))
            }
        };
        let hm = (f_rel - rule("services.elec.work_plane")).max(1.0);
        let ki = len * depth / (hm * (len + depth));
        let count = (e * area / (n.uf(ki) * n.mf() * ty.1)).ceil().max(1.0) as usize;
        // half the face side of each air terminal of the ventilation catalogue
        let neck = [
            rule("services.hvac.supply_l_face") / 2.0,
            rule("services.hvac.extract_l_face") / 2.0,
            rule("services.hvac.supply_s_face") / 2.0,
            rule("services.hvac.extract_s_face") / 2.0,
        ];
        let terminals: Vec<svc::R> = st
            .layout
            .fixtures
            .iter()
            .filter(|f| matches!(f.family, "Supply Diffuser" | "Exhaust Grille"))
            .filter_map(|f| {
                let h = if f.ty.starts_with("600") { neck[0] } else if f.ty.starts_with("400") { neck[1] } else if f.ty.starts_with("300") { neck[2] } else { neck[3] };
                Some([f.at.0 - h, f.at.1 - h, f.at.0 + h, f.at.1 + h])
            })
            .collect();
        let lum_clear = rule("services.elec.lum_clear");
        let free = |q: (f64, f64)| {
            let r = ty.3 / 2.0 + lum_clear;
            !terminals.iter().any(|t| svc::hit(t, &[q.0 - r, q.1 - r, q.0 + r, q.1 + r], 0.0))
        };
        let sd = room.sides[j].clone();
        let (na, ni) = match spine {
            Some(_) => (count, 1),
            None => {
                let na = ((count as f64 * len / depth).sqrt().round() as usize).clamp(1, count);
                (na, count.div_ceil(na))
            }
        };
        let (rm_d, rm_s) = (rule("services.elec.row_margin_d"), rule("services.elec.row_margin_s"));
        let (dlo, dhi) = (d_ring() + rm_d, depth - d_ring() - rm_d);
        let (slo, shi) = (d_ring() + rm_s, len - d_ring() - rm_s);
        let mut placed = 0usize;
        let lum_shift = rule("services.elec.lum_shift");
        for a in 0..na {
            let s0 = (len * (a as f64 + 0.5) / na as f64).clamp(slo.min(len / 2.0), shi.max(len / 2.0));
            let mut best: Option<Col> = None;
            for shift in [0.0, lum_shift, -lum_shift, 2.0 * lum_shift, -2.0 * lum_shift] {
                let s = s0 + shift;
                if s < slo.min(len / 2.0) - EPS || s > shi.max(len / 2.0) + EPS || occupied(&devs, &cols, j, s) {
                    continue;
                }
                let ds: Vec<f64> = match spine {
                    Some(_) => vec![depth / 2.0],
                    None => (0..ni)
                        .map(|r| (depth * (r as f64 + 0.5) / ni as f64).clamp(dlo.min(depth / 2.0), dhi.max(depth / 2.0)))
                        .collect(),
                };
                let ds: Vec<f64> = ds.into_iter().filter(|&d| free(sd.at(s, d))).collect();
                if best.as_ref().is_none_or(|b| ds.len() > b.ds.len()) {
                    best = Some(Col { j, s, ds, ty });
                }
                if best.as_ref().is_some_and(|b| b.ds.len() == ni) {
                    break;
                }
            }
            if let Some(c) = best.filter(|c| !c.ds.is_empty()) {
                placed += c.ds.len();
                cols.push(c);
            }
        }
        let ki_uf = n.uf(ki) * n.mf();
        lux = (placed as f64 * ty.1 * ki_uf / area.max(1e-6), e);
    }
    // one switch beside the first door with wall to spare
    if !cols.is_empty() {
        let zsw = n.switch_h() - drop;
        let beside = rule("services.elec.switch_off");
        'door: for &j in &usable {
            let sd = &room.sides[j];
            for g in sd.gaps.iter().filter(|g| g.door) {
                for s in [g.s1 + beside, g.s0 - beside] {
                    if wall_ok(room, j, s, zsw, top, fps, false) && !occupied(&devs, &cols, j, s) {
                        devs.push(Dev { fam: "Light Switch", ty: "One gang", j, s, z: zsw, load: 0.0, use_: CircuitUse::Lighting, existing: None, off: 0.0 });
                        break 'door;
                    }
                }
            }
        }
    }
    Plan { room: room.clone(), devs, cols, lux }
}

/// A board: storey, room, side, along, type.
#[derive(Clone, Copy, Debug)]
struct Board {
    room: usize,
    j: usize,
    s: f64,
}

/// Where the floor boards go: the same wall stretch of a circulation room on every storey,
/// the riser beside them. Returns every such stretch in order of preference, per storey the
/// room index (in `svc::rooms`) and side.
fn floor_boards(p: &GProject, rooms: &[Vec<Room>], fps: &[Vec<svc::R>]) -> Vec<Vec<Board>> {
    let mut found = vec![];
    let r0 = &rooms[0];
    let cand = |r: &Room| circulation(r.kind) && r.owner == PRole::Public;
    let (before, after, end) = (rule("services.elec.board_before"), rule("services.elec.board_after"), rule("services.elec.board_end"));
    let (box_before, box_after, box_depth) = (rule("services.elec.board_box_before"), rule("services.elec.board_box_after"), rule("services.elec.board_box_depth"));
    let free_top = WALL_TOP;
    let board_step = rule("services.elec.board_step");
    let fits_side = |room: &Room, j: usize, s: f64, fp: &[svc::R]| {
        let sd = &room.sides[j];
        if sd.half <= 0.0 || s < before || s + after > sd.len - end {
            return false;
        }
        if !sd.free(s - before, s + after, 0.0, 0.0, free_top) {
            return false;
        }
        let b = svc::bbox(&[sd.at(s - box_before, 0.0), sd.at(s + box_after, box_depth)]);
        !fp.iter().any(|f| svc::hit(f, &b, 0.0))
    };
    let mut order: Vec<&Room> = r0.iter().filter(|r| cand(r)).collect();
    order.sort_by_key(|r| (r.kind != RoomKind::Corridor && r.kind != RoomKind::Lobby, r.kind == RoomKind::Stair));
    for room in order {
        for j in 0..4 {
            let sd = &room.sides[j];
            let mut s = rule("services.elec.board_first");
            while s + after < sd.len {
                if fits_side(room, j, s, &fps[0]) {
                    let w = sd.at(s, 0.0);
                    let mut boards = vec![Board { room: room.idx, j, s }];
                    for k in 1..p.storeys.len() {
                        let hit = rooms[k].iter().filter(|r| cand(r)).find_map(|r| {
                            (0..4).find_map(|jj| {
                                let o = &r.sides[jj];
                                let same = o.horiz == sd.horiz && (o.line - sd.line).abs() < EPS && o.t == sd.t && o.d_of(w).abs() < 1e-6;
                                let ss = o.s_of(w);
                                (same && fits_side(r, jj, ss, &fps[k])).then_some(Board { room: r.idx, j: jj, s: ss })
                            })
                        });
                        match hit {
                            Some(b) => boards.push(b),
                            None => break,
                        }
                    }
                    if boards.len() == p.storeys.len() {
                        found.push(boards);
                    }
                }
                s += board_step;
            }
        }
    }
    found
}

/// A tee on a carrier: ring length or spine `s`, the downstream load, what hangs there.
#[derive(Clone, Debug)]
struct Tee {
    at: f64,
    load: f64,
    kind: TeeKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TeeKind {
    Dev(usize),
    Col(usize),
    /// Where a child room's feed leaves.
    Child,
    /// Where this room's own feed arrives.
    Feed,
    Board,
    Riser,
}

/// The tree edge into a room: parent room, the world point on the shared wall, and the side
/// of the child and of the parent.
#[derive(Clone, Copy, Debug)]
struct Feed {
    parent: usize,
    pj: usize,
    ps: f64,
    cj: usize,
    cs: f64,
}

struct Built {
    runs: Vec<GRun>,
    ports: Vec<(usize, SvcPort, Option<usize>)>,
}

/// Runs of a room's carrier and of everything on it, with the connectors they reach. `dev`
/// and `col` entries removed earlier are `None`.
#[allow(clippy::too_many_arguments)]
fn build_room(
    k: usize,
    pl: &Plan,
    spine: Option<usize>,
    tees: &[Tee],
    feed_from: Option<V3>,
    z: f64,
    zf: f64,
    e: f64,
    dr: f64,
    open: Option<f64>,
) -> Built {
    let room = &pl.room;
    let mut runs = vec![];
    let mut ports = vec![];
    let total: f64 = tees.iter().map(|t| t.load).sum();
    let carrier = |at: f64| carrier_xy(room, spine, at, dr);
    // the upstream point: the feed (the first tee), else the riser or the board
    let p_at = tees.iter().find(|t| matches!(t.kind, TeeKind::Riser)).or_else(|| tees.iter().find(|t| matches!(t.kind, TeeKind::Board))).map(|t| t.at);
    let origin = match (feed_from, p_at) {
        (Some(_), _) => tees.first().map_or(0.0, |t| t.at),
        (None, Some(a)) => a,
        (None, None) => 0.0,
    };
    // the feed into the room
    if let Some(from) = feed_from {
        let to = carrier(origin);
        runs.push(run_for(k, total, vec![from, v3(to.0, to.1, z)]));
    }
    // two arms from the origin: tees on each side by distance
    let per = match spine {
        Some(_) => f64::INFINITY,
        None => Ring::new(room, dr).per(),
    };
    let mut arms: [Vec<(f64, &Tee)>; 2] = [vec![], vec![]];
    for t in tees.iter().skip(usize::from(feed_from.is_some())) {
        if matches!(t.kind, TeeKind::Riser | TeeKind::Board) && feed_from.is_none() && (t.at - origin).abs() < 1e-9 {
            continue;
        }
        let (d, side) = if spine.is_some() {
            ((t.at - origin).abs(), usize::from(t.at < origin))
        } else {
            let df = (t.at - origin).rem_euclid(per);
            if df <= open.unwrap_or(per / 2.0) {
                (df, 0)
            } else {
                (per - df, 1)
            }
        };
        arms[side].push((d, t));
    }
    let ring = Ring::new(room, dr);
    let pos = |d: f64, side: usize| if side == 0 { origin + d } else { origin - d };
    let path = |a: f64, b: f64| -> Vec<V3> {
        let pts = match spine {
            Some(j) => vec![room.sides[j].at(a, dr), room.sides[j].at(b, dr)],
            None => ring.path(a, b),
        };
        pts.into_iter().map(|q| v3(q.0, q.1, z)).collect()
    };
    // the last tee of an arm (a device drop or a luminaire row) moved along the arm so the
    // horizontal from it holds its elbows; the tee before it moved back too when it must
    let mut moved: Vec<(TeeKind, f64)> = vec![];
    for (side, arm) in arms.iter_mut().enumerate() {
        // an arm ends at its last tee, turning into what hangs there: no end left in the air
        if arm.is_empty() {
            continue;
        }
        arm.sort_by(|a, b| a.0.total_cmp(&b.0));
        let at = |d: f64| carrier(pos(d, side));
        let n = arm.len();
        let Some(end) = hang(room, pl, spine, arm[n - 1].1.kind, k) else { continue };
        let prev = (n >= 2).then(|| (arm[n - 2].0, arm[n - 2].1.kind));
        let d_pp = if n >= 3 { arm[n - 3].0 } else { 0.0 };
        let floor = prev.map_or(0.0, |p| p.0);
        if let Some(d_new) = arm_end(&at, arm[n - 1].0, floor, end) {
            if d_new != arm[n - 1].0 {
                moved.push((arm[n - 1].1.kind, pos(d_new, side)));
                arm[n - 1].0 = d_new;
            }
        } else if let Some((d_new, d_prev_new)) = prev.and_then(|(_, pk)| {
            // the tee before stands back, holding its own tee leg and drop
            let pe = hang(room, pl, spine, pk, k)?;
            let d_new = arm_end(&at, arm[n - 1].0, d_pp, end)?;
            let (_, near) = trims(&at, d_new, end);
            let d_prev_new = d_new - near;
            let ok = d_prev_new > d_pp + 2.0 * end.leg_prev && tee_holds(&at, d_prev_new, pe);
            ok.then_some((d_new, d_prev_new))
        }) {
            moved.push((arm[n - 1].1.kind, pos(d_new, side)));
            moved.push((arm[n - 2].1.kind, pos(d_prev_new, side)));
            arm[n - 1].0 = d_new;
            arm[n - 2].0 = d_prev_new;
        }
    }
    for (side, arm) in arms.iter_mut().enumerate() {
        if arm.is_empty() {
            continue;
        }
        let end = arm.last().map_or(0.0, |a| a.0);
        // stretches between tees carry the load beyond them; a section holds until it changes
        let sec = |load: f64| if spine.is_some() { (tray().0, Some(tray().1)) } else { section_for(load) };
        let mut pieces: Vec<(f64, f64, f64)> = vec![];
        let mut beyond: f64 = arm.iter().map(|a| a.1.load).sum();
        let mut from = 0.0;
        for (d, t) in arm.iter().map(|(d, t)| (*d, Some(*t))).chain([(end, None)]) {
            if d - from > EPS {
                match pieces.last_mut() {
                    Some(pc) if sec(pc.2) == sec(beyond) => pc.1 = d,
                    _ => pieces.push((from, d, beyond)),
                }
            }
            from = from.max(d);
            beyond -= t.map_or(0.0, |t| t.load);
        }
        for pc in &pieces {
            let pts = path(pos(pc.0, side), pos(pc.1, side));
            runs.push(match sec(pc.2) {
                (d, None) => svc::conduit_run(k, d, pts),
                (w, Some(h)) => svc::tray_run(k, w, h, pts),
            });
        }
    }
    // what hangs on the tees
    for t in tees {
        let at = moved.iter().find(|m| m.0 == t.kind).map_or(t.at, |m| m.1);
        let a = carrier(at);
        match t.kind {
            TeeKind::Dev(i) => {
                let d = &pl.devs[i];
                let sd = &room.sides[d.j];
                let w = sd.at(d.s, d.off);
                let pz = e + d.z + if d.existing.is_some() { rule("services.elec.appliance_drop") } else { rule("services.elec.plate_drop") };
                let pts = vec![v3(a.0, a.1, z), v3(w.0, w.1, z), v3(w.0, w.1, pz)];
                runs.push(run_for(k, d.load, pts));
                ports.push((
                    i,
                    SvcPort { name: "Power", sys: SvcSys::Power, at: (w.0, w.1, pz), dir: (0.0, 0.0, 0.0), size: conduit_s(), flow: d.load, source: false },
                    None,
                ));
            }
            TeeKind::Col(i) => {
                let c = &pl.cols[i];
                let sd = &room.sides[c.j];
                let mut pts = vec![v3(a.0, a.1, z)];
                for &d in &c.ds {
                    let q = sd.at(c.s, d);
                    pts.push(v3(q.0, q.1, z));
                }
                let lastq = *pts.last().unwrap();
                pts.push(v3(lastq.x, lastq.y, zf));
                let load = c.ds.len() as f64 * c.ty.2;
                let n_ds = c.ds.len();
                // middle luminaires tee down off the row
                for (r, &d) in c.ds.iter().enumerate() {
                    let q = sd.at(c.s, d);
                    if r + 1 < n_ds {
                        runs.push(svc::conduit_run(k, conduit_s(), vec![v3(q.0, q.1, z), v3(q.0, q.1, zf)]));
                    }
                    ports.push((
                        i,
                        SvcPort { name: "Power", sys: SvcSys::Power, at: (q.0, q.1, zf), dir: (0.0, 0.0, 1.0), size: conduit_s(), flow: c.ty.2, source: false },
                        Some(r),
                    ));
                }
                runs.push(run_for(k, load, pts));
            }
            _ => {}
        }
    }
    Built { runs, ports }
}

/// Designs power and lighting into the project. Returns the rooms left without power.
pub(crate) fn wire(spec: &ProjectSpec, p: &mut GProject) -> usize {
    let n_st = p.storeys.len();
    let rooms: Vec<Vec<Room>> = p.storeys.iter().map(svc::rooms).collect();
    let fps: Vec<Vec<svc::R>> = p.storeys.iter().map(|s| footprints(&s.layout.fixtures)).collect();
    let stacks = floor_boards(p, &rooms, &fps);
    let ducts: Vec<[f64; 6]> = svc::obstacles(p).into_iter().filter(|o| matches!(o.1, "duct" | "air terminal neck")).map(|o| o.0).collect();
    let zc: Vec<f64> = (0..n_st)
        .map(|k| {
            // layers on: the tray stands in its own band, whatever the ceiling does
            // depth capped at void - min drop: the ring keeps room for the luminaire drops
            match svc::layer(p.storeys[k].height, "tray") {
                Some(z) => p.storeys[k].elevation + z.max(svc::ceiling(p, k) + lum_drop_min()),
                None => over_ducts(p, &ducts, k, p.storeys[k].elevation + svc::ceiling(p, k) + over()),
            }
        })
        .collect();
    let mut space = Space::new(p);
    let mut runs: Vec<GRun> = vec![];
    // devices, rows and rooms the retries leave out, each told in the services check
    let mut dropped: Vec<String> = vec![];
    // fixtures to add per storey, links by (storey, fixture index), circuits per board
    let mut add: Vec<Vec<GFixture>> = vec![vec![]; n_st];
    let mut links: Vec<GSvcLink> = vec![];
    let mut unserved = 0usize;
    let mut lux_short = 0usize;
    // board fixture indices: (storey, index)
    let mut board_fx: Vec<(usize, usize)> = vec![];
    // per storey, per room: board key and devices (storey, fixture, connector, use, load, room)
    let mut served: Vec<(usize, usize, CircuitUse, f64, usize, usize, &'static str)> = vec![];
    let mut unit_boards: HashMap<(usize, u32), (usize, usize)> = HashMap::new();
    let n_fx: Vec<usize> = p.storeys.iter().map(|s| s.layout.fixtures.len()).collect();
    // the riser stands this far along the spine from the board, m; never closer than the tee the
    // riser makes on an upper floor and the conduit bending down into the board
    let riser_off = {
        let (t, d) = (svc::tray_run(0, tray().0, tray().1, vec![]), svc::conduit_run(0, rule("services.elec.board_drop"), vec![]));
        let need = ddc_geom::mep::junction_leg(&t.section) + svc::bend_max(&d) + ddc_geom::mep::transition_length(&d.section, &t.section);
        rule("services.elec.riser_off").max(need)
    };
    // the riser first: it must fit through every storey; the first board stretch it fits beside
    let riser_at = |floor: &[Board]| {
        let b = floor[0];
        let r = rooms[0].iter().find(|r| r.idx == b.room).unwrap();
        r.sides[b.j].at(b.s + riser_off, d_spine())
    };
    // the building's cable shaft, when it has one the tray fits: the riser stands in it and each
    // floor's spine reaches it by a stub at the floor's tray level
    let core_xy: Option<(f64, f64)> = super::hvac::core_bays(p, crate::ShaftKind::Electrical)
        .into_iter()
        .find(|r| (r.2 - r.0).min(r.3 - r.1) >= tray().0 + rule("services.elec.tray_clear"))
        .map(|r| ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0));
    let spine_at = |floor: &[Board], k: usize| {
        let b = floor[k];
        let r = rooms[k].iter().find(|r| r.idx == b.room).unwrap();
        r.sides[b.j].at(b.s + riser_off, d_spine())
    };
    let mut chosen: Option<(Vec<Board>, (f64, f64))> = None;
    for floor in stacks {
        let riser_xy = core_xy.unwrap_or_else(|| riser_at(&floor));
        if n_st > 1 {
            let mut riser: Vec<GRun> = (0..n_st - 1)
                .map(|k| svc::tray_run(k, tray().0, tray().1, vec![v3(riser_xy.0, riser_xy.1, zc[k]), v3(riser_xy.0, riser_xy.1, zc[k + 1])]))
                .collect();
            if core_xy.is_some() {
                for k in 0..n_st {
                    let q = spine_at(&floor, k);
                    riser.push(svc::tray_run(k, tray().0, tray().1, vec![v3(q.0, q.1, zc[k]), v3(riser_xy.0, riser_xy.1, zc[k])]));
                }
            }
            if !space.try_add(&riser, svc::gap()) {
                continue;
            }
            runs.extend(riser);
        }
        chosen = Some((floor, riser_xy));
        break;
    }
    let Some((floor, riser_xy)) = chosen else {
        drop(space);
        let all: usize = rooms.iter().map(|r| r.len()).sum();
        p.metrics.push(("power_unserved_rooms", all as f64));
        return all;
    };
    // a storey whose child room fails after its parent was built is built again: the failed feed
    // spots are barred, past `retries` the failed rooms are left out, so no arm ends in the air
    let retries = rule("services.elec.floor_retries") as usize;
    let mut k = 0;
    // barred feeds of this storey (child room, parent side, spot in mm), rooms left out, tries
    let mut barred: Vec<(usize, usize, i64)> = vec![];
    let mut cut: Vec<usize> = vec![];
    let mut tries_k = 0;
    while k < n_st {
        let undo = (space.mark(), runs.len(), dropped.len(), links.len(), served.len(), board_fx.len(), unserved, lux_short);
        let mut failed: Vec<(usize, Feed)> = vec![];
        let st = &p.storeys[k];
        let e = st.elevation;
        let f_rel = svc::ceiling(p, k);
        let z = zc[k];
        let zf = e + f_rel;
        let rs = &rooms[k];
        let fb = floor[k];
        let board_room = rs.iter().position(|r| r.idx == fb.room).unwrap();
        // spines: the board side for the board room; others decided by their feed
        let mut spine: Vec<Option<usize>> = vec![None; rs.len()];
        spine[board_room] = Some(fb.j);
        // ring offset per room: a ring that meets a shaft, a duct or an air terminal steps off
        let mut co: Vec<f64> = vec![d_ring(); rs.len()];
        co[board_room] = d_spine();
        let (co_step, co_tries) = (rule("services.elec.ring_shift"), rule("services.elec.ring_tries") as usize);
        // tree by breadth from the board room; a feed square across a shared wall
        let mut feed: Vec<Option<Feed>> = vec![None; rs.len()];
        let mut seen = vec![false; rs.len()];
        seen[board_room] = true;
        let mut q = VecDeque::from([board_room]);
        let mut order = vec![];
        let apt_has_hall = |a: u32| rs.iter().any(|r| r.owner == PRole::Apartment(a) && r.kind == RoomKind::Hall);
        // plans need the spine side: circulation children take theirs from the shared wall
        let mut plans: Vec<Option<Plan>> = vec![None; rs.len()];
        plans[board_room] = Some(plan(spec, p, k, &rs[board_room], f_rel, &fps[k], spine[board_room], &[(fb.j, fb.s), (fb.j, fb.s + riser_off)]));
        let mut taken: Vec<Vec<(usize, f64)>> = vec![vec![]; rs.len()];
        let note = |pl: &Plan, taken: &mut Vec<(usize, f64)>| {
            for d in &pl.devs {
                taken.push((d.j, d.s));
            }
            for c in &pl.cols {
                taken.push((c.j, c.s));
            }
        };
        if let Some(pl) = &plans[board_room] {
            note(pl, &mut taken[board_room]);
        }
        taken[board_room].push((fb.j, fb.s));
        taken[board_room].push((fb.j, fb.s + riser_off));
        // a tee on a spine this far from its ends; candidate feed spots step this far from the middle
        let (spine_margin, feed_step) = (rule("services.elec.spine_margin"), rule("services.elec.feed_step"));
        while let Some(u) = q.pop_front() {
            order.push(u);
            let ru = &rs[u];
            for v in 0..rs.len() {
                if seen[v] || cut.contains(&v) {
                    continue;
                }
                let rv = &rs[v];
                if !wired(rv.kind) {
                    continue;
                }
                let owner_ok = ru.owner == rv.owner
                    || (ru.owner == PRole::Public
                        && match rv.owner {
                            PRole::Apartment(a) => rv.kind == RoomKind::Hall || !apt_has_hall(a),
                            PRole::Public => true,
                        });
                if !owner_ok {
                    continue;
                }
                // a shared wall: parallel sides on one line facing each other
                let mut found = None;
                'pair: for pj in 0..4 {
                    let a = &ru.sides[pj];
                    if let Some(sj) = spine[u] {
                        if pj != sj && pj != (sj + 2) % 4 {
                            continue;
                        }
                    }
                    for cj in 0..4 {
                        let b = &rv.sides[cj];
                        if a.horiz != b.horiz || (a.line - b.line).abs() > EPS || a.t.0 * b.t.0 + a.t.1 * b.t.1 > -0.5 {
                            continue;
                        }
                        let child_spine = circulation(rv.kind);
                        let (alo, ahi) = if spine[u].is_some() { (spine_margin, a.len - spine_margin) } else { (d_ring() + spacing(), a.len - d_ring() - spacing()) };
                        let (blo, bhi) = if child_spine { (spine_margin, b.len - spine_margin) } else { (d_ring() + spacing(), b.len - d_ring() - spacing()) };
                        // candidates from the middle of the overlap outward
                        let w0 = a.at(alo, 0.0);
                        let w1 = a.at(ahi, 0.0);
                        let (bs0, bs1) = (b.s_of(w0), b.s_of(w1));
                        let lo = bs0.min(bs1).max(blo);
                        let hi = bs0.max(bs1).min(bhi);
                        if hi < lo {
                            continue;
                        }
                        let mid = (lo + hi) / 2.0;
                        let mut tries = vec![mid];
                        let mut dd = feed_step;
                        while mid - dd >= lo || mid + dd <= hi {
                            if mid + dd <= hi {
                                tries.push(mid + dd);
                            }
                            if mid - dd >= lo {
                                tries.push(mid - dd);
                            }
                            dd += feed_step;
                        }
                        for cs in tries {
                            let w = b.at(cs, 0.0);
                            let ps_side = a.s_of(w);
                            // on a spine the tee is on the spine side
                            let ps = match spine[u] {
                                Some(sj) => ru.sides[sj].s_of(w),
                                None => ps_side,
                            };
                            let pj_c = spine[u].unwrap_or(pj);
                            if barred.contains(&(v, pj_c, ddc_geom::to_mm(ps))) {
                                continue;
                            }
                            // the feed across the wall clear of shafts, ducts and pipes
                            let from = carrier_xy(ru, spine[u], carrier_at(ru, spine[u], pj_c, ps, co[u]), co[u]);
                            let v_spine = circulation(rv.kind).then_some(cj);
                            let dv = if v_spine.is_some() { d_spine() } else { co[v] };
                            let to = carrier_xy(rv, v_spine, carrier_at(rv, v_spine, cj, cs, dv), dv);
                            let probe = run_for(k, 0.0, vec![v3(from.0, from.1, z), v3(to.0, to.1, z)]);
                            if !crowded(ru, spine[u], &taken[u], pj_c, ps) && space.fits(&probe, svc::gap()) {
                                found = Some(Feed { parent: u, pj: pj_c, ps, cj, cs });
                                break 'pair;
                            }
                        }
                    }
                }
                if let Some(f) = found {
                    seen[v] = true;
                    if circulation(rv.kind) {
                        (spine[v], co[v]) = (Some(f.cj), d_spine());
                    }
                    taken[u].push((f.pj, f.ps));
                    taken[v].push((f.cj, f.cs));
                    let pl = plan(spec, p, k, rv, f_rel, &fps[k], spine[v], &taken[v]);
                    note(&pl, &mut taken[v]);
                    plans[v] = Some(pl);
                    feed[v] = Some(f);
                    q.push_back(v);
                }
            }
        }
        // board drop: top of the board above its bottom, conduit diameter; fix attempts per room
        let (board_top, board_drop) = (rule("services.elec.board_top"), rule("services.elec.board_drop"));
        let attempts = rule("services.elec.attempts") as usize;
        // unit boards in the apartment halls, on the spine side beside the entrance feed
        let mut unit_at: Vec<Option<f64>> = vec![None; rs.len()];
        if spec.services.boards == Boards::Unit && spec.typology == Typology::ApartmentSection {
            let (ub_margin, ub_step) = (rule("services.elec.unit_board_margin"), rule("services.elec.unit_board_step"));
            let (ub_before, ub_after, ub_depth) = (rule("services.elec.board_box_before"), rule("services.elec.unit_box_after"), rule("services.elec.board_box_depth"));
            let (ub_clear, ub_top) = (rule("services.elec.unit_clear_half"), WALL_TOP);
            for &v in &order {
                let (Some(j), PRole::Apartment(_)) = (spine[v], rs[v].owner) else { continue };
                if rs[v].kind != RoomKind::Hall {
                    continue;
                }
                let sd = &rs[v].sides[j];
                let mut s = ub_margin;
                while s < sd.len - ub_margin {
                    let b = svc::bbox(&[sd.at(s - ub_before, 0.0), sd.at(s + ub_after, ub_depth)]);
                    if sd.half > 0.0 && sd.free(s - ub_clear, s + ub_clear, 0.0, 0.0, ub_top) && !fps[k].iter().any(|f| svc::hit(f, &b, 0.0)) && !taken[v].iter().any(|t| t.0 == j && (t.1 - s).abs() < spacing()) {
                        unit_at[v] = Some(s);
                        taken[v].push((j, s));
                        break;
                    }
                    s += ub_step;
                }
            }
        }
        // subtree loads, leaves first
        let mut sub: Vec<f64> = plans.iter().map(|p| p.as_ref().map_or(0.0, Plan::load)).collect();
        for &v in order.iter().rev() {
            if let Some(f) = feed[v] {
                sub[f.parent] += sub[v];
            }
        }
        // build in tree order; a room that does not fit loses its subtree
        let mut ok = vec![false; rs.len()];
        // each built room's runs: a child's check takes the parent's carrier where its feed tees on
        let mut room_runs: Vec<Vec<GRun>> = vec![vec![]; rs.len()];
        for &v in &order {
            if feed[v].is_some_and(|f| !ok[f.parent]) {
                unserved += 1;
                continue;
            }
            let Some(mut pl) = plans[v].clone() else { continue };
            let (pl0, told) = (pl.clone(), dropped.len());
            let (mut shift, mut split): (usize, usize) = (0, 0);
            // every carrier variant tried: back on the usual one, items at the end of a blocked
            // stretch go one by one
            let mut trim = false;
            let mut attempt = 0;
            let built = loop {
                attempt += 1;
                let child_tees: Vec<Tee> = order
                    .iter()
                    .filter_map(|&c| feed[c].filter(|f| f.parent == v).map(|f| (c, f)))
                    .map(|(c, f)| Tee { at: carrier_at(&rs[v], spine[v], f.pj, f.ps, co[v]), load: sub[c], kind: TeeKind::Child })
                    .collect();
                let corner_leg = ring_corner_leg(k, pl.load() + child_tees.iter().map(|t| t.load).sum::<f64>());
                let mut tees: Vec<Tee> = vec![];
                let feed_from = feed[v].map(|f| {
                    let pr = &rs[f.parent];
                    let q = match spine[f.parent] {
                        Some(sj) => pr.sides[sj].at(f.ps, co[f.parent]),
                        None => pr.sides[f.pj].at(f.ps, co[f.parent]),
                    };
                    v3(q.0, q.1, z)
                });
                if let Some(f) = feed[v] {
                    tees.push(Tee { at: carrier_at(&rs[v], spine[v], f.cj, f.cs, co[v]), load: 0.0, kind: TeeKind::Feed });
                }
                if v == board_room {
                    if n_st > 1 {
                        tees.push(Tee { at: fb.s + riser_off, load: 0.0, kind: TeeKind::Riser });
                    }
                    tees.push(Tee { at: fb.s, load: 0.0, kind: TeeKind::Board });
                }
                if let Some(s) = unit_at[v] {
                    tees.push(Tee { at: s, load: 0.0, kind: TeeKind::Board });
                }
                for (i, d) in pl.devs.iter().enumerate() {
                    let at = match spine[v] {
                        Some(sj) => rs[v].sides[sj].s_of(rs[v].sides[d.j].at(d.s, d.off)),
                        None => carrier_at(&rs[v], None, d.j, ring_tee(&rs[v], d, corner_leg, co[v]), co[v]),
                    };
                    tees.push(Tee { at, load: d.load, kind: TeeKind::Dev(i) });
                }
                for (i, c) in pl.cols.iter().enumerate() {
                    tees.push(Tee { at: carrier_at(&rs[v], spine[v], c.j, c.s, co[v]), load: c.ds.len() as f64 * c.ty.2, kind: TeeKind::Col(i) });
                }
                tees.extend(child_tees.iter().cloned());
                let opens = if spine[v].is_none() { ring_opens(&rs[v], co[v], &tees) } else { vec![] };
                let open = split.checked_sub(1).and_then(|i| opens.get(i).copied());
                let mut b = build_room(k, &pl, spine[v], &tees, feed_from, z, zf, e, co[v], open);
                // board drops and the riser hang on the board room's spine
                if v == board_room {
                    let sd = &rs[v].sides[fb.j];
                    let w = sd.at(fb.s, d_spine());
                    let pz = e + board_z() + board_top;
                    let r = if k == 0 { svc::tray_run(k, tray().0, tray().1, vec![v3(w.0, w.1, z), v3(w.0, w.1, pz)]) } else { svc::conduit_run(k, board_drop, vec![v3(w.0, w.1, z), v3(w.0, w.1, pz)]) };
                    b.runs.push(r);
                }
                if let (Some(s), Some(j)) = (unit_at[v], spine[v]) {
                    let w = rs[v].sides[j].at(s, co[v]);
                    b.runs.push(svc::conduit_run(k, board_drop, vec![v3(w.0, w.1, z), v3(w.0, w.1, e + board_z() + board_top)]));
                }
                // route faults found alone: drop the device or row nearest to each
                let mut pseudo: Vec<V3> = b.ports.iter().map(|x| v3(x.1.at.0, x.1.at.1, x.1.at.2)).collect();
                if let Some(f) = feed_from {
                    pseudo.push(f);
                }
                // the children's feeds are built with the children; their starts take the fittings
                // where they tee on here, so they stand in, open at the child's carrier
                let mut stubs: Vec<GRun> = vec![];
                for (c, f) in order.iter().filter_map(|&c| feed[c].filter(|f| f.parent == v).map(|f| (c, f))) {
                    let from = carrier_xy(&rs[v], spine[v], carrier_at(&rs[v], spine[v], f.pj, f.ps, co[v]), co[v]);
                    let to = carrier_xy(&rs[c], spine[c], carrier_at(&rs[c], spine[c], f.cj, f.cs, co[c]), co[c]);
                    stubs.push(run_for(k, sub[c], vec![v3(from.0, from.1, z), v3(to.0, to.1, z)]));
                    pseudo.push(v3(to.0, to.1, z));
                }
                // the parent's carrier where the feed tees on, open at its other ends
                let parent_here: Vec<GRun> = feed_from
                    .zip(feed[v])
                    .map(|(q, f)| room_runs[f.parent].iter().filter(|r| passes(r, q)).cloned().collect())
                    .unwrap_or_default();
                for r in &parent_here {
                    for e in [r.points.first(), r.points.last()].into_iter().flatten() {
                        pseudo.push(v3(e.0, e.1, e.2));
                    }
                }
                if v == board_room {
                    let w = rs[v].sides[fb.j].at(fb.s, d_spine());
                    pseudo.push(v3(w.0, w.1, e + board_z() + board_top));
                    if n_st > 1 {
                        let q = core_xy.unwrap_or_else(|| rs[v].sides[fb.j].at(fb.s + riser_off, d_spine()));
                        if k > 0 {
                            pseudo.push(v3(q.0, q.1, zc[k - 1]));
                        }
                        if k + 1 < n_st {
                            pseudo.push(v3(q.0, q.1, zc[k + 1]));
                        }
                    }
                }
                if let (Some(s), Some(j)) = (unit_at[v], spine[v]) {
                    let w = rs[v].sides[j].at(s, co[v]);
                    pseudo.push(v3(w.0, w.1, e + board_z() + board_top));
                }
                let mut check: Vec<&GRun> = b.runs.iter().collect();
                // the riser pieces meeting this spine
                let riser_here: Vec<GRun> = if v == board_room && n_st > 1 {
                    let q = rs[v].sides[fb.j].at(fb.s + riser_off, d_spine());
                    let mut rr = vec![];
                    if let Some(c) = core_xy {
                        // the stub from the spine to the shaft, the riser pieces in the shaft
                        rr.push(svc::tray_run(k, tray().0, tray().1, vec![v3(q.0, q.1, z), v3(c.0, c.1, z)]));
                        if k > 0 {
                            rr.push(svc::tray_run(k, tray().0, tray().1, vec![v3(c.0, c.1, zc[k - 1]), v3(c.0, c.1, z)]));
                        }
                        if k + 1 < n_st {
                            rr.push(svc::tray_run(k, tray().0, tray().1, vec![v3(c.0, c.1, z), v3(c.0, c.1, zc[k + 1])]));
                        }
                        rr
                    } else {
                    if k > 0 {
                        rr.push(svc::tray_run(k, tray().0, tray().1, vec![v3(q.0, q.1, zc[k - 1]), v3(q.0, q.1, z)]));
                    }
                    if k + 1 < n_st {
                        rr.push(svc::tray_run(k, tray().0, tray().1, vec![v3(q.0, q.1, z), v3(q.0, q.1, zc[k + 1])]));
                    }
                    rr
                    }
                } else {
                    vec![]
                };
                check.extend(riser_here.iter());
                check.extend(stubs.iter());
                check.extend(parent_here.iter());
                let faults = svc::net_faults_at(&check, &pseudo);
                let bad = if faults.is_empty() { None } else { nearest_item(&pl, &rs[v], &faults) };
                if let Some(item) = bad.filter(|_| attempt < attempts) {
                    let why = if faults.iter().any(|f| f.1.contains("too short")) { "no room for bends" } else { "no fitting network" };
                    dropped.push(left_out(k, &rs[v], &pl, item, why));
                    match item {
                        Item::Dev(i) => {
                            pl.devs.remove(i);
                        }
                        Item::Col(i) => {
                            pl.cols.remove(i);
                        }
                    }
                    continue;
                }
                // a ring given up is tried open elsewhere, then at the next offsets, from the full
                // plan again: the usual offset plus one step, less one step, plus two steps, ...
                let n_opens = opens.len();
                let reshift = |(shift, split): (&mut usize, &mut usize), co: &mut [f64], trim: bool| {
                    if trim {
                        return false;
                    }
                    if *split < n_opens {
                        *split += 1;
                        return true;
                    }
                    *split = 0;
                    // a spine steps off its wall only, the board room's never: the riser stands on it
                    while v != board_room && *shift < co_tries {
                        *shift += 1;
                        let n = (*shift).div_ceil(2) as f64;
                        let d = match spine[v] {
                            Some(_) => d_spine() + co_step * *shift as f64,
                            None => d_ring() + co_step * if *shift % 2 == 1 { n } else { -n },
                        };
                        if d > EPS {
                            co[v] = d;
                            return true;
                        }
                    }
                    false
                };
                if !faults.is_empty() {
                    if reshift((&mut shift, &mut split), &mut co, trim) {
                        (pl, attempt) = (pl0.clone(), 0);
                        dropped.truncate(told);
                        continue;
                    }
                    break None;
                }
                // clashes: drop what does not fit, else give the room up
                let m = space.mark();
                if space.try_add(&b.runs, svc::gap()) {
                    break Some(b);
                }
                space.rollback(m);
                let culprit = b.runs.iter().position(|r| !space.fits(r, svc::gap()));
                let item = culprit.and_then(|ri| owner_of(&b, ri, &pl, &rs[v])).or_else(|| {
                    let q = culprit.filter(|_| trim).and_then(|ri| b.runs[ri].points.last().copied())?;
                    nearest_item(&pl, &rs[v], &[(v3(q.0, q.1, q.2), String::new())])
                });
                if let Some(it) = item.filter(|_| attempt < attempts) {
                    dropped.push(left_out(k, &rs[v], &pl, it, "no room clear of other services"));
                }
                match item.filter(|_| attempt < attempts) {
                    Some(Item::Dev(i)) => {
                        pl.devs.remove(i);
                    }
                    Some(Item::Col(i)) => {
                        pl.cols.remove(i);
                    }
                    None if reshift((&mut shift, &mut split), &mut co, trim) => {
                        (pl, attempt) = (pl0.clone(), 0);
                        dropped.truncate(told);
                    }
                    None if !trim => {
                        (trim, co[v], split) = (true, if spine[v].is_some() { d_spine() } else { d_ring() }, 0);
                        (pl, attempt) = (pl0.clone(), 0);
                        dropped.truncate(told);
                    }
                    None => break None,
                }
            };
            let Some(b) = built else {
                if let Some(f) = feed[v] {
                    failed.push((v, f));
                }
                dropped.push(format!("storey {k} room {} {:?} unpowered: its network does not fit", rs[v].idx, rs[v].kind));
                unserved += 1;
                continue;
            };
            ok[v] = true;
            if pl.lux.1 > 0.0 {
                let placed: usize = pl.cols.iter().map(|c| c.ds.len()).sum();
                let ty = pl.cols.first().map_or(light_s(), |c| c.ty);
                let area = pl.room.area();
                let ki = {
                    let (l, d) = dims(&pl.room, pl.cols.first().map_or(0, |c| c.j));
                    let hm = (f_rel - rule("services.elec.work_plane")).max(1.0);
                    l * d / (hm * (l + d))
                };
                let reached = placed as f64 * ty.1 * norms().uf(ki) * norms().mf() / area.max(1e-6);
                if reached + 1e-6 < pl.lux.1 {
                    lux_short += 1;
                }
            }
            room_runs[v] = b.runs.clone();
            runs.extend(b.runs);
            // fixtures and links
            let room = &pl.room;
            let owner_key = match room.owner {
                PRole::Apartment(a) => Some(a),
                PRole::Public => None,
            };
            if let (Some(s), Some(j), Some(a)) = (unit_at[v], spine[v], owner_key) {
                let sd = &room.sides[j];
                let idx = n_fx[k] + add[k].len();
                add[k].push(wall_fixture(&p.storeys[k].layout, sd, s, "Distribution Board", "Sub 400x600", board_z()));
                links.push(GSvcLink { storey: k, fixture: idx, connector: "Main", sys: SvcSys::Power });
                unit_boards.insert((k, a), (k, idx));
                board_fx.push((k, idx));
            }
            if v == board_room {
                let sd = &room.sides[fb.j];
                let idx = n_fx[k] + add[k].len();
                add[k].push(wall_fixture(&p.storeys[k].layout, sd, fb.s, "Distribution Board", if k == 0 { "400x600" } else { "Sub 400x600" }, board_z()));
                links.push(GSvcLink { storey: k, fixture: idx, connector: "Main", sys: SvcSys::Power });
                board_fx.push((k, idx));
            }
            let mut dev_idx: HashMap<usize, usize> = HashMap::new();
            let mut col_idx: HashMap<(usize, usize), (usize, bool)> = HashMap::new();
            let gname = &p.storeys[k].layout.rooms[room.idx].name;
            let em_every = emergency_every(spec, room.kind, gname);
            let essential = on_backup(spec, room.kind, gname);
            for (pi, port, row) in &b.ports {
                match row {
                    None => {
                        let d = &pl.devs[*pi];
                        let idx = match d.existing {
                            Some(i) => i,
                            None => *dev_idx.entry(*pi).or_insert_with(|| {
                                let i = n_fx[k] + add[k].len();
                                add[k].push(wall_fixture(&p.storeys[k].layout, &room.sides[d.j], d.s, d.fam, d.ty, d.z));
                                i
                            }),
                        };
                        links.push(GSvcLink { storey: k, fixture: idx, connector: "Power", sys: SvcSys::Power });
                        // sockets of a room on backup power hang on the essential supply
                        let use_ = if essential && d.use_ == CircuitUse::Sockets { CircuitUse::Essential } else { d.use_ };
                        served.push((k, idx, use_, port.flow, v, room.idx, d.fam));
                    }
                    Some(r) => {
                        let c = &pl.cols[*pi];
                        let nth = col_idx.len();
                        let (idx, em) = *col_idx.entry((*pi, *r)).or_insert_with(|| {
                            let i = n_fx[k] + add[k].len();
                            // the first luminaire of the room, then every n-th, is an emergency one
                            let em = em_every.is_some_and(|e| nth % e == 0);
                            let ty = if em { emergency_type(c.ty.0) } else { c.ty.0 };
                            add[k].push(GFixture { scale: 1.0, min_clear: 0.0, family: "Ceiling Light", ty, at: (port.at.0, port.at.1), rotation: 0.0, wall: None, z: f_rel });
                            (i, em)
                        });
                        links.push(GSvcLink { storey: k, fixture: idx, connector: "Power", sys: SvcSys::Power });
                        let use_ = if em { CircuitUse::Emergency } else { CircuitUse::Lighting };
                        served.push((k, idx, use_, port.flow, v, room.idx, "Ceiling Light"));
                    }
                }
            }
        }
        for v in 0..rs.len() {
            if !seen[v] && wired(rs[v].kind) {
                if cut.contains(&v) {
                    dropped.push(format!("storey {k} room {} {:?} unpowered: no feed into it fits", rs[v].idx, rs[v].kind));
                }
                unserved += 1;
            }
        }
        if !failed.is_empty() {
            space.rollback(undo.0);
            runs.truncate(undo.1);
            dropped.truncate(undo.2);
            links.truncate(undo.3);
            served.truncate(undo.4);
            board_fx.truncate(undo.5);
            (unserved, lux_short) = (undo.6, undo.7);
            add[k].clear();
            unit_boards.retain(|key, _| key.0 != k);
            tries_k += 1;
            for (v, f) in failed {
                if tries_k > retries {
                    cut.push(v);
                } else {
                    barred.push((v, f.pj, ddc_geom::to_mm(f.ps)));
                }
            }
            continue;
        }
        (barred, cut, tries_k) = (vec![], vec![], 0);
        k += 1;
    }
    drop(space);
    // a core shaft cuts the slabs itself
    let holes: Vec<(usize, svc::R)> = (1..n_st).filter(|_| core_xy.is_none()).map(|k| (k, svc::hole(riser_xy.0, riser_xy.1, tray().0))).collect();
    svc::add_holes(p, &holes);
    for (k, fx) in add.into_iter().enumerate() {
        p.storeys[k].layout.fixtures.extend(fx);
    }
    p.services.elec = runs;
    p.services.issues = dropped;
    p.services.links.extend(links);
    p.services.circuits = circuits(spec, p, &floor_board_fx(&board_fx, n_st), &unit_boards, &served);
    p.metrics.push(("power_unserved_rooms", unserved as f64));
    p.metrics.push(("lux_short_rooms", lux_short as f64));
    unserved
}

fn floor_board_fx(boards: &[(usize, usize)], n_st: usize) -> Vec<Option<(usize, usize)>> {
    // the first board placed on a storey is its floor board (the board room is built first)
    (0..n_st).map(|k| boards.iter().copied().find(|b| b.0 == k)).collect()
}

/// Where a device tees off a ring: square to it, or along the wall for a conduit too stiff to
/// turn down within the ring's distance from the wall, so its drop meets the wall slantwise.
/// The tee keeps `leg` from the ring's corners on its side, so the stretch to the corner holds
/// the tee and the elbow; a device nearer the corner is reached by a slanted drop.
fn ring_tee(room: &Room, d: &Dev, leg: f64, dr: f64) -> f64 {
    let len = room.sides[d.j].len;
    let s = if d.load <= w20() {
        d.s
    } else {
        let shift = rule("services.elec.ring_tee_shift");
        if d.s + shift <= len - dr - rule("services.elec.ring_tee_slack") {
            d.s + shift
        } else {
            d.s - shift
        }
    };
    let (lo, hi) = (dr + leg, len - dr - leg);
    if hi < lo {
        len / 2.0
    } else {
        s.clamp(lo, hi)
    }
}

/// Least distance of a tee from a ring corner: the tee's leg and the corner elbow of the
/// largest section the ring carries.
fn ring_corner_leg(k: usize, load: f64) -> f64 {
    let r = run_for(k, load, vec![]);
    ddc_geom::mep::junction_leg(&r.section) + svc::bend_max(&r)
}

fn carrier_at(room: &Room, spine: Option<usize>, j: usize, s: f64, dr: f64) -> f64 {
    match spine {
        Some(sj) => room.sides[sj].s_of(room.sides[j].at(s, 0.0)),
        None => Ring::new(room, dr).u(j, s),
    }
}

/// What hangs on an arm's last tee, as the arm end sees it: the point the horizontal runs to,
/// its elbow radius, what its far end takes (the elbow down, or the tee of a middle luminaire),
/// what the arm's end takes besides the elbow (a reducer off a tray) and the tee leg of the
/// stretch before.
#[derive(Clone, Copy)]
struct End {
    w: (f64, f64),
    r: f64,
    far: f64,
    near: f64,
    leg_prev: f64,
}

fn hang(room: &Room, pl: &Plan, spine: Option<usize>, kind: TeeKind, k: usize) -> Option<End> {
    let (w, run, far_tee) = match kind {
        TeeKind::Dev(i) => {
            let d = &pl.devs[i];
            (room.sides[d.j].at(d.s, d.off), run_for(k, d.load, vec![]), false)
        }
        TeeKind::Col(i) => {
            let c = &pl.cols[i];
            let load = c.ds.len() as f64 * c.ty.2;
            (room.sides[c.j].at(c.s, *c.ds.first()?), run_for(k, load, vec![]), c.ds.len() > 1)
        }
        _ => return None,
    };
    let r = svc::bend_max(&run);
    let leg = ddc_geom::mep::junction_leg(&run.section);
    let (arm, near) = match spine {
        Some(_) => {
            let t = svc::tray_run(k, tray().0, tray().1, vec![]);
            (t.section, ddc_geom::mep::transition_length(&t.section, &run.section))
        }
        None => (run.section, 0.0),
    };
    let leg_prev = ddc_geom::mep::junction_leg(&arm);
    Some(End { w, r, far: if far_tee { leg } else { r }, near, leg_prev })
}

/// At arm distance `de`: what the elbow off the arm takes from the horizontal and from the arm
/// (the arm's share with its reducer and the tee leg before it).
fn trims(at: &dyn Fn(f64) -> (f64, f64), de: f64, e: End) -> (f64, f64) {
    let step = rule("services.elec.drop_end_step");
    let (a, b) = (at(de), at(de - step));
    let (ix, iy) = (a.0 - b.0, a.1 - b.1);
    let (ox, oy) = (e.w.0 - a.0, e.w.1 - a.1);
    let (li, lo) = (ix.hypot(iy), ox.hypot(oy));
    if li < EPS || lo < EPS {
        return (f64::INFINITY, f64::INFINITY);
    }
    let turn = ((ix * ox + iy * oy) / (li * lo)).clamp(-1.0, 1.0).acos();
    let t = e.r * (turn / 2.0).tan();
    (t + e.far, t + e.near + e.leg_prev)
}

/// Where an arm ending at `e` stops, between `floor` (the tee before) and `d_end`: `d_end` when
/// it holds, else the first stop back along the arm whose horizontal holds the elbow off the
/// arm (by its turn) and what its far end takes, and whose stretch from `floor` holds the
/// elbow, the reducer and the tee leg. `None` when no stop there holds.
fn arm_end(at: &dyn Fn(f64) -> (f64, f64), d_end: f64, floor: f64, e: End) -> Option<f64> {
    let step = rule("services.elec.drop_end_step");
    let holds = |de: f64| {
        let (a, w) = (at(de), e.w);
        let (h, arm) = trims(at, de, e);
        (w.0 - a.0).hypot(w.1 - a.1) >= h + EPS && de - floor >= arm + EPS
    };
    let mut de = d_end;
    while de > floor {
        if holds(de) {
            return Some(de);
        }
        de -= step;
    }
    None
}

/// Whether a tee at `de` holds what hangs on it: the horizontal from the tee leg to its end.
fn tee_holds(at: &dyn Fn(f64) -> (f64, f64), de: f64, e: End) -> bool {
    let a = at(de);
    (e.w.0 - a.0).hypot(e.w.1 - a.1) >= e.leg_prev + e.far + EPS
}

/// The services check line for a device or luminaire row the retries leave out.
fn left_out(k: usize, room: &Room, pl: &Plan, item: Item, why: &str) -> String {
    let what = match item {
        Item::Dev(i) => format!("device {} {} unpowered", pl.devs[i].fam, pl.devs[i].ty),
        Item::Col(i) => format!("luminaire row of {} unlit", pl.cols[i].ds.len()),
    };
    format!("storey {k} room {} {:?}: {what}: {why}", room.idx, room.kind)
}

/// Where else a ring may stand open, as the farthest a tee may be along the first arm: before
/// every tee, between each two and after the last, so a run that meets an obstacle can go the
/// other way round. The first tee is the feed the arms start from.
fn ring_opens(room: &Room, dr: f64, tees: &[Tee]) -> Vec<f64> {
    let Some(origin) = tees.first().map(|t| t.at) else { return vec![] };
    let per = Ring::new(room, dr).per();
    let mut df: Vec<f64> = tees[1..].iter().map(|t| (t.at - origin).rem_euclid(per)).collect();
    df.sort_by(f64::total_cmp);
    df.dedup_by(|a, b| (*a - *b).abs() < EPS);
    let mut out = vec![f64::NEG_INFINITY];
    out.extend(df.windows(2).map(|w| (w[0] + w[1]) / 2.0).filter(|g| (g - per / 2.0).abs() > EPS));
    out.push(f64::INFINITY);
    out
}

/// Whether point `q` lies on the run's axis.
fn passes(r: &GRun, q: V3) -> bool {
    r.points.windows(2).any(|w| {
        let (a, b) = (v3(w[0].0, w[0].1, w[0].2), v3(w[1].0, w[1].1, w[1].2));
        let ab = b - a;
        let t = if ab.len() < EPS { 0.0 } else { ((q - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0) };
        (a + ab * t - q).len() < EPS
    })
}

fn carrier_xy(room: &Room, spine: Option<usize>, at: f64, dr: f64) -> (f64, f64) {
    match spine {
        Some(sj) => room.sides[sj].at(at, dr),
        None => Ring::new(room, dr).at(at),
    }
}

fn wall_fixture(l: &crate::GLayout, sd: &svc::Side, s: f64, family: &'static str, ty: &'static str, z: f64) -> GFixture {
    let at = sd.at(s, 0.0);
    let line_at = if sd.horiz { at.0 } else { at.1 };
    let band = rule("services.elec.wall_band");
    let wall = svc::wall_at(l, sd.horiz, sd.line, line_at - band, line_at + band);
    GFixture { scale: 1.0, min_clear: 0.0, family, ty, at, rotation: sd.wall_rot(), wall, z }
}

#[derive(Clone, Copy, Debug)]
enum Item {
    Dev(usize),
    Col(usize),
}

/// The device or row of luminaires nearest to the first fault.
fn nearest_item(pl: &Plan, room: &Room, faults: &[(V3, String)]) -> Option<Item> {
    let q = faults[0].0;
    let mut best: Option<(f64, Item)> = None;
    for (i, d) in pl.devs.iter().enumerate() {
        let w = room.sides[d.j].at(d.s, 0.0);
        let dist = ((w.0 - q.x).powi(2) + (w.1 - q.y).powi(2)).sqrt();
        if best.is_none_or(|b| dist < b.0) {
            best = Some((dist, Item::Dev(i)));
        }
    }
    for (i, c) in pl.cols.iter().enumerate() {
        for &d in &c.ds {
            let w = room.sides[c.j].at(c.s, d);
            let w0 = room.sides[c.j].at(c.s, d_ring());
            let dist = ((w.0 - q.x).powi(2) + (w.1 - q.y).powi(2)).sqrt().min(((w0.0 - q.x).powi(2) + (w0.1 - q.y).powi(2)).sqrt());
            if best.is_none_or(|b| dist < b.0) {
                best = Some((dist, Item::Col(i)));
            }
        }
    }
    best.filter(|b| b.0 < 2.0).map(|b| b.1)
}

/// The device or row whose run is `ri`, by where the run ends.
fn owner_of(b: &Built, ri: usize, pl: &Plan, room: &Room) -> Option<Item> {
    let r = &b.runs[ri];
    let end = r.points.last().map(|q| v3(q.0, q.1, q.2))?;
    let start = r.points.first().map(|q| v3(q.0, q.1, q.2))?;
    // only device drops and rows end at a connector; carrier arms and the feed stay
    let at_port = |q: V3| b.ports.iter().any(|x| (v3(x.1.at.0, x.1.at.1, x.1.at.2) - q).len() < 1e-6);
    if r.caps.1 || !(at_port(end) || at_port(start)) {
        return None;
    }
    nearest_item(pl, room, &[(end, String::new())])
}

/// Circuits per board: lighting and general sockets packed up to the breaker, a worktop and
/// a wet circuit per room, one per appliance, and a feeder to every sub-board.
fn circuits(
    spec: &ProjectSpec,
    p: &GProject,
    floor: &[Option<(usize, usize)>],
    units: &HashMap<(usize, u32), (usize, usize)>,
    served: &[(usize, usize, CircuitUse, f64, usize, usize, &'static str)],
) -> Vec<GCircuit> {
    let ru = spec.ru;
    let word = |u: CircuitUse| {
        let key = match u {
            CircuitUse::Lighting => "lighting",
            CircuitUse::Sockets => "sockets",
            CircuitUse::Worktop => "worktop",
            CircuitUse::Wet => "wet",
            CircuitUse::Appliance => "appliance",
            CircuitUse::Feeder => "feeder",
            CircuitUse::Emergency => "emergency",
            CircuitUse::Essential => "essential",
        };
        svc::text(&["circuits", key], ru)
    };
    let lengths = route_lengths(p);
    let mut out: Vec<GCircuit> = vec![];
    // board of a device: the unit board of its apartment, else its floor board
    let rooms: Vec<Vec<Room>> = p.storeys.iter().map(svc::rooms).collect();
    let board_of = |k: usize, room_idx: usize| -> Option<(usize, usize)> {
        let owner = rooms[k].iter().find(|r| r.idx == room_idx).map(|r| r.owner);
        if let Some(PRole::Apartment(a)) = owner {
            if let Some(b) = units.get(&(k, a)) {
                return Some(*b);
            }
        }
        floor[k]
    };
    let mut groups: HashMap<((usize, usize), u8, usize), Vec<(usize, usize, f64)>> = HashMap::new();
    let mut keys: Vec<((usize, usize), u8, usize)> = vec![];
    // appliance circuits key on the fixture, told apart per storey by this stride
    let stride = KEY_STRIDE;
    for &(k, fx, use_, load, _, room_idx, _) in served {
        let Some(b) = board_of(k, room_idx) else { continue };
        let (code, per_room) = match use_ {
            CircuitUse::Lighting => (0u8, usize::MAX),
            CircuitUse::Sockets => (1, usize::MAX),
            CircuitUse::Worktop => (2, room_idx),
            CircuitUse::Wet => (3, usize::MAX),
            CircuitUse::Appliance => (4, fx + stride * k),
            CircuitUse::Feeder => (5, fx),
            CircuitUse::Emergency => (6, usize::MAX),
            CircuitUse::Essential => (7, usize::MAX),
        };
        let key = (b, code, per_room);
        if !groups.contains_key(&key) {
            keys.push(key);
        }
        groups.entry(key).or_default().push((k, fx, load));
    }
    let (a_light, a_other) = (rule("services.elec.breaker_light"), rule("services.elec.breaker_other"));
    let limit = |code: u8| match code {
        0 | 6 => a_light * volts(),
        _ => a_other * volts(),
    };
    let mut count: HashMap<((usize, usize), u8), usize> = HashMap::new();
    for key in keys {
        let devs = &groups[&key];
        let use_ = [
            CircuitUse::Lighting,
            CircuitUse::Sockets,
            CircuitUse::Worktop,
            CircuitUse::Wet,
            CircuitUse::Appliance,
            CircuitUse::Feeder,
            CircuitUse::Emergency,
            CircuitUse::Essential,
        ][key.1 as usize];
        // pack by the breaker
        let mut packs: Vec<Vec<(usize, usize, f64)>> = vec![];
        for d in devs {
            match packs.last_mut() {
                Some(pk) if key.1 != 4 && pk.iter().map(|x| x.2).sum::<f64>() + d.2 <= limit(key.1) => pk.push(*d),
                _ => packs.push(vec![*d]),
            }
        }
        for pk in packs {
            let c = count.entry((key.0, key.1)).or_insert(0);
            *c += 1;
            let code = ["L", "S", "K", "W", "A", "F", "E", "N"][key.1 as usize];
            let load: f64 = pk.iter().map(|x| x.2).sum();
            let length = pk.iter().map(|x| lengths.get(&(key.0, (x.0, x.1))).copied().unwrap_or(0.0)).fold(0.0, f64::max);
            let name = if key.1 == 4 {
                let f = &p.storeys[pk[0].0].layout.fixtures[pk[0].1];
                format!("{code}{} {}", c, f.family)
            } else {
                format!("{code}{} {}", c, word(use_))
            };
            out.push(GCircuit {
                board: key.0,
                name,
                use_,
                devices: pk.iter().map(|x| (x.0, x.1, if use_ == CircuitUse::Feeder { "Main" } else { "Power" })).collect(),
                load,
                length,
            });
        }
    }
    // feeders: the main board to each floor board, a floor board to each unit board
    let main = floor.first().copied().flatten();
    let sub_load = |b: (usize, usize)| -> f64 {
        served.iter().filter(|s| board_of(s.0, s.5) == Some(b)).map(|s| s.3).sum()
    };
    let mut feeders: Vec<((usize, usize), (usize, usize))> = vec![];
    if let Some(m) = main {
        for f in floor.iter().skip(1).flatten() {
            feeders.push((m, *f));
        }
    }
    for (&(k, _), &u) in units {
        if let Some(f) = floor[k] {
            feeders.push((f, u));
        }
    }
    feeders.sort();
    for (n, (from, to)) in feeders.into_iter().enumerate() {
        let mut load = sub_load(to);
        // a floor board also carries its unit boards
        for (&(k, _), &u) in units {
            if floor[k] == Some(to) {
                load += sub_load(u);
            }
        }
        out.push(GCircuit {
            board: from,
            name: format!("F{} {}", n + 1, word(CircuitUse::Feeder)),
            use_: CircuitUse::Feeder,
            devices: vec![(to.0, to.1, "Main")],
            load,
            length: lengths.get(&(from, to)).copied().unwrap_or(0.0),
        });
    }
    out
}

/// Route length along the power runs from every board connector to every connector, m.
fn route_lengths(p: &GProject) -> HashMap<((usize, usize), (usize, usize)), f64> {
    let s = &p.services;
    let runs: Vec<&GRun> = s.elec.iter().collect();
    let mut out = HashMap::new();
    if runs.is_empty() {
        return out;
    }
    let net = svc::network(&runs);
    let mut adj: Vec<Vec<(usize, f64)>> = vec![vec![]; net.nodes.len()];
    for (_, pts, nd, _) in &net.edges {
        let l = svc::len(pts);
        adj[nd[0]].push((nd[1], l));
        adj[nd[1]].push((nd[0], l));
    }
    let node_of = |q: (f64, f64, f64)| net.nodes.iter().position(|n| (*n - v3(q.0, q.1, q.2)).len() < 5e-3);
    let ports: Vec<((usize, usize), usize)> = s
        .links
        .iter()
        .filter(|l| l.sys == SvcSys::Power)
        .filter_map(|l| svc::port_world(p, l.storey, l.fixture, l.connector).and_then(|c| node_of(c.at)).map(|n| ((l.storey, l.fixture), n)))
        .collect();
    let boards: Vec<((usize, usize), usize)> = ports.iter().copied().filter(|(f, _)| p.storeys[f.0].layout.fixtures[f.1].family == "Distribution Board").collect();
    for (b, bn) in boards {
        // tree: one path to each node
        let mut dist = vec![f64::INFINITY; net.nodes.len()];
        dist[bn] = 0.0;
        let mut st = vec![bn];
        while let Some(u) = st.pop() {
            for &(w, l) in &adj[u] {
                if dist[w].is_infinite() {
                    dist[w] = dist[u] + l;
                    st.push(w);
                }
            }
        }
        for (f, n) in &ports {
            if dist[*n].is_finite() {
                out.insert((b, *f), dist[*n]);
            }
        }
    }
    out
}
