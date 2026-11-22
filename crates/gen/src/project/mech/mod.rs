// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Mechanical design over the ventilation: room heating and cooling loads, the system the
//! rules choose, the units as parametric objects with ports, the duct network with its
//! fittings and the pressure of every path, the fans on the critical path, the water side to
//! every coil, schedules and a schematic. Opt-in by `mep.hvac.system`: at `off` nothing here
//! runs and every project stays as it was.

pub mod ducts;
pub mod equip;
pub mod geo;
pub mod loads;
pub mod model;
pub mod pipes;
pub mod sched;
pub mod select;

pub use geo::Beam;

use super::hvac::air_ports;
use super::rules::{self, rule};
use super::why_text::Args;
use super::{svc, AirSys, GProject, ProjectSpec};
use ddc_geom::mep::fittings::{motor_kw, shaft_kw};
use equip::{Equip, Kind, Medium, Port};
use ddc_geom::mep::duct_sizing::RHO;
use geo::{bbox2, centroid, inside, overlap, Bx, Solid};
use loads::{air_wh, RoomLoad};
use select::{AirSide, Choice};
use serde_json::{json, Value as J};
use std::collections::HashMap;

/// Whether the rules ask for the mechanical design.
pub fn enabled() -> bool {
    rule("mep.hvac.system") > 0.0
}

/// Room airflows the ventilation sizes with: the base demand raised to the room's outdoor air
/// and, for an all-air system, to the air that carries its sensible cooling.
#[derive(Clone, Debug, Default)]
pub struct AirPlan {
    need: HashMap<(usize, usize), f64>,
    ret: f64,
    step: f64,
}

impl AirPlan {
    pub fn new(spec: &ProjectSpec, p: &GProject) -> AirPlan {
        if !enabled() {
            return AirPlan::default();
        }
        let res = loads::residential(spec.typology);
        let ls = loads::room_loads(spec, res, p);
        let choice = select::choose(res, &ls, void(p));
        let dt = rule("mep.hvac.dt_supply");
        let mut need = HashMap::new();
        for l in &ls {
            let carry = if choice.air.all_air() { l.sensible() / (air_wh() * dt) } else { 0.0 };
            need.insert((l.storey, l.room), l.oa.max(carry));
        }
        AirPlan { need, ret: rule("mep.hvac.return_ratio"), step: rule("services.hvac.flow_step") }
    }

    /// Supply and extract of room `room` on storey `storey` from its base demand.
    pub fn room(&self, storey: usize, room: usize, base: (f64, f64)) -> (f64, f64) {
        let (qs, qe) = base;
        let Some(&need) = self.need.get(&(storey, room)) else { return base };
        if qs <= 0.0 {
            return base;
        }
        let up = |q: f64| (q / self.step).ceil() * self.step;
        let s = up(qs.max(need));
        let e = if qe > 0.0 { up(qe.max(self.ret * s)) } else { qe };
        (s, e)
    }
}

/// Least ceiling void over the storeys: storey height less the slab and the clear height.
pub fn void(p: &GProject) -> f64 {
    p.storeys.iter().map(|s| s.height - svc::slab() - rule("mep.hvac.ceiling_min")).fold(f64::MAX, f64::min).max(0.0)
}

/// A fan chosen on the critical path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fan {
    /// m3/h, Pa outside the unit, Pa inside it.
    pub flow: f64,
    pub dp_ext: f64,
    pub dp_unit: f64,
    pub shaft_kw: f64,
    pub input_kw: f64,
    pub motor_kw: f64,
}

impl Fan {
    fn new(flow: f64, dp_ext: f64, dp_unit: f64) -> Fan {
        let shaft = shaft_kw(flow, dp_ext + dp_unit, rule("mep.hvac.fan_eff"));
        Fan {
            flow,
            dp_ext,
            dp_unit,
            shaft_kw: shaft,
            input_kw: shaft / rule("mep.hvac.motor_eff"),
            motor_kw: motor_kw(shaft * (1.0 + rule("mep.hvac.motor_margin"))),
        }
    }
    pub fn dp(&self) -> f64 {
        self.dp_ext + self.dp_unit
    }
}

