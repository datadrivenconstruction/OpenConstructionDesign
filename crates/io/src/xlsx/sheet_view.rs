// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! How the user arranged the app's own sheets in the old book, carried by a re-export: the row
//! order, hidden rows and columns, column widths and row heights. Columns are matched by their
//! normalised header (through the Keys row when the book has one), rows by element id (by type
//! and position on layer sheets). What this cannot carry (filters, sort state, hidden columns
//! without a header) is listed by `loss.rs`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use calamine::{open_workbook, Data, Range, Reader, Xlsx};
use rust_xlsxwriter::Worksheet;

use super::export::{Rows, V};
use super::extra::{cols_of, XCol};
use super::units;
use super::*;

/// One column of the old book as the user left it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ColView {
    pub hidden: bool,
    /// Custom width in the writer's units (`set_column_width`); None = the writer's own.
    pub width: Option<f64>,
}

/// One app sheet of the old book as the user left it.
#[derive(Clone, Debug, Default)]
pub(crate) struct View {
    /// The sheet has a readable row key column (id, or type and position).
    pub keyed: bool,
    /// Row keys of the data rows, top to bottom, each once.
    pub order: Vec<i64>,
    pub hidden_rows: HashSet<i64>,
    /// Custom row heights in points by row key.
    pub heights: HashMap<i64, f64>,
    /// Custom height of the header row.
    pub head_height: Option<f64>,
    /// Normalised header -> column state; only columns with something to carry.
    pub cols: HashMap<String, ColView>,
    /// Calculated formulas the user put in the app's own columns, by (column key, row key).
    pub formulas: HashMap<(String, i64), FCell>,
    /// Header of each column key in `formulas` as the old book shows it.
    pub fheads: HashMap<String, String>,
    /// Columns hidden from past the header to the sheet end; the value is the count of visible
    /// columns between the header's last column and the hidden ones.
    pub hidden_tail: Option<u32>,
}

/// A formula of an app column as the old book holds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FCell {
    /// Formula text with the leading "=".
    pub text: String,
    /// Its result as the book stores it (book units).
    pub cached: Data,
    /// 1-based sheet row it sat on.
    pub row: u32,
}

thread_local! {
    /// Views of the old book of the export running on this thread, by sheet.
    static VIEWS: RefCell<Rc<HashMap<String, View>>> = RefCell::new(Rc::new(HashMap::new()));
    /// Last parsed books by (path, size, mtime): the export and its loss check read each once.
    static CACHE: RefCell<Vec<(PathBuf, Stamp, Rc<HashMap<String, View>>)>> = const { RefCell::new(Vec::new()) };
}

type Stamp = (u64, Option<web_time::SystemTime>);

/// [`read_book`] once per book state: a changed or rewritten file (size or mtime) is read again.
pub(crate) fn cached(path: &Path) -> Rc<HashMap<String, View>> {
    let Some(stamp) = std::fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok())) else {
        return Rc::new(HashMap::new());
    };
    if let Some(v) = CACHE.with(|c| {
        c.borrow().iter().find(|(p, s, _)| p == path && *s == stamp).map(|x| x.2.clone())
    }) {
        return v;
    }
    let v = Rc::new(read_book(path));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.retain(|(p, _, _)| p != path);
        if c.len() >= 2 {
            c.remove(0);
        }
        c.push((path.to_path_buf(), stamp, v.clone()));
    });
    v
}

/// Sheets the writer lays out through `export::sheet`.
fn written_here(name: &str) -> bool {
    matches!(
        name,
        LEVELS | TYPES | LAYERS | SLAB_LAYERS | MATERIALS | WALLS | OPENINGS | ROOMS | SLABS
            | CURTAINS | QUANTITIES | SUMMARY
    ) || param_sheet(name).is_some()
}

fn is_layer(name: &str) -> bool {
    name == LAYERS || name == SLAB_LAYERS
}

/// Reads the view of `old` for the export starting on this thread; a missing book, a book
/// without Info or an unreadable one gives no view (the first export is unchanged).
pub(crate) fn begin(old: &Path) {
    let v = cached(old);
    VIEWS.with(|x| *x.borrow_mut() = v);
}

