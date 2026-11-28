// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `create` of doors, windows, rooms, slabs, roofs and columns (docs/design/AI_NATIVE.md, "Lowering").
//!
//! Each kind runs the model function the editor tools use (`add_opening_with`, `add_room`,
//! `add_slab`, `add_column`). Field names are the ones the XLSX adapter emits, so an envelope
//! and the legacy import give the same rows. Plan points take z = 0 or the level elevation.

use ddc_ops::{Cat, Dim, Field, NewEl, Place};
use rusqlite::Connection;

use super::set::{flag_of, len, opt_len, positive_len};
use super::{along_host, at_point, bad_value, el_name, level_row, resolve_as, unsupported, Fail, Run, EPS};
use crate::{ColumnSpec, Model, OpeningSpec, RoofKind, RoofSpec, RoomSpec, SlabSpec, Swing};

/// Room height until a later `set` gives one: the wire room has no height row.
pub(super) const ROOM_HEIGHT: f64 = 3.0;

// `sill` as a field is the door form (a window has the typed `sill`)
const OPENING_FIELDS: &[&str] = &["width", "height", "swing_side", "swing_hinge", "sill"];
const SLAB_FIELDS: &[&str] = &["holes"];
const ROOF_FIELDS: &[&str] = &["overhang", "offset", "fascia", "gables"];
const COLUMN_FIELDS: &[&str] = &["base_offset", "top_offset", "structural", "top_at_x", "top_at_y"];

pub(super) fn create(m: &mut Model, run: &mut Run, slot: u32, el: &NewEl) -> Result<(), Fail> {
    match el {
        NewEl::Door { .. } | NewEl::Window { .. } => opening(m, run, slot, el),
        NewEl::Room { .. } => room(m, run, slot, el),
        NewEl::Slab { .. } => slab(m, run, slot, el),
        NewEl::Column { .. } => column(m, run, slot, el),
        NewEl::Roof { .. } => roof(m, run, slot, el),
        other => Err(unsupported(format!("creating `{}` is not lowered yet", el_name(other)))),
    }
}

/// The fields of one new element, each name known and given once.
struct Picked<'a>(Vec<&'a Field>);

impl<'a> Picked<'a> {
    fn get(&self, name: &str) -> Option<&'a Field> {
        self.0.iter().copied().find(|f| f.name == name)
    }

    fn need(&self, el: &str, name: &str) -> Result<&'a Field, Fail> {
        self.get(name).ok_or_else(|| bad_value(format!("new {el} needs `{name}`")).at("/el/fields"))
    }

    fn len_or(&self, name: &str, default: f64) -> Result<f64, Fail> {
        self.get(name).map_or(Ok(default), len)
    }

    fn flag_or(&self, name: &str, default: bool) -> Result<bool, Fail> {
        self.get(name).map_or(Ok(default), flag_of)
    }
}

fn pick<'a>(el: &str, fields: &'a [Field], known: &[&str]) -> Result<Picked<'a>, Fail> {
    let mut out: Vec<&Field> = Vec::with_capacity(fields.len());
    for (i, f) in fields.iter().enumerate() {
        let at = format!("/el/fields/{i}");
        if !known.contains(&f.name.as_str()) {
            return Err(unsupported(format!(
                "field `{}` on a new {el} is not lowered yet; known: {}",
                f.name,
                known.join(", ")
            ))
            .at(at));
        }
        if out.iter().any(|g| g.name == f.name) {
            return Err(bad_value(format!("`{}` is given more than once", f.name)).at(at));
        }
        out.push(f);
    }
    Ok(Picked(out))
}

/// Plan point of a placement; z must be 0 or the level elevation (heights come from offsets).
fn plan_point(p: &Place, elev: f64) -> Result<(f64, f64), Fail> {
    let [x, y, z] = at_point(p)?;
    if z.abs() > EPS && (z - elev).abs() > EPS {
        return Err(bad_value(format!(
            "a plan point needs z 0 or the level elevation {elev}, got {z}; heights come from offsets"
        )));
    }
    Ok((x, y))
}

