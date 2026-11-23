// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Building services tools: fixtures and devices from the library, networks routed by intent
//! (the AI names source, targets and system; the router builds), power from a panel with
//! circuits, and the panel schedule. Ids are the draft's: what one item placed, the next routes.

use super::fmt::{self, Out};
use super::mep_plan::{plan_of, MepOp};
use super::plan::{ElRef, IdOrNew, PlanOp};
use super::session::{Core, Done, Handler};
use super::tools_write::why_of;
use ddc_ai::CallCx;
use ddc_model::{
    CircuitIssue, CompSpec, DeviceSpec, MepPlanned, Model, NetReq, PanelSchedule, RouteFrom,
    SystemKind, ROUTE_VARIANTS,
};
use ddc_ui::Catalog;
use serde_json::{json, Value as J};

pub const HANDLERS: &[(&str, Handler)] = &[
    ("place_fixtures", place_fixtures),
    ("route_mep", route_mep),
    ("supply_fixtures", supply_fixtures),
    ("power_devices", power_devices),
    ("panel_schedule", panel_schedule),
];

/// Fixtures placed per item at most.
const MAX_FIXTURES: usize = ddc_ai::limits::FIXTURES.us();
/// Riser offsets of the water and drain networks from the given point, m.
const RISER_STEP: [(SystemKind, f64); 3] = [
    (SystemKind::ColdWater, 0.0),
    (SystemKind::HotWater, 0.3),
    (SystemKind::Sanitary, -0.5),
];

fn title_of(a: &J, dflt: &str) -> String {
    let t = fmt::text(a["title"].as_str().unwrap_or_default());
    if t.is_empty() {
        dflt.to_string()
    } else {
        t
    }
}

fn level_of(c: &Core, a: &J) -> Result<i64, String> {
    match &a["level"] {
        J::Null => Ok(c.view.level),
        v => v.as_i64().ok_or_else(|| "level must be a level id".to_string()),
    }
}

/// `comp:12`: the component id.
fn comp_of(s: &str) -> Result<i64, String> {
    let r = ElRef::parse(s).map_err(|e| e.0)?;
    match (r.cat.as_str(), r.id) {
        ("comp", IdOrNew::Id(id)) => Ok(id),
        _ => Err(format!("{} is not a placed component such as comp:12", fmt::text(s))),
    }
}

/// `comp:12/Cold`: component and connector (None: every connector that fits).
fn port_ref(s: &str) -> Result<(i64, Option<String>), String> {
    match s.split_once('/') {
        Some((c, n)) => Ok((comp_of(c)?, Some(n.trim().to_string()))),
        None => Ok((comp_of(s)?, None)),
    }
}

pub fn kind_of(s: &str) -> Result<SystemKind, String> {
    let k = match s {
        "cold" | "cold_water" => SystemKind::ColdWater,
        "hot" | "hot_water" => SystemKind::HotWater,
        "waste" | "sanitary" | "sewage" => SystemKind::Sanitary,
        "heating" => SystemKind::Heating,
        "chilled_water" | "chw" => SystemKind::ChilledWater,
        "sprinkler" => SystemKind::Sprinkler,
        "medical_gas" | "medgas" => SystemKind::MedicalGas,
        "supply_air" => SystemKind::SupplyAir,
        "return_air" => SystemKind::ReturnAir,
        "exhaust_air" => SystemKind::ExhaustAir,
        "power" => SystemKind::Power,
        other => return Err(format!("unknown system kind {}", fmt::text(other))),
    };
    Ok(k)
}

fn default_name(k: SystemKind) -> &'static str {
    match k {
        SystemKind::ColdWater => "Cold water",
        SystemKind::HotWater => "Hot water",
        SystemKind::Sanitary => "Waste",
        SystemKind::Heating => "Heating",
        SystemKind::ChilledWater => "Chilled water",
        SystemKind::Sprinkler => "Sprinkler",
        SystemKind::SupplyAir => "Supply air",
        SystemKind::ReturnAir => "Return air",
        SystemKind::ExhaustAir => "Exhaust air",
        SystemKind::Power => "Power",
        SystemKind::MedicalGas => "Medical gas",
    }
}

