// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Columns beyond the sheet format: project parameters, stages, user columns.
//! Export writes them after the format columns (user columns and sheets from the previous workbook —
//! by row id); import turns them into edits, and unknown ones into parameter proposals.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use calamine::{open_workbook, Data, Range, Reader, Xlsx};
use ddc_model::{Edit, Model, PVal, ParamDef, ParamSpec, ParamType};
use rust_xlsxwriter::{Format, Formula, Workbook, Worksheet, XlsxError};

use super::baseline::{AppliedCell, BVal, KeptCell};
use super::export::V;
use super::param_head::{self, Bound};
use super::units::{self, Dim};
use super::*;

/// Stage column headers are machine names, like the format columns.
pub const PHASE_CREATED: &str = "phase_created";
pub const PHASE_DEMOLISHED: &str = "phase_demolished";
/// Headers of workbooks from earlier exports and manual edits: also recognised on read.
const PHASE_CREATED_ALT: [&str; 2] = ["\u{421}\u{442}\u{430}\u{434}\u{438}\u{44f} \u{432}\u{43e}\u{437}\u{432}\u{435}\u{434}\u{435}\u{43d}\u{438}\u{44f}", "Phase created"];
const PHASE_DEMOLISHED_ALT: [&str; 2] = ["\u{421}\u{442}\u{430}\u{434}\u{438}\u{44f} \u{441}\u{43d}\u{43e}\u{441}\u{430}", "Phase demolished"];

fn is_created_head(n: &str) -> bool {
    n == units::norm(PHASE_CREATED) || PHASE_CREATED_ALT.iter().any(|h| units::norm(h) == n)
}

fn is_demolished_head(n: &str) -> bool {
    n == units::norm(PHASE_DEMOLISHED) || PHASE_DEMOLISHED_ALT.iter().any(|h| units::norm(h) == n)
}

const EPS: f64 = 1e-9;

pub(crate) struct SheetCat {
    pub sheet: &'static str,
    /// Table of the sheet's rows (for the types sheet — the type table).
    pub kind: &'static str,
    pub cats: &'static [&'static str],
    pub per_type: bool,
}

/// Sheets whose rows are elements (or types) with parameters.
pub(crate) const SHEET_CATS: [SheetCat; 12] = [
    SheetCat {
        sheet: WALLS,
        kind: "wall",
        cats: &["wall"],
        per_type: false,
    },
    SheetCat {
        sheet: OPENINGS,
        kind: "opening",
        cats: &["door", "window"],
        per_type: false,
    },
    SheetCat {
        sheet: ROOMS,
        kind: "room",
        cats: &["room"],
        per_type: false,
    },
    SheetCat {
        sheet: SLABS,
        kind: "slab",
        cats: &["slab"],
        per_type: false,
    },
    SheetCat {
        sheet: CURTAINS,
        kind: "curtain_wall",
        cats: &["curtain_wall"],
        per_type: false,
    },
    SheetCat {
        sheet: TYPES,
        kind: "wall_type",
        cats: &["wall"],
        per_type: true,
    },
    SheetCat {
        sheet: COLUMNS,
        kind: "col",
        cats: &["col"],
        per_type: false,
    },
    SheetCat {
        sheet: BEAMS,
        kind: "beam",
        cats: &["beam"],
        per_type: false,
    },
    SheetCat {
        sheet: ROOFS,
        kind: "roof",
        cats: &["roof"],
        per_type: false,
    },
    SheetCat {
        sheet: CEILINGS,
        kind: "ceiling",
        cats: &["ceiling"],
        per_type: false,
    },
    SheetCat {
        sheet: SLAB_TYPES,
        kind: "slab_type",
        cats: &["slab", "roof", "ceiling"],
        per_type: true,
    },
    SheetCat {
        sheet: MEMBER_TYPES,
        kind: "member_type",
        cats: &["col", "beam"],
        per_type: true,
    },
];

/// All format sheets: user columns on them, except on category sheets, are not carried over.
const OWN: [&str; 22] = [
    LEVELS, TYPES, LAYERS, SLAB_LAYERS, MATERIALS, WALLS, OPENINGS, QUANTITIES, ROOMS, SLABS, CURTAINS,
    SUMMARY, INFO, BASELINE, KEYS, LISTS, COLUMNS, BEAMS, ROOFS, CEILINGS, SLAB_TYPES,
    MEMBER_TYPES,
];

pub(crate) fn sheet_cat(name: &str) -> Option<&'static SheetCat> {
    SHEET_CATS.iter().find(|s| s.sheet == name)
}

/// Element sheet of an instance table (`col` -> Columns); None for a table without one.
pub(crate) fn sheet_of_kind(kind: &str) -> Option<&'static str> {
    SHEET_CATS
        .iter()
        .find(|s| !s.per_type && s.kind == kind)
        .map(|s| s.sheet)
}

pub(crate) fn cols_of(sheet: &str) -> &'static [Col] {
    match sheet {
        WALLS => WALLS_COLS,
        OPENINGS => OPENINGS_COLS,
        ROOMS => ROOMS_COLS,
        SLABS => SLABS_COLS,
        CURTAINS => CURTAINS_COLS,
        TYPES => TYPES_COLS,
        LEVELS => LEVELS_COLS,
        LAYERS => LAYERS_COLS,
        SLAB_LAYERS => SLAB_LAYERS_COLS,
        _ => param_sheet(sheet).map(|p| p.cols).unwrap_or(&[]),
    }
}

/// Sheets that are not category sheets but still carry the user's own columns across a re-export
/// (SlabTypes is a category sheet and carries them on that path).
fn is_keyed(name: &str) -> bool {
    matches!(name, LEVELS | LAYERS | SLAB_LAYERS)
}

/// Layer sheets have no id column: a row is keyed by (type, position).
fn is_layer_sheet(name: &str) -> bool {
    name == LAYERS || name == SLAB_LAYERS
}

/// User cells of the last export on this thread that had no row left to stay with.
#[derive(Default, Debug, PartialEq)]
pub struct LostCells {
    /// One note per vanished row.
    pub notes: Vec<String>,
    /// Filled cells dropped with those rows.
    pub cells: usize,
}

thread_local! {
    static LOST: std::cell::RefCell<LostCells> = std::cell::RefCell::new(LostCells::default());
}

/// Starts an export: forgets the notes of the previous one.
pub(crate) fn begin_notes() {
    LOST.with(|l| *l.borrow_mut() = LostCells::default());
}

