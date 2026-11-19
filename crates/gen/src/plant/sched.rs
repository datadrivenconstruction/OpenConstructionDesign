//! Schedules: equipment, line list with design conditions and isometric legs, valves,
//! instruments, fittings, supports, steel, and the totals the line list adds up to.

use std::collections::BTreeMap;

use serde_json::{json, Value as J};

use super::check;
use super::layout::{self, Lay};
use super::route::{FitKind, Piping, SupportKind};
use super::size::{self, r1, r2};
use super::topo::Topo;
use super::{End, Equip};

const MM: f64 = 1000.0;

/// One row of the line list.
#[derive(Clone, Debug, PartialEq)]
pub struct LineRow {
    pub number: String,
    pub service: &'static str,
    pub spec: &'static str,
    pub material: &'static str,
    pub dn: u32,
    pub pn: u32,
    pub from: String,
    pub to: Vec<String>,
    pub t_op: f64,
    pub p_op: f64,
    pub t_design: f64,
    pub p_design: f64,
    pub p_test: f64,
    pub insul_mm: f64,
    pub length: f64,
    pub elbows: usize,
    pub tees: usize,
    pub reducers: usize,
    pub flanges: usize,
    pub valves: usize,
    pub instruments: usize,
    pub supports: usize,
    /// Isometric legs: direction (E, W, N, S, U, D) and length in mm, from the root.
    pub legs: Vec<String>,
}

/// What the whole plant takes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub pipe_m: f64,
    pub elbows: usize,
    pub tees: usize,
    pub reducers: usize,
    pub flanges: usize,
    pub valves: usize,
    pub instruments: usize,
    pub supports: usize,
    pub steel_m: f64,
    /// Pipe metres by size.
    pub by_dn: BTreeMap<u32, f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Schedules {
    pub equipment: Vec<J>,
    pub lines: Vec<LineRow>,
    pub valves: Vec<J>,
    pub instruments: Vec<J>,
    pub fittings: Vec<J>,
    pub supports: Vec<J>,
    pub steel: Vec<J>,
    pub platforms: Vec<J>,
    pub totals: Totals,
}

fn end_name(eqs: &[Equip], e: End) -> String {
    format!("{}.{}", eqs[e.eq].tag, eqs[e.eq].nozzles[e.nz].name)
}

fn leg(d: ddc_geom::math::V3) -> String {
    let (ax, ay, az) = (d.x.abs(), d.y.abs(), d.z.abs());
    let (c, l) = if az >= ax.max(ay) {
        (if d.z > 0.0 { 'U' } else { 'D' }, d.len())
    } else if ax >= ay {
        (if d.x > 0.0 { 'E' } else { 'W' }, d.len())
    } else {
        (if d.y > 0.0 { 'N' } else { 'S' }, d.len())
    };
    format!("{c}{}", (l * MM).round() as i64)
}