/// Target connectors: listed refs, or every fitting one on the level (in a room).
fn targets(
    m: &Model,
    a: &J,
    level: i64,
    kind: SystemKind,
) -> Result<Vec<(i64, String)>, String> {
    let room = a["room"].as_str();
    let fit = m
        .mep_targets(level, kind, room)
        .map_err(|e| e.to_string())?;
    let Some(list) = a["to"].as_array() else {
        return Ok(fit);
    };
    let mut out = vec![];
    for r in list {
        let (comp, conn) = port_ref(r.as_str().ok_or("to: refs must be text")?)?;
        match conn {
            Some(n) => out.push((comp, n)),
            None => out.extend(fit.iter().filter(|t| t.0 == comp).cloned()),
        }
    }
    Ok(out)
}

fn metrics_json(p: &MepPlanned) -> J {
    let m = &p.metrics;
    json!({
        "variant": m.variant,
        "runs": m.runs,
        "length_m": fmt::num(m.length),
        "elbows": m.elbows,
        "tees": m.tees,
        "sleeves": m.sleeves,
        "slab_holes": m.holes,
        "clashes": m.clashes,
        "z_min": fmt::num(m.z_min),
        "z_max": fmt::num(m.z_max),
        "min_slope_pct": m.min_slope.map(fmt::num),
        "failed": m.failed.iter().map(|f| format!("comp:{}/{}", f.0, f.1)).collect::<Vec<_>>(),
        "circuits": p.build.circuits.len(),
        "critical_pa": m.critical_pa.map(fmt::num),
        "critical_to": m.critical_to.as_ref().map(|f| format!("comp:{}/{}", f.0, f.1)),
    })
}

fn added(c: &mut Core, a: &J, title: String, op: MepOp, cx: &CallCx<'_>) -> Result<J, String> {
    let r = c.add_item(title, why_of(a), PlanOp::Mep(op), cx.actor)?;
    Ok(json!({"item": r.0, "changes": {"created": r.1 .0, "changed": r.1 .1, "deleted": r.1 .2}}))
}

fn place_fixtures(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let items = a["items"].as_array().ok_or("items must be a list")?;
    if items.is_empty() || items.len() > MAX_FIXTURES {
        return Err(format!("items: 1 to {MAX_FIXTURES}"));
    }
    let dl = level_of(c, a)?;
    let mut specs = vec![];
    for o in items {
        let family = o["family"].as_str().ok_or("family must be text")?.to_string();
        let ty = o["type"].as_str().ok_or("type must be text")?.to_string();
        let at = o["at"].as_array().ok_or("at must be [x, y] or [x, y, z]")?;
        let n = |i: usize| at.get(i).and_then(J::as_f64).filter(|v| v.is_finite());
        let (x, y) = (n(0).ok_or("at: x")?, n(1).ok_or("at: y")?);
        let level = o["level"].as_i64().unwrap_or(dl);
        let elev = c
            .ed
            .model
            .level_list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|l| l.0 == level)
            .ok_or_else(|| format!("no level {level}"))?
            .2;
        let mut place = CompSpec::at(level, 0, x, y);
        place.at.2 = n(2).unwrap_or(elev);
        place.rotation = o["rotation_deg"].as_f64().unwrap_or(0.0).to_radians();
        let system = match o["system"].as_str() {
            None => None,
            Some(s) => match ElRef::parse(s).map_err(|e| e.0)?.id {
                IdOrNew::Id(id) => Some(id),
                IdOrNew::New(_) => return Err("system: a system already in the draft".into()),
            },
        };
        specs.push(DeviceSpec {
            family,
            ty,
            place,
            system,
            flow: o["flow"].as_f64(),
        });
    }
    let before: i64 = max_comp(&c.ed.model)?;
    let mut o = added(c, a, title_of(a, "Place fixtures"), MepOp::Place(specs), cx)?;
    let after: i64 = max_comp(&c.ed.model)?;
    o["created"] = json!(((before + 1)..=after).map(|i| format!("comp:{i}")).collect::<Vec<_>>());
    let mut out = Out::new(c.max_chars);
    for (k, v) in o.as_object().into_iter().flatten() {
        out.put(k, v);
    }
    Ok(out.finish().into())
}

