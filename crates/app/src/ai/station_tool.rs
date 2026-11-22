// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `plan_station`: an underground station from the `station.*` rules as plan items: the levels
//! below ground it lacks, its own wall, slab and column types, and one item that draws the box,
//! slabs, wells, stairs, columns and rooms on those levels. The reply carries the reasons.

use super::create_more::{planned_levels, planned_type};
use super::modify_plan::ToolAct;
use super::plan::{ElRef, NewElem, PlanOp, RoofVault};
use super::session::{Core, Done};
use super::tools_write::{p2, plan_type, title_of, type_ref, why_of};
use ddc_ai::CallCx;
use ddc_gen::project::rules::{Layer, RuleSet};
use ddc_gen::station::{self, GStation, Lvl, ShaftKind, SlabKind, WallKind};
use serde_json::{json, Value as J};
use std::collections::BTreeMap;

pub const HANDLERS: [(&str, super::session::Handler); 1] = [("plan_station", plan_station)];

const ESCALATOR: &str = "Escalator";
const LIFT_CAR: &str = "Lift Car";
const PLATFORM_LIFT: &str = "Platform Lift";

/// Elevations closer than this are the same level.
const SAME_LEVEL: f64 = 1e-6;

fn rule_set(a: &J) -> Result<RuleSet, String> {
    let mut rs = RuleSet::new();
    let Some(m) = a["rules"].as_object() else {
        return match &a["rules"] {
            J::Null => Ok(rs),
            _ => Err("rules must be an object of station.* ids and numbers".into()),
        };
    };
    for (id, v) in m {
        if !id.starts_with("station.") {
            return Err(format!("{id}: only station.* rules shape a station (see list_rules)"));
        }
        let x = v.as_f64().ok_or_else(|| format!("{id} must be a number"))?;
        rs.set(Layer::Brief, id, x).map_err(|e| e.to_string())?;
    }
    Ok(rs)
}

/// The id of the level named `name` at `z`, planning it when the model and the plan lack it.
fn level_at(c: &mut Core, cx: &CallCx<'_>, why: &str, name: &str, z: f64) -> Result<i64, String> {
    let real = c.ed.levels();
    if let Some(l) = real.iter().find(|l| l.1 == name) {
        if (l.2 - z).abs() > SAME_LEVEL {
            return Err(format!("level {name} exists at {} m, the station needs it at {z} m: rename or move it", l.2));
        }
        return Ok(l.0);
    }
    if let Some(l) = planned_levels(c).into_iter().find(|l| l.1 == name) {
        return Ok(l.0);
    }
    let op = PlanOp::Tool(ToolAct::AddLevel { name: name.to_string(), elevation: z });
    c.add_item(format!("Level {name}"), why.to_string(), op, cx.actor)?;
    planned_levels(c)
        .into_iter()
        .find(|l| l.1 == name)
        .map(|l| l.0)
        .ok_or_else(|| format!("level {name} was planned but has no id"))
}

/// The ground level: the model's level at elevation zero, else a planned one.
fn ground(c: &mut Core, cx: &CallCx<'_>, why: &str, name: &str) -> Result<i64, String> {
    if let Some(l) = c.ed.levels().into_iter().find(|l| l.2.abs() < SAME_LEVEL) {
        return Ok(l.0);
    }
    level_at(c, cx, why, name, 0.0)
}

/// A type of `table` by name: the model's, the plan's, or planned now with `fields`.
fn type_of(c: &mut Core, cx: &CallCx<'_>, why: &str, cat: &str, table: &str, name: &str, fields: J) -> Result<ElRef, String> {
    if let Ok(Some(r)) = type_ref(c, &json!({"type": name}), table) {
        return Ok(r);
    }
    let one = json!({"title": format!("Type {name}"), "why": why, "cat": cat, "name": name, "fields": fields});
    plan_type(c, &one, cx)?;
    planned_type(c, table, &name.trim().to_lowercase()).ok_or_else(|| format!("type {name} was planned but not found"))
}

