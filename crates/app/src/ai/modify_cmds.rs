// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Handlers of the modify, level and room commands: they check the arguments, add one plan item
//! (`modify_plan`) and answer what it created. Lengths are metres, angles degrees
//! counter-clockwise.

use super::exec::ModelStep;
use super::modify_plan::{cat_of_table, levels, JoinMode, ModifyAct, ToolAct, P2};
use super::plan::PlanOp;
use super::session::{Core, Done};
use super::tools_write::{added_of, reply, target_of, title_of, why_of};
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_ai::CallCx;
use serde_json::Value as J;

/// Most copies one call may ask for; the array tool keeps the same cap.
pub const MAX_COPIES: usize = 200;

fn bad(path: &str, why: &str) -> String {
    format!("args.{path}: {why}")
}

fn finite(v: &J) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}

fn numbers(v: &J) -> Option<Vec<f64>> {
    v.as_array()?.iter().map(finite).collect()
}

fn pair(v: &J) -> Option<P2> {
    match numbers(v)?.as_slice() {
        [x, y] => Some((*x, *y)),
        _ => None,
    }
}

fn point(a: &J, k: &str) -> Result<P2, String> {
    pair(&a[k]).ok_or_else(|| bad(k, "needs [x, y] in metres"))
}

fn number(a: &J, k: &str, unit: &str) -> Result<f64, String> {
    finite(&a[k]).ok_or_else(|| bad(k, &format!("needs a number in {unit}")))
}

fn flag(a: &J, k: &str, default: bool) -> Result<bool, String> {
    match &a[k] {
        J::Null => Ok(default),
        v => v.as_bool().ok_or_else(|| bad(k, "needs true or false")),
    }
}

fn count(a: &J) -> Result<usize, String> {
    match a["count"].as_u64() {
        Some(n) if (2..=MAX_COPIES as u64 + 1).contains(&n) => Ok(n as usize),
        _ => Err(bad("count", &format!("needs a whole number 2..={}", MAX_COPIES + 1))),
    }
}

/// Adds the item; the answer names every element it created, such as `wall:12`.
pub(super) fn add_op(c: &mut Core, a: &J, cx: &CallCx<'_>, op: PlanOp) -> Result<Done, String> {
    let r = c.add_item(title_of(a), why_of(a), op, cx.actor)?;
    let created = c
        .plan
        .steps
        .iter()
        .filter(|s| s.0 == r.0)
        .flat_map(|s| s.1.iter())
        .filter_map(|s| match s {
            ModelStep::Tool { expect, .. } => Some(expect),
            _ => None,
        })
        .flatten()
        .map(|(table, id)| format!("{}:{id}", cat_of_table(table)))
        .collect();
    Ok(reply(c, &added_of(r, created)))
}

fn add(c: &mut Core, a: &J, cx: &CallCx<'_>, act: ModifyAct) -> Result<Done, String> {
    let target = target_of(c, &a["target"]).map_err(|e| bad("target", &e))?;
    add_op(c, a, cx, PlanOp::Modify { target, act })
}

/// A level argument: `level:3` or the exact name.
fn level_arg(c: &Core, a: &J, k: &str) -> Result<i64, String> {
    let s = a[k].as_str().map(str::trim).ok_or_else(|| bad(k, "needs level:ID or a name"))?;
    let all = levels(&c.ed.model).map_err(|e| e.to_string())?;
    let id = match s.strip_prefix("level:") {
        Some(n) => n.trim().parse::<i64>().ok().filter(|id| all.iter().any(|l| l.0 == *id)),
        None => all.iter().find(|l| l.1 == s).map(|l| l.0),
    };
    id.ok_or_else(|| {
        let why = format!("no level {}; list_levels shows them", super::fmt::text(s));
        CmdError::wire(ErrorCode::NotFound, why)
    })
}

fn name_arg(a: &J, k: &str) -> Result<Option<String>, String> {
    match &a[k] {
        J::Null => Ok(None),
        v => match v.as_str().map(str::trim) {
            Some(s) if !s.is_empty() && s.chars().count() <= 80 => Ok(Some(s.to_string())),
            _ => Err(bad(k, "needs a name of 1..=80 characters")),
        },
    }
}

