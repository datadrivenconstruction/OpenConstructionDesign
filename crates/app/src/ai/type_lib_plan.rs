// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI plan items of the wall type library: change one layer of a wall type, copy wall types
//! from another project file. Each compiles, on the copy, into one `TypeLibStep` with every
//! value resolved, so Apply replays exactly what the preview showed.
use super::exec::ModelStep;
use super::plan::{push, CompileCx, PlanError};
use ddc_model::wall_types::{check_type, ImportFate, LayerFn, TypeLayer, TypeLibStep};
use ddc_model::{Align, Conflict, Model, TypePack};
use std::collections::HashSet;

/// One layer change; indices are 0-based, exterior first.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerAct {
    /// Inserted at `at` (None: after the last layer).
    Add { at: Option<usize>, layer: TypeLayer },
    Remove(usize),
    Resize(usize, f64),
}

#[derive(Clone, Debug)]
pub enum WallTypeOp {
    Layer {
        ty: i64,
        act: LayerAct,
        align: Align,
    },
    CopyFrom {
        pack: TypePack,
        /// Type names to copy; empty: all.
        names: Vec<String>,
        rule: Conflict,
        align: Align,
    },
}

pub fn type_name(m: &Model, ty: i64) -> Result<String, String> {
    m.wall_type_usage()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|t| t.id == ty)
        .map(|t| t.name)
        .ok_or_else(|| format!("no wall type {ty}"))
}

/// Wall type id of a reference (`wall_type:3`) or an exact name.
pub fn resolve_type(m: &Model, s: &str) -> Result<i64, String> {
    let s = s.trim();
    let all = m.wall_type_usage().map_err(|e| e.to_string())?;
    if let Some(id) = s.strip_prefix("wall_type:").and_then(|n| n.trim().parse::<i64>().ok()) {
        return all
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.id)
            .ok_or_else(|| format!("no wall type {id}"));
    }
    all.iter()
        .find(|t| t.name == s)
        .map(|t| t.id)
        .ok_or_else(|| format!("no wall type named {s}; list_types cat=wall shows them"))
}

/// The layers of `ty` after `act`, checked against the type rules.
pub fn layers_after(m: &Model, ty: i64, act: &LayerAct) -> Result<Vec<TypeLayer>, String> {
    let mut ls = m.wall_type_layers_full(ty).map_err(|e| e.to_string())?;
    let n = ls.len();
    let out_of = |i: usize| format!("layer {} is out of 1..={n}", i + 1);
    match act {
        LayerAct::Add { at, layer } => {
            let k = at.unwrap_or(n);
            if k > n {
                return Err(out_of(k));
            }
            ls.insert(k, layer.clone());
        }
        LayerAct::Remove(i) => {
            if *i >= n {
                return Err(out_of(*i));
            }
            if n == 1 {
                return Err("a wall type keeps at least one layer".into());
            }
            ls.remove(*i);
        }
        LayerAct::Resize(i, t) => {
            let l = ls.get_mut(*i).ok_or_else(|| out_of(*i))?;
            l.thickness = *t;
        }
    }
    let name = type_name(m, ty)?;
    check_type(&name, &ls).map_err(|e| e.to_string())?;
    Ok(ls)
}

/// The resolved model step of `op` on the model as it is now.
pub fn step_of(m: &Model, op: &WallTypeOp) -> Result<TypeLibStep, String> {
    Ok(match op {
        WallTypeOp::Layer { ty, act, align } => TypeLibStep::Edit {
            id: *ty,
            name: type_name(m, *ty)?,
            layers: layers_after(m, *ty, act)?,
            align: *align,
        },
        WallTypeOp::CopyFrom {
            pack,
            names,
            rule,
            align,
        } => TypeLibStep::Import {
            pack: pack.clone(),
            pick: names.clone(),
            rule: *rule,
            align: *align,
        },
    })
}

/// Placed walls whose thickness the step changes: (changed, walls of the touched types).
pub fn walls_hit(m: &Model, op: &WallTypeOp) -> Result<(usize, usize), String> {
    let e = |e: ddc_model::Error| e.to_string();
    match op {
        WallTypeOp::Layer { ty, act, .. } => {
            let n = m.walls_of_type(*ty).map_err(e)?.len();
            let old = ddc_model::wall_types::total(&m.wall_type_layers_full(*ty).map_err(e)?);
            let new = ddc_model::wall_types::total(&layers_after(m, *ty, act)?);
            Ok((if (old - new).abs() > 1e-9 { n } else { 0 }, n))
        }
        WallTypeOp::CopyFrom { pack, names, rule, .. } => {
            let rows = m.plan_type_import(pack).map_err(e)?;
            let picked = |n: &str| names.is_empty() || names.iter().any(|p| p.trim() == n);
            let hit: usize = rows
                .iter()
                .filter(|r| picked(&r.name) && r.fate == ImportFate::Conflict)
                .map(|r| r.walls)
                .sum();
            Ok((if *rule == Conflict::Overwrite { hit } else { 0 }, hit))
        }
    }
}

