//! Standpipes for the fire service: in a building over the rule's height, a riser in each stair
//! core, standing in a corner clear of the flights, landings and doors on every storey, a
//! landing valve beside it on every storey, an inlet at its foot where the fire service or the
//! pump room feeds it, and the hose reach of the valves measured over every room. Every number
//! is a rule.

use ddc_geom::math::v3;
use ddc_geom::mep::PipeMaterial;

use super::rules::rule;
use super::sprinkler::{coverage_by, edge_dist, inside, steel_from, P2, SAMPLES};
use super::svc::{self, pipe_run, Room, R};
use super::{slots, FireRun, GFireGap, GFireLink, GFireRun, GProject, GStandpipe, ProjectSpec, RoomKind, SvcSys};
use crate::GFixture;

/// The valve and inlet families with their metric library types, named `DN<size>`.
const VALVE: (&str, &[&str]) = ("Landing Valve", &["DN65"]);
const INLET: (&str, &[&str]) = ("Fire Inlet", &["DN100"]);

/// The pipe sizes of the valve branch and of the riser, from their flows and size rules.
fn sizes() -> (u32, u32) {
    let dv = steel_from(rule("fire.valve_flow"), rule("fire.valve_dn") as u32);
    let dn = steel_from(rule("fire.standpipe_flow"), rule("fire.standpipe_dn_min") as u32);
    (dv, dn)
}

/// The library type of a family for the nominal size `dn`, when the library holds one.
fn ty_of(types: &[&'static str], dn: u32) -> Option<&'static str> {
    types.iter().copied().find(|t| t.strip_prefix("DN").and_then(|n| n.parse::<u32>().ok()) == Some(dn))
}

/// The valve and inlet types that fit the computed branch and riser sizes.
fn kit() -> Option<(&'static str, &'static str)> {
    let (dv, dn) = sizes();
    Some((ty_of(VALVE.1, dv)?, ty_of(INLET.1, dn)?))
}

/// Where a standpipe stands: the riser axis, the landing valve and the direction of its branch.
#[derive(Clone, Copy, Debug)]
struct Spot {
    riser: P2,
    valve: P2,
    dir: P2,
    valve_ty: &'static str,
    inlet_ty: &'static str,
}

pub(super) fn standpipes(spec: &ProjectSpec, p: &mut GProject) {
    let Some(last) = p.storeys.last() else { return };
    // the norms count from the floor of the top occupied storey, as for the sprinklers
    if last.elevation < rule("fire.standpipe_height") {
        return;
    }
    if kit().is_none() {
        let (dv, dn) = sizes();
        p.fire.issues.push(format!("standpipes: no library valve of DN{dv} or inlet of DN{dn}"));
        return;
    }
    for (i, s) in spots(p).into_iter().enumerate() {
        let name = if spec.ru { format!("{} {}", crate::lang::ru("Standpipe"), i + 1) } else { format!("Standpipe {}", i + 1) };
        build(p, name, s);
    }
    reach(p);
}

fn valve_at(s: &Spot) -> GFixture {
    GFixture { scale: 1.0, min_clear: 0.0, family: VALVE.0, ty: s.valve_ty, at: s.valve, rotation: s.dir.1.atan2(s.dir.0), wall: None, z: 0.0 }
}

/// Plan rings of the flights and landings of a storey's stairs.
fn stair_rings(p: &GProject, k: usize) -> Vec<Vec<P2>> {
    let mut out = vec![];
    for g in &p.storeys[k].layout.stairs {
        let Ok(st) = g.def().layout() else { continue };
        for f in &st.flights {
            out.push(f.ring(st.tread).iter().map(|q| (q.x, q.y)).collect());
        }
        for l in &st.landings {
            out.push(l.ring.iter().map(|q| (q.x, q.y)).collect());
        }
    }
    out
}

/// A plan box clear of every ring by `gap`.
fn box_clear(b: &R, rings: &[Vec<P2>], gap: f64) -> bool {
    rings.iter().all(|r| {
        let rb = svc::bbox(r);
        !svc::hit(b, &rb, gap)
    })
}

/// The side of a storey's stair room on the same wall line as `side`, if that room holds `q`.
fn same_side<'a>(rooms: &'a [Room], side: &svc::Side, q: P2) -> Option<&'a svc::Side> {
    let room = rooms.iter().find(|r| r.kind == RoomKind::Stair && svc::inside(q, &r.clear))?;
    room.sides.iter().find(|s| s.horiz == side.horiz && (s.line - side.line).abs() < svc::EPS)
}