/// Takes the lost cells of the last export on this thread.
pub fn take_lost_cells() -> LostCells {
    LOST.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// Notes of the last export on this thread: user cells that had no row left to stay with.
pub fn lost_user_cells() -> Vec<String> {
    take_lost_cells().notes
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= EPS * (1.0 + a.abs().max(b.abs()))
}

fn blank(d: &Data) -> bool {
    match d {
        Data::Empty => true,
        Data::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

fn text_of(d: &Data) -> String {
    match d {
        Data::String(s) | Data::DateTimeIso(s) => s.trim().to_string(),
        // a date in a text column is a date, not a serial number
        Data::DateTime(dt) => iso_date(dt.as_f64()),
        other => other.to_string(),
    }
}

/// XLSX serial number (1900 system) → `YYYY-MM-DD` (with `HH:MM` if there is a time).
pub(crate) fn iso_date(serial: f64) -> String {
    let days = serial.floor() as i64;
    // 1899-12-30 is day zero; 1970-01-01 is day 25569
    let z = days - 25569 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let mins = ((serial - serial.floor()) * 1440.0).round() as i64;
    if mins > 0 && mins < 1440 {
        format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", mins / 60, mins % 60)
    } else {
        format!("{y:04}-{m:02}-{d:02}")
    }
}

/// Text numbers on import (one rule for every text cell of a number column):
/// - typed number cells are used as they are, never re-read through text;
/// - text cells go through [`units::number`]: the decimal mark the book's Info sheet records
///   (our own export always writes it, so its values read back exactly), otherwise each value
///   decides alone, as rule 3 of `crate::num_file` (`1,5` = 1.5, `1.234,5` and `1,234.5` =
///   1234.5);
/// - a value that reads two ways (`1,000`, `1,250`) is never guessed: the cell is refused with
///   a review reason and the model value stays (see [`cell_val`]).
///
/// For guessing a new parameter's type a text cell is a number only when the number shows all
/// of its text: digits and marks only, no leading zero (`007`), no exponent; a trailing
/// fraction zero (`2.50`) is a price, not a code. A lone point (`15.03`) stays text unless the
/// book records a point mark: in other books it is a date or code more often than a number.
/// A [`hard_text`] value is never a number, whatever the mark; a [`soft_date`] value
/// (`20.12`) is, unless the whole column looks like that (see `Cand::guess`).
fn lossless_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let body = t.strip_prefix('-').unwrap_or(t);
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') {
        return None;
    }
    if hard_text(t) {
        return None;
    }
    let lone_point = !body.contains(',') && body.matches('.').count() == 1;
    if lone_point && units::book_dec() != Some(units::Dec::Point) {
        return None;
    }
    let v = units::number(t, None).ok().filter(|v| v.is_finite())?;
    let digits = |x: &str| {
        let d: String = x.chars().filter(char::is_ascii_digit).collect();
        d.trim_end_matches('0').to_string()
    };
    (digits(body) == digits(&v.to_string())).then_some(v)
}

/// A day.month shape that is also a fine number (`15.03`, `1.05`, `20.12`): day 1-31 without
/// a leading zero, a point, month 01-12. Text only when every text cell of a column has it.
fn soft_date(s: &str) -> bool {
    let t = s.trim();
    let Some((d, m)) = t.split_once('.') else {
        return false;
    };
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let n = |p: &str| p.parse::<u32>().unwrap_or(0);
    digits(d)
        && digits(m)
        && (1..=2).contains(&d.len())
        && !d.starts_with('0')
        && m.len() == 2
        && (1..=31).contains(&n(d))
        && (1..=12).contains(&n(m))
}

/// Text that is never a number for a new column: `1.10.25`, `15.03.2025`, `03/15`,
/// `2025-03-15`, `12:30`, and a leading zero (`007`, `01.10`).
fn hard_text(s: &str) -> bool {
    let t = s.trim();
    let body = t.strip_prefix('-').unwrap_or(t).as_bytes();
    if body.len() > 1 && body[0] == b'0' && body[1].is_ascii_digit() {
        return true;
    }
    let parts: Vec<&str> = t.split(|c: char| matches!(c, '.' | '/' | '-' | ':')).collect();
    let digits = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if parts.len() < 2 || !parts.iter().all(digits) {
        return false;
    }
    if t.contains(|c: char| matches!(c, '/' | '-' | ':')) {
        return true;
    }
    match parts.as_slice() {
        [d, m, y] => d.len() <= 2 && m.len() <= 2 && matches!(y.len(), 2 | 4),
        _ => false,
    }
}

/// A text cell that reads as two different numbers (`1,250`): kept numeric for type guessing,
/// refused per cell so the review asks for a decision.
fn ambiguous(d: &Data) -> bool {
    let Data::String(s) = d else {
        return false;
    };
    matches!(units::len_value(s, 1.0), Err(units::NumErr::Ambiguous { .. }))
}

fn num_of(d: &Data) -> Option<f64> {
    match d {
        Data::Float(x) => Some(*x).filter(|v| v.is_finite()),
        Data::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn int_of(d: &Data) -> Option<i64> {
    let v = match d {
        // ids are written as plain integers: only the invariant form is an id
        Data::String(s) => ddc_model::num_text::parse_invariant(s)?,
        d => num_of(d)?,
    };
    (v.fract() == 0.0 && v.abs() < 9e15).then_some(v as i64)
}

fn invalid(e: ddc_model::Error) -> String {
    match e {
        ddc_model::Error::Invalid(s) => s,
        e => e.to_string(),
    }
}

fn dim_of(t: ParamType) -> Dim {
    match t {
        ParamType::Length => Dim::Len,
        ParamType::Area => Dim::Area,
        ParamType::Volume => Dim::Vol,
        _ => Dim::None,
    }
}

/// Sheet parameters: bound to its categories, "instance/type" as for the sheet.
fn sheet_params(defs: &[ParamDef], sc: &SheetCat) -> Vec<ParamDef> {
    defs.iter()
        .filter(|d| {
            d.spec.per_type == sc.per_type
                && d.spec
                    .categories
                    .iter()
                    .any(|c| sc.cats.contains(&c.as_str()))
        })
        .cloned()
        .collect()
}

/// Canonical parameter header: SI units in brackets, like the format columns.
fn param_header(d: &ParamDef) -> String {
    let n = d.spec.name.trim();
    param_unit(d).map_or_else(|| n.to_string(), |x| format!("{n} {x}"))
}

/// Unit of a parameter column in brackets, in SI.
fn param_unit(d: &ParamDef) -> Option<String> {
    let unit = d.spec.unit.trim();
    match d.spec.ptype {
        ParamType::Length => Some("[m]".to_string()),
        ParamType::Area => Some("[m2]".to_string()),
        ParamType::Volume => Some("[m3]".to_string()),
        // a "[m]" label on a number would read as a length
        ParamType::Number if !unit.is_empty() && units::dim(&format!("[{unit}]")) == Dim::None => {
            Some(format!("[{unit}]"))
        }
        _ => None,
    }
}

/// Header written to the book; only the unit follows the book's units, never the name.
fn param_shown(d: &ParamDef, tagged: bool, u: Units) -> String {
    let n = d.spec.name.trim();
    let head = if tagged { param_head::tagged_name(n, d.id) } else { n.to_string() };
    param_unit(d).map_or_else(|| head.clone(), |x| format!("{head} {}", u.header(&x)))
}

/// Metres per unit of a parameter column under header `h`; Err on an unknown unit.
fn param_factor(h: &str, d: &ParamDef, u: Units) -> Result<f64, ()> {
    let dim = dim_of(d.spec.ptype);
    let tail = param_head::unit_part(h, d);
    match units::header_factor(tail, dim) {
        Some(k) => Ok(k),
        None if dim != Dim::None && tail.contains('[') => Err(()),
        None => Ok(u.factor(&param_header(d))),
    }
}

/// Header without bracketed units, case preserved: "Price [rub]" → "Price".
fn strip_unit(h: &str) -> String {
    let mut out = String::with_capacity(h.len());
    let mut depth = 0;
    for ch in h.chars() {
        match ch {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn unit_label(h: &str) -> Option<String> {
    let a = h.find('[')?;
    let b = a + h[a..].find(']')?;
    let u = h[a + 1..b].trim();
    (!u.is_empty()).then(|| u.to_string())
}

// ---------------------------------------------------------------- export

/// A column beyond the format: cells already in workbook units, by sheet rows.
#[derive(Clone)]
pub(crate) struct XCol {
    pub header: String,
    pub edit: bool,
    pub cells: Vec<V>,
}

struct KCol {
    header: String,
    /// Row id → (value or formula, row in the previous workbook, 1-based).
    cells: HashMap<i64, (V, u32)>,
}

/// User columns and sheets from the previous workbook at the same path.
#[derive(Default)]
pub(crate) struct Kept {
    sheets: HashMap<String, Vec<KCol>>,
    others: Vec<(String, Vec<(u32, u16, V)>)>,
    /// Rows a user put above the header of an app sheet: (header row, 0-based; their cells),
    /// written back in place with the header below them.
    titles: HashMap<String, Titles>,
    /// Sheets of the old book that are not worksheets (chart sheets and the like): the new
    /// book cannot carry them, so the caller names them and keeps the old book aside.
    #[allow(dead_code)] // the app reads the same list through `dropped_sheets`
    pub(crate) dropped: Vec<String>,
    /// What the user put into the book, counted for the loss check of a re-export.
    pub(crate) counts: UserCounts,
}

/// Non-blank cells the user owns, by place; a re-export that writes fewer has dropped some.
#[derive(Default)]
pub(crate) struct UserCounts {
    /// (sheet, normalised header ("" = no header), header as shown, ids of the rows with a cell)
    pub(crate) cols: Vec<(String, String, String, Vec<i64>)>,
    /// (app sheet, ids of its rows)
    pub(crate) ids: Vec<(String, Vec<i64>)>,
    /// (sheet, rows without a readable id that hold anything)
    pub(crate) idless: Vec<(String, usize)>,
    /// (user sheet, cells)
    pub(crate) sheets: Vec<(String, usize)>,
    /// (app sheet, filled cells in the rows above its header)
    pub(crate) titles: Vec<(String, usize)>,
}

/// Header row of an app sheet (0-based) and the cells of the rows above it.
pub(crate) type Titles = (u32, Vec<(u32, u16, V)>);

thread_local! {
    /// Rows above the header of the export running on this thread, by sheet.
    static TITLES: std::cell::RefCell<HashMap<String, Titles>> = std::cell::RefCell::new(HashMap::new());
}

/// Header row of `sheet` in the export running on this thread and the rows above it to write.
pub(crate) fn titles_of(sheet: &str) -> Titles {
    TITLES.with(|t| t.borrow().get(sheet).cloned()).unwrap_or_default()
}

/// Writes the rows above the header (unlocked: they are the user's) and returns the header row.
pub(crate) fn write_titles(ws: &mut Worksheet, t: &Titles) -> Result<u32, XlsxError> {
    let open = Format::new().set_unlocked();
    for (r, c, v) in &t.1 {
        write_v(ws, *r, *c, v, Some(&open))?;
    }
    Ok(t.0)
}

/// Names of the non-worksheet sheets of an open book, in book order.
fn non_worksheets<R: std::io::Read + std::io::Seek>(wb: &Xlsx<R>) -> Vec<String> {
    wb.sheets_metadata()
        .iter()
        .filter(|s| s.typ != calamine::SheetType::WorkSheet)
        .map(|s| s.name.clone())
        .collect()
}

/// Sheets of the own book at `path` that a re-export cannot carry over (chart sheets, ...).
/// Empty for a missing, unreadable or foreign book.
pub fn dropped_sheets(path: &Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    let wb: Result<Xlsx<std::io::BufReader<std::fs::File>>, _> = open_workbook(path);
    let Ok(wb) = wb else {
        return Vec::new();
    };
    if !wb.sheet_names().iter().any(|n| n == INFO) {
        return Vec::new();
    }
    non_worksheets(&wb)
}

/// Error constants a formula may hold; other error kinds of the reader stay text.
const ERRORS: [&str; 7] = ["#N/A", "#DIV/0!", "#NAME?", "#NULL!", "#NUM!", "#REF!", "#VALUE!"];

pub(crate) fn error_const(t: &str) -> Option<&'static str> {
    let t = t.trim().trim_start_matches('=').trim();
    ERRORS.iter().find(|e| e.eq_ignore_ascii_case(t)).copied()
}

/// Number format of a kept date cell. The reader gives the kind, not the code, so a code of
/// the same kind is chosen; it reads back as the same kind and value.
fn date_v(x: f64, duration: bool) -> V {
    let code = if duration {
        "[h]:mm:ss"
    } else if (0.0..1.0).contains(&x) {
        "hh:mm:ss"
    } else if x.fract() == 0.0 {
        "yyyy-mm-dd"
    } else {
        "yyyy-mm-dd hh:mm:ss"
    };
    V::D(x, code)
}

/// Days from 1970-01-01 of a civil date (proleptic Gregorian).
fn days_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Seconds of "HH:MM[:SS[.fff]]" as a fraction of a day.
fn iso_time(t: &str) -> Option<f64> {
    // ISO 8601 fields use the invariant form (point fraction), whatever the book's mark
    let inv = ddc_model::num_text::parse_invariant;
    let mut it = t.split(':');
    let h: f64 = inv(it.next()?)?;
    let m: f64 = inv(it.next()?)?;
    let s: f64 = it.next().map_or(Some(0.0), inv)?;
    if it.next().is_some()
        || !(0.0..24.0).contains(&h)
        || !(0.0..60.0).contains(&m)
        || !(0.0..61.0).contains(&s)
    {
        return None;
    }
    Some((h * 3600.0 + m * 60.0 + s) / 86_400.0)
}

/// XLSX serial of an ISO date, date-time or time ("2025-01-21", "2025-01-21T12:00:00", "12:00:00").
pub(crate) fn iso_serial(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (Some(d), Some(t)),
        None if s.contains(':') => (None, Some(s)),
        None => (Some(s), None),
    };
    let day = match date {
        Some(d) => {
            let p: Vec<&str> = d.split('-').collect();
            let [y, m, dd] = p.as_slice() else { return None };
            let (y, m, dd): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, dd.parse().ok()?);
            if !(1..=12).contains(&m) || !(1..=31).contains(&dd) {
                return None;
            }
            (days_civil(y, m, dd) - days_civil(1899, 12, 30)) as f64
        }
        None => 0.0,
    };
    let frac = match time {
        Some(t) => iso_time(t)?,
        None => 0.0,
    };
    Some(day + frac)
}

/// Kept cell value with its type: dates stay dates, booleans booleans, errors errors.
fn data_v(d: &Data) -> V {
    match d {
        Data::Empty => V::Blank,
        Data::Float(x) => V::N(*x),
        Data::Int(i) => V::N(*i as f64),
        Data::Bool(b) => V::B(*b),
        Data::String(s) if s.trim().is_empty() => V::Blank,
        Data::String(s) => V::S(s.clone()),
        Data::DateTime(t) => date_v(t.as_f64(), t.is_duration()),
        Data::DateTimeIso(s) => iso_serial(s).map_or_else(|| V::S(s.clone()), |x| date_v(x, false)),
        Data::Error(e) => {
            let t = e.to_string();
            error_const(&t).map_or(V::S(t), |c| V::E(c.to_string()))
        }
        other => V::S(other.to_string()),
    }
}

/// Book value of a kept conflict cell, typed like `data_v`; a formula result counts as its value.
fn kept_book(d: &Data) -> BVal {
    super::baseline::v_bval(&data_v(d))
}

/// Writes a kept cell with its own type. The writer has no error cell, so an error constant
/// becomes the same constant as a formula ("=#N/A"); it reads back as that error.
pub(crate) fn write_v(
    ws: &mut Worksheet,
    r: u32,
    c: u16,
    v: &V,
    fmt: Option<&Format>,
) -> Result<(), XlsxError> {
    if let Some(done) = super::cellsafe::write_safe(ws, r, c, v, None) {
        return done;
    }
    let plain = Format::new();
    let f = fmt.unwrap_or(&plain);
    match (v, fmt.is_some()) {
        (V::Blank, false) => return Ok(()),
        (V::Blank, true) => ws.write_blank(r, c, f),
        (V::N(x), false) => ws.write_number(r, c, *x),
        (V::N(x), true) => ws.write_number_with_format(r, c, *x, f),
        (V::S(s), false) => ws.write_string(r, c, s),
        (V::S(s), true) => ws.write_string_with_format(r, c, s, f),
        (V::F(s, res), false) => ws.write_formula(r, c, formula(s, res)),
        (V::F(s, res), true) => ws.write_formula_with_format(r, c, formula(s, res), f),
        (V::D(x, code), _) => {
            ws.write_number_with_format(r, c, *x, &f.clone().set_num_format(*code))
        }
        (V::B(b), false) => ws.write_boolean(r, c, *b),
        (V::B(b), true) => ws.write_boolean_with_format(r, c, *b, f),
        (V::E(e), _) => {
            let fm = Formula::new(format!("={e}")).set_result(e.clone());
            ws.write_formula_with_format(r, c, fm, f)
        }
    }
    .map(|_| ())
}

/// A kept user formula with the result its book cached, when there is one.
pub(crate) fn formula(text: &str, res: &Option<String>) -> Formula {
    match res {
        Some(r) => Formula::new(text).set_result(r),
        None => Formula::new(text),
    }
}

/// Text of a cached formula result that the writer stores back as the same value and type;
/// None when it would not (a text that reads as a number, bool or error), or there is none.
fn cached_result(d: Option<&Data>) -> Option<String> {
    const WRITER_ERRORS: [&str; 7] =
        ["#DIV/0!", "#N/A", "#NAME?", "#NULL!", "#NUM!", "#REF!", "#VALUE!"];
    match d? {
        Data::Float(x) if x.is_finite() => Some(x.to_string()),
        Data::Int(i) => Some(i.to_string()),
        Data::Bool(b) => Some(if *b { "TRUE" } else { "FALSE" }.into()),
        Data::DateTime(t) => Some(t.as_f64().to_string()),
        Data::Error(e) => Some(e.to_string()).filter(|t| WRITER_ERRORS.contains(&t.as_str())),
        Data::String(s) => {
            let typed = s.parse::<f64>().is_ok()
                || matches!(s.as_str(), "TRUE" | "FALSE")
                || s.starts_with('#');
            (!typed && !s.is_empty() && s.chars().count() <= 32_767).then(|| s.clone())
        }
        _ => None,
    }
}

/// Reason shown when the previous book cannot be read and so cannot be carried over.
pub(crate) const UNREADABLE: &str =
    "book could not be read, its own columns would be lost; close it in a spreadsheet app and retry";

impl Kept {
    /// Our own workbook only (has an Info sheet): a foreign file is not mixed with the model on overwrite.
    pub(crate) fn read(path: &Path, fams: &[super::family::FamilySheet]) -> Result<Kept, String> {
        let mut k = Kept::default();
        // a missing book has nothing to keep; an existing one that cannot be read must not be replaced
        if !path.exists() {
            return Ok(k);
        }
        let wb: Result<Xlsx<std::io::BufReader<std::fs::File>>, _> = open_workbook(path);
        let mut wb = wb.map_err(|e| format!("{UNREADABLE}: {e}"))?;
        let names = wb.sheet_names();
        if !names.iter().any(|n| n == INFO) {
            return Ok(k);
        }
        k.dropped = non_worksheets(&wb);
        let keys = super::book::read_keys(&mut wb);
        for name in names {
            if k.dropped.contains(&name) {
                continue;
            }
            let vals = wb
                .worksheet_range(&name)
                .map_err(|e| format!("{UNREADABLE}: {e}"))?;
            let forms = wb.worksheet_formula(&name).ok();
            let cell = |r: u32, c: u32| -> V {
                if let Some(f) = forms.as_ref().and_then(|f| f.get_value((r, c))) {
                    if let Some(e) = error_const(f) {
                        return V::E(e.to_string());
                    }
                    if !f.trim().is_empty() {
                        let res = cached_result(vals.get_value((r, c)));
                        return V::F(format!("={}", f.trim_start_matches('=')), res);
                    }
                }
                vals.get_value((r, c)).map_or(V::Blank, data_v)
            };
            let ends = [vals.end(), forms.as_ref().and_then(|f| f.end())];
            let Some(end) = ends
                .iter()
                .flatten()
                .fold(None, |a: Option<(u32, u32)>, e| {
                    Some(a.map_or(*e, |a| (a.0.max(e.0), a.1.max(e.1))))
                })
            else {
                continue;
            };
            let Some((r0, c0)) = vals.start() else {
                continue;
            };
            // headers the format writes itself; the rest of a sheet's columns is the user's
            let fam_heads = fams
                .iter()
                .find(|f| f.sheet == name)
                .map(super::family::known_heads);
            let known: Option<HashSet<String>> = if sheet_cat(&name).is_some() {
                Some(
                    cols_of(&name)
                        .iter()
                        .map(|c| units::norm(c.header))
                        .chain([units::norm(PHASE_CREATED), units::norm(PHASE_DEMOLISHED)])
                        .chain(
                            PHASE_CREATED_ALT
                                .iter()
                                .chain(&PHASE_DEMOLISHED_ALT)
                                .map(|h| units::norm(h)),
                        )
                        // a parameter sheet the app writes as a family sheet
                        .chain(fam_heads.into_iter().flatten())
                        .collect(),
                )
            } else if is_keyed(&name) {
                Some(cols_of(&name).iter().map(|c| units::norm(c.header)).collect())
            } else {
                fam_heads
            };
            if let Some(known) = known {
                // the header row is found the way the import finds it: title rows may sit above
                let kv: Vec<String> = known.iter().cloned().collect();
                let h = super::book::find_header(&vals, &kv, |row| {
                    super::book::resolve(&keys, &name, cols_of(&name), c0 as usize, row)
                        .texts
                        .iter()
                        .map(|t| units::norm(t))
                        .collect()
                });
                let top = r0;
                let r0 = r0 + h as u32;
                if r0 > top {
                    let mut cells = vec![];
                    for r in top..r0 {
                        for c in c0..=end.1 {
                            let v = cell(r, c);
                            if !matches!(v, V::Blank) && c <= u16::MAX as u32 {
                                cells.push((r, c as u16, v));
                            }
                        }
                    }
                    if !cells.is_empty() {
                        k.counts.titles.push((name.clone(), cells.len()));
                        k.titles.insert(name.clone(), (r0, cells));
                    }
                }
                let raw: Vec<String> = (c0..=end.1)
                    .map(|c| vals.get_value((r0, c)).map_or(String::new(), text_of))
                    .collect();
                // columns the key row does not place stay the user's
                let heads = super::book::resolve(&keys, &name, cols_of(&name), c0 as usize, &raw);
                let copy_at: HashSet<u32> = heads.copies.iter().map(|c| c0 + c.0 as u32).collect();
                let head: Vec<(u32, String)> = (c0..=end.1).zip(heads.texts).collect();
                let head_col = |n: &str| head.iter().find(|h| units::norm(&h.1) == n).map(|h| h.0);
                let by_layer = is_layer_sheet(&name);
                let idc = head_col(if by_layer { "type_id" } else { "id" });
                let pos_col = head_col("pos").filter(|_| by_layer);
                // the key a row's user cells follow: its id, or (type, position) on layer sheets;
                // the loss check counts by the same key, so carried cells never read as lost
                let key_at = |r: u32| -> Option<i64> {
                    let id = vals.get_value((r, idc?)).and_then(int_of)?;
                    if !by_layer {
                        return Some(id);
                    }
                    let pos = vals.get_value((r, pos_col?)).and_then(int_of)?;
                    Some(layer_key(id, pos))
                };
                let filled = |r: u32, c: u32| !matches!(cell(r, c), V::Blank);
                let row_ids: Vec<i64> = (r0 + 1..=end.0).filter_map(&key_at).collect();
                let mut heads_seen: HashSet<String> = HashSet::new();
                for (c, h) in &head {
                    let n = units::norm(h);
                    let first = n.is_empty() || heads_seen.insert(n.clone());
                    let copy = !first || copy_at.contains(c);
                    if !copy && !h.is_empty() && known.contains(&n) {
                        continue;
                    }
                    let with: Vec<i64> =
                        (r0 + 1..=end.0).filter(|r| filled(*r, *c)).filter_map(&key_at).collect();
                    if !with.is_empty() {
                        // the new book writes a field once: a copy is counted apart, so the loss
                        // check names it instead of matching it with the column it repeats
                        let key = if copy { format!("{n}\u{1}{c}") } else { n };
                        k.counts.cols.push((name.clone(), key, h.clone(), with));
                    }
                }
                k.counts.ids.push((name.clone(), row_ids));
                // layer rows are keyed by type and position: no id is handed out to lose
                if idc.is_some() && !by_layer {
                    let idless = (r0 + 1..=end.0)
                        .filter(|r| key_at(*r).is_none() && (c0..=end.1).any(|c| filled(*r, c)))
                        .count();
                    if idless > 0 {
                        k.counts.idless.push((name.clone(), idless));
                    }
                }
                if idc.is_none() || (by_layer && pos_col.is_none()) {
                    continue;
                }
                let mut cols: Vec<(u32, KCol)> = head
                    .iter()
                    .filter(|(c, h)| {
                        !h.is_empty() && !known.contains(&units::norm(h)) && !copy_at.contains(c)
                    })
                    .map(|(c, h)| {
                        let kc = KCol {
                            header: h.clone(),
                            cells: HashMap::new(),
                        };
                        (*c, kc)
                    })
                    .collect();
                for r in r0 + 1..=end.0 {
                    let Some(id) = key_at(r) else {
                        continue;
                    };
                    for (c, kc) in &mut cols {
                        let v = cell(r, *c);
                        if !matches!(v, V::Blank) {
                            kc.cells.insert(id, (v, r + 1));
                        }
                    }
                }
                k.sheets
                    .insert(name.clone(), cols.into_iter().map(|c| c.1).collect());
            } else if !OWN.contains(&name.as_str()) {
                let mut cells = vec![];
                for r in r0.min(end.0)..=end.0 {
                    for c in c0.min(end.1)..=end.1 {
                        let v = cell(r, c);
                        if !matches!(v, V::Blank) && c <= u16::MAX as u32 {
                            cells.push((r, c as u16, v));
                        }
                    }
                }
                k.counts.sheets.push((name.clone(), cells.len()));
                k.others.push((name.clone(), cells));
            }
        }
        Ok(k)
    }

    /// User columns of `sheet` from the previous book, one cell per row id; a header already in
    /// `taken` is the format's own and is skipped. Formulas follow their row.
    pub(crate) fn user_cols(
        &self,
        sheet: &str,
        ids: &[i64],
        taken: &mut HashSet<String>,
    ) -> Vec<XCol> {
        self.user_cols_at(sheet, ids, taken, true)
    }

    /// `user_cols`; `arranged`: the writer puts the rows back in the old book's order
    /// (`sheet_view::arrange`), so a formula goes straight to the row it ends on.
    pub(crate) fn user_cols_at(
        &self,
        sheet: &str,
        ids: &[i64],
        taken: &mut HashSet<String>,
        arranged: bool,
    ) -> Vec<XCol> {
        let mut out = vec![];
        // rows above the header push the data rows down in the new book as well
        let top = self.titles.get(sheet).map_or(0, |t| t.0);
        let at = if arranged {
            super::sheet_view::final_rows(sheet, ids)
        } else {
            (0..ids.len()).collect()
        };
        // the old book's row each row takes its cells from (layer rows may have moved)
        let olds: Vec<Option<i64>> = ids.iter().map(|id| super::layer_match::old_key(sheet, *id)).collect();
        for kc in self.sheets.get(sheet).into_iter().flatten() {
            if !taken.insert(units::norm(&kc.header)) {
                continue;
            }
            let cells = olds
                .iter()
                .enumerate()
                .map(|(i, id)| match id.and_then(|id| kc.cells.get(&id)) {
                    None => V::Blank,
                    Some((V::F(f, res), old)) => {
                        V::F(shift_row(f, *old, top + at[i] as u32 + 2), res.clone())
                    }
                    Some((v, _)) => v.clone(),
                })
                .collect();
            out.push(XCol {
                header: kc.header.clone(),
                edit: true,
                cells,
            });
        }
        out
    }

    /// `user_cols` for a sheet outside the categories (`ids` are row ids, or layer keys). Cells
    /// of a row that no longer exists are dropped, never moved to another row, and named in
    /// `lost_user_cells`.
    pub(crate) fn keyed_cols(&self, sheet: &str, ids: &[i64]) -> Vec<XCol> {
        let mut taken: HashSet<String> = cols_of(sheet)
            .iter()
            .map(|c| units::norm(c.header))
            .collect();
        let out = self.user_cols(sheet, ids, &mut taken);
        let live: HashSet<i64> = ids.iter().filter_map(|id| super::layer_match::old_key(sheet, *id)).collect();
        let mut gone: Vec<i64> = vec![];
        let mut filled = 0;
        for kc in self.sheets.get(sheet).into_iter().flatten() {
            if !out.iter().any(|x| x.header == kc.header) {
                continue;
            }
            for (k, (v, _)) in &kc.cells {
                if live.contains(k) {
                    continue;
                }
                gone.push(*k);
                filled += usize::from(!matches!(v, V::Blank));
            }
        }
        gone.sort_unstable();
        gone.dedup();
        for k in gone {
            let what = if is_layer_sheet(sheet) {
                format!("type {} layer {}", k / 1_000_000, k % 1_000_000)
            } else {
                format!("id {k}")
            };
            let note = format!("{sheet}: {what} no longer exists, its own columns' cells were dropped");
            LOST.with(|l| l.borrow_mut().notes.push(note));
        }
        LOST.with(|l| l.borrow_mut().cells += filled);
        out
    }

    /// The old book has user cells on `sheet`.
    pub(crate) fn has_cols(&self, sheet: &str) -> bool {
        self.sheets.get(sheet).is_some_and(|c| c.iter().any(|k| !k.cells.is_empty()))
    }

    /// Hands the rows above each header to the sheet writers of this export (`titles_of`).
    pub(crate) fn share_titles(&self) {
        TITLES.with(|t| *t.borrow_mut() = self.titles.clone());
    }

    /// Family sheets are written afresh: old copies are not carried over as user sheets.
    pub(crate) fn forget_sheets<'a>(&mut self, names: impl Iterator<Item = &'a str>) {
        let gone: Vec<&str> = names.collect();
        self.others.retain(|(n, _)| !gone.contains(&n.as_str()));
    }

    /// User sheets as they were (values and formulas, without formatting).
    pub(crate) fn write_others(&self, wb: &mut Workbook) -> Result<(), String> {
        for (name, cells) in &self.others {
            let ws = wb.add_worksheet();
            ws.set_name(name).map_err(err)?;
            for (r, c, v) in cells {
                write_v(ws, *r, *c, v, None).map_err(err)?;
            }
        }
        Ok(())
    }
}

/// Formula references to row `old` (no `$` on the row number) → to `new`: formulas on an
/// element's row move with it when rows shift.
pub(crate) fn shift_row(f: &str, old: u32, new: u32) -> String {
    if old == new {
        return f.to_string();
    }
    let b = f.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'!' | b'$');
    let mut out = String::with_capacity(f.len() + 4);
    let (mut i, mut quoted) = (0, false);
    while i < b.len() {
        if b[i] == b'"' {
            quoted = !quoted;
        }
        if !quoted && (i == 0 || !word(b[i - 1])) && (b[i] == b'$' || b[i].is_ascii_alphabetic()) {
            let mut j = i;
            if b[j] == b'$' {
                j += 1;
            }
            let ls = j;
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            let nl = j - ls;
            let abs_row = j < b.len() && b[j] == b'$';
            if abs_row {
                j += 1;
            }
            let ds = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let next_ok = j >= b.len()
                || !(b[j].is_ascii_alphanumeric() || matches!(b[j], b'_' | b'(' | b'.'));
            if (1..=3).contains(&nl) && j > ds && !abs_row && next_ok {
                out.push_str(&f[i..ds]);
                match f[ds..j].parse::<u32>() {
                    Ok(r) if r == old => out.push_str(&new.to_string()),
                    _ => out.push_str(&f[ds..j]),
                }
                i = j;
                continue;
            }
        }
        let ch = f[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8().max(1);
    }
    out
}

/// Parameters, stages and user columns for a category sheet (`ids` — row ids in order).
pub(crate) fn columns(
    model: &Model,
    sheet: &str,
    ids: &[i64],
    u: Units,
    kept: &Kept,
) -> Result<Vec<XCol>, String> {
    if sheet_cat(sheet).is_none() {
        return Ok(vec![]);
    }
    let mut taken: HashSet<String> = cols_of(sheet)
        .iter()
        .map(|c| units::norm(c.header))
        .collect();
    let mut out = param_cols(model, sheet, ids, u, &mut taken)?;
    out.extend(kept.user_cols(sheet, ids, &mut taken));
    Ok(out)
}

/// Parameter and phase columns of a category sheet; their headers go into `taken`.
pub(crate) fn param_cols(
    model: &Model,
    sheet: &str,
    ids: &[i64],
    u: Units,
    taken: &mut HashSet<String>,
) -> Result<Vec<XCol>, String> {
    let Some(sc) = sheet_cat(sheet) else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    let defs = model.params().map_err(err)?;
    let phased = !sc.per_type && ddc_model::PHASED.contains(&sc.kind);
    let phases = phased && !model.phase_index().map_err(err)?.phases.is_empty();
    // headers the sheet reads as something else, though no kept column may go by them
    let mut reserved: HashSet<String> = HashSet::new();
    if super::book::active() {
        let shown = cols_of(sheet).iter().map(|c| super::book::header(c.key, c.header, u));
        reserved.extend(shown.map(|h| units::norm(&h)));
    }
    if phases {
        reserved.extend([PHASE_CREATED, PHASE_DEMOLISHED].map(units::norm));
        let alt = PHASE_CREATED_ALT.iter().chain(&PHASE_DEMOLISHED_ALT);
        reserved.extend(alt.map(|h| units::norm(h)));
    }
    let params = sheet_params(&defs, sc);
    // tags are settled in id order: the older parameter keeps the plain header, so adding a
    // parameter never retitles a column of a book already out
    let mut by_id: Vec<&ParamDef> = params.iter().collect();
    by_id.sort_by_key(|d| d.id);
    let mut tags: HashMap<i64, bool> = HashMap::new();
    for d in by_id {
        let plain = units::norm(&param_header(d));
        let tagged = taken.contains(&plain)
            || reserved.contains(&plain)
            || param_head::split_tag(d.spec.name.trim()).is_some();
        // either form is this parameter's: a column kept under the other is no user column
        taken.insert(plain);
        taken.insert(units::norm(&param_head::tagged_name(&d.spec.name, d.id)));
        tags.insert(d.id, tagged);
    }
    for d in params {
        let canon = param_header(&d);
        let tagged = tags.get(&d.id).copied().unwrap_or(true);
        let k = u.factor(&canon);
        let vals = model.param_values(d.id).map_err(err)?;
        let cells = ids
            .iter()
            .map(|id| match vals.get(&(sc.kind.to_string(), *id)) {
                None => V::Blank,
                Some(PVal::Text(s)) => V::S(s.clone()),
                Some(PVal::Int(i)) => V::N(*i as f64),
                Some(PVal::Num(x)) => V::N(*x / k),
                Some(PVal::Bool(b)) => V::N(f64::from(u8::from(*b))),
            })
            .collect();
        out.push(XCol {
            header: param_shown(&d, tagged, u),
            edit: true,
            cells,
        });
    }
    if phased {
        let idx = model.phase_index().map_err(err)?;
        if !idx.phases.is_empty() {
            let (mut c, mut d) = (vec![], vec![]);
            for id in ids {
                let e = idx.get(sc.kind, *id);
                c.push(V::S(idx.name(e.created).to_string()));
                d.push(
                    e.demolished
                        .map_or(V::Blank, |p| V::S(idx.name(p).to_string())),
                );
            }
            for (h, cells) in [(PHASE_CREATED, c), (PHASE_DEMOLISHED, d)] {
                taken.insert(units::norm(h));
                out.push(XCol {
                    header: h.to_string(),
                    edit: true,
                    cells,
                    });
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- import

/// A category sheet as seen by parsing beyond the format.
pub(crate) struct SheetView<'a> {
    pub name: &'static str,
    /// Header row, 1-based.
    pub r0: u32,
    /// First column of the range, 0-based.
    pub c0: u32,
    pub head: &'a [String],
    /// Format column indices.
    pub known: HashSet<usize>,
    pub rows: &'a [(u32, Vec<Data>)],
    /// Values as exported, if the book carries them.
    pub base: Option<&'a super::baseline::Base>,
    /// (row, cell index) under a merge that were not given: the model value is kept.
    pub covered: &'a HashSet<(u32, usize)>,
}

impl SheetView<'_> {
    fn col(&self, j: usize) -> u32 {
        self.c0 + j as u32 + 1
    }

    fn find(&self, norm: &str) -> Option<usize> {
        self.head.iter().position(|h| units::norm(h) == norm)
    }
}

/// Cell → parameter value (`k` — metres per header unit for lengths/areas/volumes).
fn cell_val(d: &Data, t: ParamType, k: f64) -> Result<Option<PVal>, String> {
    if blank(d) {
        return Ok(None);
    }
    if let Data::Error(e) = d {
        return Err(format!("cell holds an error value ({e}); not applied"));
    }
    let s = text_of(d);
    let num = match d {
        Data::Bool(b) => Some(f64::from(u8::from(*b))),
        // text numbers: grouping and both decimal marks; `1,250` is refused, never 1.25
        Data::String(_) if !matches!(t, ParamType::Text | ParamType::YesNo) => {
            match units::len_value(&s, 1.0) {
                Err(e @ units::NumErr::Ambiguous { .. }) => {
                    return Err(units::num_reason(e, &s, String::new()))
                }
                _ => match units::number(&s, None) {
                    Ok(x) => Some(x),
                    Err(_) if t == ParamType::Length => None,
                    Err(_) => return Err(format!("not a number: '{s}'")),
                },
            }
        }
        d => num_of(d),
    };
    if num.is_some_and(|x| !x.is_finite()) {
        return Err("value is not finite".into());
    }
    let parsed = |t: ParamType| ddc_model::parse_value(t, &s).map_err(invalid);
    Ok(Some(match t {
        ParamType::Text => match d {
            // a number typed into a text cell reads `102`, never `102.0` or float noise
            Data::Float(_) | Data::Int(_) => PVal::Text(num.map_or(s.clone(), units::num_cell_text)),
            _ => PVal::Text(s.clone()),
        },
        ParamType::Length => match num {
            Some(x) => PVal::Num(x * k),
            None => match units::len_value(&s, k) {
                Ok(v) if v.is_finite() => PVal::Num(v),
                r => {
                    let t = format!("not a number or length: '{s}' (e.g. 3.5, 3500 mm, 10'-6\")");
                    return Err(units::num_reason(r.err().unwrap_or(units::NumErr::Invalid), &s, t));
                }
            },
        },
        ParamType::Number | ParamType::Area | ParamType::Volume => match num {
            Some(x) => PVal::Num(x * k),
            None => match parsed(t)? {
                PVal::Num(x) => PVal::Num(x * k),
                p => p,
            },
        },
        ParamType::Int => match num {
            Some(x) if x.fract() == 0.0 && x.abs() < 9e15 => PVal::Int(x as i64),
            Some(x) => return Err(format!("not an integer: {x}")),
            None => parsed(t)?,
        },
        ParamType::YesNo => match (d, num) {
            (Data::Bool(b), _) => PVal::Bool(*b),
            (_, Some(x)) if x == 0.0 || x == 1.0 => PVal::Bool(x == 1.0),
            _ => parsed(t)?,
        },
    }))
}

/// A phase cell of the book against the export: `true` if the book value is an edit to apply.
/// A cell still holding the exported name leaves the model alone; both sides changed is a
/// conflict, kept as the model value unless the user takes the book's.
fn phase_take(rep: &mut Report, v: &SheetView<'_>, (r, j, id): (u32, usize, i64), cur: &str, book: &str) -> bool {
    let key = super::baseline::extra_key(&v.head[j]);
    let bv = |t: &str| if t.is_empty() { BVal::Blank } else { BVal::S(t.into()) };
    let took = |rep: &mut Report| {
        rep.applied_cells.extend(AppliedCell::new(v.name, Some(id), &key, bv(book)));
        true
    };
    let live = |b: &BVal| match b {
        BVal::S(t) => t.trim() == cur,
        BVal::Blank => cur.is_empty(),
        _ => false,
    };
    let base = match super::baseline::base_now(v.base.and_then(|b| b.get(v.name, id, &key)), v.name, Some(id), &key, live) {
        Some(BVal::S(t)) => t.trim().to_string(),
        Some(BVal::Blank) => String::new(),
        _ => return book == cur || took(rep),
    };
    if book == cur {
        return true;
    }
    if book == base {
        rep.model_newer += 1;
        return false;
    }
    if cur == base {
        return took(rep);
    }
    let take = super::baseline::taken(v.name, r, v.col(j));
    rep.conflicts.push(Conflict {
        sheet: v.name.into(),
        row: r,
        col: v.col(j),
        column: v.head[j].clone(),
        id: Some(id),
        base: base.clone(),
        book: book.into(),
        model: cur.into(),
        take_book: take,
    });
    if !take {
        rep.kept.extend(KeptCell::new(v.name, Some(id), &key, bv(book), bv(&base)));
        return false;
    }
    took(rep)
}

fn same_val(a: Option<&PVal>, b: Option<&PVal>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(PVal::Text(x)), Some(PVal::Text(y))) => x == y,
        (Some(PVal::Bool(x)), Some(PVal::Bool(y))) => x == y,
        (Some(PVal::Int(x)), Some(PVal::Int(y))) => x == y,
        (Some(x @ (PVal::Num(_) | PVal::Int(_))), Some(y @ (PVal::Num(_) | PVal::Int(_)))) => {
            match (x.as_f64(), y.as_f64()) {
                (Some(x), Some(y)) => same(x, y),
                _ => false,
            }
        }
        _ => false,
    }
}

fn show(v: Option<&PVal>, k: f64) -> String {
    match v {
        None => String::new(),
        // in header units, without a binary-fraction tail (0.30000000000000004 → 0.3)
        Some(PVal::Num(x)) => {
            let y = x / k;
            let r = (y * 1e9).round() / 1e9;
            if r.is_finite() {
                r.to_string()
            } else {
                y.to_string()
            }
        }
        Some(p) => p.text(),
    }
}

/// Opening kind cell, trimmed and lowercased (" Door " -> "door").
fn opening_kind(d: &Data) -> String {
    text_of(d).trim().to_ascii_lowercase()
}

/// A new-parameter candidate: one per name across all sheets.
struct Cand {
    sheet: &'static str,
    row: u32,
    col: u32,
    header: String,
    name: String,
    per_type: bool,
    cats: Vec<String>,
    /// (row, table, id, cell, metres per header unit).
    cells: Vec<(u32, &'static str, i64, Data, f64)>,
    dim: Dim,
    unit: Option<String>,
}

impl Cand {
    /// Type from the header (unit) and values; values not reducible to a number — text.
    fn guess(&self) -> (ParamType, String) {
        let typed = match self.dim {
            Dim::Len => Some(ParamType::Length),
            Dim::Area => Some(ParamType::Area),
            Dim::Vol => Some(ParamType::Volume),
            Dim::None => None,
        };
        // one vote per whole column: a hard-text or unreadable cell keeps it text, and so does
        // a column whose every cell is day.month text; one plain number among them wins
        let filled: Vec<_> = self.cells.iter().filter(|c| !blank(&c.3)).collect();
        let all_soft = filled.iter().all(|c| matches!(&c.3, Data::String(s) if soft_date(s)));
        if filled.is_empty() || all_soft {
            return (ParamType::Text, String::new());
        }
        let convertible = |t: ParamType| {
            filled.iter().all(|c| {
                !matches!(&c.3, Data::String(s) if hard_text(s))
                    && (matches!(cell_val(&c.3, t, c.4), Ok(Some(_))) || ambiguous(&c.3))
            })
        };
        if let Some(t) = typed {
            if convertible(t) {
                return (t, String::new());
            }
            return (ParamType::Text, String::new());
        }
        let unit = self.unit.clone().unwrap_or_default();
        let nums: Option<Vec<f64>> = filled
            .iter()
            .map(|c| match &c.3 {
                // the decimal reading keeps the guess at Number, which holds either answer
                Data::String(s) if ambiguous(&c.3) => {
                    units::number(s, Some(units::Dec::Comma)).ok()
                }
                Data::String(s) => lossless_number(s),
                d => num_of(d),
            })
            .collect();
        match nums {
            Some(v) if v.iter().all(|x| x.is_finite()) => {
                if unit.is_empty() && v.iter().all(|x| x.fract() == 0.0 && x.abs() < 9e15) {
                    (ParamType::Int, unit)
                } else {
                    (ParamType::Number, unit)
                }
            }
            _ => (ParamType::Text, String::new()),
        }
    }
}

/// Normalised headers the parameter reader takes on a category sheet in the family layout:
/// parameter and phase columns. Empty for a sheet that is no category sheet.
pub(crate) fn main_heads(model: &Model, sheet: &str) -> Result<HashSet<String>, String> {
    if sheet_cat(sheet).is_none() {
        return Ok(HashSet::new());
    }
    let defs = model.params().map_err(err)?;
    let mut out: HashSet<String> = defs.iter().map(|d| units::norm(&d.spec.name)).collect();
    out.extend(defs.iter().map(|d| units::norm(&param_head::tagged_name(&d.spec.name, d.id))));
    out.extend([PHASE_CREATED, PHASE_DEMOLISHED].map(units::norm));
    out.extend(PHASE_CREATED_ALT.iter().chain(&PHASE_DEMOLISHED_ALT).map(|h| units::norm(h)));
    Ok(out)
}

/// The header row of a parameter sheet the app wrote in the family layout.
pub(crate) fn is_family_layout(head: &[String]) -> bool {
    let marker = units::norm("level (read-only)");
    head.iter().any(|h| units::norm(h) == marker)
}

/// Columns of a parameter sheet in the family layout (the app writes it with editable geometry)
/// that the parameter reader skips: all but parameter and phase columns, which the family reader
/// leaves to it. None for the parameter sheet layout.
pub(crate) fn family_known(
    model: &Model,
    sheet: &str,
    head: &[String],
) -> Result<Option<HashSet<usize>>, String> {
    if !is_family_layout(head) {
        return Ok(None);
    }
    let ours = main_heads(model, sheet)?;
    // a family's own column is never a parameter column, whatever a parameter is called
    let fam = FAMILY_OWN.with(|o| o.borrow().get(sheet).cloned()).unwrap_or_default();
    let known = head
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            let n = units::norm(h);
            !ours.contains(&n) || fam.contains(&n)
        })
        .map(|(j, _)| j)
        .collect();
    Ok(Some(known))
}

thread_local! {
    /// Normalised headers of the family sheets of the import running on this thread.
    static FAMILY_OWN: std::cell::RefCell<HashMap<String, HashSet<String>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Clears the family columns of this thread's import when dropped.
pub(crate) struct FamilyOwn;

impl Drop for FamilyOwn {
    fn drop(&mut self) {
        FAMILY_OWN.with(|o| o.borrow_mut().clear());
    }
}

/// Names the family columns for the import running on this thread until the guard drops.
pub(crate) fn family_own(fams: &[super::family::FamilySheet]) -> FamilyOwn {
    let own = fams.iter().map(|f| (f.sheet.to_string(), super::family::known_heads(f)));
    FAMILY_OWN.with(|o| *o.borrow_mut() = own.collect());
    FamilyOwn
}

/// Metres per unit of a column under header `h`, 1 without a known unit.
fn head_factor(h: &str) -> f64 {
    [Dim::Len, Dim::Area, Dim::Vol]
        .into_iter()
        .find_map(|d| units::header_factor(h, d))
        .unwrap_or(1.0)
}

/// Normalised name of the parameter of another sheet or category a header refers to.
fn other_sheet_param(h: &str, n: &str, defs: &[ParamDef]) -> Option<String> {
    if let Some(d) = param_head::tag_of(h, defs) {
        return Some(units::norm(&d.spec.name));
    }
    defs.iter().any(|d| units::norm(&d.spec.name) == n).then(|| n.to_string())
}

/// Parameter values, stages and new-parameter proposals from all category sheets.
pub(crate) fn import_extra(
    model: &Model,
    views: &[SheetView],
    u: Units,
    new_params: bool,
    rep: &mut Report,
    edits: &mut Vec<Edit>,
) -> Result<(), String> {
    let defs = model.params().map_err(err)?;
    let idx = model.phase_index().map_err(err)?;
    let mut deleted: HashSet<(&str, i64)> = HashSet::new();
    for e in edits.iter() {
        match e {
            Edit::Delete { table, id } => deleted.insert((*table, *id)),
            Edit::DeleteRoom(i) => deleted.insert(("room", *i)),
            Edit::DeleteCurtain(i) => deleted.insert(("curtain_wall", *i)),
            _ => false,
        };
    }
    let db = model.sql();
    let mut exists: HashMap<&str, HashSet<i64>> = HashMap::new();
    for sc in &SHEET_CATS {
        let mut st = db
            .prepare(&format!("SELECT id FROM {}", sc.kind))
            .map_err(err)?;
        let ids = st
            .query_map([], |r| r.get::<_, i64>(0))
            .map_err(err)?
            .collect::<Result<HashSet<i64>, _>>()
            .map_err(err)?;
        exists.insert(sc.kind, ids);
    }
    // opening: (wall, kind) — the kind gives the category, the wall — demolition together with it
    let mut opening: HashMap<i64, (i64, String)> = HashMap::new();
    {
        let mut st = db
            .prepare("SELECT id, wall, kind FROM opening")
            .map_err(err)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?))))
            .map_err(err)?;
        for r in rows {
            let (i, v) = r.map_err(err)?;
            opening.insert(i, v);
        }
    }
    let mut values: HashMap<i64, HashMap<(String, i64), PVal>> = HashMap::new();
    let mut cands: Vec<Cand> = vec![];
    let ignore = |rep: &mut Report, v: &SheetView, r: u32, j: usize, id, old, new, why: String| {
        rep.ignored.push(Ignored {
            sheet: v.name.into(),
            row: r,
            col: if j == usize::MAX { 0 } else { v.col(j) },
            column: v.head.get(j).cloned().unwrap_or_default(),
            id,
            old,
            new,
            reason: why,
        });
    };
    let reject = |rep: &mut Report, v: &SheetView, r: u32, j: usize, why: String| {
        rep.rejected.push(Rejection {
            sheet: v.name.into(),
            row: r,
            col: v.col(j),
            column: v.head.get(j).cloned().unwrap_or_default(),
            reason: why,
        });
    };

    for v in views {
        let Some(sc) = sheet_cat(v.name) else {
            continue;
        };
        let own = sheet_params(&defs, sc);
        let phased = !sc.per_type && ddc_model::PHASED.contains(&sc.kind) && !idx.phases.is_empty();
        let Some(id_col) = v.find("id") else {
            continue;
        };
        let del_col = v.find("delete");
        let kind_col = (sc.kind == "opening").then(|| v.find("kind")).flatten();
        let (mut created_col, mut demol_col) = (None, None);
        let mut pcols: Vec<(usize, ParamDef, f64)> = vec![];
        let mut ccols: Vec<(usize, usize)> = vec![]; // (column, candidate index)
        let mut first_of: HashMap<String, usize> = HashMap::new();
        let mut groups: Vec<super::dup_cols::Group> = vec![];
        for (j, h) in v.head.iter().enumerate() {
            if v.known.contains(&j) || h.trim().is_empty() {
                continue;
            }
            let n = units::norm(h);
            let is_phase = phased && (is_created_head(&n) || is_demolished_head(&n));
            let bound = if is_phase { Bound::None } else { param_head::bind(h, &own) };
            // two columns repeat each other when they carry one field, not when they look alike
            let slot = match &bound {
                Bound::One(d) => format!("\u{1}{}", d.id),
                _ => n.clone(),
            };
            if let Some(&keep) = first_of.get(&slot) {
                let k_of = |c: usize| match &bound {
                    Bound::One(d) => param_factor(&v.head[c], d, u).unwrap_or(1.0),
                    _ => head_factor(&v.head[c]),
                };
                match groups.iter_mut().find(|g| g.keep.0 == keep) {
                    Some(g) => g.copies.push((j, k_of(j))),
                    None => groups.push(super::dup_cols::Group {
                        keep: (keep, k_of(keep)),
                        copies: vec![(j, k_of(j))],
                        names_row: false,
                    }),
                }
                continue;
            }
            first_of.insert(slot, j);
            if phased && is_created_head(&n) {
                created_col = Some(j);
            } else if phased && is_demolished_head(&n) {
                demol_col = Some(j);
            } else if let Bound::One(d) = &bound {
                let Ok(k) = param_factor(h, d, u) else {
                    let why = "unknown unit in header (use m, mm, cm, ft, in)".to_string();
                    reject(rep, v, v.r0, j, why);
                    continue;
                };
                if !values.contains_key(&d.id) {
                    values.insert(d.id, model.param_values(d.id).map_err(err)?);
                }
                pcols.push((j, (*d).clone(), k));
            } else if let Bound::Many(names) = &bound {
                let why = super::msg::fill_template(
                    super::msg::PARAM_AMBIGUOUS.1,
                    &[h.trim(), &names.join(", ")],
                );
                ignore(rep, v, v.r0, j, None, String::new(), String::new(), why);
            } else if let Some(n) = other_sheet_param(h, &n, &defs) {
                let name = strip_unit(h);
                // bound here but kept on the other kind of row: say which sheet holds it
                let elsewhere = defs.iter().any(|d| {
                    units::norm(&d.spec.name) == n
                        && d.spec.per_type != sc.per_type
                        && d.spec.categories.iter().any(|c| sc.cats.contains(&c.as_str()))
                });
                let why = match (elsewhere, sc.per_type) {
                    (true, false) => format!(
                        "parameter '{name}' is a type parameter; edit it on the types sheet, not applied"
                    ),
                    (true, true) => format!(
                        "parameter '{name}' is an instance parameter; edit it on the elements sheet, not applied"
                    ),
                    _ => format!("parameter '{name}' is not bound to this category; not applied"),
                };
                ignore(rep, v, v.r0, j, None, String::new(), String::new(), why);
            } else if matches!(bound, Bound::StaleTag) {
                let why = super::msg::fill_template(super::msg::PARAM_TAG_STALE.1, &[h.trim()]);
                ignore(rep, v, v.r0, j, None, String::new(), String::new(), why);
            } else {
                let name = strip_unit(h);
                if name.is_empty() {
                    continue;
                }
                let key = units::norm(&name);
                let dim = [Dim::Len, Dim::Area, Dim::Vol]
                    .into_iter()
                    .find(|d| units::header_factor(h, *d).is_some())
                    .unwrap_or(Dim::None);
                let unit = if dim == Dim::None {
                    unit_label(h)
                } else {
                    None
                };
                match cands.iter().position(|c| units::norm(&c.name) == key) {
                    Some(i) if cands[i].per_type == sc.per_type => {
                        for c in sc.cats {
                            if !cands[i].cats.iter().any(|x| x == c) {
                                cands[i].cats.push(c.to_string());
                            }
                        }
                        ccols.push((j, i));
                    }
                    Some(_) => {
                        let why = format!(
                            "parameter '{name}' is not bound to this category; not applied"
                        );
                        ignore(rep, v, v.r0, j, None, String::new(), String::new(), why);
                    }
                    None => {
                        cands.push(Cand {
                            sheet: v.name,
                            row: v.r0,
                            col: v.col(j),
                            header: h.trim().to_string(),
                            name,
                            per_type: sc.per_type,
                            cats: sc.cats.iter().map(|c| c.to_string()).collect(),
                            cells: vec![],
                            dim,
                            unit,
                        });
                        ccols.push((j, cands.len() - 1));
                    }
                }
            }
        }
        // cells whose copies differ stay out, as cells under a merge do
        let mut gone: HashSet<(u32, usize)> = HashSet::new();
        if !groups.is_empty() {
            gone = v.covered.clone();
            let sheet = super::dup_cols::Sheet {
                name: v.name,
                r0: v.r0,
                c0: v.c0,
                head: v.head,
                rows: v.rows,
            };
            super::dup_cols::check(&sheet, &groups, &mut gone, rep);
        }
        if pcols.is_empty() && ccols.is_empty() && created_col.is_none() && demol_col.is_none() {
            continue;
        }
        let factor_of = |j: usize| -> f64 { head_factor(&v.head[j]) };
        let extra_cols: Vec<usize> = pcols
            .iter()
            .map(|p| p.0)
            .chain(ccols.iter().map(|c| c.0))
            .chain(created_col)
            .chain(demol_col)
            .collect();
        for (r, cells) in v.rows {
            let r = *r;
            let get = |j: usize| cells.get(j).filter(|d| !blank(d) && !gone.contains(&(r, j)));
            let given = |j: &usize| !v.covered.contains(&(r, *j)) && !gone.contains(&(r, *j));
            if cells.iter().all(blank) {
                continue;
            }
            let Some(idd) = get(id_col) else {
                // a new row: its parameter values go in with the creating edit, found by the
                // row mark the creating edit carries
                let n = (!sc.per_type
                    && edits
                        .iter()
                        .any(|e| matches!(e, Edit::Mark { kind, row } if kind.as_str() == sc.kind && *row == r)))
                .then_some(r);
                // category of the new row: an opening takes it from its kind cell (a void has none)
                let new_cat: Option<String> = if sc.kind == "opening" {
                    kind_col
                        .and_then(get)
                        .map(opening_kind)
                        .filter(|k| k == "door" || k == "window")
                } else {
                    sc.cats.first().map(|c| c.to_string())
                };
                for &j in &extra_cols {
                    let Some(d) = get(j) else { continue };
                    if let (Some(n), Some((_, pd, k))) = (n, pcols.iter().find(|p| p.0 == j)) {
                        let bound = new_cat
                            .as_deref()
                            .is_some_and(|nc| pd.spec.categories.iter().any(|c| c == nc));
                        if !bound {
                            let why = format!(
                                "parameter '{}' is not bound to this category; not applied",
                                pd.spec.name
                            );
                            ignore(rep, v, r, j, None, String::new(), text_of(d), why);
                            continue;
                        }
                        {
                            match cell_val(d, pd.spec.ptype, *k) {
                                Ok(Some(val)) => {
                                    rep.changes.push(Change {
                                        sheet: v.name.into(),
                                        row: r,
                                        col: v.col(j),
                                        column: v.head[j].clone(),
                                        id: None,
                                        old: String::new(),
                                        new: show(Some(&val), *k),
                                    });
                                    edits.push(Edit::ParamValueNew {
                                        param: pd.spec.name.clone(),
                                        kind: sc.kind.into(),
                                        row: n,
                                        value: Some(val),
                                    });
                                }
                                Ok(None) => {}
                                Err(why) => reject(rep, v, r, j, why),
                            }
                            continue;
                        }
                    }
                    if let (Some(n), Some(&(_, ci))) = (n, ccols.iter().find(|c| c.0 == j)) {
                        // negative id: the `-1 - id`-th new row of the batch
                        cands[ci]
                            .cells
                            .push((r, sc.kind, -1 - i64::from(n), d.clone(), factor_of(j)));
                        continue;
                    }
                    let why = "new row: values are taken after re-export; not applied".to_string();
                    ignore(rep, v, r, j, None, String::new(), text_of(d), why);
                }
                continue;
            };
            let Some(id) = int_of(idd) else { continue };
            if !exists.get(sc.kind).is_some_and(|s| s.contains(&id)) {
                continue;
            }
            if let Some(d) = del_col.and_then(get) {
                match units::delete_word(d) {
                    units::DelWord::Yes => continue,
                    units::DelWord::No => {}
                    units::DelWord::Unknown(t) => {
                        // the main pass refuses the row too; the row is kept, never deleted
                        let why =
                            super::msg::fill_template(super::msg::DELETE_UNKNOWN.1, &[t.as_str()]);
                        let seen = rep
                            .rejected
                            .iter()
                            .any(|x| x.sheet == v.name && x.row == r && x.reason == why);
                        if !seen {
                            reject(rep, v, r, del_col.unwrap_or(0), why);
                        }
                    }
                }
            }
            if deleted.contains(&(sc.kind, id)) {
                continue;
            }
            let op = (sc.kind == "opening").then(|| opening.get(&id)).flatten();
            if op.is_some_and(|o| deleted.contains(&("wall", o.0))) {
                continue;
            }
            // row category: for an opening — by kind (a niche has no category)
            let cat: Option<&str> = match op {
                Some((_, k)) if k == "door" => Some("door"),
                Some((_, k)) if k == "window" => Some("window"),
                Some(_) => None,
                None => sc.cats.first().copied(),
            };
            for (j, d, k) in &pcols {
                let Some(cell) = cells.get(*j).filter(|_| given(j)) else {
                    continue;
                };
                let new = match cell_val(cell, d.spec.ptype, *k) {
                    Ok(x) => x,
                    Err(why) => {
                        reject(rep, v, r, *j, why);
                        continue;
                    }
                };
                let bound =
                    sc.per_type || cat.is_some_and(|c| d.spec.categories.iter().any(|x| x == c));
                if !bound {
                    if new.is_some() {
                        let why = match cat {
                            None => "void opening has no parameters; not applied".to_string(),
                            Some(_) => format!(
                                "parameter '{}' is not bound to this category; not applied",
                                d.spec.name
                            ),
                        };
                        ignore(rep, v, r, *j, Some(id), String::new(), text_of(cell), why);
                    }
                    continue;
                }
                let cur = values
                    .get(&d.id)
                    .and_then(|m| m.get(&(sc.kind.to_string(), id)));
                if same_val(cur, new.as_ref()) {
                    continue;
                }
                // a code the spreadsheet turned into a number keeps the model text
                // (`01` -> 1 kept with a note, a code past 15 digits refused, a date never renames)
                if d.spec.ptype == ParamType::Text {
                    let model_t = match cur {
                        Some(PVal::Text(c)) => c.as_str(),
                        _ => "",
                    };
                    if matches!(cell, Data::Float(_) | Data::Int(_)) {
                        match units::num_text(cell, model_t, units::book_dec()) {
                            units::NumText::Same => {
                                let why = super::textcode::REASON.to_string();
                                ignore(rep, v, r, *j, Some(id), model_t.to_string(), text_of(cell), why);
                                continue;
                            }
                            units::NumText::Lossy => {
                                reject(rep, v, r, *j, super::textcode::LOSSY.to_string());
                                continue;
                            }
                            units::NumText::Edit(_) => {}
                        }
                    }
                    if matches!(cell, Data::DateTime(_)) && !model_t.is_empty() {
                        let why = "the spreadsheet turned this value into a date \u{2014} format the column as Text and type it again";
                        reject(rep, v, r, *j, why.to_string());
                        continue;
                    }
                }
                // a cell cut to fit the sheet limit is read-only: unchanged form skipped, edits refused
                if let (Data::String(t), Some(PVal::Text(_)) | None) = (cell, cur) {
                    let c = match cur {
                        Some(PVal::Text(c)) => Some(c.as_str()),
                        _ => None,
                    };
                    if let Some(same) = super::cellsafe::cut_cell(t, c) {
                        if !same {
                            let why = super::cellsafe::CLIPPED_REASON.to_string();
                            ignore(rep, v, r, *j, Some(id), String::new(), String::new(), why);
                        }
                        continue;
                    }
                }
                // book value as exported: the model moved on; edited on both sides: conflict
                let key = super::baseline::extra_key(&v.head[*j]);
                let as_val = |b: &BVal| match b {
                    BVal::N(x) => cell_val(&Data::Float(*x), d.spec.ptype, *k).ok(),
                    BVal::S(t) => cell_val(&Data::String(t.clone()), d.spec.ptype, *k).ok(),
                    BVal::Blank => Some(None),
                    _ => None,
                };
                // a value an earlier Apply took from this book, while the model still holds it
                let live = |b: &BVal| as_val(b).is_some_and(|w| same_val(w.as_ref(), cur));
                let file = v.base.and_then(|b| b.get(v.name, id, &key));
                let was = super::baseline::base_now(file, v.name, Some(id), &key, live).and_then(|b| as_val(&b));
                if let Some(was) = was {
                    if same_val(was.as_ref(), new.as_ref()) {
                        rep.model_newer += 1;
                        continue;
                    }
                    if !same_val(was.as_ref(), cur) {
                        let take = super::baseline::taken(v.name, r, v.col(*j));
                        rep.conflicts.push(Conflict {
                            sheet: v.name.into(),
                            row: r,
                            col: v.col(*j),
                            column: v.head[*j].clone(),
                            id: Some(id),
                            base: show(was.as_ref(), *k),
                            book: show(new.as_ref(), *k),
                            model: show(cur, *k),
                            take_book: take,
                        });
                        if !take {
                            let was = v.base.and_then(|b| b.get(v.name, id, &key)).cloned();
                            let now = KeptCell::new(v.name, Some(id), &key, kept_book(cell), was.unwrap_or(BVal::Blank));
                            rep.kept.extend(now);
                            continue;
                        }
                    }
                }
                rep.changes.push(Change {
                    sheet: v.name.into(),
                    row: r,
                    col: v.col(*j),
                    column: v.head[*j].clone(),
                    id: Some(id),
                    old: show(cur, *k),
                    new: show(new.as_ref(), *k),
                });
                rep.applied_cells.extend(AppliedCell::new(v.name, Some(id), &key, kept_book(cell)));
                edits.push(Edit::ParamValue {
                    param: d.spec.name.clone(),
                    kind: sc.kind.into(),
                    id,
                    value: new,
                });
            }
            if created_col.is_some() || demol_col.is_some() {
                let cur = idx.get(sc.kind, id);
                let by_name = |t: &str| ddc_model::find_phase(&idx.phases, t);
                let mut ok = true;
                let mut created = cur.created;
                if let Some(j) = created_col {
                    if let Some(d) = get(j) {
                        let t = text_of(d);
                        match by_name(&t) {
                            Some(p) => {
                                let (now, book) = (idx.name(cur.created).to_string(), idx.name(p).to_string());
                                if phase_take(rep, v, (r, j, id), &now, &book) {
                                    created = p;
                                }
                            }
                            None => {
                                reject(rep, v, r, j, format!("unknown phase '{t}'"));
                                ok = false;
                            }
                        }
                    }
                }
                let mut demolished = cur.demolished;
                if let Some(j) = demol_col.filter(given) {
                    let now = cur.demolished.map_or(String::new(), |p| idx.name(p).to_string());
                    demolished = match get(j) {
                        None => {
                            if phase_take(rep, v, (r, j, id), &now, "") {
                                None
                            } else {
                                cur.demolished
                            }
                        }
                        Some(d) => {
                            let t = text_of(d);
                            match by_name(&t) {
                                Some(p) => {
                                    let book = idx.name(p).to_string();
                                    if phase_take(rep, v, (r, j, id), &now, &book) {
                                        Some(p)
                                    } else {
                                        cur.demolished
                                    }
                                }
                                None => {
                                    reject(rep, v, r, j, format!("unknown phase '{t}'"));
                                    ok = false;
                                    cur.demolished
                                }
                            }
                        }
                    };
                }
                if ok {
                    if let (Some(dm), Some(j)) = (demolished, demol_col) {
                        if idx.pos(dm) < idx.pos(created) {
                            let why = format!(
                                "demolition phase '{}' precedes creation phase '{}'",
                                idx.name(dm),
                                idx.name(created)
                            );
                            reject(rep, v, r, j, why);
                            ok = false;
                        }
                    }
                }
                if ok && (created != cur.created || demolished != cur.demolished) {
                    let name =
                        |p: Option<i64>| p.map_or(String::new(), |p| idx.name(p).to_string());
                    for (j, a, b) in [
                        (created_col, Some(cur.created), Some(created)),
                        (demol_col, cur.demolished, demolished),
                    ] {
                        if let (Some(j), true) = (j, a != b) {
                            rep.changes.push(Change {
                                sheet: v.name.into(),
                                row: r,
                                col: v.col(j),
                                column: v.head[j].clone(),
                                id: Some(id),
                                old: name(a),
                                new: name(b),
                            });
                        }
                    }
                    edits.push(Edit::Phase {
                        kind: sc.kind.into(),
                        id,
                        created,
                        demolished,
                    });
                }
            }
            for &(j, ci) in &ccols {
                let Some(d) = get(j) else { continue };
                if cat.is_none() {
                    let why = "void opening has no parameters; not applied".to_string();
                    ignore(rep, v, r, j, Some(id), String::new(), text_of(d), why);
                    continue;
                }
                cands[ci]
                    .cells
                    .push((r, sc.kind, id, d.clone(), factor_of(j)));
            }
        }
    }

    for c in cands {
        let (ptype, unit) = c.guess();
        let values = c
            .cells
            .iter()
            .filter_map(|(r, kind, id, d, k)| {
                let v = cell_val(d, ptype, *k).ok().flatten()?;
                Some((*r, kind.to_string(), *id, v))
            })
            .collect();
        rep.proposals.push(Proposal {
            sheet: c.sheet.into(),
            row: c.row,
            col: c.col,
            header: c.header,
            spec: ParamSpec {
                name: c.name,
                ptype,
                per_type: c.per_type,
                unit,
                group: String::new(),
                guid: None,
                categories: c.cats,
            },
            values,
            accept: new_params,
            refused: vec![],
        });
    }
    Ok(())
}

