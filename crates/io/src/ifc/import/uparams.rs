// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `DDC_Parameters` property set (written by the exporter from project parameter values) goes
//! back onto the project parameter of the same name and type instead of the generic `prop` table.
//! A missing parameter is created, bound to the element's category; a parameter of another type
//! keeps the value as a generic property. Every write goes through the import batch's snapshots
//! and watermarks, so the import stays one undo step. On a linked row the value is 3-way merged
//! with the last import's base (field `p:DDC_Parameters.<name>`), like any other field.

use ddc_model::source::{Decision, Merge};
use ddc_model::{Error, Snapshots, PARAM_CATEGORIES, PARAM_TYPE_OF};
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as J;

use super::{PProp, PVal};

pub(super) const PREFIX: &str = "DDC_Parameters.";

/// True: `key` belongs to the `DDC_Parameters` set.
pub(super) fn is_param(key: &str) -> bool {
    key.strip_prefix(PREFIX).is_some_and(|n| !n.is_empty())
}

/// Categories of a row of table `kind` (door/window by the opening kind); empty: not a category row.
fn categories(db: &Connection, kind: &str, id: i64) -> Result<Vec<&'static str>, Error> {
    let mut out = vec![];
    for (cat, table) in PARAM_CATEGORIES {
        if table != kind {
            continue;
        }
        let ok: Option<i64> = if kind == "opening" {
            db.query_row(
                "SELECT 1 FROM opening WHERE id=?1 AND kind=?2",
                params![id, cat],
                |r| r.get(0),
            )
            .optional()?
        } else {
            db.query_row(&format!("SELECT 1 FROM {kind} WHERE id=?1"), [id], |r| r.get(0))
                .optional()?
        };
        if ok.is_some() {
            out.push(cat);
        }
    }
    Ok(out)
}

/// (dimension, factor to SI) of a unit label the importer may hand over.
fn scale(unit: &str) -> Option<(char, f64)> {
    Some(match unit {
        "m" => ('L', 1.0),
        "mm" => ('L', 1e-3),
        "cm" => ('L', 1e-2),
        "km" => ('L', 1e3),
        "ft" => ('L', 0.3048),
        "in" => ('L', 0.0254),
        "m2" => ('A', 1.0),
        "mm2" => ('A', 1e-6),
        "cm2" => ('A', 1e-4),
        "m3" => ('V', 1.0),
        "mm3" => ('V', 1e-9),
        "cm3" => ('V', 1e-6),
        "kg" => ('M', 1.0),
        "g" => ('M', 1e-3),
        _ => return None,
    })
}

/// Stored value of `v` (unit `unit`) for an existing parameter of type `ptype` with unit label
/// `punit`; None: does not fit. A Number parameter exported with a unit comes back as a measure
/// (SI) and is turned back into its own unit.
fn fit(ptype: &str, punit: &str, v: &PVal, unit: &str) -> Option<Value> {
    let real = match v {
        PVal::Real(x) => Some(*x),
        PVal::Int(i) => Some(*i as f64),
        _ => None,
    };
    let si = |dim: char| -> Option<Value> {
        let x = real?;
        match unit {
            "" => Some(Value::Real(x)),
            u => match scale(u) {
                Some((d, f)) if d == dim => Some(Value::Real(x * f)),
                _ => None,
            },
        }
    };
    match (ptype, v) {
        ("text", PVal::Text(s)) => Some(Value::Text(s.clone())),
        ("yesno", PVal::Bool(b)) => Some(Value::Integer(i64::from(*b))),
        ("int", PVal::Int(i)) => Some(Value::Integer(*i)),
        ("length", _) => si('L'),
        ("area", _) => si('A'),
        ("volume", _) => si('V'),
        ("number", _) => {
            let x = real?;
            let x = match (scale(unit), scale(punit)) {
                (Some((a, f)), Some((b, g))) if a == b && unit != punit => x * f / g,
                _ => x,
            };
            Some(Value::Real(x))
        }
        _ => None,
    }
}

/// Where a `DDC_Parameters` value of the file goes, before anything is written.
pub(super) struct Target {
    /// Existing parameter; None: created on [`Target::write`].
    pid: Option<i64>,
    /// Parameter name as stored (the file's spelling for a new one).
    name: String,
    ptype: &'static str,
    unit: String,
    per_type: bool,
    cats: Vec<&'static str>,
    value: Value,
}

impl Target {
    /// Merge field of this value (see `ddc_model::source::UPARAM_FIELD`).
    pub fn field(&self) -> String {
        format!("{}{}", ddc_model::source::UPARAM_FIELD, self.name)
    }

    /// The file's value as the merge compares it.
    pub fn theirs(&self) -> J {
        ddc_model::source::enc(&self.value)
    }

    /// The value the element holds here now; Null without the parameter or a value.
    pub fn mine(&self, db: &Connection, kind: &str, id: i64) -> Result<J, Error> {
        let Some(pid) = self.pid else {
            return Ok(J::Null);
        };
        match self.slot(db, pid, kind, id)? {
            Some(s) => ddc_model::source::uparam_value(db, &s),
            None => Ok(J::Null),
        }
    }

    fn slot(
        &self,
        db: &Connection,
        pid: i64,
        kind: &str,
        id: i64,
    ) -> Result<Option<(i64, String, i64)>, Error> {
        if !self.per_type {
            return Ok(Some((pid, kind.to_string(), id)));
        }
        let Some(&(_, tt, col)) = PARAM_TYPE_OF.iter().find(|t| self.cats.contains(&t.0)) else {
            return Ok(None);
        };
        let t: Option<i64> = db
            .query_row(&format!("SELECT {col} FROM {kind} WHERE id=?1"), [id], |r| r.get(0))
            .optional()?;
        Ok(t.map(|t| (pid, tt.to_string(), t)))
    }