pub fn flip_openings(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let hand = match a["flip"].as_str() {
        Some("hand") => true,
        Some("facing") => false,
        _ => return Err(bad("flip", "needs hand or facing")),
    };
    add(c, a, cx, ModifyAct::Flip { hand })
}

pub fn join_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let mode = match a["action"].as_str() {
        Some("join") => JoinMode::Join,
        Some("unjoin") => JoinMode::Unjoin,
        Some("switch") => JoinMode::Switch,
        _ => return Err(bad("action", "needs join, unjoin or switch")),
    };
    add(c, a, cx, ModifyAct::Join(mode))
}

pub fn create_level(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let name = name_arg(a, "name")?.ok_or_else(|| bad("name", "needs a name"))?;
    let elevation = number(a, "elevation", "metres")?;
    let all = levels(&c.ed.model).map_err(|e| e.to_string())?;
    if all.iter().any(|l| l.1 == name) {
        return Err(bad("name", "a level of that name exists already"));
    }
    add_op(c, a, cx, PlanOp::Tool(ToolAct::AddLevel { name, elevation }))
}

pub fn edit_level(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let id = level_arg(c, a, "level")?;
    let name = name_arg(a, "name")?;
    let elevation = match &a["elevation"] {
        J::Null => None,
        _ => Some(number(a, "elevation", "metres")?),
    };
    if name.is_none() && elevation.is_none() {
        return Err(bad("name", "give a new name, a new elevation or both"));
    }
    add_op(c, a, cx, PlanOp::Tool(ToolAct::EditLevel { id, name, elevation }))
}

pub fn delete_level(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let id = level_arg(c, a, "level")?;
    add_op(c, a, cx, PlanOp::Tool(ToolAct::DeleteLevel { id }))
}

pub fn place_rooms(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let level = level_arg(c, a, "level")?;
    let height = match &a["height"] {
        // the room tool's own default: the editor's storey height
        J::Null => c.ed.height,
        _ => number(a, "height", "metres")?,
    };
    if height <= 0.0 {
        return Err(bad("height", "needs a height above zero"));
    }
    let name = name_arg(a, "name")?.unwrap_or_default();
    add_op(c, a, cx, PlanOp::Tool(ToolAct::PlaceRooms { level, height, name }))
}

pub fn place_footings(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let level = level_arg(c, a, "level")?;
    add_op(c, a, cx, PlanOp::Tool(ToolAct::FootingsUnderColumns { level }))
}

/// Most survey points one call may carry.
pub const MAX_TERRAIN_POINTS: usize = 5000;

fn ring_arg(a: &J, k: &str) -> Result<Vec<P2>, String> {
    match &a[k] {
        J::Null => Ok(vec![]),
        v => {
            let ring: Option<Vec<P2>> = v.as_array().and_then(|r| r.iter().map(pair).collect());
            match ring {
                Some(r) if r.len() >= 3 => Ok(r),
                _ => Err(bad(k, "needs three or more [x, y] points in metres")),
            }
        }
    }
}