/// The Quantities sheet is recomputed from the model: a differing cell is "read only", not a rejection.
pub(crate) fn quantities(
    model: &Model,
    range: &Range<Data>,
    u: Units,
    rep: &mut Report,
) -> Result<(), String> {
    let Some((r0, c0)) = range.start() else {
        return Ok(());
    };
    let mut it = range.rows();
    let Some(head) = it.next() else {
        return Ok(());
    };
    let head: Vec<String> = head.iter().map(text_of).collect();
    let find = |key: &str| {
        let c = QUANTITIES_COLS.iter().find(|c| c.key == key)?;
        let want = units::norm(c.header);
        let j = head.iter().position(|h| units::norm(h) == want)?;
        Some((j, c.header))
    };
    let (Some((jw, _)), Some((jp, _))) = (find("wall"), find("pos")) else {
        return Ok(());
    };
    let mut cur: HashMap<(i64, i64), (String, f64, f64)> = HashMap::new();
    {
        let mut st = model
            .sql()
            .prepare("SELECT wall,pos,material,plan_area,volume FROM wall_qty")
            .map_err(err)?;
        let rows = st
            .query_map([], |r| {
                Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?, r.get(4)?)))
            })
            .map_err(err)?;
        for r in rows {
            let (k, v) = r.map_err(err)?;
            cur.insert(k, v);
        }
    }
    let num_cols: Vec<(usize, f64, usize)> = [("area", 1usize), ("volume", 2usize)]
        .into_iter()
        .filter_map(|(key, slot)| {
            let (j, canon) = find(key)?;
            let d = units::dim(canon);
            let k = units::header_factor(&head[j], d).unwrap_or_else(|| u.factor(canon));
            Some((j, k, slot))
        })
        .collect();
    let mat = find("material").map(|x| x.0);
    let read_only = "read-only sheet; not applied";
    for (i, cells) in it.enumerate() {
        let r = r0 + i as u32 + 2;
        if cells.iter().all(blank) {
            continue;
        }
        let at = |j: usize| c0 + j as u32 + 1;
        let key = cells
            .get(jw)
            .and_then(int_of)
            .zip(cells.get(jp).and_then(int_of));
        let Some(q) = key.and_then(|k| cur.get(&k).map(|q| (k, q))) else {
            rep.ignored.push(Ignored {
                sheet: QUANTITIES.into(),
                row: r,
                col: 0,
                column: String::new(),
                id: key.map(|k| k.0),
                old: String::new(),
                new: String::new(),
                reason: "row is not in the model; not applied".into(),
            });
            continue;
        };
        let ((wall, _), (m0, a0, v0)) = q;
        let mut push = |j: usize, old: String, new: String| {
            rep.ignored.push(Ignored {
                sheet: QUANTITIES.into(),
                row: r,
                col: at(j),
                column: head[j].clone(),
                id: Some(wall),
                old,
                new,
                reason: read_only.into(),
            });
        };
        if let Some(j) = mat {
            let t = cells.get(j).map_or(String::new(), text_of);
            if &t != m0 {
                push(j, m0.clone(), t);
            }
        }
        for &(j, k, slot) in &num_cols {
            let model_v = if slot == 1 { *a0 } else { *v0 };
            let cell = cells.get(j).unwrap_or(&Data::Empty);
            let book = match cell {
                Data::String(s) => units::number(s, None).ok(),
                d => num_of(d),
            };
            if !book.is_some_and(|b| same(b * k, model_v)) {
                push(j, (model_v / k).to_string(), text_of(cell));
            }
        }
    }
    Ok(())
}

