// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project parameters: the definition (`pparam`: name, quantity type, instance/type, unit, group,
//! shared-parameter guid), category binding (`pparam_cat`), values (`pparam_val`, keyed by the
//! row's table and its id; for type parameters, the type table). Length/area/volume are in SI.
//! Values of deleted elements are removed by `sweep` in every operation (a cascade with undo).

use std::collections::HashMap;

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

/// Binding category -> instance table.
pub const CATEGORIES: [(&str, &str); 16] = [
    ("wall", "wall"),
    ("door", "opening"),
    ("window", "opening"),
    ("curtain_wall", "curtain_wall"),
    ("slab", "slab"),
    ("room", "room"),
    ("col", "col"),
    ("beam", "beam"),
    ("roof", "roof"),
    ("ceiling", "ceiling"),
    ("stair", "stair"),
    ("railing", "railing"),
    ("mep_run", "mep_run"),
    ("comp", "comp"),
    ("shaft", "shaft"),
    ("ramp", "ramp"),
];

/// (category, type table, type column in the instance table).
pub const TYPE_OF: [(&str, &str, &str); 7] = [
    ("wall", "wall_type", "wtype"),
    ("slab", "slab_type", "stype"),
    ("roof", "slab_type", "stype"),
    ("ceiling", "slab_type", "stype"),
    ("col", "member_type", "mtype"),
    ("beam", "member_type", "mtype"),
    ("comp", "comp_type", "ctype"),
];

/// Tables whose rows the values refer to (instances and types), for the cascade.
pub(crate) const VALUE_KINDS: [&str; 19] = [
    "wall",
    "opening",
    "curtain_wall",
    "slab",
    "room",
    "col",
    "beam",
    "roof",
    "ceiling",
    "stair",
    "railing",
    "mep_run",
    "comp",
    "shaft",
    "ramp",
    "wall_type",
    "slab_type",
    "member_type",
    "comp_type",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamType {
    Text,
    Int,
    Number,
    Length,
    Area,
    Volume,
    YesNo,
}

impl ParamType {
    pub const ALL: [ParamType; 7] = [
        ParamType::Text,
        ParamType::Int,
        ParamType::Number,
        ParamType::Length,
        ParamType::Area,
        ParamType::Volume,
        ParamType::YesNo,
    ];
    pub fn key(self) -> &'static str {
        match self {
            ParamType::Text => "text",
            ParamType::Int => "int",
            ParamType::Number => "number",
            ParamType::Length => "length",
            ParamType::Area => "area",
            ParamType::Volume => "volume",
            ParamType::YesNo => "yesno",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == s)
    }
    pub fn numeric(self) -> bool {
        !matches!(self, ParamType::Text | ParamType::YesNo)
    }
}

/// A project parameter value.
#[derive(Clone, Debug, PartialEq)]
pub enum PVal {
    Text(String),
    Int(i64),
    Num(f64),
    Bool(bool),
}

impl PVal {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PVal::Int(i) => Some(*i as f64),
            PVal::Num(x) => Some(*x),
            PVal::Bool(b) => Some(f64::from(u8::from(*b))),
            PVal::Text(_) => None,
        }
    }
    /// Text without units (SI); display in the view's units is the UI's concern.
    pub fn text(&self) -> String {
        match self {
            PVal::Text(s) => s.clone(),
            PVal::Int(i) => i.to_string(),
            PVal::Num(x) => {
                let s = format!("{x:.6}");
                let s = s.trim_end_matches('0').trim_end_matches('.');
                if s == "-0" {
                    "0".into()
                } else {
                    s.into()
                }
            }
            PVal::Bool(b) => if *b { "1" } else { "0" }.into(),
        }
    }
}

/// A definition for creation/editing.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: String,
    pub ptype: ParamType,
    /// The value lives on the type (otherwise on the instance).
    pub per_type: bool,
    /// Unit label for `Number` ("rub", "pcs"); not used for length/area/volume.
    pub unit: String,
    pub group: String,
    /// Shared parameter: a stable guid from the shared-parameter file.
    pub guid: Option<String>,
    pub categories: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamDef {
    pub id: i64,
    pub spec: ParamSpec,
}

/// One value record: `value` None clears it.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSet {
    pub param: i64,
    pub kind: String,
    pub id: i64,
    pub value: Option<PVal>,
}

/// Category of a table row (door/window by opening kind); None if there is no row or the opening is a niche.
pub fn category_of(db: &Connection, kind: &str, id: i64) -> Result<Option<&'static str>, Error> {
    if kind == "opening" {
        let k: Option<String> = db
            .prepare_cached("SELECT kind FROM opening WHERE id=?1")?
            .query_row([id], |r| r.get(0))
            .optional()?;
        return Ok(match k.as_deref() {
            Some("door") => Some("door"),
            Some("window") => Some("window"),
            _ => None,
        });
    }
    let Some(&(cat, table)) = CATEGORIES.iter().find(|c| c.1 == kind) else {
        return Ok(None);
    };
    Ok(exists(db, table, id)?.then_some(cat))
}