fn opening(m: &mut Model, run: &mut Run, slot: u32, el: &NewEl) -> Result<(), Fail> {
    let (cat, kind, host, at, sill, ty, fields) = match el {
        NewEl::Door { host, at, ty, fields } => (Cat::Door, "door", host, at, 0.0, ty, fields),
        NewEl::Window { host, at, sill, ty, fields } => {
            (Cat::Window, "window", host, at, sill.to_m(), ty, fields)
        }
        other => return Err(unsupported(format!("`{}` is not an opening", el_name(other)))),
    };
    if ty.is_some() {
        return Err(unsupported(format!(
            "{kind} types are not lowered yet; give `width` and `height` in fields"
        ))
        .at("/el/type"));
    }
    let f = pick(kind, fields, OPENING_FIELDS)?;
    let sill = if cat == Cat::Door {
        f.len_or("sill", 0.0)?
    } else if f.get("sill").is_some() {
        return Err(bad_value("a window takes `sill` as its own member, not in fields").at("/el/fields"));
    } else {
        sill
    };
    let wall = resolve_as(m, run, host, Cat::Wall).map_err(|e| e.at("/el/host"))?;
    let width = positive_len(f.need(kind, "width")?)?;
    let height = positive_len(f.need(kind, "height")?)?;
    if !sill.is_finite() || sill < 0.0 {
        return Err(bad_value(format!("{kind} `sill` must be 0 or above, got {sill}")).at("/el/sill"));
    }
    let along = along_host(m, run, wall, at, width, sill).map_err(|e| e.at("/el/at"))?;
    let spec = OpeningSpec { along, width, sill, height, kind: kind.into() };
    m.opening_fits(wall, &spec)?;
    let swing = Swing { side: f.flag_or("swing_side", false)?, hinge: f.flag_or("swing_hinge", false)? };
    let id = m.add_opening_with(wall, &spec, swing)?;
    run.made(slot, cat, id);
    Ok(())
}

fn room(m: &mut Model, run: &mut Run, slot: u32, el: &NewEl) -> Result<(), Fail> {
    let NewEl::Room { level, at, name, number } = el else {
        return Err(unsupported(format!("`{}` is not a room", el_name(el))));
    };
    let lv = resolve_as(m, run, level, Cat::Level).map_err(|e| e.at("/el/level"))?;
    let (_, elev) = level_row(m, lv)?;
    let point = plan_point(at, elev).map_err(|e| e.at("/el/at"))?;
    let spec = RoomSpec {
        level: lv,
        name: name.clone(),
        number: number.clone().unwrap_or_default(),
        point,
        height: ROOM_HEIGHT,
    };
    let id = m.add_room(&spec)?;
    run.made(slot, Cat::Room, id);
    Ok(())
}

fn slab(m: &mut Model, run: &mut Run, slot: u32, el: &NewEl) -> Result<(), Fail> {
    let NewEl::Slab { level, boundary, offset, ty, fields } = el else {
        return Err(unsupported(format!("`{}` is not a slab", el_name(el))));
    };
    let f = pick("slab", fields, SLAB_FIELDS)?;
    let lv = resolve_as(m, run, level, Cat::Level).map_err(|e| e.at("/el/level"))?;
    let (_, elev) = level_row(m, lv)?;
    if boundary.len() < 3 {
        return Err(bad_value(format!(
            "a slab boundary needs at least 3 points, got {}",
            boundary.len()
        ))
        .at("/el/boundary"));
    }
    let mut outer = Vec::with_capacity(boundary.len());
    for (i, p) in boundary.iter().enumerate() {
        outer.push(plan_point(p, elev).map_err(|e| e.at(format!("/el/boundary/{i}")))?);
    }
    let holes = match f.get("holes") {
        Some(h) => rings_of(h)?,
        None => vec![],
    };
    let slab_type = match ty {
        Some(t) => resolve_as(m, run, t, Cat::SlabType).map_err(|e| e.at("/el/type"))?,
        None => first_row(&m.db, "slab_type", "slab type")?,
    };
    let id = m.add_slab(&SlabSpec { level: lv, slab_type, offset: offset.to_m(), outer, holes })?;
    run.made(slot, Cat::Slab, id);
    Ok(())
}

/// A roof over a plan outline, every edge sloped by `slope` (degrees on the wire as an angle);
/// `gables` lists the edge indices left as gable ends.
fn roof(m: &mut Model, run: &mut Run, slot: u32, el: &NewEl) -> Result<(), Fail> {
    let NewEl::Roof { level, boundary, slope, ty, fields } = el else {
        return Err(unsupported(format!("`{}` is not a roof", el_name(el))));
    };
    let f = pick("roof", fields, ROOF_FIELDS)?;
    let lv = resolve_as(m, run, level, Cat::Level).map_err(|e| e.at("/el/level"))?;
    let (_, elev) = level_row(m, lv)?;
    if boundary.len() < 3 {
        return Err(bad_value(format!("a roof boundary needs at least 3 points, got {}", boundary.len()))
            .at("/el/boundary"));
    }
    let mut ring = Vec::with_capacity(boundary.len());
    for (i, p) in boundary.iter().enumerate() {
        ring.push(plan_point(p, elev).map_err(|e| e.at(format!("/el/boundary/{i}")))?);
    }
    let deg = slope.to_rad().to_degrees();
    if !(0.0..90.0).contains(&deg) {
        return Err(bad_value(format!("roof `slope` must be from 0 to 90 degrees, got {deg}")).at("/el/slope"));
    }
    let mut slopes = vec![Some(deg); ring.len()];
    if let Some(g) = f.get("gables") {
        let bad = || bad_value("`gables` needs a list of edge numbers (0 is the edge from the first point)");
        for e in g.value.v.as_array().ok_or_else(bad)? {
            let i = e.as_u64().ok_or_else(bad)? as usize;
            *slopes.get_mut(i).ok_or_else(bad)? = None;
        }
    }
    let slab_type = match ty {
        Some(t) => resolve_as(m, run, t, Cat::SlabType).map_err(|e| e.at("/el/type"))?,
        None => first_row(&m.db, "slab_type", "slab type")?,
    };
    let spec = RoofSpec {
        level: lv,
        slab_type,
        offset: f.len_or("offset", 0.0)?,
        overhang: f.len_or("overhang", 0.0)?,
        fascia: f.len_or("fascia", 0.0)?,
        kind: RoofKind::Footprint { ring, slopes, mansard: None },
    };
    let id = m.add_roof(&spec)?;
    run.made(slot, Cat::Roof, id);
    Ok(())
}