/// The terrain's survey points: as given, or the boundary's corners on a plane through `z` at
/// the boundary's centre that falls `slope` % towards `downhill` (degrees from +x).
fn terrain_points(a: &J, boundary: &[P2]) -> Result<Vec<(f64, f64, f64)>, String> {
    if !a["points"].is_null() {
        let pts: Option<Vec<(f64, f64, f64)>> = a["points"].as_array().and_then(|r| {
            r.iter()
                .map(|p| match numbers(p)?.as_slice() {
                    [x, y, z] => Some((*x, *y, *z)),
                    _ => None,
                })
                .collect()
        });
        return match pts {
            Some(p) if (3..=MAX_TERRAIN_POINTS).contains(&p.len()) => Ok(p),
            _ => Err(bad("points", &format!("needs 3..={MAX_TERRAIN_POINTS} [x, y, z] points in metres"))),
        };
    }
    if boundary.is_empty() {
        return Err(bad("points", "give survey points, or a boundary with z and an optional slope"));
    }
    let z = match &a["z"] {
        J::Null => 0.0,
        _ => number(a, "z", "metres")?,
    };
    let slope = match &a["slope"] {
        J::Null => 0.0,
        _ => number(a, "slope", "percent")?,
    };
    if !(0.0..=100.0).contains(&slope) {
        return Err(bad("slope", "needs 0..=100 percent"));
    }
    let dir = match &a["downhill"] {
        J::Null => 0.0,
        _ => number(a, "downhill", "degrees")?,
    }
    .to_radians();
    let n = boundary.len() as f64;
    let c = boundary.iter().fold((0.0, 0.0), |s, p| (s.0 + p.0 / n, s.1 + p.1 / n));
    let fall = slope / 100.0;
    Ok(boundary
        .iter()
        .map(|p| (p.0, p.1, z - fall * ((p.0 - c.0) * dir.cos() + (p.1 - c.1) * dir.sin())))
        .collect())
}

/// A toposolid as the terrain tool makes it: earth, the tool's thickness and name.
pub fn create_terrain(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let boundary = ring_arg(a, "boundary")?;
    let points = terrain_points(a, &boundary)?;
    let thickness = match &a["thickness"] {
        J::Null => c.ed.topo.opts.thickness,
        _ => number(a, "thickness", "metres")?,
    };
    if thickness <= 0.0 {
        return Err(bad("thickness", "needs a thickness above zero"));
    }
    let name = match name_arg(a, "name")? {
        Some(n) => n,
        None => {
            let n = c.ed.model.toposolids().map(|l| l.len()).unwrap_or(0) + 1;
            format!("{} {n}", c.ed.topo.t("topo.default_name"))
        }
    };
    let spec = ddc_model::TopoSpec {
        name,
        thickness,
        material: crate::topo::EARTH.to_string(),
        points,
        breaklines: vec![],
        boundary,
    };
    add_op(c, a, cx, PlanOp::Tool(ToolAct::Terrain(spec)))
}

/// `set_material`: the look of a project material by name (a new name becomes a material).
pub fn set_material(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let name = name_arg(a, "name")?.ok_or_else(|| bad("name", "needs a material name"))?;
    let name = super::materials::canon(&super::materials::names(&c.ed.model), &name);
    let look = a["look"].as_str().map(|s| s.trim().to_ascii_lowercase());
    let color = match &a["color"] {
        J::Null => None,
        v => Some(
            v.as_str()
                .and_then(|s| s.trim().strip_prefix('#'))
                .filter(|h| h.len() == 6)
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .ok_or_else(|| bad("color", "needs #RRGGBB"))?,
        ),
    };
    let texture = match a["texture"].as_str() {
        None => None,
        Some(k) => {
            let tile = match &a["tile"] {
                J::Null => None,
                _ => Some(number(a, "tile", "metres")?).filter(|t| *t > 0.0),
            };
            Some((k.trim().to_ascii_lowercase(), tile))
        }
    };
    if look.is_none() && color.is_none() && texture.is_none() {
        return Err(bad("look", "give look, color or texture"));
    }
    add_op(c, a, cx, PlanOp::Tool(ToolAct::SetMaterial { name, look, color, texture }))
}

pub fn move_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let by = match numbers(&a["by"]).as_deref() {
        Some([x, y]) => (*x, *y, 0.0),
        Some([x, y, z]) => (*x, *y, *z),
        _ => return Err(bad("by", "needs [dx, dy] or [dx, dy, dz] in metres")),
    };
    if by.0.hypot(by.1).hypot(by.2) < 1e-9 {
        return Err(bad("by", "a move by zero changes nothing"));
    }
    add(c, a, cx, ModifyAct::Move { by })
}

