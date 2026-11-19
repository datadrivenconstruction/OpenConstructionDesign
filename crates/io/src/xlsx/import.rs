//! Import of an edited workbook: parse → validate all cells → one model operation.
//! Any rejection = nothing applied; the report lists all rejections at once.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{
    CurtainSpec, Edit, GridLines, LayerSpec, Model, MullionSection, OpeningSpec, RoomSpec,
    SlabSpec, Swing, WallSpec,
};

use std::rc::Rc;

use super::baseline::{self, Base, BVal, KeptCell};
use super::export::ring_text;
use super::units::{self, Dim};

use super::*;

const EPS: f64 = 1e-9;

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= EPS * (1.0 + a.abs().max(b.abs()))
}

fn bool_text(b: bool) -> &'static str {
    if b {
        "TRUE"
    } else {
        "FALSE"
    }
}

pub(super) fn blank(d: &Data) -> bool {
    match d {
        Data::Empty => true,
        Data::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

struct Tab {
    name: &'static str,
    /// Header row, 1-based.
    r0: u32,
    c0: u32,
    head: Vec<String>,
    idx: HashMap<&'static str, usize>,
    headers: HashMap<&'static str, String>,
    factors: HashMap<&'static str, f64>,
    dims: HashMap<&'static str, Dim>,
    rows: Vec<(u32, Vec<Data>)>,
    /// Values as exported, if the book carries them.
    base: Option<Rc<Base>>,
    /// Further columns the parameter reader skips (the family layout's own).
    also_known: HashSet<usize>,
    /// First row of a merge down the id column: rows are hidden, so nothing is deleted.
    merged_id: Option<u32>,
    /// (row, cell index) under a merge that were not given: an existing element keeps its value.
    covered: HashSet<(u32, usize)>,
    /// (row, cell index) of calculated formulas in the format's columns: never a model edit.
    formulas: HashSet<(u32, usize)>,
}

type Book = Xlsx<std::io::BufReader<std::fs::File>>;

/// Sheet name without case, spaces and a trailing copy suffix such as " (2)".
fn sheet_key(n: &str) -> String {
    let t = n.trim_end();
    let t = match t.strip_suffix(')').and_then(|h| h.rsplit_once('(')) {
        Some((head, d)) if !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()) => head,
        _ => t,
    };
    t.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// Sheets of the book that are not `name` but look like it.
fn look_alikes(have: &[String], name: &str) -> Vec<String> {
    let key = sheet_key(name);
    have.iter().filter(|n| n.as_str() != name && sheet_key(n) == key).cloned().collect()
}

/// What every sheet read of one import shares.
struct Reading<'a> {
    alias: &'a HashMap<String, String>,
    /// Elements the model holds per sheet: what an emptied sheet is measured against when the
    /// book has no baseline to tell.
    live: &'a HashMap<&'static str, HashSet<i64>>,
}

/// Elements `sheet` stands for: the rows the export wrote, or without that record what the model
/// holds (`live`). A scoped book without a record cannot tell: its sheet may rightly have no row.
/// With the record only exported ids the model still holds count: one deleted and one new
/// element since the export keep one. `live` is None for a sheet whose ids are not taken.
pub(super) fn sheet_held(
    base: Option<&Base>,
    rep: &Report,
    sheet: &str,
    live: Option<&HashSet<i64>>,
) -> usize {
    match base {
        Some(b) if b.has_sheet(sheet) => {
            let wrote = b.ids(sheet);
            live.map_or(wrote.len(), |now| wrote.iter().filter(|id| now.contains(id)).count())
        }
        _ if !rep.scope.is_empty() || !rep.scope_spec.is_empty() => 0,
        _ => live.map_or(0, HashSet::len),
    }
}

fn load(
    wb: &mut Book,
    name: &'static str,
    cols: &'static [Col],
    u: Units,
    rep: &mut Report,
    base: &Option<Rc<Base>>,
    al: &Reading<'_>,
) -> Option<Tab> {
    let have = wb.sheet_names();
    // the sheet to read: `name`, or the one the user picked for it in this import
    let actual = super::sheet_alias::target(&have, al.alias, name);
    if !have.iter().any(|n| *n == actual) {
        let found = look_alikes(&have, name);
        if !found.is_empty() {
            rep.look_alikes.push(super::LookAlike { expected: name.into(), found });
        }
        let keys = super::book::read_keys(wb);
        let found = super::sheet_alias::find(wb, &have, name, cols, &keys, al.alias);
        if !found.is_empty() {
            rep.candidates.push(super::SheetCandidate { expected: name.into(), found });
        }
        return None;
    }
    let keys = super::book::read_keys(wb);
    let held = sheet_held(base.as_deref(), rep, name, al.live.get(name));
    let issue = |rep: &mut Report, kind| rep.sheet_issues.push(SheetIssue::new(name, kind, held));
    let Ok(range) = wb.worksheet_range(&actual) else {
        issue(rep, SheetIssueKind::Unreadable);
        return None;
    };
    // without its formulas a calculated cell would read as an edit: the sheet is not read at all
    let Ok(forms) = wb.worksheet_formula(&actual) else {
        issue(rep, SheetIssueKind::FormulasUnreadable);
        return None;
    };
    if forms.is_empty() && range.rows().all(|r| r.iter().all(blank)) {
        issue(rep, SheetIssueKind::Empty);
        return None;
    }
    let marks = (rep.rejected.len(), rep.ignored.len(), rep.warnings.len());
    let (r0, c0) = range.start().unwrap_or((0, 0));
    let known: Vec<String> = cols.iter().map(|c| units::norm(c.header)).collect();
    let h = super::book::find_header(&range, &known, |row| {
        let texts = super::book::resolve(&keys, name, cols, c0 as usize, row).texts;
        texts.iter().map(|t| units::norm(t)).collect()
    });
    let r0 = r0 + h as u32;
    let mut it = range.rows().skip(h);
    let head: Vec<String> = it
        .next()?
        .iter()
        .map(|d| d.to_string().trim().to_string())
        .collect();
    let mut idx = HashMap::new();
    let mut headers = HashMap::new();
    let mut factors = HashMap::new();
    let mut dims = HashMap::new();
    // the key row names a column by its exported caption, wherever the user moved it
    let heads = super::book::resolve(&keys, name, cols, c0 as usize, &head);
    if let Some(why) = &heads.refused {
        rep.rejected.push(Rejection {
            sheet: name.into(),
            row: r0 + 1,
            col: 0,
            column: String::new(),
            reason: why.clone(),
        });
    }
    let mut normed: Vec<String> = heads.texts.iter().map(|h| units::norm(h)).collect();
    for (i, h) in &heads.copies {
        normed[*i] = units::norm(h);
    }
    let is_copy = |i: usize| heads.copies.iter().any(|c| c.0 == i);
    // the family reader owns the format columns of that layout, copies included
    let family_layout = param_sheet(name).is_some() && super::extra::is_family_layout(&head);
    let mut copied: Vec<super::dup_cols::Group> = vec![];
    let mut also_known: HashSet<usize> = HashSet::new();
    for col in cols {
        let d = units::dim(col.header);
        dims.insert(col.key, d);
        let want = units::norm(col.header);
        let all: Vec<usize> = (0..normed.len()).filter(|&i| normed[i] == want).collect();
        match all.iter().copied().find(|&i| !is_copy(i)).or(all.first().copied()) {
            Some(i) => {
                idx.insert(col.key, i);
                let actual = &head[i];
                headers.insert(col.key, actual.clone());
                let k = match units::header_factor(actual, d) {
                    Some(k) => k,
                    None if d != Dim::None && actual.contains('[') => {
                        rep.rejected.push(Rejection {
                            sheet: name.into(),
                            row: r0 + 1,
                            col: c0 + i as u32 + 1,
                            column: actual.clone(),
                            reason: "unknown unit in header (use m, mm, cm, ft, in)".into(),
                        });
                        1.0
                    }
                    None => u.factor(col.header),
                };
                factors.insert(col.key, k);
                if all.len() > 1 && !family_layout {
                    let of = |j: usize| {
                        units::header_factor(&head[j], d).unwrap_or_else(|| u.factor(col.header))
                    };
                    let copies: Vec<(usize, f64)> =
                        all.iter().filter(|&&j| j != i).map(|&j| (j, of(j))).collect();
                    also_known.extend(copies.iter().map(|c| c.0));
                    copied.push(super::dup_cols::Group {
                        keep: (i, k),
                        copies,
                        names_row: matches!(col.key, "id" | "wtype" | "stype" | "pos"),
                    });
                }
            }
            None => {
                let shown = u.header(col.header);
                headers.insert(col.key, shown.clone());
                factors.insert(col.key, u.factor(col.header));
                let optional =
                    matches!(col.key, "delete" | "offset0" | "swing" | "hinge" | "bulge")
                        || (name == WALLS && col.key == "base_offset")
                        || (name == OPENINGS && col.key == "sill")
                        || (name == ROOMS && matches!(col.key, "px" | "py" | "height"))
                        || (name == SLABS && col.key == "outline")
                        || (name == MATERIALS && !matches!(col.key, "id" | "name"))
                        || name == CURTAINS
                        || param_sheet(name).is_some();
                let computed = !col.edit && !matches!(col.key, "id" | "wtype" | "pos");
                let gone = heads.missing.iter().find(|(k, _)| k == col.key);
                if !optional && !computed {
                    rep.rejected.push(Rejection {
                        sheet: name.into(),
                        row: r0 + 1,
                        col: 0,
                        column: gone.map_or(shown, |g| g.1.clone()),
                        reason: "required column missing in header".into(),
                    });
                } else if let (Some((_, cap)), true) = (gone, col.edit) {
                    rep.ignored.push(Ignored {
                        sheet: name.into(),
                        row: r0 + 1,
                        col: 0,
                        column: cap.clone(),
                        id: None,
                        old: String::new(),
                        new: String::new(),
                        reason: "column is gone from the sheet; its values stay as they are".into(),
                    });
                }
            }
        }
    }
    let total = super::book::total_row(wb, &actual);
    let groups = super::book::group_rows(wb, &actual);
    let mut rows: Vec<(u32, Vec<Data>)> = it
        .enumerate()
        .map(|(i, r)| (r0 + i as u32 + 2, r.to_vec()))
        .collect();
    let merged = super::merge::regions(wb, &actual);
    let mg = super::merge::apply(name, r0, c0, &head, idx.get("id").copied(), &merged, &mut rows, rep);
    rows.retain(|(r, _)| Some(*r) != total && !groups.contains(r));
    // only a header left where the export wrote rows: a wiped sheet, not an order to delete them
    if held > 0 && forms.is_empty() && rows.iter().all(|(_, r)| r.iter().all(blank)) {
        rep.rejected.truncate(marks.0);
        rep.ignored.truncate(marks.1);
        rep.warnings.truncate(marks.2);
        issue(rep, SheetIssueKind::Empty);
        return None;
    }
    let mut covered = mg.covered;
    let mut formulas = HashSet::new();
    uncalculated(&forms, &actual, c0, &head, &idx, &rows, (&mut covered, &mut formulas), rep);
    let sheet = super::dup_cols::Sheet { name, r0: r0 + 1, c0, head: &head, rows: &rows };
    super::dup_cols::check(&sheet, &copied, &mut covered, rep);
    Some(Tab {
        formulas,
        name,
        r0: r0 + 1,
        c0,
        head,
        idx,
        headers,
        factors,
        dims,
        rows,
        base: base.clone(),
        also_known,
        merged_id: mg.id_row,
        covered,
    })
}

/// Where the import reads the ids of `name`: the sheet of the book (its alias when picked), the
/// 0-based id column and the 1-based data rows. None without an id column or with merged ids.
pub(crate) fn id_column(
    wb: &mut Book,
    name: &'static str,
    cols: &'static [Col],
    alias: &HashMap<String, String>,
) -> Option<(String, u32, Vec<u32>)> {
    let mut rep = Report::default();
    let live = HashMap::new();
    let al = Reading { alias, live: &live };
    let tab = load(wb, name, cols, Units::default(), &mut rep, &None, &al)?;
    if tab.merged_id.is_some() {
        return None;
    }
    let i = *tab.idx.get("id")?;
    let actual = super::sheet_alias::target(&wb.sheet_names(), alias, name);
    Some((actual, tab.c0 + i as u32, tab.rows.iter().map(|r| r.0).collect()))
}

/// A formula in a model column whose result was never calculated (a script or an app that does
/// not recalculate wrote the book) reads as blank. It is refused instead, naming the cell, and
/// the cell is covered so the element keeps its value. In the id column the row cannot be told
/// from a missing one, so that refusal stops the import. A calculated formula outside the id
/// column goes to `formulas`: its result is never applied as an edit of the element.
#[allow(clippy::too_many_arguments)]
fn uncalculated(
    forms: &calamine::Range<String>,
    name: &str,
    c0: u32,
    head: &[String],
    idx: &HashMap<&'static str, usize>,
    rows: &[(u32, Vec<Data>)],
    (covered, formulas): (&mut HashSet<(u32, usize)>, &mut HashSet<(u32, usize)>),
    rep: &mut Report,
) {
    let known: HashSet<usize> = idx.values().copied().collect();
    let id_col = idx.get("id").copied();
    for (rno, cells) in rows {
        for (i, d) in cells.iter().enumerate() {
            if !known.contains(&i) {
                continue;
            }
            let Some(f) = forms.get_value((rno - 1, c0 + i as u32)) else {
                continue;
            };
            let f = f.trim().trim_start_matches('=');
            if f.is_empty() {
                continue;
            }
            // only a cell with no cached result at all; a calculated "" (=IF(..,"")) is a value
            if !matches!(d, Data::Empty) {
                // an error result keeps its own refusal ("formula error ...")
                if Some(i) != id_col && !matches!(d, Data::Error(_)) {
                    formulas.insert((*rno, i));
                }
                continue;
            }
            let col = c0 + i as u32 + 1;
            let at = cell_ref(name, *rno, col);
            let reason = super::msg::fill_template(super::msg::NO_CALC.1, &[&at, &format!("={f}")]);
            covered.insert((*rno, i));
            rep.rejected.push(Rejection {
                sheet: name.into(),
                row: if Some(i) == id_col { 0 } else { *rno },
                col,
                column: head.get(i).cloned().unwrap_or_default(),
                reason,
            });
        }
    }
}

impl Tab {
    fn view(&self) -> super::extra::SheetView<'_> {
        super::extra::SheetView {
            name: self.name,
            r0: self.r0,
            c0: self.c0,
            head: &self.head,
            known: self.idx.values().chain(&self.also_known).copied().collect(),
            rows: &self.rows,
            base: self.base.as_deref(),
            covered: &self.covered,
        }
    }
}

/// One sheet row + an "all cells valid" flag.
struct Row<'a> {
    tab: &'a Tab,
    r: u32,
    cells: &'a [Data],
    ok: bool,
    id: Option<i64>,
}

impl<'a> Row<'a> {
    fn new(tab: &'a Tab, r: u32, cells: &'a [Data]) -> Self {
        Row {
            tab,
            r,
            cells,
            ok: true,
            id: None,
        }
    }

    fn empty(&self) -> bool {
        self.cells.iter().all(blank)
    }

    /// Trimmed text of a key cell (None when blank).
    fn key_text(&self, key: &str) -> Option<String> {
        let t = self.cell(key)?.to_string().trim().to_string();
        (!t.is_empty()).then_some(t)
    }

    /// The column is on the sheet and this row's cell is not covered by a merge.
    fn has(&self, key: &str) -> bool {
        self.tab.idx.contains_key(key) && !self.covered(key)
    }

    fn covered(&self, key: &str) -> bool {
        self.tab.idx.get(key).is_some_and(|i| self.tab.covered.contains(&(self.r, *i)))
    }

    fn factor(&self, key: &str) -> f64 {
        self.tab.factors.get(key).copied().unwrap_or(1.0)
    }

    fn cell(&self, key: &str) -> Option<&Data> {
        let i = *self.tab.idx.get(key)?;
        if self.tab.covered.contains(&(self.r, i)) {
            return None;
        }
        self.cells.get(i).filter(|d| !blank(d))
    }