/// The mechanical design of a project.
#[derive(Clone, Debug, PartialEq)]
pub struct Mech {
    pub choice: Choice,
    pub loads: Vec<RoomLoad>,
    pub air: ducts::Air,
    /// The air handling unit first, then VAV boxes, fan coils, panels and radiators.
    pub equip: Vec<Equip>,
    pub cons: Vec<pipes::Consumer>,
    pub water: pipes::Hydro,
    pub fans: [Fan; 2],
    /// Fans of the dedicated units, one pair each: supply, extract.
    pub extra_fans: Vec<[Fan; 2]>,
    /// Coils of all the units, kW: cooling (sensible and latent), heating.
    pub coils: (f64, f64),
    /// Outdoor air and supply air of the unit, m3/h.
    pub oa: f64,
    pub supply: f64,
    /// Specific fan power, kW per m3/s.
    pub sfp: f64,
    /// Ducts and units in the structure, ducts through a slab outside a shaft.
    pub clashes: Vec<String>,
}

impl Mech {
    pub fn faults(&self) -> Vec<String> {
        self.air.faults.iter().chain(&self.water.faults).chain(&self.clashes).cloned().collect()
    }
    pub fn count(&self, k: Kind) -> usize {
        self.equip.iter().filter(|e| e.kind == k).count()
    }
}

/// The mechanical design of `p`, `None` when the rules do not ask for it.
pub fn design(spec: &ProjectSpec, p: &GProject) -> Option<Mech> {
    design_with(spec, p, &[])
}

/// As [`design`], with the downstand beams the ducts and pipes must pass under.
pub fn design_with(spec: &ProjectSpec, p: &GProject, beams: &[Beam]) -> Option<Mech> {
    rules::scoped(&spec.rules, || enabled().then(|| build(spec, p, beams)))
}

