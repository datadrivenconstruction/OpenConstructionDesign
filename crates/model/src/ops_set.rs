// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `Set` on existing rows (docs/design/AI_NATIVE.md, "Set fields").
//!
//! Walls, columns, beams, slabs, openings and rooms are lowered to `bulk::Edit` and project
//! parameters to `Edit::ParamValue`, the functions the XLSX import runs, so an envelope and the import give
//! the same rows. Field names are the ones the XLSX adapter emits. An unknown field, a wrong
//! unit or a missing row rejects the whole envelope with the field named. Type rows
//! (`wtype:`, `stype:`, `mtype:`) are lowered in `ops_set_type.rs`.

use ddc_ops::{Cat, Dim, Field, Ref, Targets};
use serde_json::Value as J;

use super::{bad_ref, bad_value, length_of, level_row, table, targets, text_of, unsupported, Fail, Run};
use crate::{BeamJoin, Edit, Just, Model, OpeningSpec, PVal, ParamDef, ParamType, RoomSpec, Swing, WallSpec};

/// A field with this prefix always names a project parameter (for names a kind also uses).
pub const PARAM_PREFIX: &str = "param.";

const LEVEL: &[&str] = &["name", "elevation"];
const WALL: &[&str] = &[
    "level", "ax", "ay", "bx", "by", "base_offset", "height", "wall_type", "offset0", "bulge",
    "lean", "top_scale",
];
const COLUMN: &[&str] = &[
    "level", "top_level", "member_type", "structural", "x", "y", "base_offset", "top_offset",
    "rotation", "top_at_x", "top_at_y",
];
const BEAM: &[&str] = &[
    "level", "member_type", "ax", "ay", "az", "bx", "by", "bz", "bulge", "yjust", "zjust",
    "rotation", "start_ext", "end_ext", "join_start", "join_end",
];
const SLAB: &[&str] = &["level", "slab_type", "offset"];
const OPENING: &[&str] = &["wall", "along", "width", "sill", "height", "kind", "swing_side", "swing_hinge"];
const ROOM: &[&str] = &["level", "name", "number", "px", "py", "height"];
const OPENING_KIND: &[&str] = &["door", "window", "void"];
const JUST: &[&str] = &["neg", "center", "pos"];
const JOIN: &[&str] = &["auto", "butt", "miter", "cope", "none"];

fn own_fields(cat: Cat) -> &'static [&'static str] {
    match cat {
        Cat::Level => LEVEL,
        Cat::Wall => WALL,
        Cat::Column => COLUMN,
        Cat::Beam => BEAM,
        Cat::Slab => SLAB,
        Cat::Door | Cat::Window | Cat::Opening => OPENING,
        Cat::Room => ROOM,
        c if ty::is_type(c) => ty::TYPE_FIELDS,
        _ => &[],
    }
}

/// Fields of one target split into the kind's own fields and project parameters.
struct Split<'a> {
    own: Vec<&'a Field>,
    params: Vec<(&'a ParamDef, &'a Field)>,
    /// type parameters given on an element: written on the element's type row
    via_type: Vec<(&'a ParamDef, &'a Field)>,
}

impl<'a> Split<'a> {
    fn param(&mut self, cat: Cat, label: &str, d: &'a ParamDef, f: &'a Field) -> Result<(), Fail> {
        if on_row(cat, label, d)? {
            self.params.push((d, f));
        } else {
            self.via_type.push((d, f));
        }
        Ok(())
    }
}