fn max_comp(m: &Model) -> Result<i64, String> {
    m.last_component().map_err(|e| e.to_string())
}

fn from_of(a: &J) -> Result<RouteFrom, String> {
    let f = &a["from"];
    if let Some(p) = f["port"].as_str() {
        let (comp, conn) = port_ref(p)?;
        let connector = conn.ok_or("from.port needs a connector: comp:12/Main")?;
        return Ok(RouteFrom::Port { comp, connector });
    }
    if let Some(r) = f["riser"].as_array() {
        let n = |i: usize| r.get(i).and_then(J::as_f64).filter(|v| v.is_finite());
        return Ok(RouteFrom::Riser {
            x: n(0).ok_or("riser: [x, y]")?,
            y: n(1).ok_or("riser: [x, y]")?,
        });
    }
    if let Some(r) = f["run"].as_str() {
        let e = ElRef::parse(r).map_err(|e| e.0)?;
        if let IdOrNew::Id(id) = e.id {
            return Ok(RouteFrom::Run(id));
        }
    }
    Err("from needs one of port, riser, run".into())
}

fn route_mep(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let kind = kind_of(a["kind"].as_str().ok_or("kind must be text")?)?;
    let level = level_of(c, a)?;
    let name = a["system"].as_str().unwrap_or(default_name(kind)).to_string();
    let mut req = NetReq::new(&name, kind, level, from_of(a)?);
    req.to = targets(&c.ed.model, a, level, kind)?;
    if req.to.is_empty() {
        return Err("no target connectors of that system".into());
    }
    req.size = a["size_mm"].as_f64();
    req.rect = a["shape"].as_str() == Some("rect");
    if let Some(s) = a["slope_pct"].as_f64() {
        req.slope = s;
    }
    if let Some(f) = a["friction_pa_m"].as_f64() {
        if !(f > 0.0 && f.is_finite()) {
            return Err("friction_pa_m must be positive".into());
        }
        req.friction = Some(f);
    }
    if let Some(t) = a["insulation_mm"].as_f64() {
        if !(t >= 0.0 && t.is_finite()) {
            return Err("insulation_mm must be non-negative".into());
        }
        req.insulation = t / 1000.0;
    }
    if let Some(p) = a["plenum"].as_array() {
        match (p.first().and_then(J::as_f64), p.get(1).and_then(J::as_f64)) {
            (Some(lo), Some(hi)) if hi > lo => req.plenum = Some((lo, hi)),
            _ => return Err("plenum: [ceiling_z, top_z] with top above ceiling".into()),
        }
    }
    let variants = c.ed.model.mep_plan_variants(&req).map_err(|e| e.to_string())?;
    let pick = match a["variant"].as_u64() {
        Some(v) if (v as usize) < ROUTE_VARIANTS => v as usize,
        Some(_) => return Err(format!("variant: 0 to {}", ROUTE_VARIANTS - 1)),
        None => (0..variants.len())
            .min_by(|x, y| {
                variants[*x]
                    .metrics
                    .score()
                    .total_cmp(&variants[*y].metrics.score())
            })
            .unwrap_or(0),
    };
    req.variant = pick;
    let list: Vec<J> = variants.iter().map(metrics_json).collect();
    let title = title_of(a, &format!("Route {name}"));
    let o = added(c, a, title, MepOp::Route(vec![req]), cx)?;
    let mut out = Out::new(c.max_chars);
    out.put("item", &o["item"]);
    out.put("changes", &o["changes"]);
    out.put("chosen", &json!(pick));
    out.put(
        "criteria",
        &json!(["0 shortest", "1 fewest fittings", "2 along grid axes, fewest crossings"]),
    );
    out.list("variants", &list);
    Ok(out.finish().into())
}