fn build(spec: &ProjectSpec, p: &GProject, beams: &[Beam]) -> Mech {
    let res = loads::residential(spec.typology);
    let ls = loads::room_loads(spec, res, p);
    let choice = select::choose(res, &ls, void(p));
    let solids = geo::structure(p, beams);
    let mut air = ducts::analyse(p, choice.air == AirSide::Vav, beams, &solids);
    let n = p.storeys.len();
    let roof = p.roof.elevation;
    let stub = rule("mep.hvac.stub");
    let (qs, qe) = p.hvac.flow;
    let oa_total: f64 = ls.iter().map(|l| l.oa).sum();
    let oa = if choice.air.all_air() { oa_total.min(qs) } else { qs };
    // coils of the unit
    let (ti, te, tc, ts) = (rule("mep.hvac.t_in_winter"), rule("mep.hvac.t_out_winter"), rule("mep.hvac.t_in_summer"), rule("mep.hvac.t_out_summer"));
    let hr = rule("mep.hvac.hr_eff");
    let share = if qs > 0.0 { (oa / qs).min(1.0) } else { 1.0 };
    let tn = rule("mep.hvac.t_supply_neutral");
    let mix_w = share * (te + hr * (ti - te)) + (1.0 - share) * ti;
    let mix_s = share * (ts - hr * (ts - tc)) + (1.0 - share) * tc;
    let heat_coil = (air_wh() * qs * (tn - mix_w)).max(0.0) / 1000.0;
    let leave = if choice.air.all_air() { tc - rule("mep.hvac.dt_supply") } else { tn };
    // W: kg/s of air times g/kg of moisture times kJ/kg
    let latent_oa = RHO / 3600.0 * oa * rule("mep.hvac.dx_summer") * rule("mep.hvac.hfg");
    let latent_rooms: f64 = if choice.air.all_air() { ls.iter().map(|l| l.latent).sum() } else { 0.0 };
    let cool_coil = if choice.air.cooling() {
        ((air_wh() * qs * (mix_s - leave)).max(0.0) + latent_oa + latent_rooms) / 1000.0
    } else {
        0.0
    };
    // the unit
    let (sup, ext) = equip::ahu_parts(cool_coil > 0.0, heat_coil > 0.0, hr > 0.0);
    let deck_dp = |d: &[equip::Part]| d.iter().map(|q| q.dp).sum::<f64>();
    let unit_dp = if p.hvac.plant.is_empty() { [0.0; 2] } else { [deck_dp(&sup), deck_dp(&ext)] };
    let mut equip_list: Vec<Equip> = vec![];
    let mut cons: Vec<pipes::Consumer> = vec![];
    if let Some(f) = p.hvac.plant.first() {
        let face = qs.max(qe) / 3600.0 / rule("mep.hvac.face_v");
        let deck = (face / rule("mep.hvac.ahu_aspect")).sqrt();
        let width = face / deck.max(1e-9);
        let pts = air_ports(f);
        let span = if pts.len() >= 2 { geo::dist(pts[0].1, pts[1].1) } else { 0.0 };
        let l = sup.iter().map(|q| q.len).sum::<f64>().max(ext.iter().map(|q| q.len).sum()).max(span + 2.0 * stub);
        let mut e = Equip::new(Kind::Ahu, n, (f.at.0, f.at.1, roof + f.z), f.rotation, (l, width, 2.0 * deck));
        for (name, q) in pts {
            let medium = if name == "Supply" { Medium::SupplyAir } else { Medium::ExtractAir };
            let flow = if name == "Supply" { qs } else { qe };
            e.ports.push(Port { name, medium, at: (q.0, q.1, roof + q.2), dir: (0.0, 0.0, 1.0), size: 0.0, flow });
        }
        e.ports.push(Port { name: "Outdoor", medium: Medium::OutdoorAir, at: e.local(-l / 2.0, 0.0, deck / 2.0), dir: e.dir(-1.0, 0.0, 0.0), size: 0.0, flow: oa });
        e.ports.push(Port { name: "Exhaust", medium: Medium::ExhaustAir, at: e.local(l / 2.0, 0.0, deck + deck / 2.0), dir: e.dir(1.0, 0.0, 0.0), size: 0.0, flow: oa.min(qe) });
        // water ports beside the coil sections, on the side away from the roof edge
        let mut u = -l / 2.0;
        let mut ports = [None; 4];
        for part in &sup {
            let mid = u + part.len / 2.0;
            let slot = match part.name {
                "cooling coil" => Some(0),
                "heating coil" => Some(2),
                _ => None,
            };
            if let Some(s) = slot {
                let pitch = rule("mep.hvac.port_pitch");
                ports[s] = Some(e.local(mid - pitch / 2.0, -width / 2.0 - stub, deck / 2.0));
                ports[s + 1] = Some(e.local(mid + pitch / 2.0, -width / 2.0 - stub, deck / 2.0));
            }
            u += part.len;
        }
        for (s, q) in ports.iter().enumerate() {
            if let Some(at) = q {
                e.ports.push(Port { name: pipes::SLOTS[s].name(), medium: pipes::SLOTS[s], at: *at, dir: e.dir(0.0, -1.0, 0.0), size: 0.0, flow: 0.0 });
            }
        }
        e.parts = sup.into_iter().chain(ext).collect();
        e.duty = vec![("supply_m3h", qs), ("extract_m3h", qe), ("outdoor_m3h", oa), ("cool_kw", cool_coil), ("heat_kw", heat_coil)];
        e.tag = format!("{}1", Kind::Ahu.tag());
        if cool_coil + heat_coil > 0.0 {
            cons.push(pipes::Consumer { equip: 0, storey: n, side: 0, low: false, kw: [cool_coil, heat_coil], coil_dp: rule("mep.hvac.coil_dp"), ports });
        }
        equip_list.push(e);
    }
    // dedicated outdoor air units: all outdoor air, so pre-heat, cooling and reheat coils on the
    // water side and fans for their own ducts
    let mut extra: Vec<[Fan; 2]> = vec![];
    let mut coils = (cool_coil, heat_coil);
    for (pi, f) in p.hvac.plant.iter().enumerate().skip(1) {
        let net = p.hvac.links.iter().find(|l| l.storey == n && l.fixture == pi).map_or(1, |l| l.net);
        let flow = |sys: AirSys| p.hvac.ducts.iter().filter(|d| d.storey == n && d.net == net && d.sys == sys).map(|d| d.flow).fold(0.0, f64::max);
        let (fs, fe) = (flow(AirSys::Supply), flow(AirSys::Extract));
        let mix_w = te + hr * (ti - te);
        let mix_s = ts - hr * (ts - tc);
        let latent = RHO / 3600.0 * fs * rule("mep.hvac.dx_summer") * rule("mep.hvac.hfg");
        let cool = if choice.air.cooling() { ((air_wh() * fs * (mix_s - leave)).max(0.0) + latent) / 1000.0 } else { 0.0 };
        let pre = (air_wh() * fs * (tn - mix_w)).max(0.0) / 1000.0;
        let reheat = if cool > 0.0 { (air_wh() * fs * (tn - leave)).max(0.0) / 1000.0 } else { 0.0 };
        let heat = pre + reheat;
        let (sup, ext) = equip::ahu_parts(cool > 0.0, heat > 0.0, hr > 0.0);
        let dp_net = |sys: AirSys| {
            air.terms.iter().filter(|t| t.sys == sys && t.run.is_some_and(|r| air.runs[r].net == net)).map(|t| t.path).fold(0.0, f64::max)
        };
        let deck = |d: &[equip::Part]| d.iter().map(|q| q.dp).sum::<f64>();
        extra.push([Fan::new(fs, dp_net(AirSys::Supply), deck(&sup)), Fan::new(fe, dp_net(AirSys::Extract), deck(&ext))]);
        coils = (coils.0 + cool, coils.1 + heat);
        let face = fs.max(fe) / 3600.0 / rule("mep.hvac.face_v");
        let deck_h = (face / rule("mep.hvac.ahu_aspect")).sqrt();
        let width = face / deck_h.max(1e-9);
        let pts = air_ports(f);
        let span = if pts.len() >= 2 { geo::dist(pts[0].1, pts[1].1) } else { 0.0 };
        let l = sup.iter().map(|q| q.len).sum::<f64>().max(ext.iter().map(|q| q.len).sum()).max(span + 2.0 * stub);
        let mut e = Equip::new(Kind::Ahu, n, (f.at.0, f.at.1, roof + f.z), f.rotation, (l, width, 2.0 * deck_h));
        e.net = net;
        for (name, q) in pts {
            let medium = if name == "Supply" { Medium::SupplyAir } else { Medium::ExtractAir };
            let flow = if name == "Supply" { fs } else { fe };
            e.ports.push(Port { name, medium, at: (q.0, q.1, roof + q.2), dir: (0.0, 0.0, 1.0), size: 0.0, flow });
        }
        // outdoor air in at one end, exhaust out at the other, as on the general unit
        e.ports.push(Port { name: "Outdoor", medium: Medium::OutdoorAir, at: e.local(-l / 2.0, 0.0, deck_h / 2.0), dir: e.dir(-1.0, 0.0, 0.0), size: 0.0, flow: fs });
        e.ports.push(Port { name: "Exhaust", medium: Medium::ExhaustAir, at: e.local(l / 2.0, 0.0, deck_h + deck_h / 2.0), dir: e.dir(1.0, 0.0, 0.0), size: 0.0, flow: fe.min(fs) });
        // water ports beside the coil sections, as on the general unit
        let mut u = -l / 2.0;
        let mut wports = [None; 4];
        for part in &sup {
            let mid = u + part.len / 2.0;
            let slot = match part.name {
                "cooling coil" => Some(0),
                "heating coil" => Some(2),
                _ => None,
            };
            if let Some(k) = slot {
                let pitch = rule("mep.hvac.port_pitch");
                wports[k] = Some(e.local(mid - pitch / 2.0, -width / 2.0 - stub, deck_h / 2.0));
                wports[k + 1] = Some(e.local(mid + pitch / 2.0, -width / 2.0 - stub, deck_h / 2.0));
            }
            u += part.len;
        }
        for (k, q) in wports.iter().enumerate() {
            if let Some(at) = q {
                e.ports.push(Port { name: pipes::SLOTS[k].name(), medium: pipes::SLOTS[k], at: *at, dir: e.dir(0.0, -1.0, 0.0), size: 0.0, flow: 0.0 });
            }
        }
        e.parts = sup.into_iter().chain(ext).collect();
        e.duty = vec![("supply_m3h", fs), ("extract_m3h", fe), ("outdoor_m3h", fs), ("cool_kw", cool), ("heat_kw", heat), ("reheat_kw", reheat)];
        e.tag = format!("{}{}", Kind::Ahu.tag(), net + 1);
        if cool + heat > 0.0 {
            cons.push(pipes::Consumer { equip: equip_list.len(), storey: n, side: 0, low: false, kw: [cool, heat], coil_dp: rule("mep.hvac.coil_dp"), ports: wports });
        }
        equip_list.push(e);
    }
    let fans = [Fan::new(qs, air.critical[0].dp, unit_dp[0]), Fan::new(qe, air.critical[1].dp, unit_dp[1])];
    let sfp = (fans[0].input_kw + fans[1].input_kw) / (qs.max(qe) / 3600.0).max(1e-9);
    // terminal units and their water
    let side_of = |k: usize, y: f64| {
        let sb = bbox2(&p.storeys[k].layout.slab);
        usize::from(y > (sb[1] + sb[3]) / 2.0)
    };
    let room_load = |k: usize, r: usize| ls.iter().find(|l| l.storey == k && l.room == r);
    let boxes = std::mem::take(&mut air.boxes);
    let mut box_room_flow: HashMap<(usize, usize), f64> = HashMap::new();
    for b in &boxes {
        if let Some(r) = b.room {
            *box_room_flow.entry((b.storey, r)).or_default() += b.duty("flow_m3h");
        }
    }
    for (bi, mut b) in boxes.into_iter().enumerate() {
        b.tag = format!("{}-{}.{}", Kind::VavBox.tag(), b.storey + 1, bi + 1);
        let heat = b.room.and_then(|r| room_load(b.storey, r)).map_or(0.0, |l| {
            let part = b.duty("flow_m3h") / box_room_flow.get(&(b.storey, l.room)).copied().unwrap_or(1.0).max(1e-9);
            l.heating() * part
        });
        let reheat = (heat + air_wh() * b.duty("min_m3h") * (ti - tn).max(0.0)) / 1000.0;
        b.duty.push(("reheat_kw", reheat));
        let bb = b.bbox();
        let side = side_of(b.storey, (bb[1] + bb[4]) / 2.0);
        let y = if side == 1 { bb[4] + stub } else { bb[1] - stub };
        let base = ((bb[0] + bb[3]) / 2.0, y, (bb[2] + bb[5]) / 2.0);
        let ports = pipes::ports_along_x(base, [false, false, true, true]);
        cons.push(pipes::Consumer { equip: equip_list.len(), storey: b.storey, side, low: false, kw: [0.0, reheat], coil_dp: rule("mep.hvac.coil_dp"), ports });
        equip_list.push(b);
    }
    let duct_boxes: Vec<(Bx, String)> = air
        .runs
        .iter()
        .flat_map(|r| r.boxes().into_iter().map(move |b| (b, format!("{:?} duct {}", r.sys, r.duct))))
        .collect();
    units(p, &choice, &ls, &duct_boxes, &solids, &mut equip_list, &mut cons, &mut air.faults);
    let mut obst: Vec<(Bx, String)> = solids.iter().map(|s| (s.b, s.what.clone())).collect();
    obst.extend(duct_boxes.iter().cloned());
    let water = pipes::plan(p, &cons, &equip_list, &obst, choice.plant);
    let clashes = clashes(p, &air, &equip_list, &solids);
    Mech {
        choice,
        loads: ls,
        air,
        equip: equip_list,
        cons,
        water,
        fans,
        extra_fans: extra,
        coils,
        oa,
        supply: qs,
        sfp,
        clashes,
    }
}