fn split<'a>(cat: Cat, label: &str, fields: &'a [Field], defs: &'a [ParamDef]) -> Result<Split<'a>, Fail> {
    let own_names = own_fields(cat);
    let by_name = move |n: &str| defs.iter().find(|d| d.spec.name == n.trim());
    let mut out = Split { own: vec![], params: vec![], via_type: vec![] };
    for (i, f) in fields.iter().enumerate() {
        if fields[..i].iter().any(|g| g.name == f.name) {
            return Err(bad_value(format!("{label}: field `{}` is given twice", f.name)));
        }
        if let Some(p) = f.name.strip_prefix(PARAM_PREFIX) {
            let d = by_name(p)
                .ok_or_else(|| bad_ref(format!("{label}: field `{}`: no project parameter \"{p}\"", f.name)))?;
            out.param(cat, label, d, f)?;
        } else if own_names.contains(&f.name.as_str()) || ty::is_layer_field(cat, &f.name) {
            // a parameter with the same name could be what an XLSX column meant: refuse to guess
            if defs.iter().any(|d| d.spec.name.trim().eq_ignore_ascii_case(&f.name)) {
                return Err(bad_value(format!(
                    "{label}: field `{0}` is both a {1} field and a project parameter; \
                     write `{PARAM_PREFIX}{0}` for the parameter",
                    f.name,
                    cat.key()
                )));
            }
            out.own.push(f);
        } else if let Some(d) = by_name(&f.name) {
            out.param(cat, label, d, f)?;
        } else {
            let known = if own_names.is_empty() {
                "it has no settable fields of its own".to_owned()
            } else {
                format!("settable: {}{}", own_names.join(", "), ty::layer_hint(cat))
            };
            return Err(unsupported(format!(
                "{label}: unknown field `{}` (not a {} field nor a project parameter; {known})",
                f.name,
                cat.key()
            )));
        }
    }
    Ok(out)
}

/// A type parameter is stored on type rows only and an instance parameter on elements only.
/// True: the value goes on this row. False: a type parameter given on an element, written on
/// the element's type as the AI plan and the type sheet of the book do. An instance parameter on
/// a type row would mean every element of the type: refused, the caller names them.
fn on_row(cat: Cat, label: &str, d: &ParamDef) -> Result<bool, Fail> {
    let name = &d.spec.name;
    match (ty::is_type(cat), d.spec.per_type) {
        (true, false) => Err(bad_value(format!(
            "{label}: `{name}` is an instance parameter; set it on the elements, not on a type row"
        ))),
        (false, true) if type_link(cat).is_none() => Err(bad_value(format!(
            "{label}: `{name}` is a type parameter and a {} has no type row to hold it",
            cat.key()
        ))),
        (false, true) => Ok(false),
        _ => Ok(true),
    }
}

/// Element table, its type column and the type kind, for elements whose type holds parameters.
fn type_link(cat: Cat) -> Option<(&'static str, &'static str, Cat)> {
    Some(match cat {
        Cat::Wall => ("wall", "wtype", Cat::WallType),
        Cat::Slab => ("slab", "stype", Cat::SlabType),
        Cat::Roof => ("roof", "stype", Cat::SlabType),
        Cat::Ceiling => ("ceiling", "stype", Cat::SlabType),
        Cat::Column => ("col", "mtype", Cat::MemberType),
        Cat::Beam => ("beam", "mtype", Cat::MemberType),
        _ => return None,
    })
}

/// Own fields that move the element to another type.
const TYPE_FIELDS_OF_ELEMENT: &[&str] = &["wall_type", "slab_type", "member_type"];

pub(super) fn set_fields(m: &mut Model, run: &mut Run, t: &Targets, fields: &[Field]) -> Result<(), Fail> {
    if fields.is_empty() {
        return Err(bad_value("`set` without fields"));
    }
    let defs = m.params()?;
    let rows = targets(m, run, t)?;
    if fields.iter().any(|f| f.name == "name") {
        // type names are unique, and the per-row check sees only the stored names
        let mut types: Vec<(Cat, i64)> = rows.iter().copied().filter(|(c, _)| ty::is_type(*c)).collect();
        types.sort_unstable();
        types.dedup();
        if types.len() > 1 {
            return Err(bad_value(format!(
                "`name` on {} type rows at once: type names are unique, rename one type per `set`",
                types.len()
            )));
        }
    }
    // a type reached from several of its elements gets each type parameter once
    let mut types_done: Vec<(Cat, i64)> = Vec::new();
    for (cat, id) in rows {
        if cat == Cat::Type {
            // a bare `type:N` written as v2 does not say which table; slab or member type
            // parameters would land on a wall type
            return Err(unsupported(format!(
                "{}: `set` needs the type table: write wtype:, stype: or mtype:",
                Ref::id(cat, id).label()
            )));
        }
        set_row(m, run, cat, id, fields, &defs, &mut types_done)?;
    }
    Ok(())
}