/// Instance type: (type table, id) for categories with types.
pub fn type_of(db: &Connection, kind: &str, id: i64) -> Result<Option<(&'static str, i64)>, Error> {
    let Some(&(_, tt, col)) = TYPE_OF.iter().find(|t| t.0 == kind) else {
        return Ok(None);
    };
    let t: Option<i64> = db
        .prepare_cached(&format!("SELECT {col} FROM {kind} WHERE id=?1"))?
        .query_row([id], |r| r.get(0))
        .optional()?;
    Ok(t.map(|t| (tt, t)))
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    Ok(db
        .prepare_cached(&format!("SELECT 1 FROM {table} WHERE id=?1"))?
        .query_row([id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn bad(msg: String) -> Error {
    Error::Invalid(msg)
}

fn check_spec(s: &ParamSpec) -> Result<(), Error> {
    let name = s.name.trim();
    // characters, not bytes: Cyrillic is 2 bytes per letter
    if name.is_empty() || name.chars().count() > 120 {
        return Err(bad("parameter name must be 1..120 characters".into()));
    }
    if s.categories.is_empty() {
        return Err(bad(format!("parameter '{name}': no categories bound")));
    }
    for c in &s.categories {
        if !CATEGORIES.iter().any(|k| k.0 == c) {
            return Err(bad(format!("parameter '{name}': unknown category '{c}'")));
        }
        if s.per_type && !TYPE_OF.iter().any(|t| t.0 == c) {
            return Err(bad(format!(
                "parameter '{name}': category '{c}' has no types for a type parameter"
            )));
        }
    }
    Ok(())
}

/// Stored or machine text -> value by parameter type. Numbers in the invariant form, or an
/// unambiguous legacy comma decimal ([`crate::num_text`]); never by the UI mark, no grouping
/// (`1,250`, `1 250` are refused). Yes/no: 1/0, yes/no and the Russian words.
pub fn parse_value(t: ParamType, s: &str) -> Result<PVal, Error> {
    let s = s.trim();
    let num = || -> Result<f64, Error> {
        crate::num_text::parse_stored(s).ok_or_else(|| bad(format!("not a number: '{s}'")))
    };
    Ok(match t {
        ParamType::Text => PVal::Text(s.to_string()),
        ParamType::Int => {
            let x = num()?;
            if x.fract() != 0.0 || x.abs() > 9.0e15 {
                return Err(bad(format!("not an integer: '{s}'")));
            }
            PVal::Int(x as i64)
        }
        ParamType::YesNo => match s.to_lowercase().as_str() {
            "1" | "\u{434}\u{430}" | "yes" | "true" | "\u{438}\u{441}\u{442}\u{438}\u{43d}\u{430}" | "y" | "\u{434}" => PVal::Bool(true),
            "0" | "\u{43d}\u{435}\u{442}" | "no" | "false" | "\u{43b}\u{43e}\u{436}\u{44c}" | "n" | "\u{43d}" | "" => PVal::Bool(false),
            _ => return Err(bad(format!("not yes/no: '{s}'"))),
        },
        _ => PVal::Num(num()?),
    })
}

/// Value -> SQL by type; an incompatible value is an error (text is not converted to a number).
fn coerce(t: ParamType, v: &PVal) -> Result<Value, Error> {
    Ok(match (t, v) {
        (ParamType::Text, v) => Value::Text(v.text()),
        (ParamType::YesNo, PVal::Bool(b)) => Value::Integer(i64::from(*b)),
        (ParamType::YesNo, PVal::Int(i)) if *i == 0 || *i == 1 => Value::Integer(*i),
        (ParamType::Int, PVal::Int(i)) => Value::Integer(*i),
        (ParamType::Int, PVal::Num(x)) if x.fract() == 0.0 && x.abs() < 9.0e15 => {
            Value::Integer(*x as i64)
        }
        (_, PVal::Text(s)) => match parse_value(t, s)? {
            PVal::Text(_) => unreachable!(),
            p => return coerce(t, &p),
        },
        (ParamType::Number | ParamType::Length | ParamType::Area | ParamType::Volume, v) => {
            match v.as_f64() {
                Some(x) if x.is_finite() && !matches!(v, PVal::Bool(_)) => Value::Real(x),
                _ => return Err(bad(format!("not a finite number: {v:?}"))),
            }
        }
        (t, v) => return Err(bad(format!("value {v:?} does not fit {}", t.key()))),
    })
}

fn from_sql(t: ParamType, v: Value) -> Option<PVal> {
    Some(match (t, v) {
        (_, Value::Null) => return None,
        (ParamType::Text, Value::Text(s)) => PVal::Text(s),
        (ParamType::YesNo, Value::Integer(i)) => PVal::Bool(i != 0),
        (ParamType::Int, Value::Integer(i)) => PVal::Int(i),
        (_, Value::Integer(i)) => PVal::Num(i as f64),
        (_, Value::Real(x)) => PVal::Num(x),
        (_, Value::Text(s)) => PVal::Text(s),
        (_, Value::Blob(_)) => return None,
    })
}

pub(crate) fn load_defs(db: &Connection) -> Result<Vec<ParamDef>, Error> {
    let mut cats: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut st = db.prepare_cached("SELECT param, category FROM pparam_cat ORDER BY rowid")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for r in rows {
            let (p, c) = r?;
            cats.entry(p).or_default().push(c);
        }
    }
    let mut st = db.prepare_cached(
        "SELECT id, name, ptype, per_type, unit, grp, guid FROM pparam ORDER BY grp, name",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    let mut out = vec![];
    for r in rows {
        let (id, name, pt, per_type, unit, group, guid) = r?;
        let ptype = ParamType::from_key(&pt)
            .ok_or_else(|| Error::Corrupt(format!("parameter {id}: type '{pt}'")))?;
        out.push(ParamDef {
            id,
            spec: ParamSpec {
                name,
                ptype,
                per_type: per_type != 0,
                unit,
                group,
                guid,
                categories: cats.remove(&id).unwrap_or_default(),
            },
        });
    }
    Ok(out)
}

fn def_by_id(db: &Connection, id: i64) -> Result<ParamDef, Error> {
    load_defs(db)?
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| bad(format!("no such parameter {id}")))
}

/// Value rows of `id` not bound by `cats`: (rowid, kind, ref, value is non-empty).
/// One rule for `edit_param` (deletes them) and `dropped_values` (counts them).
fn unbound_rows(
    db: &Connection,
    id: i64,
    per_type: bool,
    cats: &[String],
) -> Result<Vec<(i64, String, i64, bool)>, Error> {
    let rows: Vec<(i64, String, i64, bool)> = {
        let mut st = db.prepare(
            "SELECT rowid, kind, ref, (value IS NOT NULL AND value <> '') \
             FROM pparam_val WHERE param=?1 ORDER BY rowid",
        )?;
        let r = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        r.collect::<Result<_, _>>()?
    };
    let bound = |c: &str| cats.iter().any(|x| x == c);
    let mut out = vec![];
    for row in rows {
        let keep = if per_type {
            TYPE_OF.iter().any(|t| t.1 == row.1 && bound(t.0))
        } else {
            category_of(db, &row.1, row.2)?.is_some_and(bound)
        };
        if !keep {
            out.push(row);
        }
    }
    Ok(out)
}

/// The guid is not taken by another parameter (`except` is the one being edited).
fn guid_free(db: &Connection, g: &str, except: Option<i64>) -> Result<(), Error> {
    let dup: Option<String> = db
        .query_row(
            "SELECT name FROM pparam WHERE guid=?1 AND id<>?2",
            params![g, except.unwrap_or(-1)],
            |r| r.get(0),
        )
        .optional()?;
    match dup {
        Some(n) => Err(bad(format!("shared parameter guid {g} is already '{n}'"))),
        None => Ok(()),
    }
}

/// Inserts a definition (shared by `add_param` and the XLSX batch); the inverse command goes to `inv`.
pub(crate) fn insert_def(db: &Connection, s: &ParamSpec, inv: Inv) -> Result<(i64, Inv), Error> {
    check_spec(s)?;
    let name = s.name.trim();
    let dup: Option<i64> = db
        .query_row("SELECT id FROM pparam WHERE name=?1", [name], |r| r.get(0))
        .optional()?;
    if dup.is_some() {
        return Err(bad(format!("parameter '{name}' already exists")));
    }
    if let Some(g) = &s.guid {
        guid_free(db, g, None)?;
    }
    // the guid is assigned once at creation, so the shared-parameter export is stable
    let guid = match &s.guid {
        Some(g) => g.clone(),
        None => free_guid(db, &format!("{name}|{}", s.ptype.key()))?,
    };
    db.execute(
        "INSERT INTO pparam(name, ptype, per_type, unit, grp, guid) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            name,
            s.ptype.key(),
            i64::from(s.per_type),
            s.unit,
            s.group,
            guid
        ],
    )?;
    let id = db.last_insert_rowid();
    for c in &s.categories {
        db.execute(
            "INSERT OR IGNORE INTO pparam_cat(param, category) VALUES (?1,?2)",
            params![id, c],
        )?;
    }
    let inv = Inv::default()
        .delete("pparam_cat", "param", id)
        .delete("pparam", "id", id)
        .then(inv);
    Ok((id, inv))
}

/// Validates and writes one value; a snapshot of the previous one goes to `inv`.
pub(crate) fn write_value(
    db: &Connection,
    defs: &[ParamDef],
    s: &ParamSet,
    mut inv: Inv,
) -> Result<Inv, Error> {
    let d = defs
        .iter()
        .find(|d| d.id == s.param)
        .ok_or_else(|| bad(format!("no such parameter {}", s.param)))?;
    let name = &d.spec.name;
    if crate::groups::is_locked_in(db, &s.kind, s.id, name)? {
        return Err(bad(format!(
            "parameter '{name}' of {} {} is locked: unlock it to change the value",
            s.kind, s.id
        )));
    }
    let bound = |c: &str| d.spec.categories.iter().any(|x| x == c);
    if d.spec.per_type {
        let ok = TYPE_OF.iter().any(|t| t.1 == s.kind && bound(t.0));
        if !ok || !exists(db, &s.kind, s.id)? {
            return Err(bad(format!(
                "parameter '{name}' is a type parameter: {} {} is not a bound type",
                s.kind, s.id
            )));
        }
    } else {
        match category_of(db, &s.kind, s.id)? {
            Some(c) if bound(c) => {}
            Some(c) => {
                return Err(bad(format!(
                    "parameter '{name}' is not bound to category '{c}'"
                )))
            }
            None => return Err(bad(format!("no such element {} {}", s.kind, s.id))),
        }
    }
    let old: Option<(i64, Value)> = db
        .prepare_cached(
            "SELECT rowid, value FROM pparam_val WHERE param=?1 AND kind=?2 AND ref=?3",
        )?
        .query_row(params![s.param, s.kind, s.id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    inv = match &old {
        Some((rowid, v)) => inv.sql(
            "INSERT OR REPLACE INTO pparam_val(rowid, param, kind, ref, value) VALUES (?1,?2,?3,?4,?5)",
            vec![
                Value::Integer(*rowid),
                Value::Integer(s.param),
                Value::Text(s.kind.clone()),
                Value::Integer(s.id),
                v.clone(),
            ],
        ),
        None => inv.sql(
            "DELETE FROM pparam_val WHERE param=?1 AND kind=?2 AND ref=?3",
            vec![
                Value::Integer(s.param),
                Value::Text(s.kind.clone()),
                Value::Integer(s.id),
            ],
        ),
    };
    let changed = match &s.value {
        None => {
            db.prepare_cached("DELETE FROM pparam_val WHERE param=?1 AND kind=?2 AND ref=?3")?
                .execute(params![s.param, s.kind, s.id])?;
            old.is_some()
        }
        Some(v) => {
            let v = coerce(d.spec.ptype, v)?;
            let changed = old.as_ref().is_none_or(|o| o.1 != v);
            match old {
                Some((rowid, _)) => {
                    db.prepare_cached("UPDATE pparam_val SET value=?2 WHERE rowid=?1")?
                        .execute(params![rowid, v])?;
                }
                None => {
                    db.prepare_cached(
                        "INSERT INTO pparam_val(param, kind, ref, value) VALUES (?1,?2,?3,?4)",
                    )?
                    .execute(params![s.param, s.kind, s.id, v])?;
                }
            }
            changed
        }
    };
    if !changed {
        // the same value written again (an unchanged XLSX round trip) keeps its source
        return Ok(inv);
    }
    // a new value: who set the old one no longer holds (the AI Apply records after this)
    crate::value_src::forget(db, &s.kind, s.id, name, inv)
}

pub(crate) fn issues(db: &Connection) -> Result<Vec<crate::Issue>, Error> {
    let mut out = vec![];
    let mut push = |table: &str, sql: &str, problem: &str| -> Result<(), Error> {
        let mut st = db.prepare(sql)?;
        let ids: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        out.extend(ids.into_iter().map(|id| crate::Issue {
            table: table.into(),
            id,
            problem: problem.into(),
        }));
        Ok(())
    };
    push(
        "pparam_cat",
        "SELECT rowid FROM pparam_cat WHERE param NOT IN (SELECT id FROM pparam)",
        "category binding of a missing parameter",
    )?;
    push(
        "pparam_val",
        "SELECT rowid FROM pparam_val WHERE param NOT IN (SELECT id FROM pparam)",
        "value of a missing parameter",
    )?;
    for k in VALUE_KINDS {
        push(
            "pparam_val",
            &format!(
                "SELECT rowid FROM pparam_val WHERE kind='{k}' AND ref NOT IN (SELECT id FROM {k})"
            ),
            &format!("value of a missing {k}"),
        )?;
    }
    let known = VALUE_KINDS
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");
    push(
        "pparam_val",
        &format!("SELECT rowid FROM pparam_val WHERE kind NOT IN ({known})"),
        "value of an unknown element kind",
    )?;
    Ok(out)
}

impl Model {
    /// All project parameters (by group and name).
    pub fn params(&self) -> Result<Vec<ParamDef>, Error> {
        load_defs(&self.db)
    }

    /// Parameters bound to a category (`CATEGORIES`).
    pub fn params_for(&self, category: &str) -> Result<Vec<ParamDef>, Error> {
        Ok(load_defs(&self.db)?
            .into_iter()
            .filter(|d| d.spec.categories.iter().any(|c| c == category))
            .collect())
    }

    pub fn param_by_name(&self, name: &str) -> Result<Option<ParamDef>, Error> {
        Ok(load_defs(&self.db)?
            .into_iter()
            .find(|d| d.spec.name == name.trim()))
    }

    pub fn add_param(&mut self, s: &ParamSpec) -> Result<i64, Error> {
        self.op("pparam_add", |db| {
            let (id, inv) = insert_def(db, s, Inv::default())?;
            Ok((id, id, inv))
        })
    }

    /// Many definitions in one operation (shared-parameter import): the whole list is validated
    /// first (names case-insensitively, guids within the list and the model), then inserted; an
    /// error in any one adds nothing.
    pub fn add_params(&mut self, specs: &[ParamSpec]) -> Result<Vec<i64>, Error> {
        if specs.is_empty() {
            return Ok(vec![]);
        }
        let mut names = std::collections::HashSet::new();
        let mut guids = std::collections::HashSet::new();
        for s in specs {
            check_spec(s)?;
            let n = s.name.trim().to_lowercase();
            if !names.insert(n) {
                return Err(bad(format!(
                    "parameter '{}' is listed twice",
                    s.name.trim()
                )));
            }
            if let Some(g) = &s.guid {
                if !guids.insert(g.to_lowercase()) {
                    return Err(bad(format!("shared parameter guid {g} is listed twice")));
                }
            }
        }
        self.op("pparam_add", |db| {
            let have: Vec<String> = load_defs(db)?
                .into_iter()
                .map(|d| d.spec.name.to_lowercase())
                .collect();
            if let Some(s) = specs
                .iter()
                .find(|s| have.contains(&s.name.trim().to_lowercase()))
            {
                return Err(bad(format!("parameter '{}' already exists", s.name.trim())));
            }
            let mut inv = Inv::default();
            let mut ids = vec![];
            for s in specs {
                let (id, i) = insert_def(db, s, Inv::default())?;
                inv = i.then(inv);
                ids.push(id);
            }
            Ok((ids.clone(), ids[0], inv))
        })
    }

    /// Assigns a guid to parameters without one (files from before creation-time assignment); not a
    /// model edit, so it is not journaled. Call before exporting shared parameters.
    pub fn ensure_param_guids(&mut self) -> Result<usize, Error> {
        let rows: Vec<(i64, String)> = {
            let mut st = self
                .db
                .prepare("SELECT id, name FROM pparam WHERE guid IS NULL OR guid=''")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            r.collect::<Result<_, _>>()?
        };
        for (id, name) in &rows {
            self.db.execute(
                "UPDATE pparam SET guid=?2 WHERE id=?1",
                params![id, new_guid(&format!("{id}:{name}"))],
            )?;
        }
        Ok(rows.len())
    }

    /// Edits a definition. Changing the quantity type or "instance/type" while values exist is
    /// rejected; a removed category takes the values of its elements with it (one operation, undo).
    pub fn edit_param(&mut self, id: i64, s: &ParamSpec) -> Result<(), Error> {
        check_spec(s)?;
        self.op("pparam_edit", |db| {
            let old = def_by_id(db, id)?;
            let nvals: i64 =
                db.query_row("SELECT count(*) FROM pparam_val WHERE param=?1", [id], |r| r.get(0))?;
            if nvals > 0 && (old.spec.ptype != s.ptype || old.spec.per_type != s.per_type) {
                return Err(Error::Conflict(format!(
                    "parameter '{}' has {nvals} values: its kind cannot change",
                    old.spec.name
                )));
            }
            let name = s.name.trim();
            let dup: Option<i64> = db
                .query_row("SELECT id FROM pparam WHERE name=?1 AND id<>?2", params![name, id], |r| r.get(0))
                .optional()?;
            if dup.is_some() {
                return Err(bad(format!("parameter '{name}' already exists")));
            }
            if let Some(g) = &s.guid {
                guid_free(db, g, Some(id))?;
            }
            let mut inv = Inv::default()
                .restore(db, "pparam", "id", id)?
                .delete("pparam_cat", "param", id)
                .restore(db, "pparam_cat", "param", id)?;
            db.execute(
                "UPDATE pparam SET name=?2, ptype=?3, per_type=?4, unit=?5, grp=?6, guid=?7 WHERE id=?1",
                params![id, name, s.ptype.key(), i64::from(s.per_type), s.unit, s.group, s.guid],
            )?;
            db.execute("DELETE FROM pparam_cat WHERE param=?1", [id])?;
            for c in &s.categories {
                db.execute(
                    "INSERT OR IGNORE INTO pparam_cat(param, category) VALUES (?1,?2)",
                    params![id, c],
                )?;
            }
            // values of elements in the removed categories
            let defs = load_defs(db)?;
            let d = defs.iter().find(|d| d.id == id).expect("just written");
            for (rowid, _, _, _) in unbound_rows(db, id, d.spec.per_type, &d.spec.categories)? {
                inv = inv.restore(db, "pparam_val", "rowid", rowid)?;
                db.execute("DELETE FROM pparam_val WHERE rowid=?1", [rowid])?;
            }
            let inv = inv.then(crate::groups::rename_locks(db, &old.spec.name, name)?);
            Ok(((), id, inv))
        })
    }

    /// Deletes a definition together with its bindings and values (one operation, undo).
    pub fn delete_param(&mut self, id: i64) -> Result<(), Error> {
        self.op("pparam_delete", |db| {
            let name = def_by_id(db, id)?.spec.name;
            let inv = Inv::default()
                .restore(db, "pparam", "id", id)?
                .restore(db, "pparam_cat", "param", id)?
                .restore(db, "pparam_val", "param", id)?;
            db.execute("DELETE FROM pparam_val WHERE param=?1", [id])?;
            db.execute("DELETE FROM pparam_cat WHERE param=?1", [id])?;
            db.execute("DELETE FROM pparam WHERE id=?1", [id])?;
            let inv = inv.then(crate::groups::drop_locks(db, &name)?);
            Ok(((), id, inv))
        })
    }

    /// Non-empty values `edit_param(id, ..)` with `categories` would remove, per unticked
    /// category in the parameter's order (read-only). Empty when nothing would be lost.
    pub fn dropped_values(&self, id: i64, categories: &[String]) -> Result<Vec<(String, usize)>, Error> {
        let old = def_by_id(&self.db, id)?.spec;
        let mut out: Vec<(String, usize)> = vec![];
        for (_, kind, rf, filled) in unbound_rows(&self.db, id, old.per_type, categories)? {
            if !filled {
                continue;
            }
            let cat = if old.per_type {
                // a shared type table (slab_type) goes to the first unticked category using it
                old.categories
                    .iter()
                    .find(|c| {
                        !categories.contains(c) && TYPE_OF.iter().any(|t| t.0 == c.as_str() && t.1 == kind)
                    })
                    .cloned()
            } else {
                category_of(&self.db, &kind, rf)?.map(str::to_string)
            }
            .unwrap_or(kind);
            match out.iter_mut().find(|(c, _)| *c == cat) {
                Some((_, n)) => *n += 1,
                None => out.push((cat, 1)),
            }
        }
        let rank = |c: &str| old.categories.iter().position(|x| x == c).unwrap_or(usize::MAX);
        out.sort_by_key(|(c, _)| rank(c));
        Ok(out)
    }

    /// (values, elements) that `delete_param` would remove. A value on a type counts the
    /// instances of that type as elements; a value on an instance counts that instance.
    pub fn param_usage(&self, id: i64) -> Result<(usize, usize), Error> {
        let rows: Vec<(String, i64)> = self
            .db
            .prepare("SELECT kind, ref FROM pparam_val WHERE param=?1")?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        // distinct (table, id): an element valued on its type and on itself counts once
        let mut els: std::collections::HashSet<(String, i64)> = Default::default();
        let mut types: Vec<(String, i64)> = vec![];
        for (kind, rf) in &rows {
            if TYPE_OF.iter().any(|t| t.1 == kind) {
                types.push((kind.clone(), *rf));
            } else {
                els.insert((kind.clone(), *rf));
            }
        }
        for &(inst, tk, col) in &TYPE_OF {
            let mut st = self.db.prepare(&format!("SELECT id, {col} FROM {inst}"))?;
            let all = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            for x in all {
                let (eid, tid) = x?;
                if types.iter().any(|(k, r)| k == tk && *r == tid) {
                    els.insert((inst.to_string(), eid));
                }
            }
        }
        Ok((rows.len(), els.len()))
    }

    /// Values of many elements in one operation (schedule cell edits, properties, XLSX).
    pub fn set_param_values(&mut self, sets: &[ParamSet]) -> Result<(), Error> {
        if sets.is_empty() {
            return Ok(());
        }
        self.op("pparam_set", |db| {
            let defs = load_defs(db)?;
            let mut inv = Inv::default();
            // inverse commands in reverse order: a repeated cell rolls back to the original
            for s in sets {
                inv = write_value(db, &defs, s, Inv::default())?.then(inv);
            }
            Ok(((), sets[0].id, inv))
        })
    }

    /// A row's own value (`kind` is the instance or type table).
    pub fn param_value(&self, param: i64, kind: &str, id: i64) -> Result<Option<PVal>, Error> {
        let d = def_by_id(&self.db, param)?;
        raw_value(&self.db, d.spec.ptype, param, kind, id)
    }

    /// Value for an instance: for a type parameter, the value of its type.
    pub fn param_value_of(&self, d: &ParamDef, kind: &str, id: i64) -> Result<Option<PVal>, Error> {
        // a type row holds its own value: `kind` names the type table itself
        let type_row = TYPE_OF.iter().any(|t| t.1 == kind);
        if d.spec.per_type && !type_row {
            match type_of(&self.db, kind, id)? {
                Some((tk, tid)) => raw_value(&self.db, d.spec.ptype, d.id, tk, tid),
                None => Ok(None),
            }
        } else {
            raw_value(&self.db, d.spec.ptype, d.id, kind, id)
        }
    }

    /// All own values of a parameter: (table, id) -> value.
    pub fn param_values(&self, param: i64) -> Result<HashMap<(String, i64), PVal>, Error> {
        let d = def_by_id(&self.db, param)?;
        let mut st = self
            .db
            .prepare_cached("SELECT kind, ref, value FROM pparam_val WHERE param=?1")?;
        let rows = st.query_map([param], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Value>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let (k, id, v) = r?;
            if let Some(v) = from_sql(d.spec.ptype, v) {
                out.insert((k, id), v);
            }
        }
        Ok(out)
    }

    /// Row category (door/window by opening kind).
    pub fn category_of(&self, kind: &str, id: i64) -> Result<Option<&'static str>, Error> {
        category_of(&self.db, kind, id)
    }

    /// Instance type: (type table, id); None if the category has no model types.
    pub fn type_of(&self, kind: &str, id: i64) -> Result<Option<(&'static str, i64)>, Error> {
        type_of(&self.db, kind, id)
    }
}

fn raw_value(
    db: &Connection,
    t: ParamType,
    param: i64,
    kind: &str,
    id: i64,
) -> Result<Option<PVal>, Error> {
    let v: Option<Value> = db
        .prepare_cached("SELECT value FROM pparam_val WHERE param=?1 AND kind=?2 AND ref=?3")?
        .query_row(params![param, kind, id], |r| r.get(0))
        .optional()?;
    Ok(v.and_then(|v| from_sql(t, v)))
}

/// Shared-parameter file (text, UTF-8): a header and rows
/// `guid<TAB>name<TAB>type<TAB>unit<TAB>group<TAB>type|instance<TAB>comma-separated categories`.
pub const SHARED_HEADER: &str = "# DDC shared parameters 1";

pub fn shared_text(defs: &[ParamDef]) -> String {
    let mut s = String::from(SHARED_HEADER);
    s.push('\n');
    for d in defs {
        let p = &d.spec;
        let clean = |x: &str| x.replace(['\t', '\n', '\r'], " ");
        s += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            p.guid.clone().unwrap_or_else(|| new_guid(&p.name)),
            clean(&p.name),
            p.ptype.key(),
            clean(&p.unit),
            clean(&p.group),
            if p.per_type { "type" } else { "instance" },
            p.categories.join(",")
        );
    }
    s
}

/// Parses a shared-parameter file; an error names the line.
pub fn parse_shared(text: &str) -> Result<Vec<ParamSpec>, Error> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(SHARED_HEADER) {
        return Err(bad("not a shared parameters file (header)".into()));
    }
    let mut out = vec![];
    for (i, l) in lines.enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split('\t').collect();
        let line = i + 2;
        if f.len() < 7 {
            return Err(bad(format!(
                "shared parameters line {line}: 7 fields expected"
            )));
        }
        let ptype = ParamType::from_key(f[2])
            .ok_or_else(|| bad(format!("shared parameters line {line}: type '{}'", f[2])))?;
        let spec = ParamSpec {
            name: f[1].trim().into(),
            ptype,
            per_type: f[5].trim() == "type",
            unit: f[3].into(),
            group: f[4].into(),
            guid: Some(f[0].trim().into()),
            categories: f[6]
                .split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect(),
        };
        check_spec(&spec).map_err(|e| bad(format!("shared parameters line {line}: {e}")))?;
        out.push(spec);
    }
    Ok(out)
}

