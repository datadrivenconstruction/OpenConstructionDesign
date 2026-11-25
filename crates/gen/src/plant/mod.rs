// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Process plants from a brief: a boiler house, a pumping station, water treatment or a small
//! process line; a standalone storage tank is built on its own (`tank`). The result is a
//! P&ID-level graph (equipment with nozzles; lines with service, size and spec; valves and
//! instruments on them) sized from the duty, laid out in two rows with maintenance
//! clearances, platforms, ladders and stairs, and piped orthogonally between the nozzles over
//! a rack, with fittings, supports, schedules and isometric legs. Every number
//! a decision rests on is a `plant.*` rule; [`why`] says which rule made which decision.

mod brief;
mod check;
mod equip;
mod layout;
mod model;
mod route;
mod sched;
mod size;
mod tank;
mod topo;
pub mod why;

use ddc_geom::math::{v3, V3};
use serde_json::{json, Value as J};

use crate::project::rules::{self, Layer, RuleSet};
use crate::project::why::Why;

pub use brief::{Brief, Kind};
pub use check::Report;
pub use layout::{Access, Bund, Flue, Hall, Platform, Steel, SteelKind, Zone, STEEL_R};
pub use model::{system_kind, Kit, KitComp, KitFamily, KitFooting, KitMember, KitRun, KitWall};
pub use route::{FitKind, Fitting, Piping, Placed, Seg, Support, SupportKind};
pub use sched::{LineRow, Schedules, Totals};
pub use size::{ItemKind, Spec};

/// What a line carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Svc {
    Hws,
    Hwr,
    Ng,
    Mw,
    Rw,
    Tw,
    Bw,
    Ch,
    Pr,
    Cws,
    Cwr,
    Dr,
}

impl Svc {
    pub const ALL: [Svc; 12] = [
        Svc::Hws,
        Svc::Hwr,
        Svc::Ng,
        Svc::Mw,
        Svc::Rw,
        Svc::Tw,
        Svc::Bw,
        Svc::Ch,
        Svc::Pr,
        Svc::Cws,
        Svc::Cwr,
        Svc::Dr,
    ];
    pub fn code(self) -> &'static str {
        match self {
            Svc::Hws => "HWS",
            Svc::Hwr => "HWR",
            Svc::Ng => "NG",
            Svc::Mw => "MW",
            Svc::Rw => "RW",
            Svc::Tw => "TW",
            Svc::Bw => "BW",
            Svc::Ch => "CH",
            Svc::Pr => "PR",
            Svc::Cws => "CWS",
            Svc::Cwr => "CWR",
            Svc::Dr => "DR",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Svc::Hws => "heating water supply",
            Svc::Hwr => "heating water return",
            Svc::Ng => "natural gas",
            Svc::Mw => "make-up water",
            Svc::Rw => "raw water",
            Svc::Tw => "treated water",
            Svc::Bw => "backwash water",
            Svc::Ch => "chemical dosing",
            Svc::Pr => "process product",
            Svc::Cws => "cooling water supply",
            Svc::Cwr => "cooling water return",
            Svc::Dr => "drain",
        }
    }
    /// Operating temperature, °C.
    pub fn temp(self) -> f64 {
        match self {
            Svc::Hws => rules::rule("plant.heat.t_supply"),
            Svc::Hwr => rules::rule("plant.heat.t_return"),
            Svc::Pr => rules::rule("plant.process.t_hot"),
            Svc::Cws => rules::rule("plant.process.cw_in"),
            Svc::Cwr => rules::rule("plant.process.cw_in") + rules::rule("plant.process.cw_dt"),
            _ => rules::rule("plant.heat.t_fill"),
        }
    }
}

/// Kind of a unit of equipment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EqKind {
    Boiler,
    Pump,
    Separator,
    Expansion,
    Softener,
    Tank,
    Filter,
    Reactor,
    Exchanger,
    Surge,
    Dosing,
    Terminal,
    Sump,
    Column,
}