/// `type.rename` and `type.set`, lowered like `set` on the type row.
pub(super) fn type_op(m: &mut Model, run: &mut Run, op: &ddc_ops::TypeOp) -> Result<(), Fail> {
    ty::type_op(m, run, op)
}

/// Own fields and parameters of one resolved row, as one `apply_edits` batch.
fn set_one(m: &mut Model, run: &mut Run, cat: Cat, id: i64, fields: &[Field], defs: &[ParamDef]) -> Result<(), Fail> {
    set_row(m, run, cat, id, fields, defs, &mut Vec::new())
}

/// `set_one`; type parameters given on an element go on its type row in the same batch,
/// skipped for a type already in `types_done`.
fn set_row(
    m: &mut Model,
    run: &mut Run,
    cat: Cat,
    id: i64,
    fields: &[Field],
    defs: &[ParamDef],
    types_done: &mut Vec<(Cat, i64)>,
) -> Result<(), Fail> {
    // a piece of a spline wall: the wall's own fields rewrite the whole spline wall after the
    // piece row takes the rest (parameters; its geometry is refused by the model)
    let spline = if cat == Cat::Wall { m.spline_of_wall(id)? } else { None };
    let Some(s) = spline else {
        return set_row_fields(m, run, cat, id, fields, defs, types_done);
    };
    let (own, rest) = super::ops_spline::split(fields);
    if !rest.is_empty() {
        set_row_fields(m, run, cat, id, &rest, defs, types_done)?;
    }
    super::ops_spline::set(m, run, s, &own)
}

fn set_row_fields(
    m: &mut Model,
    run: &mut Run,
    cat: Cat,
    id: i64,
    fields: &[Field],
    defs: &[ParamDef],
    types_done: &mut Vec<(Cat, i64)>,
) -> Result<(), Fail> {
    let label = Ref::id(cat, id).label();
    let s = split(cat, &label, fields, defs)?;
    let mut type_edits = Vec::new();
    if let (Some((d, _)), Some((etable, col, tcat))) = (s.via_type.first(), type_link(cat)) {
        // which type would get the value, the old or the new one, is a guess: refuse it
        if let Some(f) = s.own.iter().find(|f| TYPE_FIELDS_OF_ELEMENT.contains(&f.name.as_str())) {
            return Err(bad_value(format!(
                "{label}: `{}` is a type parameter and `{}` changes the type in the same `set`; \
                 change the type first, then set the parameter in a second op",
                d.spec.name, f.name
            )));
        }
        let tid: i64 = m.db.query_row(&format!("SELECT {col} FROM {etable} WHERE id=?1"), [id], |r| r.get(0))?;
        if !types_done.contains(&(tcat, tid)) {
            types_done.push((tcat, tid));
            let tkind = table(tcat).ok_or_else(|| unsupported(format!("{label}: no type table")))?;
            for (d, f) in &s.via_type {
                type_edits.push(Edit::ParamValue {
                    param: d.spec.name.clone(),
                    kind: tkind.to_owned(),
                    id: tid,
                    value: pval(d, f)?,
                });
            }
            run.receipt.changed.push(Ref::id(tcat, tid));
        }
    }
    let mut edits = Vec::new();
    if !s.own.is_empty() {
        match cat {
            Cat::Level => set_level(m, id, &s.own)?,
            Cat::Wall => edits.extend(wall_edit(m, id, &s.own)?),
            Cat::Column => edits.push(column_edit(m, id, &s.own)?),
            Cat::Beam => edits.push(beam_edit(m, id, &s.own)?),
            Cat::Slab => edits.push(slab_edit(m, id, &s.own)?),
            Cat::Door | Cat::Window | Cat::Opening => edits.extend(opening_edit(m, id, &s.own)?),
            Cat::Room => edits.push(room_edit(m, id, &s.own)?),
            c if ty::is_type(c) => edits.extend(ty::type_own(m, c, id, &s.own, fields)?),
            _ => return Err(unsupported(format!("{label}: no settable fields"))),
        }
    }
    let kind = table(cat).ok_or_else(|| unsupported(format!("{label}: no table")))?;
    for (d, f) in s.params {
        edits.push(Edit::ParamValue {
            param: d.spec.name.clone(),
            kind: kind.to_owned(),
            id,
            value: pval(d, f)?,
        });
    }
    edits.extend(type_edits);
    if !edits.is_empty() {
        m.apply_edits(&edits)?;
    }
    run.receipt.changed.push(Ref::id(receipt_cat(cat, &s.own), id));
    Ok(())
}

