//! Generic sheets for element families: one sheet per family, one column per editable row of the
//! caller's element table. Export writes the values; import reports every changed cell and every
//! bad row, and returns the edits as `PropEdit`s that the caller applies as one undo step.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use calamine::{open_workbook, Data, Range, Reader, Xlsx};
use ddc_model::{Error, Model};
use rust_xlsxwriter::{Color, Format, Workbook};

use super::baseline::BVal;
use super::units::{self, Dim};
use super::*;

pub type Get = fn(&Model, i64) -> Result<f64, Error>;
pub type SetFn = fn(&mut Model, i64, f64) -> Result<(), Error>;

/// What a column holds; the model value is metres, radians, a type id or a choice index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FamDim {
    /// Metres in the model, book units in the sheet.
    Len,
    /// Radians in the model, degrees in the sheet.
    Deg,
    /// Percent, the same number in the sheet.
    Pct,
    /// Row id in this type table.
    Type(&'static str),
    /// Index below this count.
    Choice(usize),
}

#[derive(Clone)]
pub struct FamilyCol {
    pub key: &'static str,
    /// English label; the column header is derived from it and must stay stable.
    pub label: String,
    pub dim: FamDim,
    /// English option labels of a `Choice` column by index; empty when only the number is known.
    pub labels: Vec<String>,
    /// Valid range of a length (metres) or an angle (degrees).
    pub lo: f64,
    pub hi: f64,
    pub get: Get,
    pub set: SetFn,
}

impl FamilyCol {
    pub fn header(&self, u: Units) -> String {
        match self.dim {
            FamDim::Len => u.header(&format!("{} [m]", self.label)),
            FamDim::Deg => format!("{} [deg]", self.label),
            FamDim::Pct => format!("{} [%]", self.label),
            FamDim::Type(_) => format!("{} (type id)", self.label),
            FamDim::Choice(n) => format!("{} (choice 0-{})", self.label, n.saturating_sub(1)),
        }
    }
}

/// Whether a sheet row may remove its element (flag column or row removed from the book).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RowDelete {
    /// The model deletes the element through its own delete path, as one undo step.
    Allowed,
    /// English message (a report text) saying why the element can only be deleted in the editor.
    Refused(&'static str),
}

#[derive(Clone)]
pub struct FamilySheet {
    pub sheet: &'static str,
    /// Model table whose `id` column keys the rows.
    pub table: &'static str,
    pub cols: Vec<FamilyCol>,
    pub delete: RowDelete,
}

/// Header of the flag column that deletes a row's element.
pub const DELETE_HEAD: &str = "delete";

/// Normalised headers the sheet writes itself; any other column belongs to the user.
pub(crate) fn known_heads(fam: &FamilySheet) -> HashSet<String> {
    let mut out: HashSet<String> = ["id"].iter().map(|h| units::norm(h)).collect();
    out.extend(READ_ONLY.iter().map(|h| units::norm(h)));
    out.insert(units::norm(DELETE_HEAD));
    out.extend(fam.cols.iter().map(|c| units::norm(&c.header(Units::Metric))));
    out
}

/// One accepted cell edit; `set` is the row setter of the element table.
#[derive(Clone)]
pub struct PropEdit {
    pub sheet: String,
    pub table: &'static str,
    pub id: i64,
    pub key: &'static str,
    pub value: f64,
    pub set: SetFn,
    /// Remove the element instead of setting a value (`key` is `DELETE_HEAD`).
    pub delete: bool,
}

impl std::fmt::Debug for PropEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PropEdit({} {} {} = {})",
            self.table, self.id, self.key, self.value
        )
    }
}

fn no_set(_: &mut Model, _: i64, _: f64) -> Result<(), Error> {
    Ok(())
}

/// Same text as the main sheets' pinned skip.
fn pinned_reason(table: &str, id: i64) -> String {
    format!("{table} {id} is pinned: skipped")
}

/// A pinned element (or one hosting a pinned element) refuses a delete from the book.
fn pin_blocks(model: &Model, table: &'static str, id: i64) -> Result<bool, String> {
    let del = [ddc_model::Edit::Delete { table, id }];
    model.split_pinned(&del).map(|(_, p)| !p.is_empty()).map_err(|e| e.to_string())
}

/// The props without those that would move or delete a pinned element: (the rest, skipped
/// (table, id)). Backstop for the apply; the import already leaves them out with a reason.
pub fn split_pinned_props(model: &Model, props: &[PropEdit]) -> Result<(Vec<PropEdit>, Vec<(String, i64)>), String> {
    let (mut keep, mut skipped) = (vec![], vec![]);
    for p in props {
        let blocked = if p.delete {
            pin_blocks(model, p.table, p.id)?
        } else {
            model.is_pinned(p.table, p.id).map_err(|e| e.to_string())?
        };
        if !blocked {
            keep.push(p.clone());
            continue;
        }
        let it = (p.table.to_string(), p.id);
        if !skipped.contains(&it) {
            skipped.push(it);
        }
    }
    Ok((keep, skipped))
}

fn delete_edit(fam: &FamilySheet, id: i64) -> PropEdit {
    PropEdit {
        sheet: fam.sheet.into(),
        table: fam.table,
        id,
        key: DELETE_HEAD,
        value: 1.0,
        set: no_set,
        delete: true,
    }
}

/// Delete flag cell: empty or a negative word is no flag.
fn flag(d: &Data) -> Result<bool, String> {
    match units::delete_word(d) {
        units::DelWord::Yes => Ok(true),
        units::DelWord::No => Ok(false),
        units::DelWord::Unknown(other) => {
            Err(super::msg::fill_template(super::msg::DELETE_UNKNOWN.1, &[other.as_str()]))
        }
    }
}

const READ_ONLY: [&str; 3] = [
    "level (read-only)",
    "name (read-only)",
    "kind (read-only)",
];

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * (1.0 + a.abs().max(b.abs()))
}