pub fn copy_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let list = a["offsets"].as_array().ok_or_else(|| bad("offsets", "needs [[dx, dy], ...]"))?;
    if list.is_empty() || list.len() > MAX_COPIES {
        return Err(bad("offsets", &format!("needs 1..={MAX_COPIES} offsets")));
    }
    let mut by = Vec::with_capacity(list.len());
    for (i, o) in list.iter().enumerate() {
        let p = match numbers(o).as_deref() {
            Some([x, y]) => (*x, *y, 0.0),
            Some([x, y, z]) => (*x, *y, *z),
            _ => return Err(bad(&format!("offsets[{i}]"), "needs [dx, dy] or [dx, dy, dz] in metres")),
        };
        if p.0.hypot(p.1).hypot(p.2) < 1e-9 {
            return Err(bad(&format!("offsets[{i}]"), "a copy in place would overlap"));
        }
        by.push(p);
    }
    add(c, a, cx, ModifyAct::Copy { by })
}

pub fn rotate_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let center = point(a, "center")?;
    let deg = number(a, "angle", "degrees")?;
    let turn = deg.rem_euclid(360.0);
    if turn < 1e-9 || 360.0 - turn < 1e-9 {
        return Err(bad("angle", "a whole turn changes nothing"));
    }
    let copy = flag(a, "copy", false)?;
    add(c, a, cx, ModifyAct::Rotate { center, deg, copy })
}

pub fn mirror_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let (p, q) = (point(a, "a")?, point(a, "b")?);
    if (p.0 - q.0).hypot(p.1 - q.1) < 1e-6 {
        return Err(bad("b", "the axis needs two distinct points"));
    }
    let copy = flag(a, "copy", false)?;
    add(c, a, cx, ModifyAct::Mirror { a: p, b: q, copy })
}

pub fn scale_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let center = point(a, "center")?;
    let factor = number(a, "factor", "times")?;
    if factor <= 0.0 || (factor - 1.0).abs() < 1e-9 {
        return Err(bad("factor", "needs a factor above zero other than 1"));
    }
    let copy = flag(a, "copy", false)?;
    add(c, a, cx, ModifyAct::Scale { center, factor, copy })
}

pub fn array_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let n = count(a)?;
    let act = match a["mode"].as_str() {
        Some("linear") => {
            let step = point(a, "step")?;
            if step.0.hypot(step.1) < 1e-9 {
                return Err(bad("step", "needs a step other than zero"));
            }
            ModifyAct::ArrayLinear { count: n, step, fit: flag(a, "fit", false)? }
        }
        Some("radial") => {
            let center = point(a, "center")?;
            let deg = number(a, "angle", "degrees")?;
            if deg.abs() < 1e-9 || deg.abs() > 360.0 {
                return Err(bad("angle", "needs a total angle in 0..360 degrees, not 0"));
            }
            ModifyAct::ArrayRadial { count: n, center, deg }
        }
        _ => return Err(bad("mode", "needs linear or radial")),
    };
    add(c, a, cx, act)
}

pub fn offset_wall(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let dist = number(a, "distance", "metres")?;
    if dist <= 0.0 {
        return Err(bad("distance", "needs a distance above zero"));
    }
    let left = match a["side"].as_str() {
        Some("left") => true,
        Some("right") => false,
        _ => return Err(bad("side", "needs left or right of the wall's start-to-end axis")),
    };
    add(c, a, cx, ModifyAct::Offset { dist, left })
}

pub fn split_wall(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    match (a.get("at"), a.get("parts")) {
        (Some(_), Some(_)) => Err(bad("parts", "give either at or parts, not both")),
        (None, Some(p)) => match p.as_u64() {
            Some(n @ 2..=100) => add(c, a, cx, ModifyAct::Divide { parts: n as usize }),
            _ => Err(bad("parts", "needs a whole number 2..=100")),
        },
        _ => {
            let at = point(a, "at")?;
            add(c, a, cx, ModifyAct::Split { at })
        }
    }
}

pub fn pin_elements(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let on = a["pinned"].as_bool().ok_or_else(|| bad("pinned", "needs true or false"))?;
    add(c, a, cx, ModifyAct::Pin(on))
}