fn column(m: &mut Model, run: &mut Run, slot: u32, el: &NewEl) -> Result<(), Fail> {
    let NewEl::Column { level, at, top, height, rotation, ty, fields } = el else {
        return Err(unsupported(format!("`{}` is not a column", el_name(el))));
    };
    let f = pick("column", fields, COLUMN_FIELDS)?;
    let lv = resolve_as(m, run, level, Cat::Level).map_err(|e| e.at("/el/level"))?;
    let (_, elev) = level_row(m, lv)?;
    let at = plan_point(at, elev).map_err(|e| e.at("/el/at"))?;
    let base_offset = f.len_or("base_offset", 0.0)?;
    let h = height.to_m();
    // height 0: the top is the top level (or the base level) plus `top_offset`
    let (top_level, top_offset) = if h.abs() > EPS {
        if h < 0.0 {
            return Err(bad_value(format!("column `height` must be above 0, got {h}")).at("/el/height"));
        }
        if top.is_some() || f.get("top_offset").is_some() {
            return Err(bad_value("a column takes `height` or `top` with `top_offset`, not both")
                .at("/el/height"));
        }
        (None, base_offset + h)
    } else {
        let tl = match top {
            Some(t) => Some(resolve_as(m, run, t, Cat::Level).map_err(|e| e.at("/el/top"))?),
            None => None,
        };
        (tl, f.len_or("top_offset", 0.0)?)
    };
    let tx = f.get("top_at_x").map_or(Ok(None), opt_len)?;
    let ty_ = f.get("top_at_y").map_or(Ok(None), opt_len)?;
    let top_at = match (tx, ty_) {
        (Some(x), Some(y)) => Some((x, y)),
        (None, None) => None,
        _ => return Err(bad_value("a slanted column needs both `top_at_x` and `top_at_y`")),
    };
    let member_type = match ty {
        Some(t) => resolve_as(m, run, t, Cat::MemberType).map_err(|e| e.at("/el/type"))?,
        None => first_row(&m.db, "member_type", "member type")?,
    };
    let spec = ColumnSpec {
        level: lv,
        top_level,
        member_type,
        structural: f.flag_or("structural", true)?,
        at,
        base_offset,
        top_offset,
        rotation: rotation.to_rad(),
        top_at,
    };
    let id = m.add_column(&spec)?;
    run.made(slot, Cat::Column, id);
    Ok(())
}

/// Default type when the op names none: the oldest row of the table.
fn first_row(db: &Connection, table: &str, what: &str) -> Result<i64, Fail> {
    let id: Option<i64> = db.query_row(&format!("SELECT min(id) FROM {table}"), [], |r| r.get(0))?;
    id.ok_or_else(|| bad_value(format!("the model has no {what}; give `type`")).at("/el/type"))
}

/// Rings of `[x, y]` points; metres when the value has no unit, else in its length unit.
pub(super) fn rings_of(f: &Field) -> Result<Vec<Vec<(f64, f64)>>, Fail> {
    let bad = || bad_value(format!("`{}` needs a list of rings of [x, y] points", f.name));
    let k = match f.value.unit {
        None => 1.0,
        Some(u) if u.dim() == Dim::Length => u.to_si(1.0),
        Some(_) => return Err(bad()),
    };
    if f.value.v.is_null() {
        return Ok(vec![]);
    }
    let rings = f.value.v.as_array().ok_or_else(bad)?;
    let mut out = Vec::with_capacity(rings.len());
    for r in rings {
        let pts = r.as_array().ok_or_else(bad)?;
        let mut ring = Vec::with_capacity(pts.len());
        for p in pts {
            let xy = match p.as_array().map(Vec::as_slice) {
                Some([x, y]) => x.as_f64().zip(y.as_f64()),
                _ => None,
            };
            match xy {
                Some((x, y)) if x.is_finite() && y.is_finite() => ring.push((x * k, y * k)),
                _ => return Err(bad()),
            }
        }
        out.push(ring);
    }
    Ok(out)
}

#[cfg(test)]
#[path = "ops_create_tests.rs"]
mod tests;