/// Fan coils, chilled panels or radiators in each room the system serves, with their water.
#[allow(clippy::too_many_arguments)]
fn units(
    p: &GProject,
    choice: &Choice,
    ls: &[RoomLoad],
    ducts: &[(Bx, String)],
    solids: &[Solid],
    out: &mut Vec<Equip>,
    cons: &mut Vec<pipes::Consumer>,
    faults: &mut Vec<String>,
) {
    let gap = rule("services.gap");
    let stub = rule("mep.hvac.stub");
    let side_of = |k: usize, y: f64| {
        let sb = bbox2(&p.storeys[k].layout.slab);
        usize::from(y > (sb[1] + sb[3]) / 2.0)
    };
    let free = |b: &Bx, out: &[Equip]| {
        !ducts.iter().any(|d| overlap(b, &d.0, gap)) && !solids.iter().any(|s| overlap(b, &s.b, gap)) && !out.iter().any(|e| overlap(b, &e.bbox(), gap))
    };
    for l in ls.iter().filter(|l| l.kind.habitable()) {
        let k = l.storey;
        let rb = loads::room_box(p, k, l.room);
        let poly = &p.storeys[k].layout.rooms[l.room].polygon;
        let ceil = geo::ceiling(p, k);
        let top = geo::soffit(p, k) - gap;
        let along_x = rb[2] - rb[0] >= rb[3] - rb[1];
        let side = side_of(k, (rb[1] + rb[3]) / 2.0);
        match choice.air {
            AirSide::FcuDoas => {
                let kw = l.sensible() / 1000.0;
                if kw <= 0.0 {
                    continue;
                }
                let sizes = equip::fcu_sizes();
                let big = sizes[sizes.len() - 1].cap;
                let count = (kw / big).ceil().max(1.0) as usize;
                let s = equip::pick(&sizes, kw / count as f64);
                if ceil + s.h > top {
                    faults.push(format!("storey {k} room {}: no void for a {} fan coil", l.number, s.ty));
                    continue;
                }
                let step = rule("mep.hvac.unit_step");
                for j in 0..count {
                    let t = (j as f64 + 0.5) / count as f64;
                    let c0 = if along_x { (rb[0] + t * (rb[2] - rb[0]), (rb[1] + rb[3]) / 2.0) } else { ((rb[0] + rb[2]) / 2.0, rb[1] + t * (rb[3] - rb[1])) };
                    let mut placed = None;
                    // nearest clear spot to the even spacing inside the room
                    'find: for m in 0..rule("mep.hvac.unit_tries") as usize {
                        for sg in [1.0, -1.0] {
                            let d = sg * step * m as f64;
                            let c = if along_x { (c0.0 + d, c0.1) } else { (c0.0, c0.1 + d) };
                            let e = Equip::new(Kind::FanCoil, k, (c.0, c.1, ceil), 0.0, (s.l, s.w, s.h));
                            let bb = e.bbox();
                            let inside_room = [(bb[0], bb[1]), (bb[3], bb[1]), (bb[3], bb[4]), (bb[0], bb[4])].iter().all(|q| inside(*q, poly));
                            if inside_room && free(&bb, out) {
                                placed = Some(e);
                                break 'find;
                            }
                        }
                    }
                    let Some(mut e) = placed else {
                        faults.push(format!("storey {k} room {}: no clear place for fan coil {}", l.number, j + 1));
                        continue;
                    };
                    let per = [kw / count as f64, l.heating() / 1000.0 / count as f64];
                    e.duty = vec![("cool_kw", per[0]), ("heat_kw", per[1]), ("cap_kw", s.cap)];
                    e.room = Some(l.room);
                    e.tag = format!("{}-{}.{}", Kind::FanCoil.tag(), k + 1, out.len());
                    let bb = e.bbox();
                    let y = if side == 1 { bb[4] + stub } else { bb[1] - stub };
                    let ports = pipes::ports_along_x(((bb[0] + bb[3]) / 2.0, y, ceil + s.h / 2.0), [true, true, per[1] > 0.0, per[1] > 0.0]);
                    for (si, q) in ports.iter().enumerate() {
                        if let Some(at) = q {
                            e.ports.push(Port { name: pipes::SLOTS[si].name(), medium: pipes::SLOTS[si], at: *at, dir: (0.0, if side == 1 { 1.0 } else { -1.0 }, 0.0), size: s.d, flow: 0.0 });
                        }
                    }
                    cons.push(pipes::Consumer { equip: out.len(), storey: k, side, low: false, kw: per, coil_dp: rule("mep.hvac.coil_dp"), ports });
                    out.push(e);
                }
            }
            AirSide::RadiantDoas => {
                let kw = l.sensible() / 1000.0;
                let edge = rule("mep.hvac.panel_edge");
                let (w, d) = (rb[2] - rb[0] - 2.0 * edge, rb[3] - rb[1] - 2.0 * edge);
                if kw <= 0.0 || w <= 0.0 || d <= 0.0 {
                    continue;
                }
                let need = kw * 1000.0 / rule("mep.hvac.radiant_max");
                let area = need.min(w * d * rule("mep.hvac.radiant_cover"));
                let cap = area * rule("mep.hvac.radiant_max") / 1000.0;
                if cap + 1e-9 < kw {
                    faults.push(format!("storey {k} room {}: panels carry {cap:.2} of {kw:.2} kW", l.number));
                }
                let (pl, pw) = if along_x { (area / d, d) } else { (w, area / w) };
                let c = centroid(poly);
                let t = rule("mep.hvac.panel_t");
                let mut e = Equip::new(Kind::RadiantPanel, k, (c.0, c.1, ceil), 0.0, (pl, pw, t));
                let heat = l.heating() / 1000.0;
                e.duty = vec![("cool_kw", kw), ("heat_kw", heat), ("area_m2", area)];
                e.room = Some(l.room);
                e.tag = format!("{}-{}.{}", Kind::RadiantPanel.tag(), k + 1, out.len());
                let bb = e.bbox();
                let y = if side == 1 { bb[4] } else { bb[1] };
                let ports = pipes::ports_along_x(((bb[0] + bb[3]) / 2.0, y, ceil + t), [true, true, heat > 0.0, heat > 0.0]);
                cons.push(pipes::Consumer { equip: out.len(), storey: k, side, low: false, kw: [kw, heat], coil_dp: rule("mep.hvac.coil_dp"), ports });
                out.push(e);
            }
            AirSide::Radiators => {
                let kw = l.heating() / 1000.0;
                if kw <= 0.0 {
                    continue;
                }
                let sb = bbox2(&p.storeys[k].layout.slab);
                let wall = crate::WallRole::Exterior.thickness();
                let face = if side == 1 { sb[3] - wall } else { sb[1] + wall };
                let (h, d) = (rule("mep.hvac.rad_h"), rule("mep.hvac.rad_d"));
                let len = (kw * 1000.0 / rule("mep.hvac.rad_w_m")).clamp(rule("mep.hvac.rad_min_l"), (rb[2] - rb[0] - 2.0 * rule("mep.hvac.panel_edge")).max(rule("mep.hvac.rad_min_l")));
                let off = rule("mep.hvac.rad_off") + d / 2.0;
                let y = if side == 1 { face - off } else { face + off };
                let z = p.storeys[k].elevation + rule("mep.hvac.rad_z");
                let mut e = Equip::new(Kind::Radiator, k, ((rb[0] + rb[2]) / 2.0, y, z), 0.0, (len, d, h));
                e.duty = vec![("heat_kw", kw), ("len_m", len)];
                e.room = Some(l.room);
                e.tag = format!("{}-{}.{}", Kind::Radiator.tag(), k + 1, out.len());
                let ports = [None, None, Some(e.local(-len / 2.0, 0.0, 0.0)), Some(e.local(len / 2.0, 0.0, 0.0))];
                cons.push(pipes::Consumer { equip: out.len(), storey: k, side, low: true, kw: [0.0, kw], coil_dp: rule("mep.hvac.coil_dp"), ports });
                out.push(e);
            }
            AirSide::Vav => {}
        }
    }
}