/// Views of the app sheets of `path`, by sheet; empty for a book this app did not write.
pub(crate) fn read_book(path: &Path) -> HashMap<String, View> {
    let mut out = HashMap::new();
    if !path.exists() {
        return out;
    }
    let wb: Result<Xlsx<std::io::BufReader<std::fs::File>>, _> = open_workbook(path);
    let Ok(mut wb) = wb else {
        return out;
    };
    let names = wb.sheet_names();
    if !names.iter().any(|n| n == INFO) {
        return out;
    }
    let keys = super::book::read_keys(&mut wb);
    let xmls = super::loss::sheet_xmls(path, written_here);
    for name in names.iter().filter(|n| written_here(n)) {
        let (Ok(vals), Some(xml)) = (wb.worksheet_range(name), xmls.get(name)) else {
            continue;
        };
        let forms = wb.worksheet_formula(name).ok();
        if let Some(v) = view_of(name, &vals, &keys, xml, forms.as_ref()) {
            out.insert(name.clone(), v);
        }
    }
    out
}

fn int_of(d: &Data) -> Option<i64> {
    let v = match d {
        Data::Float(x) => *x,
        Data::Int(i) => *i as f64,
        // ids are written as plain integers: only the invariant form is an id
        Data::String(s) => ddc_model::num_text::parse_invariant(s)?,
        _ => return None,
    };
    (v.is_finite() && v.fract() == 0.0 && v.abs() < 9e15).then_some(v as i64)
}

fn on(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true"))
}

/// Width as the writer takes it from a stored `<col width>`: the inverse of its conversion
/// (7 px digit, 5 px padding), so the width stays the same over any number of re-exports.
pub(crate) fn user_width(stored: f64) -> f64 {
    let px = (stored * 7.0).round();
    if px >= 12.0 {
        (px - 5.0) / 7.0
    } else {
        px / 12.0
    }
}

fn view_of(
    name: &str,
    vals: &Range<Data>,
    keys: &super::book::KeyRows,
    xml: &str,
    forms: Option<&Range<String>>,
) -> Option<View> {
    let (r0, c0) = vals.start()?;
    let end = vals.end()?;
    let cols = cols_of(name);
    let known: Vec<String> = cols.iter().map(|c| units::norm(c.header)).collect();
    // the header row is found the way the import and the kept columns find it
    let h = if known.is_empty() {
        0
    } else {
        super::book::find_header(vals, &known, |row| {
            super::book::resolve(keys, name, cols, c0 as usize, row)
                .texts
                .iter()
                .map(|t| units::norm(t))
                .collect()
        })
    };
    let hr = r0 + h as u32;
    let raw: Vec<String> = (c0..=end.1)
        .map(|c| vals.get_value((hr, c)).map_or(String::new(), |d| d.to_string().trim().to_string()))
        .collect();
    let heads = super::book::resolve(keys, name, cols, c0 as usize, &raw).texts;
    let head: Vec<(u32, String)> = (c0..=end.1).zip(heads.iter().map(|t| units::norm(t))).collect();
    let at = |n: &str| head.iter().find(|h| h.1 == n).map(|h| h.0);
    let layer = is_layer(name);
    let idc = at(if layer { "type_id" } else { "id" });
    let pc = if layer { at("pos") } else { None };
    let key_at = |r: u32| -> Option<i64> {
        let id = vals.get_value((r, idc?)).and_then(int_of)?;
        if !layer {
            return Some(id);
        }
        Some(layer_key(id, vals.get_value((r, pc?)).and_then(int_of)?))
    };
    let mut v = View {
        keyed: idc.is_some() && (!layer || pc.is_some()),
        ..View::default()
    };
    if v.keyed {
        let mut seen = HashSet::new();
        v.order = (hr + 1..=end.0).filter_map(&key_at).filter(|k| seen.insert(*k)).collect();
        if let Some(fr) = forms {
            app_formulas(&mut v, cols, &head, &raw, (hr, c0, end.0), vals, fr, &key_at);
        }
    }
    let mut hidden_ranges: Vec<(u32, u32)> = vec![];
    for (tag, t) in super::loss::all_tags(xml) {
        let a = |k: &str| super::loss::attr(t, k);
        match tag {
            "row" => {
                let Some(r) = a("r").and_then(|s| s.parse::<u32>().ok()).filter(|r| *r > 0) else {
                    continue;
                };
                let r = r - 1;
                // sheet XML attributes are invariant numbers
                let ht = a("ht")
                    .filter(|_| on(a("customHeight")))
                    .and_then(ddc_model::num_text::parse_invariant)
                    .filter(|h| h.is_finite() && *h > 0.0);
                if r == hr {
                    v.head_height = ht;
                    continue;
                }
                if r < hr {
                    continue;
                }
                let Some(k) = key_at(r) else {
                    continue;
                };
                if on(a("hidden")) {
                    v.hidden_rows.insert(k);
                }
                if let Some(h) = ht {
                    v.heights.insert(k, h);
                }
            }
            "col" => {
                let n = |k: &str| a(k).and_then(|s| s.parse::<u32>().ok()).filter(|x| *x > 0);
                let (Some(lo), Some(hi)) = (n("min"), n("max")) else {
                    continue;
                };
                let hidden = on(a("hidden"));
                let width = a("width")
                    .filter(|_| on(a("customWidth")))
                    .and_then(ddc_model::num_text::parse_invariant)
                    .filter(|w| w.is_finite() && *w > 0.0)
                    .map(user_width);
                if !hidden && width.is_none() {
                    continue;
                }
                if hidden {
                    hidden_ranges.push((lo, hi));
                }
                // a range up to the last sheet column stops at the header's last cell
                for c in lo - 1..=(hi - 1).min(end.1) {
                    if let Some((_, h)) = head.iter().find(|h| h.0 == c).filter(|h| !h.1.is_empty()) {
                        v.cols.insert(h.clone(), ColView { hidden, width });
                    }
                }
            }
            _ => {}
        }
    }
    note_hidden_tail(&mut v, &head, end.1, &hidden_ranges);
    Some(v)
}

