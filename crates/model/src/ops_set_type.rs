// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `Set` on type rows: `wtype:N`, `stype:N`, `mtype:N` (docs/design/AI_NATIVE.md, "Set fields").
//!
//! The own field is `name` (unique within its table). Type parameters go through
//! `Edit::ParamValue` on the type table, as instance parameters do; `ops_set.rs` splits the
//! fields and calls in here for the own ones. Wall type layers are set by position as
//! `layer.N.material` / `layer.N.thickness` (N is the stored `pos`, from 0, as in the XLSX
//! layers sheet); slab layers and member sections keep their values.
//!
//! `type.rename` and `type.set` lower to the same path; there a bare `type:N` means a wall type.

use ddc_ops::{Cat, Field, Ref, TypeOp, Value};
use rusqlite::OptionalExtension;

use super::super::{bad_ref, bad_value, length_of, resolve, table, text_of, unsupported, Fail, Run};
use crate::ops::Inv;
use crate::{Edit, LayerSpec, MemberShape, Model};

/// Own fields of every type kind.
pub(super) const TYPE_FIELDS: &[&str] = &["name"];
/// Prefix of the wall type layer fields `layer.N.material` and `layer.N.thickness`.
pub(super) const LAYER_PREFIX: &str = "layer.";

pub(super) fn is_layer_field(cat: Cat, name: &str) -> bool {
    cat == Cat::WallType && name.starts_with(LAYER_PREFIX)
}

/// Addition to the "settable" list of an unknown-field message.
pub(super) fn layer_hint(cat: Cat) -> &'static str {
    if cat == Cat::WallType {
        ", layer.N.material, layer.N.thickness"
    } else {
        ""
    }
}

/// Every `type.*` op. Rename/set: the target must be a type row; the rest is `set` on that row.
pub(super) fn type_op(m: &mut Model, run: &mut Run, op: &TypeOp) -> Result<(), Fail> {
    let renamed;
    let (target, fields, rename) = match op {
        TypeOp::Rename { target, name } => {
            renamed = [Field::new("name", Value::text(name))];
            (target, &renamed[..], true)
        }
        TypeOp::Set { target, fields } => (target, fields.as_slice(), false),
        TypeOp::Duplicate { slot, from, name, fields } => {
            return duplicate(m, run, *slot, from, name, fields)
        }
        TypeOp::Delete { target, replace_with } => {
            return delete(m, run, target, replace_with.as_ref())
        }
    };
    if fields.is_empty() {
        return Err(bad_value("`type.set` without fields").at("/fields"));
    }
    let (cat, id) = resolve(m, run, target).map_err(|e| e.at("/target"))?;
    let cat = if cat == Cat::Type { Cat::WallType } else { cat };
    if !is_type(cat) {
        return Err(bad_ref(format!(
            "{} is not a type row (wtype:, stype:, mtype:)",
            target.label()
        ))
        .at("/target"));
    }
    let defs = m.params()?;
    super::set_one(m, run, cat, id, fields, &defs).map_err(|e| match e {
        // the name came from `name`, not from a field list
        Fail::Under(_, inner) if rename => (*inner).at("/name"),
        e => e,
    })
}

