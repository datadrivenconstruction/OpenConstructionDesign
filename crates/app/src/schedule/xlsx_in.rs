//! Schedule <-> XLSX round trip. The export puts the element Id in a leading column; the import
//! reads the sheet back, matches rows by that Id and columns by the header labels of the current
//! table, and writes the changed cells through [`extra::write_cells`]: one op, one undo step.
//! Unknown or deleted Ids never create elements; every refused cell is listed with its address.

use super::extra::{self, Outcome};
use super::{export_num, fmt, header, num_key, Cat, Col, Kind, Table, Val};
use crate::editor::sched_ops::Reason;
use crate::editor::Editor;
use crate::units::{area_in_units, parse_locale_number, ui_decimal_mark, volume_in_units, Units};
use ddc_io::xlsx::{cell_ref, ReadSheet, SheetCell, TableCell};
use std::collections::{HashMap, HashSet};

/// Header of the leading key column (a file format key, the same in every UI language).
pub const ID_HEADER: &str = "Id";

/// Header rows searched for the Id column.
const HEAD_SCAN: usize = 16;

/// Rows of the hidden key sheet before its Id header (file format keys).
const KEY_PROJECT: &str = "project";
const KEY_HEAD: &str = "head";

static EMPTY: SheetCell = SheetCell::Empty;

/// Hidden key sheet grid of a keyed export: project uid, change head, then the exported
/// table itself (`headers` and `data` already carry the leading Id column).
pub fn key_grid(model: &ddc_model::Model, headers: &[String], data: &[Vec<TableCell>]) -> Vec<Vec<TableCell>> {
    let uid = crate::shell::xlsx_project::model_uid(model);
    let mut g = vec![
        vec![
            TableCell::Text(KEY_PROJECT.into()),
            TableCell::Text(uid.map(|u| u.to_string()).unwrap_or_default()),
        ],
        vec![TableCell::Text(KEY_HEAD.into()), TableCell::Num(model.change_head() as f64)],
        headers.iter().cloned().map(TableCell::Text).collect(),
    ];
    g.extend(data.iter().cloned());
    g
}

/// What a keyed export wrote: the base of the three-way import (book then, book now, model now).
#[derive(Clone, Debug, Default)]
pub struct Key {
    pub project: Option<i64>,
    pub head: Option<u64>,
    /// Lower-case header label -> key sheet column.
    cols: HashMap<String, usize>,
    rows: HashMap<(i64, u32), Vec<SheetCell>>,
    /// Cells this session wrote from the same book after the export ([`Applied`]).
    applied: Applied,
}

/// Book cells written by Apply, per (element, lower-case header), newest last. The key sheet
/// is written only at export and the user's book is never rewritten behind their back, so an
/// Apply keeps its new base here; it is used only while the model still holds that value.
#[derive(Clone, Debug, Default)]
pub struct Applied(HashMap<((i64, u32), String), Vec<SheetCell>>);

impl Applied {
    /// Remember the cells `p` planned to write (a cell the write refused fails the
    /// model-value check later and falls back to the key sheet).
    pub fn record(&mut self, p: &Plan) {
        for (id, label, v) in p.src.values() {
            let h = self.0.entry((*id, label.clone())).or_default();
            if h.last().map_or(true, |x| !cell_eq(x, v)) {
                h.push(v.clone());
            }
        }
    }
}

impl Key {
    /// This key with the cells applied from its book since the export.
    pub fn with_applied(mut self, a: Option<&Applied>) -> Key {
        self.applied = a.cloned().unwrap_or_default();
        self
    }

    /// Base of the three-way compare for a cell whose model value `live` accepts: the newest
    /// applied book value the model still holds (an undo or a later model edit drops it),
    /// else the exported cell.
    fn base_now(
        &self,
        id: (i64, u32),
        label: &str,
        live: impl Fn(&SheetCell) -> bool,
    ) -> Option<&SheetCell> {
        let hit = self
            .applied
            .0
            .get(&(id, label.to_string()))
            .and_then(|h| h.iter().rev().find(|a| live(*a)));
        hit.or_else(|| self.base(id, label))
    }

    /// The key sheet of the book at `path`; None for an older book without it.
    pub fn read(path: &std::path::Path) -> Option<Key> {
        let groups = [vec![ddc_io::xlsx::KEY_SHEET.to_string()]];
        let read = ddc_io::xlsx::read_first_of(path, &groups, &|| false).ok()?;
        Key::of_sheet(read.first().and_then(Option::as_ref))
    }

    /// The key from its sheet as read; None without one.
    pub fn of_sheet(s: Option<&ReadSheet>) -> Option<Key> {
        s.filter(|s| s.name == ddc_io::xlsx::KEY_SHEET)
            .and_then(|s| Key::from_grid(&s.grid))
    }

    pub fn from_grid(grid: &[Vec<SheetCell>]) -> Option<Key> {
        let hr = grid
            .iter()
            .take(HEAD_SCAN)
            .position(|r| r.first().is_some_and(is_id_head))?;
        let mut k = Key::default();
        for r in &grid[..hr] {
            match (r.first().map(shown).as_deref(), r.get(1)) {
                (Some(KEY_PROJECT), Some(v)) => k.project = shown(v).parse().ok(),
                (Some(KEY_HEAD), Some(SheetCell::Num(x))) => k.head = Some(*x as u64),
                _ => {}
            }
        }
        k.cols = grid[hr]
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, c)| !is_blank(c))
            .map(|(j, c)| (shown(c).to_lowercase(), j))
            .collect();
        for r in &grid[hr + 1..] {
            if let Some(id) = r.first().and_then(parse_id) {
                k.rows.insert(id, r.clone());
            }
        }
        Some(k)
    }

    /// Exported cell of element `id` under header `label` (lower case); None: not exported.
    fn base(&self, id: (i64, u32), label: &str) -> Option<&SheetCell> {
        let j = *self.cols.get(label)?;
        let r = self.rows.get(&id)?;
        Some(r.get(j).unwrap_or(&EMPTY))
    }
}

/// Two book cells hold the same value (a spreadsheet may turn a number into text on save).
fn cell_eq(a: &SheetCell, b: &SheetCell) -> bool {
    match (a, b) {
        (SheetCell::Num(x), SheetCell::Num(y)) => close(*x, *y),
        (SheetCell::Num(x), SheetCell::Text(t)) | (SheetCell::Text(t), SheetCell::Num(x)) => {
            num_key(t).is_some_and(|y| close(*x, y))
        }
        (SheetCell::Bool(x), SheetCell::Bool(y)) => x == y,
        _ => shown(a) == shown(b),
    }
}

/// Key cell of a row: the element Id; panels (several rows per element) carry `id/cell`.
pub fn id_cell(cat: Cat, id: i64, sub: u32) -> TableCell {
    if cat == Cat::Panels || sub != 0 {
        TableCell::Text(format!("{id}/{sub}"))
    } else {
        TableCell::Num(id as f64)
    }
}

fn parse_id(c: &SheetCell) -> Option<(i64, u32)> {
    match c {
        SheetCell::Num(x) if x.is_finite() && x.fract() == 0.0 => Some((*x as i64, 0)),
        SheetCell::Text(s) => {
            let s = s.trim();
            match s.split_once('/') {
                Some((a, b)) => Some((a.trim().parse().ok()?, b.trim().parse().ok()?)),
                None => Some((s.parse().ok()?, 0)),
            }
        }
        _ => None,
    }
}

fn is_blank(c: &SheetCell) -> bool {
    match c {
        SheetCell::Empty => true,
        SheetCell::Text(s) => s.trim().is_empty(),
        _ => false,
    }
}

fn is_id_head(c: &SheetCell) -> bool {
    matches!(c, SheetCell::Text(s) if s.trim().eq_ignore_ascii_case(ID_HEADER))
}

/// Cell as the user sees it, for the refusal list.
fn shown(c: &SheetCell) -> String {
    match c {
        SheetCell::Empty => String::new(),
        SheetCell::Num(x) => format!("{x}"),
        SheetCell::Text(s) => s.trim().to_string(),
        SheetCell::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_string(),
        SheetCell::Error(t) | SheetCell::Uncalc(t) => t.trim().to_string(),
    }
}