    /// Creates the parameter or binds the element's category when needed, then stores the value;
    /// false: no slot (type parameter without a type). Undoable through `snaps`.
    pub fn write(
        &self,
        db: &Connection,
        snaps: &mut Snapshots,
        kind: &str,
        id: i64,
    ) -> Result<bool, Error> {
        let pid = match self.pid {
            Some(pid) => pid,
            None => {
                snaps.watermark(db, "pparam")?;
                db.execute(
                    "INSERT INTO pparam(name, ptype, per_type, unit, grp) VALUES (?1,?2,0,?3,'')",
                    params![self.name, self.ptype, self.unit],
                )?;
                db.last_insert_rowid()
            }
        };
        let Some(slot) = self.slot(db, pid, kind, id)? else {
            return Ok(false);
        };
        for c in &self.cats {
            let bound: Option<i64> = db
                .query_row(
                    "SELECT 1 FROM pparam_cat WHERE param=?1 AND category=?2",
                    params![pid, c],
                    |r| r.get(0),
                )
                .optional()?;
            if bound.is_none() {
                snaps.watermark(db, "pparam_cat")?;
                db.execute(
                    "INSERT INTO pparam_cat(param, category) VALUES (?1,?2)",
                    params![pid, c],
                )?;
            }
        }
        ddc_model::source::write_uparam(db, snaps, &slot, &self.value)?;
        Ok(true)
    }
}

/// Target of property `p` on row (kind, id); None: not a `DDC_Parameters` value this element
/// can hold as a parameter (kept as a generic property). Reads only.
pub(super) fn target(
    db: &Connection,
    kind: &str,
    id: i64,
    p: &PProp,
) -> Result<Option<Target>, Error> {
    let Some(name) = p.key.strip_prefix(PREFIX).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    let cats = categories(db, kind, id)?;
    if cats.is_empty() {
        return Ok(None);
    }
    let found: Option<(i64, String, String, i64, String)> = db
        .query_row(
            "SELECT id, name, ptype, per_type, unit FROM pparam WHERE name=?1 COLLATE NOCASE",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    Ok(match found {
        Some((pid, stored, t, pt, u)) => fit(&t, &u, &p.value, &p.unit).map(|value| Target {
            pid: Some(pid),
            name: stored,
            ptype: "",
            unit: u,
            per_type: pt != 0,
            cats,
            value,
        }),
        None => {
            let (ptype, unit) = match (&p.value, p.unit.as_str()) {
                (PVal::Text(_), _) => ("text", ""),
                (PVal::Bool(_), _) => ("yesno", ""),
                (PVal::Int(_), _) => ("int", ""),
                (PVal::Real(_), "m") => ("length", ""),
                (PVal::Real(_), "m2") => ("area", ""),
                (PVal::Real(_), "m3") => ("volume", ""),
                (PVal::Real(_), u) => ("number", u),
            };
            fit(ptype, unit, &p.value, &p.unit).map(|value| Target {
                pid: None,
                name: name.to_string(),
                ptype,
                unit: unit.to_string(),
                per_type: false,
                cats,
                value,
            })
        }
    })
}

/// True: the element stores `p` as a project parameter value (a slot exists or the parameter
/// would be created); false: it stays a generic property and goes through the generic merge.
pub(super) fn held(db: &Connection, kind: &str, id: i64, p: &PProp) -> Result<bool, Error> {
    let Some(t) = target(db, kind, id, p)? else {
        return Ok(false);
    };
    Ok(match t.pid {
        Some(pid) => t.slot(db, pid, kind, id)?.is_some(),
        None => true,
    })
}

/// Stores the property as a project parameter value without a merge (rows this import inserted);
/// Some(field, value) for the import's base, None: not a parameter value here.
pub(super) fn put(
    db: &Connection,
    snaps: &mut Snapshots,
    kind: &str,
    id: i64,
    p: &PProp,
) -> Result<Option<(String, J)>, Error> {
    let Some(t) = target(db, kind, id, p)? else {
        return Ok(None);
    };
    if !t.write(db, snaps, kind, id)? {
        return Ok(None);
    }
    Ok(Some((t.field(), t.theirs())))
}

/// Linked row with a base: the file value is decided against the last import like any other
/// field (local edit kept, file change taken, both changed: conflict with the local value kept);
/// the merge records the file value as the new base. False: not a parameter value here.
#[allow(clippy::too_many_arguments)]
pub(super) fn merge(
    db: &Connection,
    snaps: &mut Snapshots,
    merge: &mut Merge,
    guid: &str,
    kind: &str,
    id: i64,
    p: &PProp,
) -> Result<bool, Error> {
    if !held(db, kind, id, p)? {
        return Ok(false);
    }
    let Some(t) = target(db, kind, id, p)? else {
        return Ok(false);
    };
    let mine = t.mine(db, kind, id)?;
    // a locked value is kept without a merge decision; a change of the file is reported
    if let Some(pid) = t.pid {
        if let Some(s) = t.slot(db, pid, kind, id)? {
            if ddc_model::source::uparam_locked(db, &s)? {
                if mine != t.theirs() {
                    merge.stats.locked.push((kind.to_string(), id, t.name.clone()));
                }
                return Ok(true);
            }
        }
    }
    let f = t.field();
    let d = merge.row(db, snaps, guid, kind, id, &[(f.as_str(), mine, t.theirs())])?;
    if d.first() == Some(&Decision::Take) {
        t.write(db, snaps, kind, id)?;
    }
    Ok(true)
}