/// `type.duplicate`: copies the layers (or section) of `from` under a free `name`, registers
/// the new row as `slot`, then applies `fields` to it like `type.set`.
fn duplicate(
    m: &mut Model,
    run: &mut Run,
    slot: u32,
    from: &Ref,
    name: &str,
    fields: &[Field],
) -> Result<(), Fail> {
    let (cat, src) = resolve(m, run, from).map_err(|e| e.at("/from"))?;
    let cat = if cat == Cat::Type { Cat::WallType } else { cat };
    if !is_type(cat) {
        return Err(bad_ref(format!("{} is not a type row (wtype:, stype:, mtype:)", from.label()))
            .at("/from"));
    }
    let name = name.trim();
    if name.is_empty() {
        return Err(bad_value("`type.duplicate` needs a non-empty name").at("/name"));
    }
    let t = table(cat).ok_or_else(|| unsupported("no table for the type"))?;
    let used: Option<i64> = m
        .db
        .query_row(&format!("SELECT id FROM {t} WHERE name=?1 LIMIT 1"), [name], |r| r.get(0))
        .optional()?;
    if let Some(o) = used {
        return Err(bad_value(format!(
            "name \"{name}\" is already used by {}; type names are unique",
            Ref::id(cat, o).label()
        ))
        .at("/name"));
    }
    let label = Ref::id(cat, src).label();
    let new = match cat {
        Cat::WallType => {
            let layers: Vec<LayerSpec> = wall_layers(m, src)?.into_iter().map(|(_, l)| l).collect();
            m.add_wall_type(name, &layers)?
        }
        Cat::SlabType => {
            let layers = slab_layers(m, src)?;
            m.add_slab_type(name, &layers)?
        }
        _ => {
            let (shape, material) = member_row(m, src, &label)?;
            m.add_member_type(name, &shape, &material)?
        }
    };
    run.made(slot, cat, new);
    if fields.is_empty() {
        return Ok(());
    }
    let defs = m.params()?;
    super::set_one(m, run, cat, new, fields, &defs).map_err(|e| e.at("/fields"))?;
    // a fresh row is reported as created only
    run.receipt.changed.retain(|r| *r != Ref::id(cat, new));
    Ok(())
}

/// Rows that point at a type: (table, column, instance category when it has one).
fn users(cat: Cat) -> &'static [(&'static str, &'static str, Option<Cat>)] {
    match cat {
        Cat::WallType => &[("wall", "wtype", Some(Cat::Wall))],
        Cat::SlabType => &[
            ("slab", "stype", Some(Cat::Slab)),
            ("roof", "stype", Some(Cat::Roof)),
            ("ceiling", "stype", Some(Cat::Ceiling)),
        ],
        _ => &[
            ("col", "mtype", Some(Cat::Column)),
            ("beam", "mtype", Some(Cat::Beam)),
            ("brace", "mtype", Some(Cat::Brace)),
            ("beam_system", "mtype", None),
            ("truss", "chord_type", None),
            ("truss", "web_type", None),
        ],
    }
}

/// `type.delete`: a type in use is refused with the instance count unless `replace_with` names
/// another type of the same kind; the instances move there, then the type row goes.
fn delete(m: &mut Model, run: &mut Run, target: &Ref, with: Option<&Ref>) -> Result<(), Fail> {
    let (cat, id) = resolve(m, run, target).map_err(|e| e.at("/target"))?;
    let cat = if cat == Cat::Type { Cat::WallType } else { cat };
    if !is_type(cat) {
        return Err(bad_ref(format!(
            "{} is not a type row (wtype:, stype:, mtype:)",
            target.label()
        ))
        .at("/target"));
    }
    let label = Ref::id(cat, id).label();
    let mut used = Vec::new();
    for &(t, col, ic) in users(cat) {
        let mut st = m.db.prepare(&format!("SELECT id FROM {t} WHERE {col}=?1 ORDER BY id"))?;
        let ids: Vec<i64> = st.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        used.push((t, col, ic, ids));
    }
    let count: usize = used.iter().map(|u| u.3.len()).sum();
    if count > 0 {
        let Some(with) = with else {
            let by: Vec<String> = used
                .iter()
                .filter(|u| !u.3.is_empty())
                .map(|u| format!("{} {}", u.3.len(), u.0))
                .collect();
            return Err(bad_value(format!(
                "{label} is used by {count} instance(s) ({}); name `replace_with` or retype them first",
                by.join(", ")
            ))
            .at("/target"));
        };
        let (wcat, to) = resolve(m, run, with).map_err(|e| e.at("/replace_with"))?;
        let wcat = if wcat == Cat::Type { Cat::WallType } else { wcat };
        if wcat != cat {
            return Err(bad_value(format!(
                "{} is not a type of the same kind as {label}",
                with.label()
            ))
            .at("/replace_with"));
        }
        if to == id {
            return Err(bad_value("`replace_with` is the type being deleted").at("/replace_with"));
        }
        let kind = match cat {
            Cat::WallType => "wall_type_retype",
            Cat::SlabType => "slab_type_retype",
            _ => "member_type_retype",
        };
        m.op(kind, |db| {
            // snapshots first: a row that points at the type twice must restore whole
            let mut inv = Inv::default();
            for u in used.iter().filter(|u| !u.3.is_empty()) {
                inv = inv.restore(db, u.0, u.1, id)?;
            }
            for u in used.iter().filter(|u| !u.3.is_empty()) {
                db.execute(
                    &format!("UPDATE {t} SET {c}=?2 WHERE {c}=?1", t = u.0, c = u.1),
                    rusqlite::params![id, to],
                )?;
            }
            Ok(((), id, inv))
        })?;
        for (_, _, ic, ids) in &used {
            if let Some(ic) = ic {
                run.receipt.changed.extend(ids.iter().map(|&i| Ref::id(*ic, i)));
            }
        }
    }
    match cat {
        Cat::WallType => m.delete_wall_type(id)?,
        Cat::SlabType => m.delete_slab_type(id)?,
        _ => m.delete_member_type(id)?,
    }
    run.receipt.deleted.push(Ref::id(cat, id));
    Ok(())
}