/// The corner spot of a ground storey stair room, with the valve branch along `side` from
/// its start or end, when it is clear on every storey.
fn try_spot(p: &GProject, rings: &[Vec<Vec<P2>>], rooms: &[Vec<Room>], side: &svc::Side, end: bool) -> Option<Spot> {
    let w = rule("fire.standpipe_wall");
    let off = rule("fire.valve_offset");
    let sign = if end { -1.0 } else { 1.0 };
    let s_r = if end { side.len - w } else { w };
    let s_v = s_r + sign * off;
    if s_v < w || s_v > side.len - w {
        return None;
    }
    let (valve_ty, inlet_ty) = kit()?;
    let dir = (side.t.0 * sign, side.t.1 * sign);
    let spot = Spot { riser: side.at(s_r, w), valve: side.at(s_v, w), dir, valve_ty, inlet_ty };
    let fp = svc::footprint(&valve_at(&spot))?;
    let inlet = GFixture { scale: 1.0, min_clear: 0.0, family: INLET.0, ty: spot.inlet_ty, at: spot.riser, rotation: 0.0, wall: None, z: 0.0 };
    let ib = svc::footprint(&inlet)?;
    let dn = sizes().1;
    let rad = svc::od(PipeMaterial::Steel, dn) / 2.0;
    let gap = svc::gap();
    let pad = rule("fire.valve_door_clear");
    let top = rule("fire.valve_height") + (fp[3] - fp[1]).max(fp[2] - fp[0]);
    for k in 0..p.storeys.len() {
        let sd = same_side(&rooms[k], side, spot.riser)?;
        let room = rooms[k].iter().find(|r| r.kind == RoomKind::Stair && svc::inside(spot.riser, &r.clear))?;
        let fits = |b: &R| b[0] >= room.clear[0] - svc::EPS && b[1] >= room.clear[1] - svc::EPS && b[2] <= room.clear[2] + svc::EPS && b[3] <= room.clear[3] + svc::EPS;
        if !fits(&fp) || (k == 0 && !fits(&ib)) {
            return None;
        }
        let rs = &rings[k];
        let riser_clear = rs.iter().all(|r| !inside(r, spot.riser) && edge_dist(r, spot.riser) >= rad + gap);
        if !riser_clear || !box_clear(&fp, rs, gap) || (k == 0 && !box_clear(&ib, rs, gap)) {
            return None;
        }
        // no door on this wall over the riser and the valve, nor on the wall at the corner
        let (a, b) = (sd.s_of(spot.riser), sd.s_of(spot.valve));
        let (s0, s1) = (a.min(b) - w, a.max(b) + (fp[2] - fp[0]).max(fp[3] - fp[1]));
        if !sd.free(s0, s1, pad, 0.0, top) {
            return None;
        }
        let corner = room.sides.iter().find(|o| o.horiz != sd.horiz && (o.d_of(spot.riser) - w).abs() < svc::EPS)?;
        let c = corner.s_of(spot.riser);
        if !corner.free(c - w, c + w, pad, 0.0, top) {
            return None;
        }
    }
    Some(spot)
}

/// One spot per stair room of the ground storey that has a clear corner on every storey.
fn spots(p: &GProject) -> Vec<Spot> {
    let n = p.storeys.len();
    let rings: Vec<Vec<Vec<P2>>> = (0..n).map(|k| stair_rings(p, k)).collect();
    let rooms: Vec<Vec<Room>> = p.storeys.iter().map(svc::rooms).collect();
    let mut out = vec![];
    for r in rooms[0].iter().filter(|r| r.kind == RoomKind::Stair) {
        let found = r
            .sides
            .iter()
            .flat_map(|s| [false, true].map(|e| (s, e)))
            .find_map(|(s, e)| try_spot(p, &rings, &rooms, s, e));
        if let Some(s) = found {
            out.push(s);
        }
    }
    out
}