/// A `kind` write re-categorises an opening; the receipt names the category it now has.
fn receipt_cat(cat: Cat, own: &[&Field]) -> Cat {
    if !matches!(cat, Cat::Door | Cat::Window | Cat::Opening) {
        return cat;
    }
    match own.iter().find(|f| f.name == "kind").and_then(|f| f.value.v.as_str()) {
        Some("door") => Cat::Door,
        Some("window") => Cat::Window,
        _ => cat,
    }
}

fn set_level(m: &mut Model, id: i64, own: &[&Field]) -> Result<(), Fail> {
    let (mut name, mut z) = level_row(m, id)?;
    for f in own {
        match f.name.as_str() {
            "name" => name = text_of(f)?,
            "elevation" => z = length_of(f)?,
            other => return Err(unsupported(format!("level field `{other}` is not settable"))),
        }
    }
    m.update_level(id, &name, z)?;
    Ok(())
}

// ---- values; every message names the field ----

fn got(f: &Field) -> String {
    match f.value.unit {
        Some(u) => format!("{} {u:?}", f.value.v),
        None => f.value.v.to_string(),
    }
}

fn plain_num(f: &Field) -> Result<f64, Fail> {
    match (f.value.unit, f.value.v.as_f64()) {
        (None, Some(x)) if x.is_finite() => Ok(x),
        _ => Err(bad_value(format!("`{}` needs a plain number without a unit, got {}", f.name, got(f)))),
    }
}

fn angle_of(f: &Field) -> Result<f64, Fail> {
    match (f.value.unit, f.value.si()) {
        (Some(u), Some(x)) if u.dim() == Dim::Angle && x.is_finite() => Ok(x),
        _ => Err(bad_value(format!("`{}` needs an angle with a unit (rad, deg), got {}", f.name, got(f)))),
    }
}

pub(super) fn len(f: &Field) -> Result<f64, Fail> {
    length_of(f).map_err(|_| bad_value(format!("`{}` needs a length with a unit (m, mm, ...), got {}", f.name, got(f))))
}

/// `null` without a unit clears the value.
pub(super) fn opt_len(f: &Field) -> Result<Option<f64>, Fail> {
    if f.value.v.is_null() && f.value.unit.is_none() {
        return Ok(None);
    }
    len(f).map(Some)
}

/// Text without a unit; empty is allowed (a room number may be blank).
fn str_of(f: &Field) -> Result<String, Fail> {
    match (f.value.unit, f.value.v.as_str()) {
        (None, Some(s)) => Ok(s.to_owned()),
        _ => Err(bad_value(format!("`{}` needs text without a unit, got {}", f.name, got(f)))),
    }
}

pub(super) fn positive_len(f: &Field) -> Result<f64, Fail> {
    let x = len(f)?;
    if x <= 0.0 {
        return Err(bad_value(format!("`{}` must be above 0, got {}", f.name, got(f))));
    }
    Ok(x)
}

pub(super) fn flag_of(f: &Field) -> Result<bool, Fail> {
    match (f.value.unit, f.value.v.as_bool()) {
        (None, Some(b)) => Ok(b),
        _ => Err(bad_value(format!("`{}` needs true or false, got {}", f.name, got(f)))),
    }
}

fn word<'a>(f: &Field, allowed: &[&'a str]) -> Result<&'a str, Fail> {
    let s = match (f.value.unit, f.value.v.as_str()) {
        (None, Some(s)) => allowed.iter().find(|w| **w == s).copied(),
        _ => None,
    };
    s.ok_or_else(|| bad_value(format!("`{}` needs one of {}, got {}", f.name, allowed.join(", "), got(f))))
}