/// QuantitiesSummary is recomputed from the model: a cell that differs is reported as read-only,
/// never applied. The caller runs it only while the model is as exported.
pub(crate) fn summary(
    model: &Model,
    range: &Range<Data>,
    u: Units,
    rep: &mut Report,
) -> Result<(), String> {
    let Some((r0, c0)) = range.start() else {
        return Ok(());
    };
    let mut it = range.rows();
    let Some(head) = it.next() else {
        return Ok(());
    };
    let head: Vec<String> = head.iter().map(text_of).collect();
    let find = |key: &str| {
        let c = SUMMARY_COLS.iter().find(|c| c.key == key)?;
        let want = units::norm(c.header);
        let j = head.iter().position(|h| units::norm(h) == want)?;
        Some((j, c.header))
    };
    let (Some((jc, _)), Some((jt, _)), Some((jl, _))) =
        (find("category"), find("type"), find("level"))
    else {
        return Ok(());
    };
    let key = |a: &str, b: &str, c: &str| format!("{a}\u{1}{b}\u{1}{c}");
    let mut cur: HashMap<String, Vec<Option<f64>>> = HashMap::new();
    for row in super::export::query(model, super::export::SUMMARY_SQL, 6)? {
        let t = |i: usize| match row.get(i) {
            Some(V::S(s)) => s.trim().to_string(),
            _ => String::new(),
        };
        let n = |i: usize| match row.get(i) {
            Some(V::N(x)) => Some(*x),
            _ => None,
        };
        cur.insert(key(&t(0), &t(1), &t(2)), vec![n(3), n(4), n(5)]);
    }
    let cols: Vec<(usize, f64, usize)> = [("count", 0usize), ("area", 1usize), ("volume", 2usize)]
        .into_iter()
        .filter_map(|(k, slot)| {
            let (j, canon) = find(k)?;
            let k = units::header_factor(&head[j], units::dim(canon)).unwrap_or_else(|| u.factor(canon));
            Some((j, k, slot))
        })
        .collect();
    for (i, cells) in it.enumerate() {
        let r = r0 + i as u32 + 2;
        if cells.iter().all(blank) {
            continue;
        }
        let at = |j: usize| c0 + j as u32 + 1;
        let txt = |j: usize| cells.get(j).map_or(String::new(), text_of);
        let Some(model_row) = cur.get(&key(&txt(jc), &txt(jt), &txt(jl))) else {
            rep.ignored.push(Ignored {
                sheet: SUMMARY.into(),
                row: r,
                col: 0,
                column: String::new(),
                id: None,
                old: String::new(),
                new: String::new(),
                reason: "row is not in the model; not applied".into(),
            });
            continue;
        };
        for &(j, k, slot) in &cols {
            let cell = cells.get(j).unwrap_or(&Data::Empty);
            let book = match cell {
                d if blank(d) => None,
                Data::String(s) => units::number(s, None).ok(),
                d => num_of(d),
            };
            let model_v = model_row[slot];
            let ok = match (book, model_v) {
                (None, None) => true,
                (Some(b), Some(m)) => same(b * k, m),
                _ => false,
            };
            if !ok {
                rep.ignored.push(Ignored {
                    sheet: SUMMARY.into(),
                    row: r,
                    col: at(j),
                    column: head[j].clone(),
                    id: None,
                    old: model_v.map_or(String::new(), |m| (m / k).to_string()),
                    new: text_of(cell),
                    reason: "read-only sheet; not applied".into(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("ddc_xlsx_tests");
        std::fs::create_dir_all(&d).unwrap();
        d.join(format!("{}_xl13_{name}.xlsx", std::process::id()))
    }

    fn garbage(name: &str) -> PathBuf {
        let p = tmp(name);
        std::fs::write(&p, b"not a zip, locked or damaged").unwrap();
        p
    }

    fn s(t: &str) -> Data {
        Data::String(t.to_string())
    }

    #[test]
    fn new_door_row_kind_with_spaces_applies() {
        assert_eq!(opening_kind(&s(" door")), "door");
        assert_eq!(opening_kind(&s("Window ")), "window");
        assert_eq!(opening_kind(&s("  DOOR\t")), "door");
    }

    #[test]
    fn new_opening_kind_cell_is_trimmed() {
        assert_eq!(opening_kind(&s(" niche ")), "niche");
        assert!(matches!(opening_kind(&s(" window ")).as_str(), "door" | "window"));
    }

    #[test]
    fn id_cell_with_spaces_parses() {
        assert_eq!(int_of(&s(" 12 ")), Some(12));
        assert_eq!(int_of(&s("1,000")), None, "a grouped id is no id");
    }

    fn num_cell(d: &Data) -> Result<Option<PVal>, String> {
        cell_val(d, ParamType::Number, 1.0)
    }

    #[test]
    fn own_export_numbers_read_back_exactly_in_both_marks() {
        use super::super::import::ro_shown;
        let xs = [1.5, 1234.5, 1000.0, 1.125, -0.25, 3.0, 12.5];
        for (dec, mark) in [(units::Dec::Comma, ','), (units::Dec::Point, '.')] {
            let _g = units::DecGuard::set(Some(dec));
            for x in xs {
                assert_eq!(num_cell(&Data::Float(x)), Ok(Some(PVal::Num(x))), "typed {x}");
                let t = ro_shown(x, "value");
                let other = if mark == ',' { '.' } else { ',' };
                assert!(!t.contains(other), "{t:?} uses the book mark only");
                assert_eq!(num_cell(&s(&t)), Ok(Some(PVal::Num(x))), "text {t:?} under {dec:?}");
                assert_eq!(lossless_number(&t), Some(x), "guess {t:?} under {dec:?}");
            }
            // the book mark settles a value that alone reads two ways
            let want = if dec == units::Dec::Comma { 1.0 } else { 1000.0 };
            assert_eq!(num_cell(&s("1,000")), Ok(Some(PVal::Num(want))));
        }
    }

    #[test]
    fn text_numbers_without_book_mark() {
        let _g = units::DecGuard::set(None);
        for (t, want) in [("1,5", 1.5), ("1.234,5", 1234.5), ("1,234.5", 1234.5), ("-0,25", -0.25)] {
            assert_eq!(num_cell(&s(t)), Ok(Some(PVal::Num(want))), "{t}");
            assert_eq!(lossless_number(t), Some(want), "{t}");
        }
        assert_eq!(num_cell(&Data::Float(1.25)), Ok(Some(PVal::Num(1.25))), "typed cell as is");
        assert_eq!(num_cell(&Data::Int(1000)), Ok(Some(PVal::Num(1000.0))));
    }

    #[test]
    fn ambiguous_text_number_is_refused_not_guessed() {
        let _g = units::DecGuard::set(None);
        for t in ["1,000", "1,250", "-12,500"] {
            let e = num_cell(&s(t)).expect_err(t);
            assert!(e.starts_with(&format!("ambiguous number '{t}'")), "{e}");
            assert!(ambiguous(&s(t)), "{t}");
            assert_eq!(lossless_number(t), None, "{t}");
        }
    }

    #[test]
    fn guess_keeps_lossy_or_date_like_text_as_text() {
        let _g = units::DecGuard::set(None);
        for t in ["007", "01.10", "15.03", "1.2.3", "1e3", "1 250", "", "-", "abc"] {
            assert_eq!(lossless_number(t), None, "{t:?}");
        }
        let _p = units::DecGuard::set(Some(units::Dec::Point));
        assert_eq!(lossless_number("15.03"), Some(15.03), "soft day.month: the column decides");
        assert_eq!(lossless_number("01.10"), None, "leading zero code stays text");
        assert_eq!(lossless_number("3.14"), Some(3.14));
        assert_eq!(lossless_number("12.50"), Some(12.5), "trailing fraction zero is a price");
        assert!(soft_date("1.05") && soft_date("20.12") && soft_date("15.03"));
        assert!(!soft_date("13.4") && !soft_date("2.50") && !soft_date("01.10"));
        assert!(!soft_date("32.01") && !soft_date("3.14"));
    }

    #[test]
    fn soft_day_month_cells_do_not_flip_a_numeric_column() {
        for dec in [units::Dec::Point, units::Dec::Comma] {
            let _g = units::DecGuard::set(Some(dec));
            let (a, b, c, d) = if dec == units::Dec::Point {
                ("1.05", "2.50", "20.12", "13.4")
            } else {
                ("1,05", "2,50", "20,12", "13,4")
            };
            assert!(matches!(guessed(&[a, b, c, d]), ParamType::Number), "{dec:?}");
        }
        let _p = units::DecGuard::set(Some(units::Dec::Point));
        assert!(matches!(guessed(&["15.03", "01.04", "20.12"]), ParamType::Text));
        assert!(matches!(guessed(&["15.03", "1.04", "20.12"]), ParamType::Text), "all day.month");
        assert!(matches!(guessed(&["1.05"]), ParamType::Text), "a lone day.month cell");
    }

    fn cand(cells: &[Data], unit: Option<&str>) -> Cand {
        Cand {
            sheet: "Walls",
            row: 0,
            col: 0,
            header: "X".into(),
            name: "X".into(),
            per_type: false,
            cats: vec![],
            cells: cells
                .iter()
                .enumerate()
                .map(|(i, d)| (i as u32 + 1, "wall", i as i64 + 1, d.clone(), 1.0))
                .collect(),
            dim: Dim::None,
            unit: unit.map(str::to_string),
        }
    }

    fn guessed(cells: &[&str]) -> ParamType {
        cand(&cells.iter().map(|t| s(t)).collect::<Vec<_>>(), None).guess().0
    }

    #[test]
    fn date_like_column_stays_text_in_every_book() {
        let cols: [&[&str]; 8] = [
            &["15.03", "01.10", "1.04"],
            &["15.03", "20.12"],
            &["15.03.2025", "1.10.25"],
            &["03/15", "12/31"],
            &["2025-03-15", "2025-10-01"],
            &["12:30", "08:15:00"],
            &["007", "012", "100"],
            &["01.10", "02.20"],
        ];
        for dec in [Some(units::Dec::Point), Some(units::Dec::Comma), None] {
            let _g = units::DecGuard::set(dec);
            for col in cols {
                assert!(matches!(guessed(col), ParamType::Text), "{col:?} under {dec:?}");
            }
        }
    }

    #[test]
    fn whole_column_votes_mixed_is_text() {
        let _g = units::DecGuard::set(Some(units::Dec::Point));
        assert!(matches!(guessed(&["1.5", "2.25", "3"]), ParamType::Number));
        assert!(matches!(guessed(&["1", "2", ""]), ParamType::Int), "blank cells do not vote");
        assert!(matches!(guessed(&["1.5", "15.03"]), ParamType::Number), "one plain number wins");
        assert!(matches!(guessed(&["1.5", "15.03.2025"]), ParamType::Text), "hard date: text");
        assert!(matches!(guessed(&["1.5", "abc"]), ParamType::Text));
        assert!(matches!(guessed(&["", " "]), ParamType::Text));
        let _c = units::DecGuard::set(Some(units::Dec::Comma));
        assert!(matches!(guessed(&["1,5", "2,25", "-0,75"]), ParamType::Number));
        assert!(matches!(guessed(&["1,5", "007"]), ParamType::Text));
    }

    #[test]
    fn typed_numeric_cells_guess_number() {
        let _g = units::DecGuard::set(Some(units::Dec::Point));
        let c = cand(&[Data::Float(15.03), Data::Int(3), Data::Float(1.1)], None);
        assert!(matches!(c.guess().0, ParamType::Number), "typed cells are never date-like");
        let c = cand(&[Data::Int(1), Data::Int(7)], None);
        assert!(matches!(c.guess().0, ParamType::Int));
        let c = cand(&[Data::Float(2.5), s("15.03")], None);
        assert!(matches!(c.guess().0, ParamType::Number), "a typed number outvotes day.month");
        let c = cand(&[Data::Float(2.5), s("12:30")], None);
        assert!(matches!(c.guess().0, ParamType::Text), "a hard-text cell keeps it text");
    }

    #[test]
    fn own_export_column_guesses_number_in_both_marks() {
        use super::super::import::ro_shown;
        let xs = [1.5, 1234.5, 1000.0, 1.125, -0.25, 3.0, 12.5];
        for dec in [units::Dec::Comma, units::Dec::Point] {
            let _g = units::DecGuard::set(Some(dec));
            let col: Vec<Data> = xs.iter().map(|x| s(&ro_shown(*x, "value"))).collect();
            assert!(matches!(cand(&col, None).guess().0, ParamType::Number), "{dec:?}");
        }
    }

    #[test]
    fn iso_time_fields_are_invariant() {
        assert!(iso_time("12:30:15.5").is_some());
        assert!(iso_time("12:30:15,5").is_none());
        assert!(iso_time("12:inf").is_none());
    }

    #[test]
    fn delete_mark_with_spaces_and_case_is_truthy() {
        use units::DelWord::{No, Yes};
        assert_eq!(units::delete_word(&s(" Delete ")), Yes);
        assert_eq!(units::delete_word(&s(" X")), Yes);
        assert_eq!(units::delete_word(&s(" no ")), No);
    }

    #[test]
    fn kept_read_errs_on_unreadable_book() {
        let p = garbage("unreadable");
        let e = Kept::read(&p, &[]).err().expect("must be refused");
        assert!(e.contains("close it in a spreadsheet app"), "{e}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn kept_read_empty_for_foreign_book() {
        let p = tmp("foreign");
        let mut wb = rust_xlsxwriter::Workbook::new();
        wb.add_worksheet().write_string(0, 0, "x").unwrap();
        wb.save(&p).unwrap();
        let k = Kept::read(&p, &[]).expect("foreign book is not an error");
        assert!(k.sheets.is_empty() && k.others.is_empty());
        let missing = tmp("missing");
        let _ = std::fs::remove_file(&missing);
        let k = Kept::read(&missing, &[]).expect("missing book is not an error");
        assert!(k.sheets.is_empty() && k.others.is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn copy_from_unreadable_book_refused_not_stripped() {
        let from = garbage("copy_from");
        let to = tmp("copy_to");
        let _ = std::fs::remove_file(&to);
        let m = ddc_model::Model::open(":memory:").unwrap();
        let r = super::super::export_xlsx_families_from(&m, &from, &to, super::super::Units::Metric, &[]);
        assert!(r.is_err());
        assert!(!to.exists(), "no stripped copy may be written");
        let _ = std::fs::remove_file(&from);
    }

    #[test]
    fn kept_cell_types_map_to_typed_values() {
        use calamine::{CellErrorType, ExcelDateTime, ExcelDateTimeType};
        let d = |x: f64, t| data_v(&Data::DateTime(ExcelDateTime::new(x, t, false)));
        assert!(matches!(d(45678.0, ExcelDateTimeType::DateTime), V::D(x, "yyyy-mm-dd") if x == 45678.0));
        assert!(matches!(d(45678.5, ExcelDateTimeType::DateTime), V::D(_, "yyyy-mm-dd hh:mm:ss")));
        assert!(matches!(d(0.25, ExcelDateTimeType::DateTime), V::D(_, "hh:mm:ss")));
        assert!(matches!(d(1.5, ExcelDateTimeType::TimeDelta), V::D(_, "[h]:mm:ss")));
        assert!(matches!(data_v(&Data::Bool(true)), V::B(true)));
        assert!(matches!(data_v(&Data::Error(CellErrorType::NA)), V::E(e) if e == "#N/A"));
        assert!(matches!(data_v(&Data::Error(CellErrorType::Div0)), V::E(e) if e == "#DIV/0!"));
        // not a constant a formula may hold: kept as its text
        assert!(matches!(data_v(&Data::Error(CellErrorType::GettingData)), V::S(_)));
        let iso = |t: &str| data_v(&Data::DateTimeIso(t.into()));
        assert!(matches!(iso("2025-01-21"), V::D(x, _) if x == 45678.0));
        assert!(matches!(iso("2025-01-21T12:00:00"), V::D(x, _) if x == 45678.5));
        assert!(matches!(iso("06:00:00"), V::D(x, _) if (x - 0.25).abs() < 1e-12));
        assert!(matches!(iso("not a date"), V::S(_)));
        assert_eq!(error_const("=#n/a"), Some("#N/A"));
        assert_eq!(error_const("NA()"), None);
    }

    #[test]
    fn kept_conflict_cell_keeps_type_and_value() {
        use super::super::baseline::bval_v;
        use calamine::{CellErrorType, ExcelDateTime, ExcelDateTimeType};
        let dt = |x: f64, t| Data::DateTime(ExcelDateTime::new(x, t, false));
        let cases = [
            (dt(45678.0, ExcelDateTimeType::DateTime), BVal::D(45678.0, "yyyy-mm-dd")),
            (dt(45678.5, ExcelDateTimeType::DateTime), BVal::D(45678.5, "yyyy-mm-dd hh:mm:ss")),
            (dt(1.5, ExcelDateTimeType::TimeDelta), BVal::D(1.5, "[h]:mm:ss")),
            (Data::Bool(true), BVal::B(true)),
            (Data::Bool(false), BVal::B(false)),
            (Data::Error(CellErrorType::NA), BVal::E("#N/A".into())),
            (Data::Float(2.5), BVal::N(2.5)),
            (Data::Int(3), BVal::N(3.0)),
            (s("abc"), BVal::S("abc".into())),
            (Data::Empty, BVal::Blank),
        ];
        for (cell, want) in cases {
            let got = kept_book(&cell);
            assert_eq!(got, want, "{cell:?}");
            // re-export writes the same typed value back
            let back = data_v(&cell);
            assert_eq!(format!("{:?}", bval_v(&got)), format!("{back:?}"), "{cell:?}");
        }
    }
}
