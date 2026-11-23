// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The chat tools `define_family` and `create_geometry`: the structured family JSON of
//! `model/src/family_define.rs` and the geometry ops of `ops/src/geo.rs`, as plan items. Both
//! are drafts: a family lands with `status draft` and nothing reaches the live model before the
//! person's Apply. Every length, area, volume and angle carries its unit.

use super::exec::ModelStep;
use super::plan::{push, CompileCx, PlanError};
use ddc_ops::{Actor, Envelope, GeoOp, Group, Op};
use serde_json::{json, Value as J};

/// Operations of one `create_geometry` call.
pub const MAX_GEO_OPS: usize = ddc_ai::limits::GEO_OPS.us();
const UNIT_KINDS: [&str; 4] = ["length", "area", "volume", "angle"];

/// Names of the family's parameters that are not quantities (counts, switches, plain
/// numbers): a value filed under such a name is dimensionless, so it carries no unit.
type Plain = std::collections::HashSet<String>;

fn bare(v: &J, path: &str, plain: &Plain) -> Result<(), String> {
    match v {
        J::Number(n) if n.as_f64().is_some_and(|x| x != 0.0) => Err(format!(
            "{path}: a bare number has no unit; write \"500mm\", \"2.1m\", \"90deg\" \
             or a parameter formula"
        )),
        // num-lint: only detects a bare number in a model's argument
        J::String(s) if s.trim().parse::<f64>().is_ok_and(|x| x != 0.0) => Err(format!(
            "{path}: \"{}\" has no unit; write it with mm, m, deg or as a formula",
            s.trim()
        )),
        J::Array(a) => a
            .iter()
            .enumerate()
            .try_for_each(|(i, x)| bare(x, &format!("{path}/{i}"), plain)),
        J::Object(o) => o
            .iter()
            .filter(|(k, _)| !plain.contains(k.as_str()))
            .try_for_each(|(k, x)| bare(x, &format!("{path}/{k}"), plain)),
        _ => Ok(()),
    }
}

/// The family definition as the person will review it: units on every dimensioned parameter
/// and on every literal length of planes, forms and constraints.
pub fn check_family(def: &J) -> Result<(), String> {
    let o = def.as_object().ok_or("family must be an object")?;
    if o.get("name").and_then(|n| n.as_str()).map_or(true, |n| n.trim().is_empty()) {
        return Err("family needs a name".to_string());
    }
    let list = |k: &str| o.get(k).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let plain: Plain = list("params")
        .iter()
        .filter(|p| !UNIT_KINDS.contains(&p["type"].as_str().unwrap_or_default()))
        .filter_map(|p| p["name"].as_str().map(str::to_string))
        .collect();
    for (i, p) in list("params").iter().enumerate() {
        let kind = p["type"].as_str().unwrap_or_default();
        if UNIT_KINDS.contains(&kind) && p["unit"].as_str().map_or(true, str::is_empty) {
            return Err(format!(
                "/params/{i}: {} is a {kind}: give its unit (mm, m, deg ...)",
                p["name"].as_str().unwrap_or("?")
            ));
        }
    }
    for (i, r) in list("refs").iter().enumerate() {
        for k in ["at", "free"] {
            bare(&r[k], &format!("/refs/{i}/{k}"), &plain)?;
        }
    }
    for (i, f) in list("forms").iter().enumerate() {
        for (k, v) in f.as_object().into_iter().flatten() {
            if !matches!(k.as_str(), "name" | "mat" | "void") {
                bare(v, &format!("/forms/{i}/{k}"), &plain)?;
            }
        }
    }
    for (i, c) in list("constraints").iter().enumerate() {
        for k in ["dim", "angle"] {
            bare(&c[k], &format!("/constraints/{i}/{k}"), &plain)?;
        }
    }
    Ok(())
}

fn remap(v: &mut J, slot0: u32, n: usize) -> Result<(), String> {
    match v {
        J::Object(o) => {
            if o.get("by").and_then(|b| b.as_str()) == Some("new") {
                let k = o.get("slot").and_then(|s| s.as_u64()).ok_or("new ref needs a slot")?;
                if k as usize >= n {
                    return Err(format!("slot {k} is not an operation of this call (0..{n})"));
                }
                o.insert("slot".into(), json!(slot0 as u64 + k));
            }
            o.values_mut().try_for_each(|x| remap(x, slot0, n))
        }
        J::Array(a) => a.iter_mut().try_for_each(|x| remap(x, slot0, n)),
        _ => Ok(()),
    }
}