/// Collects the calculated formulas of the app's own columns (not the row key columns). A
/// never-calculated or error formula is left to the import's refusal and the kept-cell path.
#[allow(clippy::too_many_arguments)]
fn app_formulas(
    v: &mut View,
    cols: &[Col],
    head: &[(u32, String)],
    raw: &[String],
    (hr, c0, last): (u32, u32, u32),
    vals: &Range<Data>,
    forms: &Range<String>,
    key_at: &dyn Fn(u32) -> Option<i64>,
) {
    for (c, h) in head {
        let Some(col) = cols.iter().find(|x| !h.is_empty() && units::norm(x.header) == *h) else {
            continue;
        };
        if matches!(col.key, "id" | "pos" | "wtype" | "stype") || matches!(h.as_str(), "id" | "type_id" | "pos") {
            continue;
        }
        for r in hr + 1..=last {
            let Some(f) = forms.get_value((r, *c)).map(|f| f.trim()).filter(|f| !f.is_empty()) else {
                continue;
            };
            let cached = vals.get_value((r, *c)).cloned().unwrap_or(Data::Empty);
            if matches!(cached, Data::Empty | Data::Error(_)) || super::extra::error_const(f).is_some() {
                continue;
            }
            let Some(k) = key_at(r) else {
                continue;
            };
            let shown = raw.get((c - c0) as usize).cloned().unwrap_or_default();
            v.fheads.entry(col.key.to_string()).or_insert(shown);
            let text = format!("={}", f.trim_start_matches('='));
            v.formulas.entry((col.key.to_string(), k)).or_insert(FCell { text, cached, row: r + 1 });
        }
    }
}

/// Book text of the model value `v` (cell units: model value / `k`) when it still equals the
/// formula's stored result, compared as the import compares; None when the model moved on.
fn same_result(cached: &Data, v: &V, k: f64) -> Option<String> {
    let k = if k.is_finite() && k != 0.0 { k } else { 1.0 };
    match v {
        V::N(x) => {
            let c = match cached {
                Data::Float(c) => *c,
                Data::Int(c) => *c as f64,
                // a text result counts only in the invariant form; any other text is "moved on"
                Data::String(s) => ddc_model::num_text::parse_invariant(s)?,
                _ => return None,
            };
            let (a, b) = (c * k, *x);
            ((a - b).abs() <= 1e-9 * (1.0 + a.abs().max(b.abs()))).then(|| (*x / k).to_string())
        }
        V::S(s) => {
            let t = match cached {
                Data::String(t) => t.clone(),
                Data::Float(_) | Data::Int(_) | Data::Bool(_) => cached.to_string(),
                _ => return None,
            };
            // a text past the cell limit is written cut (cellsafe.rs), never as a result
            // an empty result reads back as never calculated (the import refuses it)
            if !units::same_text(s, &t) || s.trim().is_empty() || s.chars().count() > 32_767 {
                return None;
            }
            // the writer stores a result that reads as a number as a number: "007" would come
            // back as 7, so such a text is written as a value (and listed by the loss check);
            // deliberately the std float parse: it mirrors the writer's own test, not user input
            match s.trim().parse::<f64>() {
                Ok(n) if !(n.is_finite() && n.to_string() == s.trim()) => None,
                _ => Some(s.clone()),
            }
        }
        _ => None,
    }
}