fn bool_text(b: bool, c: &Col) -> String {
    match c.choices.as_slice() {
        [yes, no] => (if b { yes } else { no }).clone(),
        _ => (if b { "TRUE" } else { "FALSE" }).to_string(),
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

/// The book cell already shows the table value (numbers in header units, as exported).
fn same(v: &SheetCell, cur: &Val, c: &Col, u: Units) -> bool {
    let k = c.kind;
    match (v, cur) {
        (SheetCell::Empty, Val::T(s)) => s.trim().is_empty(),
        (SheetCell::Empty, Val::N(x)) => x.is_nan(),
        (SheetCell::Num(a), Val::N(x)) => !x.is_nan() && close(*a, export_num(*x, k, u)),
        (SheetCell::Num(a), Val::T(s)) => num_key(s).is_some_and(|b| close(*a, b)),
        (SheetCell::Text(t), Val::T(s)) => t.trim() == s.trim(),
        (SheetCell::Text(t), Val::N(x)) => {
            let t = t.trim();
            if x.is_nan() {
                return t == "#";
            }
            let book = export_num(*x, k, u);
            t == fmt(cur, k, u).trim()
                || bare_num(t, k).is_some_and(|a| close(a, book))
                || num_key(t).is_some_and(|a| close(a, book))
        }
        (SheetCell::Bool(b), Val::T(s)) => bool_text(*b, c) == s.trim(),
        (SheetCell::Bool(_), Val::N(_)) => false,
        // never a value: refused before any compare
        (SheetCell::Error(_) | SheetCell::Uncalc(_), _) => false,
    }
}

/// A unitless number typed as text in a length, area or volume column (UI decimal mark, as
/// every cell edit); text with a unit (`12'6"`, `150 in`, `3 m2`) is None and keeps its unit.
fn bare_num(s: &str, k: Kind) -> Option<f64> {
    if !matches!(k, Kind::Len | Kind::Area | Kind::Vol) {
        return None;
    }
    parse_locale_number(s.trim(), ui_decimal_mark()).ok()
}

/// Book cell to the text a schedule edit takes; numbers come in header units. None for an
/// error or an uncalculated formula: no edit at all (an empty text would clear the value).
fn edit_text(v: &SheetCell, c: &Col, u: Units) -> Option<String> {
    Some(match v {
        SheetCell::Empty => String::new(),
        // a bare number is in the header unit (ft in an imperial book), like a number cell
        SheetCell::Text(s) => match bare_num(s, c.kind) {
            Some(a) => return edit_text(&SheetCell::Num(a), c, u),
            None => s.trim().to_string(),
        },
        SheetCell::Bool(b) => bool_text(*b, c),
        SheetCell::Error(_) | SheetCell::Uncalc(_) => return None,
        SheetCell::Num(a) => match c.kind {
            Kind::Len => {
                let mm = match u {
                    Units::Metric => *a,
                    Units::Imperial => a * 304.8,
                };
                format!("{mm}mm")
            }
            // explicit SI unit: the writer never guesses the book's units
            Kind::Area => format!("{}m2", a / area_in_units(1.0, u)),
            Kind::Vol => format!("{}m3", a / volume_in_units(1.0, u)),
            _ => format!("{a}"),
        },
    })
}

/// Why a cell or row was not taken.
#[derive(Clone, Debug, PartialEq)]
pub enum Why {
    /// No element with this Id (deleted, or typed in): nothing is created.
    UnknownId,
    /// The Id repeats an earlier row of the sheet: the later row is skipped.
    RepeatedId,
    ReadOnly,
    /// The cell would change a locked parameter value: left as it is.
    Locked,
    Bad,
    /// A valid value the model refuses together with the other cells of the book.
    Refused(Reason),
    /// Changed in the book and, after the export, in the model too (`model` = value now):
    /// never applied, the model edit stays.
    Conflict { model: String },
    /// An error value (`#REF!`): never written, the model value stays.
    FormulaError,
    /// A formula saved without its result: never read as a blank, the model value stays.
    Uncalculated,
    /// A formula with its result: a calculation, not an edit of the element.
    Formula,
}

/// One refused cell: sheet row/column 1-based, the column header and the cell text.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    pub row: u32,
    pub col: u32,
    pub header: String,
    pub text: String,
    pub why: Why,
}

impl Refusal {
    /// "Walls!D7 Height, mm "abc": bad value".
    pub fn line(&self, sheet: &str) -> String {
        let key = match &self.why {
            Why::UnknownId => "sched_import.why_unknown",
            Why::RepeatedId => "sched_import.why_repeated",
            Why::ReadOnly => "sched_import.why_ro",
            Why::Locked => "sched_import.why_locked",
            Why::Bad => "sched_import.why_bad",
            Why::FormulaError => "sched_import.why_error",
            Why::Uncalculated => "sched_import.why_uncalc",
            Why::Formula => "sched_import.why_formula",
            Why::Refused(r) => return self.line_with(sheet, r.text_ui()),
            Why::Conflict { model } => {
                let why = crate::tr("sched_import.why_conflict", &[("model", model.as_str())]);
                return self.line_with(sheet, why);
            }
        };
        self.line_with(sheet, crate::tr(key, &[]))
    }

    fn line_with(&self, sheet: &str, why: String) -> String {
        crate::tr(
            "sched_import.refusal",
            &[
                ("at", &cell_ref(sheet, self.row, self.col)),
                ("col", &self.header),
                ("text", &self.text),
                ("why", &why),
            ],
        )
    }
}

/// What the book would change, counted before anything is written.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub sheet: String,
    /// Cells to write `(table row, table column, edit text)`.
    pub cells: Vec<(usize, usize, String)>,
    /// Book rows matched to table rows.
    pub rows: usize,
    pub unchanged: usize,
    pub unknown_ids: usize,
    pub repeated_ids: usize,
    pub readonly: usize,
    /// Cells of locked parameter values (refused like read-only cells).
    pub locked: usize,
    pub bad: usize,
    pub refusals: Vec<Refusal>,
    /// Book headers that match no column of the table (not read).
    pub ignored_cols: Vec<String>,
    /// Book cell as exported, model changed since: the model value stays.
    pub kept: usize,
    /// Changed in the book and in the model since the export: listed, never applied.
    pub conflicts: usize,
    /// Preview warning: the book cannot tell its edits from later model edits, or names no project.
    pub warn: Option<String>,
    /// Error cells and formulas without a result: refused, the model value stays.
    pub errors: usize,
    /// Formula results: a calculation is not an edit, the model value stays.
    pub formulas: usize,
    /// Sheet rows (1-based) with values but no Id: never an element, listed by number.
    pub no_id_rows: Vec<u32>,
    /// What each planned cell changes, in sheet order, as the preview lists it.
    pub changes: Vec<CellChange>,
    /// Book cell behind each planned cell, by (table row, table column): the base after Apply.
    src: HashMap<(usize, usize), ((i64, u32), String, SheetCell)>,
}

/// One planned cell as the preview lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct CellChange {
    /// Sheet row and column, 1-based.
    pub row: u32,
    pub col: u32,
    /// Element Id as the book shows it (`12` or `12/3`).
    pub element: String,
    pub column: String,
    pub before: String,
    pub after: String,
}

impl CellChange {
    /// "Walls!D7 · 12 · Height, mm: 3000 -> 3500".
    pub fn line(&self, sheet: &str) -> String {
        let blank = crate::tr("sched_import.blank", &[]);
        let or_blank = |s: &str| if s.is_empty() { blank.clone() } else { s.to_string() };
        crate::tr(
            "sched_import.change",
            &[
                ("at", &cell_ref(sheet, self.row, self.col)),
                ("el", &self.element),
                ("col", &self.column),
                ("before", &or_blank(&self.before)),
                ("after", &or_blank(&self.after)),
            ],
        )
    }
}

/// Most rows a preview line names before "and N more".
const ROWS_NAMED: usize = 8;

/// A text the export writes on a total or group total line ("Total: 12"), in either language.
fn is_total_label(s: &str) -> bool {
    let s = s.trim();
    ["sch.total", "sch.group_total"].iter().any(|key| {
        [false, true].iter().any(|&ru| {
            let lab = crate::tr_lang(ru, key, &[("n", "\u{1f}")]);
            let Some((pre, post)) = lab.split_once('\u{1f}') else {
                return false;
            };
            s.strip_prefix(pre.trim())
                .and_then(|r| r.strip_suffix(post.trim()))
                .is_some_and(|n| n.trim().parse::<usize>().is_ok())
        })
    })
}

/// A line the export itself writes without an Id (total, group total): its cells are labels,
/// calculations and "#", never values the user typed.
fn is_export_line(
    row: &[SheetCell],
    ri: usize,
    idc: usize,
    formulas: &HashSet<(usize, usize)>,
) -> bool {
    let mut typed = row
        .iter()
        .enumerate()
        .filter(|(j, c)| *j != idc && !is_blank(c));
    let label = row.iter().any(|c| matches!(c, SheetCell::Text(t) if is_total_label(t)));
    label
        || typed.all(|(j, c)| {
            formulas.contains(&(ri, j)) || matches!(c, SheetCell::Text(t) if t.trim() == "#")
        })
}

impl Plan {
    pub fn changed(&self) -> usize {
        self.cells.len()
    }

    /// "3 rows without Id, not read (rows 12, 15, 18)"; None when every row has its Id.
    pub fn no_id_line(&self) -> Option<String> {
        let n = self.no_id_rows.len();
        if n == 0 {
            return None;
        }
        let mut list: Vec<String> =
            self.no_id_rows.iter().take(ROWS_NAMED).map(|r| r.to_string()).collect();
        if n > ROWS_NAMED {
            list.push(crate::tr("sched_import.more", &[("n", &(n - ROWS_NAMED).to_string())]));
        }
        Some(crate::tr(
            "sched_import.no_id_rows",
            &[("n", &n.to_string()), ("rows", &list.join(", "))],
        ))
    }

    /// Count lines for the preview.
    pub fn count_lines(&self) -> Vec<String> {
        let mut out = vec![];
        for (key, n, always) in [
            ("sched_import.n_changed", self.changed(), true),
            ("sched_import.n_unchanged", self.unchanged, true),
            ("sched_import.n_kept", self.kept, false),
            ("sched_import.n_conflict", self.conflicts, false),
            ("sched_import.n_unknown", self.unknown_ids, false),
            ("sched_import.n_repeated", self.repeated_ids, false),
            ("sched_import.n_ro", self.readonly, false),
            ("sched_import.n_locked", self.locked, false),
            ("sched_import.n_bad", self.bad, false),
            ("sched_import.n_error", self.errors, false),
            ("sched_import.n_formula", self.formulas, false),
        ] {
            if always || n > 0 {
                out.push(crate::tr(key, &[("n", &n.to_string())]));
            }
        }
        out
    }
}

/// Read the book grid against the current table; nothing is written. Without a key the
/// book is compared with the model only (two-way).
pub fn plan(
    ed: &Editor,
    tb: &Table,
    u: Units,
    sheet: &str,
    grid: &[Vec<SheetCell>],
) -> Result<Plan, String> {
    plan_keyed(ed, tb, u, sheet, grid, None)
}