fn declare_type(cx: &mut CompileCx<'_>, ty: i64) -> Result<(), PlanError> {
    cx.declared.insert(("wall_type".to_string(), ty));
    for w in cx.ed.model.walls_of_type(ty)? {
        cx.declared.insert(("wall".to_string(), w));
    }
    Ok(())
}

pub fn compile(
    cx: &mut CompileCx<'_>,
    op: &WallTypeOp,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let step = step_of(&cx.ed.model, op)?;
    match op {
        WallTypeOp::Layer { ty, .. } => {
            declare_type(cx, *ty)?;
            push(cx, out, ModelStep::TypeLib(step))?;
        }
        WallTypeOp::CopyFrom { pack, names, rule, .. } => {
            let before: HashSet<i64> =
                cx.ed.model.wall_type_usage()?.iter().map(|t| t.id).collect();
            if *rule == Conflict::Overwrite {
                let rows = cx.ed.model.plan_type_import(pack)?;
                let usage = cx.ed.model.wall_type_usage()?;
                for r in rows.iter().filter(|r| r.fate == ImportFate::Conflict) {
                    if !names.is_empty() && !names.iter().any(|p| p.trim() == r.name) {
                        continue;
                    }
                    if let Some(t) = usage.iter().find(|t| t.name == r.name) {
                        declare_type(cx, t.id)?;
                    }
                }
            }
            push(cx, out, ModelStep::TypeLib(step))?;
            for t in cx.ed.model.wall_type_usage()? {
                if !before.contains(&t.id) {
                    cx.declared.insert(("wall_type".to_string(), t.id));
                }
            }
        }
    }
    Ok(())
}

/// A layer from tool arguments: material, thickness in metres, an optional function.
pub fn new_layer(
    material: &str,
    thickness: f64,
    function: Option<&str>,
) -> Result<TypeLayer, String> {
    let f = match function {
        Some(s) => LayerFn::parse(s).ok_or_else(|| format!("unknown layer function {s}"))?,
        None => LayerFn::guess(material),
    };
    Ok(TypeLayer::new(material, thickness, f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::LayerSpec;

    fn model() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let ls = [
            LayerSpec { material: "plaster".into(), thickness: 0.02 },
            LayerSpec { material: "brick".into(), thickness: 0.25 },
        ];
        let t = m.add_wall_type("AI test", &ls).unwrap();
        (m, t)
    }

    #[test]
    fn layer_actions_resolve_and_refuse_bad_input() {
        let (m, t) = model();
        let add = LayerAct::Add { at: Some(0), layer: new_layer("render", 0.015, None).unwrap() };
        let ls = layers_after(&m, t, &add).unwrap();
        assert_eq!(ls.len(), 3);
        assert_eq!(ls[0].material, "render");
        let ls = layers_after(&m, t, &LayerAct::Resize(1, 0.3)).unwrap();
        assert!((ls[1].thickness - 0.3).abs() < 1e-12);
        assert_eq!(layers_after(&m, t, &LayerAct::Remove(0)).unwrap().len(), 1);
        assert!(layers_after(&m, t, &LayerAct::Remove(5)).is_err());
        assert!(layers_after(&m, t, &LayerAct::Resize(0, 0.0)).is_err());
        assert!(layers_after(&m, t, &LayerAct::Resize(0, -1.0)).is_err());
        assert!(new_layer("x", 0.1, Some("nope")).is_err());
        assert_eq!(new_layer("x", 0.1, Some("membrane")).unwrap().function, LayerFn::Membrane);
    }

    #[test]
    fn types_resolve_by_reference_or_name() {
        let (m, t) = model();
        assert_eq!(resolve_type(&m, &format!("wall_type:{t}")).unwrap(), t);
        assert_eq!(resolve_type(&m, " AI test ").unwrap(), t);
        assert!(resolve_type(&m, "wall_type:99999").is_err());
        assert!(resolve_type(&m, "missing").is_err());
    }

    #[test]
    fn the_step_carries_resolved_layers_and_replays() {
        let (mut m, t) = model();
        let op = WallTypeOp::Layer { ty: t, act: LayerAct::Resize(1, 0.3), align: Align::Exterior };
        let s = step_of(&m, &op).unwrap();
        assert_eq!(walls_hit(&m, &op).unwrap(), (0, 0));
        m.apply_type_lib(&s).unwrap();
        assert!((m.wall_type_layers_full(t).unwrap()[1].thickness - 0.3).abs() < 1e-12);
        m.undo().unwrap();
        assert!((m.wall_type_layers_full(t).unwrap()[1].thickness - 0.25).abs() < 1e-12);
    }
}