impl EqKind {
    /// Tag prefix.
    pub fn prefix(self) -> &'static str {
        match self {
            EqKind::Boiler => "B",
            EqKind::Pump => "P",
            EqKind::Separator => "HS",
            EqKind::Expansion => "EV",
            EqKind::Softener => "SF",
            EqKind::Tank => "T",
            EqKind::Filter => "F",
            EqKind::Reactor => "R",
            EqKind::Exchanger => "E",
            EqKind::Surge => "SV",
            EqKind::Dosing => "DT",
            EqKind::Terminal => "TP",
            EqKind::Sump => "SP",
            EqKind::Column => "C",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            EqKind::Boiler => "boiler",
            EqKind::Pump => "pump",
            EqKind::Separator => "hydraulic separator",
            EqKind::Expansion => "expansion vessel",
            EqKind::Softener => "water softener",
            EqKind::Tank => "tank",
            EqKind::Filter => "pressure filter",
            EqKind::Reactor => "jacketed reactor",
            EqKind::Exchanger => "shell-and-tube heat exchanger",
            EqKind::Surge => "surge vessel",
            EqKind::Dosing => "dosing tank",
            EqKind::Terminal => "terminal point",
            EqKind::Sump => "drain sump",
            EqKind::Column => "distillation column",
        }
    }
    /// IFC class and predefined type.
    pub fn ifc(self) -> &'static str {
        match self {
            EqKind::Boiler => "IfcBoiler WATER",
            EqKind::Pump => "IfcPump ENDSUCTION",
            EqKind::Separator | EqKind::Reactor | EqKind::Column => "IfcTank VESSEL",
            EqKind::Expansion => "IfcTank EXPANSION",
            EqKind::Softener | EqKind::Filter => "IfcFilter WATERFILTER",
            EqKind::Tank | EqKind::Dosing => "IfcTank STORAGE",
            EqKind::Exchanger => "IfcHeatExchanger SHELLANDTUBE",
            EqKind::Surge => "IfcTank PRESSUREVESSEL",
            EqKind::Terminal => "IfcPipeFitting CONNECTOR",
            EqKind::Sump => "IfcDistributionChamberElement SUMP",
        }
    }
    /// Stands in a row of the hall (terminals sit in the wall, the sump under the floor).
    pub fn in_row(self) -> bool {
        !matches!(self, EqKind::Terminal | EqKind::Sump)
    }
}

/// Where a nozzle sits on its unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Spot {
    Top,
    Back,
    Left,
    Right,
    /// On the burner of a boiler.
    Gas,
    Suction,
    Discharge,
    /// A terminal point in the wall.
    Wall,
    /// The inlet of the drain sump.
    Pit,
}

/// Height of a side nozzle on its shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Low,
    Mid,
    High,
}

/// What a nozzle asks for before the pipe size is known: an isolating valve of the line's size,
/// or a given item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Iso,
    Item(ItemKind),
}

/// A connection of a unit: local position and outward direction of its flange face.
#[derive(Clone, Debug, PartialEq)]
pub struct Nozzle {
    pub name: &'static str,
    pub svc: Svc,
    /// Size of the nozzle itself.
    pub dn: u32,
    /// Size of the pipe at the nozzle (larger at a pump: a reducer between).
    pub pipe_dn: u32,
    /// Design flow through the nozzle, m³/h.
    pub flow: f64,
    pub spot: Spot,
    pub level: Level,
    pub want: Vec<Want>,
    /// Items on the nozzle from its face outward.
    pub items: Vec<ItemKind>,
    pub at: V3,
    pub dir: V3,
    pub line: usize,
}

/// Main sizes of a unit; their meaning depends on the kind (diameter, straight height, length,
/// power).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dims {
    pub d: f64,
    pub h: f64,
    pub l: f64,
    pub kw: f64,
}

/// Maintenance clearance on each side of a unit's body, m (front faces the aisle).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clear {
    pub front: f64,
    pub back: f64,
    pub left: f64,
    pub right: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Row {
    North,
    South,
    Wall,
    Under,
}