/// Integer id of a row of `table` (levels, types), given as a plain number.
pub(super) fn row_id(m: &Model, f: &Field, table: &str) -> Result<i64, Fail> {
    let id = match (f.value.unit, f.value.v.as_i64()) {
        (None, Some(i)) => i,
        _ => return Err(bad_value(format!("`{}` needs a {table} id (integer), got {}", f.name, got(f)))),
    };
    if !super::row_exists(&m.db, table, id)? {
        return Err(bad_ref(format!("`{}`: no {table} {id}", f.name)));
    }
    Ok(id)
}

fn opt_row_id(m: &Model, f: &Field, table: &str) -> Result<Option<i64>, Fail> {
    if f.value.v.is_null() && f.value.unit.is_none() {
        return Ok(None);
    }
    row_id(m, f, table).map(Some)
}

fn just_of(f: &Field) -> Result<Just, Fail> {
    Ok(match word(f, JUST)? {
        "neg" => Just::Neg,
        "pos" => Just::Pos,
        _ => Just::Center,
    })
}

fn join_of(f: &Field) -> Result<BeamJoin, Fail> {
    word(f, JOIN).map(BeamJoin::parse)
}

/// Wire value -> parameter value, the inverse of the XLSX adapter; `None` clears.
fn pval(d: &ParamDef, f: &Field) -> Result<Option<PVal>, Fail> {
    let v = &f.value;
    let dim = match d.spec.ptype {
        ParamType::Length => Some(Dim::Length),
        ParamType::Area => Some(Dim::Area),
        ParamType::Volume => Some(Dim::Volume),
        _ => None,
    };
    if let Some(u) = v.unit {
        if Some(u.dim()) != dim {
            return Err(bad_value(format!(
                "`{}`: unit {u:?} does not fit the {} parameter \"{}\"",
                f.name,
                d.spec.ptype.key(),
                d.spec.name
            )));
        }
        return match v.si() {
            Some(x) if x.is_finite() => Ok(Some(PVal::Num(x))),
            _ => Err(bad_value(format!("`{}` needs a number with its unit, got {}", f.name, got(f)))),
        };
    }
    Ok(Some(match &v.v {
        J::Null => return Ok(None),
        J::String(s) => PVal::Text(s.clone()),
        J::Bool(b) => PVal::Bool(*b),
        // json integers stay integers, json floats stay floats (as the adapter wrote them)
        J::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => PVal::Int(i),
            (None, Some(x)) if x.is_finite() => PVal::Num(x),
            _ => return Err(bad_value(format!("`{}` is not a finite number", f.name))),
        },
        _ => {
            return Err(bad_value(format!(
                "`{}` needs text, a number, true/false or null, got {}",
                f.name,
                got(f)
            )))
        }
    }))
}

// ---- kinds ----

/// `lean` (an angle) and `top_scale` (top/base thickness, plain number) of a wall.
pub(super) fn lean_fields(m: &Model, id: Option<i64>, own: &[&Field]) -> Result<Option<crate::Lean>, Fail> {
    let mut l = match id {
        Some(id) => m.wall_lean(id)?,
        None => crate::Lean::PLAIN,
    };
    let mut any = false;
    for f in own {
        match f.name.as_str() {
            "lean" => l.angle = angle_of(f)?,
            "top_scale" => l.top_scale = plain_num(f)?,
            _ => continue,
        }
        any = true;
    }
    if any && !l.valid() {
        return Err(bad_value("`lean` must be within ±60 deg and `top_scale` within 0.1..10"));
    }
    Ok(any.then_some(l))
}

fn wall_edit(m: &Model, id: i64, own: &[&Field]) -> Result<Vec<Edit>, Fail> {
    let lean = lean_fields(m, Some(id), own)?;
    let shape = lean.map(|l| Edit::WallShape { id, lean: l.angle, top_scale: l.top_scale });
    let own: Vec<&Field> = own
        .iter()
        .copied()
        .filter(|f| f.name != "lean" && f.name != "top_scale")
        .collect();
    if own.is_empty() {
        return Ok(shape.into_iter().collect());
    }
    let mut out = vec![wall_spec_edit(m, id, &own)?];
    out.extend(shape);
    Ok(out)
}