/// A library family ready for the plan: in the model, loaded earlier in the plan, or loaded now.
/// `None` when the library has no such family.
fn family(c: &mut Core, cx: &CallCx<'_>, why: &str, name: &str) -> Result<Option<ddc_model::Family>, String> {
    let fam = ddc_model::library().map_err(|e| e.to_string())?.into_iter().find(|f| f.name == name);
    let Some(fam) = fam else { return Ok(None) };
    let head = format!("family {name}");
    let has = c.ed.model.families().map_err(|e| e.to_string())?.iter().any(|f| f.1 == name)
        || c.plan.items.iter().any(|i| matches!(&i.op, PlanOp::LoadFamily { def } if def.lines().next() == Some(head.as_str())));
    if !has {
        let def = ddc_model::library_sources().into_iter().find(|s| s.lines().next() == Some(head.as_str()));
        let def = def.ok_or_else(|| format!("library family {name} has no source"))?.to_string();
        c.add_item(format!("Family {name}"), why.to_string(), PlanOp::LoadFamily { def }, cx.actor)?;
    }
    Ok(Some(fam))
}

/// Numeric values of a library type as `fields.values` of a planned type.
fn values_of(t: &ddc_geom::component::FamilyType) -> J {
    J::Object(
        t.values
            .iter()
            .filter_map(|(k, v)| match v {
                ddc_model::ParamValue::Num(x) => Some((k.clone(), json!(x))),
                ddc_model::ParamValue::Text(_) => None,
            })
            .collect(),
    )
}

/// The car type of the station lifts: the lift car of the rated load (`station.lift_load`) when
/// the library has that family, else the largest platform lift that fits the shaft, running from
/// the platform to the street.
fn lift_type(c: &mut Core, cx: &CallCx<'_>, why: &str, rs: &RuleSet, s: &GStation, shafts: &[&station::SShaft]) -> Result<ElRef, String> {
    let side = shafts.first().map_or(0.0, |sh| {
        let b = station::bounds(&sh.ring);
        (b.2 - b.0).min(b.3 - b.1)
    });
    if let Some(fam) = family(c, cx, why, LIFT_CAR)? {
        let load = rs.get("station.lift_load");
        let want = format!("{load} kg");
        let t = fam.types.iter().find(|t| t.name == want).ok_or_else(|| {
            let have: Vec<&str> = fam.types.iter().map(|t| t.name.as_str()).collect();
            format!("station.lift_load {load}: the {LIFT_CAR} family has types {have:?}")
        })?;
        return type_of(c, cx, why, "comp", "comp_type", &format!("Station lift {want}"), json!({"family": LIFT_CAR, "values": values_of(t)}));
    }
    let fam = family(c, cx, why, PLATFORM_LIFT)?.ok_or_else(|| format!("the library has no {LIFT_CAR} or {PLATFORM_LIFT} family"))?;
    let num = |t: &ddc_geom::component::FamilyType, k: &str| {
        t.values.iter().find(|v| v.0 == k).and_then(|v| match v.1 {
            ddc_model::ParamValue::Num(x) => Some(x),
            ddc_model::ParamValue::Text(_) => None,
        })
    };
    let size = |t: &ddc_geom::component::FamilyType| num(t, "Width").zip(num(t, "Depth"));
    let t = fam
        .types
        .iter()
        .filter(|t| size(t).is_some_and(|(w, d)| w.max(d) <= side))
        .max_by(|a, b| {
            let area = |t| size(t).map_or(0.0, |(w, d)| w * d);
            area(a).total_cmp(&area(b))
        })
        .ok_or_else(|| format!("no {PLATFORM_LIFT} type fits a {side} m shaft: raise station.lift_shaft"))?;
    let travel = s.elevation(Lvl::Ground) - s.elevation(Lvl::Platform);
    let mut values = values_of(t);
    values["Travel"] = json!(travel);
    let name = format!("Station lift {} travel {} mm", t.name, mm(travel));
    type_of(c, cx, why, "comp", "comp_type", &name, json!({"family": PLATFORM_LIFT, "values": values}))
}

fn layer(t: f64) -> J {
    json!({"layers": [{"material": "concrete", "thickness": t}]})
}

fn mm(t: f64) -> String {
    format!("{}", (t * 1000.0).round())
}