/// A unit of equipment: what it is, its duty, its parametric geometry and where it stands.
#[derive(Clone, Debug, PartialEq)]
pub struct Equip {
    pub tag: String,
    pub kind: EqKind,
    /// Its role in the plant (`boiler`, `network_pump`, ...).
    pub role: &'static str,
    pub standby: bool,
    /// Duty figures: name and value.
    pub duty: Vec<(&'static str, f64)>,
    pub dims: Dims,
    pub nozzles: Vec<Nozzle>,
    /// Family definition with real geometry and the nozzles as connectors.
    pub family: String,
    /// Body boxes in local coordinates: what pipes and steel keep clear of.
    pub boxes: Vec<(V3, V3)>,
    pub clear: Clear,
    /// Flue diameter of a boiler, m.
    pub flue: Option<f64>,
    pub at: V3,
    /// Quarter turns about z.
    pub rot: u8,
    pub row: Row,
}

/// `p` turned `r` quarter turns counterclockwise about z.
pub fn rot(p: V3, r: u8) -> V3 {
    match r % 4 {
        0 => p,
        1 => v3(-p.y, p.x, p.z),
        2 => v3(-p.x, -p.y, p.z),
        _ => v3(p.y, -p.x, p.z),
    }
}

/// The box `lo..hi` turned `r` quarter turns and moved to `at`.
pub fn box_at(lo: V3, hi: V3, r: u8, at: V3) -> (V3, V3) {
    let a = rot(lo, r) + at;
    let b = rot(hi, r) + at;
    (v3(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)), v3(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)))
}

impl Equip {
    /// Family name: what the unit is, then its tag, so a schedule or a search finds it by kind.
    pub fn family_name(&self) -> String {
        format!("{}_{}", self.kind.name().replace([' ', '-'], "_"), self.tag.replace('-', "_"))
    }
    pub fn world(&self, p: V3) -> V3 {
        rot(p, self.rot) + self.at
    }
    pub fn world_dir(&self, d: V3) -> V3 {
        rot(d, self.rot)
    }
    /// Body boxes in world coordinates.
    pub fn body(&self) -> Vec<(V3, V3)> {
        self.boxes.iter().map(|&(lo, hi)| box_at(lo, hi, self.rot, self.at)).collect()
    }
    /// Union of the local body boxes (a point at the origin for a unit without a body).
    pub fn extent(&self) -> (V3, V3) {
        let mut lo = v3(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut hi = v3(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(a, b) in &self.boxes {
            lo = v3(lo.x.min(a.x), lo.y.min(a.y), lo.z.min(a.z));
            hi = v3(hi.x.max(b.x), hi.y.max(b.y), hi.z.max(b.z));
        }
        if self.boxes.is_empty() {
            (V3::default(), V3::default())
        } else {
            (lo, hi)
        }
    }
    /// World flange face and outward direction of nozzle `i`.
    pub fn nozzle_world(&self, i: usize) -> (V3, V3) {
        let n = &self.nozzles[i];
        (self.world(n.at), self.world_dir(n.dir))
    }
}

/// One end of a line: a nozzle of a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct End {
    pub eq: usize,
    pub nz: usize,
}

/// A line: a tree of pipe from its root nozzle to its other nozzles, one service, one size
/// class and one spec.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// `HWS-001`: service and running number.
    pub seq: String,
    pub svc: Svc,
    pub spec: Spec,
    pub root: End,
    pub ends: Vec<End>,
    /// Design flow at the root, m³/h: no piece carries more.
    pub flow: f64,
    /// Velocity rule the line is sized by.
    pub v_rule: &'static str,
    /// Largest size of the line.
    pub dn: u32,
    pub pn: u32,
    /// Operating temperature °C and pressure bar(g).
    pub t_op: f64,
    pub p_op: f64,
    /// Insulation thickness, m.
    pub insul: f64,
}

impl Line {
    /// Line number: size, service, number, spec.
    pub fn number(&self) -> String {
        format!("{}-{}-{}", self.dn, self.seq, self.spec.code())
    }
    pub fn all_ends(&self) -> Vec<End> {
        std::iter::once(self.root).chain(self.ends.iter().copied()).collect()
    }
    pub fn t_design(&self) -> f64 {
        self.t_op + rules::rule("plant.pipe.design_margin_t")
    }
    pub fn p_design(&self) -> f64 {
        size::r1(self.p_op * rules::rule("plant.pipe.design_margin_p"))
    }
    pub fn p_test(&self) -> f64 {
        size::r1(self.p_design() * rules::rule("plant.pipe.test_factor"))
    }
}

/// A designed plant.
#[derive(Clone, Debug, PartialEq)]
pub struct Plant {
    pub brief: Brief,
    pub equipment: Vec<Equip>,
    pub lines: Vec<Line>,
    pub hall: Hall,
    pub zones: Vec<Zone>,
    pub platforms: Vec<Platform>,
    pub steel: Vec<Steel>,
    pub flues: Vec<Flue>,
    pub bund: Option<Bund>,
    pub piping: Piping,
    pub schedules: Schedules,
    pub report: Report,
    pub why: Vec<Why>,
    /// The plant as model elements: families, placements and pipe runs.
    pub kit: Kit,
}