fn shown(v: f64) -> String {
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

struct Base {
    has: [bool; 3],
    rows: Vec<(i64, [String; 3])>,
}

fn base_rows(model: &Model, table: &str) -> Result<Base, String> {
    let db = model.sql();
    let mut st = db
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(err)?;
    let names: HashSet<String> = st
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(err)?
        .filter_map(Result::ok)
        .collect();
    let has = [
        names.contains("level"),
        names.contains("name"),
        names.contains("kind"),
    ];
    let pick = |on: bool, e: &str| if on { e.to_string() } else { "NULL".to_string() };
    let sql = format!(
        "SELECT t.id,{},{},{} FROM \"{table}\" t {} ORDER BY t.id",
        pick(has[0], "lv.name"),
        pick(has[1], "CAST(t.name AS TEXT)"),
        pick(has[2], "CAST(t.kind AS TEXT)"),
        if has[0] {
            "LEFT JOIN level lv ON lv.id=t.level"
        } else {
            ""
        }
    );
    let mut st = db.prepare(&sql).map_err(err)?;
    let rows = st
        .query_map([], |r| {
            let t = |i: usize| r.get::<_, Option<String>>(i).map(Option::unwrap_or_default);
            Ok((r.get::<_, i64>(0)?, [t(1)?, t(2)?, t(3)?]))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(Base { has, rows })
}

fn book_units(wb: &mut Xlsx<std::io::BufReader<std::fs::File>>) -> Units {
    let t = wb
        .worksheet_range(INFO)
        .ok()
        .and_then(|r| r.get_value((0, 1)).map(|d| d.to_string().to_ascii_lowercase()));
    match t {
        Some(t) if t.trim() == "imperial" => Units::Imperial,
        _ => Units::Metric,
    }
}

/// Options a `Type` or `Choice` cell can name: (value, label), blank labels left out.
fn options(model: &Model, c: &FamilyCol) -> Vec<(i64, String)> {
    let out: Vec<(i64, String)> = match c.dim {
        FamDim::Type(t) => model
            .sql()
            .prepare(&format!("SELECT id,CAST(name AS TEXT) FROM \"{t}\" ORDER BY id"))
            .and_then(|mut st| {
                let rows = st.query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
                })?;
                let v: Vec<(i64, String)> = rows.flatten().collect();
                Ok(v)
            })
            .unwrap_or_default(),
        FamDim::Choice(n) => c
            .labels
            .iter()
            .take(n)
            .enumerate()
            .map(|(i, s)| (i as i64, s.clone()))
            .collect(),
        _ => vec![],
    };
    out.into_iter().filter(|(_, s)| !s.trim().is_empty()).collect()
}

fn same_name(a: &str, b: &str) -> bool {
    units::same_text(&a.to_lowercase(), &b.to_lowercase())
}

/// The name a cell shows for value `v`; None when it has none or the name is shared.
fn label_in(opts: &[(i64, String)], v: f64) -> Option<String> {
    if v.fract() != 0.0 {
        return None;
    }
    let (_, s) = opts.iter().find(|(i, _)| *i == v as i64)?;
    (opts.iter().filter(|(_, o)| same_name(o, s)).count() == 1).then(|| s.trim().to_string())
}

/// Cell text -> value by name: None when no option has that name.
fn by_name(opts: &[(i64, String)], c: &FamilyCol, s: &str) -> Option<Result<f64, String>> {
    let hit: Vec<i64> = opts
        .iter()
        .filter(|(_, o)| same_name(o, s))
        .map(|x| x.0)
        .collect();
    match hit.as_slice() {
        [] => None,
        [one] => Some(Ok(*one as f64)),
        many => {
            let ids: Vec<String> = many.iter().map(|i| i.to_string()).collect();
            let (_, tpl) = if matches!(c.dim, FamDim::Type(_)) {
                super::msg::FAM_TYPE_AMBIGUOUS
            } else {
                super::msg::FAM_CHOICE_AMBIGUOUS
            };
            Some(Err(super::msg::fill_template(tpl, &[s.trim(), &ids.join(", ")])))
        }
    }
}

/// Cell text for the report: the option name when there is one, else the number.
fn shown_in(opts: &[(i64, String)], c: &FamilyCol, v: f64, k: f64) -> String {
    label_in(opts, v).unwrap_or_else(|| shown(to_cell(c, v, k)))
}

fn to_cell(c: &FamilyCol, v: f64, k: f64) -> f64 {
    match c.dim {
        FamDim::Len => v / k,
        FamDim::Deg => v.to_degrees(),
        _ => v,
    }
}

/// Writes the sheet of one family; every element of the table gets a row.
pub(crate) fn write_sheet(
    wb: &mut Workbook,
    model: &Model,
    fam: &FamilySheet,
    u: Units,
    kept: &super::extra::Kept,
) -> Result<(), String> {
    super::export::gate_sheet()?;
    super::cellsafe::enter(fam.sheet);
    let base = base_rows(model, fam.table)?;
    let ws = wb.add_worksheet();
    ws.set_name(fam.sheet).map_err(err)?;
    // rows the user keeps above the header stay there; the table starts below them
    let top = super::extra::write_titles(ws, &super::extra::titles_of(fam.sheet)).map_err(err)?;
    let grey = Format::new()
        .set_font_color(Color::RGB(0x666666))
        .set_background_color(Color::RGB(0xEEEEEE));
    let head_ro = grey.clone().set_bold();
    let head_rw = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xDDEBF7));
    let mut heads: Vec<(String, bool)> = vec![("id".into(), false)];
    heads.extend(
        READ_ONLY
            .iter()
            .zip(base.has)
            .filter(|x| x.1)
            .map(|x| (x.0.to_string(), false)),
    );
    let fixed = heads.len();
    heads.extend(fam.cols.iter().map(|c| (c.header(u), true)));
    if fam.delete == RowDelete::Allowed {
        heads.push((DELETE_HEAD.into(), true));
    }
    // user columns of the previous book follow the row by its id
    let ids: Vec<i64> = base.rows.iter().map(|r| r.0).collect();
    let mut taken = known_heads(fam);
    // project parameters of the table's category: book cells (conflicts kept) and baseline
    let mut params = super::extra::param_cols(model, fam.sheet, &ids, u, &mut taken)?;
    let mut params_base = params.clone();
    if super::baseline::kept_on(fam.sheet) {
        let mut id_rows: Vec<Vec<super::export::V>> =
            ids.iter().map(|i| vec![super::export::V::N(*i as f64)]).collect();
        super::baseline::overlay(fam.sheet, &[], &mut id_rows, &mut params, true);
        super::baseline::overlay(fam.sheet, &[], &mut id_rows, &mut params_base, false);
    }
    let at_params = heads.len();
    heads.extend(params.iter().map(|x| (x.header.clone(), x.edit)));
    let users = kept.user_cols_at(fam.sheet, &ids, &mut taken, false);
    let own = heads.len();
    heads.extend(users.iter().map(|x| (x.header.clone(), true)));
    for (j, (h, edit)) in heads.iter().enumerate() {
        let f = if *edit { &head_rw } else { &head_ro };
        ws.write_string_with_format(top, j as u16, h, f).map_err(err)?;
        ws.set_column_width(j as u16, (h.chars().count() as f64 + 4.0).max(12.0))
            .map_err(err)?;
    }
    let k = u.factor("[m]");
    let opts: Vec<Vec<(i64, String)>> = fam.cols.iter().map(|c| options(model, c)).collect();
    let name_fmt = Format::new().set_unlocked().set_num_format_index(49);
    for (i, (id, text)) in base.rows.iter().enumerate() {
        super::export::gate_rows(i, base.rows.len())?;
        let r = top + i as u32 + 1;
        ws.write_number_with_format(r, 0, *id as f64, &grey)
            .map_err(err)?;
        let mut j = 1u16;
        for (on, s) in base.has.iter().zip(text) {
            if *on {
                let fits = super::cellsafe::clipped(s).is_none();
                let cut = if fits {
                    None
                } else {
                    let v = super::export::V::S(s.clone());
                    super::cellsafe::write_safe(ws, r, j, &v, Some(*id))
                };
                match cut {
                    Some(done) => done.map_err(err)?,
                    None => ws.write_string_with_format(r, j, s, &grey).map(|_| ()).map_err(err)?,
                }
                j += 1;
            }
        }
        for (n, c) in fam.cols.iter().enumerate() {
            let col = (fixed + n) as u16;
            let kept = super::baseline::kept_cell(fam.sheet, *id, c.key);
            let v = match (kept, (c.get)(model, *id)) {
                (Some((BVal::N(b), _)), Ok(_)) => Some(b),
                (_, Ok(v)) => Some(v),
                (_, Err(_)) => None,
            };
            let Some(v) = v else { continue };
            match label_in(&opts[n], v) {
                // Text format: a type named `007` is not retyped into the number 7
                Some(name) => ws.write_string_with_format(r, col, name, &name_fmt).map(|_| ()),
                None => ws.write_number(r, col, to_cell(c, v, k)).map(|_| ()),
            }
            .map_err(err)?;
        }
    }
    for (n, c) in fam.cols.iter().enumerate() {
        let names: Vec<String> = opts[n].iter().map(|x| x.1.trim().to_string()).collect();
        let list = format!("{}.{}", fam.sheet, c.key);
        super::book::list_validation(ws, &list, &names, (fixed + n) as u16, base.rows.len(), top)?;
    }
    // text parameters take the Text number format: a typed 007 stays text in the book
    let text_fmt = Format::new().set_unlocked().set_num_format_index(49);
    for (n, x) in params.iter().enumerate() {
        let col = (at_params + n) as u16;
        use super::export::V;
        let text = x.edit
            && x.cells.iter().any(|v| matches!(v, V::S(_)))
            && x.cells.iter().all(|v| matches!(v, V::S(_) | V::Blank));
        let fmt = text.then_some(&text_fmt);
        for (i, v) in x.cells.iter().enumerate() {
            super::extra::write_v(ws, top + i as u32 + 1, col, v, fmt).map_err(err)?;
        }
    }
    for (n, x) in users.iter().enumerate() {
        let col = (own + n) as u16;
        for (i, v) in x.cells.iter().enumerate() {
            let r = top + i as u32 + 1;
            super::extra::write_v(ws, r, col, v, None).map_err(err)?;
        }
    }
    ws.set_freeze_panes(top + 1, 0).map_err(err)?;
    ws.autofilter(top, 0, top + base.rows.len() as u32, heads.len() as u16 - 1)
        .map_err(err)?;
    let rows = base
        .rows
        .iter()
        .enumerate()
        .map(|(i, (id, _))| {
            let mut v: Vec<super::export::V> = fam
                .cols
                .iter()
                .map(|c| match super::baseline::kept_cell(fam.sheet, *id, c.key) {
                    Some((_, BVal::N(b))) => super::export::V::N(b),
                    _ => (c.get)(model, *id).map_or(super::export::V::Blank, super::export::V::N),
                })
                .collect();
            v.extend(
                params_base
                    .iter()
                    .map(|x| x.cells.get(i).cloned().unwrap_or(super::export::V::Blank)),
            );
            // the name each type or choice cell showed: an untouched cell stays untouched
            // after the type is renamed or deleted in the editor
            for (n, c) in fam.cols.iter().enumerate() {
                if named(c) {
                    let x = match v.get(n) {
                        Some(super::export::V::N(x)) => Some(*x),
                        _ => None,
                    };
                    v.push(
                        x.and_then(|x| label_in(&opts[n], x))
                            .map_or(super::export::V::Blank, super::export::V::S),
                    );
                }
            }
            (*id, v)
        })
        .collect();
    let mut keys: Vec<String> = fam.cols.iter().map(|c| c.key.to_string()).collect();
    keys.extend(params_base.iter().map(|x| super::baseline::extra_key(&x.header)));
    keys.extend(fam.cols.iter().filter(|c| named(c)).map(name_key));
    super::baseline::record_raw(fam.sheet, keys, rows);
    Ok(())
}

