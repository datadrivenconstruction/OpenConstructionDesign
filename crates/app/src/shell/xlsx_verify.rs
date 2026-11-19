// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! XLSX Apply check: the step just written must do exactly what the accepted preview rows
//! say. The plan is taken from the batch before the write (with the same pinned and locked
//! split the editor makes); after the write the change journal and the model are compared
//! with it. The first difference is named and the caller takes the whole step back.
//!
//! Compared, in this order: rows deleted (and none deleted besides them and the rows they
//! host), rows created per table, updated rows written in the journal and still present,
//! then field values read back (element columns and project parameter values).
//! Not compared: follow-up rows the model's own rules write (joins, hosted and attached
//! elements, quantities) and fields such rules may adjust (wall axis and height, opening
//! position). A parameter value on a type row (`kind` = the type table) is one change of
//! that type row: the elements of the type are never expected in the journal for it.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ddc_io::xlsx::{self, PropEdit};
use ddc_model::{Edit, Model, PVal};
use rusqlite::types::Value;
use rusqlite::OptionalExtension;

/// Value a field must hold after the Apply.
#[derive(Clone, Debug, PartialEq)]
pub enum Want {
    Int(i64),
    Num(f64),
    Text(String),
}

#[derive(Clone, Debug)]
struct Probe {
    table: &'static str,
    /// Key column and value; `pos` narrows a layer row of a type.
    key: &'static str,
    id: i64,
    pos: Option<i64>,
    field: &'static str,
    want: Want,
    old: Option<Value>,
}

#[derive(Clone, Debug)]
struct ParamProbe {
    param: String,
    kind: String,
    id: i64,
    want: Option<PVal>,
    old: Option<PVal>,
}

/// What the accepted batch must do to the model.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    head: u64,
    /// Rows an update statement writes: the journal must show them.
    upd: Vec<(String, i64)>,
    /// Rows changed in a way the journal may not show (props, boundaries): they must stay.
    keep: Vec<(String, i64)>,
    del: Vec<(String, i64)>,
    new: BTreeMap<String, usize>,
    /// Rows a deletion takes with it (openings and components of a deleted host).
    cascade: HashSet<(String, i64)>,
    /// id -> rowid of the watched tables before the write.
    before: HashMap<String, HashMap<i64, i64>>,
    probes: Vec<Probe>,
    params: Vec<ParamProbe>,
}

/// The first way the written step differs from the plan.
#[derive(Clone, Debug, PartialEq)]
pub enum Diff {
    /// The preview changes this row; the journal shows no write to it.
    NotWritten { table: String, id: i64 },
    /// The preview changes this row; it is gone after the Apply.
    Lost { table: String, id: i64 },
    NotDeleted { table: String, id: i64 },
    /// Deleted although the preview keeps it.
    ExtraDeleted { table: String, id: i64 },
    Created { table: String, want: usize, got: usize },
    Value { table: String, id: i64, field: String, old: String, want: String, got: String },
}