fn wall_spec_edit(m: &Model, id: i64, own: &[&Field]) -> Result<Edit, Fail> {
    let (mut s, mut bulge) = m.db.query_row(
        "SELECT level,wtype,ax,ay,bx,by,base_offset,height,offset0,bulge FROM wall WHERE id=?1",
        [id],
        |r| {
            Ok((
                WallSpec {
                    level: r.get(0)?,
                    wall_type: r.get(1)?,
                    a: (r.get(2)?, r.get(3)?),
                    b: (r.get(4)?, r.get(5)?),
                    base_offset: r.get(6)?,
                    height: r.get(7)?,
                    offset0: r.get(8)?,
                },
                r.get::<_, f64>(9)?,
            ))
        },
    )?;
    for f in own {
        match f.name.as_str() {
            "level" => s.level = row_id(m, f, "level")?,
            "wall_type" => s.wall_type = row_id(m, f, "wall_type")?,
            "ax" => s.a.0 = len(f)?,
            "ay" => s.a.1 = len(f)?,
            "bx" => s.b.0 = len(f)?,
            "by" => s.b.1 = len(f)?,
            "base_offset" => s.base_offset = len(f)?,
            "height" => {
                s.height = len(f)?;
                if s.height <= 0.0 {
                    return Err(bad_value(format!("`height` must be above 0, got {}", got(f))));
                }
            }
            "offset0" => s.offset0 = opt_len(f)?,
            "bulge" => bulge = plain_num(f)?,
            other => return Err(unsupported(format!("wall field `{other}` is not settable"))),
        }
    }
    if (s.b.0 - s.a.0).hypot(s.b.1 - s.a.1) < 1e-6 {
        return Err(bad_value("`ax`, `ay`, `bx`, `by`: wall ends coincide"));
    }
    Ok(Edit::Wall { id, spec: s, bulge })
}

fn column_edit(m: &Model, id: i64, own: &[&Field]) -> Result<Edit, Fail> {
    let mut s = m.column(id)?;
    let (mut tx, mut ty) = (s.top_at.map(|p| p.0), s.top_at.map(|p| p.1));
    for f in own {
        match f.name.as_str() {
            "level" => s.level = row_id(m, f, "level")?,
            "top_level" => s.top_level = opt_row_id(m, f, "level")?,
            "member_type" => s.member_type = row_id(m, f, "member_type")?,
            "structural" => s.structural = flag_of(f)?,
            "x" => s.at.0 = len(f)?,
            "y" => s.at.1 = len(f)?,
            "base_offset" => s.base_offset = len(f)?,
            "top_offset" => s.top_offset = len(f)?,
            "rotation" => s.rotation = angle_of(f)?,
            "top_at_x" => tx = opt_len(f)?,
            "top_at_y" => ty = opt_len(f)?,
            other => return Err(unsupported(format!("column field `{other}` is not settable"))),
        }
    }
    s.top_at = match (tx, ty) {
        (Some(x), Some(y)) => Some((x, y)),
        (None, None) => None,
        _ => return Err(bad_value("`top_at_x` and `top_at_y` are both set or both null")),
    };
    Ok(Edit::Column { id, spec: s })
}

fn beam_edit(m: &Model, id: i64, own: &[&Field]) -> Result<Edit, Fail> {
    let mut s = m.beam(id)?;
    for f in own {
        match f.name.as_str() {
            "level" => s.level = row_id(m, f, "level")?,
            "member_type" => s.member_type = row_id(m, f, "member_type")?,
            "ax" => s.a.0 = len(f)?,
            "ay" => s.a.1 = len(f)?,
            "az" => s.a.2 = len(f)?,
            "bx" => s.b.0 = len(f)?,
            "by" => s.b.1 = len(f)?,
            "bz" => s.b.2 = len(f)?,
            "bulge" => s.bulge = plain_num(f)?,
            "yjust" => s.yjust = just_of(f)?,
            "zjust" => s.zjust = just_of(f)?,
            "rotation" => s.rotation = angle_of(f)?,
            "start_ext" => s.start_ext = len(f)?,
            "end_ext" => s.end_ext = len(f)?,
            "join_start" => s.join_start = join_of(f)?,
            "join_end" => s.join_end = join_of(f)?,
            other => return Err(unsupported(format!("beam field `{other}` is not settable"))),
        }
    }
    Ok(Edit::Beam { id, spec: s })
}