fn named(c: &FamilyCol) -> bool {
    matches!(c.dim, FamDim::Type(_) | FamDim::Choice(_))
}

/// Baseline key of the name a type or choice cell showed at the export.
fn name_key(c: &FamilyCol) -> String {
    format!("{}@name", c.key)
}

fn blank(d: &Data) -> bool {
    match d {
        Data::Empty => true,
        Data::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

fn number(d: &Data) -> Option<f64> {
    number_or(d).ok()
}

/// Cell -> number; a refused text carries its review reason (ambiguous `1,250` included).
/// Non-finite values (inf, NaN) are refused like any other non-number.
fn number_or(d: &Data) -> Result<f64, String> {
    let v = match d {
        Data::Float(x) => Ok(*x),
        Data::Int(i) => Ok(*i as f64),
        Data::String(s) => units::number(s, None)
            .map_err(|e| units::num_reason(e, s, format!("not a number: '{}'", text(d)))),
        _ => Err(format!("not a number: '{}'", text(d))),
    };
    v.and_then(|x| if x.is_finite() { Ok(x) } else { Err(format!("not a number: '{}'", text(d))) })
}

fn text(d: &Data) -> String {
    match d {
        Data::String(s) => s.trim().to_string(),
        Data::Empty => String::new(),
        d => d.to_string().trim().to_string(),
    }
}

/// Cell -> model value (metres, radians, id or index), or the reason it is refused.
fn parse(model: &Model, opts: &[(i64, String)], c: &FamilyCol, d: &Data, k: f64) -> Result<f64, String> {
    // a type or choice may be named; a number (older books) is the id or index as before
    if let (FamDim::Type(_) | FamDim::Choice(_), Data::String(s)) = (c.dim, d) {
        if let Some(r) = by_name(opts, c, s) {
            return r;
        }
        if number_or(d).is_err() && !opts.is_empty() {
            let (_, tpl) = if matches!(c.dim, FamDim::Type(_)) {
                super::msg::FAM_TYPE_UNKNOWN
            } else {
                super::msg::FAM_CHOICE_UNKNOWN
            };
            return Err(super::msg::fill_template(tpl, &[s.trim()]));
        }
    }
    let v = match (c.dim, d) {
        (FamDim::Len, Data::String(s)) => units::len_value(s, k).map_err(|e| {
            let t = format!("not a number or length: '{s}' (e.g. 3.5, 3500 mm, 10'-6\")");
            units::num_reason(e, s, t)
        })?,
        (FamDim::Len, d) => number_or(d)? * k,
        (_, d) => number_or(d)?,
    };
    if !v.is_finite() {
        return Err("value is not finite".into());
    }
    match c.dim {
        FamDim::Len if v < c.lo || v > c.hi => Err(format!(
            "value must be between {} and {} m, got {}",
            shown(c.lo),
            shown(c.hi),
            shown(v)
        )),
        FamDim::Deg if v < c.lo || v > c.hi => Err(format!(
            "value must be between {} and {} degrees, got {}",
            shown(c.lo),
            shown(c.hi),
            shown(v)
        )),
        FamDim::Pct if v < c.lo || v > c.hi => Err(format!(
            "value must be between {} and {} percent, got {}",
            shown(c.lo),
            shown(c.hi),
            shown(v)
        )),
        FamDim::Deg => Ok(v.to_radians()),
        FamDim::Type(t) => {
            let ok = v.fract() == 0.0
                && model
                    .sql()
                    .query_row(
                        &format!("SELECT COUNT(*) FROM \"{t}\" WHERE id=?1"),
                        [v as i64],
                        |r| r.get::<_, i64>(0),
                    )
                    .is_ok_and(|n| n > 0);
            if ok {
                Ok(v)
            } else {
                Err(format!("unknown type id {}", shown(v)))
            }
        }
        FamDim::Choice(n) if v.fract() != 0.0 || v < 0.0 || v >= n as f64 => Err(format!(
            "choice must be a whole number from 0 to {}, got {}",
            n.saturating_sub(1),
            shown(v)
        )),
        _ => Ok(v),
    }
}

static EMPTY: Data = Data::Empty;

/// Elements a family sheet stands for: see `import::sheet_held`.
fn family_held(
    model: &Model,
    fam: &FamilySheet,
    bl: Option<&super::baseline::Base>,
    rep: &Report,
) -> Result<usize, String> {
    let live: HashSet<i64> = base_rows(model, fam.table)?.rows.iter().map(|r| r.0).collect();
    Ok(super::import::sheet_held(bl, rep, fam.sheet, Some(&live)))
}

#[allow(clippy::too_many_arguments)]
fn read_sheet(
    model: &Model,
    fam: &FamilySheet,
    range: &Range<Data>,
    merged: &[calamine::Dimensions],
    u: Units,
    delete_missing: bool,
    rep: &mut Report,
    bl: Option<&super::baseline::Base>,
) -> Result<(), String> {
    let sheet = fam.sheet;
    let (r0, c0) = range.start().unwrap_or((0, 0));
    let mut known: Vec<String> = READ_ONLY.iter().map(|n| units::norm(n)).collect();
    known.push(units::norm(DELETE_HEAD));
    known.extend(fam.cols.iter().map(|c| units::norm(&c.header(Units::Metric))));
    let h = super::book::find_header(range, &known, |row| {
        row.iter().map(|t| units::norm(t)).collect()
    });
    // a cleared sheet, or only its header left where it held elements: they stay, named
    let none_below = |n: usize| {
        range.rows().skip(n).all(|r| r.iter().all(super::import::blank))
    };
    let cleared = none_below(0);
    if cleared || none_below(h + 1) {
        let held = family_held(model, fam, bl, rep)?;
        if cleared || held > 0 {
            rep.sheet_issues.push(SheetIssue::new(sheet, SheetIssueKind::Empty, held));
            return Ok(());
        }
    }
    let r0 = r0 + h as u32;
    let mut it = range.rows().skip(h);
    let Some(head) = it.next() else {
        return Ok(());
    };
    let head: Vec<String> = head.iter().map(|d| d.to_string().trim().to_string()).collect();
    let normed: Vec<String> = head.iter().map(|h| units::norm(h)).collect();
    // a parameter sheet in its own layout (no family marker) is read by the main import alone
    if super::param_sheet(sheet).is_some() && !normed.contains(&units::norm(READ_ONLY[0])) {
        return Ok(());
    }
    let at = |name: &str| normed.iter().position(|h| *h == units::norm(name));
    let reject = |rep: &mut Report, row: u32, col: u32, column: &str, reason: String| {
        rep.rejected.push(Rejection {
            sheet: sheet.into(),
            row,
            col,
            column: column.into(),
            reason,
        });
    };
    let skip = |rep: &mut Report, row: u32, col: u32, column: &str, id: i64, reason: String| {
        rep.ignored.push(Ignored {
            sheet: sheet.into(),
            row,
            col,
            column: column.into(),
            id: Some(id),
            old: String::new(),
            new: String::new(),
            reason,
        });
    };
    let Some(idc) = at("id") else {
        reject(rep, r0 + 1, c0 + 1, "id", "required column missing in header".into());
        return Ok(());
    };
    let ro: Vec<Option<usize>> = READ_ONLY.iter().map(|n| at(n)).collect();
    let delcol = at(DELETE_HEAD);
    // property columns: (index in the sheet, column, metres per unit)
    let mut props: Vec<(usize, &FamilyCol, f64)> = vec![];
    let mut known: HashSet<usize> = [idc]
        .into_iter()
        .chain(ro.iter().flatten().copied())
        .chain(delcol)
        .collect();
    for c in &fam.cols {
        let Some(i) = at(&c.header(Units::Metric)) else {
            continue;
        };
        known.insert(i);
        let mut k = 1.0;
        if c.dim == FamDim::Len {
            k = match units::header_factor(&head[i], Dim::Len) {
                Some(k) => k,
                None if head[i].contains('[') => {
                    reject(
                        rep,
                        r0 + 1,
                        c0 + i as u32 + 1,
                        &head[i],
                        "unknown unit in header (use m, mm, cm, ft, in)".into(),
                    );
                    continue;
                }
                None => u.factor("[m]"),
            };
        }
        props.push((i, c, k));
    }
    // option lists are read once per sheet, not once per cell
    let popts: Vec<Vec<(i64, String)>> = props.iter().map(|(_, c, _)| options(model, c)).collect();
    // a column under the header of one read above is a copy of it, checked once the rows are in
    let fields: Vec<(usize, f64, bool, bool)> = std::iter::once((idc, 1.0, true, false))
        .chain(ro.iter().flatten().map(|&i| (i, 1.0, false, false)))
        .chain(delcol.map(|i| (i, 1.0, false, false)))
        .chain(props.iter().map(|(i, c, k)| (*i, *k, false, c.dim == FamDim::Len)))
        .collect();
    let mut groups: Vec<super::dup_cols::Group> = vec![];
    for (keep, k, names_row, len) in fields {
        let of = |j: usize| match len {
            true => units::header_factor(&head[j], Dim::Len).unwrap_or(u.factor("[m]")),
            false => 1.0,
        };
        let copies: Vec<(usize, f64)> = (keep + 1..normed.len())
            .filter(|&j| normed[j] == normed[keep])
            .map(|j| (j, of(j)))
            .collect();
        if !copies.is_empty() {
            known.extend(copies.iter().map(|c| c.0));
            groups.push(super::dup_cols::Group { keep: (keep, k), copies, names_row });
        }
    }
    // parameter and phase columns of a category sheet are read by the main import
    let main = super::extra::main_heads(model, sheet)?;
    for (i, h) in head.iter().enumerate() {
        if !h.is_empty() && !known.contains(&i) && !main.contains(&units::norm(h)) {
            rep.ignored.push(Ignored {
                sheet: sheet.into(),
                row: r0 + 1,
                col: c0 + i as u32 + 1,
                column: h.clone(),
                id: None,
                old: String::new(),
                new: String::new(),
                reason: format!("column '{h}' is not a field of {sheet}; not applied"),
            });
        }
    }
    let base: HashMap<i64, [String; 3]> = base_rows(model, fam.table)?.rows.into_iter().collect();
    let mut seen = HashSet::new();
    // an unreadable id cannot be told from a deleted row
    let mut bad_id = false;
    let mut rows: Vec<(u32, Vec<Data>)> =
        it.enumerate().map(|(n, r)| (r0 + 2 + n as u32, r.to_vec())).collect();
    let mut mg = super::merge::apply(sheet, r0, c0, &head, Some(idc), merged, &mut rows, rep);
    let view = super::dup_cols::Sheet { name: sheet, r0: r0 + 1, c0, head: &head, rows: &rows };
    super::dup_cols::check(&view, &groups, &mut mg.covered, rep);
    if let Some(r) = mg.id_row {
        if delete_missing {
            reject(rep, r, c0 + idc as u32 + 1, "id", super::import::NO_DELETES.into());
        }
        bad_id = true;
    }
    for (rno, row) in &rows {
        let rno = *rno;
        let cell = |i: usize| row.get(i).unwrap_or(&EMPTY);
        if row.iter().all(blank) {
            continue;
        }
        let idcol = c0 + idc as u32 + 1;
        let idd = cell(idc);
        if blank(idd) {
            rep.ignored.push(Ignored {
                sheet: sheet.into(),
                row: rno,
                col: 0,
                column: String::new(),
                id: None,
                old: String::new(),
                new: String::new(),
                reason: format!(
                    "row has no id; creating {} from the workbook is not supported",
                    fam.table
                ),
            });
            continue;
        }
        let id = match number(idd).filter(|v| v.fract() == 0.0) {
            Some(v) => v as i64,
            None => {
                reject(rep, rno, idcol, "id", format!("not an integer: '{}'", text(idd)));
                if delete_missing {
                    // same text as the main import; only this sheet's deletes are skipped
                    reject(rep, rno, idcol, "id", super::import::NO_DELETES.into());
                }
                bad_id = true;
                continue;
            }
        };
        if !seen.insert(id) {
            let why = format!("duplicate id {id}; row ignored (clear the id to create a new element)");
            skip(rep, rno, idcol, "id", id, why);
            continue;
        }
        let Some(now) = base.get(&id) else {
            let why = format!("{} {id} no longer exists; row ignored", fam.table);
            skip(rep, rno, idcol, "id", id, why);
            continue;
        };
        // a pinned element neither moves nor goes away from a book edit, as on the main sheets
        let pinned = model.is_pinned(fam.table, id).map_err(|e| e.to_string())?;
        let mut pin_noted = false;
        // copies of the delete column that differ delete nothing
        if let Some(dc) = delcol.filter(|d| !mg.covered.contains(&(rno, *d))) {
            let col = c0 + dc as u32 + 1;
            match flag(cell(dc)) {
                Err(e) => {
                    // the extra pass may have refused this same word already
                    let seen = rep
                        .rejected
                        .iter()
                        .any(|x| x.sheet == sheet && x.row == rno && x.reason == e);
                    if !seen {
                        reject(rep, rno, col, DELETE_HEAD, e);
                    }
                }
                Ok(false) => {}
                Ok(true) => {
                    match fam.delete {
                        RowDelete::Allowed if pin_blocks(model, fam.table, id)? => {
                            skip(rep, rno, col, DELETE_HEAD, id, pinned_reason(fam.table, id));
                        }
                        RowDelete::Allowed => {
                            removal(rep, fam, rno, id, DELETED);
                            rep.props.push(delete_edit(fam, id));
                        }
                        RowDelete::Refused(why) => reject(rep, rno, col, DELETE_HEAD, why.into()),
                    }
                    continue;
                }
            }
        }
        for (j, pos) in ro.iter().enumerate() {
            let Some(i) = *pos else { continue };
            // a name the spreadsheet showed as a number (`007` -> 7) is the same name
            let coerced = matches!(cell(i), Data::Float(_) | Data::Int(_))
                && units::num_text(cell(i), &now[j], units::book_dec()) == units::NumText::Same;
            if !coerced && !units::same_text(&text(cell(i)), &now[j]) {
                let why = super::msg::fill_template(super::msg::READ_ONLY.1, &[now[j].trim()]);
                skip(rep, rno, c0 + i as u32 + 1, &super::msg::column_label(&head[i], READ_ONLY[j]), id, why);
            }
        }
        for (n, (i, c, k)) in props.iter().enumerate() {
            let col = c0 + *i as u32 + 1;
            if mg.covered.contains(&(rno, *i)) {
                // under a merge: not given, the model value stays (the merge note says so)
                continue;
            }
            let d = cell(*i);
            let cur = (c.get)(model, id).ok();
            let opts = &popts[n];
            if blank(d) {
                if cur.is_some() {
                    reject(rep, rno, col, &head[*i], "value required".into());
                }
                continue;
            }
            // a name the export wrote and the user left alone is no edit, whatever the
            // type is called now (or whether it still exists)
            let untouched = match (d, bl.and_then(|b| b.get(sheet, id, &name_key(c)))) {
                (Data::String(t), Some(BVal::S(was))) if named(c) && same_name(t, was) => {
                    match bl.and_then(|b| b.get(sheet, id, c.key)) {
                        Some(BVal::N(x)) => Some(*x),
                        _ => None,
                    }
                }
                _ => None,
            };
            // a type or choice named `007` that the spreadsheet showed as 7 is no edit (never
            // read as the id 7)
            if named(c) && matches!(d, Data::Float(_) | Data::Int(_)) {
                if let Some(name) = cur.and_then(|x| label_in(opts, x)) {
                    if units::num_text(d, &name, units::book_dec()) == units::NumText::Same {
                        if units::num_text(d, "", None) != units::NumText::Edit(name.clone()) {
                            skip(rep, rno, col, &head[*i], id, super::textcode::REASON.into());
                        }
                        continue;
                    }
                }
            }
            let v = match untouched.map_or_else(|| parse(model, opts, c, d, *k), Ok) {
                Ok(v) => v,
                Err(e) => {
                    reject(rep, rno, col, &head[*i], e);
                    continue;
                }
            };
            if cur.is_some_and(|x| same(x, v)) {
                continue;
            }
            if pinned {
                if !pin_noted {
                    skip(rep, rno, col, &head[*i], id, pinned_reason(fam.table, id));
                    pin_noted = true;
                }
                continue;
            }
            // 3-way against the export: an untouched book cell is no edit; both sides changed
            // is a conflict, kept as the model value unless the user takes the book's
            // a value an earlier Apply took from this book counts while the model still holds it
            let live = |b: &BVal| match b {
                BVal::N(x) => cur.is_some_and(|c| same(c, *x)),
                BVal::Blank => cur.is_none(),
                _ => false,
            };
            let file = bl.and_then(|b| b.get(sheet, id, c.key));
            if let Some(b) = super::baseline::base_now(file, sheet, Some(id), c.key, live) {
                let bn = match &b {
                    BVal::N(x) => Some(*x),
                    _ => None,
                };
                let eq = |a: Option<f64>, b: Option<f64>| match (a, b) {
                    (None, None) => true,
                    (Some(a), Some(b)) => same(a, b),
                    _ => false,
                };
                if eq(bn, Some(v)) {
                    rep.model_newer += 1;
                    continue;
                }
                if !eq(bn, cur) {
                    let show = |x: Option<f64>| x.map_or(String::new(), |x| shown_in(opts, c, x, *k));
                    let take = super::baseline::taken(sheet, rno, col);
                    rep.conflicts.push(Conflict {
                        sheet: sheet.into(),
                        row: rno,
                        col,
                        column: head[*i].clone(),
                        id: Some(id),
                        base: show(bn),
                        book: show(Some(v)),
                        model: show(cur),
                        take_book: take,
                    });
                    if !take {
                        let (bk, bs) = (BVal::N(v), bn.map_or(BVal::Blank, BVal::N));
                        rep.kept.extend(super::baseline::KeptCell::new(sheet, Some(id), c.key, bk, bs));
                        continue;
                    }
                }
            }
            rep.changes.push(Change {
                sheet: sheet.into(),
                row: rno,
                col,
                column: head[*i].clone(),
                id: Some(id),
                old: cur.map_or(String::new(), |x| shown_in(opts, c, x, *k)),
                new: shown_in(opts, c, v, *k),
            });
            rep.applied_cells.extend(super::baseline::AppliedCell::new(sheet, Some(id), c.key, BVal::N(v)));
            rep.props.push(PropEdit {
                sheet: sheet.into(),
                table: fam.table,
                id,
                key: c.key,
                value: v,
                set: c.set,
                delete: false,
            });
        }
    }
    if !bad_id || !delete_missing {
        let mut gone: Vec<i64> = base.keys().filter(|i| !seen.contains(i)).copied().collect();
        gone.sort_unstable();
        let mut deleted = 0usize;
        for id in gone {
            // made in the editor after the export: the book never had a row for it
            if bl.is_some_and(|b| b.has_sheet(sheet) && !b.has_id(sheet, id)) {
                continue;
            }
            if matches!(fam.delete, RowDelete::Allowed) && pin_blocks(model, fam.table, id)? {
                rep.ignored.push(Ignored {
                    sheet: sheet.into(),
                    row: 0,
                    col: 0,
                    column: "id".into(),
                    id: Some(id),
                    old: String::new(),
                    new: String::new(),
                    reason: pinned_reason(fam.table, id),
                });
                continue;
            }
            if !delete_missing {
                if matches!(fam.delete, RowDelete::Allowed) {
                    rep.removed.push(Removed {
                        sheet: sheet.into(),
                        id,
                    });
                }
                continue;
            }
            match fam.delete {
                RowDelete::Allowed => {
                    removal(rep, fam, 0, id, MISSING);
                    rep.props.push(delete_edit(fam, id));
                    deleted += 1;
                }
                RowDelete::Refused(why) => rep.ignored.push(Ignored {
                    sheet: sheet.into(),
                    row: 0,
                    col: 0,
                    column: "id".into(),
                    id: Some(id),
                    old: String::new(),
                    new: String::new(),
                    reason: why.into(),
                }),
            }
        }
        rep.note_deletions(sheet, deleted, base.len());
    }
    Ok(())
}

/// A whole-row change: the element goes away (`what` is `DELETED` or `MISSING`).
fn removal(rep: &mut Report, fam: &FamilySheet, row: u32, id: i64, what: &str) {
    rep.changes.push(Change {
        sheet: fam.sheet.into(),
        row,
        col: 0,
        column: "id".into(),
        id: Some(id),
        old: String::new(),
        new: what.into(),
    });
}

/// Adds the family sheets of the book to the report of the main import.
pub(crate) fn read(
    model: &Model,
    path: &Path,
    fams: &[FamilySheet],
    delete_missing: bool,
    rep: &mut Report,
) -> Result<(), String> {
    if fams.is_empty() {
        return Ok(());
    }
    let mut wb: Xlsx<_> = open_workbook(path).map_err(|e| format!("cannot open workbook: {e}"))?;
    let names = wb.sheet_names();
    let u = book_units(&mut wb);
    let _dec = units::DecGuard::set(wb.worksheet_range(INFO).ok().and_then(|r| super::import::info_dec(&r)));
    let bl = super::baseline::read(&mut wb);
    for fam in fams.iter().filter(|f| names.iter().any(|n| n == f.sheet)) {
        // the main read has already named this sheet and read nothing from it
        if rep.sheet_issues.iter().any(|s| s.sheet == fam.sheet) {
            continue;
        }
        let bad = match wb.worksheet_range(fam.sheet) {
            Err(_) => Some(SheetIssueKind::Unreadable),
            Ok(range) if wb.worksheet_formula(fam.sheet).is_ok() => {
                let merged = super::merge::regions(&mut wb, fam.sheet);
                read_sheet(model, fam, &range, &merged, u, delete_missing, rep, bl.as_ref())?;
                None
            }
            Ok(_) => Some(SheetIssueKind::FormulasUnreadable),
        };
        if let Some(kind) = bad {
            let held = family_held(model, fam, bl.as_ref(), rep)?;
            rep.sheet_issues.push(SheetIssue::new(fam.sheet, kind, held));
        }
    }
    Ok(())
}

fn op_count(m: &Model) -> usize {
    m.sql()
        .query_row("SELECT COUNT(*) FROM op", [], |r| r.get::<_, i64>(0))
        .map_or(0, |n| n as usize)
}

/// Applies the edits through the row setters, then removes the flagged elements in one model
/// operation; returns the number of model operations written (the undo depth). A refusal rolls
/// back what this call wrote.
pub fn apply_props(model: &mut Model, props: &[PropEdit]) -> Result<usize, String> {
    let before = op_count(model);
    let rollback = |m: &mut Model| {
        for _ in before..op_count(m) {
            let _ = m.undo();
        }
    };
    for p in props.iter().filter(|p| !p.delete) {
        if let Err(e) = (p.set)(model, p.id, p.value) {
            rollback(model);
            return Err(format!("{} {} {}: {e}", p.table, p.id, p.key));
        }
    }
    let gone: Vec<(&str, i64)> = props
        .iter()
        .filter(|p| p.delete)
        .map(|p| (p.table, p.id))
        .collect();
    if !gone.is_empty() {
        if let Err(e) = model.delete_items("xlsx_delete", &gone) {
            rollback(model);
            return Err(format!("delete {} {}: {e}", gone[0].0, gone[0].1));
        }
    }
    Ok(op_count(model).saturating_sub(before))
}

/// `import_xlsx_families` with the user's choice for conflicts: cells in `take` get the book
/// value instead of keeping the model's.
pub fn import_xlsx_families_take(
    model: &mut Model,
    path: &Path,
    opts: ImportOptions,
    fams: &[FamilySheet],
    take: &super::baseline::Take,
) -> Result<Report, String> {
    import_xlsx_families_take_keyed(model, path, opts, fams, take, &KeyDecisions::default())
}

/// `import_xlsx_families_take` with `dec` settling by-key matches of rows without an id.
pub fn import_xlsx_families_take_keyed(
    model: &mut Model,
    path: &Path,
    opts: ImportOptions,
    fams: &[FamilySheet],
    take: &super::baseline::Take,
    dec: &KeyDecisions,
) -> Result<Report, String> {
    super::baseline::with_take(take, || import_xlsx_families_keyed(model, path, opts, fams, dec))
}

/// `import_xlsx_families_take_keyed` with a progress/cancel hook on the book read; a stop gives
/// the error `job::CANCELLED` and the model is not touched (the hook is dropped before Apply).
pub fn import_xlsx_families_hooked(
    model: &mut Model,
    path: &Path,
    opts: ImportOptions,
    fams: &[FamilySheet],
    take: &super::baseline::Take,
    dec: &KeyDecisions,
    hook: Option<super::export::Hook>,
) -> Result<Report, String> {
    let _guard = super::import_gate::install(hook);
    import_xlsx_families_take_keyed(model, path, opts, fams, take, dec)
}

/// Main import plus the family sheets. Preview (`apply` false) reads both; apply writes the
/// family edits and then the main edits, and undoes the first if the second is refused.
pub fn import_xlsx_families(
    model: &mut Model,
    path: &Path,
    opts: ImportOptions,
    fams: &[FamilySheet],
) -> Result<Report, String> {
    import_xlsx_families_keyed(model, path, opts, fams, &KeyDecisions::default())
}

/// Like `import_xlsx_families`; `dec` settles by-key matches of rows without an id.
pub fn import_xlsx_families_keyed(
    model: &mut Model,
    path: &Path,
    opts: ImportOptions,
    fams: &[FamilySheet],
    dec: &KeyDecisions,
) -> Result<Report, String> {
    let _own = super::extra::family_own(fams);
    let dry = ImportOptions {
        apply: false,
        delete_missing: opts.delete_missing,
        new_params: opts.new_params,
    };
    let mut rep = import_xlsx_keyed(model, path, dry, dec)?;
    // a book without baseline deletes nothing, family rows included
    read(model, path, fams, opts.delete_missing && !rep.no_base, &mut rep)?;
    // the model changes from here: a stop is no longer safe
    super::import_gate::off();
    if !opts.apply || rep.blocking() > 0 {
        return Ok(rep);
    }
    let n = match apply_props(model, &rep.props) {
        Ok(n) => n,
        Err(reason) => {
            rep.rejected.push(Rejection {
                sheet: "(model)".into(),
                row: 0,
                col: 0,
                column: String::new(),
                reason,
            });
            return Ok(rep);
        }
    };
    let done = import_xlsx_keyed(model, path, opts, dec)?;
    if done.blocking() > 0 {
        for _ in 0..n {
            let _ = model.undo();
        }
        rep.rejected = done.rejected;
        return Ok(rep);
    }
    for i in done.ignored.into_iter().filter(|i| i.sheet == "(model)") {
        if !rep.ignored.contains(&i) {
            rep.ignored.push(i);
        }
    }
    rep.applied = true;
    Ok(rep)
}

/// Ids of the first column of a sheet, in row order.
pub fn sheet_ids(path: &Path, sheet: &str) -> Result<Vec<i64>, String> {
    let mut wb: Xlsx<_> = open_workbook(path).map_err(err)?;
    let range = wb
        .worksheet_range(sheet)
        .map_err(|_| format!("no sheet '{sheet}'"))?;
    Ok(range
        .rows()
        .skip(1)
        .filter_map(|r| r.first().and_then(number))
        .map(|v| v as i64)
        .collect())
}

/// Copies a book with one number replaced in the row of `id` under `header` (a stand-in for
/// editing in a spreadsheet program, used by the element matrix test).
#[doc(hidden)]
pub fn patch_cell(
    src: &Path,
    dst: &Path,
    sheet: &str,
    id: i64,
    header: &str,
    value: f64,
) -> Result<(), String> {
    let mut wb: Xlsx<_> = open_workbook(src).map_err(err)?;
    let mut out = Workbook::new();
    let mut hit = false;
    for name in wb.sheet_names() {
        let range = wb.worksheet_range(&name).map_err(err)?;
        let mut grid: Vec<Vec<Data>> = range.rows().map(|r| r.to_vec()).collect();
        if name == sheet {
            let want = units::norm(header);
            let c = grid
                .first()
                .and_then(|h| h.iter().position(|d| units::norm(&d.to_string()) == want))
                .ok_or_else(|| format!("no column '{header}'"))?;
            for row in grid.iter_mut().skip(1) {
                if row.first().and_then(number) == Some(id as f64) {
                    row[c] = Data::Float(value);
                    hit = true;
                }
            }
        }
        let ws = out.add_worksheet();
        ws.set_name(&name).map_err(err)?;
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    _ => Ok(()),
                }
                .map_err(err)?;
            }
        }
    }
    if !hit {
        return Err(format!("no row with id {id} in '{sheet}'"));
    }
    crate::fsutil::replace_durably(dst, |p| out.save(p).map_err(err))
}