fn ident(t: &str) -> bool {
    !t.is_empty() && t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn id_rows(m: &Model, t: &str) -> Result<HashMap<i64, i64>, String> {
    if !ident(t) {
        return Err(format!("bad table name {t}"));
    }
    let mut st = m.sql().prepare(&format!("SELECT id, rowid FROM {t}")).map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<HashMap<_, _>, _>>().map_err(|e| e.to_string())
}

fn ids_where(m: &Model, t: &str, col: &str, id: i64) -> Result<Vec<i64>, String> {
    let mut st = m
        .sql()
        .prepare(&format!("SELECT id FROM {t} WHERE {col}=?1"))
        .map_err(|e| e.to_string())?;
    let rows = st.query_map([id], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn read(m: &Model, p: &Probe) -> Result<Option<Value>, String> {
    let sql = match p.pos {
        Some(_) => format!("SELECT {} FROM {} WHERE {}=?1 AND pos=?2", p.field, p.table, p.key),
        None => format!("SELECT {} FROM {} WHERE {}=?1", p.field, p.table, p.key),
    };
    let r = match p.pos {
        Some(pos) => m.sql().query_row(&sql, [p.id, pos], |r| r.get::<_, Value>(0)),
        None => m.sql().query_row(&sql, [p.id], |r| r.get::<_, Value>(0)),
    };
    r.optional().map_err(|e| e.to_string())
}

fn num_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn holds(want: &Want, got: &Value) -> bool {
    match (want, got) {
        (Want::Int(a), Value::Integer(b)) => a == b,
        (Want::Int(a), Value::Real(b)) => num_eq(*a as f64, *b),
        (Want::Num(a), Value::Real(b)) => num_eq(*a, *b),
        (Want::Num(a), Value::Integer(b)) => num_eq(*a, *b as f64),
        (Want::Text(a), Value::Text(b)) => a == b,
        _ => false,
    }
}

fn show_want(w: &Want) -> String {
    match w {
        Want::Int(i) => i.to_string(),
        Want::Num(x) => x.to_string(),
        Want::Text(s) => format!("'{s}'"),
    }
}

fn show_value(v: Option<&Value>) -> String {
    match v {
        Some(Value::Integer(i)) => i.to_string(),
        Some(Value::Real(x)) => x.to_string(),
        Some(Value::Text(s)) => format!("'{s}'"),
        Some(Value::Blob(b)) => format!("<{} bytes>", b.len()),
        Some(Value::Null) | None => "-".into(),
    }
}

fn show_pval(v: Option<&PVal>) -> String {
    match v {
        Some(PVal::Text(s)) => format!("'{s}'"),
        Some(PVal::Int(i)) => i.to_string(),
        Some(PVal::Num(x)) => x.to_string(),
        Some(PVal::Bool(b)) => b.to_string(),
        None => "-".into(),
    }
}

/// An empty text clears a value; numbers compare by value whatever their kind.
fn pval_eq(a: Option<&PVal>, b: Option<&PVal>) -> bool {
    let blank = |v: Option<&PVal>| match v {
        None => true,
        Some(PVal::Text(s)) => s.trim().is_empty(),
        _ => false,
    };
    match (a, b) {
        _ if blank(a) && blank(b) => true,
        (Some(PVal::Text(x)), Some(PVal::Text(y))) => x.trim() == y.trim(),
        (Some(PVal::Bool(x)), Some(PVal::Bool(y))) => x == y,
        (Some(x), Some(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(p), Some(q)) => num_eq(p, q),
            _ => false,
        },
        _ => false,
    }
}

impl Plan {
    /// The plan of `edits` and `props` against the model as it is now (before the write).
    pub fn of(m: &Model, edits: &[Edit], props: &[PropEdit]) -> Result<Plan, String> {
        // the editor leaves out pinned and locked rows the same way (and says so)
        let (edits, _) = m.split_pinned(edits).map_err(|e| e.to_string())?;
        let (edits, _) = m.split_locked(&edits).map_err(|e| e.to_string())?;
        let (props, _) = xlsx::split_pinned_props(m, props)?;
        let mut p = Plan { head: m.change_head(), ..Default::default() };
        for e in &edits {
            p.note(e);
        }
        for pe in &props {
            let row = (pe.table.to_string(), pe.id);
            if pe.delete {
                p.del.push(row);
            } else {
                p.keep.push(row);
            }
        }
        for (t, id) in p.del.clone() {
            let hosted: &[(&str, &str)] = match t.as_str() {
                "wall" => &[("opening", "wall"), ("comp", "host_wall")],
                "slab" => &[("comp", "host_slab")],
                _ => &[],
            };
            for (ct, col) in hosted {
                // a table or column this model lacks hosts nothing
                for c in ids_where(m, ct, col, id).unwrap_or_default() {
                    p.cascade.insert((ct.to_string(), c));
                }
            }
        }
        let tables: BTreeSet<String> = p
            .upd
            .iter()
            .chain(&p.keep)
            .chain(&p.del)
            .map(|(t, _)| t.clone())
            .chain(p.new.keys().cloned())
            .collect();
        for t in tables {
            p.before.insert(t.clone(), id_rows(m, &t)?);
        }
        for pr in &mut p.probes {
            pr.old = read(m, pr).ok().flatten();
        }
        for pp in &mut p.params {
            if let Ok(Some(d)) = m.param_by_name(pp.param.trim()) {
                pp.old = m.param_value(d.id, &pp.kind, pp.id).ok().flatten();
            }
        }
        Ok(p)
    }

    /// True when the batch changes nothing the check can see.
    pub fn is_empty(&self) -> bool {
        self.upd.is_empty()
            && self.keep.is_empty()
            && self.del.is_empty()
            && self.new.is_empty()
            && self.probes.is_empty()
            && self.params.is_empty()
    }

    fn upd(&mut self, t: &str, id: i64) {
        self.upd.push((t.into(), id));
    }

    fn add(&mut self, t: &str) {
        *self.new.entry(t.into()).or_default() += 1;
    }

    fn probe(&mut self, table: &'static str, id: i64, field: &'static str, want: Want) {
        self.probe_at(table, "id", id, None, field, want);
    }

    fn probe_at(&mut self, table: &'static str, key: &'static str, id: i64, pos: Option<i64>, field: &'static str, want: Want) {
        // a later edit of the same field wins
        self.probes.retain(|p| !(p.table == table && p.id == id && p.pos == pos && p.field == field));
        self.probes.push(Probe { table, key, id, pos, field, want, old: None });
    }

    /// A level id below 0 is a level the same batch makes: its id is not known yet.
    fn probe_level(&mut self, table: &'static str, id: i64, level: i64) {
        if level >= 0 {
            self.probe(table, id, "level", Want::Int(level));
        }
    }

    fn note(&mut self, e: &Edit) {
        match e {
            Edit::Level { id, name, elevation } => {
                self.upd("level", *id);
                self.probe("level", *id, "name", Want::Text(name.clone()));
                self.probe("level", *id, "elevation", Want::Num(*elevation));
            }
            Edit::NewLevel { .. } => self.add("level"),
            Edit::WallType { id, name } => {
                self.upd("wall_type", *id);
                self.probe("wall_type", *id, "name", Want::Text(name.clone()));
            }
            Edit::Layer { wtype, pos, layer } => {
                let (w, p) = (*wtype, Some(*pos));
                self.probe_at("wall_type_layer", "wtype", w, p, "material", Want::Text(layer.material.clone()));
                self.probe_at("wall_type_layer", "wtype", w, p, "thickness", Want::Num(layer.thickness));
            }
            Edit::SlabType { id, name, .. } => {
                self.keep.push(("slab_type".into(), *id));
                self.probe("slab_type", *id, "name", Want::Text(name.clone()));
            }
            Edit::Wall { id, spec, bulge } => {
                self.upd("wall", *id);
                self.probe_level("wall", *id, spec.level);
                self.probe("wall", *id, "wtype", Want::Int(spec.wall_type));
                self.probe("wall", *id, "base_offset", Want::Num(spec.base_offset));
                self.probe("wall", *id, "bulge", Want::Num(*bulge));
            }
            Edit::NewWall { .. } => self.add("wall"),
            Edit::WallShape { id, lean, top_scale } => {
                self.upd("wall", *id);
                self.probe("wall", *id, "lean", Want::Num(*lean));
                self.probe("wall", *id, "top_scale", Want::Num(*top_scale));
            }
            Edit::Delete { table, id } => self.del.push((table.to_string(), *id)),
            Edit::Opening { id, wall, spec } => {
                self.upd("opening", *id);
                self.probe("opening", *id, "wall", Want::Int(*wall));
                self.probe("opening", *id, "width", Want::Num(spec.width));
                self.probe("opening", *id, "height", Want::Num(spec.height));
                self.probe("opening", *id, "kind", Want::Text(spec.kind.clone()));
            }
            Edit::NewOpening { .. } => self.add("opening"),
            Edit::OpeningSwing { id, .. } => self.upd("opening", *id),
            Edit::Room { id, spec } => {
                self.upd("room", *id);
                self.probe_level("room", *id, spec.level);
                self.probe("room", *id, "name", Want::Text(spec.name.clone()));
                self.probe("room", *id, "number", Want::Text(spec.number.clone()));
                self.probe("room", *id, "height", Want::Num(spec.height));
            }
            Edit::NewRoom(_) => self.add("room"),
            Edit::DeleteRoom(id) => self.del.push(("room".into(), *id)),
            Edit::Slab { id, level, slab_type, offset } => {
                self.upd("slab", *id);
                self.probe_level("slab", *id, *level);
                self.probe("slab", *id, "stype", Want::Int(*slab_type));
                self.probe("slab", *id, "offset", Want::Num(*offset));
            }
            Edit::Column { id, .. } => self.upd("col", *id),
            Edit::NewColumn(_) => self.add("col"),
            Edit::DeleteColumn(id) => self.del.push(("col".into(), *id)),
            Edit::Beam { id, .. } => self.upd("beam", *id),
            Edit::NewBeam(_) => self.add("beam"),
            Edit::DeleteBeam(id) => self.del.push(("beam".into(), *id)),
            Edit::Roof { id, offset, overhang, fascia } => {
                self.upd("roof", *id);
                self.probe("roof", *id, "offset", Want::Num(*offset));
                self.probe("roof", *id, "overhang", Want::Num(*overhang));
                self.probe("roof", *id, "fascia", Want::Num(*fascia));
            }
            Edit::SlabBoundary { id, .. } => self.keep.push(("slab".into(), *id)),
            Edit::NewSlab(_) => self.add("slab"),
            Edit::CurtainProps { id, level, name, base_offset } => {
                self.upd("curtain_wall", *id);
                self.probe_level("curtain_wall", *id, *level);
                self.probe("curtain_wall", *id, "name", Want::Text(name.clone()));
                self.probe("curtain_wall", *id, "base_offset", Want::Num(*base_offset));
            }
            Edit::NewCurtain(_) => self.add("curtain_wall"),
            Edit::DeleteCurtain(id) => self.del.push(("curtain_wall".into(), *id)),
            Edit::ParamValue { param, kind, id, value } => {
                self.params.retain(|p| !(p.param == *param && p.kind == *kind && p.id == *id));
                self.params.push(ParamProbe { param: param.clone(), kind: kind.clone(), id: *id, want: value.clone(), old: None });
            }
            // new parameters, materials, marks, values of new rows and phases: no row of their
            // own the preview names; their effect shows in the rows above
            _ => {}
        }
    }

    /// The first difference between the model now and the plan; None when they agree.
    pub fn check(&self, m: &Model) -> Result<Option<Diff>, String> {
        let ch = m.changes_since(self.head);
        // rows the batch deletes, directly or with their host: their edits are not compared
        let dead: HashSet<(&str, i64)> =
            self.del.iter().chain(&self.cascade).map(|(t, id)| (t.as_str(), *id)).collect();
        let mut after: HashMap<&str, HashMap<i64, i64>> = HashMap::new();
        for t in self.before.keys() {
            after.insert(t.as_str(), id_rows(m, t)?);
        }
        for (t, b) in self.before.iter().collect::<BTreeMap<_, _>>() {
            let a = &after[t.as_str()];
            let want_new = self.new.get(t).copied().unwrap_or(0);
            let mut got_new = a.keys().filter(|id| !b.contains_key(*id)).count();
            // a new row may take the id a deletion of the same batch freed
            let mut reuse = want_new.saturating_sub(got_new);
            for (_, id) in self.del.iter().filter(|(dt, _)| dt == t) {
                if b.contains_key(id) && a.contains_key(id) {
                    if reuse > 0 {
                        reuse -= 1;
                        got_new += 1;
                    } else {
                        return Ok(Some(Diff::NotDeleted { table: t.clone(), id: *id }));
                    }
                }
            }
            // a row removed with its host frees its id too (an opening on a deleted wall)
            let mut freed: Vec<i64> =
                self.cascade.iter().filter(|(ct, _)| ct == t).map(|(_, id)| *id).collect();
            freed.sort_unstable();
            for id in freed {
                if reuse > 0 && b.contains_key(&id) && a.contains_key(&id) {
                    reuse -= 1;
                    got_new += 1;
                }
            }
            let mut gone: Vec<i64> = b.keys().filter(|id| !a.contains_key(*id)).copied().collect();
            gone.sort_unstable();
            for id in gone {
                if !dead.contains(&(t.as_str(), id)) && !self.cascade.contains(&(t.clone(), id)) {
                    return Ok(Some(Diff::ExtraDeleted { table: t.clone(), id }));
                }
            }
            if got_new != want_new {
                return Ok(Some(Diff::Created { table: t.clone(), want: want_new, got: got_new }));
            }
        }
        for (t, id) in &self.upd {
            if dead.contains(&(t.as_str(), *id)) {
                continue;
            }
            let (Some(b), Some(a)) = (self.before.get(t), after.get(t.as_str())) else { continue };
            let Some(&rowid) = b.get(id) else { continue };
            if !a.contains_key(id) {
                return Ok(Some(Diff::Lost { table: t.clone(), id: *id }));
            }
            if !ch.whole(t) && !ch.ids(t).any(|r| r == rowid) {
                return Ok(Some(Diff::NotWritten { table: t.clone(), id: *id }));
            }
        }
        for (t, id) in &self.keep {
            if dead.contains(&(t.as_str(), *id)) {
                continue;
            }
            let (Some(b), Some(a)) = (self.before.get(t), after.get(t.as_str())) else { continue };
            if b.contains_key(id) && !a.contains_key(id) {
                return Ok(Some(Diff::Lost { table: t.clone(), id: *id }));
            }
        }
        for p in &self.probes {
            if dead.contains(&(p.table, p.id)) {
                continue;
            }
            let got = read(m, p)?;
            if !got.as_ref().is_some_and(|g| holds(&p.want, g)) {
                return Ok(Some(Diff::Value {
                    table: p.table.into(),
                    id: p.id,
                    field: p.field.into(),
                    old: show_value(p.old.as_ref()),
                    want: show_want(&p.want),
                    got: show_value(got.as_ref()),
                }));
            }
        }
        for p in &self.params {
            if dead.contains(&(p.kind.as_str(), p.id)) {
                continue;
            }
            let got = match m.param_by_name(p.param.trim()).map_err(|e| e.to_string())? {
                Some(d) => m.param_value(d.id, &p.kind, p.id).map_err(|e| e.to_string())?,
                None => None,
            };
            if !pval_eq(p.want.as_ref(), got.as_ref()) {
                return Ok(Some(Diff::Value {
                    table: p.kind.clone(),
                    id: p.id,
                    field: p.param.clone(),
                    old: show_pval(p.old.as_ref()),
                    want: show_pval(p.want.as_ref()),
                    got: show_pval(got.as_ref()),
                }));
            }
        }
        Ok(None)
    }
}

/// Table name in the UI language; tables without a caption are shown as they are.
fn table_label(ru: bool, t: &str) -> String {
    match t {
        "wall" | "opening" | "room" | "slab" | "level" | "wall_type" | "slab_type" | "col" | "beam" | "roof"
        | "curtain_wall" => crate::tr_lang(ru, &format!("xlsx_verify.t.{t}"), &[]),
        _ => t.to_string(),
    }
}

impl Diff {
    /// The difference in the UI language.
    pub fn text(&self, ru: bool) -> String {
        let row = |t: &str, id: i64| format!("{} #{id}", table_label(ru, t));
        let what = match self {
            Diff::NotWritten { table, id } => crate::tr_lang(ru, "xlsx_verify.not_written", &[("row", &row(table, *id))]),
            Diff::Lost { table, id } => crate::tr_lang(ru, "xlsx_verify.lost", &[("row", &row(table, *id))]),
            Diff::NotDeleted { table, id } => crate::tr_lang(ru, "xlsx_verify.not_deleted", &[("row", &row(table, *id))]),
            Diff::ExtraDeleted { table, id } => crate::tr_lang(ru, "xlsx_verify.extra_deleted", &[("row", &row(table, *id))]),
            Diff::Created { table, want, got } => crate::tr_lang(
                ru,
                "xlsx_verify.created",
                &[("table", &table_label(ru, table)), ("want", &want.to_string()), ("got", &got.to_string())],
            ),
            Diff::Value { table, id, field, old, want, got } => crate::tr_lang(
                ru,
                "xlsx_verify.value",
                &[("row", &row(table, *id)), ("field", field), ("old", old), ("want", want), ("got", got)],
            ),
        };
        crate::tr_lang(ru, "xlsx_verify.rolled_back", &[("what", &what)])
    }
}

/// Why a checked write did not go through.
#[derive(Debug, PartialEq)]
pub(crate) enum Refused {
    /// The write's own error, as it returned it (the step may still be in the model).
    Write(String),
    /// Not written, or written and taken back: the step differed from the plan (or the check
    /// could not run). The text is in the UI language.
    Check(String),
}

/// Plans the batch, writes it with `write`, checks it; a difference takes the step back.
pub(crate) fn verified(
    ed: &mut crate::editor::Editor,
    ru: bool,
    edits: Vec<Edit>,
    props: &[PropEdit],
    write: impl FnOnce(&mut crate::editor::Editor, Vec<Edit>) -> Result<(), String>,
) -> Result<(), Refused> {
    let refuse = |e: String| Refused::Check(crate::tr_lang(ru, "xlsx_verify.plan_failed", &[("e", &e)]));
    let plan = Plan::of(&ed.model, &edits, props).map_err(refuse)?;
    let top = ed.model.op_top().map_err(|e| refuse(e.to_string()))?;
    let mut edits = edits;
    tamper(&mut edits);
    let wrote = write(ed, edits).map_err(Refused::Write);
    if wrote.is_err() && ed.model.op_top().ok() == Some(top) {
        return wrote;
    }
    let diff = match plan.check(&ed.model) {
        Ok(None) => return wrote,
        Ok(Some(d)) => d.text(ru),
        // an unreadable model after the write: nothing proves the step right
        Err(e) => {
            let what = crate::tr_lang(ru, "xlsx_verify.check_failed", &[("e", &e)]);
            crate::tr_lang(ru, "xlsx_verify.rolled_back", &[("what", &what)])
        }
    };
    let msg = match ed.take_back_xlsx(top) {
        Ok(()) => diff,
        Err(e) => format!("{diff} {}", crate::tr_lang(ru, "xlsx_verify.take_back_failed", &[("e", &e)])),
    };
    ed.status = Some(msg.clone());
    Err(Refused::Check(msg))
}

#[cfg(test)]
thread_local! {
    static TAMPER: std::cell::RefCell<Option<Box<dyn Fn(&mut Vec<Edit>)>>> = const { std::cell::RefCell::new(None) };
}

/// Test hook: changes the batch between the plan and the write (a stand-in for an apply bug).
fn tamper(edits: &mut Vec<Edit>) {
    #[cfg(test)]
    TAMPER.with(|t| {
        if let Some(f) = t.borrow().as_ref() {
            f(edits);
        }
    });
    #[cfg(not(test))]
    let _ = edits;
}

#[cfg(test)]
pub(crate) fn set_tamper(f: Option<Box<dyn Fn(&mut Vec<Edit>)>>) {
    TAMPER.with(|t| *t.borrow_mut() = f);
}

#[cfg(test)]
#[path = "xlsx_verify_tests.rs"]
mod tests;