    fn header(&self, key: &str) -> String {
        self.tab
            .headers
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.into())
    }

    /// The row is left out: its cells are not applied, the rest of the book still is.
    fn reject(&mut self, rep: &mut Report, key: &str, reason: impl Into<String>) {
        self.ok = false;
        let (sheet, r) = (self.tab.name, self.r);
        rep.changes.retain(|c| !(c.sheet == sheet && c.row == r));
        let col = self
            .tab
            .idx
            .get(key)
            .map_or(0, |i| self.tab.c0 + *i as u32 + 1);
        rep.rejected.push(Rejection {
            sheet: self.tab.name.into(),
            row: self.r,
            col,
            column: self.header(key),
            reason: reason.into(),
        });
    }

    /// The cell is accepted, but the preview says what will happen to it.
    fn warn(&self, rep: &mut Report, key: &str, reason: impl Into<String>) {
        rep.warnings.push(Rejection {
            sheet: self.tab.name.into(),
            row: self.r,
            col: self.col_of(key),
            column: self.header(key),
            reason: reason.into(),
        });
    }

    /// A layer material naming no Materials row and no material in use becomes a new one.
    fn warn_new_material(&self, rep: &mut Report, material: &str, cur: &str, known: &HashSet<String>) {
        if material != cur && !known.contains(material) {
            let t = format!("material {material} is not in Materials: a new material will be used");
            self.warn(rep, "material", t);
        }
    }

    /// A bad value. In an existing row only this cell is skipped and the model value stays;
    /// a new row (or an unreadable id) is left out whole.
    fn bad(&mut self, rep: &mut Report, key: &str, reason: impl Into<String>) {
        if self.id.is_none() {
            self.reject(rep, key, reason);
            return;
        }
        rep.rejected.push(Rejection {
            sheet: self.tab.name.into(),
            row: self.r,
            col: self.col_of(key),
            column: self.header(key),
            reason: reason.into(),
        });
    }

    /// A value outside its range: the cell is skipped in an existing row (the model value is
    /// returned), the row in a new one.
    fn in_range(&mut self, rep: &mut Report, key: &str, v: f64, cur: Option<f64>, valid: bool, why: String) -> f64 {
        if valid {
            return v;
        }
        match cur {
            Some(c) if self.id.is_some() => {
                let (sheet, r, col) = (self.tab.name, self.r, self.col_of(key));
                rep.changes.retain(|ch| !(ch.sheet == sheet && ch.row == r && ch.col == col));
                self.bad(rep, key, why);
                c
            }
            _ => {
                self.reject(rep, key, why);
                v
            }
        }
    }

    fn col_of(&self, key: &str) -> u32 {
        self.tab
            .idx
            .get(key)
            .map_or(0, |i| self.tab.c0 + *i as u32 + 1)
    }

    /// The row is left out of the import with a reason; the rest of the book still applies.
    fn skip(&self, rep: &mut Report, key: &str, reason: String) {
        rep.ignored.push(Ignored {
            sheet: self.tab.name.into(),
            row: self.r,
            col: self.col_of(key),
            column: self.header(key),
            id: self.id,
            old: String::new(),
            new: String::new(),
            reason,
        });
    }

    /// The element of this row is gone from the model (deleted after the export).
    fn stale(&mut self, rep: &mut Report, what: &str, id: i64) {
        self.id = Some(id);
        self.skip(rep, "id", format!("{what} {id} no longer exists; row ignored"));
    }

    fn dup(&mut self, rep: &mut Report, id: i64) {
        self.id = Some(id);
        let why = format!("duplicate id {id}; row ignored (clear the id to create a new element)");
        self.skip(rep, "id", why);
    }

    fn base_val(&self, key: &str) -> Option<&BVal> {
        self.tab.base.as_ref()?.get(self.tab.name, self.id?, key)
    }

    /// A book number that differs from the model: `true` if it is a book edit. A book cell
    /// equal to the export is not an edit (the model moved on); a cell edited in the book while
    /// the model value also changed is a conflict, reported and never applied.
    /// The cell holds a formula: its result differs from the model, but it is not the user's
    /// edit of the element. Listed, never applied; `true` when so.
    fn formula_left(&self, rep: &mut Report, key: &str, old: String, new: String) -> bool {
        let Some(i) = self.tab.idx.get(key) else {
            return false;
        };
        if !self.tab.formulas.contains(&(self.r, *i)) {
            return false;
        }
        rep.ignored.push(Ignored {
            sheet: self.tab.name.into(),
            row: self.r,
            col: self.col_of(key),
            column: self.header(key),
            id: self.id,
            old,
            new,
            reason: super::msg::FORMULA_LEFT.1.into(),
        });
        true
    }

    /// Base of a cell for the three-way compare: what an Apply took from this book while the
    /// model still holds it (`live`), else the exported value.
    fn base_live(&self, key: &str, live: impl Fn(&BVal) -> bool) -> Option<BVal> {
        baseline::base_now(self.base_val(key), self.tab.name, self.id, key, live)
    }

    /// The book value `v` of `key` is taken into the model; always `true`.
    fn took(&self, rep: &mut Report, key: &str, v: BVal) -> bool {
        rep.applied_cells.extend(baseline::AppliedCell::new(self.tab.name, self.id, key, v));
        true
    }

    fn book_edit(&self, rep: &mut Report, key: &str, cur: Option<f64>, book: Option<f64>) -> bool {
        let f = |x: Option<f64>| x.map_or(String::new(), |x| x.to_string());
        let bv = |x: Option<f64>| x.map_or(BVal::Blank, BVal::N);
        let eq = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => same(a, b),
            _ => false,
        };
        let now = self.base_live(key, |b| match b {
            BVal::N(x) => eq(Some(*x), cur),
            BVal::Blank => cur.is_none(),
            _ => false,
        });
        let base = match now {
            Some(BVal::N(x)) => Some(x),
            Some(BVal::Blank) => None,
            _ => return !self.formula_left(rep, key, f(cur), f(book)) && self.took(rep, key, bv(book)),
        };
        if eq(base, book) {
            rep.model_newer += usize::from(!eq(base, cur));
            return false;
        }
        if self.formula_left(rep, key, f(cur), f(book)) {
            return false;
        }
        if eq(base, cur) {
            return self.took(rep, key, bv(book));
        }
        self.conflict(rep, key, (f(base), f(book), f(cur)), (bv(book), bv(base))) && self.took(rep, key, bv(book))
    }

    /// The book still has the exported number (model units): the user did not touch the cell.
    fn unchanged_num(&self, key: &str, book: f64) -> bool {
        matches!(self.base_val(key), Some(BVal::N(x)) if same(*x, book))
    }

    /// Outline text: compared as rings, so a re-typed but equal outline is no edit.
    fn book_edit_ring(
        &self,
        rep: &mut Report,
        cur: Option<&[(f64, f64)]>,
        book: &[(f64, f64)],
        k: f64,
    ) -> bool {
        let t = |r: Option<&[(f64, f64)]>| r.map_or(String::new(), |r| ring_text(r, k));
        let left = |rep: &mut Report| self.formula_left(rep, "outline", t(cur), ring_text(book, k));
        let took = |rep: &mut Report| self.took(rep, "outline", BVal::S(ring_text(book, k)));
        let fac = self.factor("outline");
        let eq = |a: Option<&[(f64, f64)]>, b: Option<&[(f64, f64)]>| match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => same_ring(a, b),
            _ => false,
        };
        let now = self.base_live("outline", |b| match b {
            BVal::S(s) => parse_outline(s, fac).is_ok_and(|r| eq(Some(r.as_slice()), cur)),
            BVal::Blank => cur.is_none(),
            _ => false,
        });
        let base = match &now {
            Some(BVal::S(s)) => match parse_outline(s, fac) {
                Ok(r) => Some(r),
                Err(_) => return !left(rep) && took(rep),
            },
            Some(BVal::Blank) => None,
            _ => return !left(rep) && took(rep),
        };
        if eq(base.as_deref(), Some(book)) {
            rep.model_newer += usize::from(!eq(base.as_deref(), cur));
            return false;
        }
        if left(rep) {
            return false;
        }
        if eq(base.as_deref(), cur) {
            return took(rep);
        }
        let was = now.clone().unwrap_or(BVal::Blank);
        let pair = (BVal::S(ring_text(book, k)), was);
        self.conflict(rep, "outline", (t(base.as_deref()), ring_text(book, k), t(cur)), pair) && took(rep)
    }

    fn book_edit_text(&self, rep: &mut Report, key: &str, cur: &str, book: &str) -> bool {
        // whitespace / line-ending noise is no edit, with or without a baseline
        if units::same_text(cur, book) {
            return false;
        }
        let bv = if book.trim().is_empty() { BVal::Blank } else { BVal::S(book.into()) };
        let now = self.base_live(key, |b| match b {
            BVal::S(x) => units::same_text(x, cur),
            BVal::Blank => cur.trim().is_empty(),
            _ => false,
        });
        let base = match &now {
            Some(BVal::S(x)) => x.trim(),
            Some(BVal::Blank) => "",
            _ => return !self.formula_left(rep, key, cur.into(), book.into()) && self.took(rep, key, bv),
        };
        if units::same_text(base, book) {
            rep.model_newer += usize::from(!units::same_text(base, cur));
            return false;
        }
        if self.formula_left(rep, key, cur.into(), book.into()) {
            return false;
        }
        if units::same_text(base, cur) {
            return self.took(rep, key, bv);
        }
        let pair = (BVal::S(book.into()), BVal::S(base.into()));
        self.conflict(rep, key, (base.into(), book.into(), cur.into()), pair) && self.took(rep, key, bv)
    }

    /// Records a conflict; `true` if the user chose the book value (the edit then applies).
    fn conflict(
        &self,
        rep: &mut Report,
        key: &str,
        (base, book, model): (String, String, String),
        (book_v, base_v): (BVal, BVal),
    ) -> bool {
        let take = super::baseline::taken(self.tab.name, self.r, self.col_of(key));
        if !take {
            rep.kept.extend(KeptCell::new(self.tab.name, self.id, key, book_v, base_v));
        }
        rep.conflicts.push(Conflict {
            sheet: self.tab.name.into(),
            row: self.r,
            col: self.col_of(key),
            column: self.header(key),
            id: self.id,
            base,
            book,
            model,
            take_book: take,
        });
        take
    }

    fn changed(&self, rep: &mut Report, key: &str, old: String, new: String) {
        let col = if key == "id" { 0 } else { self.col_of(key) };
        // a row change keeps the canonical "id" (`Change::kind`), whatever the id caption says
        let column = if key == "id" { key.to_string() } else { self.header(key) };
        rep.changes.push(Change {
            sheet: self.tab.name.into(),
            row: self.r,
            col,
            column,
            id: self.id,
            old,
            new,
        });
    }

    /// Cell as a number; Err — the rejection is already recorded.
    fn parse_num(&mut self, rep: &mut Report, key: &str) -> Result<Option<f64>, ()> {
        if let Some(r) = self.not_given(key) {
            return r;
        }
        let v = match self.cell(key) {
            None => return Ok(None),
            Some(Data::Float(f)) => *f,
            Some(Data::Int(i)) => *i as f64,
            Some(Data::String(s)) => {
                let k = self.factor(key);
                let len = self.tab.dims.get(key) == Some(&Dim::Len);
                let v = if len {
                    units::len_value(s, k)
                } else {
                    units::number(s, None).map(|v| v * k)
                };
                return match v {
                    Ok(v) if v.is_finite() => Ok(Some(v)),
                    Ok(_) => {
                        self.bad(rep, key, "value is not finite");
                        Err(())
                    }
                    Err(e) => {
                        // a formula's result is never applied, so its text is not refused
                        let old = match self.base_val(key) {
                            Some(BVal::N(x)) => x.to_string(),
                            _ => String::new(),
                        };
                        if self.id.is_some() && self.formula_left(rep, key, old, s.trim().into()) {
                            return Err(());
                        }
                        let t = if len {
                            format!(
                                "not a number or length: '{}' (e.g. 3.5, 3500 mm, 10'-6\")",
                                s.trim()
                            )
                        } else {
                            format!("not a number: '{}'", s.trim())
                        };
                        // an ambiguous value is flagged, never applied as either reading
                        self.bad(rep, key, units::num_reason(e, s, t));
                        Err(())
                    }
                };
            }
            Some(Data::Error(e)) => {
                let t = format!("formula error {e} \u{2014} fix or clear the cell");
                self.reject(rep, key, t);
                return Err(());
            }
            Some(Data::DateTime(_) | Data::DateTimeIso(_)) => {
                self.reject(
                    rep,
                    key,
                    "the spreadsheet turned this value into a date \u{2014} format the column as Number and type it again",
                );
                return Err(());
            }
            Some(Data::Bool(b)) => {
                let t = format!("not a number: '{}'", bool_text(*b));
                self.reject(rep, key, t);
                return Err(());
            }
            Some(other) => {
                let t = format!("not a number: '{other}'");
                self.bad(rep, key, t);
                return Err(());
            }
        };
        if !v.is_finite() {
            self.bad(rep, key, "value is not finite (NaN or infinity)");
            return Err(());
        }
        Ok(Some(v * self.factor(key)))
    }

    /// A cell under a merge: an existing element keeps its value (Err, no rejection: the merge
    /// note says why); a new row reads it as blank.
    fn not_given<T>(&self, key: &str) -> Option<Result<Option<T>, ()>> {
        self.covered(key).then(|| if self.id.is_some() { Err(()) } else { Ok(None) })
    }

    fn parse_text(&mut self, rep: &mut Report, key: &str) -> Result<Option<String>, ()> {
        if let Some(r) = self.not_given(key) {
            return r;
        }
        match self.cell(key) {
            None => Ok(None),
            Some(Data::String(s)) => Ok(Some(s.trim().to_string())),
            Some(d @ (Data::Float(_) | Data::Int(_))) => {
                let d = d.clone();
                // a new element has no model text: a number past 15 digits cannot be its code
                if self.id.is_none() && units::num_text(&d, "", units::book_dec()) == units::NumText::Lossy {
                    self.bad(rep, key, super::textcode::LOSSY);
                    return Err(());
                }
                let x = match d {
                    Data::Int(i) => i as f64,
                    Data::Float(x) => x,
                    _ => 0.0,
                };
                Ok(Some(units::num_cell_text(x)))
            }
            Some(Data::Bool(b)) => Ok(Some(bool_text(*b).to_string())),
            Some(Data::Error(e)) => {
                let t = format!("formula error {e} \u{2014} fix or clear the cell");
                self.reject(rep, key, t);
                Err(())
            }
            Some(Data::DateTime(_) | Data::DateTimeIso(_)) => {
                // `1-2` became a date: only this cell is refused, the model text stays
                self.bad(
                    rep,
                    key,
                    "the spreadsheet turned this value into a date \u{2014} format the column as Text and type it again",
                );
                Err(())
            }
            Some(other) => {
                let t = format!("not text: '{other}'");
                self.bad(rep, key, t);
                Err(())
            }
        }
    }

    fn parse_id(&mut self, rep: &mut Report, key: &str) -> Result<Option<i64>, ()> {
        match self.parse_num(rep, key)? {
            Some(v) if v.fract() == 0.0 && v.abs() < 9e15 => Ok(Some(v as i64)),
            Some(v) => {
                self.bad(rep, key, format!("not an integer: {v}"));
                Err(())
            }
            None => Ok(None),
        }
    }

    /// Required number. `cur` = Some for an existing row (records the change).
    fn num(&mut self, rep: &mut Report, key: &str, cur: Option<f64>) -> f64 {
        match self.parse_num(rep, key) {
            Err(()) => cur.unwrap_or(0.0),
            Ok(None) => {
                self.bad(rep, key, "value required");
                cur.unwrap_or(0.0)
            }
            Ok(Some(v)) => match cur {
                Some(c) if same(c, v) => c,
                Some(c) if !self.book_edit(rep, key, Some(c), Some(v)) => c,
                Some(c) => {
                    self.changed(rep, key, c.to_string(), v.to_string());
                    v
                }
                None => v,
            },
        }
    }

    /// Optional number (empty = None).
    fn num_opt(
        &mut self,
        rep: &mut Report,
        key: &str,
        cur: Option<f64>,
        existing: bool,
    ) -> Option<f64> {
        if !self.has(key) {
            return cur;
        }
        let Ok(v) = self.parse_num(rep, key) else {
            return cur;
        };
        let same_v = match (cur, v) {
            (None, None) => true,
            (Some(a), Some(b)) => same(a, b),
            _ => false,
        };
        if same_v {
            return cur;
        }
        if existing {
            if !self.book_edit(rep, key, cur, v) {
                return cur;
            }
            let f = |x: Option<f64>| x.map_or(String::new(), |x| x.to_string());
            self.changed(rep, key, f(cur), f(v));
        }
        v
    }

    fn text(&mut self, rep: &mut Report, key: &str, cur: Option<&str>) -> String {
        self.text_alias(rep, key, cur, None)
    }

    /// Like `text`, but a cell holding `alias` (the model's name of a level or type the book
    /// renames) counts as unchanged: the reference follows the rename.
    fn text_alias(
        &mut self,
        rep: &mut Report,
        key: &str,
        cur: Option<&str>,
        alias: Option<&str>,
    ) -> String {
        // a cell cut to fit the sheet limit is read-only: the full model text stays
        let cut = match self.cell(key) {
            Some(Data::String(cell)) => super::cellsafe::cut_cell(cell, cur),
            _ => None,
        };
        {
            if let Some(same) = cut {
                if cur.is_none() {
                    // a new row has no full text to keep: refuse it instead of an empty name
                    self.bad(rep, key, super::cellsafe::CLIPPED_REASON);
                    return String::new();
                }
                if !same {
                    rep.ignored.push(Ignored {
                        sheet: self.tab.name.into(),
                        row: self.r,
                        col: self.col_of(key),
                        column: self.header(key),
                        id: self.id,
                        old: String::new(),
                        new: String::new(),
                        reason: super::cellsafe::CLIPPED_REASON.into(),
                    });
                }
                return cur.unwrap_or("").to_string();
            }
        }
        // a whitespace-only cell is as blank as an empty one, never a rename to ""
        let parsed = self.parse_text(rep, key).map(|v| v.filter(|v| !units::same_text(v, "")));
        match parsed {
            Err(()) => cur.unwrap_or("").to_string(),
            Ok(None) => {
                self.bad(rep, key, "value required");
                cur.unwrap_or("").to_string()
            }
            Ok(Some(v)) => {
                if let Some(c) = cur {
                    // the model text stays byte-exact when the cell differs only in spacing
                    if units::same_text(c, &v) || alias.is_some_and(|a| units::same_text(a, &v)) {
                        return c.to_string();
                    }
                    if c != v && self.turned_code(rep, key, c) {
                        return c.to_string();
                    }
                    if c != v {
                        // the baseline holds the model's own name, not the renamed one
                        if !self.book_edit_text(rep, key, alias.unwrap_or(c), &v) {
                            return c.to_string();
                        }
                        self.changed(rep, key, c.to_string(), v.clone());
                    }
                }
                v
            }
        }
    }

    fn delete_flag(&mut self, rep: &mut Report) -> bool {
        match self.cell("delete") {
            None => false,
            Some(d) => match units::delete_word(d) {
                units::DelWord::Yes => true,
                units::DelWord::No => false,
                units::DelWord::Unknown(other) => {
                    let t = super::msg::fill_template(super::msg::DELETE_UNKNOWN.1, &[other.as_str()]);
                    self.bad(rep, "delete", t);
                    false
                }
            },
        }
    }

    /// Read-only computed column: a value that differs from the model is rejected.
    fn read_only(&mut self, rep: &mut Report, key: &str, cur: Option<f64>) {
        let Some(cur) = cur else { return };
        if let Ok(Some(v)) = self.parse_num(rep, key) {
            if !same(cur, v) && !self.unchanged_num(key, v) {
                let f = self.factor(key);
                let head = self.tab.headers.get(key).map(String::as_str).unwrap_or(key);
                let shown = ro_shown(cur / f, head);
                let t = super::msg::fill_template(super::msg::READ_ONLY.1, &[shown.as_str()]);
                self.skip(rep, key, t);
                if let Some(last) = rep.ignored.last_mut() {
                    (last.old, last.new) = (cur.to_string(), v.to_string());
                }
            }
        }
    }

    /// Read-only text column: a cell that differs from the model and from the export is reported.
    fn read_only_text(&mut self, rep: &mut Report, key: &str, cur: &str) {
        if !self.has(key) {
            return;
        }
        let book = self.cell(key).map_or(String::new(), ro_text);
        let cur = cur.trim();
        if book.is_empty() || book == cur {
            return;
        }
        if matches!(self.base_val(key), Some(BVal::S(x)) if x.trim() == book) {
            return;
        }
        let t = format!("read-only column, computed value is {cur}; edit not accepted");
        self.skip(rep, key, t);
        if let Some(last) = rep.ignored.last_mut() {
            (last.old, last.new) = (cur.to_string(), book);
        }
    }

    /// An existing row without the column — the current value; a new row with an empty cell — `default`.
    fn num_keep(
        &mut self,
        rep: &mut Report,
        key: &str,
        cur: Option<f64>,
        default: Option<f64>,
    ) -> f64 {
        match (cur, default) {
            (Some(c), _) if !self.has(key) => c,
            (None, Some(d)) if self.cell(key).is_none() => d,
            _ => self.num(rep, key, cur),
        }
    }

    /// Not editable for an existing row (edited in the editor): a difference is a rejection.
    fn locked(&mut self, rep: &mut Report, key: &str, cur: f64, what: &str) -> f64 {
        if !self.has(key) {
            return cur;
        }
        if let Ok(Some(v)) = self.parse_num(rep, key) {
            if !same(cur, v) && self.unchanged_num(key, v) {
                rep.model_newer += 1;
            } else if !same(cur, v) {
                self.bad(
                    rep,
                    key,
                    format!("{what} of an existing curtain wall is edited in the editor"),
                );
            }
        }
        cur
    }

    /// An id-less row an earlier Apply already created: ignored, the book needs a re-export.
    fn made(&self, rep: &mut Report) -> bool {
        let (fp, legacy) = (self.fingerprint(), self.fingerprint_legacy());
        let Some(id) = super::baseline::made(self.tab.name, self.r, &fp, &legacy) else {
            return false;
        };
        self.skip(rep, "id", format!("row already created as {id}, export the workbook again"));
        true
    }

    /// Hash of the cells that say which element the row is (name, type, level, position),
    /// not of the ones a later edit may change (sizes).
    fn fingerprint(&self) -> String {
        self.fingerprint_with(false)
    }

    /// The fingerprint before numeric name/number/type/level/kind cells read as their text:
    /// every number as "{:.4}". Only for matching records stored by older Apply runs.
    fn fingerprint_legacy(&self) -> String {
        self.fingerprint_with(true)
    }

    fn fingerprint_with(&self, legacy: bool) -> String {
        const KEYS: [&str; 14] = [
            "level", "type", "name", "number", "ax", "ay", "bx", "by", "px", "py", "wall", "along", "kind", "outline",
        ];
        let keys: &[&str] = if self.tab.name == LEVELS { &["name"] } else { &KEYS };
        let parts: Vec<(&str, String)> =
            keys.iter().filter_map(|k| Some((*k, fp_text(k, self.cell(k)?, legacy)))).collect();
        super::baseline::fingerprint(&parts)
    }

    /// A number cell of a text column against the model text `cur`: `true` when it is no edit.
    /// A code the spreadsheet coerced (`01` -> 1) keeps the model text byte-exact with a note;
    /// a code that lost digits is refused. A real numeric edit returns `false`.
    fn turned_code(&mut self, rep: &mut Report, key: &str, cur: &str) -> bool {
        let Some(d) = self.cell(key).filter(|d| matches!(d, Data::Float(_) | Data::Int(_))).cloned() else {
            return false;
        };
        match units::num_text(&d, cur, units::book_dec()) {
            units::NumText::Edit(_) => false,
            units::NumText::Lossy => {
                self.bad(rep, key, super::textcode::LOSSY);
                true
            }
            units::NumText::Same => {
                let x = match d {
                    Data::Int(i) => i as f64,
                    Data::Float(x) => x,
                    _ => f64::NAN,
                };
                if units::num_cell_text(x) != cur.trim() {
                    rep.ignored.push(Ignored {
                        sheet: self.tab.name.into(),
                        row: self.r,
                        col: self.col_of(key),
                        column: self.header(key),
                        id: self.id,
                        old: cur.to_string(),
                        new: d.to_string(),
                        reason: super::textcode::REASON.into(),
                    });
                }
                true
            }
        }
    }

    fn row_change(&self, rep: &mut Report, what: &str) {
        if what == CREATED {
            rep.fingerprints.insert((self.tab.name.to_string(), self.r), self.fingerprint());
        }
        self.changed(rep, "id", String::new(), what.into());
    }
}