fn supply_fixtures(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let level = level_of(c, a)?;
    let r = a["riser"].as_array().ok_or("riser must be [x, y]")?;
    let n = |i: usize| r.get(i).and_then(J::as_f64).filter(|v| v.is_finite());
    let (x, y) = (n(0).ok_or("riser: x")?, n(1).ok_or("riser: y")?);
    let wanted: Vec<SystemKind> = match a["systems"].as_array() {
        None => RISER_STEP.iter().map(|s| s.0).collect(),
        Some(l) => l
            .iter()
            .map(|s| kind_of(s.as_str().unwrap_or_default()))
            .collect::<Result<_, _>>()?,
    };
    let mut reqs = vec![];
    for (kind, dx) in RISER_STEP {
        if !wanted.contains(&kind) {
            continue;
        }
        let mut q = NetReq::new(
            default_name(kind),
            kind,
            level,
            RouteFrom::Riser { x: x + dx, y },
        );
        q.to = targets(&c.ed.model, a, level, kind)?;
        if let Some(s) = a["slope_pct"].as_f64() {
            q.slope = s;
        }
        if !q.to.is_empty() {
            reqs.push(q);
        }
    }
    if reqs.is_empty() {
        return Err("no fixture connectors of these systems on the level".into());
    }
    let op = MepOp::Route(reqs);
    let p = plan_of(&c.ed.model, &op).map_err(|e| e.0)?;
    let m = metrics_json(&p);
    let o = added(c, a, title_of(a, "Supply fixtures"), op, cx)?;
    let mut out = Out::new(c.max_chars);
    out.put("item", &o["item"]);
    out.put("changes", &o["changes"]);
    out.put("metrics", &m);
    Ok(out.finish().into())
}

fn power_devices(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    // sizes circuits from code tables on its own: behind the regional switch
    if !super::mep_gate::is_on_now(&c.ed.model) {
        return Err(super::mep_gate::refused("power_devices"));
    }
    let panel = comp_of(a["panel"].as_str().ok_or("panel must be a ref such as comp:5")?)?;
    let level = level_of(c, a)?;
    let mut devices = targets(&c.ed.model, a, level, SystemKind::Power)?;
    devices.retain(|d| d.0 != panel);
    if devices.is_empty() {
        return Err("no power connectors to feed".into());
    }
    let variant = a["variant"].as_u64().unwrap_or(0).min(ROUTE_VARIANTS as u64 - 1) as usize;
    let op = MepOp::Power {
        panel,
        devices,
        level,
        variant,
    };
    let p = plan_of(&c.ed.model, &op).map_err(|e| e.0)?;
    let m = metrics_json(&p);
    let o = added(c, a, title_of(a, "Power devices"), op, cx)?;
    let sched = c.ed.model.panel_schedule(panel).map_err(|e| e.to_string())?;
    let mut out = Out::new(c.max_chars);
    out.put("item", &o["item"]);
    out.put("changes", &o["changes"]);
    out.put("metrics", &m);
    out.list("circuits", &schedule_json(&sched));
    Ok(out.finish().into())
}