/// [`plan`] three-way against the book's `key`: a cell still as exported is no edit (a later
/// model edit is kept), a cell changed in both is a conflict, and a book of another project
/// is refused before any row is read.
pub fn plan_keyed(
    ed: &Editor,
    tb: &Table,
    u: Units,
    sheet: &str,
    grid: &[Vec<SheetCell>],
    key: Option<&Key>,
) -> Result<Plan, String> {
    plan_in(ed, tb, u, sheet, grid, &HashSet::new(), key)
}

/// [`plan_keyed`] for a sheet as read: its formula results are calculations, not edits.
pub fn plan_sheet(
    ed: &Editor,
    tb: &Table,
    u: Units,
    s: &ReadSheet,
    key: Option<&Key>,
) -> Result<Plan, String> {
    plan_in(ed, tb, u, &s.name, &s.grid, &s.formulas, key)
}

fn plan_in(
    ed: &Editor,
    tb: &Table,
    u: Units,
    sheet: &str,
    grid: &[Vec<SheetCell>],
    formulas: &HashSet<(usize, usize)>,
    key: Option<&Key>,
) -> Result<Plan, String> {
    use crate::shell::xlsx_project::{origin_of, Origin};
    let warn = match key.map(|k| origin_of(&ed.model, k.project)) {
        None => Some(crate::tr("sched_import.no_key", &[])),
        Some(Origin::Other) => return Err(crate::tr("xlsx_project.title", &[])),
        Some(Origin::Unknown) => Some(crate::tr("xlsx_project.no_project", &[])),
        Some(Origin::Same) => None,
    };
    let Some(hr) = grid
        .iter()
        .take(HEAD_SCAN)
        .position(|r| r.iter().any(is_id_head))
    else {
        return Err(crate::tr("sched_import.no_id_col", &[]));
    };
    let head = &grid[hr];
    let idc = head.iter().position(is_id_head).unwrap_or(0);
    let mut p = Plan {
        sheet: sheet.to_string(),
        warn,
        ..Plan::default()
    };
    // sheet column -> table column, by the header label in the current units
    let labels: Vec<String> = tb
        .cols
        .iter()
        .map(|c| header(c, u).trim().to_lowercase())
        .collect();
    let mut cols: Vec<(usize, usize)> = vec![];
    let mut taken = HashSet::new();
    for (j, c) in head.iter().enumerate() {
        if j == idc || is_blank(c) {
            continue;
        }
        let h = shown(c);
        let low = h.to_lowercase();
        match labels.iter().position(|l| *l == low) {
            Some(k) if taken.insert(k) => cols.push((j, k)),
            _ => p.ignored_cols.push(h),
        }
    }
    let index: HashMap<(i64, u32), usize> = tb
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| ((r.id, r.sub), i))
        .collect();
    let mut seen = HashSet::new();
    // sheet position of each cell to write, to place a refusal found on the combined check
    let mut at: HashMap<(usize, usize), (u32, u32, String)> = HashMap::new();
    let empty = SheetCell::Empty;
    for (ri, row) in grid.iter().enumerate().skip(hr + 1) {
        let get = |j: usize| row.get(j).unwrap_or(&empty);
        let idv = get(idc);
        let sr = ri as u32 + 1;
        // blank Id: totals row or a row the user typed below; never an element, but a typed
        // row is named so it does not vanish without a word
        if is_blank(idv) {
            let typed = row.iter().enumerate().any(|(j, c)| j != idc && !is_blank(c));
            if typed && !is_export_line(row, ri, idc, formulas) {
                p.no_id_rows.push(sr);
            }
            continue;
        }
        let refuse = |why| Refusal {
            row: sr,
            col: idc as u32 + 1,
            header: ID_HEADER.to_string(),
            text: shown(idv),
            why,
        };
        // an Id that is an error or was never calculated names no element
        let id_why = match idv {
            SheetCell::Error(_) => Some(Why::FormulaError),
            SheetCell::Uncalc(_) => Some(Why::Uncalculated),
            _ => None,
        };
        if let Some(why) = id_why {
            p.errors += 1;
            p.refusals.push(refuse(why));
            continue;
        }
        // a group header line: text in the Id column and nothing else; never an element
        let alone = row.iter().enumerate().all(|(j, c)| j == idc || is_blank(c));
        if alone && parse_id(idv).is_none() {
            continue;
        }
        let Some(i) = parse_id(idv).and_then(|k| index.get(&k).copied()) else {
            p.unknown_ids += 1;
            p.refusals.push(refuse(Why::UnknownId));
            continue;
        };
        if !seen.insert(i) {
            p.repeated_ids += 1;
            p.refusals.push(refuse(Why::RepeatedId));
            continue;
        }
        p.rows += 1;
        for &(j, k) in &cols {
            let v = get(j);
            let c = &tb.cols[k];
            let Some(cur) = tb.rows[i].vals.get(k) else {
                continue;
            };
            // an error, a lost result or a calculation never changes the model
            let kind_why = match v {
                SheetCell::Error(e) if same(&SheetCell::Text(e.clone()), cur, c, u) => None,
                SheetCell::Error(_) => Some(Why::FormulaError),
                SheetCell::Uncalc(_) => Some(Why::Uncalculated),
                _ if formulas.contains(&(ri, j)) && !same(v, cur, c, u) => Some(Why::Formula),
                _ => None,
            };
            if let Some(why) = kind_why {
                if why == Why::Formula {
                    p.formulas += 1;
                } else {
                    p.errors += 1;
                }
                p.refusals.push(Refusal {
                    row: sr,
                    col: j as u32 + 1,
                    header: header(c, u),
                    text: shown(v),
                    why,
                });
                continue;
            }
            if matches!(v, SheetCell::Error(_)) || same(v, cur, c, u) {
                p.unchanged += 1;
                continue;
            }
            let id = (tb.rows[i].id, tb.rows[i].sub);
            if let Some(b) = key.and_then(|kk| kk.base_now(id, &labels[k], |a| same(a, cur, c, u))) {
                if cell_eq(v, b) {
                    // the book still shows the export: the model moved since, keep it
                    p.kept += 1;
                    continue;
                }
                if !same(b, cur, c, u) {
                    p.conflicts += 1;
                    p.refusals.push(Refusal {
                        row: sr,
                        col: j as u32 + 1,
                        header: header(c, u),
                        text: shown(v),
                        why: Why::Conflict { model: fmt(cur, c.kind, u).trim().to_string() },
                    });
                    continue;
                }
            }
            let Some(text) = edit_text(v, c, u) else {
                continue;
            };
            let why = match extra::check_cell(ed, tb, i, k, &text) {
                Ok(true) => {
                    at.insert((i, k), (sr, j as u32 + 1, shown(v)));
                    p.src.insert((i, k), (id, labels[k].clone(), v.clone()));
                    p.cells.push((i, k, text));
                    continue;
                }
                Ok(false) => {
                    p.unchanged += 1;
                    continue;
                }
                Err(extra::Skip::ReadOnly) if extra::cell_locked(ed, tb, i, k, &text) => {
                    p.locked += 1;
                    Why::Locked
                }
                Err(extra::Skip::ReadOnly) => {
                    p.readonly += 1;
                    Why::ReadOnly
                }
                Err(extra::Skip::Bad) => {
                    p.bad += 1;
                    Why::Bad
                }
                Err(extra::Skip::Refused(r)) => {
                    p.bad += 1;
                    Why::Refused(r)
                }
            };
            p.refusals.push(Refusal {
                row: sr,
                col: j as u32 + 1,
                header: header(c, u),
                text: shown(v),
                why,
            });
        }
    }
    // the same combined check as the write, so the preview counts what the write will do:
    // cells that are fine alone but not together (overlapping openings) are refused here
    let (_, refused) = extra::dry_run(ed, tb, &p.cells);
    if !refused.is_empty() {
        let bad: HashSet<(usize, usize)> = refused.iter().map(|(r, c, _)| (*r, *c)).collect();
        p.cells.retain(|(r, c, _)| !bad.contains(&(*r, *c)));
        p.src.retain(|rc, _| !bad.contains(rc));
        for (r, c, why) in refused {
            let (row, col, text) = at.remove(&(r, c)).unwrap_or_default();
            p.bad += 1;
            p.refusals.push(Refusal {
                row,
                col,
                header: header(&tb.cols[c], u),
                text,
                why: Why::Refused(why),
            });
        }
        p.refusals.sort_by_key(|f| (f.row, f.col));
    }
    p.changes = p
        .cells
        .iter()
        .filter_map(|&(i, k, _)| {
            let (row, col, after) = at.get(&(i, k))?.clone();
            let (r, c) = (&tb.rows[i], &tb.cols[k]);
            let before = r.vals.get(k).map(|v| fmt(v, c.kind, u).trim().to_string());
            Some(CellChange {
                row,
                col,
                element: match id_cell(tb.cat, r.id, r.sub) {
                    TableCell::Text(t) => t,
                    _ => r.id.to_string(),
                },
                column: header(c, u),
                before: before.unwrap_or_default(),
                after,
            })
        })
        .collect();
    p.changes.sort_by_key(|c| (c.row, c.col));
    Ok(p)
}

/// Write the planned cells as one op (one undo step); refused cells add to the counts.
pub fn apply(ed: &mut Editor, tb: &Table, p: &Plan) -> Outcome {
    let mut o = if p.cells.is_empty() {
        Outcome::default()
    } else {
        extra::write_cells(ed, tb, &p.cells)
    };
    o.unchanged += p.unchanged + p.kept;
    o.readonly += p.readonly + p.locked;
    o.bad += p.bad + p.unknown_ids + p.repeated_ids + p.conflicts + p.errors + p.formulas;
    o
}

/// Write the plan and say what happened. `preview` is the changed-cell count the user saw;
/// when the table moved since, the status names both counts.
pub fn apply_say(ed: &mut Editor, tb: &Table, p: &Plan, preview: usize) -> (String, bool) {
    let before = ed.status.take();
    let o = apply(ed, tb, p);
    let status = ed.status.take();
    let r = say_after(&o, p.cells.len(), status.clone(), preview);
    if !r.1 || status.is_none() {
        ed.status = before;
    }
    r
}