/// The old book held formulas in the app columns of `name` (cheap check before `live_formula`).
pub(crate) fn has_formulas(name: &str) -> bool {
    VIEWS.with(|v| v.borrow().get(name).is_some_and(|v| !v.formulas.is_empty()))
}

/// The formula the old book held in app column `key` of `row`, moved to the 1-based sheet row
/// `at`, with the result to store; None when there was none or the model value changed since.
pub(crate) fn live_formula(
    name: &str,
    cols: &[Col],
    key: &str,
    row: &[V],
    at: u32,
    k: f64,
    v: &V,
) -> Option<(String, String)> {
    let rk = row_key(name, cols, row)?;
    VIEWS.with(|x| {
        let b = x.borrow();
        let f = b.get(name)?.formulas.get(&(key.to_string(), rk))?;
        let res = same_result(&f.cached, v, k)?;
        Some((super::extra::shift_row(&f.text, f.row, at), res))
    })
}

/// Formulas of app columns in `old` that `new` holds as plain values on rows both books have:
/// (sheet, header as the old book shows it, count), sorted.
pub(crate) fn formulas_dropped(
    old: &HashMap<String, View>,
    new: &HashMap<String, View>,
) -> Vec<(String, String, usize)> {
    let mut by: std::collections::BTreeMap<(String, String), usize> = Default::default();
    for (sheet, a) in old {
        let Some(b) = new.get(sheet) else {
            continue;
        };
        let live: HashSet<i64> = b.order.iter().copied().collect();
        for (key, rk) in a.formulas.keys() {
            if live.contains(rk) && !b.formulas.contains_key(&(key.clone(), *rk)) {
                let shown = a.fheads.get(key).cloned().unwrap_or_else(|| key.clone());
                *by.entry((sheet.clone(), shown)).or_default() += 1;
            }
        }
    }
    by.into_iter().map(|((s, h), n)| (s, h, n)).collect()
}

/// Keeps "hide everything right of the table": hidden `<col min max>` ranges (1-based) that
/// reach the sheet end past the header's last column (`last` when the header row is empty).
/// The writer splits one hidden run where column styles change, so adjoining ranges are one.
fn note_hidden_tail(v: &mut View, head: &[(u32, String)], last: u32, ranges: &[(u32, u32)]) {
    let last = head.iter().rev().find(|h| !h.1.is_empty()).map_or(last, |h| h.0);
    let Some(&(mut lo, hi)) = ranges.iter().find(|r| r.1 >= super::loss::LAST_COL) else {
        return;
    };
    // `r.0 < lo`: a malformed book with min > max cannot loop here
    while let Some(prev) = ranges.iter().find(|r| r.1 + 1 == lo && r.0 < lo) {
        lo = prev.0;
    }
    if hi - 1 > last {
        v.hidden_tail = Some((lo - 1).saturating_sub(last + 1));
    }
}

/// Re-applies [`View::hidden_tail`] after the `width` written columns of the new sheet.
fn apply_hidden_tail(ws: &mut Worksheet, gap: u32, width: usize) -> Result<(), rust_xlsxwriter::XlsxError> {
    let first = width as u32 + gap;
    if first < super::loss::LAST_COL {
        ws.set_column_range_hidden(first as u16, (super::loss::LAST_COL - 1) as u16)?;
    }
    Ok(())
}