/// Fingerprint text of one identifying cell. `legacy`: the conversion of older Apply runs,
/// which wrote numeric text keys as "3.0000" instead of "3".
pub(super) fn fp_text(key: &str, d: &Data, legacy: bool) -> String {
    const TEXT_KEYS: [&str; 5] = ["level", "type", "name", "number", "kind"];
    match d {
        d if !legacy && TEXT_KEYS.contains(&key) => super::baseline::cell_text(d),
        Data::String(s) => s.trim().to_lowercase(),
        Data::Float(f) => format!("{f:.4}"),
        Data::Int(i) => format!("{:.4}", *i as f64),
        other => other.to_string(),
    }
}

struct WallRow {
    level: i64,
    wtype: i64,
    a: (f64, f64),
    b: (f64, f64),
    base_offset: f64,
    height: f64,
    offset0: Option<f64>,
    bulge: f64,
}

impl WallRow {
    fn length(&self) -> f64 {
        axis_length(self.a, self.b, self.bulge)
    }
}

/// Axis length (along the arc when bulge ≠ 0), m.
fn axis_length(a: (f64, f64), b: (f64, f64), bulge: f64) -> f64 {
    ddc_geom::arc::axis_length(ddc_geom::v2(a.0, a.1), ddc_geom::v2(b.0, b.1), bulge)
}

type OpRow = (i64, f64, f64, f64, f64, String, Swing);

fn swing_text(s: Swing) -> (&'static str, &'static str) {
    (
        if s.side { "right" } else { "left" },
        if s.hinge { "end" } else { "start" },
    )
}

fn sql_err(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Rows whose name would clash are rejected and their edits dropped. Only a renamed or new row
/// is blamed (an unchanged name is unique in the model); restoring a rejected row's old name may
/// make another row clash, so this repeats until stable. Returns the dropped (id, name) rows.
fn dup_names(
    rep: &mut Report,
    tab: &Tab,
    rows: &[(u32, Option<i64>, String)],
    names: &mut HashMap<i64, String>,
    orig: &HashMap<i64, String>,
    what: &str,
) -> Vec<(Option<i64>, String)> {
    let mut out: HashSet<u32> = HashSet::new();
    let mut dropped = vec![];
    loop {
        let mut count: HashMap<&str, usize> = HashMap::new();
        for n in names.values() {
            *count.entry(n.as_str()).or_default() += 1;
        }
        for (r, id, n) in rows {
            if id.is_none() && !out.contains(r) {
                *count.entry(n.as_str()).or_default() += 1;
            }
        }
        let hit: Vec<(u32, Option<i64>, String)> = rows
            .iter()
            .filter(|(r, id, n)| {
                !out.contains(r)
                    && count.get(n.as_str()).copied().unwrap_or(0) > 1
                    && id.map_or(true, |i| orig.get(&i) != Some(n))
            })
            .cloned()
            .collect();
        if hit.is_empty() {
            return dropped;
        }
        for (r, id, n) in hit {
            out.insert(r);
            if let Some((_, cells)) = tab.rows.iter().find(|x| x.0 == r) {
                Row::new(tab, r, cells).reject(rep, "name", format!("duplicate {what} name '{n}'"));
            }
            if let Some((i, o)) = id.and_then(|i| orig.get(&i).map(|o| (i, o.clone()))) {
                names.insert(i, o);
            }
            dropped.push((id, n));
        }
    }
}

/// Ids written explicitly in the sheet: never offered to by-key matching.
fn explicit_ids(tab: &Tab) -> HashSet<i64> {
    let mut sink = Report::default();
    tab.rows
        .iter()
        .filter_map(|(r, cells)| Row::new(tab, *r, cells).parse_id(&mut sink, "id").ok().flatten())
        .collect()
}

/// Outcome of looking a row key up among existing elements.
enum Found {
    No,
    One(i64),
    Many(usize),
    /// Exactly one element, but another row of the sheet already owns it.
    Taken,
}

fn find_key(hits: &[i64], taken: &HashSet<i64>) -> Found {
    match hits {
        [] => Found::No,
        [one] if taken.contains(one) => Found::Taken,
        [one] => Found::One(*one),
        many => Found::Many(many.len()),
    }
}

fn norm_key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Reason text (English template, see `msg.rs`) of a key that was not matched.
fn key_note(f: &Found) -> Option<String> {
    match f {
        Found::Many(n) => Some(super::msg::fill_template(
            super::msg::RI1_MANY.1,
            &[&n.to_string()],
        )),
        Found::Taken => Some(super::msg::RI1_TAKEN.1.into()),
        _ => None,
    }
}

/// A key match the user rejected: the element stays as it is (seen, so `delete_missing`
/// never removes it), the row creates a new element, and the report lists it per row.
fn note_kept(rep: &mut Report, sheet: &str, row: u32, id: i64, key: String) {
    note_unmatched(rep, sheet, row, key, super::msg::RI1_KEPT.1.into());
    if let Some(last) = rep.ignored.last_mut() {
        last.id = Some(id);
    }
}

/// `note_kept` plus a listed `Rejected` match, so the decision can still be changed.
fn note_rejected(rep: &mut Report, sheet: &str, row: u32, id: i64, kind: &'static str, key: String) {
    rep.matches.push(KeyMatch {
        sheet: sheet.into(),
        row,
        id,
        kind,
        key: key.clone(),
        state: MatchState::Rejected,
        changes: Vec::new(),
    });
    note_kept(rep, sheet, row, id, key);
}

fn note_unmatched(rep: &mut Report, sheet: &str, row: u32, key: String, why: String) {
    rep.ignored.push(Ignored {
        sheet: sheet.into(),
        row,
        col: 0,
        column: String::new(),
        id: None,
        old: String::new(),
        new: key,
        reason: why,
    });
}

/// Closes a by-key match: a pending one keeps its would-be edits out of the batch.
#[allow(clippy::too_many_arguments)]
fn finish_match(
    rep: &mut Report,
    edits: &mut Vec<Edit>,
    marks: (usize, usize),
    sheet: &str,
    row: u32,
    id: i64,
    kind: &'static str,
    key: String,
    dec: &KeyDecisions,
) {
    let accepted = dec.accept.contains(&(sheet.to_string(), row));
    let changes: Vec<Change> = if accepted {
        rep.changes[marks.0..].to_vec()
    } else {
        edits.truncate(marks.1);
        rep.changes.drain(marks.0..).collect()
    };
    rep.matches.push(KeyMatch {
        sheet: sheet.into(),
        row,
        id,
        kind,
        key,
        state: if accepted { MatchState::Accepted } else { MatchState::Pending },
        changes,
    });
}

pub(crate) const NO_DELETES: &str = "unreadable id: elements without a row are not deleted in this import";

/// Decimal separator the Info sheet declares (rows `decimal` or `locale`); None when it says nothing.
pub(crate) fn info_dec(r: &calamine::Range<Data>) -> Option<units::Dec> {
    let rows: Vec<(String, String)> = r
        .rows()
        .map(|row| {
            let at = |i: usize| row.get(i).map(|d| d.to_string()).unwrap_or_default();
            (at(0), at(1))
        })
        .collect();
    units::dec_from_info(rows.iter().map(|(k, v)| (k.as_str(), v.as_str())))
}

/// First non-empty row whose id cell is filled but is not an id.
fn unreadable_id(tab: &Tab) -> Option<u32> {
    let mut scratch = Report::default();
    tab.rows.iter().find_map(|(r, cells)| {
        let mut row = Row::new(tab, *r, cells);
        (!row.empty() && row.parse_id(&mut scratch, "id").is_err()).then_some(*r)
    })
}

/// `#RRGGBB` of a colour value.
fn color_text(c: u32) -> String {
    format!("#{c:06X}")
}

/// Colour cell text: empty = none (the theme class colour), else `#RRGGBB` or `RRGGBB`.
fn parse_color(t: &str) -> Result<Option<u32>, String> {
    let t = t.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let h = t.strip_prefix('#').unwrap_or(t);
    match u32::from_str_radix(h, 16) {
        Ok(v) if h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()) => Ok(Some(v)),
        _ => Err(format!("not a colour: '{t}' (use #RRGGBB)")),
    }
}