/// Pure part of [`apply_say`]: a write that failed shows the failure itself, never "Imported 0".
pub fn say_after(o: &Outcome, planned: usize, status: Option<String>, preview: usize) -> (String, bool) {
    if planned > 0 && o.done == 0 {
        if let Some(s) = status {
            return (s, true);
        }
    }
    let (msg, flag) = say(o);
    if preview == o.done {
        return (msg, flag);
    }
    let diff = crate::tr(
        "sched_import.preview_diff",
        &[
            ("p", &preview.to_string()),
            ("n", &o.done.to_string()),
            ("d", &preview.abs_diff(o.done).to_string()),
        ],
    );
    (format!("{msg}. {diff}"), true)
}

/// Status after an import; the flag is "something was skipped".
pub fn say(o: &Outcome) -> (String, bool) {
    if o.done == 0 && o.skipped() == 0 {
        return (crate::tr("sched_import.no_changes", &[]), false);
    }
    let n = o.done.to_string();
    if o.skipped() == 0 {
        (crate::tr("sched_import.applied", &[("n", &n)]), false)
    } else {
        let k = o.skipped().to_string();
        (crate::tr("sched_import.applied_skipped", &[("n", &n), ("k", &k)]), true)
    }
}

#[cfg(test)]
mod safety_tests;

#[cfg(test)]
mod tests {
    use super::super::{build, export_xlsx_keyed, order};
    use super::*;
    use ddc_io::xlsx::{export_table, read_sheet_cells};

    fn walls() -> Editor {
        let mut e = Editor::new_in_memory().unwrap();
        super::super::tests::room(&mut e);
        e
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ddc_sch3_{tag}_{}.xlsx", std::process::id()))
    }

    /// Export all rows and columns, read the book back.
    fn exported(e: &Editor, tag: &str) -> (Table, std::path::PathBuf, String, Vec<Vec<SheetCell>>) {
        let tb = build(e, Cat::Walls);
        let ord = order(&tb, &[], None, Units::Metric);
        let cols: Vec<usize> = (0..tb.cols.len()).collect();
        let p = tmp(tag);
        export_xlsx_keyed(&e.model, &tb, &ord, &cols, Units::Metric, &p, &[]).unwrap();
        let (sheet, grid) = read_sheet_cells(&p, None).unwrap();
        (tb, p, sheet, grid)
    }

    /// Save an edited grid as the user's book (header row + rows) and read it back.
    fn resave(p: &std::path::Path, sheet: &str, grid: &[Vec<SheetCell>]) -> Vec<Vec<SheetCell>> {
        let to = |c: &SheetCell| match c {
            SheetCell::Num(x) => TableCell::Num(*x),
            other => TableCell::Text(shown(other)),
        };
        let headers: Vec<String> = grid[0].iter().map(shown).collect();
        let rows: Vec<Vec<TableCell>> = grid[1..]
            .iter()
            .map(|r| r.iter().map(to).collect())
            .collect();
        export_table(p, sheet, &headers, &[], &rows, None).unwrap();
        read_sheet_cells(p, Some(sheet)).unwrap().1
    }

    fn col_of(grid: &[Vec<SheetCell>], tb: &Table, key: &str) -> usize {
        let k = tb.cols.iter().position(|c| c.key == key).unwrap();
        let h = header(&tb.cols[k], Units::Metric);
        grid[0]
            .iter()
            .position(|c| matches!(c, SheetCell::Text(s) if *s == h))
            .unwrap()
    }

    fn row_of(grid: &[Vec<SheetCell>], id: i64) -> usize {
        grid.iter()
            .position(|r| r.first() == Some(&SheetCell::Num(id as f64)))
            .unwrap()
    }

    fn val(e: &Editor, id: i64, key: &str) -> Val {
        let tb = build(e, Cat::Walls);
        let k = tb.cols.iter().position(|c| c.key == key).unwrap();
        tb.rows.iter().find(|r| r.id == id).unwrap().vals[k].clone()
    }