fn schedule_json(s: &PanelSchedule) -> Vec<J> {
    s.rows
        .iter()
        .map(|r| {
            json!({
                "number": r.number, "name": r.name, "kind": r.kind.name(),
                "devices": r.devices, "load_w": fmt::num(r.load), "current_a": fmt::num(r.current),
                "breaker_a": fmt::num(r.breaker), "cable_mm2": fmt::num(r.cable),
                "length_m": fmt::num(r.length), "drop_pct": fmt::num(r.drop),
                "issues": r.issues.iter().map(CircuitIssue::key).collect::<Vec<_>>(),
                "rules_failed": r.rules.iter().filter(|f| !f.ok).map(|f| f.rule).collect::<Vec<_>>(),
                "rule_notes": r.rules.iter().filter_map(|f| f.note.clone()).collect::<Vec<_>>(),
                "x0_estimated": r.x0_estimated,
                "load_type": r.load_type, "conductor": r.conductor.name(),
                "rcd": r.rcd, "cable_type": r.cable_type,
            })
        })
        .collect()
}

/// The panel schedule as a table in the catalog's language: header and rows.
pub fn schedule_table(cat: &Catalog, s: &PanelSchedule) -> (Vec<String>, Vec<Vec<String>>) {
    let head = [
        "number", "name", "kind", "devices", "load", "current", "breaker", "cable", "length",
        "drop", "cos_phi", "laying", "ambient", "grouped", "issues",
    ]
    .iter()
    .map(|k| cat.t(&format!("mep.sched.{k}")))
    .collect();
    let mut rows: Vec<Vec<String>> = s
        .rows
        .iter()
        .map(|r| {
            let issues: Vec<String> = r
                .issues
                .iter()
                .map(|i| {
                    let args = i.args();
                    let a: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
                    cat.tf(i.key(), &a)
                })
                .chain(r.x0_estimated.then(|| cat.t("mep.sched.x0_estimated")))
                .chain(r.rules.iter().filter_map(|f| f.note.clone()))
                .collect();
            vec![
                r.number.to_string(),
                r.name.clone(),
                cat.t(&format!("mep.sched.kind.{}", r.kind.name())),
                r.devices.to_string(),
                format!("{:.0}", r.load),
                format!("{:.1}", r.current),
                format!("{:.0}", r.breaker),
                format!("{:.1}", r.cable),
                format!("{:.1}", r.length),
                format!("{:.2}", r.drop),
                format!("{}", r.cos_phi),
                r.laying.clone(),
                format!("{}", r.ambient_c),
                r.grouped.to_string(),
                issues.join("; "),
            ]
        })
        .collect();
    rows.push(vec![
        cat.t("mep.sched.total"),
        String::new(),
        String::new(),
        s.rows.iter().map(|r| r.devices).sum::<usize>().to_string(),
        format!("{:.0}", s.load),
        format!("{:.1}", s.current),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    ]);
    (head, rows)
}

fn panel_schedule(c: &mut Core, a: &J, _: &CallCx<'_>) -> Result<Done, String> {
    let panel = comp_of(a["panel"].as_str().ok_or("panel must be a ref such as comp:5")?)?;
    let s = c.ed.model.panel_schedule(panel).map_err(|e| e.to_string())?;
    if s.rows.is_empty() {
        return Err(format!("comp:{panel} feeds no circuits"));
    }
    let lang = match a["lang"].as_str() {
        Some("ru") => ddc_ui::Lang::Ru,
        _ => ddc_ui::Lang::En,
    };
    let (head, rows) = schedule_table(&Catalog::new(lang), &s);
    let mut out = Out::new(c.max_chars);
    out.put("panel", &json!(format!("comp:{panel}")));
    out.put("load_w", &json!(fmt::num(s.load)));
    out.put("current_a", &json!(fmt::num(s.current)));
    out.put("phase_current_a", &json!(s.phases.iter().map(|p| fmt::num(*p)).collect::<Vec<_>>()));
    out.list("circuits", &schedule_json(&s));
    out.put("table", &json!({"head": head, "rows": rows}));
    if a["diagram"].as_bool() == Some(true) {
        let d = ddc_model::SingleLine::from_schedule(&s);
        out.put("connections", &json!(d.connections()));
        out.put("diagram_svg", &json!(d.to_svg()));
    }
    Ok(out.finish().into())
}

#[cfg(test)]
#[path = "mep_tools_tests.rs"]
mod tests;