/// The first free guid for a seed: a taken one (the parameter was renamed) yields the next.
fn free_guid(db: &Connection, seed: &str) -> Result<String, Error> {
    for n in 0..1000 {
        let g = new_guid(&format!("{seed}#{n}"));
        if guid_free(db, &g, None).is_ok() {
            return Ok(g);
        }
    }
    Err(bad(format!("no free shared parameter guid for '{seed}'")))
}

/// Version of the `new_guid` algorithm. Version 1 is SipHash-1-3 with zero keys, which is what
/// the standard library's `DefaultHasher` computed when files were first written, so guids made
/// before this algorithm was fixed in code are reproduced bit for bit.
pub const GUID_ALGO: u32 = 1;

/// SipHash-1-3 with zero keys over the whole message.
fn sip13(data: &[u8]) -> u64 {
    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }
    let mut v = [
        0x736f_6d65_7073_6575u64,
        0x646f_7261_6e64_6f6d,
        0x6c79_6765_6e65_7261,
        0x7465_6462_7974_6573,
    ];
    let words = data.chunks_exact(8);
    let rest = words.remainder();
    for w in words {
        let m = u64::from_le_bytes(w.try_into().expect("8 bytes"));
        v[3] ^= m;
        round(&mut v);
        v[0] ^= m;
    }
    let mut b = (data.len() as u64) << 56;
    for (i, x) in rest.iter().enumerate() {
        b |= u64::from(*x) << (8 * i);
    }
    v[3] ^= b;
    round(&mut v);
    v[0] ^= b;
    v[2] ^= 0xff;
    for _ in 0..3 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

