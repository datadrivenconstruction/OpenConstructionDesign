// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Propose tools of the wall type library: `edit_wall_type` (one layer added, removed or
//! resized) and `copy_types_from` (wall types of another project file). Both add a plan item;
//! nothing reaches the model before Apply. Replies count the placed walls they resize.
use super::fmt::{self, Out};
use super::materials;
use super::plan::PlanOp;
use super::session::{Core, Done};
use super::tools_write::why_of;
use super::type_lib_plan::{self as tl, LayerAct, WallTypeOp};
use ddc_ai::CallCx;
use ddc_model::wall_types::ImportFate;
use ddc_model::{Align, Conflict};
use serde_json::{json, Value as J};

fn title_of(a: &J) -> String {
    fmt::text(a["title"].as_str().unwrap_or_default())
}

fn align_of(a: &J) -> Result<Align, String> {
    match a["align"].as_str() {
        None => Ok(Align::Core),
        Some(s) => Align::parse(s).ok_or_else(|| "align must be core or exterior".to_string()),
    }
}

fn thickness_of(a: &J) -> Result<f64, String> {
    a["thickness"]
        .as_f64()
        .filter(|t| t.is_finite() && *t > 0.0)
        .ok_or_else(|| "thickness must be a positive number of metres".to_string())
}

/// 0-based layer index from a 1-based `layer`, at most `max` (1-based).
fn layer_of(a: &J, max: usize) -> Result<Option<usize>, String> {
    match &a["layer"] {
        J::Null => Ok(None),
        v => match v.as_u64() {
            Some(k) if k >= 1 && k as usize <= max => Ok(Some(k as usize - 1)),
            _ => Err(format!("layer must be a number from 1 to {max}")),
        },
    }
}

fn reply(c: &Core, added: (u32, (usize, usize, usize)), extra: &[(&str, J)]) -> Done {
    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(added.0));
    let (n, ch, d) = added.1;
    o.put("changes", &json!({"created": n, "changed": ch, "deleted": d}));
    for (k, v) in extra {
        o.put(k, v);
    }
    o.finish().into()
}

pub fn edit_wall_type(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let m = &c.ed.model;
    let ty = tl::resolve_type(m, a["type"].as_str().ok_or("type must be text")?)?;
    let n = m.wall_type_layers_full(ty).map_err(|e| e.to_string())?.len();
    let act = match a["action"].as_str() {
        Some("add") => {
            let at = layer_of(a, n + 1)?;
            let text = a["material"].as_str().ok_or("material is needed to add a layer")?;
            let mat = materials::canon(&materials::names(m), &fmt::visible(text));
            let layer = tl::new_layer(&mat, thickness_of(a)?, a["function"].as_str())?;
            LayerAct::Add { at, layer }
        }
        Some("remove") => LayerAct::Remove(layer_of(a, n)?.ok_or("layer is needed")?),
        Some("resize") => {
            LayerAct::Resize(layer_of(a, n)?.ok_or("layer is needed")?, thickness_of(a)?)
        }
        _ => return Err("action must be add, remove or resize".to_string()),
    };
    let op = WallTypeOp::Layer {
        ty,
        act,
        align: align_of(a)?,
    };
    let (hit, walls) = tl::walls_hit(m, &op)?;
    let name = tl::type_name(m, ty)?;
    let added = c.add_item(title_of(a), why_of(a), PlanOp::WallType(op), cx.actor)?;
    let extra = [
        ("wall_type", json!(format!("wall_type:{ty}"))),
        ("name", json!(fmt::text(&name))),
        ("walls_resized", json!(format!("{hit} of {walls}"))),
    ];
    Ok(reply(c, added, &extra))
}

pub fn copy_types_from(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let path = a["path"].as_str().ok_or("path must be text")?.trim();
    // only project files: the tool reads wall types, never any other file
    if !path.to_ascii_lowercase().ends_with(".ddc") {
        return Err("path must be a project file (.ddc)".to_string());
    }
    let pack = ddc_model::read_type_pack(&super::tools_write::local_file_arg(path, c.project_file().as_deref())?)
        .map_err(|e| format!("cannot read {}: {e}", fmt::text(path)))?;
    let names: Vec<String> = a["names"]
        .as_array()
        .map(|v| v.iter().filter_map(J::as_str).map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let unknown: Vec<&String> =
        names.iter().filter(|n| !pack.types.iter().any(|t| t.name.trim() == n.as_str())).collect();
    if !unknown.is_empty() {
        let list: Vec<String> = unknown.iter().map(|s| fmt::text(s)).collect();
        return Err(format!("not in that file: {}", list.join(", ")));
    }
    let rule = match a["on_conflict"].as_str() {
        None => Conflict::Rename,
        Some(s) => Conflict::parse(s).ok_or("on_conflict must be rename, skip or overwrite")?,
    };
    let rows = c.ed.model.plan_type_import(&pack).map_err(|e| e.to_string())?;
    let picked: Vec<_> = rows
        .iter()
        .filter(|r| names.is_empty() || names.iter().any(|n| *n == r.name))
        .collect();
    let count = |f: ImportFate| picked.iter().filter(|r| r.fate == f).count();
    let (new, same, conflict) = (count(ImportFate::New), count(ImportFate::Same), count(ImportFate::Conflict));
    let total = pack.types.len();
    let op = WallTypeOp::CopyFrom {
        pack,
        names,
        rule,
        align: align_of(a)?,
    };
    let (hit, _) = tl::walls_hit(&c.ed.model, &op)?;
    let added = c.add_item(title_of(a), why_of(a), PlanOp::WallType(op), cx.actor)?;
    let extra = [
        (
            "types",
            json!({"new": new, "same": same, "conflict": conflict, "picked": picked.len(), "in_file": total}),
        ),
        ("on_conflict", json!(rule.key())),
        ("walls_resized", json!(hit)),
    ];
    Ok(reply(c, added, &extra))
}