    fn len(e: &Editor, id: i64) -> f64 {
        match val(e, id, "len") {
            Val::N(x) => x,
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn schedule_xlsx_roundtrip_edits_one_undo() {
        let mut e = walls();
        let (tb, p, sheet, mut grid) = exported(&e, "rt");
        assert!(is_id_head(&grid[0][0]), "{:?}", grid[0]);
        let (a, b) = (tb.rows[0].id, tb.rows[1].id);
        let h0 = val(&e, b, "h");
        let (jl, jh) = (col_of(&grid, &tb, "len"), col_of(&grid, &tb, "h"));
        let (ra, rb) = (row_of(&grid, a), row_of(&grid, b));
        grid[ra][jl] = SheetCell::Num(7000.0);
        grid[rb][jh] = SheetCell::Num(3500.0);
        let grid = resave(&p, &sheet, &grid);
        let pl = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert_eq!(pl.changed(), 2, "{:?}", pl.refusals);
        assert_eq!(pl.rows, 4);
        assert!(pl.refusals.is_empty(), "{:?}", pl.refusals);
        let rev = e.revision;
        let o = apply(&mut e, &tb, &pl);
        assert_eq!(o.done, 2, "{:?}", e.status);
        assert_ne!(e.revision, rev);
        assert!((len(&e, a) - 7.0).abs() < 1e-9, "{:?}", val(&e, a, "len"));
        assert_eq!(val(&e, b, "h"), Val::N(3.5));
        // one undo step returns both cells
        e.undo();
        let tb2 = build(&e, Cat::Walls);
        let jl2 = tb2.cols.iter().position(|c| c.key == "len").unwrap();
        assert_eq!(tb2.rows.iter().find(|r| r.id == a).unwrap().vals[jl2], tb.rows[0].vals[jl2]);
        assert_eq!(val(&e, b, "h"), h0);
        let _ = std::fs::remove_file(&p);
    }

    /// Export one category with every column, read the book back.
    fn exported_cat(e: &Editor, cat: Cat, tag: &str) -> (Table, std::path::PathBuf, String, Vec<Vec<SheetCell>>) {
        let tb = build(e, cat);
        let ord = order(&tb, &[], None, Units::Metric);
        let cols: Vec<usize> = (0..tb.cols.len()).collect();
        let p = tmp(tag);
        export_xlsx_keyed(&e.model, &tb, &ord, &cols, Units::Metric, &p, &[]).unwrap();
        let (sheet, grid) = read_sheet_cells(&p, None).unwrap();
        (tb, p, sheet, grid)
    }

    /// Two 1 m doors on the first wall at 1.0..2.0 and 2.2..3.2 m.
    fn doors(e: &mut Editor) -> [i64; 2] {
        let wall = e.walls[0].id;
        let spec = |along: f64| ddc_model::OpeningSpec {
            along,
            width: 1.0,
            sill: 0.0,
            height: 2.0,
            kind: "door".into(),
        };
        let a = e.model.add_opening(wall, &spec(1.0)).unwrap();
        let b = e.model.add_opening(wall, &spec(2.2)).unwrap();
        e.reload().unwrap();
        [a, b]
    }

    #[test]
    fn two_cells_of_one_wall_count_as_two_and_roundtrip() {
        let mut e = walls();
        let (tb, p, sheet, mut grid) = exported(&e, "lenh");
        let a = tb.rows[0].id;
        let (jl, jh) = (col_of(&grid, &tb, "len"), col_of(&grid, &tb, "h"));
        let ra = row_of(&grid, a);
        grid[ra][jl] = SheetCell::Num(5500.0);
        grid[ra][jh] = SheetCell::Num(3200.0);
        let grid = resave(&p, &sheet, &grid);
        let pl = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert_eq!(pl.changed(), 2, "{:?}", pl.refusals);
        let u0 = e.undo_len();
        let o = apply(&mut e, &tb, &pl);
        assert_eq!(o.done, pl.changed(), "applied equals preview: {:?}", e.status);
        assert_eq!(e.undo_len(), u0 + 1);
        assert!((len(&e, a) - 5.5).abs() < 1e-9);
        assert_eq!(val(&e, a, "h"), Val::N(3.2));
        // the written model exports back to the edited book: nothing more to import
        let (tb2, p2, sheet2, grid2) = exported(&e, "lenh2");
        let again = plan(&e, &tb2, Units::Metric, &sheet2, &grid2).unwrap();
        assert_eq!(again.changed(), 0, "{:?}", again.refusals);
        assert!(again.refusals.is_empty(), "{:?}", again.refusals);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&p2);
    }

    #[test]
    fn book_with_overlapping_openings_writes_only_the_valid_one() {
        let mut e = walls();
        let [a, b] = doors(&mut e);
        let (tb, p, sheet, mut grid) = exported_cat(&e, Cat::Doors, "ovl");
        let jw = col_of(&grid, &tb, "w");
        // a: 1.0..2.5 runs into b (2.2..3.2); b: 2.2..3.7 is free
        let (ra, rb) = (row_of(&grid, a), row_of(&grid, b));
        grid[ra][jw] = SheetCell::Num(1500.0);
        grid[rb][jw] = SheetCell::Num(1500.0);
        let grid = resave(&p, &sheet, &grid);
        let pl = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        // the preview runs the combined check: a is refused there with its address and reason
        assert_eq!((pl.changed(), pl.bad), (1, 1), "{:?}", pl.refusals);
        assert_eq!(pl.refusals.len(), 1);
        let f = &pl.refusals[0];
        assert_eq!((f.row, f.col), (ra as u32 + 1, jw as u32 + 1));
        assert!(
            matches!(&f.why, Why::Refused(Reason::Place { err: crate::opening_tool::PlaceError::Overlap { .. }, .. })),
            "{:?}",
            f.why
        );
        let u0 = e.undo_len();
        let o = apply(&mut e, &tb, &pl);
        assert_eq!((o.done, o.bad), (pl.changed(), pl.bad), "applied equals preview: {:?}", e.status);
        assert_eq!(e.undo_len(), u0 + 1);
        assert!((e.opening(a).unwrap().width - 1.0).abs() < 1e-9, "refused cell kept");
        assert!((e.opening(b).unwrap().width - 1.5).abs() < 1e-9);
        let (msg, flag) = say_after(&o, pl.cells.len(), None, pl.changed());
        assert_eq!(msg, say(&o).0, "no preview/applied mismatch note");
        assert!(flag);
        let line = f.line(&sheet);
        assert!(line.contains(&format!("#{a}")), "{line}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn book_shortening_wall_under_its_door_refused_in_preview() {
        let mut e = walls();
        let [a, _] = doors(&mut e);
        let wall = e.walls[0].id;
        let (tb, p, sheet, mut grid) = exported(&e, "short");
        let len0 = len(&e, wall);
        let jl = col_of(&grid, &tb, "len");
        // door a ends at 2.0 m, so a 1.5 m wall cannot keep it
        let rw = row_of(&grid, wall);
        grid[rw][jl] = SheetCell::Num(1500.0);
        let grid = resave(&p, &sheet, &grid);
        let pl = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert_eq!(pl.changed(), 0);
        assert_eq!(pl.bad, 1, "{:?}", pl.refusals);
        assert!(
            matches!(&pl.refusals[0].why, Why::Refused(Reason::Place { id, .. }) if *id == a),
            "reason names the door: {:?}",
            pl.refusals
        );
        assert_eq!((pl.refusals[0].row, pl.refusals[0].col), (rw as u32 + 1, jl as u32 + 1));
        let (u0, rev) = (e.undo_len(), e.revision);
        let o = apply(&mut e, &tb, &pl);
        assert_eq!((o.done, o.bad), (pl.changed(), pl.bad), "applied equals preview");
        assert_eq!(o.done, 0);
        assert_eq!((e.undo_len(), e.revision), (u0, rev), "nothing written, no undo step");
        assert!((len(&e, wall) - len0).abs() < 1e-9);
        assert!(e.opening(a).is_some());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn schedule_xlsx_unknown_id_reported_not_created() {
        let mut e = walls();
        let (tb, p, sheet, mut grid) = exported(&e, "unk");
        let jh = col_of(&grid, &tb, "h");
        let width = grid[0].len();
        let mut extra_row = |id: SheetCell| {
            let mut r = vec![SheetCell::Empty; width];
            r[0] = id;
            r[jh] = SheetCell::Num(2800.0);
            grid.push(r);
        };
        // an Id that never existed and junk text; the row of a wall deleted below stays
        extra_row(SheetCell::Num(999_999.0));
        extra_row(SheetCell::Text("abc".into()));
        let gone = tb.rows[3].id;
        // the user also repeats an existing row
        let first = grid[row_of(&grid, tb.rows[0].id)].clone();
        grid.push(first);
        let grid = resave(&p, &sheet, &grid);
        e.selection = vec![crate::editor::Sel::Wall(gone)];
        e.delete_selection();
        let tb = build(&e, Cat::Walls);
        assert_eq!(tb.rows.len(), 3, "{:?}", e.status);
        let pl = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert_eq!(pl.unknown_ids, 3, "{:?}", pl.refusals);
        assert_eq!(pl.repeated_ids, 1, "{:?}", pl.refusals);
        let unk: Vec<&Refusal> = pl
            .refusals
            .iter()
            .filter(|r| r.why == Why::UnknownId)
            .collect();
        assert!(unk.iter().all(|r| r.col == 1 && r.row > 1));
        assert!(unk.iter().any(|r| r.text == gone.to_string()));
        assert!(unk.iter().any(|r| r.text == "abc"));
        // joined neighbours of the deleted wall may differ from the book; unknown rows plan nothing
        let gone_row = grid.iter().position(|r| r.first() == Some(&SheetCell::Num(gone as f64)));
        assert!(gone_row.is_some());
        assert!(pl.cells.iter().all(|(i, _, _)| tb.rows[*i].id != gone));
        let walls = e.walls.len();
        let o = apply(&mut e, &tb, &pl);
        assert_eq!(o.done, pl.changed());
        assert_eq!(e.walls.len(), walls, "nothing created");
        assert!(e.walls.iter().all(|w| w.id != gone && w.id != 999_999));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn schedule_xlsx_readonly_and_bad_cells_reported() {
        let mut e = walls();
        let (tb, p, sheet, mut grid) = exported(&e, "rob");
        let (a, b) = (tb.rows[0].id, tb.rows[1].id);
        let (jv, jl, jh) = (
            col_of(&grid, &tb, "vol"),
            col_of(&grid, &tb, "len"),
            col_of(&grid, &tb, "h"),
        );
        let (ra, rb) = (row_of(&grid, a), row_of(&grid, b));
        grid[ra][jv] = SheetCell::Num(99.0);
        grid[ra][jl] = SheetCell::Text("abc".into());
        grid[rb][jh] = SheetCell::Num(3100.0);
        let grid = resave(&p, &sheet, &grid);
        let pl = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert_eq!((pl.changed(), pl.readonly, pl.bad), (1, 1, 1), "{:?}", pl.refusals);
        let ro = pl.refusals.iter().find(|r| r.why == Why::ReadOnly).unwrap();
        assert_eq!((ro.row, ro.col), (ra as u32 + 1, jv as u32 + 1));
        assert_eq!(ro.header, header(&tb.cols[tb.cols.iter().position(|c| c.key == "vol").unwrap()], Units::Metric));
        let bad = pl.refusals.iter().find(|r| r.why == Why::Bad).unwrap();
        assert_eq!((bad.row, bad.col, bad.text.as_str()), (ra as u32 + 1, jl as u32 + 1, "abc"));
        assert!(bad.line(&sheet).contains("abc"));
        let len_a = val(&e, a, "len");
        let o = apply(&mut e, &tb, &pl);
        assert_eq!((o.done, o.readonly, o.bad), (1, 1, 1), "{:?}", e.status);
        assert_eq!(val(&e, b, "h"), Val::N(3.1));
        assert_eq!(val(&e, a, "len"), len_a, "bad cell not written");
        assert!(say(&o).1, "skipped cells flagged");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn schedule_xlsx_unchanged_book_no_changes() {
        let mut e = walls();
        let (tb, p, sheet, grid) = exported(&e, "same");
        let pl = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert_eq!(pl.changed(), 0, "{:?}", pl.cells);
        assert!(pl.refusals.is_empty(), "{:?}", pl.refusals);
        assert!(pl.ignored_cols.is_empty(), "{:?}", pl.ignored_cols);
        assert_eq!(pl.rows, tb.rows.len());
        assert_eq!(pl.unchanged, tb.rows.len() * tb.cols.len());
        let rev = e.revision;
        let top = e.model.op_top().ok();
        let o = apply(&mut e, &tb, &pl);
        assert_eq!((o.done, o.skipped()), (0, 0));
        assert_eq!(e.revision, rev, "no op, no undo step");
        assert_eq!(e.model.op_top().ok(), top);
        assert!(!say(&o).1);
        let _ = std::fs::remove_file(&p);
    }

    /// Editor with one wall parameter of the given type, no value set.
    fn with_param(ptype: ddc_model::ParamType, u: Units) -> (Editor, i64) {
        let mut e = walls();
        let pid = e
            .model
            .add_param(&ddc_model::ParamSpec {
                name: "Zone".into(),
                ptype,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        e.reload().unwrap();
        e.tool.units = u;
        (e, pid)
    }

    /// One-row book: Id and the param column set to `v`.
    fn book(tb: &Table, pid: i64, u: Units, v: f64) -> Vec<Vec<SheetCell>> {
        let c = &tb.cols[tb.cols.iter().position(|c| c.key == format!("p:{pid}")).unwrap()];
        vec![
            vec![SheetCell::Text(ID_HEADER.to_string()), SheetCell::Text(header(c, u))],
            vec![SheetCell::Num(tb.rows[0].id as f64), SheetCell::Num(v)],
        ]
    }

    fn param_val(e: &Editor, pid: i64) -> f64 {
        let tb = build(e, Cat::Walls);
        let k = tb.cols.iter().position(|c| c.key == format!("p:{pid}")).unwrap();
        match tb.rows[0].vals[k] {
            Val::N(x) => x,
            ref v => panic!("{v:?}"),
        }
    }

    #[test]
    fn imperial_area_cell_writes_shown_value() {
        let u = Units::Imperial;
        let (mut e, pid) = with_param(ddc_model::ParamType::Area, u);
        let tb = build(&e, Cat::Walls);
        let grid = book(&tb, pid, u, 200.0);
        let pl = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!(pl.changed(), 1, "{:?}", pl.refusals);
        assert!(pl.cells[0].2.ends_with("m2"), "explicit SI unit: {}", pl.cells[0].2);
        let o = apply(&mut e, &tb, &pl);
        assert_eq!(o.done, 1, "{:?}", e.status);
        let m2 = param_val(&e, pid);
        assert!((m2 - 200.0 * 0.3048 * 0.3048).abs() < 1e-9, "{m2}");
        // the book shows the same number it wrote
        assert!((export_num(m2, Kind::Area, u) - 200.0).abs() < 1e-9);
        let tb = build(&e, Cat::Walls);
        let pl = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!((pl.changed(), pl.bad), (0, 0), "{:?}", pl.refusals);
    }

    #[test]
    fn metric_volume_cell_roundtrips() {
        let u = Units::Metric;
        let (mut e, pid) = with_param(ddc_model::ParamType::Volume, u);
        let tb = build(&e, Cat::Walls);
        let grid = book(&tb, pid, u, 12.5);
        let pl = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!(pl.changed(), 1, "{:?}", pl.refusals);
        assert!(pl.cells[0].2.ends_with("m3"), "{}", pl.cells[0].2);
        assert_eq!(apply(&mut e, &tb, &pl).done, 1, "{:?}", e.status);
        assert_eq!(param_val(&e, pid), 12.5);
        let tb = build(&e, Cat::Walls);
        let pl = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!((pl.changed(), pl.bad), (0, 0), "{:?}", pl.refusals);
    }

    /// One-row book with the param column set to the cell `v`.
    fn book_cell(tb: &Table, pid: i64, u: Units, v: SheetCell) -> Vec<Vec<SheetCell>> {
        let mut g = book(tb, pid, u, 0.0);
        g[1][1] = v;
        g
    }

    /// Applies the one-cell book to a fresh wall param, returns the value in SI; a second
    /// read of the same book changes nothing.
    fn applied(ptype: ddc_model::ParamType, u: Units, v: SheetCell) -> f64 {
        let (mut e, pid) = with_param(ptype, u);
        let tb = build(&e, Cat::Walls);
        let grid = book_cell(&tb, pid, u, v.clone());
        let pl = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!((pl.changed(), pl.bad), (1, 0), "{v:?} {:?}", pl.refusals);
        assert_eq!(apply(&mut e, &tb, &pl).done, 1, "{v:?} {:?}", e.status);
        let tb = build(&e, Cat::Walls);
        let pl = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!((pl.changed(), pl.bad), (0, 0), "unchanged {v:?} {:?}", pl.refusals);
        param_val(&e, pid)
    }

    #[test]
    fn imperial_text_length_reads_in_feet_like_a_number() {
        crate::units::set_ui_decimal_mark(crate::units::DecimalMark::Point);
        let (len, i) = (ddc_model::ParamType::Length, Units::Imperial);
        for v in [
            SheetCell::Text("12.5".into()),
            SheetCell::Num(12.5),
            SheetCell::Text("12' 6\"".into()),
            SheetCell::Text("12'-6\"".into()),
            SheetCell::Text("150 in".into()),
            SheetCell::Text("3810 mm".into()),
        ] {
            let m = applied(len, i, v.clone());
            assert!((m - 3.81).abs() < 1e-9, "{v:?} -> {m}");
        }
        // metric book: a bare number stays millimetres
        for v in [SheetCell::Text("3810".into()), SheetCell::Num(3810.0)] {
            let m = applied(len, Units::Metric, v.clone());
            assert!((m - 3.81).abs() < 1e-9, "{v:?} -> {m}");
        }
    }

    #[test]
    fn imperial_text_area_and_volume_read_in_header_units() {
        crate::units::set_ui_decimal_mark(crate::units::DecimalMark::Point);
        let i = Units::Imperial;
        let m2 = applied(ddc_model::ParamType::Area, i, SheetCell::Text("200".into()));
        assert!((m2 - 200.0 * 0.3048 * 0.3048).abs() < 1e-9, "{m2}");
        let m2 = applied(ddc_model::ParamType::Area, i, SheetCell::Text("3.5 m2".into()));
        assert!((m2 - 3.5).abs() < 1e-12, "explicit SI kept: {m2}");
        let m3 = applied(ddc_model::ParamType::Volume, i, SheetCell::Text("10".into()));
        assert!((m3 - 10.0 * 0.3048f64.powi(3)).abs() < 1e-9, "{m3}");
        let m2 = applied(ddc_model::ParamType::Area, Units::Metric, SheetCell::Text("12.5".into()));
        assert_eq!(m2, 12.5);
    }

    #[test]
    fn unchanged_text_length_cell_is_no_change() {
        crate::units::set_ui_decimal_mark(crate::units::DecimalMark::Point);
        let u = Units::Imperial;
        let (mut e, pid) = with_param(ddc_model::ParamType::Length, u);
        let tb = build(&e, Cat::Walls);
        let pl = plan(&e, &tb, u, "Walls", &book(&tb, pid, u, 12.5)).unwrap();
        assert_eq!(apply(&mut e, &tb, &pl).done, 1, "{:?}", e.status);
        let tb = build(&e, Cat::Walls);
        for t in ["12.5", " 12.5 ", "12.50"] {
            let pl = plan(&e, &tb, u, "Walls", &book_cell(&tb, pid, u, SheetCell::Text(t.into()))).unwrap();
            assert_eq!((pl.changed(), pl.bad), (0, 0), "{t}: {:?}", pl.refusals);
            assert_eq!(pl.unchanged, 1, "{t}");
        }
    }

    #[test]
    fn text_length_decimal_comma_follows_the_ui_mark() {
        use crate::units::{set_ui_decimal_mark, DecimalMark};
        let (len, i) = (ddc_model::ParamType::Length, Units::Imperial);
        set_ui_decimal_mark(DecimalMark::Comma);
        let m = applied(len, i, SheetCell::Text("12,5".into()));
        assert!((m - 3.81).abs() < 1e-9, "ru comma: {m}");
        // English: an ambiguous comma is refused, never scaled
        set_ui_decimal_mark(DecimalMark::Point);
        let (e, pid) = with_param(len, i);
        let tb = build(&e, Cat::Walls);
        let pl = plan(&e, &tb, i, "Walls", &book_cell(&tb, pid, i, SheetCell::Text("12,5".into()))).unwrap();
        assert_eq!((pl.changed(), pl.bad), (0, 1), "{:?}", pl.refusals);
        set_ui_decimal_mark(DecimalMark::Comma);
    }

    #[test]
    fn apply_after_model_change_names_count_difference() {
        let mut e = walls();
        let (tb, p, sheet, mut grid) = exported(&e, "diff");
        let (a, b) = (tb.rows[0].id, tb.rows[1].id);
        let (jl, jh) = (col_of(&grid, &tb, "len"), col_of(&grid, &tb, "h"));
        let (ra, rb) = (row_of(&grid, a), row_of(&grid, b));
        grid[ra][jl] = SheetCell::Num(7000.0);
        grid[rb][jh] = SheetCell::Num(3500.0);
        let grid = resave(&p, &sheet, &grid);
        let shown = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert_eq!(shown.changed(), 2);
        // the model moves to one of the book values before Apply
        e.edit_wall(b, None, None, Some(3.5));
        let tb = build(&e, Cat::Walls);
        let now = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert_eq!(now.changed(), 1, "{:?}", now.refusals);
        let (msg, flag) = apply_say(&mut e, &tb, &now, shown.changed());
        assert!(flag);
        assert!(msg.contains('2') && msg.contains('1'), "{msg}");
        assert!(msg.ends_with(&crate::tr("sched_import.preview_diff", &[("p", "2"), ("n", "1"), ("d", "1")])), "{msg}");
        // same counts: no extra text
        let (same, flag) = say_after(&Outcome { done: 2, ..Outcome::default() }, 2, None, 2);
        assert!(!flag && !same.contains("2 cells changed"), "{same}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn failed_write_says_failure_not_zero_imported() {
        let failed = Outcome { bad: 3, ..Outcome::default() };
        let (msg, flag) = say_after(&failed, 3, Some("not changed: disk full".into()), 3);
        assert!(flag);
        assert_eq!(msg, "not changed: disk full");
        assert!(!msg.contains('0'), "{msg}");
        // a plan with nothing to write is not a failure
        let (msg, _) = say_after(&Outcome::default(), 0, None, 0);
        assert_eq!(msg, say(&Outcome::default()).0);
    }

    /// Wall `id` gets value `v` of parameter `pid` ("Zone"), then that value is locked.
    fn set_and_lock(e: &mut Editor, pid: i64, id: i64, v: f64) {
        let set = ddc_model::ParamSet {
            param: pid,
            kind: "wall".into(),
            id,
            value: Some(ddc_model::PVal::Num(v)),
        };
        e.model.set_param_values(&[set]).unwrap();
        e.model.set_param_lock(&[("wall", id, "Zone")]).unwrap();
        e.reload().unwrap();
    }

    #[test]
    fn locked_value_refused_in_preview_and_kept_on_apply() {
        let u = Units::Metric;
        let (mut e, pid) = with_param(ddc_model::ParamType::Number, u);
        let id = build(&e, Cat::Walls).rows[0].id;
        set_and_lock(&mut e, pid, id, 5.0);
        let tb = build(&e, Cat::Walls);
        let grid = book(&tb, pid, u, 9.0);
        let pl = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!((pl.changed(), pl.locked, pl.readonly, pl.bad), (0, 1, 0, 0), "{:?}", pl.refusals);
        let r = &pl.refusals[0];
        assert_eq!((r.why.clone(), r.row, r.col), (Why::Locked, 2, 2));
        assert!(pl.count_lines().iter().any(|l| l.contains('1') && !l.contains("sched_import.")));
        let (top, rev) = (e.model.op_top().ok(), e.revision);
        let o = apply(&mut e, &tb, &pl);
        assert_eq!((o.done, o.readonly), (0, 1));
        assert_eq!(param_val(&e, pid), 5.0, "locked value kept");
        assert_eq!((e.model.op_top().ok(), e.revision), (top, rev), "no op, no undo step");
        assert!(say(&o).1, "a locked cell is a skipped cell");
        // the book already showing the locked value is unchanged, not refused
        let grid = book(&tb, pid, u, 5.0);
        let pl = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!((pl.changed(), pl.locked, pl.unchanged), (0, 0, 1), "{:?}", pl.refusals);
    }

    #[test]
    fn lock_set_after_preview_is_respected_on_apply() {
        let u = Units::Metric;
        let (mut e, pid) = with_param(ddc_model::ParamType::Number, u);
        let tb = build(&e, Cat::Walls);
        let (a, b) = (tb.rows[0].id, tb.rows[1].id);
        let c = &tb.cols[tb.cols.iter().position(|c| c.key == format!("p:{pid}")).unwrap()];
        let grid = vec![
            vec![SheetCell::Text(ID_HEADER.to_string()), SheetCell::Text(header(c, u))],
            vec![SheetCell::Num(a as f64), SheetCell::Num(7.0)],
            vec![SheetCell::Num(b as f64), SheetCell::Num(8.0)],
        ];
        let shown = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!(shown.changed(), 2, "{:?}", shown.refusals);
        // the user locks one value while the preview is open; Apply plans again
        set_and_lock(&mut e, pid, a, 1.0);
        let tb = build(&e, Cat::Walls);
        let now = plan(&e, &tb, u, "Walls", &grid).unwrap();
        assert_eq!((now.changed(), now.locked), (1, 1), "{:?}", now.refusals);
        let u0 = e.undo_len();
        let (msg, flag) = apply_say(&mut e, &tb, &now, shown.changed());
        assert!(flag, "{msg}");
        assert_eq!(e.undo_len(), u0 + 1, "the unlocked cell: one undo step");
        let tb = build(&e, Cat::Walls);
        let k = tb.cols.iter().position(|c| c.key == format!("p:{pid}")).unwrap();
        let v = |id: i64| tb.rows.iter().find(|r| r.id == id).unwrap().vals[k].clone();
        assert_eq!((v(a), v(b)), (Val::N(1.0), Val::N(8.0)));
    }

    #[test]
    fn grouped_keyed_export_reads_back_unchanged() {
        use super::super::group;
        let e = walls();
        let tb = build(&e, Cat::Walls);
        let by = [tb.cols.iter().position(|c| c.kind == Kind::Text).unwrap()];
        let mut ord = order(&tb, &[], None, Units::Metric);
        group::sort(&tb, &mut ord, &by);
        let lines = group::lines(&tb, &ord, &by, true, Units::Metric);
        assert!(lines.iter().any(|l| matches!(l, group::Line::Head { .. })));
        let cols: Vec<usize> = (0..tb.cols.len()).collect();
        let p = tmp("grp");
        export_xlsx_keyed(&e.model, &tb, &ord, &cols, Units::Metric, &p, &lines).unwrap();
        let (sheet, grid) = read_sheet_cells(&p, None).unwrap();
        assert!(is_id_head(&grid[0][0]), "{:?}", grid[0]);
        let pl = plan(&e, &tb, Units::Metric, &sheet, &grid).unwrap();
        assert!(pl.refusals.is_empty(), "group and subtotal lines are not elements: {:?}", pl.refusals);
        assert_eq!((pl.changed(), pl.rows), (0, tb.rows.len()));
        let _ = std::fs::remove_file(&p);
    }

    fn cells(grid: &[Vec<SheetCell>]) -> Vec<Vec<TableCell>> {
        let to = |c: &SheetCell| match c {
            SheetCell::Num(x) => TableCell::Num(*x),
            other => TableCell::Text(shown(other)),
        };
        grid.iter().map(|r| r.iter().map(to).collect()).collect()
    }

    /// Save an edited grid as the user's book, keeping the hidden key sheet of the export.
    fn resave_keyed(p: &std::path::Path, sheet: &str, grid: &[Vec<SheetCell>]) -> Vec<Vec<SheetCell>> {
        let (name, key) = read_sheet_cells(p, Some(ddc_io::xlsx::KEY_SHEET)).unwrap();
        assert_eq!(name, ddc_io::xlsx::KEY_SHEET);
        let headers: Vec<String> = grid[0].iter().map(shown).collect();
        let rows = cells(&grid[1..]);
        let lines: Vec<ddc_io::xlsx::TableLine> = (0..rows.len()).map(ddc_io::xlsx::TableLine::Row).collect();
        ddc_io::xlsx::export_keyed_table_lines_with_key(p, sheet, &headers, &[], &rows, &lines, None, &cells(&key))
            .unwrap();
        read_sheet_cells(p, Some(sheet)).unwrap().1
    }

    fn set_uid(e: &Editor, uid: i64) {
        e.model.sql().execute("DELETE FROM meta WHERE key='project_uid'", []).unwrap();
        e.model.sql().execute("INSERT INTO meta(key, value) VALUES('project_uid', ?1)", [uid]).unwrap();
    }

    #[test]
    fn key_sheet_names_project_and_head() {
        let e = walls();
        set_uid(&e, 4101);
        let (tb, p, _, _) = exported(&e, "key");
        let k = Key::read(&p).expect("keyed export writes the key sheet");
        assert_eq!(k.project, Some(4101));
        assert_eq!(k.head, Some(e.model.change_head()));
        assert_eq!(k.rows.len(), tb.rows.len());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn model_edit_after_export_survives_unchanged_book() {
        let mut e = walls();
        set_uid(&e, 4102);
        let (_, p, sheet, grid) = exported(&e, "kept");
        let b = build(&e, Cat::Walls).rows[1].id;
        // the editor changes the height after the export; the book still says the old one
        e.edit_wall(b, None, None, Some(3.2));
        assert_eq!(val(&e, b, "h"), Val::N(3.2));
        let tb = build(&e, Cat::Walls);
        let key = Key::read(&p);
        let pl = plan_keyed(&e, &tb, Units::Metric, &sheet, &grid, key.as_ref()).unwrap();
        assert_eq!((pl.changed(), pl.conflicts), (0, 0), "{:?} {:?}", pl.cells, pl.refusals);
        assert!(pl.kept >= 1, "the moved height counts as kept");
        assert!(pl.warn.is_none(), "{:?}", pl.warn);
        assert!(pl.count_lines().contains(&crate::tr("sched_import.n_kept", &[("n", &pl.kept.to_string())])));
        let (u0, rev) = (e.undo_len(), e.revision);
        let o = apply(&mut e, &tb, &pl);
        assert_eq!(o.done, 0);
        assert_eq!((e.undo_len(), e.revision), (u0, rev), "nothing written");
        assert_eq!(val(&e, b, "h"), Val::N(3.2), "model edit kept");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn both_changed_is_conflict_book_only_change_applied_one_undo() {
        let mut e = walls();
        set_uid(&e, 4103);
        let (tb0, p, sheet, mut grid) = exported(&e, "conf");
        let (a, b) = (tb0.rows[0].id, tb0.rows[1].id);
        let (jl, jh) = (col_of(&grid, &tb0, "len"), col_of(&grid, &tb0, "h"));
        let (ra, rb) = (row_of(&grid, a), row_of(&grid, b));
        grid[ra][jl] = SheetCell::Num(7000.0);
        grid[rb][jh] = SheetCell::Num(3500.0);
        let grid = resave_keyed(&p, &sheet, &grid);
        e.edit_wall(b, None, None, Some(3.2));
        let len_a = val(&e, a, "len");
        let tb = build(&e, Cat::Walls);
        let key = Key::read(&p);
        let pl = plan_keyed(&e, &tb, Units::Metric, &sheet, &grid, key.as_ref()).unwrap();
        assert_eq!((pl.changed(), pl.conflicts), (1, 1), "{:?}", pl.refusals);
        let f = pl.refusals.iter().find(|r| matches!(r.why, Why::Conflict { .. })).unwrap();
        assert_eq!((f.row, f.col), (rb as u32 + 1, jh as u32 + 1));
        assert!(f.line(&sheet).contains("3500"), "{}", f.line(&sheet));
        let u0 = e.undo_len();
        let o = apply(&mut e, &tb, &pl);
        assert_eq!(o.done, 1, "{:?}", e.status);
        assert!(say(&o).1, "a conflict is a skipped cell");
        assert_eq!(e.undo_len(), u0 + 1, "one Apply, one undo step");
        assert!((len(&e, a) - 7.0).abs() < 1e-9, "book-only edit applied");
        assert_eq!(val(&e, b, "h"), Val::N(3.2), "conflict never overwrites the model");
        e.undo();
        assert_eq!(val(&e, a, "len"), len_a);
        assert_eq!(val(&e, b, "h"), Val::N(3.2));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn book_of_another_project_refused_before_any_row() {
        let e = walls();
        set_uid(&e, 4104);
        let (tb, p, sheet, mut grid) = exported(&e, "foreign");
        let jh = col_of(&grid, &tb, "h");
        let ra = row_of(&grid, tb.rows[0].id);
        grid[ra][jh] = SheetCell::Num(3500.0);
        let grid = resave_keyed(&p, &sheet, &grid);
        set_uid(&e, 9999);
        let key = Key::read(&p);
        let r = plan_keyed(&e, &tb, Units::Metric, &sheet, &grid, key.as_ref());
        assert_eq!(r.err(), Some(crate::tr("xlsx_project.title", &[])));
        // the same book back in its own project reads fine
        set_uid(&e, 4104);
        let pl = plan_keyed(&e, &tb, Units::Metric, &sheet, &grid, key.as_ref()).unwrap();
        assert_eq!(pl.changed(), 1, "{:?}", pl.refusals);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn book_without_key_or_project_warns_and_applies_two_way() {
        let mut e = walls();
        let (tb, p, sheet, mut grid) = exported(&e, "nokey");
        let a = tb.rows[0].id;
        let jl = col_of(&grid, &tb, "len");
        let r = row_of(&grid, a);
        grid[r][jl] = SheetCell::Num(7000.0);
        // an older book: no key sheet
        let grid = resave(&p, &sheet, &grid);
        let key = Key::read(&p);
        assert!(key.is_none());
        let pl = plan_keyed(&e, &tb, Units::Metric, &sheet, &grid, key.as_ref()).unwrap();
        assert_eq!(pl.warn, Some(crate::tr("sched_import.no_key", &[])));
        assert_eq!(pl.changed(), 1, "{:?}", pl.refusals);
        let u0 = e.undo_len();
        assert_eq!(apply(&mut e, &tb, &pl).done, 1, "{:?}", e.status);
        assert_eq!(e.undo_len(), u0 + 1);
        assert!((len(&e, a) - 7.0).abs() < 1e-9);
        // a key that names no project: the note is still read
        let mut k = Key::from_grid(&[vec![SheetCell::Text(ID_HEADER.into())]]).unwrap();
        k.project = None;
        let tb = build(&e, Cat::Walls);
        let pl = plan_keyed(&e, &tb, Units::Metric, &sheet, &grid, Some(&k)).unwrap();
        assert_eq!(pl.warn, Some(crate::tr("xlsx_project.no_project", &[])));
        let _ = std::fs::remove_file(&p);
    }

    /// Plan as the schedule window does: the book's key plus what was applied from it.
    fn again(e: &Editor, p: &std::path::Path, sheet: &str, grid: &[Vec<SheetCell>], a: &Applied) -> (Table, Plan) {
        let tb = build(e, Cat::Walls);
        let key = Key::read(p).map(|k| k.with_applied(Some(a)));
        assert!(key.is_some(), "keyed book");
        let pl = plan_keyed(e, &tb, Units::Metric, sheet, grid, key.as_ref()).unwrap();
        (tb, pl)
    }

    /// Apply and remember the base, as `import_apply` does; the count written.
    fn apply_rec(e: &mut Editor, tb: &Table, pl: &Plan, a: &mut Applied) -> usize {
        let o = apply(e, tb, pl);
        a.record(pl);
        o.done
    }

    fn hgt(e: &Editor, id: i64) -> f64 {
        match val(e, id, "h") {
            Val::N(x) => x,
            v => panic!("{v:?}"),
        }
    }

    /// A keyed book of one wall whose height cell the user edits: (path, sheet, grid, row, col, wall).
    fn height_book(e: &Editor, uid: i64, tag: &str) -> (std::path::PathBuf, String, Vec<Vec<SheetCell>>, usize, usize, i64) {
        set_uid(e, uid);
        let (tb, p, sheet, grid) = exported(e, tag);
        let b = tb.rows[0].id;
        let (r, j) = (row_of(&grid, b), col_of(&grid, &tb, "h"));
        (p, sheet, grid, r, j, b)
    }

    fn put(p: &std::path::Path, sheet: &str, grid: &mut [Vec<SheetCell>], r: usize, j: usize, v: SheetCell) -> Vec<Vec<SheetCell>> {
        grid[r][j] = v;
        resave_keyed(p, sheet, grid)
    }

    #[test]
    fn value_put_back_after_apply_is_a_change() {
        let mut e = walls();
        let (p, sheet, mut grid, r, j, b) = height_book(&e, 4601, "revert");
        let (b0, h0) = (grid[r][j].clone(), hgt(&e, b));
        let mut a = Applied::default();
        let g1 = put(&p, &sheet, &mut grid, r, j, SheetCell::Num(3500.0));
        let (tb, pl) = again(&e, &p, &sheet, &g1, &a);
        assert_eq!(pl.changed(), 1, "{:?}", pl.refusals);
        let file = std::fs::read(&p).unwrap();
        assert_eq!(apply_rec(&mut e, &tb, &pl, &mut a), 1, "{:?}", e.status);
        assert_eq!(std::fs::read(&p).unwrap(), file, "Apply never rewrites the user's book");
        assert!((hgt(&e, b) - 3.5).abs() < 1e-9);
        let g0 = put(&p, &sheet, &mut grid, r, j, b0);
        // the export key alone takes the old value for "model moved since": the silent drop
        let (_, stale) = again(&e, &p, &sheet, &g0, &Applied::default());
        assert_eq!(stale.changed(), 0);
        assert!(stale.kept >= 1);
        let (tb, pl) = again(&e, &p, &sheet, &g0, &a);
        assert_eq!((pl.changed(), pl.conflicts), (1, 0), "{:?}", pl.refusals);
        let kh = tb.cols.iter().position(|c| c.key == "h").unwrap();
        assert_eq!(pl.cells[0].1, kh, "the height cell is the change");
        let u0 = e.undo_len();
        assert_eq!(apply_rec(&mut e, &tb, &pl, &mut a), 1, "{:?}", e.status);
        assert_eq!(e.undo_len(), u0 + 1, "one Apply, one undo step");
        assert!((hgt(&e, b) - h0).abs() < 1e-9, "the model is back at the exported value");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn same_book_after_apply_is_no_change() {
        let mut e = walls();
        let (p, sheet, mut grid, r, j, b) = height_book(&e, 4602, "same_apply");
        let mut a = Applied::default();
        let g1 = put(&p, &sheet, &mut grid, r, j, SheetCell::Num(3500.0));
        let (tb, pl) = again(&e, &p, &sheet, &g1, &a);
        assert_eq!(apply_rec(&mut e, &tb, &pl, &mut a), 1, "{:?}", e.status);
        let (tb, pl) = again(&e, &p, &sheet, &g1, &a);
        assert_eq!((pl.changed(), pl.conflicts), (0, 0), "{:?}", pl.refusals);
        let (u0, rev) = (e.undo_len(), e.revision);
        assert_eq!(apply_rec(&mut e, &tb, &pl, &mut a), 0);
        assert_eq!((e.undo_len(), e.revision), (u0, rev), "a true no-op writes nothing");
        assert!((hgt(&e, b) - 3.5).abs() < 1e-9);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn successive_applies_each_take_the_new_value() {
        let mut e = walls();
        let (p, sheet, mut grid, r, j, b) = height_book(&e, 4603, "twice");
        let (b0, h0) = (grid[r][j].clone(), hgt(&e, b));
        let mut a = Applied::default();
        for (cell, want) in [(SheetCell::Num(3500.0), 3.5), (SheetCell::Num(3800.0), 3.8), (b0, h0)] {
            let g = put(&p, &sheet, &mut grid, r, j, cell);
            let (tb, pl) = again(&e, &p, &sheet, &g, &a);
            assert_eq!((pl.changed(), pl.conflicts), (1, 0), "{want}: {:?}", pl.refusals);
            assert_eq!(apply_rec(&mut e, &tb, &pl, &mut a), 1, "{:?}", e.status);
            assert!((hgt(&e, b) - want).abs() < 1e-9, "{want}");
        }
        // undo of the last Apply: the model holds 3.8 again and the book's original reads as an edit
        e.undo();
        assert!((hgt(&e, b) - 3.8).abs() < 1e-9);
        let (_, pl) = again(&e, &p, &sheet, &grid, &a);
        assert_eq!((pl.changed(), pl.conflicts), (1, 0), "{:?}", pl.refusals);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn model_edit_after_apply_still_a_conflict() {
        let mut e = walls();
        let (p, sheet, mut grid, r, j, b) = height_book(&e, 4604, "medit");
        let mut a = Applied::default();
        let g1 = put(&p, &sheet, &mut grid, r, j, SheetCell::Num(3500.0));
        let (tb, pl) = again(&e, &p, &sheet, &g1, &a);
        assert_eq!(apply_rec(&mut e, &tb, &pl, &mut a), 1, "{:?}", e.status);
        e.edit_wall(b, None, None, Some(3.2));
        for g in [g1.clone(), put(&p, &sheet, &mut grid, r, j, SheetCell::Num(3800.0))] {
            let (tb, pl) = again(&e, &p, &sheet, &g, &a);
            assert_eq!((pl.changed(), pl.conflicts), (0, 1), "{:?}", pl.refusals);
            let f = pl.refusals.iter().find(|f| matches!(f.why, Why::Conflict { .. })).unwrap();
            assert_eq!((f.row, f.col), (r as u32 + 1, j as u32 + 1));
            let u0 = e.undo_len();
            assert_eq!(apply(&mut e, &tb, &pl).done, 0);
            assert_eq!(e.undo_len(), u0);
            assert_eq!(val(&e, b, "h"), Val::N(3.2), "the model edit stays");
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn undo_of_apply_offers_the_book_again() {
        let mut e = walls();
        let (p, sheet, mut grid, r, j, b) = height_book(&e, 4605, "undo");
        let (b0, h0) = (grid[r][j].clone(), hgt(&e, b));
        let mut a = Applied::default();
        let g1 = put(&p, &sheet, &mut grid, r, j, SheetCell::Num(3500.0));
        let (tb, pl) = again(&e, &p, &sheet, &g1, &a);
        let u0 = e.undo_len();
        assert_eq!(apply_rec(&mut e, &tb, &pl, &mut a), 1, "{:?}", e.status);
        assert_eq!(e.undo_len(), u0 + 1);
        e.undo();
        assert!((hgt(&e, b) - h0).abs() < 1e-9, "undo restores the model");
        assert_eq!(e.undo_len(), u0);
        // the undone value is offered again, not taken for a newer model edit
        let (_, pl) = again(&e, &p, &sheet, &g1, &a);
        assert_eq!((pl.changed(), pl.kept, pl.conflicts), (1, 0, 0), "{:?}", pl.refusals);
        // the book put back to the export matches the model: nothing to do
        let g0 = put(&p, &sheet, &mut grid, r, j, b0);
        let (_, none) = again(&e, &p, &sheet, &g0, &a);
        assert_eq!((none.changed(), none.conflicts), (0, 0), "{:?}", none.refusals);
        e.redo();
        assert!((hgt(&e, b) - 3.5).abs() < 1e-9, "redo repeats the Apply");
        let (tb, back) = again(&e, &p, &sheet, &g0, &a);
        assert_eq!((back.changed(), back.conflicts), (1, 0), "{:?}", back.refusals);
        assert_eq!(apply_rec(&mut e, &tb, &back, &mut a), 1, "{:?}", e.status);
        assert!((hgt(&e, b) - h0).abs() < 1e-9);
        let _ = std::fs::remove_file(&p);
    }
}