/// Ducts against the structure and through slabs outside a shaft; units against the structure.
fn clashes(p: &GProject, air: &ducts::Air, equip: &[Equip], solids: &[Solid]) -> Vec<String> {
    let mut out = vec![];
    let gap = rule("services.gap");
    for r in &air.runs {
        for b in r.boxes() {
            if let Some(s) = solids.iter().find(|s| overlap(&b, &s.b, 0.0)) {
                out.push(format!("{:?} duct {} hits {}", r.sys, r.duct, s.what));
            }
        }
        for w in r.points.windows(2) {
            let (lo, hi) = (w[0].2.min(w[1].2), w[0].2.max(w[1].2));
            for z in p.storeys.iter().skip(1).map(|s| s.elevation).chain([p.roof.elevation]) {
                if lo < z - svc::slab() + 1e-6 && hi > z - 1e-6 && !slab_open(p, (w[0].0, w[0].1), z) {
                    out.push(format!("{:?} duct {} passes the slab at {z:.2} outside a shaft", r.sys, r.duct));
                }
            }
        }
    }
    for e in equip.iter().filter(|e| e.kind != Kind::RadiantPanel) {
        if let Some(s) = solids.iter().find(|s| overlap(&e.bbox(), &s.b, -gap)) {
            out.push(format!("{} {} hits {}", e.kind.name(), e.tag, s.what));
        }
    }
    out
}