pub(super) fn is_type(cat: Cat) -> bool {
    matches!(cat, Cat::WallType | Cat::SlabType | Cat::MemberType)
}

/// Lowers the own fields of one type row. Wall type renames and layers come back as edits for
/// the caller's batch; slab and member types are renamed here through their model functions.
/// `all` is the op's field list: a failure names its index there as `/fields/i`.
pub(super) fn type_own(
    m: &mut Model,
    cat: Cat,
    id: i64,
    own: &[&Field],
    all: &[Field],
) -> Result<Vec<Edit>, Fail> {
    let label = Ref::id(cat, id).label();
    let mut name = None;
    let mut stored: Option<Vec<(i64, LayerSpec)>> = None;
    let mut layers: Vec<(i64, LayerSpec)> = Vec::new();
    for &f in own {
        let at = |e: Fail| match all.iter().position(|g| std::ptr::eq(g, f)) {
            Some(i) => e.at(format!("/fields/{i}")),
            None => e,
        };
        if is_layer_field(cat, &f.name) {
            if stored.is_none() {
                stored = Some(wall_layers(m, id)?);
            }
            let have = stored.as_deref().unwrap_or_default();
            layer_field(have, &mut layers, &label, f).map_err(at)?;
            continue;
        }
        match f.name.as_str() {
            "name" => name = Some(free_name(m, cat, id, &label, f).map_err(at)?),
            other => {
                return Err(at(unsupported(format!(
                    "{label}: field `{other}` is not settable (settable: {}{})",
                    TYPE_FIELDS.join(", "),
                    layer_hint(cat)
                ))))
            }
        }
    }
    let mut edits: Vec<Edit> =
        layers.into_iter().map(|(pos, layer)| Edit::Layer { wtype: id, pos, layer }).collect();
    let Some(name) = name else { return Ok(edits) };
    match cat {
        Cat::WallType => edits.insert(0, Edit::WallType { id, name }),
        Cat::SlabType => {
            let layers = slab_layers(m, id)?;
            m.set_slab_type(id, &name, &layers)?;
        }
        Cat::MemberType => {
            let (shape, material) = member_row(m, id, &label)?;
            m.set_member_type(id, &name, &shape, &material)?;
        }
        _ => return Err(unsupported(format!("{label}: not a type row"))),
    }
    Ok(edits)
}

