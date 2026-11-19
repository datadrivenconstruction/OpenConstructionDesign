//! Multi-category schedule stored in the project: categories, columns (parameters or formulas),
//! filters, sort, groups and totals. [`build`] lays the categories into one table; formula
//! columns see the columns before them and may aggregate (`sum`, `avg`, `count`, `total`).
//! A cell edit is a [`Draft`]: [`preview`] lists what would change, [`apply`] writes every
//! category as one model op (one undo step). Computed columns are read-only. The XLSX round
//! trip ([`export_xlsx`], [`import_file`]) matches rows by the Id column ("category:id").

use super::ui::preset::{Laid, Preset};
use super::xlsx_in::{self, Plan};
use super::{extra, fmt, formula, header, tl, Cat, Col, Edit, Kind, Row, Table, Val};
use crate::editor::Editor;
use crate::units::Units;
use ddc_io::xlsx::{SheetCell, TableCell as C, TableLine as L};
use serde_json::{json, Value as J};
use std::collections::HashMap;

/// One column of a schedule definition.
#[derive(Clone, Debug, PartialEq)]
pub enum ColSpec {
    /// Column key of the category tables (`len`, `h`, a project parameter key).
    Param(String),
    Formula {
        name: String,
        expr: String,
    },
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct MultiDef {
    pub name: String,
    pub cats: Vec<Cat>,
    pub columns: Vec<ColSpec>,
    /// Column key and filter text of the filter row.
    pub filters: Vec<(String, String)>,
    /// Column key, ascending.
    pub sort: Option<(String, bool)>,
    /// Group column keys, outermost first.
    pub group: Vec<String>,
    /// Group subtotals and the grand total.
    pub totals: bool,
}

fn cat_of(key: &str) -> Option<Cat> {
    Cat::ALL.iter().copied().find(|c| c.key() == key)
}

impl MultiDef {
    pub fn to_json(&self) -> J {
        let cols: Vec<J> = self
            .columns
            .iter()
            .map(|c| match c {
                ColSpec::Param(k) => json!({ "col": k }),
                ColSpec::Formula { name, expr } => json!({ "name": name, "formula": expr }),
            })
            .collect();
        json!({
            "name": self.name,
            "cats": self.cats.iter().map(|c| c.key()).collect::<Vec<_>>(),
            "columns": cols,
            "filters": self.filters.iter().map(|(k, t)| json!([k, t])).collect::<Vec<_>>(),
            "sort": self.sort.as_ref().map(|(k, a)| json!({ "col": k, "asc": a })),
            "group": self.group,
            "totals": self.totals,
        })
    }

    pub fn from_json(j: &J) -> Result<MultiDef, String> {
        let text = |v: &J| v.as_str().map(str::to_string);
        let mut d = MultiDef {
            name: text(&j["name"]).unwrap_or_default(),
            ..Default::default()
        };
        for k in j["cats"].as_array().ok_or("cats")? {
            let k = k.as_str().ok_or("cats")?;
            d.cats
                .push(cat_of(k).ok_or_else(|| format!("unknown category: {k}"))?);
        }
        for c in j["columns"].as_array().ok_or("columns")? {
            d.columns
                .push(match (text(&c["col"]), text(&c["formula"])) {
                    (Some(k), _) => ColSpec::Param(k),
                    (None, Some(expr)) => ColSpec::Formula {
                        name: text(&c["name"]).unwrap_or_default(),
                        expr,
                    },
                    _ => return Err("column".into()),
                });
        }
        for f in j["filters"].as_array().map_or(&[][..], |v| v.as_slice()) {
            match (text(&f[0]), text(&f[1])) {
                (Some(k), Some(t)) => d.filters.push((k, t)),
                _ => return Err("filter".into()),
            }
        }
        if let (Some(k), Some(a)) = (text(&j["sort"]["col"]), j["sort"]["asc"].as_bool()) {
            d.sort = Some((k, a));
        }
        for g in j["group"].as_array().map_or(&[][..], |v| v.as_slice()) {
            d.group.push(text(g).ok_or("group")?);
        }
        d.totals = j["totals"].as_bool().unwrap_or(false);
        Ok(d)
    }
}

pub fn defs_to_text(defs: &[MultiDef]) -> String {
    json!({ "v": 1, "schedules": defs.iter().map(MultiDef::to_json).collect::<Vec<_>>() })
        .to_string()
}

pub fn defs_from_text(t: &str) -> Result<Vec<MultiDef>, String> {
    let j: J = serde_json::from_str(t).map_err(|e| e.to_string())?;
    j["schedules"]
        .as_array()
        .ok_or("schedules")?
        .iter()
        .map(MultiDef::from_json)
        .collect()
}

/// The definitions stored in the project; a damaged text reads as none.
pub fn load(ed: &Editor) -> Vec<MultiDef> {
    let t = ed.model.schedule_defs().ok().flatten();
    t.and_then(|t| defs_from_text(&t).ok()).unwrap_or_default()
}

/// Stores `defs` in the project as one undo step; `Ok(false)`: nothing changed.
pub fn save(ed: &mut Editor, defs: &[MultiDef]) -> Result<bool, String> {
    let t = defs_to_text(defs);
    let top = ed.model.op_top().ok();
    let op = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        m.set_schedule_defs(&t).map(|_| ())
    }));
    ed.run_op(op).map_err(|e| e.to_string())?;
    Ok(ed.model.op_top().ok() != top)
}