/// Materials sheet: name, colour, hatch, properties and note of existing materials. A cell is
/// refused alone with its reason; a rename to a name in use (or taken by two rows) is refused.
fn materials_edits(model: &Model, tab: &Tab, rep: &mut Report) -> Vec<Edit> {
    struct Pending {
        r: u32,
        id: i64,
        old: ddc_model::MaterialSpec,
        new: ddc_model::MaterialSpec,
    }
    let cur: HashMap<i64, ddc_model::MaterialSpec> = model
        .material_rows()
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.id, m.spec))
        .collect();
    let all = model.materials().unwrap_or_default();
    let taken: HashSet<String> = all.iter().map(|m| m.spec.name.clone()).collect();
    // texts in use without a row: exported with a blank id, matched by name
    let rowless: HashMap<String, ddc_model::MaterialSpec> = all
        .into_iter()
        .filter(|m| m.id == 0)
        .map(|m| (m.spec.name.clone(), m.spec))
        .collect();
    // row-less materials no blank-id row names: an unknown name beside one is likely a rename
    let blank_names: HashSet<String> = tab
        .rows
        .iter()
        .filter_map(|(r, cells)| {
            let row = Row::new(tab, *r, cells);
            match (row.cell("id"), row.cell("name")) {
                (None, Some(Data::String(n))) => Some(n.trim().to_string()),
                _ => None,
            }
        })
        .collect();
    let mut gone: Vec<&str> =
        rowless.keys().filter(|n| !blank_names.contains(*n)).map(String::as_str).collect();
    gone.sort_unstable();
    let mut seen_rowless: HashSet<String> = HashSet::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut pend: Vec<Pending> = vec![];
    let dups = dup::Dups::plan(tab, &KeyDecisions::default());
    for (r, cells) in &tab.rows {
        let mut row = Row::new(tab, *r, cells);
        if row.empty() {
            continue;
        }
        let Ok(id) = row.parse_id(rep, "id") else {
            continue;
        };
        row.id = id;
        if !dups.route(&mut row, rep, &mut seen) {
            continue;
        }
        let (id, old) = match id {
            Some(id) => {
                let Some(old) = cur.get(&id) else {
                    row.stale(rep, "material", id);
                    continue;
                };
                if !seen.insert(id) {
                    row.dup(rep, id);
                    continue;
                }
                (id, old.clone())
            }
            None => {
                let name = match row.cell("name") {
                    Some(Data::String(n)) => n.trim().to_string(),
                    _ => String::new(),
                };
                if let Some(old) = rowless.get(&name) {
                    if !seen_rowless.insert(name.clone()) {
                        let why = format!("material {name} is listed twice; row ignored");
                        row.skip(rep, "name", why);
                        continue;
                    }
                    (0, old.clone())
                } else if cur.values().any(|s| s.name == name) {
                    // the material got a row after the export: its values are not this row's
                    let why = format!("material {name} has an id now; row ignored (export again)");
                    row.skip(rep, "name", why);
                    continue;
                } else if !name.is_empty() && !gone.is_empty() {
                    // matching by position or similarity could change the wrong material
                    let was = gone.join("', '");
                    let args = [name.as_str(), was.as_str()];
                    let why = super::msg::fill_template(super::msg::MAT_RENAMED.1, &args);
                    row.reject(rep, "name", why);
                    continue;
                } else {
                    row.reject(rep, "id", "creating materials from a workbook is not supported");
                    continue;
                }
            }
        };
        let mut new = old.clone();
        // a cell refused after it was read: its change is dropped, the model value stays
        let drop_change = |rep: &mut Report, row: &Row, key: &str| {
            let (sheet, r, col) = (tab.name, row.r, row.col_of(key));
            rep.changes.retain(|c| !(c.sheet == sheet && c.row == r && c.col == col));
        };
        new.name = row.text(rep, "name", Some(&old.name));
        let n = new.name.chars().count();
        if new.name != old.name && !(1..=120).contains(&n) {
            drop_change(rep, &row, "name");
            row.bad(rep, "name", "material name must be 1..120 characters");
            new.name = old.name.clone();
        }
        if row.has("color") {
            if let Ok(t) = row.parse_text(rep, "color") {
                match parse_color(&t.unwrap_or_default()) {
                    Err(why) => row.bad(rep, "color", why),
                    Ok(v) => {
                        let was = old.color.map(color_text).unwrap_or_default();
                        let now = v.map(color_text).unwrap_or_default();
                        if now != was && row.book_edit_text(rep, "color", &was, &now) {
                            row.changed(rep, "color", was, now);
                            new.color = v;
                        }
                    }
                }
            }
        }
        if row.has("hatch") {
            if let Ok(t) = row.parse_text(rep, "hatch") {
                // built-in keys in any case; a library hatch keeps its name as written
                let t = t.map(|h| {
                    let low = h.to_lowercase();
                    if ddc_model::MATERIAL_HATCHES.contains(&low.as_str()) { low } else { h }
                });
                match t {
                    None => row.bad(rep, "hatch", "value required"),
                    Some(h)
                        if !ddc_model::MATERIAL_HATCHES.contains(&h.as_str())
                            && !ddc_model::is_lib_hatch(&h) =>
                    {
                        let why = format!(
                            "unknown hatch '{h}' (use {})",
                            ddc_model::MATERIAL_HATCHES.join(", ")
                        );
                        row.bad(rep, "hatch", why)
                    }
                    Some(h) => {
                        if h != old.hatch && row.book_edit_text(rep, "hatch", &old.hatch, &h) {
                            row.changed(rep, "hatch", old.hatch.clone(), h.clone());
                            new.hatch = h;
                        }
                    }
                }
            }
        }
        let props = [
            ("density", old.density),
            ("conductivity", old.conductivity),
            ("specific_heat", old.specific_heat),
        ];
        let mut vals = [new.density, new.conductivity, new.specific_heat];
        for (i, (key, cv)) in props.into_iter().enumerate() {
            let nv = row.num_opt(rep, key, cv, true);
            if nv != cv {
                if let Some(v) = nv.filter(|v| *v < 0.0) {
                    drop_change(rep, &row, key);
                    row.bad(rep, key, format!("value must be 0 or more, got {v}"));
                    continue;
                }
            }
            vals[i] = nv;
        }
        [new.density, new.conductivity, new.specific_heat] = vals;
        if row.has("note") {
            if let Ok(t) = row.parse_text(rep, "note") {
                let book = t.unwrap_or_default();
                if book != old.note.trim()
                    && !row.turned_code(rep, "note", &old.note)
                    && row.book_edit_text(rep, "note", &old.note, &book)
                {
                    row.changed(rep, "note", old.note.clone(), book.clone());
                    new.note = book;
                }
            }
        }
        let touched = rep.changes.iter().any(|c| c.sheet == MATERIALS && c.row == *r);
        if row.ok && touched {
            pend.push(Pending {
                r: *r,
                id,
                old: old.clone(),
                new,
            });
        }
    }
    let mut want: HashMap<String, usize> = HashMap::new();
    for p in pend.iter().filter(|p| p.new.name != p.old.name) {
        *want.entry(p.new.name.clone()).or_default() += 1;
    }
    let pend_names: HashSet<(String, String)> = pend
        .iter()
        .map(|p| (p.old.name.clone(), p.new.name.clone()))
        .collect();
    let mut out = vec![];
    for mut p in pend {
        if p.new.name != p.old.name && (taken.contains(&p.new.name) || want[&p.new.name] > 1) {
            if let Some((_, cells)) = tab.rows.iter().find(|x| x.0 == p.r) {
                let mut row = Row::new(tab, p.r, cells);
                row.id = Some(p.id).filter(|i| *i > 0);
                let col = row.col_of("name");
                rep.changes
                    .retain(|c| !(c.sheet == MATERIALS && c.row == p.r && c.col == col));
                let swap = pend_names.contains(&(p.new.name.clone(), p.old.name.clone()));
                let why = if swap {
                    "swapping two names is not supported: rename one to a free name first".to_string()
                } else {
                    format!("duplicate material name '{}'", p.new.name)
                };
                row.bad(rep, "name", why);
            }
            p.new.name = p.old.name.clone();
            if !rep.changes.iter().any(|c| c.sheet == MATERIALS && c.row == p.r) {
                continue;
            }
        }
        out.push(Edit::Material {
            name: p.old.name,
            spec: p.new,
        });
    }
    out
}

/// `apply = false` — preview only; `delete_missing` — elements whose rows were removed from
/// the sheet are deleted (walls, openings, rooms, slabs, curtain walls); `new_params` —
/// unknown columns immediately become project parameters (otherwise — proposals in the report).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    pub apply: bool,
    pub delete_missing: bool,
    pub new_params: bool,
}

/// What a book on disk says about itself; read without a model, nothing is changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookInfo {
    /// Project the book was exported from (Info sheet "project"); None for an older book.
    pub project: Option<i64>,
    /// Ids its Baseline lists per category sheet (empty without a baseline).
    pub ids: Selected,
}

/// Book number of a cell in model units, read like the main import (group separators, the
/// book's decimal mark, units in a length column). The second value is the other reading of
/// an ambiguous text (`1,250`); None for a blank, Err for text that is no number.
fn ro_num(d: &Data, k: f64, len: bool) -> Result<Option<(f64, Option<f64>)>, ()> {
    match d {
        d if blank(d) => Ok(None),
        Data::Float(x) => Ok(Some((*x * k, None))),
        Data::Int(i) => Ok(Some((*i as f64 * k, None))),
        Data::String(s) => {
            let r = if len { units::len_value(s, k) } else { units::number(s, None).map(|v| v * k) };
            match r {
                Ok(v) if v.is_finite() => Ok(Some((v, None))),
                Err(units::NumErr::Ambiguous { grouped, decimal }) => {
                    Ok(Some((grouped * k, Some(decimal * k))))
                }
                _ => Err(()),
            }
        }
        _ => Err(()),
    }
}

/// True if a number read by [`ro_num`] (either reading) equals `y`.
fn ro_hits(b: &Result<Option<(f64, Option<f64>)>, ()>, y: f64) -> bool {
    matches!(b, Ok(Some((v, alt))) if same(*v, y) || alt.is_some_and(|a| same(a, y)))
}