/// One `layer.N.<attr>` field merged into `out` (one entry per position, stored values first).
fn layer_field(
    have: &[(i64, LayerSpec)],
    out: &mut Vec<(i64, LayerSpec)>,
    label: &str,
    f: &Field,
) -> Result<(), Fail> {
    let rest = &f.name[LAYER_PREFIX.len()..];
    let Some((n, attr)) = rest.split_once('.') else {
        return Err(unsupported(format!(
            "{label}: field `{}`: write layer.N.material or layer.N.thickness",
            f.name
        )));
    };
    let pos: i64 = match n.parse() {
        Ok(p) if p >= 0 && n.bytes().all(|b| b.is_ascii_digit()) => p,
        _ => {
            return Err(bad_value(format!(
                "{label}: field `{}`: layer index `{n}` is not a whole number from 0",
                f.name
            )))
        }
    };
    let Some(base) = have.iter().find(|(p, _)| *p == pos).map(|(_, l)| l.clone()) else {
        let known: Vec<String> = have.iter().map(|(p, _)| p.to_string()).collect();
        return Err(bad_ref(format!(
            "{label}: field `{}`: no layer {pos} (layers: {})",
            f.name,
            if known.is_empty() { "none".to_owned() } else { known.join(", ") }
        )));
    };
    let i = match out.iter().position(|(p, _)| *p == pos) {
        Some(i) => i,
        None => {
            out.push((pos, base));
            out.len() - 1
        }
    };
    let layer = &mut out[i].1;
    match attr {
        "material" => {
            let text = text_of(f).map(|t| t.trim().to_owned()).unwrap_or_default();
            if text.is_empty() {
                return Err(bad_value(format!("{label}: `{}` needs non-empty text", f.name)));
            }
            layer.material = text;
        }
        "thickness" => {
            let t = length_of(f).map_err(|_| {
                bad_value(format!("{label}: `{}` needs a length with a unit (m, mm, ...)", f.name))
            })?;
            if !(t > 0.0) {
                return Err(bad_value(format!("{label}: `{}` must be above 0", f.name)));
            }
            layer.thickness = t;
        }
        other => {
            return Err(unsupported(format!(
                "{label}: field `{}`: layer attribute `{other}` is not settable (material, thickness)",
                f.name
            )))
        }
    }
    Ok(())
}

fn wall_layers(m: &Model, id: i64) -> Result<Vec<(i64, LayerSpec)>, Fail> {
    let mut st = m
        .db
        .prepare("SELECT pos, material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos")?;
    let rows = st.query_map([id], |r| {
        Ok((r.get(0)?, LayerSpec { material: r.get(1)?, thickness: r.get(2)? }))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The new name, trimmed; refused when another row of the same table has it.
fn free_name(m: &Model, cat: Cat, id: i64, label: &str, f: &Field) -> Result<String, Fail> {
    let name = text_of(f)
        .map_err(|_| bad_value(format!("{label}: `name` needs non-empty text")))?
        .trim()
        .to_owned();
    let t = table(cat).ok_or_else(|| unsupported(format!("{label}: no table")))?;
    let other: Option<i64> = m
        .db
        .query_row(
            &format!("SELECT id FROM {t} WHERE name=?1 AND id<>?2 LIMIT 1"),
            rusqlite::params![name, id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(o) = other {
        return Err(bad_value(format!(
            "{label}: `name` \"{name}\" is already used by {}; type names are unique",
            Ref::id(cat, o).label()
        )));
    }
    Ok(name)
}

fn slab_layers(m: &Model, id: i64) -> Result<Vec<LayerSpec>, Fail> {
    let mut st = m
        .db
        .prepare("SELECT material, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos")?;
    let rows = st.query_map([id], |r| Ok(LayerSpec { material: r.get(0)?, thickness: r.get(1)? }))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn member_row(m: &Model, id: i64, label: &str) -> Result<(MemberShape, String), Fail> {
    let (shape, material): (String, String) =
        m.db.query_row("SELECT shape, material FROM member_type WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let shape = MemberShape::parse(&shape)
        .ok_or_else(|| bad_value(format!("{label}: stored section \"{shape}\" does not parse")))?;
    Ok((shape, material))
}