/// A column item whose name part is a plausible formula name (no operators or brackets).
fn formula_split(item: &str) -> Option<(&str, &str)> {
    let (n, e) = item.split_once('=')?;
    let bad = |c: char| matches!(c, '<' | '>' | '!' | '(' | ')' | '[' | ']');
    (!n.chars().any(bad) && !e.trim().is_empty()).then_some((n.trim(), e.trim()))
}

/// Columns as typed in the form: items split by `;` or a line break; `key` is a column
/// (project parameter keys such as `p:12` included), `Name = expression` a formula column.
pub fn parse_columns(text: &str) -> Vec<ColSpec> {
    text.split([';', '\n'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| match formula_split(t) {
            Some((n, e)) => ColSpec::Formula {
                name: n.into(),
                expr: e.into(),
            },
            None => ColSpec::Param(t.into()),
        })
        .collect()
}

pub fn columns_text(cols: &[ColSpec]) -> String {
    let one = |c: &ColSpec| match c {
        ColSpec::Param(k) => k.clone(),
        ColSpec::Formula { name, expr } => format!("{name} = {expr}"),
    };
    cols.iter().map(one).collect::<Vec<_>>().join("; ")
}

/// Keys separated by commas.
pub fn parse_keys(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}

/// Filters as typed in the form: `key: filter` items split by `;` or a line break, the filter
/// in the syntax of the filter row of a schedule (`>5`, `<>x`, `!x`, a substring). The key is
/// what precedes the first ": "; without one, what precedes the last ':' (`p:12:>5`).
pub fn parse_filters(text: &str) -> Vec<(String, String)> {
    text.split([';', '\n'])
        .filter_map(|t| t.split_once(": ").or_else(|| split_key(t)))
        .map(|(k, f)| (k.trim().to_string(), f.trim().to_string()))
        .filter(|(k, f)| !k.is_empty() && !f.is_empty())
        .collect()
}

/// `key:filter` without a space: a project parameter key (`p:12`) keeps its own colon.
fn split_key(t: &str) -> Option<(&str, &str)> {
    let from = if t.trim_start().starts_with("p:") {
        t.find(':')? + 1
    } else {
        0
    };
    let at = from + t[from..].find(':')?;
    Some((&t[..at], &t[at + 1..]))
}

pub fn filters_text(f: &[(String, String)]) -> String {
    f.iter()
        .map(|(k, t)| format!("{k}: {t}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `key` ascending or `key desc`.
pub fn parse_sort(text: &str) -> Option<(String, bool)> {
    let t = text.trim();
    let (k, desc) = match t.strip_suffix(" desc") {
        Some(k) => (k.trim(), true),
        None => (t, false),
    };
    (!k.is_empty()).then(|| (k.to_string(), !desc))
}

pub fn sort_text(s: &Option<(String, bool)>) -> String {
    match s {
        Some((k, true)) => k.clone(),
        Some((k, false)) => format!("{k} desc"),
        None => String::new(),
    }
}

/// A schedule laid out over the current model.
pub struct Multi {
    pub def: MultiDef,
    /// All rows of all categories; column 0 is the category.
    pub table: Table,
    /// The category tables, in the order of `def.cats` (duplicates dropped).
    pub parts: Vec<Table>,
    /// Per row of `table`: (category index, row of that category's table).
    pub origin: Vec<(usize, usize)>,
    /// Per category, per column of `table`: the column of the category's table.
    pub colmap: Vec<Vec<Option<usize>>>,
    /// Per category: (element id, panel) -> row of that category's table.
    pub index: Vec<HashMap<(i64, u32), usize>>,
    /// Problems of the definition: unknown columns, bad formulas.
    pub notes: Vec<String>,
}

/// A formula, category or other column that no cell edit can write.
pub fn is_computed(m: &Multi, col: usize) -> bool {
    col == 0
        || m.table
            .cols
            .get(col)
            .is_some_and(|c| c.key.starts_with("f:"))
}

/// Value of `Val` as a number, for aggregates.
fn num(v: &Val) -> Option<f64> {
    match v {
        Val::N(x) if x.is_finite() => Some(*x),
        _ => None,
    }
}

/// `(sum, numeric cells, rows)` per group id and column; group `usize::MAX` is all rows.
type Stats = HashMap<(usize, usize), (f64, usize, usize)>;

/// Only the rows that `inc` lets through count, so group functions agree with the visible
/// subtotals.
fn stats(rows: &[Row], gid: &[usize], inc: &[bool], cols: &[usize]) -> Stats {
    let mut s = Stats::new();
    for ((r, &g), _) in rows.iter().zip(gid).zip(inc).filter(|(_, on)| **on) {
        for &j in cols {
            for k in [g, usize::MAX] {
                let e = s.entry((k, j)).or_default();
                e.2 += 1;
                if let Some(x) = r.vals.get(j).and_then(num) {
                    e.0 += x;
                    e.1 += 1;
                }
            }
        }
    }
    s
}

/// Rows that pass the filters on non-computed columns, judged on the category tables.
fn included(
    parts: &[Table],
    cats: &[Cat],
    origin: &[(usize, usize)],
    def: &MultiDef,
    ru: bool,
    u: Units,
) -> Vec<bool> {
    let mut inc = vec![true; origin.len()];
    for (key, text) in def.filters.iter().filter(|f| !f.0.starts_with("f:")) {
        let kind = parts
            .iter()
            .find_map(|t| t.cols.iter().find(|c| c.key == *key))
            .map(|c| c.kind);
        let Some(kind) = kind.or((key == "cat").then_some(Kind::Text)) else {
            continue;
        };
        for (on, &(p, i)) in inc.iter_mut().zip(origin) {
            let v = if key == "cat" {
                Val::T(cats[p].title_in(ru))
            } else {
                match parts[p].cols.iter().position(|c| c.key == *key) {
                    Some(j) => parts[p].rows[i].vals[j].clone(),
                    None => Val::T(String::new()),
                }
            };
            *on = *on && super::matches(&v, kind, text, u);
        }
    }
    inc
}

pub fn build(ed: &Editor, def: &MultiDef, ru: bool) -> Multi {
    let u = ed.tool.units;
    let mut cats: Vec<Cat> = vec![];
    for &c in &def.cats {
        if !cats.contains(&c) {
            cats.push(c);
        }
    }
    let parts: Vec<Table> = cats.iter().map(|&c| super::build_in(ed, c, ru)).collect();
    let mut cols = vec![super::col(
        "cat",
        tl(ru, "schm.col.cat"),
        Kind::Text,
        Edit::No,
    )];
    let mut rows: Vec<Row> = vec![];
    let mut origin = vec![];
    let mut index: Vec<HashMap<(i64, u32), usize>> = vec![];
    for (p, t) in parts.iter().enumerate() {
        index.push(
            t.rows
                .iter()
                .enumerate()
                .map(|(i, r)| ((r.id, r.sub), i))
                .collect(),
        );
        for (i, r) in t.rows.iter().enumerate() {
            origin.push((p, i));
            rows.push(Row {
                sel: r.sel.clone(),
                id: r.id,
                sub: r.sub,
                vals: vec![Val::T(cats[p].title_in(ru))],
            });
        }
    }
    let inc = included(&parts, &cats, &origin, def, ru, u);
    let mut colmap: Vec<Vec<Option<usize>>> = vec![vec![None]; parts.len()];
    let mut notes = vec![];
    let mut nf = 0;
    let mut used_agg = false;
    let mut gcache: Option<Groups> = None;
    for spec in &def.columns {
        match spec {
            ColSpec::Param(key) if key == "cat" => {}
            ColSpec::Param(key) => {
                let found = parts
                    .iter()
                    .find_map(|t| t.cols.iter().find(|c| c.key == *key));
                let Some(proto) = found else {
                    notes.push(crate::tr_lang(ru, "schm.unknown_col", &[("k", key)]));
                    continue;
                };
                cols.push(proto.clone());
                for (p, t) in parts.iter().enumerate() {
                    colmap[p].push(t.cols.iter().position(|c| c.key == *key));
                }
                for (r, &(p, i)) in rows.iter_mut().zip(&origin) {
                    let at = colmap[p].last().copied().flatten();
                    r.vals.push(
                        at.map_or(Val::T(String::new()), |j| parts[p].rows[i].vals[j].clone()),
                    );
                }
            }
            ColSpec::Formula { name, expr } => {
                let n = nf;
                nf += 1;
                let title = match name.trim() {
                    "" => crate::tr("sch.f.default_name", &[("n", &(n + 1).to_string())]),
                    t => t.to_string(),
                };
                let key = format!("f:{n}");
                let made = formula::parse(expr, &cols).and_then(|e| e.unit(&cols).map(|k| (e, k)));
                for m in &mut colmap {
                    m.push(None);
                }
                match made {
                    Ok((e, kind)) => {
                        used_agg |= !e.aggs().is_empty();
                        let vals = eval_column(&e, &cols, &rows, &inc, &def.group, &mut gcache);
                        for (r, v) in rows.iter_mut().zip(vals) {
                            r.vals.push(v);
                        }
                        cols.push(super::col(key, title, kind, Edit::No));
                    }
                    Err(m) => {
                        notes.push(crate::tr_lang(
                            ru,
                            "schm.bad_formula",
                            &[("n", &title), ("e", &m)],
                        ));
                        for r in rows.iter_mut() {
                            r.vals.push(Val::T(format!("#{m}")));
                        }
                        cols.push(super::col(key, title, Kind::Num, Edit::No));
                    }
                }
            }
        }
    }
    let known = |k: &str| cols.iter().any(|c| c.key == k);
    let used = def
        .filters
        .iter()
        .map(|f| &f.0)
        .chain(&def.group)
        .chain(def.sort.iter().map(|s| &s.0));
    for k in used {
        if !known(k) {
            notes.push(crate::tr_lang(ru, "schm.unknown_col", &[("k", k)]));
        }
    }
    if used_agg && def.filters.iter().any(|f| f.0.starts_with("f:")) {
        notes.push(crate::tr_lang(ru, "schm.filter_after_agg", &[]));
    }
    // two columns under one header cannot be told apart in the book
    let heads: Vec<String> = cols.iter().map(|c| header(c, u).to_lowercase()).collect();
    for (j, h) in heads.iter().enumerate() {
        if heads[..j].contains(h) {
            notes.push(crate::tr_lang(
                ru,
                "schm.dup_header",
                &[("h", &header(&cols[j], u))],
            ));
        }
    }
    let n = rows.len();
    let table = Table {
        cat: cats.first().copied().unwrap_or(Cat::Walls),
        cols,
        rows,
        row_phase: vec![None; n],
        phases: vec![],
    };
    Multi {
        def: def.clone(),
        table,
        parts,
        origin,
        colmap,
        index,
        notes,
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum GKey {
    T(String),
    N(u64),
}

/// Group id per row: rows equal in the group columns share one. Computed once per set of
/// group columns, not once per formula.
struct Groups {
    by: Vec<usize>,
    gid: Vec<usize>,
}

fn groups<'a>(
    cache: &'a mut Option<Groups>,
    cols: &[Col],
    rows: &[Row],
    group: &[String],
) -> &'a [usize] {
    let by: Vec<usize> = group
        .iter()
        .filter_map(|k| cols.iter().position(|c| c.key == *k))
        .collect();
    if cache.as_ref().is_none_or(|g| g.by != by) {
        let mut ids: HashMap<Vec<GKey>, usize> = HashMap::new();
        let gid = rows
            .iter()
            .map(|r| {
                let k = by
                    .iter()
                    .map(|&j| match &r.vals[j] {
                        Val::T(s) => GKey::T(s.clone()),
                        Val::N(x) => GKey::N(x.to_bits()),
                    })
                    .collect();
                let n = ids.len();
                *ids.entry(k).or_insert(n)
            })
            .collect();
        *cache = Some(Groups { by, gid });
    }
    cache.as_ref().map_or(&[], |g| g.gid.as_slice())
}

/// One formula over every row; group functions see the rows in `inc` and the group columns
/// that exist before it.
fn eval_column(
    e: &formula::Expr,
    cols: &[Col],
    rows: &[Row],
    inc: &[bool],
    group: &[String],
    cache: &mut Option<Groups>,
) -> Vec<Val> {
    let aggs = e.aggs();
    if aggs.is_empty() {
        return rows.iter().map(|r| cell_of(e.eval(r))).collect();
    }
    let gid = groups(cache, cols, rows, group);
    let used: Vec<usize> = aggs.iter().map(|a| a.1).collect();
    let st = stats(rows, gid, inc, &used);
    rows.iter()
        .zip(gid)
        .map(|(r, &g)| {
            let look = |f: &str, j: usize| {
                let (sum, n, all) = *st.get(&(if f == "total" { usize::MAX } else { g }, j))?;
                match f {
                    "count" => Some(all as f64),
                    "avg" => (n > 0).then(|| sum / n as f64),
                    _ => Some(sum),
                }
            };
            cell_of(e.eval_agg(r, &look))
        })
        .collect()
}

fn cell_of(r: Result<f64, String>) -> Val {
    match r {
        Ok(x) => Val::N(x),
        Err(m) => Val::T(format!("#{m}")),
    }
}

/// The preset the definition lays the table out by.
pub fn preset(m: &Multi) -> Preset {
    Preset {
        cat: m.table.cat,
        columns: vec![],
        group: m.def.group.clone(),
        sort: m.def.sort.clone(),
        filters: m.def.filters.clone(),
        subtotals: m.def.totals,
    }
}

/// Visible rows, columns and group lines.
pub fn lay(m: &Multi, u: Units) -> Laid {
    preset(m).lay(&m.table, u)
}

/// Totals per column over the visible rows; empty when the definition has no totals.
pub fn totals(m: &Multi, laid: &Laid) -> Vec<Option<f64>> {
    if m.def.totals {
        super::totals(&m.table, &laid.ord)
    } else {
        vec![]
    }
}

// ---------- cell edits as a draft ----------

/// One staged cell. It names the element and the column by key, not by position, so an
/// element deleted or a model undone while the draft is open can never redirect the write.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftCell {
    pub cat: Cat,
    pub id: i64,
    pub sub: u32,
    /// Column key of the category's table.
    pub col: String,
    pub text: String,
}

type CellKey = (Cat, i64, u32, String);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    cells: Vec<DraftCell>,
    at: HashMap<CellKey, usize>,
    /// Cells refused before staging (computed or absent): category, element, column title.
    pub skipped: Vec<(Cat, i64, String)>,
}

impl Draft {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn cells(&self) -> &[DraftCell] {
        &self.cells
    }

    /// Staged text of a cell.
    pub fn text(&self, cat: Cat, id: i64, sub: u32, col: &str) -> Option<&str> {
        let i = *self.at.get(&(cat, id, sub, col.to_string()))?;
        Some(self.cells[i].text.as_str())
    }

    /// Stages a cell; staging it again replaces its text.
    pub fn set(&mut self, c: DraftCell) {
        let k = (c.cat, c.id, c.sub, c.col.clone());
        match self.at.get(&k) {
            Some(&i) => self.cells[i] = c,
            None => {
                self.at.insert(k, self.cells.len());
                self.cells.push(c);
            }
        }
    }

    /// Adds the cells of `o`; a cell staged again keeps its newest text.
    pub fn merge(&mut self, o: Draft) {
        for c in o.cells {
            self.set(c);
        }
        self.skipped.extend(o.skipped);
    }
}

/// Stages `(row, column, text)` cells of the schedule table.
pub fn draft_cells(m: &Multi, cells: &[(usize, usize, String)]) -> Draft {
    let mut d = Draft::default();
    for (r, c, text) in cells {
        let Some(&(p, i)) = m.origin.get(*r) else {
            continue;
        };
        let row = &m.parts[p].rows[i];
        let title = m
            .table
            .cols
            .get(*c)
            .map_or(String::new(), |c| c.title.clone());
        let j = m.colmap[p].get(*c).copied().flatten();
        match j {
            Some(j) if !is_computed(m, *c) => d.set(DraftCell {
                cat: m.parts[p].cat,
                id: row.id,
                sub: row.sub,
                col: m.parts[p].cols[j].key.clone(),
                text: text.clone(),
            }),
            _ => d.skipped.push((m.parts[p].cat, row.id, title)),
        }
    }
    d
}

/// A draft cell found in the current model, as `(row, column)` of its category's table.
type Found = (usize, usize, String);

/// The draft against the current tables: cells per category, and the ones whose element or
/// column is gone.
fn resolve(m: &Multi, d: &Draft) -> (Vec<(usize, Vec<Found>)>, Vec<(Cat, i64, String)>) {
    let mut parts: Vec<(usize, Vec<Found>)> = vec![];
    let mut lost = vec![];
    for c in d.cells() {
        let at = m.parts.iter().position(|t| t.cat == c.cat).and_then(|p| {
            let i = *m.index[p].get(&(c.id, c.sub))?;
            let j = m.parts[p].cols.iter().position(|x| x.key == c.col)?;
            Some((p, i, j))
        });
        match at {
            Some((p, i, j)) => match parts.iter_mut().find(|x| x.0 == p) {
                Some(x) => x.1.push((i, j, c.text.clone())),
                None => parts.push((p, vec![(i, j, c.text.clone())])),
            },
            None => lost.push((c.cat, c.id, c.col.clone())),
        }
    }
    (parts, lost)
}

/// One cell a draft would change.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub cat: Cat,
    pub id: i64,
    pub sub: u32,
    pub column: String,
    pub before: String,
    pub after: String,
    /// Sheet row and column (1-based) of an imported cell.
    pub at: Option<(u32, u32)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preview {
    pub changes: Vec<Change>,
    /// Cells that already hold the value.
    pub unchanged: usize,
    /// One line per refused cell.
    pub refused: Vec<String>,
}

/// What [`apply`] would change; nothing is written.
pub fn preview(ed: &Editor, m: &Multi, d: &Draft, u: Units) -> Preview {
    let mut p = Preview::default();
    let refuse = |p: &mut Preview, cat: Cat, id: i64, col: &str, why: String| {
        p.refused.push(crate::tr(
            "schm.refused",
            &[
                ("cat", &cat.title()),
                ("id", &id.to_string()),
                ("col", col),
                ("why", &why),
            ],
        ));
    };
    for (cat, id, col) in &d.skipped {
        refuse(&mut p, *cat, *id, col, crate::tr("schm.not_editable", &[]));
    }
    let (parts, lost) = resolve(m, d);
    for (cat, id, col) in &lost {
        refuse(&mut p, *cat, *id, col, crate::tr("schm.gone", &[]));
    }
    for (pi, cells) in &parts {
        let tb = &m.parts[*pi];
        for (i, j, text) in cells {
            let row = &tb.rows[*i];
            match extra::check_cell(ed, tb, *i, *j, text) {
                Ok(true) => p.changes.push(Change {
                    cat: tb.cat,
                    id: row.id,
                    sub: row.sub,
                    column: header(&tb.cols[*j], u),
                    before: fmt(&row.vals[*j], tb.cols[*j].kind, u),
                    after: text.clone(),
                    at: None,
                }),
                Ok(false) => p.unchanged += 1,
                Err(s) => {
                    let why = match s {
                        extra::Skip::ReadOnly => crate::tr("sched_import.why_ro", &[]),
                        extra::Skip::Bad => crate::tr("sched_import.why_bad", &[]),
                        extra::Skip::Refused(r) => r.text_ui(),
                    };
                    refuse(&mut p, tb.cat, row.id, &tb.cols[*j].title, why);
                }
            }
        }
    }
    p
}

/// Writes the draft: every category in ONE model op, so one undo step. Cells whose element
/// is gone are skipped; `m` must be built from the current model.
pub fn apply(ed: &mut Editor, m: &Multi, d: &Draft) -> extra::Outcome {
    let (found, lost) = resolve(m, d);
    let parts: Vec<(&Table, Vec<Found>)> =
        found.into_iter().map(|(p, c)| (&m.parts[p], c)).collect();
    let mut o = extra::write_cells_multi(ed, &parts);
    o.bad += lost.len();
    o
}

// ---------- XLSX ----------

fn id_text(cat: Cat, r: &Row) -> String {
    match xlsx_in::id_cell(cat, r.id, r.sub) {
        C::Num(x) => format!("{}:{}", cat.key(), x as i64),
        C::Text(t) => format!("{}:{t}", cat.key()),
        _ => String::new(),
    }
}

/// Sheet name of a schedule: spreadsheet limits, forbidden characters, no apostrophe at either end
/// and not the reserved name "History".
pub fn sheet_name(def: &MultiDef) -> String {
    let s: String = def
        .name
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\'))
        .take(31)
        .collect();
    let s = s.trim().trim_matches('\'').trim().to_string();
    match s.as_str() {
        "" => "Schedule".into(),
        h if h.eq_ignore_ascii_case("history") => "Schedule history".into(),
        _ => s,
    }
}

/// Writes the visible rows to `path`: leading Id column ("category:id"), the shown columns,
/// group lines and totals, and the key sheet with the project uid and every exported cell.
pub fn export_xlsx(
    model: &ddc_model::Model,
    m: &Multi,
    laid: &Laid,
    u: Units,
    path: &std::path::Path,
) -> Result<(), String> {
    let tb = &m.table;
    let mut headers = vec![xlsx_in::ID_HEADER.to_string()];
    headers.extend(laid.cols.iter().map(|&j| header(&tb.cols[j], u)));
    let (ad, vd) = super::precision();
    let formats: Vec<String> = std::iter::once("0".to_string())
        .chain(laid.cols.iter().map(|&j| match tb.cols[j].kind {
            Kind::Int => "0".into(),
            Kind::Len if u == Units::Metric => "0".into(),
            Kind::Len => "0.000".into(),
            Kind::Area if ad > 0 => format!("0.{}", "0".repeat(ad)),
            Kind::Vol if vd > 0 => format!("0.{}", "0".repeat(vd)),
            Kind::Pct => "0.0".into(),
            _ => String::new(),
        }))
        .collect();
    let formats: Vec<&str> = formats.iter().map(String::as_str).collect();
    let cell = |x: f64, k: Kind| C::Num(super::export_num(x, k, u));
    let data: Vec<Vec<C>> = laid
        .ord
        .iter()
        .map(|&i| {
            let (p, _) = m.origin[i as usize];
            let r = &tb.rows[i as usize];
            let mut row = vec![C::Text(id_text(m.parts[p].cat, r))];
            for &j in &laid.cols {
                row.push(match &r.vals[j] {
                    Val::N(x) => cell(*x, tb.cols[j].kind),
                    Val::T(s) => C::Text(s.clone()),
                });
            }
            row
        })
        .collect();
    let sum_cells = |tot: &[Option<f64>], label: C| -> Vec<C> {
        let mut v = vec![C::Text(String::new())];
        for (p, &j) in laid.cols.iter().enumerate() {
            v.push(match tot.get(j).copied().flatten() {
                Some(x) if x.is_nan() => C::Text("#".into()),
                Some(x) => C::Subtotal(109, super::export_num(x, tb.cols[j].kind, u)),
                None if p == 0 => label.clone(),
                None => C::Text(String::new()),
            });
        }
        v
    };
    let tot = totals(m, laid);
    let trow = m.def.totals.then(|| {
        let n = laid.ord.len().to_string();
        sum_cells(&tot, C::Text(crate::tr("sch.total", &[("n", &n)])))
    });
    let mut lines: Vec<L> = vec![];
    for l in &laid.lines {
        lines.push(match l {
            super::group::Line::Row(pos) => L::Row(*pos),
            super::group::Line::Head { depth, text, n } => L::Head {
                text: format!("{}{text} \u{b7} {n}", "  ".repeat(*depth as usize)),
            },
            super::group::Line::Sub { depth, tot, n } => {
                let lab = crate::tr("sch.group_total", &[("n", &n.to_string())]);
                let lab = C::Text(format!("{}{lab}", "  ".repeat(*depth as usize)));
                L::Sub {
                    cells: sum_cells(tot, lab),
                }
            }
        });
    }
    if lines.is_empty() {
        lines = (0..data.len()).map(L::Row).collect();
    }
    let key = xlsx_in::key_grid(model, &headers, &data);
    ddc_io::xlsx::export_keyed_table_lines_with_key(
        path,
        &sheet_name(&m.def),
        &headers,
        &formats,
        &data,
        &lines,
        trow.as_deref(),
        &key,
    )
}

/// What an edited book changes.
#[derive(Clone, Debug, Default)]
pub struct Imported {
    pub draft: Draft,
    pub preview: Preview,
    /// Book rows matched to elements.
    pub rows: usize,
    /// Rows with an Id of a category the schedule does not hold, or a deleted element.
    pub unknown: usize,
    /// Book columns that match no column of the schedule (not read).
    pub ignored: Vec<String>,
    /// Headers shared by several book columns (not read).
    pub ambiguous: Vec<String>,
}

fn is_blank_cell(c: &SheetCell) -> bool {
    matches!(c, SheetCell::Empty) || matches!(c, SheetCell::Text(s) if s.trim().is_empty())
}

fn is_id_head(c: &SheetCell) -> bool {
    matches!(c, SheetCell::Text(s) if s.trim().eq_ignore_ascii_case(xlsx_in::ID_HEADER))
}

/// Reads the schedule sheet of an exported book; nothing is written.
pub fn import_file(
    ed: &Editor,
    m: &Multi,
    u: Units,
    path: &std::path::Path,
) -> Result<Imported, String> {
    use crate::shell::xlsx_project::{origin_of, Origin};
    if let Some(k) = xlsx_in::Key::read(path) {
        if origin_of(&ed.model, k.project) == Origin::Other {
            return Err(crate::tr("xlsx_project.title", &[]));
        }
    }
    let (name, grid) = ddc_io::xlsx::read_sheet_cells(path, Some(&sheet_name(&m.def)))?;
    import_grid(ed, m, u, &name, &grid)
}

/// [`import_file`] on the cells of the sheet `sheet`: rows are matched by the Id column,
/// columns by header, and the difference to the model is the draft.
pub fn import_grid(
    ed: &Editor,
    m: &Multi,
    u: Units,
    sheet: &str,
    grid: &[Vec<SheetCell>],
) -> Result<Imported, String> {
    let hr = grid
        .iter()
        .take(30)
        .position(|r| r.iter().any(is_id_head))
        .ok_or_else(|| crate::tr("sched_import.no_id_col", &[]))?;
    let idc = grid[hr].iter().position(is_id_head).unwrap_or(0);
    let shown = |c: &SheetCell| match c {
        SheetCell::Text(s) => s.trim().to_lowercase(),
        SheetCell::Num(x) => format!("{x}"),
        _ => String::new(),
    };
    let mut by_head: HashMap<String, Vec<usize>> = HashMap::new();
    for (j, c) in grid[hr]
        .iter()
        .enumerate()
        .filter(|(j, c)| *j != idc && !shown(c).is_empty())
    {
        by_head.entry(shown(c)).or_default().push(j);
    }
    let mut out = Imported::default();
    // a header that two book columns share cannot say which one is meant: neither is read
    let mut labels: HashMap<String, usize> = HashMap::new();
    let mut heads_sorted: Vec<_> = by_head.into_iter().collect();
    heads_sorted.sort();
    for (h, js) in heads_sorted {
        if js.len() == 1 {
            labels.insert(h, js[0]);
        } else {
            out.preview
                .refused
                .push(crate::tr("schm.ambiguous_header", &[("h", &h)]));
            out.ambiguous.push(h);
        }
    }
    let mut used = vec![false; grid[hr].len()];
    // per category: (header, book column) per mapped table column, the rows, book row numbers
    struct Part {
        heads: Vec<(usize, usize)>,
        rows: Vec<Vec<SheetCell>>,
        src: Vec<u32>,
    }
    let mut ps: Vec<Part> = vec![];
    for p in 0..m.parts.len() {
        let mut heads = vec![];
        for j in 0..m.table.cols.len() {
            let (Some(pj), false) = (m.colmap[p][j], is_computed(m, j)) else {
                continue;
            };
            if let Some(&bj) = labels.get(&header(&m.table.cols[j], u).to_lowercase()) {
                heads.push((pj, bj));
                used[bj] = true;
            }
        }
        ps.push(Part {
            heads,
            rows: vec![],
            src: vec![],
        });
    }
    for (ri, r) in grid.iter().enumerate().skip(hr + 1) {
        let Some(SheetCell::Text(id)) = r.get(idc) else {
            continue;
        };
        let Some((k, rest)) = id.trim().split_once(':') else {
            continue;
        };
        // a group heading or total line writes text here, never "category:number"
        let is_id = |t: &str| {
            let (a, b) = t.split_once('/').unwrap_or((t, "0"));
            a.trim().parse::<i64>().is_ok() && b.trim().parse::<u32>().is_ok()
        };
        let typed = r
            .iter()
            .enumerate()
            .any(|(j, c)| j != idc && !is_blank_cell(c));
        if !is_id(rest) || !typed {
            continue;
        }
        let Some(p) = cat_of(k).and_then(|c| m.parts.iter().position(|t| t.cat == c)) else {
            out.unknown += 1;
            continue;
        };
        let idc_cell = match rest.trim().parse::<i64>() {
            Ok(v) => SheetCell::Num(v as f64),
            Err(_) => SheetCell::Text(rest.trim().to_string()),
        };
        let mut row = vec![idc_cell];
        row.extend(
            ps[p]
                .heads
                .iter()
                .map(|&(_, bj)| r.get(bj).cloned().unwrap_or(SheetCell::Empty)),
        );
        ps[p].rows.push(row);
        ps[p].src.push(ri as u32 + 1);
    }
    out.ignored = grid[hr]
        .iter()
        .enumerate()
        .filter(|(j, c)| *j != idc && !used[*j] && !shown(c).is_empty())
        .filter(|(_, c)| !out.ambiguous.contains(&shown(c)))
        .filter(|(_, c)| {
            !m.table
                .cols
                .iter()
                .any(|t| header(t, u).to_lowercase() == shown(c))
        })
        .map(|(_, c)| shown(c))
        .collect();
    for (p, part) in ps.into_iter().enumerate() {
        if part.rows.is_empty() {
            continue;
        }
        let tb = &m.parts[p];
        let mut g = vec![std::iter::once(SheetCell::Text(xlsx_in::ID_HEADER.into()))
            .chain(
                part.heads
                    .iter()
                    .map(|&(pj, _)| SheetCell::Text(header(&tb.cols[pj], u))),
            )
            .collect::<Vec<_>>()];
        g.extend(part.rows);
        let plan: Plan = xlsx_in::plan_keyed(ed, tb, u, sheet, &g, None)?;
        out.rows += plan.rows;
        out.unknown += plan.unknown_ids;
        out.preview.unchanged += plan.unchanged;
        // back to the coordinates of the book
        let book_row = |r: u32| {
            part.src
                .get((r as usize).wrapping_sub(2))
                .copied()
                .unwrap_or(r)
        };
        let book_col = |c: u32| match (c as usize).checked_sub(2) {
            Some(k) => part.heads.get(k).map_or(c, |h| h.1 as u32 + 1),
            None => idc as u32 + 1,
        };
        for c in &plan.changes {
            out.preview.changes.push(Change {
                cat: tb.cat,
                id: c
                    .element
                    .split('/')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                sub: c
                    .element
                    .split('/')
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                column: c.column.clone(),
                before: c.before.clone(),
                after: c.after.clone(),
                at: Some((book_row(c.row), book_col(c.col))),
            });
        }
        for r in &plan.refusals {
            let mut r = r.clone();
            r.row = book_row(r.row);
            r.col = book_col(r.col);
            out.preview.refused.push(r.line(sheet));
        }
        for (i, j, text) in plan.cells {
            let row = &tb.rows[i];
            out.draft.set(DraftCell {
                cat: tb.cat,
                id: row.id,
                sub: row.sub,
                col: tb.cols[j].key.clone(),
                text,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