/// Wire ops of the call. Operation `i` that creates a solid, mass, freeform, panel system or
/// surface shell gets slot `slot0 + i`;
/// `{"by": "new", "slot": k}` names operation `k` of the same call.
pub fn geo_ops(slot0: u32, ops: &[J]) -> Result<Vec<Op>, String> {
    if ops.is_empty() {
        return Err("ops is empty".to_string());
    }
    if ops.len() > MAX_GEO_OPS {
        return Err(format!("at most {MAX_GEO_OPS} operations per call"));
    }
    ops.iter()
        .enumerate()
        .map(|(i, o)| {
            let mut v = o.clone();
            if v.get("op").and_then(|x| x.as_str()) != Some("pattern") {
                super::geo_args::normalize(&mut v);
            }
            super::geo_shape::normalize(&mut v);
            let m = v.as_object_mut().ok_or_else(|| format!("ops[{i}]: expected an object"))?;
            match m.get("op").and_then(|x| x.as_str()) {
                None => {
                    m.insert("op".into(), json!("geometry"));
                }
                Some("pattern") => {}
                Some(other) => {
                    return Err(format!("ops[{i}]: `{other}` is not a geometry operation"))
                }
            }
            if matches!(
                m.get("action").and_then(|a| a.as_str()),
                Some(
                    "solid" | "mass" | "freeform" | "panelize" | "surface_shell" | "gridshell" | "member"
                        | "plate"
                )
            ) {
                m.insert("slot".into(), json!(slot0 + i as u32));
            }
            remap(&mut v, slot0, ops.len()).map_err(|e| format!("ops[{i}]: {e}"))?;
            serde_json::from_value::<Op>(v).map_err(|e| super::geo_shape::refusal(i, &e.to_string()))
        })
        .collect()
}

pub fn envelope_of(ops: Vec<Op>, title: &str) -> Result<Envelope, String> {
    let mut env = Envelope::new(Actor::Ai { session: "chat".into(), model: "plan".into() }, title);
    env.groups.push(Group {
        id: 1,
        title: title.to_string(),
        ops,
        depends_on: vec![],
        why: String::new(),
    });
    match env.check().first() {
        Some(i) => Err(format!("{i:?}")),
        None => Ok(env),
    }
}

/// Checked at the tool call, so the model sees the refusal before an item exists.
pub fn check_ops(slot0: u32, ops: &[J]) -> Result<(), String> {
    envelope_of(geo_ops(slot0, ops)?, "geometry").map(|_| ())
}

pub(super) fn compile(
    cx: &mut CompileCx<'_>,
    slot0: u32,
    ops: &[J],
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let list = geo_ops(slot0, ops).map_err(PlanError)?;
    let makers: Vec<u32> = list
        .iter()
        .filter_map(|o| match o {
            Op::Geometry(
                GeoOp::Solid { slot, .. }
                | GeoOp::Mass { slot, .. }
                | GeoOp::Freeform { slot, .. }
                | GeoOp::Panelize { slot, .. }
                | GeoOp::SurfaceShell { slot, .. }
                | GeoOp::Gridshell { slot, .. }
                | GeoOp::Member { slot, .. }
                | GeoOp::Plate { slot, .. },
            ) => Some(*slot),
            _ => None,
        })
        .collect();
    let env = envelope_of(list, "geometry").map_err(PlanError)?;
    let ids = push(cx, out, ModelStep::Geometry { env: Box::new(env), made: vec![] })?;
    for (slot, id) in makers.into_iter().zip(ids) {
        cx.slots.insert(slot, id);
    }
    if let Some(ModelStep::Geometry { made, .. }) = out.last() {
        cx.declared.extend(made.iter().cloned());
    }
    Ok(())
}