fn fire_run(kind: FireRun, k: usize, dn: u32, flow: f64, pts: Vec<ddc_geom::math::V3>) -> GFireRun {
    GFireRun { kind, run: pipe_run(SvcSys::Fire, k, PipeMaterial::Steel, dn, pts), flow, heads: 0 }
}

/// The riser of one spot from the inlet to above the top valve, a valve on every storey.
fn build(p: &mut GProject, name: String, s: Spot) {
    let n = p.storeys.len();
    let cap = slots::FIXTURES_MAX as usize;
    if (0..n).any(|k| p.storeys[k].layout.fixtures.len() + 2 > cap) {
        p.fire.issues.push(format!("{name}: no fixture room for the landing valves"));
        return;
    }
    let (q, qv) = (rule("fire.standpipe_flow"), rule("fire.valve_flow"));
    let (dv, dn) = sizes();
    let (x, y) = s.riser;
    let mut sp = GStandpipe { name, at: s.riser, ..Default::default() };
    let inlet = GFixture { scale: 1.0, min_clear: 0.0, family: INLET.0, ty: s.inlet_ty, at: s.riser, rotation: 0.0, wall: None, z: 0.0 };
    let foot = svc::svc_ports(&inlet).into_iter().find(|c| c.name == "Outlet").map_or(0.0, |c| c.at.2);
    let l = &mut p.storeys[0].layout.fixtures;
    sp.links.push(GFireLink { storey: 0, fixture: l.len(), connector: "Outlet" });
    l.push(inlet);
    let mut holes = vec![];
    for k in 0..n {
        let e = p.storeys[k].elevation;
        let mut v = valve_at(&s);
        let port = svc::svc_ports(&v).into_iter().find(|c| c.name == "Inlet").map_or(0.0, |c| c.at.2);
        v.z = rule("fire.valve_height") - port;
        let zv = e + rule("fire.valve_height");
        let z0 = if k == 0 { e + foot } else { e };
        let top = k + 1 == n;
        let z1 = if top { zv + rule("fire.standpipe_top") } else { p.storeys[k + 1].elevation };
        let mut riser = fire_run(FireRun::Riser, k, dn, q, vec![v3(x, y, z0), v3(x, y, z1)]);
        // the top stays closed: the air valve at its head is part of the pipe end
        riser.run.caps.1 = top;
        sp.runs.push(riser);
        sp.runs.push(fire_run(FireRun::Branch, k, dv, qv, vec![v3(x, y, zv), v3(s.valve.0, s.valve.1, zv)]));
        let l = &mut p.storeys[k].layout.fixtures;
        sp.links.push(GFireLink { storey: k, fixture: l.len(), connector: "Inlet" });
        sp.valves.push((k, l.len()));
        l.push(v);
        if k > 0 {
            holes.push((k, svc::hole(x, y, svc::od(PipeMaterial::Steel, dn))));
        }
    }
    svc::add_holes(p, &holes);
    p.fire.standpipes.push(sp);
}

/// The parts of every protected room out of the hose reach of all landing valves of its
/// storey, measured along the plan axes as a hose is laid round the corners.
fn reach(p: &mut GProject) {
    if p.fire.standpipes.is_empty() {
        return;
    }
    let lim = rule("fire.hose_reach");
    let step = rule("fire.head_spacing") / SAMPLES as f64;
    let mut gaps = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        let valves: Vec<P2> = p
            .fire
            .standpipes
            .iter()
            .flat_map(|s| s.valves.iter().filter(|v| v.0 == k).map(|v| st.layout.fixtures[v.1].at))
            .collect();
        for (i, (pr, gr)) in st.rooms.iter().zip(&st.layout.rooms).enumerate() {
            if matches!(pr.kind, RoomKind::Stair | RoomKind::Lift) || gr.polygon.len() < 3 {
                continue;
            }
            let near = |q: P2| valves.iter().any(|v| (v.0 - q.0).abs() + (v.1 - q.1).abs() <= lim);
            if let (_, Some(at)) = coverage_by(&gr.polygon, step, near) {
                gaps.push(GFireGap { storey: k, room: i, at });
            }
        }
    }
    p.fire.reach_gaps = gaps;
}