fn slab_edit(m: &Model, id: i64, own: &[&Field]) -> Result<Edit, Fail> {
    let (mut level, mut slab_type, mut offset): (i64, i64, f64) = m.db.query_row(
        "SELECT level,stype,offset FROM slab WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    for f in own {
        match f.name.as_str() {
            "level" => level = row_id(m, f, "level")?,
            "slab_type" => slab_type = row_id(m, f, "slab_type")?,
            "offset" => offset = len(f)?,
            other => return Err(unsupported(format!("slab field `{other}` is not settable"))),
        }
    }
    Ok(Edit::Slab { id, level, slab_type, offset })
}

/// The opening row edit (only when a geometry field is given) and the swing edit (only when
/// `swing_side` or `swing_hinge` is given; the other half is kept from the current row).
fn opening_edit(m: &Model, id: i64, own: &[&Field]) -> Result<Vec<Edit>, Fail> {
    let (mut wall, mut s, mut sw) = m.db.query_row(
        "SELECT wall,along,width,sill,height,kind,swing_side,swing_hinge FROM opening WHERE id=?1",
        [id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                OpeningSpec {
                    along: r.get(1)?,
                    width: r.get(2)?,
                    sill: r.get(3)?,
                    height: r.get(4)?,
                    kind: r.get(5)?,
                },
                Swing { side: r.get(6)?, hinge: r.get(7)? },
            ))
        },
    )?;
    let (mut geom, mut swing) = (false, false);
    for f in own {
        match f.name.as_str() {
            "swing_side" => {
                sw.side = flag_of(f)?;
                swing = true;
                continue;
            }
            "swing_hinge" => {
                sw.hinge = flag_of(f)?;
                swing = true;
                continue;
            }
            "wall" => wall = row_id(m, f, "wall")?,
            "along" => s.along = len(f)?,
            "width" => s.width = positive_len(f)?,
            "sill" => s.sill = len(f)?,
            "height" => s.height = positive_len(f)?,
            "kind" => s.kind = word(f, OPENING_KIND)?.to_owned(),
            other => return Err(unsupported(format!("opening field `{other}` is not settable"))),
        }
        geom = true;
    }
    let mut out = Vec::new();
    if geom {
        out.push(Edit::Opening { id, wall, spec: s });
    }
    if swing {
        out.push(Edit::OpeningSwing { id, swing: sw });
    }
    Ok(out)
}

fn room_edit(m: &Model, id: i64, own: &[&Field]) -> Result<Edit, Fail> {
    let mut s = m.db.query_row(
        "SELECT level,name,number,px,py,height FROM room WHERE id=?1",
        [id],
        |r| {
            Ok(RoomSpec {
                level: r.get(0)?,
                name: r.get(1)?,
                number: r.get(2)?,
                point: (r.get(3)?, r.get(4)?),
                height: r.get(5)?,
            })
        },
    )?;
    for f in own {
        match f.name.as_str() {
            "level" => s.level = row_id(m, f, "level")?,
            "name" => s.name = str_of(f)?,
            "number" => s.number = str_of(f)?,
            "px" => s.point.0 = len(f)?,
            "py" => s.point.1 = len(f)?,
            "height" => s.height = positive_len(f)?,
            other => return Err(unsupported(format!("room field `{other}` is not settable"))),
        }
    }
    Ok(Edit::Room { id, spec: s })
}

#[path = "ops_set_type.rs"]
mod ty;

#[cfg(test)]
#[path = "ops_set_tests.rs"]
mod tests;