/// A deterministic guid from a seed: a repeated operation (redo) must yield the same one (no external dependencies).
pub fn new_guid(seed: &str) -> String {
    let mut msg = seed.as_bytes().to_vec();
    msg.push(0xff);
    let a = sip13(&msg);
    msg.extend_from_slice(&a.rotate_left(17).to_le_bytes());
    let b = sip13(&msg);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xffff,
        b >> 48,
        b & 0xffff_ffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, OpeningSpec, WallSpec};

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        (m, w, t)
    }

    fn spec(name: &str, t: ParamType, cats: &[&str]) -> ParamSpec {
        ParamSpec {
            name: name.into(),
            ptype: t,
            per_type: false,
            unit: String::new(),
            group: "Estimate".into(),
            guid: None,
            categories: cats.iter().map(|c| c.to_string()).collect(),
        }
    }

    #[test]
    fn instance_values_validate_cascade_and_undo() {
        let (mut m, w, _) = model();
        let code = m
            .add_param(&spec("Work code", ParamType::Text, &["wall"]))
            .unwrap();
        let price = m
            .add_param(&spec("Price", ParamType::Number, &["wall", "door"]))
            .unwrap();
        m.set_param_values(&[
            ParamSet {
                param: code,
                kind: "wall".into(),
                id: w,
                value: Some(PVal::Text("01.02".into())),
            },
            ParamSet {
                param: price,
                kind: "wall".into(),
                id: w,
                value: Some(PVal::Text("12,5".into())),
            },
        ])
        .unwrap();
        assert_eq!(
            m.param_value(price, "wall", w).unwrap(),
            Some(PVal::Num(12.5))
        );
        // text is not converted to a number, a foreign category is rejected, nothing is written
        assert!(m
            .set_param_values(&[ParamSet {
                param: price,
                kind: "wall".into(),
                id: w,
                value: Some(PVal::Text("abc".into()))
            }])
            .is_err());
        let d = m
            .add_opening(
                w,
                &OpeningSpec {
                    along: 1.0,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        assert!(m
            .set_param_values(&[ParamSet {
                param: code,
                kind: "opening".into(),
                id: d,
                value: Some(PVal::Text("x".into()))
            }])
            .is_err());
        m.set_param_values(&[ParamSet {
            param: price,
            kind: "opening".into(),
            id: d,
            value: Some(PVal::Num(3.0)),
        }])
        .unwrap();
        m.assert_integrity();
        // deleting a wall removes its values (and the door's values); undo restores everything
        m.delete_wall(w).unwrap();
        assert!(m.param_values(price).unwrap().is_empty());
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(
            m.param_value(code, "wall", w).unwrap(),
            Some(PVal::Text("01.02".into()))
        );
        assert_eq!(
            m.param_value(price, "opening", d).unwrap(),
            Some(PVal::Num(3.0))
        );
        m.assert_integrity();
    }

    #[test]
    fn usage_counts_instances_of_a_typed_value() {
        let (mut m, w, t) = model();
        let lv = m.add_level("L2", 3.0).unwrap();
        m.add_wall(&WallSpec {
            a: (0.0, 1.0),
            b: (5.0, 1.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
        let mut s = spec("Fire", ParamType::Text, &["wall"]);
        s.per_type = true;
        let p = m.add_param(&s).unwrap();
        let set = |kind: &str, id| ParamSet {
            param: p,
            kind: kind.into(),
            id,
            value: Some(PVal::Text("EI60".into())),
        };
        m.set_param_values(&[set("wall_type", t)]).unwrap();
        // one value on the type, two walls of that type
        assert_eq!(m.param_usage(p).unwrap(), (1, 2));
        let i = m.add_param(&spec("Code", ParamType::Text, &["wall"])).unwrap();
        m.set_param_values(&[ParamSet { param: i, ..set("wall", w) }]).unwrap();
        assert_eq!(m.param_usage(i).unwrap(), (1, 1));
    }

    fn usage_fixture() -> (Model, i64, i64, i64) {
        let (mut m, w, t) = model();
        let mut s = spec("Fire", ParamType::Text, &["wall"]);
        s.per_type = true;
        let p = m.add_param(&s).unwrap();
        (m, p, w, t)
    }

    fn raw_val(m: &Model, p: i64, kind: &str, rf: i64) {
        m.db.execute(
            "INSERT INTO pparam_val(param, kind, ref, value) VALUES (?1,?2,?3,'x')",
            params![p, kind, rf],
        )
        .unwrap();
    }

    #[test]
    fn param_usage_type_and_instance_counted_once() {
        let (m, p, w, t) = usage_fixture();
        raw_val(&m, p, "wall_type", t);
        raw_val(&m, p, "wall", w);
        // two values; the one wall carrying both is a single element
        assert_eq!(m.param_usage(p).unwrap(), (2, 1));
    }

    #[test]
    fn param_usage_type_only() {
        let (m, p, _w, t) = usage_fixture();
        raw_val(&m, p, "wall_type", t);
        assert_eq!(m.param_usage(p).unwrap(), (1, 1));
    }

    #[test]
    fn param_usage_instance_only() {
        let (m, p, w, _t) = usage_fixture();
        raw_val(&m, p, "wall", w);
        assert_eq!(m.param_usage(p).unwrap(), (1, 1));
    }

    #[test]
    fn type_params_resolve_through_type() {
        let (mut m, w, t) = model();
        let mut s = spec("Fire rating", ParamType::Text, &["wall"]);
        s.per_type = true;
        let p = m.add_param(&s).unwrap();
        // an instance for a type parameter is rejected
        assert!(m
            .set_param_values(&[ParamSet {
                param: p,
                kind: "wall".into(),
                id: w,
                value: Some(PVal::Text("EI60".into()))
            }])
            .is_err());
        m.set_param_values(&[ParamSet {
            param: p,
            kind: "wall_type".into(),
            id: t,
            value: Some(PVal::Text("EI60".into())),
        }])
        .unwrap();
        let d = m.param_by_name("Fire rating").unwrap().unwrap();
        assert_eq!(
            m.param_value_of(&d, "wall", w).unwrap(),
            Some(PVal::Text("EI60".into()))
        );
        // doors have no model types
        let mut bad = spec("X", ParamType::Text, &["door"]);
        bad.per_type = true;
        assert!(m.add_param(&bad).is_err());
    }

    #[test]
    fn edit_and_delete_param_undo_exactly() {
        let (mut m, w, _) = model();
        let p = m
            .add_param(&spec("Price", ParamType::Number, &["wall"]))
            .unwrap();
        assert!(m
            .add_param(&spec("Price", ParamType::Text, &["slab"]))
            .is_err());
        m.set_param_values(&[ParamSet {
            param: p,
            kind: "wall".into(),
            id: w,
            value: Some(PVal::Num(7.0)),
        }])
        .unwrap();
        // changing the type while values exist is rejected
        assert!(m
            .edit_param(p, &spec("Price", ParamType::Text, &["wall"]))
            .is_err());
        // removing a category removes the values
        m.edit_param(p, &spec("Price, rub", ParamType::Number, &["slab"]))
            .unwrap();
        assert!(m.param_values(p).unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.param_value(p, "wall", w).unwrap(), Some(PVal::Num(7.0)));
        assert_eq!(m.params().unwrap()[0].spec.name, "Price");
        m.delete_param(p).unwrap();
        assert!(m.params().unwrap().is_empty());
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.param_value(p, "wall", w).unwrap(), Some(PVal::Num(7.0)));
        assert_eq!(
            m.params().unwrap()[0].spec.categories,
            vec!["wall".to_string()]
        );
    }

    fn door(m: &mut Model, w: i64, along: f64, kind: &str) -> i64 {
        m.add_opening(
            w,
            &OpeningSpec {
                along,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: kind.into(),
            },
        )
        .unwrap()
    }

    fn text_val(m: &mut Model, param: i64, kind: &str, id: i64, v: &str) {
        m.set_param_values(&[ParamSet {
            param,
            kind: kind.into(),
            id,
            value: Some(PVal::Text(v.into())),
        }])
        .unwrap();
    }

    #[test]
    fn dropped_values_counts_per_unticked_category_and_undo_restores_all() {
        let (mut m, w, _) = model();
        let p = m
            .add_param(&spec("Mark", ParamType::Text, &["wall", "door", "window"]))
            .unwrap();
        let doors: Vec<i64> = [0.5, 1.5, 2.5].iter().map(|a| door(&mut m, w, *a, "door")).collect();
        let win = door(&mut m, w, 3.8, "window");
        for (i, d) in doors.iter().enumerate() {
            text_val(&mut m, p, "opening", *d, &format!("D{i}"));
        }
        text_val(&mut m, p, "opening", win, "W1");
        text_val(&mut m, p, "wall", w, "A1");
        let cats = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(m.dropped_values(p, &cats(&["wall", "door", "window"])).unwrap().is_empty());
        assert_eq!(
            m.dropped_values(p, &cats(&["wall", "window"])).unwrap(),
            vec![("door".to_string(), 3)]
        );
        assert_eq!(
            m.dropped_values(p, &cats(&["wall"])).unwrap(),
            vec![("door".to_string(), 3), ("window".to_string(), 1)]
        );
        // read-only: nothing went
        assert_eq!(m.param_values(p).unwrap().len(), 5);
        m.edit_param(p, &spec("Mark", ParamType::Text, &["wall", "window"]))
            .unwrap();
        let left = m.param_values(p).unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(left.get(&("opening".to_string(), win)), Some(&PVal::Text("W1".into())));
        assert_eq!(left.get(&("wall".to_string(), w)), Some(&PVal::Text("A1".into())));
        m.undo().unwrap();
        for (i, d) in doors.iter().enumerate() {
            assert_eq!(m.param_value(p, "opening", *d).unwrap(), Some(PVal::Text(format!("D{i}"))));
        }
        assert_eq!(m.param_values(p).unwrap().len(), 5);
    }

    #[test]
    fn dropped_values_skips_a_category_without_values() {
        let (mut m, w, _) = model();
        let p = m.add_param(&spec("Mark", ParamType::Text, &["wall", "door"])).unwrap();
        text_val(&mut m, p, "wall", w, "A1");
        let _ = door(&mut m, w, 1.0, "door");
        assert!(m.dropped_values(p, &["wall".to_string()]).unwrap().is_empty());
    }

    #[test]
    fn shared_file_round_trip() {
        let (mut m, _, _) = model();
        let mut s = spec("Work code", ParamType::Text, &["wall", "slab"]);
        s.guid = Some("0000-1".into());
        m.add_param(&s).unwrap();
        let txt = shared_text(&m.params().unwrap());
        let back = parse_shared(&txt).unwrap();
        assert_eq!(back, vec![s.clone()]);
        // the same guid under another name is rejected
        let mut dup = s;
        dup.name = "Other".into();
        assert!(m.add_param(&dup).is_err());
        assert!(parse_shared("garbage").is_err());
        assert!(new_guid("a").len() == 36);
    }

    #[test]
    fn new_guid_is_a_fixed_algorithm_equal_to_what_older_files_hold() {
        assert_eq!(new_guid("a"), "719b50b9-a4f0-e9f3-677a-a311b799a363");
        assert_eq!(new_guid("viewpoint|x"), "e35fc039-e608-9d32-79bc-6fff46f3cf60");
        // the algorithm the standard library used before it was fixed here
        use std::hash::{Hash, Hasher};
        for seed in ["", "a", "wall|12", "Code:3", "a longer seed that spans several eight byte words"] {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            seed.hash(&mut h);
            let a = h.finish();
            a.rotate_left(17).hash(&mut h);
            let b = h.finish();
            let old = format!(
                "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                a >> 32,
                (a >> 16) & 0xffff,
                a & 0xffff,
                b >> 48,
                b & 0xffff_ffff_ffff
            );
            assert_eq!(new_guid(seed), old, "{seed}");
        }
    }

    #[test]
    fn add_params_is_all_or_nothing_and_guids_are_stable() {
        let (mut m, _, _) = model();
        let a = spec(crate::test_ru::ru("param_code"), ParamType::Text, &["wall"]);
        let mut b = spec(crate::test_ru::ru("param_code_lower"), ParamType::Text, &["slab"]);
        // a case-insensitive name repeat is rejected before writing
        assert!(m.add_params(&[a.clone(), b.clone()]).is_err());
        assert!(m.params().unwrap().is_empty());
        // an error in the second (long name) leaves the first not added
        b.name = "\u{444}".repeat(121);
        assert!(m.add_params(&[a.clone(), b.clone()]).is_err());
        assert!(m.params().unwrap().is_empty());
        // 120 Cyrillic letters are allowed (characters, not bytes)
        b.name = "\u{444}".repeat(120);
        let ids = m.add_params(&[a.clone(), b]).unwrap();
        assert_eq!(ids.len(), 2);
        // one operation: a single undo removes both
        let g1 = m
            .params()
            .unwrap()
            .iter()
            .map(|d| d.spec.guid.clone())
            .collect::<Vec<_>>();
        assert!(g1
            .iter()
            .all(|g| g.as_deref().is_some_and(|g| g.len() == 36)));
        // exporting twice gives the same guids
        assert_eq!(
            shared_text(&m.params().unwrap()),
            shared_text(&m.params().unwrap())
        );
        // an edit with a foreign guid is rejected
        let mut e = m.params().unwrap()[0].spec.clone();
        e.guid = m.params().unwrap()[1].spec.guid.clone();
        assert!(m.edit_param(m.params().unwrap()[0].id, &e).is_err());
        m.undo().unwrap();
        assert!(m.params().unwrap().is_empty());
        // a parameter without a guid (old file) gets one exactly once
        m.add_param(&a).unwrap();
        m.sql().execute("UPDATE pparam SET guid=NULL", []).unwrap();
        assert_eq!(m.ensure_param_guids().unwrap(), 1);
        assert_eq!(m.ensure_param_guids().unwrap(), 0);
    }

    #[test]
    fn xlsx_batch_creates_param_and_values_in_one_undo() {
        let (mut m, w, _) = model();
        let ex = m.phases().unwrap()[0].id;
        let before = m.params().unwrap().len();
        m.apply_edits(&[
            crate::Edit::NewParam(spec("Work code", ParamType::Text, &["wall"])),
            crate::Edit::ParamValue {
                param: "Work code".into(),
                kind: "wall".into(),
                id: w,
                value: Some(PVal::Text("A-1".into())),
            },
            crate::Edit::Phase {
                kind: "wall".into(),
                id: w,
                created: ex,
                demolished: Some(ex),
            },
        ])
        .unwrap();
        let d = m.param_by_name("Work code").unwrap().unwrap();
        assert_eq!(
            m.param_value(d.id, "wall", w).unwrap(),
            Some(PVal::Text("A-1".into()))
        );
        assert_eq!(m.element_phase("wall", w).unwrap().demolished, Some(ex));
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.params().unwrap().len(), before);
        assert_eq!(m.element_phase("wall", w).unwrap().demolished, None);
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM pparam_val", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        // an unknown parameter rolls back the whole batch
        assert!(m
            .apply_edits(&[crate::Edit::ParamValue {
                param: "Missing".into(),
                kind: "wall".into(),
                id: w,
                value: None,
            }])
            .is_err());
    }

    #[test]
    fn parse_values_by_type() {
        assert_eq!(parse_value(ParamType::Int, "12").unwrap(), PVal::Int(12));
        assert!(parse_value(ParamType::Int, "1.5").is_err());
        assert_eq!(
            parse_value(ParamType::YesNo, crate::test_ru::ru("word_yes")).unwrap(),
            PVal::Bool(true)
        );
        assert_eq!(parse_value(ParamType::Length, "1200.5").unwrap(), PVal::Num(1200.5));
        assert_eq!(parse_value(ParamType::Number, "1,5").unwrap(), PVal::Num(1.5));
        // grouping or an ambiguous legacy comma is never read as a number
        for s in ["1 200,5", "1,250", "1,000", "1,250.5", "1\u{a0}250"] {
            assert!(parse_value(ParamType::Number, s).is_err(), "{s}");
        }
        assert!(parse_value(ParamType::Int, "1,000").is_err());
        assert!(parse_value(ParamType::Number, "inf").is_err());
        assert_eq!(PVal::Num(2.50).text(), "2.5");
    }

    #[test]
    fn text_into_number_param_is_invariant_and_refuses_ambiguous() {
        let (mut m, w, _) = model();
        let price = m
            .add_param(&spec("Price", ParamType::Number, &["wall"]))
            .unwrap();
        let set = |v: &str| ParamSet {
            param: price,
            kind: "wall".into(),
            id: w,
            value: Some(PVal::Text(v.into())),
        };
        m.set_param_values(&[set("1.5")]).unwrap();
        assert_eq!(m.param_value(price, "wall", w).unwrap(), Some(PVal::Num(1.5)));
        // legacy `1,000` could be 1 or 1000: refused, the stored 1.5 stays
        for s in ["1,000", "1 250", "1,250.5"] {
            assert!(m.set_param_values(&[set(s)]).is_err(), "{s}");
            assert_eq!(m.param_value(price, "wall", w).unwrap(), Some(PVal::Num(1.5)), "{s}");
        }
        m.set_param_values(&[set("2,5")]).unwrap();
        assert_eq!(m.param_value(price, "wall", w).unwrap(), Some(PVal::Num(2.5)));
        m.assert_integrity();
    }
}