/// The person's one button: a draft family becomes a working one, as one undo step.
pub fn promote_family(ed: &mut crate::editor::Editor, name: &str) -> Result<(), String> {
    let id = ed
        .model
        .family_id(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no family {name}"))?;
    let f = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        m.atomic("family_promote", |m| m.promote_family(id))
    }));
    ed.run_op(f).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(w: f64) -> J {
        json!({"kind": "extrude",
               "profile": {"shape": "rect", "w": {"v": w, "unit": "m"},
                           "h": {"v": 1.0, "unit": "m"}},
               "frame": {"origin": {"x": 0, "y": 0, "z": 0, "unit": "m"},
                         "x": {"x": 1, "y": 0, "z": 0}, "z": {"x": 0, "y": 0, "z": 1}},
               "start": {"v": 0.0, "unit": "m"}, "end": {"v": 2.0, "unit": "m"}})
    }
    fn solid(w: f64) -> J {
        json!({"action": "solid", "level": null, "form": ext(w)})
    }

    #[test]
    fn bare_numbers_are_refused_but_zero_and_formulas_pass() {
        let none = Plain::new();
        assert!(bare(&json!(500), "p", &none).is_err());
        assert!(bare(&json!("500"), "p", &none).is_err());
        assert!(bare(&json!([1, 2]), "p", &none).unwrap_err().contains("p/0"));
        assert!(bare(&json!(0), "p", &none).is_ok());
        assert!(bare(&json!("500mm"), "p", &none).is_ok());
        assert!(bare(&json!("Length/2"), "p", &none).is_ok());
    }

    #[test]
    fn values_of_plain_parameters_need_no_unit_but_lengths_do() {
        let def = |extra: J| {
            json!({"name": "Stool",
                   "params": [{"name": "Legs", "type": "integer", "default": 4},
                              {"name": "Seat", "type": "length", "unit": "mm"}],
                   "forms": [{"name": "f", "extrude": {"rect": ["0", "0", "Seat", "Seat"]},
                              "z": ["0", "Seat"], "array": extra}]})
        };
        // a count filed under the name of a dimensionless parameter passes
        assert!(check_family(&def(json!({"Legs": 6, "Seat": "30mm"}))).is_ok());
        // the same number under a length parameter is a length without a unit
        let e = check_family(&def(json!({"Seat": 6}))).unwrap_err();
        assert!(e.contains("/forms/0/array/Seat"), "{e}");
        // a name that is no parameter gets no exemption
        assert!(check_family(&def(json!({"Rows": 3}))).is_err());
    }

    #[test]
    fn family_checks_name_unit_and_shape() {
        assert!(check_family(&json!([])).is_err());
        assert!(check_family(&json!({"name": "  "})).is_err());
        assert!(check_family(&json!({"name": "A"})).is_ok());
        let no_unit = json!({"name": "A", "params": [{"name": "L", "type": "length"}]});
        assert!(check_family(&no_unit).unwrap_err().contains("/params/0"));
        let ok = json!({"name": "A", "params": [{"name": "L", "type": "length", "unit": "mm"},
                                                {"name": "N", "type": "integer"}]});
        assert!(check_family(&ok).is_ok());
        let bare_ref = json!({"name": "A", "refs": [{"at": 300}]});
        assert!(check_family(&bare_ref).unwrap_err().contains("/refs/0/at"));
        let bare_dim = json!({"name": "A", "constraints": [{"dim": 5}]});
        assert!(check_family(&bare_dim).unwrap_err().contains("/constraints/0/dim"));
        let bare_form = json!({"name": "A", "forms": [{"name": "f", "depth": 5}]});
        assert!(check_family(&bare_form).unwrap_err().contains("/forms/0/depth"));
    }

    #[test]
    fn ops_errors_are_specific() {
        assert!(geo_ops(0, &[]).unwrap_err().contains("empty"));
        let many = vec![solid(1.0); MAX_GEO_OPS + 1];
        assert!(geo_ops(0, &many).unwrap_err().contains("at most"));
        assert!(geo_ops(0, &[json!(3)]).unwrap_err().contains("ops[0]"));
        let other = json!({"op": "delete", "action": "solid"});
        assert!(geo_ops(0, &[other]).unwrap_err().contains("not a geometry"));
        let far = json!({"action": "boolean", "kind": "cut", "a": {"by": "new", "slot": 5},
                         "b": {"by": "new", "slot": 0}, "keep_tool": false});
        let e = geo_ops(0, &[solid(1.0), far]).unwrap_err();
        assert!(e.contains("ops[1]") && e.contains("slot 5"), "{e}");
        let noslot = json!({"action": "boolean", "a": {"by": "new"}});
        assert!(geo_ops(0, &[noslot]).unwrap_err().contains("needs a slot"));
    }

    #[test]
    fn slots_are_offset_and_checked() {
        let cut = json!({"action": "boolean", "kind": "cut", "a": {"by": "new", "slot": 0},
                         "b": {"kind": "form", "form": ext(0.5)}, "keep_tool": false});
        let ops = geo_ops(7, &[solid(2.0), cut.clone()]).unwrap();
        assert!(matches!(&ops[0], Op::Geometry(GeoOp::Solid { slot: 7, .. })));
        assert!(check_ops(7, &[solid(2.0), cut]).is_ok());
        assert!(check_ops(0, &[json!({"action": "nonsense"})]).is_err());
    }

    /// Chat scenario: "a canopy through these three arches and three ridges, then a lighter
    /// net" -> a network surface and a rebuild of it in one call, named by its slot.
    #[test]
    fn chat_network_canopy_then_rebuild() {
        let p = |x: f64, y: f64, z: f64| json!({"x": x, "y": y, "z": z, "unit": "m"});
        let curve = |pts: Vec<J>| json!({"pts": pts, "fit": "through", "degree": 2});
        let k = [0.0, 5.0, 10.0];
        let h = |x: f64, y: f64| 6.0 + 0.04 * x * (10.0 - x) + 0.03 * y * (10.0 - y);
        let us: Vec<J> = k.iter().map(|&y| curve(k.iter().map(|&x| p(x, y, h(x, y))).collect())).collect();
        let vs: Vec<J> = k.iter().map(|&x| curve(k.iter().map(|&y| p(x, y, h(x, y))).collect())).collect();
        let level = json!({"by": "name", "cat": "level", "name": "L1"});
        let ff = json!({"action": "freeform", "level": level,
                        "surface": {"by": "network", "u_curves": us, "v_curves": vs,
                                    "degree": 3, "tolerance": {"v": 1.0, "unit": "mm"}},
                        "thickness": {"v": 100.0, "unit": "mm"}, "name": "Canopy"});
        let rb = json!({"action": "rebuild", "target": {"by": "new", "slot": 0},
                        "count_u": 6, "count_v": 6, "degree_v": 2});
        let ops = geo_ops(4, &[ff.clone(), rb.clone()]).unwrap();
        assert!(matches!(
            &ops[0],
            Op::Geometry(GeoOp::Freeform { slot: 4, surface: ddc_ops::SurfaceBy::Network { .. }, .. })
        ));
        match &ops[1] {
            Op::Geometry(GeoOp::Rebuild { target, count_u: 6, count_v: 6, degree_v: Some(2), .. }) => {
                assert_eq!(*target, ddc_ops::Ref::slot(4));
            }
            o => panic!("{o:?}"),
        }
        assert!(check_ops(4, &[ff, rb]).is_ok());
    }

    /// Chat scenario: "glaze that facade, 1.5 m panels, a row per floor, flat glass where it
    /// can" -> a freeform and a curtain system on it in one call.
    #[test]
    fn chat_facade_then_panelize() {
        let p = |x: f64, y: f64, z: f64| json!({"x": x, "y": y, "z": z, "unit": "m"});
        let row = |z: f64| vec![p(0.0, 0.0, z), p(5.0, 2.0, z), p(10.0, 0.0, z)];
        let ff = json!({"action": "freeform", "level": {"by": "name", "cat": "level", "name": "L1"},
                        "surface": {"by": "points", "rows": [row(0.0), row(12.0)], "degree_u": 1, "degree_v": 2},
                        "name": "Facade"});
        let mm = |v: f64| json!({"v": v, "unit": "mm"});
        let m = |v: f64| json!({"v": v, "unit": "m"});
        let pz = json!({"action": "panelize", "host": {"by": "new", "slot": 0},
                        "columns": {"by": "width", "width": mm(1500.0)},
                        "rows": {"by": "heights", "heights": [m(0.0), m(4.0), m(8.0), m(12.0)]},
                        "pattern": "quad", "planarize": 100,
                        "mullion": {"width": mm(60.0), "depth": mm(180.0)}, "glass": mm(28.0)});
        let ops = geo_ops(4, &[ff.clone(), pz.clone()]).unwrap();
        match &ops[1] {
            Op::Geometry(GeoOp::Panelize { slot: 5, host, rows: ddc_ops::PanelDiv::Heights { heights }, .. }) => {
                assert_eq!(*host, ddc_ops::Ref::slot(4));
                assert_eq!(heights.len(), 4);
            }
            o => panic!("{o:?}"),
        }
        assert!(check_ops(4, &[ff.clone(), pz.clone()]).is_ok());
        // a bare number is metres (models send it all the time); the card shows the unit.
        // The host stays in the call: alone, `new` slot 0 would name nothing.
        let mut bare = pz;
        bare["glass"] = json!(0.028);
        assert_eq!(check_ops(4, &[ff.clone(), bare.clone()]), Ok(()));
        match &geo_ops(4, &[ff, bare]).unwrap()[1] {
            Op::Geometry(GeoOp::Panelize { glass: Some(g), .. }) => assert!((g.to_m() - 0.028).abs() < 1e-9),
            o => panic!("{o:?}"),
        }
    }

    /// Chat scenario: "a 12 m dome over the hall, 150 concrete plus 100 insulation, with a
    /// skylight" -> a dome surface and a roof on it in one call.
    #[test]
    fn chat_dome_then_layered_roof() {
        let m = |v: f64| json!({"v": v, "unit": "m"});
        let mm = |v: f64| json!({"v": v, "unit": "mm"});
        let level = json!({"by": "name", "cat": "level", "name": "L1"});
        let dome = json!({"action": "freeform", "level": level,
                          "surface": {"by": "dome", "center": {"x": 0.0, "y": 0.0, "z": 6.0, "unit": "m"},
                                      "radius": m(6.0), "rise": m(3.0)},
                          "name": "Hall dome"});
        let p2 = |x: f64, y: f64| json!({"x": x, "y": y, "unit": "m"});
        let roof = json!({"action": "surface_shell", "host": {"by": "new", "slot": 0}, "kind": "roof",
                          "layers": [{"thickness": mm(150.0), "material": "Concrete"},
                                     {"thickness": mm(100.0), "material": "Mineral wool", "name": "Insulation"}],
                          "side": "normal",
                          "openings": [[p2(1.0, 1.0), p2(2.0, 1.0), p2(2.0, 2.0), p2(1.0, 2.0)]]});
        let ops = geo_ops(4, &[dome.clone(), roof.clone()]).unwrap();
        assert!(matches!(
            &ops[0],
            Op::Geometry(GeoOp::Freeform { slot: 4, surface: ddc_ops::SurfaceBy::Dome { .. }, .. })
        ));
        match &ops[1] {
            Op::Geometry(GeoOp::SurfaceShell { slot: 5, host, kind: ddc_ops::ShellKind::Roof, layers, openings: Some(o), .. }) => {
                assert_eq!(*host, ddc_ops::Ref::slot(4));
                assert_eq!((layers.len(), o.len(), o[0].len()), (2, 1, 4));
            }
            o => panic!("{o:?}"),
        }
        assert!(check_ops(4, &[dome.clone(), roof.clone()]).is_ok());
        // a bare number is a thickness in metres (the host stays in the call)
        let mut bare = roof.clone();
        bare["layers"][0]["thickness"] = json!(0.15);
        assert_eq!(check_ops(4, &[dome.clone(), bare.clone()]), Ok(()));
        let thick = |ops: Vec<Op>| match &ops[1] {
            Op::Geometry(GeoOp::SurfaceShell { layers, .. }) => layers[0].thickness.to_m(),
            o => panic!("{o:?}"),
        };
        assert!((thick(geo_ops(4, &[dome, bare]).unwrap()) - 0.15).abs() < 1e-9);
    }

    /// Chat scenario: "a diagrid dome 24 m across, 5 m high" -> one gridshell op with a slot,
    /// built in the model as one undo step.
    #[test]
    fn chat_diagrid_dome_is_one_gridshell_op() {
        let m = |v: f64| json!({"v": v, "unit": "m"});
        let level = json!({"by": "name", "cat": "level", "name": "L1"});
        let op = json!({"action": "gridshell", "level": level,
                        "host": {"shape": "dome", "center": {"x": 0.0, "y": 0.0, "z": 0.0, "unit": "m"},
                                 "radius": m(12.0), "rise": m(5.0)},
                        "kind": "diagrid", "count_u": 16, "count_v": 4,
                        "member": "CHS 88.9x5"});
        let ops = geo_ops(7, &[op.clone()]).unwrap();
        assert!(matches!(
            &ops[0],
            Op::Geometry(GeoOp::Gridshell { slot: 7, count_u: 16, count_v: 4, .. })
        ));
        assert!(check_ops(7, &[op]).is_ok());
        let env = envelope_of(ops, "geometry").unwrap();
        let mut model = ddc_model::Model::open(":memory:").unwrap();
        model.add_level("L1", 0.0).unwrap();
        let r = model.apply_envelope(&env).unwrap();
        let made = r.created.iter().find(|c| c.0 == 7).map(|c| c.1.clone()).unwrap();
        let ddc_ops::Ref::Id { cat: ddc_ops::Cat::Gridshell, id } = made else { panic!("{made:?}") };
        assert!(model.gridshell_nodes(id).unwrap().len() > 30);
        assert!(model.undo().is_ok());
        assert!(model.gridshells(None).unwrap().is_empty());
    }

    /// Chat scenario G39: "a parabolic dish antenna 18 m across, 4.5 m deep, tilted 30
    /// degrees" -> one gridshell op on a paraboloid host, rim and hub rings as asked.
    #[test]
    fn chat_parabolic_dish_is_one_gridshell_on_a_paraboloid() {
        let m = |v: f64| json!({"v": v, "unit": "m"});
        let level = json!({"by": "name", "cat": "level", "name": "L1"});
        let op = json!({"action": "gridshell", "level": level,
                        "host": {"shape": "paraboloid", "vertex": {"x": 0.0, "y": 0.0, "z": 8.0, "unit": "m"},
                                 "radius": m(9.0), "depth": m(4.5), "hub": m(1.2),
                                 "tilt": {"v": 30.0, "unit": "deg"}, "azimuth": {"v": 90.0, "unit": "deg"}},
                        "kind": "gridshell", "pattern": "triangular", "count_u": 32, "count_v": 8,
                        "member": "CHS 88.9x5"});
        let ops = geo_ops(3, &[op.clone()]).unwrap();
        assert!(matches!(
            &ops[0],
            Op::Geometry(GeoOp::Gridshell { slot: 3, host: ddc_ops::LatticeHost::Paraboloid { .. }, .. })
        ));
        assert!(check_ops(3, &[op]).is_ok());
        let env = envelope_of(ops, "geometry").unwrap();
        let mut model = ddc_model::Model::open(":memory:").unwrap();
        model.add_level("L1", 0.0).unwrap();
        let r = model.apply_envelope(&env).unwrap();
        let made = r.created.iter().find(|c| c.0 == 3).map(|c| c.1.clone()).unwrap();
        let ddc_ops::Ref::Id { cat: ddc_ops::Cat::Gridshell, id } = made else { panic!("{made:?}") };
        let nodes = model.gridshell_nodes(id).unwrap();
        assert_eq!(nodes.len(), 32 * 9);
        // the axis leans toward +y: the rim reaches higher on the -y side and lower on +y
        let (lo, hi) = nodes.iter().fold((f64::MAX, f64::MIN), |(a, b), n| (a.min(n.p.z), b.max(n.p.z)));
        let tilt = 30f64.to_radians();
        assert!((hi - (8.0 + 4.5 * tilt.cos() + 9.0 * tilt.sin())).abs() < 1e-6, "{hi}");
        assert!(lo < 8.0, "{lo}");
        assert_eq!(nodes.iter().filter(|n| n.fixed).count(), 32);
        assert!(model.gridshell_bars(id).unwrap().len() > 3 * 32 * 8);
    }
}