/// A slab at `z` is open at `q`: a shaft or slab hole of the storey at that level, or of the
/// top storey for the roof.
fn slab_open(p: &GProject, q: (f64, f64), z: f64) -> bool {
    if geo::slab_passable(p, q, z) {
        return true;
    }
    let top = p.storeys.last();
    (z - p.roof.elevation).abs() < 1e-3 && top.is_some_and(|st| st.layout.shafts.iter().any(|s| inside(q, &s.ring)))
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Numbers of the design for `p.metrics`-style reports.
pub fn metrics(m: &Mech) -> Vec<(&'static str, f64)> {
    let fits = m.air.fittings.len() as f64;
    vec![
        ("mech_heat_kw", r1(m.loads.iter().map(|l| l.heating()).sum::<f64>() / 1000.0)),
        ("mech_cool_kw", r1(m.loads.iter().map(|l| l.sensible() + l.latent).sum::<f64>() / 1000.0)),
        ("mech_oa_m3h", m.oa.round()),
        ("mech_supply_m3h", m.supply.round()),
        ("mech_vav_boxes", m.count(Kind::VavBox) as f64),
        ("mech_fan_coils", m.count(Kind::FanCoil) as f64),
        ("mech_duct_fittings", fits),
        ("mech_crit_supply_pa", m.air.critical[0].dp.round()),
        ("mech_crit_extract_pa", m.air.critical[1].dp.round()),
        ("mech_fan_supply_kw", m.fans[0].motor_kw),
        ("mech_sfp", (m.sfp * 100.0).round() / 100.0),
        ("mech_pipe_m", r1(m.water.pipes.iter().map(|p| p.len).sum())),
        ("mech_pumps", m.water.pumps.len() as f64),
        ("mech_faults", m.faults().len() as f64),
    ]
}

/// The reasons of the design for `why`: (rule, value, key, args).
pub fn whys(spec: &ProjectSpec, p: &GProject) -> Vec<(&'static str, f64, &'static str, Args)> {
    let Some(m) = design(spec, p) else { return vec![] };
    rules::scoped(&spec.rules, || reasons(&m))
}

fn reasons(m: &Mech) -> Vec<(&'static str, f64, &'static str, Args)> {
    let s = |x: f64| r1(x).to_string();
    let c = &m.choice;
    let heat: f64 = m.loads.iter().map(|l| l.heating()).sum::<f64>() / 1000.0;
    let cool: f64 = m.loads.iter().map(|l| l.sensible() + l.latent).sum::<f64>() / 1000.0;
    let mut out: Vec<(&'static str, f64, &'static str, Args)> = vec![
        ("mep.hvac.t_out_winter", rule("mep.hvac.t_out_winter"), "hvac_loads", vec![
            ("n", m.loads.len().to_string()), ("heat", s(heat)), ("te", s(rule("mep.hvac.t_out_winter"))),
            ("cool", s(cool)), ("ts", s(rule("mep.hvac.t_out_summer"))),
        ]),
        ("mep.hvac.u_wall", rule("mep.hvac.u_wall"), "hvac_envelope", vec![
            ("w", s100(rule("mep.hvac.u_wall"))), ("g", s100(rule("mep.hvac.u_window"))), ("r", s100(rule("mep.hvac.u_roof"))),
        ]),
        ("mep.hvac.oa_person", rule("mep.hvac.oa_person"), "hvac_oa", vec![
            ("q", m.oa.round().to_string()), ("pp", s(rule("mep.hvac.oa_person"))), ("pa", s(rule("mep.hvac.oa_area"))),
        ]),
        ("mep.hvac.system", rule("mep.hvac.system"), c.key, vec![
            ("sys", c.air.name().to_string()), ("peak", s(c.peak)), ("void", s100(c.void)),
        ]),
        ("mep.hvac.plant", rule("mep.hvac.plant"), "hvac_plant", vec![
            ("plant", c.plant.name().to_string()), ("heat", s(c.heat_kw)), ("cool", s(m.cons.iter().map(|q| q.kw[0]).sum::<f64>())),
        ]),
    ];
    if c.air.all_air() {
        out.push(("mep.hvac.dt_supply", rule("mep.hvac.dt_supply"), "hvac_supply", vec![
            ("q", m.supply.round().to_string()), ("dt", s(rule("mep.hvac.dt_supply"))), ("n", m.count(Kind::VavBox).to_string()),
        ]));
    }
    let over = m.air.runs.iter().filter(|r| r.v > r.v_max + 1e-9).count();
    out.push(("services.hvac.v_main", rule("services.hvac.v_main"), "hvac_ducts", vec![
        ("n", m.air.runs.len().to_string()), ("fit", m.air.fittings.len().to_string()), ("over", over.to_string()),
    ]));
    let tag = m.air.critical[0].term.map(|t| m.air.terms[t].fixture.to_string()).unwrap_or_default();
    out.push(("mep.hvac.fan_eff", rule("mep.hvac.fan_eff"), "hvac_fan", vec![
        ("dp", m.fans[0].dp().round().to_string()), ("ext", m.air.critical[0].dp.round().to_string()), ("t", tag),
        ("kw", s(m.fans[0].motor_kw)), ("sfp", s100(m.sfp)), ("max", s100(rule("mep.hvac.sfp_max"))),
    ]));
    if !m.water.pipes.is_empty() {
        out.push(("mep.hvac.pipe_r_max", rule("mep.hvac.pipe_r_max"), "hvac_pipes", vec![
            ("n", m.water.pipes.len().to_string()), ("chw", s(m.water.flow[0])), ("hw", s(m.water.flow[1])),
            ("h1", s(m.water.index[0] / 1000.0)), ("h2", s(m.water.index[1] / 1000.0)), ("r", s(rule("mep.hvac.pipe_r_max"))),
        ]));
    }
    let low = m.air.lowered.iter().copied().fold(0.0, f64::max);
    if low > 0.0 {
        out.push(("mep.hvac.beam_clear", rule("mep.hvac.beam_clear"), "hvac_beams", vec![("d", s100(low))]));
    }
    out
}

fn s100(x: f64) -> String {
    ((x * 100.0).round() / 100.0).to_string()
}

/// The design as data for the tool reply and the UI: loads, units, schedules and the schematic.
pub fn to_json(m: &Mech) -> J {
    let rows = |t: Vec<Vec<String>>| J::from(t.into_iter().map(J::from).collect::<Vec<_>>());
    json!({
        "system": m.choice.air.name(),
        "plant": m.choice.plant.name(),
        "metrics": metrics(m).into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
        "loads": rows(sched::loads(m)),
        "equipment": rows(sched::equipment(m)),
        "ducts": rows(sched::ducts(m)),
        "pipes": rows(sched::pipes(m)),
        "schematic": sched::schematic(m),
        "faults": m.faults(),
    })
}

/// Supply terminals on the network that are not connected to the unit.
pub fn unconnected(m: &Mech) -> usize {
    m.air.terms.iter().filter(|t| t.run.is_none() && t.sys == AirSys::Supply).count()
}
