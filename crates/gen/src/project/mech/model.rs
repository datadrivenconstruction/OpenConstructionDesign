// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The mechanical design as model content: a parametric family per kind of unit and port set,
//! a sized type per unit with its ports where the design put them, the ducts and pipes cut at
//! the units that sit inline on them, and which network each port joins.

use super::equip::{Equip, Kind, Medium, Port};
use super::geo::{dist, P3};
use super::pipes::SLOTS;
use super::Mech;
use crate::project::{AirSys, GProject};
use std::collections::{BTreeMap, HashMap};

/// A port lies on a run axis or end within this, m.
const TOL: f64 = 2e-3;

/// The network a port or a run belongs to: supply or extract air, or a water slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Net {
    Air(usize),
    Water(usize),
}

impl Net {
    /// The network of a port of a unit of ventilation system `unit`: air systems count two a
    /// system, supply then extract.
    fn of_unit(m: Medium, unit: u8) -> Option<Net> {
        match Net::of(m)? {
            Net::Air(s) => Some(Net::Air(s + 2 * usize::from(unit))),
            w => Some(w),
        }
    }

    fn of(m: Medium) -> Option<Net> {
        match m {
            Medium::SupplyAir => Some(Net::Air(0)),
            Medium::ExtractAir => Some(Net::Air(1)),
            Medium::OutdoorAir | Medium::ExhaustAir => None,
            w => SLOTS.iter().position(|s| *s == w).map(Net::Water),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelFamily {
    pub name: String,
    pub def: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelUnit {
    pub kind: Kind,
    pub tag: String,
    /// `storeys.len()` for the roof.
    pub storey: usize,
    pub family: String,
    pub ty: String,
    /// Type values in SI units.
    pub values: Vec<(String, f64)>,
    /// Centre of the casing's base, project coordinates.
    pub at: P3,
    pub rot: f64,
    /// Ports a run reaches and the network each joins.
    pub links: Vec<(&'static str, Net)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRun {
    pub net: Net,
    pub storey: usize,
    pub points: Vec<P3>,
    /// Duct width and height (0 for round); pipes carry their DN instead.
    pub size: (f64, f64),
    pub dn: u32,
    pub bend: Option<f64>,
    pub insulation: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Content {
    pub families: Vec<ModelFamily>,
    /// The air handling unit first when there is one.
    pub units: Vec<ModelUnit>,
    pub ducts: Vec<ModelRun>,
    pub pipes: Vec<ModelRun>,
    /// Ports of the design no run reaches, as `tag port`.
    pub loose: Vec<String>,
}

fn title(k: Kind) -> &'static str {
    match k {
        Kind::Ahu => "Air Handling Unit",
        Kind::VavBox => "VAV Box",
        Kind::FanCoil => "Fan Coil Unit",
        Kind::RadiantPanel => "Radiant Panel",
        Kind::Radiator => "Radiator",
        Kind::Chiller => "Chiller",
        Kind::Boiler => "Boiler",
        Kind::HeatPump => "Heat Pump",
        Kind::Pump => "Pump",
        Kind::FireDamper => "Fire Damper",
        Kind::VolumeDamper => "Volume Damper",
        Kind::Silencer => "Duct Silencer",
    }
}

fn ifc(k: Kind) -> &'static str {
    match k {
        Kind::Ahu => "IfcUnitaryEquipment AIRHANDLER",
        Kind::VavBox => "IfcAirTerminalBox VARIABLEFLOWPRESSUREDEPENDANT",
        Kind::FanCoil => "IfcUnitaryEquipment AIRCONDITIONINGUNIT",
        Kind::RadiantPanel => "IfcSpaceHeater NOTDEFINED",
        Kind::Radiator => "IfcSpaceHeater RADIATOR",
        Kind::Chiller => "IfcChiller AIRCOOLED",
        Kind::Boiler => "IfcBoiler WATER",
        Kind::HeatPump => "IfcUnitaryEquipment NOTDEFINED",
        Kind::Pump => "IfcPump CIRCULATOR",
        Kind::FireDamper => "IfcDamper FIREDAMPER",
        Kind::VolumeDamper => "IfcDamper BALANCINGDAMPER",
        Kind::Silencer => "IfcDuctSilencer RECTANGULAR",
    }
}

/// Connector words of a medium: domain, then the system kind when the port has one.
fn conn_words(m: Medium) -> &'static str {
    match m {
        Medium::SupplyAir => "duct supply_air",
        Medium::ExtractAir => "duct exhaust_air",
        Medium::OutdoorAir | Medium::ExhaustAir => "duct",
        Medium::ChwFlow | Medium::ChwReturn => "pipe chilled_water",
        Medium::HwFlow | Medium::HwReturn => "pipe heating",
    }
}

fn is_source(k: Kind) -> bool {
    matches!(k, Kind::Ahu | Kind::Chiller | Kind::Boiler | Kind::HeatPump)
}

/// The family text of a kind with these connectors: casing and ports driven by type values.
fn family_def(name: &str, k: Kind, ports: &[(&'static str, Medium)]) -> String {
    let mut s = format!("family {name}\ncategory equipment\nhost none\nifc {}\n", ifc(k));
    for p in ["Length", "Width", "Height"] {
        s += &format!("param {p} length type 500mm\n");
    }
    s += "param Source number type 0\nparam Casing material type \"Steel White\"\n";
    for (i, (pn, _)) in ports.iter().enumerate() {
        let i = i + 1;
        for a in ["X", "Y", "Z"] {
            s += &format!("param P{i}{a} length type 0mm\n");
        }
        s += &format!("param D{i}X number type 1\nparam D{i}Y number type 0\nparam D{i}Z number type 0\n");
        s += &format!("param S{i} length type 50mm\nparam Flow{pn} number type 0\n");
    }
    s += "type \"Default\" Length=500mm\n";
    s += "extrude Body rect(-Length / 2, -Width / 2, Length / 2, Width / 2) z(0mm, Height) mat(Casing) show(3d, elev)\n";
    s += "symbol rect(-Length / 2, -Width / 2, Length / 2, Width / 2)\n";
    s += "symbol line(-Length / 2, -Width / 2, Length / 2, Width / 2) show(medium, fine)\n";
    for (i, (pn, m)) in ports.iter().enumerate() {
        let i = i + 1;
        s += &format!("connector {pn} {} at(P{i}X, P{i}Y, P{i}Z) dir(D{i}X, D{i}Y, D{i}Z) size(S{i})\n", conn_words(*m));
    }
    s
}

/// Every port of a unit: its own, then the water ports its coil takes.
fn ports_of(m: &Mech, e: &Equip, i: usize) -> Vec<Port> {
    let mut out = e.ports.clone();
    for c in m.cons.iter().filter(|c| c.equip == i) {
        for (s, q) in c.ports.iter().enumerate() {
            let Some(at) = q else { continue };
            if out.iter().any(|p| p.medium == SLOTS[s]) {
                continue;
            }
            // outward: the larger plan offset from the casing centre
            let (dx, dy) = (at.0 - e.at.0, at.1 - e.at.1);
            let dir = if dx.abs() >= dy.abs() { (dx.signum(), 0.0, 0.0) } else { (0.0, dy.signum(), 0.0) };
            out.push(Port { name: SLOTS[s].name(), medium: SLOTS[s], at: *at, dir, size: 0.0, flow: 0.0 });
        }
    }
    out
}

/// Arc length along a polyline of the point on it nearest `q`, when within the tolerance.
fn along(pts: &[P3], q: P3) -> Option<f64> {
    let mut s0 = 0.0;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let l = dist(a, b);
        if l > 0.0 {
            let t = (((q.0 - a.0) * (b.0 - a.0) + (q.1 - a.1) * (b.1 - a.1) + (q.2 - a.2) * (b.2 - a.2)) / (l * l)).clamp(0.0, 1.0);
            let f = (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1), a.2 + t * (b.2 - a.2));
            if dist(f, q) <= TOL {
                return Some(s0 + t * l);
            }
        }
        s0 += l;
    }
    None
}

fn at_length(pts: &[P3], s: f64) -> P3 {
    let mut s0 = 0.0;
    for w in pts.windows(2) {
        let l = dist(w[0], w[1]);
        if s <= s0 + l && l > 0.0 {
            let t = ((s - s0) / l).clamp(0.0, 1.0);
            return (w[0].0 + t * (w[1].0 - w[0].0), w[0].1 + t * (w[1].1 - w[0].1), w[0].2 + t * (w[1].2 - w[0].2));
        }
        s0 += l;
    }
    *pts.last().expect("a run has points")
}

/// The polyline between arc lengths `a` and `b`.
fn piece(pts: &[P3], a: f64, b: f64) -> Vec<P3> {
    let mut out = vec![at_length(pts, a)];
    let mut s0 = 0.0;
    for w in pts.windows(2) {
        s0 += dist(w[0], w[1]);
        if s0 > a + 1e-9 && s0 < b - 1e-9 {
            out.push(w[1]);
        }
    }
    out.push(at_length(pts, b));
    out
}

/// A polyline cut where inline units sit: each unit's two ports on it leave the gap it fills.
fn cut(pts: &[P3], gaps: &[(f64, f64)]) -> Vec<Vec<P3>> {
    let total: f64 = pts.windows(2).map(|w| dist(w[0], w[1])).sum();
    let mut g = gaps.to_vec();
    g.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out = vec![];
    let mut from = 0.0;
    for (a, b) in g.into_iter().chain([(total, total)]) {
        if a - from > TOL {
            out.push(piece(pts, from, a));
        }
        from = b;
    }
    out
}

/// Inline units on these runs: per run, the gaps their port pairs leave.
fn inline(runs: &[(Net, &[P3])], units: &[(&Equip, Vec<Port>)]) -> HashMap<usize, Vec<(f64, f64)>> {
    let mut gaps: HashMap<usize, Vec<(f64, f64)>> = HashMap::new();
    for (e, ports) in units {
        let pair: Vec<&Port> = ports.iter().filter(|p| matches!(p.name, "Inlet" | "Outlet" | "Suction" | "Discharge")).collect();
        if pair.len() != 2 {
            continue;
        }
        for (ri, (net, pts)) in runs.iter().enumerate() {
            if Net::of_unit(pair[0].medium, e.net) != Some(*net) {
                continue;
            }
            if let (Some(a), Some(b)) = (along(pts, pair[0].at), along(pts, pair[1].at)) {
                gaps.entry(ri).or_default().push((a.min(b), a.max(b)));
                break;
            }
        }
    }
    gaps
}

fn mm(x: f64) -> f64 {
    (x * 1000.0).round()
}

fn fnv(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The model content of the design `m` of `p`.
pub fn content(p: &GProject, m: &Mech) -> Content {
    let mut out = Content::default();
    // every unit with its ports, the air handling unit first
    let mut all: Vec<(&Equip, Vec<Port>)> = vec![];
    for (i, e) in m.equip.iter().enumerate() {
        all.push((e, ports_of(m, e, i)));
    }
    for e in m.water.plant.iter().chain(&m.water.pumps) {
        all.push((e, e.ports.clone()));
    }
    // ducts and pipes cut at the units inline on them
    let air: Vec<(Net, &[P3])> = m.air.runs.iter().map(|r| (Net::Air(usize::from(r.net) * 2 + usize::from(r.sys == AirSys::Extract)), r.points.as_slice())).collect();
    let air_gaps = inline(&air, &all);
    for (ri, r) in m.air.runs.iter().enumerate() {
        let none = vec![];
        for pts in cut(&r.points, air_gaps.get(&ri).unwrap_or(&none)) {
            out.ducts.push(ModelRun { net: air[ri].0, storey: r.storey, points: pts, size: r.size, dn: 0, bend: r.bend, insulation: r.insulation });
        }
    }
    // ducts the unit does not reach stay as the ventilation drew them
    for (_, d) in p.hvac.ducts.iter().enumerate().filter(|(i, _)| !m.air.runs.iter().any(|r| r.duct == *i)) {
        out.ducts.push(ModelRun { net: Net::Air(usize::from(d.net) * 2 + usize::from(d.sys == AirSys::Extract)), storey: d.storey, points: d.points.clone(), size: d.size, dn: 0, bend: d.bend, insulation: 0.0 });
    }
    let water: Vec<(Net, &[P3])> = m.water.pipes.iter().map(|p| (Net::Water(p.slot), p.points.as_slice())).collect();
    let pipe_gaps = inline(&water, &all);
    for (pi, p) in m.water.pipes.iter().enumerate() {
        let none = vec![];
        for pts in cut(&p.points, pipe_gaps.get(&pi).unwrap_or(&none)) {
            out.pipes.push(ModelRun { net: Net::Water(p.slot), storey: p.storey, points: pts, size: (f64::from(p.dn) / 1000.0, 0.0), dn: p.dn, bend: None, insulation: p.ins });
        }
    }
    // the run end each port meets, its size and the flow it carries
    let ends = |runs: &[ModelRun], net: Net, q: P3| {
        runs.iter().find(|r| r.net == net && (dist(r.points[0], q) <= TOL || dist(*r.points.last().expect("points"), q) <= TOL)).map(|r| r.size.0)
    };
    let pipe_q = |slot: usize, q: P3| {
        m.water.pipes.iter().find(|p| p.slot == slot && p.points.iter().any(|x| dist(*x, q) <= TOL)).map_or(0.0, |p| p.q)
    };
    let mut fams: BTreeMap<String, Vec<(&'static str, Medium)>> = BTreeMap::new();
    for (e, ports) in &all {
        let sig: Vec<(&'static str, Medium)> = ports.iter().map(|p| (p.name, p.medium)).collect();
        let water: Vec<&str> = [("CHW", 0), ("HW", 2)].into_iter().filter(|(_, s)| sig.iter().any(|p| p.1 == SLOTS[*s])).map(|x| x.0).collect();
        let base = if water.is_empty() { format!("HVAC {}", title(e.kind)) } else { format!("HVAC {} {}", title(e.kind), water.join(" ")) };
        // a kind whose port set differs takes the next free name
        let mut name = base.clone();
        let mut k = 2;
        while fams.get(&name).is_some_and(|s| *s != sig) {
            name = format!("{base} {k}");
            k += 1;
        }
        fams.entry(name.clone()).or_insert_with(|| sig.clone());
        let (c, s) = (e.rot.cos(), e.rot.sin());
        let mut values = vec![("Length".to_string(), e.size.0), ("Width".to_string(), e.size.1), ("Height".to_string(), e.size.2)];
        values.push(("Source".to_string(), if is_source(e.kind) { 1.0 } else { 0.0 }));
        let mut links = vec![];
        for (i, p) in ports.iter().enumerate() {
            let i = i + 1;
            let (dx, dy, dz) = (p.at.0 - e.at.0, p.at.1 - e.at.1, p.at.2 - e.at.2);
            let local = (c * dx + s * dy, -s * dx + c * dy, dz);
            let dir = (c * p.dir.0 + s * p.dir.1, -s * p.dir.0 + c * p.dir.1, p.dir.2);
            let net = Net::of_unit(p.medium, e.net);
            let reach = net.and_then(|n| match n {
                Net::Air(_) => ends(&out.ducts, n, p.at),
                Net::Water(_) => ends(&out.pipes, n, p.at),
            });
            let size = reach.filter(|d| *d > 0.0).unwrap_or(if p.size > 0.0 { p.size } else { 0.05 });
            let flow = match net {
                _ if matches!(e.kind, Kind::VavBox | Kind::Pump) && matches!(p.name, "Inlet" | "Outlet" | "Suction" | "Discharge") => 0.0,
                Some(Net::Water(slot)) => pipe_q(slot, p.at) / 3.6,
                _ => p.flow,
            };
            for (a, v) in [("X", local.0), ("Y", local.1), ("Z", local.2)] {
                values.push((format!("P{i}{a}"), v));
            }
            for (a, v) in [("X", dir.0), ("Y", dir.1), ("Z", dir.2)] {
                values.push((format!("D{i}{a}"), v));
            }
            values.push((format!("S{i}"), size));
            values.push((format!("Flow{}", p.name), flow));
            match (net, reach) {
                (Some(n), Some(_)) => links.push((p.name, n)),
                (Some(_), None) => out.loose.push(format!("{} {}", e.tag, p.name)),
                _ => {}
            }
        }
        let key: String = values.iter().map(|(k, v)| format!("{k}={:.4};", v)).collect();
        let ty = format!("{} {}x{}x{} {:06x}", e.kind.tag(), mm(e.size.0), mm(e.size.1), mm(e.size.2), fnv(&key) & 0xff_ffff);
        out.units.push(ModelUnit { kind: e.kind, tag: e.tag.clone(), storey: e.storey, family: name, ty, values, at: e.at, rot: e.rot, links });
    }
    out.families = fams.into_iter().map(|(name, sig)| {
        let kind = out.units.iter().find(|u| u.family == name).map_or(Kind::Ahu, |u| u.kind);
        ModelFamily { def: family_def(&name, kind, &sig), name }
    }).collect();
    out
}


/// Names of the water systems by slot: chilled water flow and return, heating flow and return.
pub fn water_names(lang: &str) -> [String; 4] {
    ["CHWS", "CHWR", "HWS", "HWR"].map(|en| super::super::names::tr(lang, "", en))
}