pub(super) fn of(t: &Topo, lay: &Lay, p: &Piping) -> Schedules {
    let mut lines = vec![];
    for (li, l) in t.lines.iter().enumerate() {
        let segs: Vec<_> = p.segs.iter().filter(|s| s.line == li).collect();
        let fit = |k: &[FitKind]| p.fittings.iter().filter(|f| f.line == li && k.contains(&f.kind)).count();
        lines.push(LineRow {
            number: l.number(),
            service: l.svc.name(),
            spec: l.spec.code(),
            material: l.spec.material_name(),
            dn: l.dn,
            pn: l.pn,
            from: end_name(&t.eqs, l.root),
            to: l.ends.iter().map(|&e| end_name(&t.eqs, e)).collect(),
            t_op: r1(l.t_op),
            p_op: r1(l.p_op),
            t_design: r1(l.t_design()),
            p_design: l.p_design(),
            p_test: l.p_test(),
            insul_mm: (l.insul * MM).round(),
            length: segs.iter().map(|s| s.len()).sum(),
            elbows: fit(&[FitKind::Elbow]),
            tees: fit(&[FitKind::Tee]),
            reducers: fit(&[FitKind::Reducer, FitKind::EccReducer]),
            flanges: fit(&[FitKind::Flange]),
            valves: p.items.iter().filter(|i| i.line == li && i.item.is_valve()).count(),
            instruments: p.items.iter().filter(|i| i.line == li && i.item.is_instrument()).count(),
            supports: p.supports.iter().filter(|s| s.line == li).count(),
            legs: segs.iter().map(|s| leg(s.b - s.a)).collect(),
        });
    }
    let mut totals = Totals {
        pipe_m: p.segs.iter().map(|s| s.len()).sum(),
        elbows: p.fittings.iter().filter(|f| f.kind == FitKind::Elbow).count(),
        tees: p.fittings.iter().filter(|f| f.kind == FitKind::Tee).count(),
        reducers: p.fittings.iter().filter(|f| matches!(f.kind, FitKind::Reducer | FitKind::EccReducer)).count(),
        flanges: p.fittings.iter().filter(|f| f.kind == FitKind::Flange).count(),
        valves: p.items.iter().filter(|i| i.item.is_valve()).count(),
        instruments: p.items.iter().filter(|i| i.item.is_instrument()).count(),
        supports: p.supports.len(),
        steel_m: lay.steel.iter().map(|s| s.len()).sum(),
        by_dn: BTreeMap::new(),
    };
    for s in &p.segs {
        *totals.by_dn.entry(s.dn).or_default() += s.len();
    }
    let equipment = t
        .eqs
        .iter()
        .map(|e| {
            let duty: serde_json::Map<String, J> = e.duty.iter().map(|(k, v)| (k.to_string(), json!(r2(*v)))).collect();
            json!({"tag": e.tag, "kind": e.kind.name(), "role": e.role, "standby": e.standby, "duty": duty,
                   "d": r2(e.dims.d), "h": r2(e.dims.h), "l": r2(e.dims.l), "kw": r2(e.dims.kw),
                   "nozzles": e.nozzles.len()})
        })
        .collect();
    let item_row = |i: &super::route::Placed| {
        let l = &t.lines[i.line];
        json!({"tag": i.tag, "kind": i.item.name(), "ifc": i.item.ifc(), "dn": i.dn, "pn": l.pn,
               "line": l.number(), "at": end_name(&t.eqs, i.end), "op_z": r2(i.op.z),
               "access": check::access_of(i, lay)})
    };
    let valves = p.items.iter().filter(|i| i.item.is_valve()).map(item_row).collect();
    let instruments = p.items.iter().filter(|i| i.item.is_instrument()).map(item_row).collect();
    let mut fc: BTreeMap<(FitKind, u32, u32), usize> = BTreeMap::new();
    for f in &p.fittings {
        *fc.entry((f.kind, f.dn, f.dn2)).or_default() += 1;
    }
    let fittings = fc.into_iter().map(|((k, a, b), n)| json!({"kind": k.name(), "dn": a, "dn2": b, "count": n})).collect();
    let mut sc: BTreeMap<(SupportKind, u32), usize> = BTreeMap::new();
    for s in &p.supports {
        *sc.entry((s.kind, s.dn)).or_default() += 1;
    }
    let supports = sc.into_iter().map(|((k, dn), n)| json!({"kind": k.name(), "dn": dn, "count": n})).collect();
    let mut st: BTreeMap<layout::SteelKind, (usize, f64)> = BTreeMap::new();
    for s in &lay.steel {
        let e = st.entry(s.kind).or_default();
        e.0 += 1;
        e.1 += s.len();
    }
    let steel = st.into_iter().map(|(k, (n, m))| json!({"kind": k.name(), "count": n, "m": r2(m)})).collect();
    totals.pipe_m = size::r2(totals.pipe_m);
    totals.steel_m = size::r2(totals.steel_m);
    Schedules {
        equipment,
        lines,
        valves,
        instruments,
        fittings,
        supports,
        steel,
        platforms: lay.platforms.iter().map(layout::platform_json).collect(),
        totals,
    }
}

impl LineRow {
    pub fn to_json(&self) -> J {
        json!({"number": self.number, "service": self.service, "spec": self.spec, "material": self.material,
               "dn": self.dn, "pn": self.pn, "from": self.from, "to": self.to, "t_op": self.t_op,
               "p_op": self.p_op, "t_design": self.t_design, "p_design": self.p_design, "p_test": self.p_test,
               "insul_mm": self.insul_mm, "length_m": r2(self.length), "elbows": self.elbows, "tees": self.tees,
               "reducers": self.reducers, "flanges": self.flanges, "valves": self.valves,
               "instruments": self.instruments, "supports": self.supports, "iso": self.legs})
    }
}

impl Totals {
    pub fn to_json(&self) -> J {
        let by_dn: serde_json::Map<String, J> = self.by_dn.iter().map(|(k, v)| (k.to_string(), json!(r2(*v)))).collect();
        json!({"pipe_m": self.pipe_m, "elbows": self.elbows, "tees": self.tees, "reducers": self.reducers,
               "flanges": self.flanges, "valves": self.valves, "instruments": self.instruments,
               "supports": self.supports, "steel_m": self.steel_m, "pipe_m_by_dn": by_dn})
    }
}

impl Schedules {
    pub fn to_json(&self) -> J {
        json!({"equipment": self.equipment, "lines": self.lines.iter().map(|l| l.to_json()).collect::<Vec<_>>(),
               "valves": self.valves, "instruments": self.instruments, "fittings": self.fittings,
               "supports": self.supports, "steel": self.steel, "platforms": self.platforms,
               "totals": self.totals.to_json()})
    }
}