/// Row key the reader uses for the same row: its id, or (type, position) on layer sheets.
fn row_key(name: &str, cols: &[Col], row: &[V]) -> Option<i64> {
    let int = |v: Option<&V>| match v {
        Some(V::N(x)) if x.is_finite() && x.fract() == 0.0 => Some(*x as i64),
        _ => None,
    };
    if is_layer(name) {
        // the old book's key of the same layer (layer_match.rs): marks follow their own layer
        return super::layer_match::old_key(name, layer_key(int(row.first())?, int(row.get(1))?));
    }
    if cols.first().map(|c| c.key) != Some("id") {
        return None;
    }
    int(row.first())
}

/// New position -> index into `keys`: rows the old book had come first in its order, the rest
/// follow in their given (model, id) order. None when nothing moves.
pub(crate) fn placement(old: &[i64], keys: &[Option<i64>]) -> Option<Vec<usize>> {
    let mut rank: HashMap<i64, usize> = HashMap::with_capacity(old.len());
    for (i, k) in old.iter().enumerate() {
        rank.entry(*k).or_insert(i);
    }
    let mut known: Vec<(usize, usize)> = vec![];
    let mut fresh: Vec<usize> = vec![];
    for (i, k) in keys.iter().enumerate() {
        match k.and_then(|k| rank.get(&k)) {
            Some(r) => known.push((*r, i)),
            None => fresh.push(i),
        }
    }
    known.sort_unstable();
    let p: Vec<usize> = known.into_iter().map(|x| x.1).chain(fresh).collect();
    p.iter().enumerate().any(|(i, j)| i != *j).then_some(p)
}

/// [`placement`] for layer sheets: a new layer goes right below the row above it in model order
/// (first when it is the first row), so it stays with its type.
pub(crate) fn placement_near(old: &[i64], keys: &[Option<i64>]) -> Option<Vec<usize>> {
    let mut rank: HashMap<i64, usize> = HashMap::with_capacity(old.len());
    for (i, k) in old.iter().enumerate() {
        rank.entry(*k).or_insert(i);
    }
    let at = |k: &Option<i64>| k.and_then(|k| rank.get(&k).copied());
    let mut known: Vec<(usize, usize)> = keys.iter().enumerate().filter_map(|(i, k)| at(k).map(|r| (r, i))).collect();
    known.sort_unstable();
    let mut p: Vec<usize> = known.into_iter().map(|x| x.1).collect();
    for (i, k) in keys.iter().enumerate() {
        if at(k).is_some() {
            continue;
        }
        let to = if i == 0 { 0 } else { p.iter().position(|&x| x == i - 1).map_or(p.len(), |x| x + 1) };
        p.insert(to, i);
    }
    p.iter().enumerate().any(|(i, j)| i != *j).then_some(p)
}

/// Rows of `sheet` hidden in the book `path`, by row key.
pub(crate) fn hidden_rows_of(path: &Path, sheet: &str) -> HashSet<i64> {
    cached(path).get(sheet).map(|v| v.hidden_rows.clone()).unwrap_or_default()
}

/// The old book of this export hides rows of `sheet` or sets their heights.
pub(crate) fn marks_rows(sheet: &str) -> bool {
    VIEWS.with(|v| v.borrow().get(sheet).is_some_and(|v| !v.hidden_rows.is_empty() || !v.heights.is_empty()))
}

/// Puts `rows` of `name` into the old book's order; returns the user and parameter columns
/// moved along with them, or None when the order stays as it is.
pub(crate) fn arrange(name: &str, cols: &[Col], rows: &mut Rows, extra: &[XCol]) -> Option<Vec<XCol>> {
    let keys: Vec<Option<i64>> = rows.iter().map(|r| row_key(name, cols, r)).collect();
    let p = VIEWS.with(|v| {
        let b = v.borrow();
        let view = b.get(name).filter(|v| !v.order.is_empty())?;
        if is_layer(name) {
            placement_near(&view.order, &keys)
        } else {
            placement(&view.order, &keys)
        }
    })?;
    let mut old: Vec<Option<Vec<V>>> = std::mem::take(rows).into_iter().map(Some).collect();
    *rows = p.iter().map(|&i| old[i].take().unwrap_or_default()).collect();
    Some(
        extra
            .iter()
            .map(|x| XCol {
                header: x.header.clone(),
                edit: x.edit,
                cells: p.iter().map(|&i| x.cells.get(i).cloned().unwrap_or(V::Blank)).collect(),
            })
            .collect(),
    )
}