fn plan_station(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let rs = rule_set(a)?;
    let s: GStation = station::plan(&rs).map_err(|e| e.to_string())?;
    let ru = a["lang"].as_str() == Some("ru");
    let pick = |n: (&'static str, &'static str)| if ru { n.1 } else { n.0 };
    let off = match &a["origin"] {
        J::Null => (0.0, 0.0),
        v => p2(v, "origin")?,
    };
    let at = |p: (f64, f64)| (p.0 + off.0, p.1 + off.1);
    let why = why_of(a);

    let mut lv: BTreeMap<u8, i64> = BTreeMap::new();
    let key = |l: Lvl| l as u8;
    lv.insert(key(Lvl::Ground), ground(c, cx, &why, pick(Lvl::Ground.names()))?);
    for (l, z) in &s.levels {
        lv.insert(key(*l), level_at(c, cx, &why, pick(l.names()), *z)?);
    }
    let id = |l: Lvl| lv[&key(l)];

    let dwall = type_of(c, cx, &why, "wall", "wall_type", &format!("Diaphragm wall {} mm", mm(s.dwall_thickness)), layer(s.dwall_thickness))?;
    let mut slab_types: Vec<(u64, ElRef)> = vec![];
    for sl in &s.slabs {
        let k = sl.thickness.to_bits();
        if !slab_types.iter().any(|t| t.0 == k) {
            let r = type_of(c, cx, &why, "floor", "slab_type", &format!("Station slab {} mm", mm(sl.thickness)), layer(sl.thickness))?;
            slab_types.push((k, r));
        }
    }
    let mut col_types: Vec<(u64, ElRef)> = vec![];
    for col in &s.columns {
        let k = col.size.to_bits();
        if !col_types.iter().any(|t| t.0 == k) {
            let shape = format!("rect {} {}", col.size, col.size);
            let fields = json!({"shape": shape, "material": "concrete"});
            let r = type_of(c, cx, &why, "column", "member_type", &format!("Station column {} mm", mm(col.size)), fields)?;
            col_types.push((k, r));
        }
    }
    // escalators of the library family, one type per size; a car in every lift shaft
    let mut comps = vec![];
    family(c, cx, &why, ESCALATOR)?.ok_or_else(|| format!("the library has no {ESCALATOR} family"))?;
    let land = rs.get("station.escalator_landing");
    let mut esc_types: Vec<([u64; 3], ElRef)> = vec![];
    for e in &s.escalators {
        let rise = e.top.2 - e.bottom.2;
        let run = (e.top.0 - e.bottom.0).hypot(e.top.1 - e.bottom.1);
        let k = [rise.to_bits(), run.to_bits(), e.width.to_bits()];
        let r = match esc_types.iter().find(|t| t.0 == k) {
            Some(t) => t.1.clone(),
            None => {
                let fields = json!({"family": ESCALATOR, "values": {"Rise": rise, "Run": run, "Landing": land, "Width": e.width}});
                let name = format!("Station escalator {}x{}x{} mm", mm(rise), mm(run), mm(e.width));
                let r = type_of(c, cx, &why, "comp", "comp_type", &name, fields)?;
                esc_types.push((k, r.clone()));
                r
            }
        };
        let lvl = s.levels.iter().find(|l| (l.1 - e.bottom.2).abs() < SAME_LEVEL).map(|l| l.0).unwrap_or(Lvl::Ground);
        let rot = (e.top.1 - e.bottom.1).atan2(e.top.0 - e.bottom.0);
        comps.push((lvl, r, at((e.bottom.0, e.bottom.1)), rot));
    }
    let lift_shafts: Vec<_> = s.shafts.iter().filter(|x| x.kind == ShaftKind::Lift).collect();
    let car = lift_type(c, cx, &why, &rs, &s, &lift_shafts)?;
    for sh in &lift_shafts {
        let b = station::bounds(&sh.ring);
        comps.push((sh.level, car.clone(), at(((b.0 + b.2) * 0.5, (b.1 + b.3) * 0.5)), 0.0));
    }
    let typ = |v: &[(u64, ElRef)], x: f64| v.iter().find(|t| t.0 == x.to_bits()).map(|t| t.1.clone());

    let mut items = vec![];
    for w in &s.walls {
        let wtype = (w.kind == WallKind::Diaphragm).then(|| dwall.clone());
        items.push(NewElem::Wall { level: id(w.level), a: at(w.a), b: at(w.b), bulge: 0.0, base_offset: w.base_offset, height: w.height, wtype });
    }
    for sl in &s.slabs {
        let outer = sl.ring.iter().map(|p| at(*p)).collect();
        items.push(NewElem::Slab { level: id(sl.level), stype: typ(&slab_types, sl.thickness), offset: sl.offset, outer, holes: vec![] });
    }
    for sh in &s.shafts {
        let ring = sh.ring.iter().map(|p| at(*p)).collect();
        items.push(NewElem::Shaft { level: id(sh.level), top_level: id(sh.top), top_offset: 0.0, ring });
    }
    for st in &s.stairs {
        items.push(NewElem::GenStair {
            level: id(st.level),
            top_level: id(st.top),
            top_offset: 0.0,
            origin: at(st.origin),
            angle: st.angle,
            shape: st.shape,
            width: st.width,
            max_riser: st.max_riser,
            landing: st.landing,
        });
    }
    for col in &s.columns {
        items.push(NewElem::Column {
            level: id(col.level),
            top_level: col.top.map(id),
            mtype: typ(&col_types, col.size),
            at: at(col.at),
            base_offset: 0.0,
            top_offset: col.top_offset,
            rotation: 0.0,
            structural: true,
            top_at: None,
        });
    }
    // the tunnel vaults: barrel roof segments over each trackway and the platform
    let roof_t = s.slabs.iter().find(|x| x.kind == SlabKind::Roof).map(|x| x.thickness);
    for v in &s.vaults {
        items.push(NewElem::Roof {
            level: id(v.level),
            stype: roof_t.and_then(|t| typ(&slab_types, t)),
            offset: v.offset,
            overhang: 0.0,
            ring: vec![],
            slope: None,
            edges: vec![],
            vault: Some(RoofVault { a: at(v.a), b: at(v.b), profile: v.profile.clone(), depth: v.depth }),
            eave: None,
        });
    }
    for (i, r) in s.rooms.iter().enumerate() {
        items.push(NewElem::Room { level: id(r.level), name: pick(r.name).to_string(), number: format!("S{}", i + 1), point: at(r.point), height: r.height });
    }

    for (lvl, ctype, p, rotation) in comps {
        items.push(NewElem::Comp { level: id(lvl), ctype, at: (p.0, p.1, 0.0), rotation, host: None, scale: 1.0, min_clear: 0.0 });
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &items {
        *counts.entry(e.cat()).or_default() += 1;
    }
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(items.len() as u32, item);
    let title = match title_of(a) {
        t if t.is_empty() => "Underground station".to_string(),
        t => t,
    };
    let added = c.add_item(title.clone(), why.clone(), PlanOp::Create { slot0, items }, cx.actor)?;
    // the ground: four plane strips at street level round the box walls, none over the box
    for (k, ring) in s.ground.iter().enumerate() {
        let boundary: Vec<(f64, f64)> = ring.iter().map(|p| at(*p)).collect();
        let terrain = ddc_model::TopoSpec {
            name: format!("{title} ground {}", k + 1),
            thickness: c.ed.topo.opts.thickness,
            material: crate::topo::EARTH.to_string(),
            points: boundary.iter().map(|p| (p.0, p.1, 0.0)).collect(),
            breaklines: vec![],
            boundary,
        };
        c.add_item(format!("{title} ground {}", k + 1), why.clone(), PlanOp::Tool(ToolAct::Terrain(terrain)), cx.actor)?;
    }
    counts.insert("toposolid", s.ground.len());

    let roof = s.slabs.iter().find(|x| x.kind == SlabKind::Roof).map(|x| s.elevation(x.level) + x.offset);
    let escalators: Vec<J> = s
        .escalators
        .iter()
        .map(|e| json!({"bank": e.bank, "bottom": [e.bottom.0 + off.0, e.bottom.1 + off.1, e.bottom.2],
                         "top": [e.top.0 + off.0, e.top.1 + off.1, e.top.2], "width": e.width}))
        .collect();
    let mut out = super::fmt::Out::new(c.max_chars);
    out.put("item", &json!(added.0));
    out.put("created", &json!(counts));
    out.put("levels", &json!(s.levels.iter().map(|l| json!({"level": format!("level:{}", id(l.0)), "name": pick(l.0.names()), "elevation": l.1})).collect::<Vec<_>>()));
    out.put("metrics", &json!(s.metrics.iter().map(|m| (m.0, m.1)).collect::<BTreeMap<_, _>>()));
    out.put("roof_top", &json!(roof));
    out.put("issues", &json!(s.issues));
    out.put_fit("why", &J::from(s.why.iter().map(|w| w.to_json()).collect::<Vec<_>>()), &[]);
    out.put_fit("escalator_axes", &J::from(escalators), &[]);
    Ok(out.finish().into())
}