/// Model value as the export shows it: the column's decimals (a longer tail if rounding would
/// hide the value), no trailing zeros, the book's decimal mark.
pub(super) fn ro_shown(v: f64, header: &str) -> String {
    let n = if header.contains("[m3]") || header.contains("[ft3]") {
        3
    } else if header.contains("[m2]") || header.contains("[ft2]") {
        2
    } else if header.contains("[m]") {
        3
    } else if header.contains("[ft]") {
        2
    } else {
        6
    };
    let mut t = format!("{v:.n$}");
    // `t` is our own `{:.n$}` output: invariant form
    if !same(ddc_model::num_text::parse_invariant(&t).unwrap_or(v), v) {
        t = format!("{v:.9}");
    }
    if t.contains('.') {
        t = t.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if t == "-0" {
        t = "0".into();
    }
    if units::book_dec() == Some(units::Dec::Comma) {
        t = t.replace('.', ",");
    }
    t
}

fn ro_text(d: &Data) -> String {
    if blank(d) {
        String::new()
    } else {
        d.to_string().trim().to_string()
    }
}

/// Read-only columns of a parameter sheet: a cell that differs from the model (and from the
/// export) is reported and never applied; the parameter columns of the row still are.
fn param_read_only(model: &Model, ps: &ParamSheet, tab: &Tab, rep: &mut Report) -> Result<(), String> {
    let Some(&idc) = tab.idx.get("id") else {
        return Ok(());
    };
    let cur: HashMap<i64, Vec<super::export::V>> = super::export::query(model, ps.sql, ps.cols.len())?
        .into_iter()
        .filter_map(|r| match r.first() {
            Some(super::export::V::N(x)) => Some((*x as i64, r)),
            _ => None,
        })
        .collect();
    for (r, cells) in &tab.rows {
        let id = match cells.get(idc) {
            Some(Data::Float(x)) if x.fract() == 0.0 => *x as i64,
            Some(Data::Int(i)) => *i,
            Some(Data::String(t)) => match t.trim().parse::<i64>() {
                Ok(i) => i,
                Err(_) => continue,
            },
            _ => continue,
        };
        let Some(now) = cur.get(&id) else { continue };
        for (j, col) in ps.cols.iter().enumerate().skip(1) {
            // an editable column (a slab type name) is read by the main import
            if col.edit {
                continue;
            }
            let Some(&i) = tab.idx.get(col.key) else { continue };
            let d = cells.get(i).unwrap_or(&Data::Empty);
            let k = tab.factors.get(col.key).copied().unwrap_or(1.0);
            let base = tab.base.as_ref().and_then(|b| b.get(tab.name, id, col.key));
            let (same_now, same_base, shown) = match now.get(j) {
                Some(super::export::V::N(x)) => {
                    let len = tab.dims.get(col.key) == Some(&Dim::Len);
                    let b = ro_num(d, k, len);
                    let base_eq = match base {
                        Some(BVal::N(y)) => ro_hits(&b, *y),
                        Some(BVal::Blank) => matches!(b, Ok(None)),
                        _ => false,
                    };
                    let head = tab.headers.get(col.key).map(String::as_str).unwrap_or(col.header);
                    (ro_hits(&b, *x), base_eq, ro_shown(x / k, head))
                }
                Some(super::export::V::S(t)) => {
                    let b = ro_text(d);
                    let base_eq = match base {
                        Some(BVal::S(y)) => y.trim() == b,
                        Some(BVal::Blank) => b.is_empty(),
                        _ => false,
                    };
                    (b == t.trim(), base_eq, t.clone())
                }
                _ => {
                    let b = ro_text(d);
                    (b.is_empty(), matches!(base, Some(BVal::Blank)) && b.is_empty(), String::new())
                }
            };
            if same_now {
                continue;
            }
            if same_base {
                rep.model_newer += 1;
                continue;
            }
            rep.ignored.push(Ignored {
                sheet: tab.name.into(),
                row: *r,
                col: tab.c0 + i as u32 + 1,
                column: tab.headers.get(col.key).cloned().unwrap_or_default(),
                id: Some(id),
                old: shown.clone(),
                new: ro_text(d),
                reason: super::msg::fill_template(super::msg::READ_ONLY.1, &[shown.as_str()]),
            });
        }
    }
    Ok(())
}

/// Project uid on the Info sheet ("project" row); None for an older or hand-made book.
fn info_project(info: &calamine::Range<Data>) -> Option<i64> {
    info.rows()
        .find(|r| r.first().is_some_and(|d| d.to_string() == "project"))
        .and_then(|r| r.get(1))
        .and_then(|d| d.to_string().trim().parse().ok())
}

/// Project uid the book names, read from its Info sheet only (no data sheet, no baseline);
/// None for a book without Info or without the "project" row.
pub fn book_project(path: &Path) -> Result<Option<i64>, String> {
    let mut wb: Book = open_workbook(path).map_err(|e| format!("cannot open workbook: {e}"))?;
    Ok(wb.worksheet_range(INFO).ok().and_then(|r| info_project(&r)))
}

/// None: no Info sheet, so not a book of this format.
pub fn book_info(path: &Path) -> Result<Option<BookInfo>, String> {
    let mut wb: Book = open_workbook(path).map_err(|e| format!("cannot open workbook: {e}"))?;
    let Ok(info) = wb.worksheet_range(INFO) else {
        return Ok(None);
    };
    let project = info_project(&info);
    let ids = baseline::read(&mut wb)
        .map(|b| Selected {
            walls: b.ids(WALLS),
            openings: b.ids(OPENINGS),
            rooms: b.ids(ROOMS),
            slabs: b.ids(SLABS),
            curtains: b.ids(CURTAINS),
        })
        .unwrap_or_default();
    Ok(Some(BookInfo { project, ids }))
}

pub fn import_xlsx(model: &mut Model, path: &Path) -> Result<Report, String> {
    let o = ImportOptions {
        apply: true,
        delete_missing: false,
        new_params: false,
    };
    import_xlsx_with(model, path, o)
}

/// Elements without a sheet row → deletion in ascending id order (`del` = None — skip).
#[allow(clippy::too_many_arguments)]
fn missing(
    rep: &mut Report,
    sheet: &str,
    all: impl Iterator<Item = i64>,
    seen: &HashSet<i64>,
    base: Option<&Base>,
    delete: bool,
    mut del: impl FnMut(i64) -> Option<Edit>,
    edits: &mut Vec<Edit>,
) {
    let all: Vec<i64> = all.collect();
    let total = all.len();
    let mut ids: Vec<i64> = all.into_iter().filter(|i| !seen.contains(i)).collect();
    ids.sort_unstable();
    let mut deleted = 0usize;
    for id in ids {
        // made in the editor after the export: the book never had a row for it
        if base.is_some_and(|b| b.has_sheet(sheet) && !b.has_id(sheet, id)) {
            continue;
        }
        if let Some(e) = del(id) {
            if !delete {
                rep.removed.push(Removed {
                    sheet: sheet.into(),
                    id,
                });
                continue;
            }
            rep.changes.push(Change {
                sheet: sheet.into(),
                row: 0,
                col: 0,
                column: "id".into(),
                id: Some(id),
                old: String::new(),
                new: MISSING.into(),
            });
            edits.push(e);
            deleted += 1;
        }
    }
    rep.note_deletions(sheet, deleted, total);
}

struct RoomRow {
    level: i64,
    name: String,
    number: String,
    point: (f64, f64),
    height: f64,
    q: Option<(f64, f64, f64)>,
}

struct SlabRow {
    level: i64,
    stype: i64,
    offset: f64,
    q: Option<(f64, f64)>,
}

/// Regions per level, or why that level cannot be checked.
type StagedRegions = HashMap<i64, Result<Vec<ddc_geom::rooms::Region>, String>>;

/// Closed regions of `levels` as they will be once `geo` (the book's staged wall, type and
/// level edits) is applied. The edits run on a throwaway copy of the model, so nothing is
/// written to the open document. A copy or a read that fails is an Err for the level: the
/// caller refuses the rows instead of letting a duplicate through unchecked.
fn staged_regions(model: &Model, geo: &[Edit], levels: &[i64]) -> StagedRegions {
    let read = |m: &Model| -> StagedRegions {
        levels.iter().map(|l| (*l, m.room_regions(*l).map_err(|e| e.to_string()))).collect()
    };
    if geo.is_empty() {
        return read(model);
    }
    let stamp = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = std::env::temp_dir().join(format!("ddc_xlsx_stage_{}_{stamp}.db", std::process::id()));
    let run = || -> Result<StagedRegions, String> {
        model.snapshot_to(&path).map_err(|e| e.to_string())?;
        let p = path.to_str().ok_or("temp path is not UTF-8")?;
        let mut copy = Model::open_copy(p).map_err(|e| e.to_string())?;
        copy.apply_edits(geo).map_err(|e| e.to_string())?;
        Ok(read(&copy))
    };
    let out = run();
    for ext in ["", "-wal", "-shm", "-journal"] {
        let mut p = path.clone().into_os_string();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
    out.unwrap_or_else(|e| levels.iter().map(|l| (*l, Err(e.clone()))).collect())
}

/// One outline coordinate. Export writes rings with a dot in every book, so a lone dot is the
/// decimal mark here whatever the book's mark says: `5.125` in a comma book is never 5125 m.
fn outline_len(x: &str, k: f64) -> Option<f64> {
    let _g = (x.contains('.') && !x.contains(','))
        .then(|| units::DecGuard::set(Some(units::Dec::Point)));
    units::parse_len(x, k)
}

/// `x y; x y; ...` → points, m; Err — the reason for rejection.
fn parse_outline(t: &str, k: f64) -> Result<Vec<(f64, f64)>, String> {
    let mut pts = vec![];
    let pairs = t.split(';').map(str::trim).filter(|p| !p.is_empty());
    for (i, pair) in pairs.enumerate() {
        let xy: Vec<&str> = pair.split_whitespace().collect();
        let p = match xy.as_slice() {
            [x, y] => outline_len(x, k).zip(outline_len(y, k)),
            _ => None,
        };
        match p {
            Some(p) => pts.push(p),
            None => return Err(format!("point {} '{pair}' is not 'x y'", i + 1)),
        }
    }
    if pts.len() < 3 {
        return Err(format!(
            "outline needs at least 3 points, got {}",
            pts.len()
        ));
    }
    let area2: f64 = (0..pts.len())
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    if area2.abs() <= 1e-9 {
        return Err("outline has zero area".into());
    }
    Ok(pts)
}

fn same_ring(a: &[(f64, f64)], b: &[(f64, f64)]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(p, q)| same(p.0, q.0) && same(p.1, q.1))
}

/// `import_xlsx_with` with an optional progress/cancel hook (see `Hook`); a stop gives the
/// error `job::CANCELLED` and the model is not touched.
pub fn import_xlsx_hooked(
    model: &mut Model,
    path: &Path,
    opts: ImportOptions,
    hook: Option<super::export::Hook>,
) -> Result<Report, String> {
    let _guard = super::import_gate::install(hook);
    import_xlsx_with(model, path, opts)
}

pub fn import_xlsx_with(
    model: &mut Model,
    path: &Path,
    opts: ImportOptions,
) -> Result<Report, String> {
    import_xlsx_keyed(model, path, opts, &KeyDecisions::default())
}

/// Like `import_xlsx_with`; `dec` settles rows without an id that match an element by key.
pub fn import_xlsx_keyed(
    model: &mut Model,
    path: &Path,
    opts: ImportOptions,
    dec: &KeyDecisions,
) -> Result<Report, String> {
    let mut wb: Book = open_workbook(path).map_err(|e| format!("cannot open workbook: {e}"))?;
    super::import_gate::begin();
    let mut report = Report::default();
    let mut edits: Vec<Edit> = vec![];
    let db = model.sql();

    // --- current state ---
    let mut levels: HashMap<i64, (String, f64)> = HashMap::new();
    let mut types: HashMap<i64, String> = HashMap::new();
    let mut layers: HashMap<(i64, i64), (String, f64)> = HashMap::new();
    let mut walls: HashMap<i64, WallRow> = HashMap::new();
    let mut openings: HashMap<i64, OpRow> = HashMap::new();
    {
        let mut st = db
            .prepare("SELECT id,name,elevation FROM level")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))
            .map_err(sql_err)?;
        for r in rows {
            let (i, v) = r.map_err(sql_err)?;
            levels.insert(i, v);
        }
        let mut st = db
            .prepare("SELECT id,name FROM wall_type")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sql_err)?;
        for r in rows {
            let (i, v) = r.map_err(sql_err)?;
            types.insert(i, v);
        }
        let mut st = db
            .prepare("SELECT wtype,pos,material,thickness FROM wall_type_layer")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))))
            .map_err(sql_err)?;
        for r in rows {
            let (k, v) = r.map_err(sql_err)?;
            layers.insert(k, v);
        }
        let mut st = db
            .prepare("SELECT id,level,wtype,ax,ay,bx,by,base_offset,height,offset0,bulge FROM wall")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    WallRow {
                        level: r.get(1)?,
                        wtype: r.get(2)?,
                        a: (r.get(3)?, r.get(4)?),
                        b: (r.get(5)?, r.get(6)?),
                        base_offset: r.get(7)?,
                        height: r.get(8)?,
                        offset0: r.get(9)?,
                        bulge: r.get(10)?,
                    },
                ))
            })
            .map_err(sql_err)?;
        for r in rows {
            let (i, v) = r.map_err(sql_err)?;
            walls.insert(i, v);
        }
        let mut st = db
            .prepare(
                "SELECT id,wall,along,width,sill,height,kind,swing_side,swing_hinge FROM opening",
            )
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    (
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        Swing {
                            side: r.get(7)?,
                            hinge: r.get(8)?,
                        },
                    ),
                ))
            })
            .map_err(sql_err)?;
        for r in rows {
            let (i, v) = r.map_err(sql_err)?;
            openings.insert(i, v);
        }
    }

    let mut slab_types: HashMap<String, i64> = HashMap::new();
    let mut slab_layers: HashMap<(i64, i64), (String, f64)> = HashMap::new();
    let mut rooms: HashMap<i64, RoomRow> = HashMap::new();
    let mut slabs: HashMap<i64, SlabRow> = HashMap::new();
    let mut rings: HashMap<i64, Vec<Vec<(f64, f64)>>> = HashMap::new();
    let mut curtains: HashMap<i64, CurtainSpec> = HashMap::new();
    {
        let mut st = db
            .prepare("SELECT id,name FROM slab_type")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(0)?)))
            .map_err(sql_err)?;
        for r in rows {
            let (n, i) = r.map_err(sql_err)?;
            slab_types.insert(n, i);
        }
        let mut st = db
            .prepare("SELECT stype,pos,material,thickness FROM slab_type_layer")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))))
            .map_err(sql_err)?;
        for r in rows {
            let (k, v) = r.map_err(sql_err)?;
            slab_layers.insert(k, v);
        }
        let mut st = db
            .prepare(
                "SELECT r.id,r.level,r.name,r.number,r.px,r.py,r.height,q.area,q.perimeter,\
                 q.volume FROM room r LEFT JOIN room_qty q ON q.room=r.id",
            )
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| {
                let q = match r.get::<_, Option<f64>>(7)? {
                    Some(a) => Some((a, r.get(8)?, r.get(9)?)),
                    None => None,
                };
                Ok((
                    r.get::<_, i64>(0)?,
                    RoomRow {
                        level: r.get(1)?,
                        name: r.get(2)?,
                        number: r.get(3)?,
                        point: (r.get(4)?, r.get(5)?),
                        height: r.get(6)?,
                        q,
                    },
                ))
            })
            .map_err(sql_err)?;
        for r in rows {
            let (i, v) = r.map_err(sql_err)?;
            rooms.insert(i, v);
        }
        let mut st = db
            .prepare(
                "SELECT s.id,s.level,s.stype,s.offset,MAX(q.area),SUM(q.volume) FROM slab s \
                 LEFT JOIN slab_qty q ON q.slab=s.id GROUP BY s.id",
            )
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| {
                let q = match r.get::<_, Option<f64>>(4)? {
                    Some(a) => Some((a, r.get(5)?)),
                    None => None,
                };
                Ok((
                    r.get::<_, i64>(0)?,
                    SlabRow {
                        level: r.get(1)?,
                        stype: r.get(2)?,
                        offset: r.get(3)?,
                        q,
                    },
                ))
            })
            .map_err(sql_err)?;
        for r in rows {
            let (i, v) = r.map_err(sql_err)?;
            slabs.insert(i, v);
        }
        let mut st = db
            .prepare("SELECT slab,ring,x,y FROM slab_vertex ORDER BY slab,ring,idx")
            .map_err(sql_err)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    (r.get(2)?, r.get(3)?),
                ))
            })
            .map_err(sql_err)?;
        for r in rows {
            let (sl, ring, p) = r.map_err(sql_err)?;
            let v = rings.entry(sl).or_default();
            while v.len() <= ring as usize {
                v.push(vec![]);
            }
            v[ring as usize].push(p);
        }
    }
    for id in model.curtain_walls().map_err(|e| e.to_string())? {
        curtains.insert(id, model.curtain_wall(id).map_err(|e| e.to_string())?);
    }

    // Units are declared on the Info sheet (absent = metric).
    let units = match wb.worksheet_range(INFO) {
        Ok(r) => match r
            .get_value((0, 1))
            .map(|d| d.to_string().to_ascii_lowercase())
        {
            Some(t) if t.trim() == "imperial" => Units::Imperial,
            Some(t) if t.trim() == "metric" => Units::Metric,
            Some(t) => {
                report.rejected.push(Rejection {
                    sheet: INFO.into(),
                    row: 1,
                    col: 2,
                    column: "units".into(),
                    reason: format!("units must be metric or imperial, got '{}'", t.trim()),
                });
                Units::Metric
            }
            None => Units::Metric,
        },
        Err(_) => Units::Metric,
    };
    // the guard lives to the end of this import: text numbers use the book's separator
    let _dec = units::DecGuard::set(wb.worksheet_range(INFO).ok().and_then(|r| info_dec(&r)));
    report.no_project = wb.worksheet_range(INFO).ok().and_then(|r| info_project(&r)).is_none();
    if let Ok(r) = wb.worksheet_range(INFO) {
        if r.get_value((2, 0)).is_some_and(|d| d.to_string() == "scope") {
            report.scope = r.get_value((2, 1)).map(|d| d.to_string()).unwrap_or_default();
        }
        if r.get_value((3, 0)).is_some_and(|d| d.to_string() == "scope_spec") {
            report.scope_spec = r.get_value((3, 1)).map(|d| d.to_string()).unwrap_or_default();
        }
    }
    let base: Option<Rc<Base>> = baseline::read(&mut wb)
        .filter(|b| super::no_base::covers(b, &wb.sheet_names()))
        .map(Rc::new);
    report.no_base = base.is_none();
    if let Some(b) = base.as_ref().filter(|_| !report.scope.is_empty() || !report.scope_spec.is_empty()) {
        report.base_ids = Selected {
            walls: b.ids(WALLS),
            openings: b.ids(OPENINGS),
            rooms: b.ids(ROOMS),
            slabs: b.ids(SLABS),
            curtains: b.ids(CURTAINS),
        };
    }
    // without its baseline a book cannot tell a deleted row from one it never had (or a scope)
    let mut opts = opts;
    if base.is_none() {
        opts.delete_missing = false;
    }
    let keys = |k: Vec<i64>| -> HashSet<i64> { k.into_iter().collect() };
    let pairs = |k: Vec<&(i64, i64)>| keys(k.into_iter().map(|&(t, p)| layer_key(t, p)).collect());
    let live: HashMap<&'static str, HashSet<i64>> = [
        (LEVELS, keys(levels.keys().copied().collect())),
        (TYPES, keys(types.keys().copied().collect())),
        (LAYERS, pairs(layers.keys().collect())),
        (SLAB_LAYERS, pairs(slab_layers.keys().collect())),
        (WALLS, keys(walls.keys().copied().collect())),
        (OPENINGS, keys(openings.keys().copied().collect())),
        (ROOMS, keys(rooms.keys().copied().collect())),
        (SLABS, keys(slabs.keys().copied().collect())),
        (CURTAINS, keys(curtains.keys().copied().collect())),
    ]
    .into_iter()
    .collect();
    let scoped = !report.scope.is_empty() || !report.scope_spec.is_empty();
    if base.is_none() && !scoped {
        // an exported book that lost its baseline: the model says which sheets it should have
        let have = wb.sheet_names();
        // only sheets the book's own export wrote: an older book never had a newer sheet
        if have.iter().any(|h| h.as_str() == INFO) {
            let info = wb.worksheet_range(INFO).ok();
            let wrote = |n: &str| info.as_ref().is_none_or(|i| super::no_base::wrote(i, n));
            let mut gone: Vec<&str> = vec![];
            for (&n, now) in &live {
                let there = have.iter().any(|h| h.as_str() == n);
                let aliased = super::sheet_alias::is_aliased(&have, &dec.alias, n);
                if !now.is_empty() && !there && !aliased && wrote(n) {
                    gone.push(n);
                }
            }
            gone.sort_unstable();
            for n in gone {
                report.sheet_issues.push(SheetIssue::missing(n));
            }
        }
    }
    if let Some(b) = &base {
        let have = wb.sheet_names();
        let mut gone: Vec<&str> = b
            .sheet_names()
            .filter(|n| !have.iter().any(|h| h.as_str() == *n) && !super::sheet_alias::is_aliased(&have, &dec.alias, n))
            .collect();
        gone.sort_unstable();
        for n in gone {
            report.sheet_issues.push(SheetIssue::missing(n));
        }
    }
    let al = &Reading { alias: &dec.alias, live: &live };
    let tab_levels = load(&mut wb, LEVELS, LEVELS_COLS, units, &mut report, &base, al);
    let tab_types = load(&mut wb, TYPES, TYPES_COLS, units, &mut report, &base, al);
    let tab_layers = load(&mut wb, LAYERS, LAYERS_COLS, units, &mut report, &base, al);
    let tab_slayers = load(&mut wb, SLAB_LAYERS, SLAB_LAYERS_COLS, units, &mut report, &base, al);
    let tab_mats = load(&mut wb, MATERIALS, MATERIALS_COLS, units, &mut report, &base, al);
    let tab_walls = load(&mut wb, WALLS, WALLS_COLS, units, &mut report, &base, al);
    let tab_ops = load(&mut wb, OPENINGS, OPENINGS_COLS, units, &mut report, &base, al);
    let tab_rooms = load(&mut wb, ROOMS, ROOMS_COLS, units, &mut report, &base, al);
    let tab_slabs = load(&mut wb, SLABS, SLABS_COLS, units, &mut report, &base, al);
    let tab_cws = load(&mut wb, CURTAINS, CURTAINS_COLS, units, &mut report, &base, al);
    let mut tab_params: Vec<Tab> = vec![];
    for ps in &PARAM_SHEETS {
        let Some(mut tab) = load(&mut wb, ps.sheet, ps.cols, units, &mut report, &base, al) else {
            continue;
        };
        match super::extra::family_known(model, ps.sheet, &tab.head)? {
            // the family reader owns the format columns of this layout
            Some(known) => {
                tab.idx.retain(|k, _| *k == "id");
                tab.also_known = known;
            }
            None => param_read_only(model, ps, &tab, &mut report)?,
        }
        tab_params.push(tab);
    }
    let rep = &mut report;
    // an unreadable id cannot be told from a deleted row: this import deletes nothing
    if opts.delete_missing {
        let tabs = [&tab_walls, &tab_ops, &tab_rooms, &tab_slabs, &tab_cws];
        for tab in tabs.into_iter().flatten() {
            if let Some(r) = unreadable_id(tab).or(tab.merged_id) {
                opts.delete_missing = false;
                let row = Row::new(tab, r, &[]);
                rep.rejected.push(Rejection {
                    sheet: tab.name.into(),
                    row: r,
                    col: row.col_of("id"),
                    column: row.header("id"),
                    reason: NO_DELETES.into(),
                });
            }
        }
    }

    // --- Levels ---
    let mut level_names: HashMap<i64, String> =
        levels.iter().map(|(i, v)| (*i, v.0.clone())).collect();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut new_levels: Vec<(String, i64)> = vec![];
    if let Some(tab) = &tab_levels {
        let mut names: Vec<(u32, Option<i64>, String)> = vec![];
        let dups = dup::Dups::plan(tab, dec);
        super::import_gate::sheet()?;
        for (gi, (r, cells)) in tab.rows.iter().enumerate() {
            super::import_gate::rows(gi, tab.rows.len())?;
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let Ok(id) = row.parse_id(rep, "id") else {
                continue;
            };
            row.id = id;
            if !dups.route(&mut row, rep, &mut seen) {
                continue;
            }
            let id = row.id;
            match id {
                Some(id) => {
                    let Some((n0, e0)) = levels.get(&id) else {
                        row.stale(rep, "level", id);
                        continue;
                    };
                    if !seen.insert(id) {
                        row.dup(rep, id);
                        continue;
                    }
                    let c0 = rep.changes.len();
                    let name = row.text(rep, "name", Some(n0));
                    let elevation = row.num(rep, "elevation", Some(*e0));
                    if row.ok && rep.changes.len() > c0 {
                        edits.push(Edit::Level {
                            id,
                            name: name.clone(),
                            elevation,
                        });
                    }
                    // a rejected row leaves the model's name in place
                    if row.ok {
                        level_names.insert(id, name.clone());
                        names.push((*r, Some(id), name));
                    }
                }
                None if row.made(rep) => {}
                None => {
                    let name = row.text(rep, "name", None);
                    let elevation = row.num(rep, "elevation", None);
                    if row.ok {
                        row.row_change(rep, CREATED);
                        edits.push(Edit::NewLevel {
                            name: name.clone(),
                            elevation,
                        });
                        edits.push(Edit::Mark { kind: "level".into(), row: *r });
                        new_levels.push((name.clone(), ddc_model::new_level_ref(*r)));
                        names.push((*r, None, name));
                    }
                }
            }
        }
        let orig: HashMap<i64, String> = levels.iter().map(|(i, v)| (*i, v.0.clone())).collect();
        for (id, n) in dup_names(rep, tab, &names, &mut level_names, &orig, "level") {
            edits.retain(|e| match (e, id) {
                (Edit::Level { id: i, .. }, Some(d)) => *i != d,
                (Edit::NewLevel { name, .. }, None) => *name != n,
                _ => true,
            });
            if id.is_none() {
                new_levels.retain(|(nn, _)| *nn != n);
                let gone: Vec<u32> =
                    names.iter().filter(|(_, i, nn)| i.is_none() && *nn == n).map(|x| x.0).collect();
                edits.retain(|e| match e {
                    Edit::Mark { kind, row } => !(kind == "level" && gone.contains(row)),
                    _ => true,
                });
            }
        }
    }
    let mut level_by_name: HashMap<String, i64> =
        level_names.iter().map(|(i, n)| (n.clone(), *i)).collect();
    // the model's old name of a renamed level still resolves, so rows that were not edited follow
    // (a name taken by a level created in this book stays ambiguous: such rows are unknown)
    for (i, (old, _)) in &levels {
        if !new_levels.iter().any(|(n, _)| n == old) {
            level_by_name.entry(old.clone()).or_insert(*i);
        }
    }
    // levels created in this book resolve through a batch-local reference, so rows of other
    // sheets can sit on them
    for (n, r) in &new_levels {
        if !levels.values().any(|(old, _)| old == n) {
            level_by_name.entry(n.clone()).or_insert(*r);
        }
    }

    // --- WallTypes ---
    let mut type_names = types.clone();
    seen.clear();
    // layer count and total thickness per type: read-only columns, an edit is reported
    let type_ro: HashMap<i64, (f64, f64)> = super::export::query(model, super::export::TYPES_SQL, 4)?
        .into_iter()
        .filter_map(|r| match (r.first(), r.get(2), r.get(3)) {
            (
                Some(super::export::V::N(a)),
                Some(super::export::V::N(n)),
                Some(super::export::V::N(t)),
            ) => Some((*a as i64, (*n, *t))),
            _ => None,
        })
        .collect();
    if let Some(tab) = &tab_types {
        let mut names: Vec<(u32, Option<i64>, String)> = vec![];
        let dups = dup::Dups::plan(tab, dec);
        super::import_gate::sheet()?;
        for (gi, (r, cells)) in tab.rows.iter().enumerate() {
            super::import_gate::rows(gi, tab.rows.len())?;
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let Ok(id) = row.parse_id(rep, "id") else {
                continue;
            };
            row.id = id;
            if !dups.route(&mut row, rep, &mut seen) {
                continue;
            }
            let Some(id) = id else {
                row.reject(
                    rep,
                    "id",
                    "creating wall types from a workbook is not supported",
                );
                continue;
            };
            let Some(n0) = types.get(&id) else {
                row.stale(rep, "wall type", id);
                continue;
            };
            if !seen.insert(id) {
                row.dup(rep, id);
                continue;
            }
            if let Some(&(n, total)) = type_ro.get(&id) {
                row.read_only(rep, "layers", Some(n));
                row.read_only(rep, "total", Some(total));
            }
            let c0 = rep.changes.len();
            let name = row.text(rep, "name", Some(n0));
            if row.ok && rep.changes.len() > c0 {
                edits.push(Edit::WallType {
                    id,
                    name: name.clone(),
                });
            }
            type_names.insert(id, name.clone());
            names.push((*r, Some(id), name));
        }
        for (id, _) in dup_names(rep, tab, &names, &mut type_names, &types, "wall type") {
            edits.retain(|e| !matches!(e, Edit::WallType { id: i, .. } if Some(*i) == id));
        }
    }
    let mut type_by_name: HashMap<String, i64> =
        type_names.iter().map(|(i, n)| (n.clone(), *i)).collect();
    for (i, old) in &types {
        type_by_name.entry(old.clone()).or_insert(*i);
    }

    // materials a layer may name without a warning: Materials rows (as the book names them) and
    // materials in use
    let known_mats: HashSet<String> = model
        .materials()
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.spec.name)
        .chain(tab_mats.iter().flat_map(|t| {
            t.rows.iter().filter_map(move |(r, cells)| match Row::new(t, *r, cells).cell("name") {
                Some(Data::String(n)) => Some(n.trim().to_string()),
                _ => None,
            })
        }))
        .collect();

    // --- Layers ---
    let mut lseen: HashSet<(i64, i64)> = HashSet::new();
    if let Some(tab) = &tab_layers {
        super::import_gate::sheet()?;
        for (gi, (r, cells)) in tab.rows.iter().enumerate() {
            super::import_gate::rows(gi, tab.rows.len())?;
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let (Ok(t), Ok(p)) = (row.parse_id(rep, "wtype"), row.parse_id(rep, "pos")) else {
                continue;
            };
            let (Some(t), Some(p)) = (t, p) else {
                row.reject(
                    rep,
                    "wtype",
                    "creating layers from a workbook is not supported",
                );
                continue;
            };
            // the Baseline keys a layer by type and position
            row.id = Some(layer_key(t, p));
            let Some((m0, th0)) = layers.get(&(t, p)) else {
                row.reject(rep, "pos", format!("unknown layer {p} of type {t}"));
                continue;
            };
            if !lseen.insert((t, p)) {
                row.reject(rep, "pos", format!("duplicate layer {t}/{p}"));
                continue;
            }
            if let Some(n) = types.get(&t) {
                row.read_only_text(rep, "type_name", n);
            }
            let c0 = rep.changes.len();
            let material = row.text(rep, "material", Some(m0));
            let thickness = row.num(rep, "thickness", Some(*th0));
            let thickness = row.in_range(
                rep,
                "thickness",
                thickness,
                Some(*th0),
                thickness > 0.0,
                format!("thickness must be > 0, got {thickness}"),
            );
            if row.ok && rep.changes.len() > c0 {
                row.warn_new_material(rep, &material, m0, &known_mats);
                edits.push(Edit::Layer {
                    wtype: t,
                    pos: p,
                    layer: LayerSpec {
                        material,
                        thickness,
                    },
                });
            }
        }
    }

    // --- SlabTypes (a parameter sheet: its tab is read with them), SlabLayers ---
    // one Edit::SlabType per touched type; ids the parameter reader reports are skipped here
    let old_names: HashMap<i64, String> =
        slab_types.iter().map(|(n, i)| (*i, n.clone())).collect();
    let mut stype_names = old_names.clone();
    let mut touched: BTreeSet<i64> = BTreeSet::new();
    seen.clear();
    let tab_stypes = tab_params.iter().find(|t| t.name == SLAB_TYPES && t.idx.contains_key("name"));
    if let Some(tab) = tab_stypes {
        let mut names: Vec<(u32, Option<i64>, String)> = vec![];
        let mut renamed: Vec<i64> = vec![];
        let dups = dup::Dups::plan(tab, dec);
        for (r, cells) in &tab.rows {
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let mut sink = Report::default();
            let id = match row.parse_id(&mut sink, "id") {
                Ok(Some(id)) => id,
                Ok(None) => {
                    row.reject(rep, "id", "creating slab types from a workbook is not supported");
                    continue;
                }
                Err(()) => continue,
            };
            row.id = Some(id);
            let Some(n0) = old_names.get(&id) else {
                continue;
            };
            if !dups.route(&mut row, rep, &mut seen) {
                continue;
            }
            if !seen.insert(id) {
                row.dup(rep, id);
                continue;
            }
            let c0 = rep.changes.len();
            let name = row.text(rep, "name", Some(n0));
            if row.ok && rep.changes.len() > c0 {
                renamed.push(id);
            }
            stype_names.insert(id, name.clone());
            names.push((*r, Some(id), name));
        }
        let dropped = dup_names(rep, tab, &names, &mut stype_names, &old_names, "slab type");
        touched.extend(renamed.into_iter().filter(|i| !dropped.iter().any(|d| d.0 == Some(*i))));
    }
    let mut slayer_new: HashMap<(i64, i64), LayerSpec> = HashMap::new();
    if let Some(tab) = &tab_slayers {
        let mut lseen: HashSet<(i64, i64)> = HashSet::new();
        for (r, cells) in &tab.rows {
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let (Ok(t), Ok(p)) = (row.parse_id(rep, "stype"), row.parse_id(rep, "pos")) else {
                continue;
            };
            let (Some(t), Some(p)) = (t, p) else {
                row.reject(rep, "stype", "creating slab layers from a workbook is not supported");
                continue;
            };
            if !old_names.contains_key(&t) {
                // an exported layer of a type deleted since is no edit, as on the types sheet
                if tab.base.as_deref().is_some_and(|b| b.has_id(tab.name, layer_key(t, p))) {
                    continue;
                }
                row.reject(rep, "stype", format!("unknown slab type '{t}'"));
                continue;
            }
            let Some((m0, th0)) = slab_layers.get(&(t, p)) else {
                row.reject(
                    rep,
                    "pos",
                    format!("slab type {t} has no layer {p}; adding layers from a workbook is not supported"),
                );
                continue;
            };
            if !lseen.insert((t, p)) {
                row.reject(rep, "pos", format!("duplicate layer {t}/{p}"));
                continue;
            }
            // the Baseline keys a layer by type and position
            row.id = Some(layer_key(t, p));
            if let Some(n) = old_names.get(&t) {
                row.read_only_text(rep, "type_name", n);
            }
            let c0 = rep.changes.len();
            let material = row.text(rep, "material", Some(m0));
            let thickness = row.num(rep, "thickness", Some(*th0));
            let thickness = row.in_range(
                rep,
                "thickness",
                thickness,
                Some(*th0),
                thickness > 0.0,
                format!("thickness must be > 0, got {thickness}"),
            );
            if row.ok && rep.changes.len() > c0 {
                row.warn_new_material(rep, &material, m0, &known_mats);
                slayer_new.insert((t, p), LayerSpec { material, thickness });
                touched.insert(t);
            }
        }
    }
    for t in &touched {
        let mut ls: Vec<(i64, LayerSpec)> = slab_layers
            .iter()
            .filter(|((st, _), _)| st == t)
            .map(|((_, p), (m, th))| {
                let l = slayer_new.get(&(*t, *p)).cloned().unwrap_or(LayerSpec {
                    material: m.clone(),
                    thickness: *th,
                });
                (*p, l)
            })
            .collect();
        ls.sort_by_key(|x| x.0);
        edits.push(Edit::SlabType {
            id: *t,
            name: stype_names[t].clone(),
            layers: ls.into_iter().map(|x| x.1).collect(),
        });
    }
    slab_types = stype_names.iter().map(|(i, n)| (n.clone(), *i)).collect();

    // --- Materials: one edit per touched material, applied after the other edits ---
    let mat_edits = match &tab_mats {
        Some(tab) => materials_edits(model, tab, rep),
        None => vec![],
    };

    // --- Walls ---
    let mut deleted: HashSet<i64> = HashSet::new();
    let mut lens: HashMap<i64, f64> = walls.iter().map(|(i, w)| (*i, w.length())).collect();
    seen.clear();
    if let Some(tab) = &tab_walls {
        let dups = dup::Dups::plan(tab, dec);
        super::import_gate::sheet()?;
        for (gi, (r, cells)) in tab.rows.iter().enumerate() {
            super::import_gate::rows(gi, tab.rows.len())?;
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let Ok(id) = row.parse_id(rep, "id") else {
                continue;
            };
            row.id = id;
            if !dups.route(&mut row, rep, &mut seen) {
                continue;
            }
            let id = row.id;
            let del = row.delete_flag(rep);
            let cur = match id {
                Some(id) => {
                    let Some(w) = walls.get(&id) else {
                        row.stale(rep, "wall", id);
                        continue;
                    };
                    if !seen.insert(id) {
                        row.dup(rep, id);
                        continue;
                    }
                    if del {
                        if row.ok {
                            row.row_change(rep, DELETED);
                            edits.push(Edit::Delete { table: "wall", id });
                            deleted.insert(id);
                        }
                        continue;
                    }
                    Some(w)
                }
                None => {
                    if del {
                        row.reject(rep, "delete", "row has no id; nothing to delete");
                        continue;
                    }
                    None
                }
            };
            let c0 = rep.changes.len();
            let ex = cur.is_some();
            let lname = row.text_alias(
                rep,
                "level",
                cur.map(|w| level_names[&w.level].as_str()),
                cur.and_then(|w| levels.get(&w.level)).map(|l| l.0.as_str()),
            );
            let tname = row.text_alias(
                rep,
                "type",
                cur.map(|w| type_names[&w.wtype].as_str()),
                cur.and_then(|w| types.get(&w.wtype)).map(String::as_str),
            );
            let mut level = 0;
            let mut wtype = 0;
            if row.ok {
                match level_by_name.get(&lname) {
                    Some(l) => level = *l,
                    None => row.reject(rep, "level", format!("unknown level '{lname}'")),
                }
                match type_by_name.get(&tname) {
                    Some(t) => wtype = *t,
                    None => row.reject(rep, "type", format!("unknown wall type '{tname}'")),
                }
            }
            let a = (
                row.num(rep, "ax", cur.map(|w| w.a.0)),
                row.num(rep, "ay", cur.map(|w| w.a.1)),
            );
            let b = (
                row.num(rep, "bx", cur.map(|w| w.b.0)),
                row.num(rep, "by", cur.map(|w| w.b.1)),
            );
            let base_offset =
                row.num_keep(rep, "base_offset", cur.map(|w| w.base_offset), Some(0.0));
            let height = row.num(rep, "height", cur.map(|w| w.height));
            let offset0 = row.num_opt(rep, "offset0", cur.and_then(|w| w.offset0), ex);
            // bulge = tan(angle/4): when coordinates are edited the arc angle is kept, the radius follows the chord.
            let bulge = row
                .num_opt(rep, "bulge", cur.map(|w| w.bulge), ex)
                .unwrap_or(0.0);
            row.read_only(rep, "length", cur.map(|w| w.length()));
            if !row.ok {
                continue;
            }
            let height = row.in_range(
                rep,
                "height",
                height,
                cur.map(|w| w.height),
                height > 0.0,
                format!("height must be > 0, got {height}"),
            );
            let len = axis_length(a, b, bulge);
            if (b.0 - a.0).hypot(b.1 - a.1) <= EPS {
                row.reject(rep, "bx", "wall has zero length");
            } else if let Err(e) = ddc_model::check_bulge(a, b, bulge) {
                row.reject(rep, "bulge", e.to_string());
            }
            if !row.ok {
                continue;
            }
            let spec = WallSpec {
                a,
                b,
                level,
                base_offset,
                height,
                wall_type: wtype,
                offset0,
            };
            match id {
                Some(id) if rep.changes.len() > c0 => {
                    lens.insert(id, len);
                    edits.push(Edit::Wall { id, spec, bulge });
                }
                Some(_) => {}
                None if row.made(rep) => {}
                None => {
                    row.row_change(rep, CREATED);
                    edits.push(Edit::NewWall { spec, bulge });
                    edits.push(Edit::Mark { kind: "wall".into(), row: row.r });
                }
            }
        }
    }

    if tab_walls.is_some() {
        let before = edits.len();
        missing(
            rep,
            WALLS,
            walls.keys().copied(),
            &seen,
            base.as_deref(),
            opts.delete_missing,
            |id| Some(Edit::Delete { table: "wall", id }),
            &mut edits,
        );
        for e in &edits[before..] {
            if let Edit::Delete { table: "wall", id } = e {
                deleted.insert(*id);
            }
        }
    }

    // --- Openings ---
    seen.clear();
    if let Some(tab) = &tab_ops {
        let dups = dup::Dups::plan(tab, dec);
        super::import_gate::sheet()?;
        for (gi, (r, cells)) in tab.rows.iter().enumerate() {
            super::import_gate::rows(gi, tab.rows.len())?;
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let Ok(id) = row.parse_id(rep, "id") else {
                continue;
            };
            row.id = id;
            if !dups.route(&mut row, rep, &mut seen) {
                continue;
            }
            let id = row.id;
            let del = row.delete_flag(rep);
            let cur = match id {
                Some(id) => {
                    let Some(o) = openings.get(&id) else {
                        row.stale(rep, "opening", id);
                        continue;
                    };
                    if !seen.insert(id) {
                        row.dup(rep, id);
                        continue;
                    }
                    if deleted.contains(&o.0) {
                        continue; // goes away together with the wall
                    }
                    if del {
                        if row.ok {
                            row.row_change(rep, DELETED);
                            edits.push(Edit::Delete { table: "opening", id });
                        }
                        continue;
                    }
                    Some(o)
                }
                None => {
                    if del {
                        row.reject(rep, "delete", "row has no id; nothing to delete");
                        continue;
                    }
                    None
                }
            };
            let c0 = rep.changes.len();
            let wall = match row.parse_id(rep, "wall") {
                Ok(Some(w)) => {
                    match cur {
                        Some(o) if o.0 != w => {
                            if row.book_edit(rep, "wall", Some(o.0 as f64), Some(w as f64)) {
                                row.changed(rep, "wall", o.0.to_string(), w.to_string());
                                w
                            } else {
                                o.0
                            }
                        }
                        _ => w,
                    }
                }
                Ok(None) => {
                    row.reject(rep, "wall", "value required");
                    0
                }
                Err(()) => 0,
            };
            let along = row.num(rep, "along", cur.map(|o| o.1));
            let width = row.num(rep, "width", cur.map(|o| o.2));
            let sill = row.num_keep(rep, "sill", cur.map(|o| o.3), Some(0.0));
            let height = row.num(rep, "height", cur.map(|o| o.4));
            let kind = row
                .text(rep, "kind", cur.map(|o| o.5.as_str()))
                .to_ascii_lowercase();
            let cur_sw = cur.map(|o| o.6).unwrap_or_default();
            let mut sw = cur_sw;
            let mut sw_changed = false;
            for (key, opts) in [("swing", ["left", "right"]), ("hinge", ["start", "end"])] {
                let Ok(t) = row.parse_text(rep, key) else {
                    continue;
                };
                let Some(t) = t else { continue };
                let t = t.to_ascii_lowercase();
                let Some(pos) = opts.iter().position(|o| *o == t) else {
                    row.reject(
                        rep,
                        key,
                        format!("'{t}' (expected {} or {})", opts[0], opts[1]),
                    );
                    continue;
                };
                let v = pos == 1;
                let was = if key == "swing" { sw.side } else { sw.hinge };
                if was != v {
                    if cur.is_some() {
                        let a = swing_text(cur_sw);
                        let now = if key == "swing" { a.0 } else { a.1 };
                        if !row.book_edit_text(rep, key, now, &t) {
                            continue;
                        }
                    }
                    let slot = if key == "swing" {
                        &mut sw.side
                    } else {
                        &mut sw.hinge
                    };
                    *slot = v;
                    sw_changed = true;
                    if cur.is_some() {
                        let (a, b) = (swing_text(cur_sw), swing_text(sw));
                        let (o, n) = if key == "swing" {
                            (a.0, b.0)
                        } else {
                            (a.1, b.1)
                        };
                        row.changed(rep, key, o.into(), n.into());
                    }
                }
            }
            if !row.ok {
                continue;
            }
            if !walls.contains_key(&wall) || deleted.contains(&wall) {
                row.reject(rep, "wall", format!("unknown or deleted wall {wall}"));
            }
            if !["door", "window", "void"].contains(&kind.as_str()) {
                row.reject(rep, "kind", format!("kind '{kind}' (door/window/void)"));
            }
            let width = row.in_range(
                rep,
                "width",
                width,
                cur.map(|o| o.2),
                width > 0.0,
                format!("width must be > 0, got {width}"),
            );
            let height = row.in_range(
                rep,
                "height",
                height,
                cur.map(|o| o.4),
                height > 0.0,
                format!("height must be > 0, got {height}"),
            );
            let along = row.in_range(
                rep,
                "along",
                along,
                cur.map(|o| o.1),
                along >= 0.0,
                format!("along must be >= 0, got {along}"),
            );
            let sill = row.in_range(
                rep,
                "sill",
                sill,
                cur.map(|o| o.3),
                sill >= 0.0,
                format!("sill must be >= 0, got {sill}"),
            );
            // an untouched opening row is judged with its wall by the app-side fit check
            let touched = id.is_none() || rep.changes.len() > c0;
            if row.ok && touched {
                if let Some(len) = lens.get(&wall) {
                    if along + width > len + EPS {
                        let t =
                            format!("opening ends at {} beyond wall length {len}", along + width);
                        row.reject(rep, "width", t);
                    }
                }
            }
            if !row.ok {
                continue;
            }
            let spec = OpeningSpec {
                along,
                width,
                sill,
                height,
                kind,
            };
            match id {
                Some(id) if rep.changes.len() > c0 => {
                    edits.push(Edit::Opening { id, wall, spec });
                    if sw_changed {
                        edits.push(Edit::OpeningSwing { id, swing: sw });
                    }
                }
                Some(_) => {}
                None if row.made(rep) => {}
                None => {
                    row.row_change(rep, CREATED);
                    edits.push(Edit::NewOpening {
                        wall,
                        spec,
                        swing: sw,
                    });
                    edits.push(Edit::Mark { kind: "opening".into(), row: row.r });
                }
            }
        }
    }

    if tab_ops.is_some() {
        let ids = openings
            .iter()
            .filter(|(_, o)| !deleted.contains(&o.0))
            .map(|(i, _)| *i);
        missing(
            rep,
            OPENINGS,
            ids,
            &seen,
            base.as_deref(),
            opts.delete_missing,
            |id| Some(Edit::Delete { table: "opening", id }),
            &mut edits,
        );
    }

    // --- Rooms ---
    seen.clear();
    if let Some(tab) = &tab_rooms {
        let explicit = explicit_ids(tab);
        let mut claimed: HashSet<i64> = HashSet::new();
        let mut pending: Vec<(u32, &Vec<Data>, RoomSpec)> = vec![];
        let dups = dup::Dups::plan(tab, dec);
        super::import_gate::sheet()?;
        for (gi, (r, cells)) in tab.rows.iter().enumerate() {
            super::import_gate::rows(gi, tab.rows.len())?;
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let Ok(mut id) = row.parse_id(rep, "id") else {
                continue;
            };
            row.id = id;
            if !dups.route(&mut row, rep, &mut seen) {
                continue;
            }
            id = row.id;
            let del = row.delete_flag(rep);
            let marks = (rep.changes.len(), edits.len());
            let mut km: Option<(i64, String)> = None;
            let rejected = dec.reject.contains(&(tab.name.to_string(), *r));
            if id.is_none() && !del {
                let by = row
                    .key_text("number")
                    .map(|k| (true, k))
                    .or_else(|| row.key_text("name").map(|k| (false, k)));
                if let Some((by_number, key)) = by {
                    let nk = norm_key(&key);
                    let hits: Vec<i64> = rooms
                        .iter()
                        .filter(|(_, c)| norm_key(if by_number { &c.number } else { &c.name }) == nk)
                        .map(|(i, _)| *i)
                        .collect();
                    let taken: HashSet<i64> = explicit.union(&claimed).copied().collect();
                    let found = find_key(&hits, &taken);
                    if let Found::One(one) = found {
                        claimed.insert(one);
                        if rejected {
                            seen.insert(one);
                            note_rejected(rep, tab.name, *r, one, "room", key);
                        } else {
                            id = Some(one);
                            row.id = id;
                            km = Some((one, key));
                        }
                    } else if let Some(why) = key_note(&found) {
                        note_unmatched(rep, tab.name, *r, key, why);
                    }
                }
            }
            let cur = match id {
                Some(id) => {
                    let Some(c) = rooms.get(&id) else {
                        row.stale(rep, "room", id);
                        continue;
                    };
                    if !seen.insert(id) {
                        row.dup(rep, id);
                        continue;
                    }
                    if del {
                        if row.ok {
                            row.row_change(rep, DELETED);
                            edits.push(Edit::DeleteRoom(id));
                        }
                        continue;
                    }
                    Some(c)
                }
                None => {
                    if del {
                        row.reject(rep, "delete", "row has no id; nothing to delete");
                        continue;
                    }
                    None
                }
            };
            let c0 = rep.changes.len();
            let lname = row.text_alias(
                rep,
                "level",
                cur.map(|c| level_names[&c.level].as_str()),
                cur.and_then(|c| levels.get(&c.level)).map(|l| l.0.as_str()),
            );
            let name = row.text(rep, "name", cur.map(|c| c.name.as_str()));
            let number = match row.parse_text(rep, "number") {
                Ok(v) => {
                    let v = v.unwrap_or_default();
                    match cur {
                        Some(c) if v != c.number => {
                            if !row.turned_code(rep, "number", &c.number)
                                && row.book_edit_text(rep, "number", &c.number, &v)
                            {
                                row.changed(rep, "number", c.number.clone(), v.clone());
                                v
                            } else {
                                c.number.clone()
                            }
                        }
                        _ => v,
                    }
                }
                Err(()) => cur.map(|c| c.number.clone()).unwrap_or_default(),
            };
            let px = row.num_keep(rep, "px", cur.map(|c| c.point.0), None);
            let py = row.num_keep(rep, "py", cur.map(|c| c.point.1), None);
            let height = row.num_keep(rep, "height", cur.map(|c| c.height), None);
            if let Some(q) = cur.and_then(|c| c.q) {
                row.read_only(rep, "area", Some(q.0));
                row.read_only(rep, "perimeter", Some(q.1));
                row.read_only(rep, "volume", Some(q.2));
            }
            let mut level = cur.map_or(0, |c| c.level);
            if row.ok {
                match level_by_name.get(&lname) {
                    Some(l) => level = *l,
                    None => row.reject(rep, "level", format!("unknown level '{lname}'")),
                }
            }
            let height = if row.ok {
                row.in_range(
                    rep,
                    "height",
                    height,
                    cur.map(|c| c.height),
                    height > 0.0,
                    format!("height must be > 0, got {height}"),
                )
            } else {
                height
            };
            if !row.ok {
                continue;
            }
            let spec = RoomSpec {
                level,
                name,
                number,
                point: (px, py),
                height,
            };
            match id {
                Some(id) if rep.changes.len() > c0 => edits.push(Edit::Room { id, spec }),
                Some(_) => {}
                None if row.made(rep) => {}
                // checked after the loop, against the walls and rooms the whole book sets
                None => pending.push((*r, cells, spec)),
            }
            if let Some((mid, key)) = km {
                finish_match(rep, &mut edits, marks, tab.name, *r, mid, "room", key, dec);
            }
        }
        // rooms the book drops are gone before new rows are judged
        {
            missing(
                rep,
                ROOMS,
                rooms.keys().copied(),
                &seen,
                base.as_deref(),
                opts.delete_missing,
                |id| Some(Edit::DeleteRoom(id)),
                &mut edits,
            );
        }
        if !pending.is_empty() {
            // a second room in a held region would count its area twice
            let mut cur: HashMap<i64, (i64, (f64, f64), String)> = rooms
                .iter()
                .map(|(id, c)| (*id, (c.level, c.point, format!("{} ({})", c.number, c.name))))
                .collect();
            for e in &edits {
                match e {
                    Edit::Room { id, spec } => {
                        cur.insert(*id, (spec.level, spec.point, format!("{} ({})", spec.number, spec.name)));
                    }
                    Edit::DeleteRoom(id) => {
                        cur.remove(id);
                    }
                    _ => {}
                }
            }
            let mut held: Vec<(i64, (f64, f64), String)> = cur.into_values().collect();
            held.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));
            let geo: Vec<Edit> = edits
                .iter()
                .filter(|e| {
                    matches!(e, Edit::Mark { kind, .. } if kind == "level")
                        || matches!(
                        e,
                        Edit::Level { .. }
                            | Edit::NewLevel { .. }
                            | Edit::WallType { .. }
                            | Edit::Layer { .. }
                            | Edit::Wall { .. }
                            | Edit::NewWall { .. }
                            | Edit::Delete { table: "wall", .. }
                    )
                })
                .cloned()
                .collect();
            let mut levels: Vec<i64> = pending.iter().map(|p| p.2.level).collect();
            levels.sort_unstable();
            levels.dedup();
            let region_cache = staged_regions(model, &geo, &levels);
            for (r, cells, spec) in pending {
                let mut row = Row::new(tab, r, cells);
                let level = spec.level;
                let regions: &[ddc_geom::rooms::Region] = match region_cache.get(&level) {
                    Some(Ok(v)) => v.as_slice(),
                    Some(Err(e)) => {
                        row.reject(rep, "px", format!("cannot check rooms on this level: {e}"));
                        continue;
                    }
                    None => &[],
                };
                let pts: Vec<((f64, f64), String)> =
                    held.iter().filter(|h| h.0 == level).map(|h| (h.1, h.2.clone())).collect();
                // a copied room turned new at its room's point (no closed region there) is the
                // same room twice
                let who = ddc_model::region_holder(regions, &pts, spec.point).or_else(|| {
                    let at = |q: &(f64, f64)| (q.0 - spec.point.0).hypot(q.1 - spec.point.1) < 1e-6;
                    dups.is_new(r).then(|| pts.iter().find(|(q, _)| at(q)).map(|(_, w)| w.clone())).flatten()
                });
                if let Some(who) = who {
                    row.reject(rep, "px", format!("point is inside room {who}"));
                    continue;
                }
                held.push((level, spec.point, format!("{} ({})", spec.number, spec.name)));
                row.row_change(rep, CREATED);
                edits.push(Edit::NewRoom(spec));
                edits.push(Edit::Mark { kind: "room".into(), row: r });
            }
        }
    }

    // --- Slabs ---
    seen.clear();
    if let Some(tab) = &tab_slabs {
        let type_name: HashMap<i64, &str> =
            slab_types.iter().map(|(n, i)| (*i, n.as_str())).collect();
        let dups = dup::Dups::plan(tab, dec);
        super::import_gate::sheet()?;
        for (gi, (r, cells)) in tab.rows.iter().enumerate() {
            super::import_gate::rows(gi, tab.rows.len())?;
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let Ok(id) = row.parse_id(rep, "id") else {
                continue;
            };
            row.id = id;
            if !dups.route(&mut row, rep, &mut seen) {
                continue;
            }
            let id = row.id;
            let del = row.delete_flag(rep);
            let cur = match id {
                Some(id) => {
                    let Some(c) = slabs.get(&id) else {
                        row.stale(rep, "slab", id);
                        continue;
                    };
                    if !seen.insert(id) {
                        row.dup(rep, id);
                        continue;
                    }
                    if del {
                        if row.ok {
                            row.row_change(rep, DELETED);
                            edits.push(Edit::Delete { table: "slab", id });
                        }
                        continue;
                    }
                    Some(c)
                }
                None => {
                    if del {
                        row.reject(rep, "delete", "row has no id; nothing to delete");
                        continue;
                    }
                    None
                }
            };
            let c0 = rep.changes.len();
            let lname = row.text_alias(
                rep,
                "level",
                cur.map(|c| level_names[&c.level].as_str()),
                cur.and_then(|c| levels.get(&c.level)).map(|l| l.0.as_str()),
            );
            let tname = row.text(
                rep,
                "type",
                cur.map(|c| type_name.get(&c.stype).copied().unwrap_or("")),
            );
            let offset = row.num_keep(rep, "offset", cur.map(|c| c.offset), Some(0.0));
            let cur_rings = id.and_then(|i| rings.get(&i));
            let cur_outer = cur_rings.and_then(|r| r.first());
            let mut outer: Option<Vec<(f64, f64)>> = None;
            match row.parse_text(rep, "outline") {
                Ok(Some(t)) => match parse_outline(&t, row.factor("outline")) {
                    Ok(pts) => {
                        let k = row.factor("outline");
                        let unchanged = cur_outer.is_some_and(|o| same_ring(o, &pts))
                            || (cur.is_some()
                                && !row.book_edit_ring(rep, cur_outer.map(|o| o.as_slice()), &pts, k));
                        if !unchanged {
                            if cur.is_some() {
                                let k = row.factor("outline");
                                let old = cur_outer.map_or(String::new(), |o| ring_text(o, k));
                                row.changed(rep, "outline", old, ring_text(&pts, k));
                            }
                            outer = Some(pts);
                        }
                    }
                    Err(e) => row.reject(rep, "outline", e),
                },
                Ok(None) if cur.is_none() => row.reject(rep, "outline", "value required"),
                Ok(None) => {}
                Err(()) => {}
            }
            if let Some(q) = cur.and_then(|c| c.q) {
                row.read_only(rep, "area", Some(q.0));
                let f = id
                    .and_then(|i| model.slab_slope(i).ok().flatten())
                    .map_or(1.0, |p| p.area_factor());
                row.read_only(rep, "surface_area", Some(q.0 * f));
                row.read_only(rep, "volume", Some(q.1));
            }
            let (mut level, mut slab_type) = cur.map_or((0, 0), |c| (c.level, c.stype));
            if row.ok {
                match level_by_name.get(&lname) {
                    Some(l) => level = *l,
                    None => row.reject(rep, "level", format!("unknown level '{lname}'")),
                }
                match slab_types.get(&tname) {
                    Some(t) => slab_type = *t,
                    None => row.reject(rep, "type", format!("unknown slab type '{tname}'")),
                }
            }
            if !row.ok {
                continue;
            }
            match (id, cur) {
                (Some(id), Some(c)) if rep.changes.len() > c0 => {
                    if level != c.level || slab_type != c.stype || !same(offset, c.offset) {
                        edits.push(Edit::Slab {
                            id,
                            level,
                            slab_type,
                            offset,
                        });
                    }
                    if let Some(outer) = outer {
                        let holes = cur_rings.map_or(vec![], |r| r[1.min(r.len())..].to_vec());
                        edits.push(Edit::SlabBoundary { id, outer, holes });
                    }
                }
                (Some(_), _) => {}
                (None, _) if row.made(rep) => {}
                (None, _) => {
                    row.row_change(rep, CREATED);
                    edits.push(Edit::NewSlab(SlabSpec {
                        level,
                        slab_type,
                        offset,
                        outer: outer.unwrap_or_default(),
                        holes: vec![],
                    }));
                    edits.push(Edit::Mark { kind: "slab".into(), row: row.r });
                }
            }
        }
        {
            missing(
                rep,
                SLABS,
                slabs.keys().copied(),
                &seen,
                base.as_deref(),
                opts.delete_missing,
                |id| Some(Edit::Delete { table: "slab", id }),
                &mut edits,
            );
        }
    }

    // --- Curtain walls ---
    seen.clear();
    if let Some(tab) = &tab_cws {
        let explicit = explicit_ids(tab);
        let mut claimed: HashSet<i64> = HashSet::new();
        let dups = dup::Dups::plan(tab, dec);
        super::import_gate::sheet()?;
        for (gi, (r, cells)) in tab.rows.iter().enumerate() {
            super::import_gate::rows(gi, tab.rows.len())?;
            let mut row = Row::new(tab, *r, cells);
            if row.empty() {
                continue;
            }
            let Ok(mut id) = row.parse_id(rep, "id") else {
                continue;
            };
            row.id = id;
            if !dups.route(&mut row, rep, &mut seen) {
                continue;
            }
            id = row.id;
            let del = row.delete_flag(rep);
            let marks = (rep.changes.len(), edits.len());
            let mut km: Option<(i64, String)> = None;
            let rejected = dec.reject.contains(&(tab.name.to_string(), *r));
            if id.is_none() && !del {
                if let Some(key) = row.key_text("name") {
                    let nk = norm_key(&key);
                    let hits: Vec<i64> = curtains
                        .iter()
                        .filter(|(_, c)| norm_key(&c.name) == nk)
                        .map(|(i, _)| *i)
                        .collect();
                    let taken: HashSet<i64> = explicit.union(&claimed).copied().collect();
                    let found = find_key(&hits, &taken);
                    if let Found::One(one) = found {
                        claimed.insert(one);
                        if rejected {
                            seen.insert(one);
                            note_rejected(rep, tab.name, *r, one, "curtain_wall", key);
                        } else {
                            id = Some(one);
                            row.id = id;
                            km = Some((one, key));
                        }
                    } else if let Some(why) = key_note(&found) {
                        note_unmatched(rep, tab.name, *r, key, why);
                    }
                }
            }
            let cur = match id {
                Some(id) => {
                    let Some(c) = curtains.get(&id) else {
                        row.stale(rep, "curtain wall", id);
                        continue;
                    };
                    if !seen.insert(id) {
                        row.dup(rep, id);
                        continue;
                    }
                    if del {
                        if row.ok {
                            row.row_change(rep, DELETED);
                            edits.push(Edit::DeleteCurtain(id));
                        }
                        continue;
                    }
                    Some(c)
                }
                None => {
                    if del {
                        row.reject(rep, "delete", "row has no id; nothing to delete");
                        continue;
                    }
                    None
                }
            };
            let c0 = rep.changes.len();
            let lname = row.text_alias(
                rep,
                "level",
                cur.map(|c| level_names[&c.level].as_str()),
                cur.and_then(|c| levels.get(&c.level)).map(|l| l.0.as_str()),
            );
            let name = if cur.is_none() && row.cell("name").is_none() {
                "Curtain wall".to_string()
            } else {
                row.text(rep, "name", cur.map(|c| c.name.as_str()))
            };
            let base_offset =
                row.num_keep(rep, "base_offset", cur.map(|c| c.base_offset), Some(0.0));
            let grid = |row: &mut Row, rep: &mut Report, key: &str, cur: Option<&GridLines>| {
                let parsed = match row.parse_text(rep, key) {
                    Ok(Some(t)) => match GridLines::parse(&t) {
                        Ok(g) => Some(g),
                        Err(e) => {
                            row.reject(rep, key, e.to_string());
                            None
                        }
                    },
                    _ => None,
                };
                match (cur, parsed) {
                    (Some(c), Some(g)) if *c != g => {
                        // the book still has the exported grid: the editor changed it since
                        let was = match row.base_val(key) {
                            Some(BVal::S(t)) => GridLines::parse(t).ok(),
                            _ => None,
                        };
                        if was.as_ref() == Some(&g) {
                            rep.model_newer += 1;
                            return c.clone();
                        }
                        row.reject(
                            rep,
                            key,
                            "grid of an existing curtain wall is edited in the editor",
                        );
                        c.clone()
                    }
                    (Some(c), _) => c.clone(),
                    (None, g) => g.unwrap_or(GridLines::Spacing(1.5)),
                }
            };
            let u = grid(&mut row, rep, "ugrid", cur.map(|c| &c.u));
            let v = grid(&mut row, rep, "vgrid", cur.map(|c| &c.v));
            let spec = match cur {
                Some(c) => {
                    for (key, val) in [("ax", c.a.0), ("ay", c.a.1), ("bx", c.b.0), ("by", c.b.1)] {
                        row.locked(rep, key, val, "axis");
                    }
                    row.locked(rep, "bulge", c.bulge, "arc");
                    row.locked(rep, "height", c.height, "height");
                    let q = model.curtain_quantities(id.unwrap_or(0)).ok();
                    if let Some(q) = q {
                        row.read_only(rep, "area", Some(q.gross_area));
                        row.read_only(rep, "glass", Some(q.glass_area));
                        row.read_only(rep, "panels", Some(q.panels as f64));
                        row.read_only(rep, "mullions", Some(q.mullion_length));
                    }
                    CurtainSpec {
                        base_offset,
                        name: name.clone(),
                        ..c.clone()
                    }
                }
                None => {
                    let a = (row.num(rep, "ax", None), row.num(rep, "ay", None));
                    let b = (row.num(rep, "bx", None), row.num(rep, "by", None));
                    let bulge = row.num_keep(rep, "bulge", None, Some(0.0));
                    let height = row.num(rep, "height", None);
                    if row.ok && height <= 0.0 {
                        row.reject(rep, "height", format!("height must be > 0, got {height}"));
                    }
                    if row.ok && (b.0 - a.0).hypot(b.1 - a.1) <= EPS {
                        row.reject(rep, "bx", "curtain wall has zero length");
                    }
                    if row.ok && bulge.abs() > 1.0 + 1e-12 {
                        row.reject(rep, "bulge", "arc over a half circle");
                    }
                    let sec = MullionSection::Rect { w: 0.05, d: 0.15 };
                    CurtainSpec {
                        level: 0,
                        name: name.clone(),
                        a,
                        b,
                        bulge,
                        base_offset,
                        height,
                        u,
                        v,
                        interior: sec,
                        border: sec,
                        lines: Default::default(),
                        top: vec![],
                    }
                }
            };
            let mut level = cur.map_or(0, |c| c.level);
            if row.ok {
                match level_by_name.get(&lname) {
                    Some(l) => level = *l,
                    None => row.reject(rep, "level", format!("unknown level '{lname}'")),
                }
            }
            if !row.ok {
                continue;
            }
            match id {
                Some(id) if rep.changes.len() > c0 => edits.push(Edit::CurtainProps {
                    id,
                    level,
                    name,
                    base_offset,
                }),
                Some(_) => {}
                None if row.made(rep) => {}
                None => {
                    row.row_change(rep, CREATED);
                    edits.push(Edit::NewCurtain(CurtainSpec { level, ..spec }));
                    edits.push(Edit::Mark { kind: "curtain_wall".into(), row: row.r });
                }
            }
            if let Some((mid, key)) = km {
                finish_match(rep, &mut edits, marks, tab.name, *r, mid, "curtain_wall", key, dec);
            }
        }
        {
            missing(
                rep,
                CURTAINS,
                curtains.keys().copied(),
                &seen,
                base.as_deref(),
                opts.delete_missing,
                |id| Some(Edit::DeleteCurtain(id)),
                &mut edits,
            );
        }
    }

    // parameters, stages, new columns; the Quantities sheet is verification only
    let tabs: Vec<&Tab> = [&tab_types, &tab_walls, &tab_ops, &tab_rooms, &tab_slabs, &tab_cws]
        .into_iter()
        .filter_map(|t| t.as_ref())
        .chain(&tab_params)
        .collect();
    // parameters follow the row each sheet read for a repeated id
    let routed: Vec<_> = tabs.iter().map(|t| dup::view_rows(t, dec)).collect();
    let views: Vec<super::extra::SheetView> = tabs
        .iter()
        .zip(&routed)
        .map(|(t, rows)| super::extra::SheetView { rows: rows.as_ref(), ..t.view() })
        .collect();
    super::extra::import_extra(model, &views, units, opts.new_params, rep, &mut edits)?;
    if let Ok(q) = wb.worksheet_range(QUANTITIES) {
        super::extra::quantities(model, &q, units, rep)?;
    }
    // the summary is checked against the model only while it is as exported
    if let Ok(q) = wb.worksheet_range(SUMMARY) {
        let rev = wb.worksheet_range(INFO).ok().and_then(|i| {
            i.rows()
                .find(|r| r.first().is_some_and(|d| d.to_string() == "revision"))
                .and_then(|r| r.get(1))
                // the export writes the revision as a plain integer: invariant form
                .and_then(|d| ddc_model::num_text::parse_invariant(&d.to_string()))
        });
        if rev.is_some_and(|v| v == model.change_head() as f64) {
            super::extra::summary(model, &q, units, rep)?;
        }
    }

    // materials last: other edits snapshot rows before a rename repoints them (one clean undo);
    // a layer naming a renamed material's new name is written with the old one, then repointed
    let renamed: HashMap<String, String> = mat_edits
        .iter()
        .filter_map(|e| match e {
            Edit::Material { name, spec } if spec.name != *name => {
                Some((spec.name.clone(), name.clone()))
            }
            _ => None,
        })
        .collect();
    for e in edits.iter_mut() {
        let layers: &mut [LayerSpec] = match e {
            Edit::Layer { layer, .. } => std::slice::from_mut(layer),
            Edit::SlabType { layers, .. } => layers.as_mut_slice(),
            _ => continue,
        };
        for l in layers {
            if let Some(old) = renamed.get(&l.material) {
                l.material = old.clone();
            }
        }
    }
    edits.extend(mat_edits);
    report.edits = edits;
    // a pinned element is skipped, with a reason, in the preview as well as on apply
    if let Ok((keep, pinned)) = model.split_pinned(&report.edits) {
        if !pinned.is_empty() {
            report.edits = keep;
        }
        for (k, id) in pinned {
            let sheet = super::extra::sheet_of_kind(k.as_str()).unwrap_or("");
            report
                .changes
                .retain(|c| !(c.id == Some(id) && c.sheet == sheet));
            report.ignored.push(Ignored {
                sheet: "(model)".into(),
                row: 0,
                col: 0,
                column: String::new(),
                id: Some(id),
                old: String::new(),
                new: String::new(),
                reason: format!("{k} {id} is pinned: skipped"),
            });
        }
    }
    // a locked parameter value is never written: its cell is skipped, with the reason
    if let Ok((keep, locked)) = model.split_locked(&report.edits) {
        if !locked.is_empty() {
            report.edits = keep;
        }
        for (k, id, name) in locked {
            let head = units::norm(&name);
            let on_sheet = |s: &str| {
                super::extra::SHEET_CATS
                    .iter()
                    .any(|c| c.kind == k && c.sheet == s)
            };
            let at = report.changes.iter().position(|c| {
                c.id == Some(id) && on_sheet(&c.sheet) && units::norm(&c.column) == head
            });
            let cell = match at {
                Some(i) => report.changes.remove(i),
                None => Change {
                    sheet: "(model)".into(),
                    row: 0,
                    col: 0,
                    column: name.clone(),
                    id: Some(id),
                    old: String::new(),
                    new: String::new(),
                },
            };
            report.ignored.push(Ignored {
                sheet: cell.sheet,
                row: cell.row,
                col: cell.col,
                column: cell.column,
                id: Some(id),
                old: cell.old,
                new: cell.new,
                reason: format!("value of '{name}' is locked: skipped"),
            });
        }
    }
    if report.blocking() > 0 || !opts.apply {
        return Ok(report);
    }
    let edits = report.batch();
    // accepted by-key proposals were not split above: their locked values are skipped too
    let edits = match model.split_locked(&edits) {
        Ok((keep, locked)) => {
            for (_, id, name) in locked {
                report.ignored.push(Ignored {
                    sheet: "(model)".into(),
                    row: 0,
                    col: 0,
                    column: name.clone(),
                    id: Some(id),
                    old: String::new(),
                    new: String::new(),
                    reason: format!("value of '{name}' is locked: skipped"),
                });
            }
            keep
        }
        Err(_) => edits,
    };
    if !edits.is_empty() {
        if let Err(e) = model.apply_edits(&edits) {
            report.rejected.push(Rejection {
                sheet: "(model)".into(),
                row: 0,
                col: 0,
                column: String::new(),
                reason: e.to_string(),
            });
            return Ok(report);
        }
    }
    report.applied = true;
    Ok(report)
}

#[cfg(test)]
#[path = "tests_types.rs"]
mod tests_types;

#[path = "dup.rs"]
mod dup;

#[cfg(test)]
#[path = "tests_dup.rs"]
mod tests_dup;

#[cfg(test)]
mod tests_outline {
    use super::*;

    #[test]
    fn exported_ring_reads_the_same_under_any_book_mark() {
        let ring = [(5.125, 0.0), (6.25, 0.0), (6.25, 1.5), (5.125, 1.5)];
        let text = ring_text(&ring, 1.0);
        let marks = [Some(units::Dec::Comma), Some(units::Dec::Point), None];
        for dec in marks {
            let _g = units::DecGuard::set(dec);
            let got = parse_outline(&text, 1.0).unwrap();
            assert!(same_ring(&got, &ring), "{dec:?}: {got:?}");
        }
    }

    #[test]
    fn typed_comma_coordinate_still_follows_the_book() {
        let _g = units::DecGuard::set(Some(units::Dec::Comma));
        let got = parse_outline("0 0; 1,5 0; 1,5 1,5", 1.0).unwrap();
        assert!(got.contains(&(1.5, 1.5)), "{got:?}");
    }
}