/// Designs the plant a brief asks for: `{kind?, text?, duty_kw?, flow_m3h?, head_m?, units?,
/// rules?}`; `rules` overrides `plant.*` rules at the brief layer.
pub fn design(brief: &J) -> Result<Plant, String> {
    let b = Brief::parse(brief)?;
    let mut rs = RuleSet::new();
    if let Some(p) = brief.get("rules") {
        rs.apply_patch(Layer::Brief, p)
            .map_err(|e| e.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("; "))?;
    }
    let mut plant = rules::scoped(&rs, || build(b))?;
    for y in &mut plant.why {
        if let Some(l) = rs.source(&y.rule) {
            y.source = l.name().to_string();
        }
    }
    Ok(plant)
}

fn build(brief: Brief) -> Result<Plant, String> {
    if brief.kind == Kind::StorageTank {
        return tank::build(brief);
    }
    let mut t = topo::of(&brief)?;
    for i in 0..t.eqs.len() {
        equip::shape(&mut t.eqs, i, &t.lines);
    }
    // room for the bund wall round the tanks, so no other unit stands in it
    let gap = rules::rule("plant.bund.margin");
    for i in layout::bunded(&t) {
        let c = &mut t.eqs[i].clear;
        (c.front, c.back, c.left, c.right) = (c.front.max(gap), c.back.max(gap), c.left.max(gap), c.right.max(gap));
    }
    let lay = layout::place(&mut t)?;
    let piping = route::route(&mut t, &lay);
    let mut lay = lay;
    layout::finish(&mut lay, &t, &piping);
    let schedules = sched::of(&t, &lay, &piping);
    let report = check::run(&t, &lay, &piping);
    let kit = model::of(&t, &lay, &piping)?;
    let mut why = std::mem::take(&mut t.why);
    why::layout(&mut why, &t, &lay, &piping);
    t.why = why;
    Ok(Plant {
        brief,
        equipment: t.eqs,
        lines: t.lines,
        hall: lay.hall,
        zones: lay.zones,
        platforms: lay.platforms,
        steel: lay.steel,
        flues: lay.flues,
        bund: lay.bund,
        piping,
        schedules,
        report,
        why: t.why,
        kit,
    })
}

fn p3(p: V3) -> J {
    json!([size::r2(p.x), size::r2(p.y), size::r2(p.z)])
}

impl Plant {
    /// Everything but the family texts ([`Plant::families`]).
    pub fn to_json(&self) -> J {
        let eq: Vec<J> = self
            .equipment
            .iter()
            .map(|e| {
                let duty: serde_json::Map<String, J> =
                    e.duty.iter().map(|(k, v)| (k.to_string(), json!(size::r2(*v)))).collect();
                let noz: Vec<J> = e
                    .nozzles
                    .iter()
                    .enumerate()
                    .map(|(i, n)| {
                        let (at, dir) = e.nozzle_world(i);
                        json!({"name": n.name, "service": n.svc.code(), "dn": n.dn, "pipe_dn": n.pipe_dn,
                               "at": p3(at), "dir": p3(dir), "line": self.lines[n.line].number(),
                               "items": n.items.iter().map(|k| k.code()).collect::<Vec<_>>()})
                    })
                    .collect();
                json!({"tag": e.tag, "kind": e.kind.name(), "ifc": e.kind.ifc(), "role": e.role,
                       "standby": e.standby, "duty": duty, "at": p3(e.at), "rot": e.rot,
                       "nozzles": noz})
            })
            .collect();
        json!({
            "kind": self.brief.kind.name(),
            "hall": self.hall.to_json(),
            "bund": self.bund.as_ref().map(|b| b.to_json()),
            "equipment": eq,
            "schedules": self.schedules.to_json(),
            "report": self.report.to_json(),
            "why": self.why.iter().map(|y| y.to_json()).collect::<Vec<_>>(),
            "note": crate::SCHEMATIC_NOTE,
        })
    }

    /// Family text of every unit, by tag.
    pub fn families(&self) -> Vec<(String, String)> {
        self.equipment.iter().map(|e| (e.tag.clone(), e.family.clone())).collect()
    }
}