/// Final position of each of `keys` (model order) once `arrange` puts the rows of `name` in
/// the old book's order: a user formula is moved once, straight to the row it ends on.
pub(crate) fn final_rows(name: &str, keys: &[i64]) -> Vec<usize> {
    // same keys and placement as `arrange`: layer rows by their old book key (layer_match.rs)
    let layer = is_layer(name);
    let keys: Vec<Option<i64>> = keys
        .iter()
        .map(|k| if layer { super::layer_match::old_key(name, *k) } else { Some(*k) })
        .collect();
    let mut at: Vec<usize> = (0..keys.len()).collect();
    let p = VIEWS.with(|v| {
        let b = v.borrow();
        let view = b.get(name).filter(|v| !v.order.is_empty())?;
        if layer {
            placement_near(&view.order, &keys)
        } else {
            placement(&view.order, &keys)
        }
    });
    for (j, i) in p.into_iter().flatten().enumerate() {
        at[i] = j;
    }
    at
}

/// Carries the old view onto the written sheet `name`: header at row `top`, data rows below it
/// in `rows` order; `captions` are the written headers of `cols`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    ws: &mut Worksheet,
    name: &str,
    cols: &[Col],
    captions: &[String],
    extra: &[XCol],
    top: u32,
    rows: &Rows,
) -> Result<(), String> {
    VIEWS.with(|v| {
        let b = v.borrow();
        let Some(view) = b.get(name) else {
            return Ok(());
        };
        let e = |x: rust_xlsxwriter::XlsxError| x.to_string();
        let heads: Vec<Vec<String>> = cols
            .iter()
            .enumerate()
            .map(|(j, c)| {
                let mut h = vec![units::norm(c.header)];
                h.extend(captions.get(j).map(|t| units::norm(t)));
                h
            })
            .chain(extra.iter().map(|x| vec![units::norm(&x.header)]))
            .collect();
        for (c, names) in heads.iter().enumerate() {
            let Some(cv) = names.iter().find_map(|h| view.cols.get(h)) else {
                continue;
            };
            let c = c as u16;
            if let Some(w) = cv.width {
                ws.set_column_width(c, w).map_err(e)?;
            }
            if cv.hidden {
                ws.set_column_hidden(c).map_err(e)?;
            }
        }
        if let Some(gap) = view.hidden_tail {
            apply_hidden_tail(ws, gap, heads.len()).map_err(e)?;
        }
        if let Some(h) = view.head_height {
            ws.set_row_height(top, h).map_err(e)?;
        }
        for (i, row) in rows.iter().enumerate() {
            let Some(k) = row_key(name, cols, row) else {
                continue;
            };
            let r = top + 1 + i as u32;
            if let Some(h) = view.heights.get(&k) {
                ws.set_row_height(r, *h).map_err(e)?;
            }
            if view.hidden_rows.contains(&k) {
                ws.set_row_hidden(r).map_err(e)?;
            }
        }
        Ok(())
    })
}

/// Sheets whose hidden rows a re-export from the old book (views `a`) into the new one (`b`)
/// carries by row key: keyed in both books and not written as a family sheet. A hidden row of
/// a deleted element goes with it.
pub(crate) fn rows_kept(
    a: &HashMap<String, View>,
    b: &HashMap<String, View>,
    fams: &[super::family::FamilySheet],
) -> HashSet<String> {
    a.iter()
        .filter(|(n, v)| v.keyed && b.get(*n).is_some_and(|w| w.keyed) && !fams.iter().any(|f| f.sheet == n.as_str()))
        .map(|(n, _)| n.clone())
        .collect()
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn a_book_is_parsed_once_per_state_and_garbage_gives_no_view() {
        let p = std::env::temp_dir().join(format!("ddc_sv_cache_{}.xlsx", std::process::id()));
        std::fs::write(&p, b"not a zip").unwrap();
        let a = cached(&p);
        assert!(a.is_empty(), "a malformed old book never blocks: model order");
        assert!(Rc::ptr_eq(&a, &cached(&p)), "the loss check reuses the export's parse");
        std::fs::write(&p, b"still not a zip, longer").unwrap();
        assert!(!Rc::ptr_eq(&a, &cached(&p)), "a rewritten book is read again");
        assert!(cached(&p.with_extension("missing")).is_empty());
        let _ = std::fs::remove_file(&p);
    }
}
