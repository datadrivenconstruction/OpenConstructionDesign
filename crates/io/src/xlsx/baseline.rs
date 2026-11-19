//! Export baseline: a hidden sheet with every exported value, in the model's units. Import
//! compares each cell with it to tell a book edit from a model edit made after the export.
//!
//! Layout: a block per sheet. Header row `# | sheet | key | key ...`, then one row per element:
//! `id | (empty) | value | value ...` aligned with the keys.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};

use calamine::{Data, Reader, Xlsx};
use rust_xlsxwriter::Workbook;

use super::export::V;
use super::extra::XCol;
use super::{Col, BASELINE};

/// Keys of a block that only lists ids (written after the export, see `id_write`).
pub(crate) const ID_ONLY: &str = "id";

/// Key of a user column (parameter) in the baseline: its header, apart from the format keys.
pub(crate) fn extra_key(header: &str) -> String {
    format!("x:{}", header.trim())
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BVal {
    N(f64),
    S(String),
    Blank,
    /// A user formula at the export: not compared, the book value counts as an edit.
    Any,
    /// Kept date, time or duration cell: serial and number format.
    D(f64, &'static str),
    /// Kept boolean cell.
    B(bool),
    /// Kept error constant ("#N/A", ...).
    E(String),
}

impl BVal {
    pub(crate) fn same(&self, o: &BVal) -> bool {
        match (self, o) {
            (BVal::N(a), BVal::N(b)) => (a - b).abs() <= 1e-9 * (1.0 + a.abs().max(b.abs())),
            (BVal::S(a), BVal::S(b)) => a.trim() == b.trim(),
            (BVal::Blank, BVal::Blank) | (BVal::Any, BVal::Any) => true,
            (BVal::D(a, _), BVal::D(b, _)) => a == b,
            (BVal::B(a), BVal::B(b)) => a == b,
            (BVal::E(a), BVal::E(b)) => a == b,
            _ => false,
        }
    }

    pub(crate) fn text(&self) -> String {
        match self {
            BVal::N(x) => x.to_string(),
            BVal::S(s) | BVal::E(s) => s.clone(),
            BVal::D(x, _) => x.to_string(),
            BVal::B(v) => v.to_string(),
            BVal::Blank | BVal::Any => String::new(),
        }
    }
}

struct Block {
    sheet: String,
    keys: Vec<String>,
    rows: Vec<(i64, Vec<V>)>,
}

thread_local! {
    static CAPTURE: RefCell<Vec<Block>> = const { RefCell::new(Vec::new()) };
}

/// Starts collecting the sheets of one export.
pub(crate) fn begin() {
    CAPTURE.with(|c| c.borrow_mut().clear());
}

/// Notes a sheet whose first column is the element id; other sheets are not compared.
pub(crate) fn record(sheet: &str, cols: &[Col], rows: &[Vec<V>], extra: &[XCol]) {
    if cols.first().map(|c| c.key) != Some("id") {
        return;
    }
    let rows = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r.first() {
            Some(V::N(x)) => {
                let mut vals: Vec<V> = r.iter().take(cols.len()).cloned().collect();
                vals.resize(cols.len(), V::Blank);
                vals.extend(extra.iter().map(|x| x.cells.get(i).cloned().unwrap_or(V::Blank)));
                Some((*x as i64, vals))
            }
            _ => None,
        })
        .collect();
    let mut keys: Vec<String> = cols.iter().map(|c| c.key.to_string()).collect();
    keys.extend(extra.iter().map(|x| extra_key(&x.header)));
    let block = Block {
        sheet: sheet.to_string(),
        keys,
        rows,
    };
    CAPTURE.with(|c| c.borrow_mut().push(block));
}

/// Notes a sheet whose rows are keyed by `id` and whose values are in the model's units.
pub(crate) fn record_raw(sheet: &str, keys: Vec<String>, rows: Vec<(i64, Vec<V>)>) {
    let mut ks = vec!["id".to_string()];
    ks.extend(keys);
    let rows = rows
        .into_iter()
        .map(|(id, mut v)| {
            v.insert(0, V::N(id as f64));
            (id, v)
        })
        .collect();
    let block = Block {
        sheet: sheet.to_string(),
        keys: ks,
        rows,
    };
    CAPTURE.with(|c| c.borrow_mut().push(block));
}

/// Cells whose book value the user chose over the model's in a conflict.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Take {
    /// Every conflict of the book.
    pub all: bool,
    /// (sheet, row, column), 1-based as in `Conflict`.
    pub cells: HashSet<(String, u32, u32)>,
    /// (sheet, row fingerprint) of id-less rows an earlier Apply already created -> (row the
    /// row had then, element id (0: unknown)). The row number is only a hint; an empty
    /// fingerprint is a record from before fingerprints, matched by row number alone.
    pub made: Made,
    /// Records a row of this import already matched (an equal row matches the next record).
    pub claimed: HashSet<(String, String, u32)>,
    /// Cells Apply took from this book since its baseline was written ([`Applied`]).
    pub applied: Applied,
}

/// A book cell an Apply took into the model: (canonical sheet, element, baseline key) and the
/// value in the form the baseline holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct AppliedCell {
    pub(crate) sheet: String,
    pub(crate) id: i64,
    pub(crate) key: String,
    pub(crate) val: BVal,
}

impl AppliedCell {
    pub(crate) fn new(sheet: &str, id: Option<i64>, key: &str, val: BVal) -> Option<AppliedCell> {
        Some(AppliedCell { sheet: sheet.into(), id: id?, key: key.into(), val })
    }

    /// A cell of element `id` on `sheet`.
    pub fn is_of(&self, sheet: &str, id: i64) -> bool {
        self.sheet == sheet && self.id == id
    }
}

/// Values applied from a book whose baseline was not rewritten after the Apply (ids written in
/// place, a skipped, failed or cancelled re-export), newest last per cell. The next read
/// compares with the newest one the model still holds, so a value put back in the book is an
/// edit and not "model newer"; an undo or a later model edit drops it by itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied(HashMap<(String, i64, String), Vec<BVal>>);

/// Values kept per cell: older ones only matter after several undos.
const APPLIED_DEPTH: usize = 16;

impl Applied {
    pub fn record(&mut self, cells: &[AppliedCell]) {
        for c in cells {
            let h = self.0.entry((c.sheet.clone(), c.id, c.key.clone())).or_default();
            h.retain(|v| !v.same(&c.val));
            h.push(c.val.clone());
            if h.len() > APPLIED_DEPTH {
                h.remove(0);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Cells remembered.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// Base of the three-way compare of one cell: the newest value an Apply took from this book
/// that the model still holds (`live` tests it against the model value), else `file`.
pub(crate) fn base_now(
    file: Option<&BVal>,
    sheet: &str,
    id: Option<i64>,
    key: &str,
    live: impl Fn(&BVal) -> bool,
) -> Option<BVal> {
    let hit = id.and_then(|id| {
        TAKE.with(|t| {
            let t = t.borrow();
            let h = t.applied.0.get(&(sheet.to_string(), id, key.to_string()))?;
            h.iter().rev().find(|b| live(b)).cloned()
        })
    });
    hit.or_else(|| file.cloned())
}

/// Created-row records of one book.
pub type Made = std::collections::HashMap<(String, String), Vec<(u32, i64)>>;

/// Stable hash of a row's identifying cells.
pub fn fingerprint(parts: &[(&str, String)]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for (k, v) in parts {
        for b in k.bytes().chain([b'=']).chain(v.bytes()).chain([0x1f]) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    format!("{h:016x}")
}

/// Fingerprint of a level row (and of a level of the model): the name alone.
pub fn level_fp(name: &str) -> String {
    fingerprint(&[("name", name.trim().to_lowercase())])
}

/// Text of an identifying cell, the conversion the import uses for a text cell: a number
/// reads as its plain text ("3", not "3.0000"), so it matches the model's name text.
pub(crate) fn cell_text(d: &calamine::Data) -> String {
    use calamine::Data;
    match d {
        Data::String(s) => s.trim().to_lowercase(),
        other => other.to_string().trim().to_lowercase(),
    }
}

impl Take {
    pub fn has(&self, sheet: &str, row: u32, col: u32) -> bool {
        self.all || self.cells.contains(&(sheet.to_string(), row, col))
    }

    /// Chooses one cell; `all_cells` lists every conflict, so choosing out of "all" keeps the rest.
    pub fn set(&mut self, key: (String, u32, u32), take: bool, all_cells: &[(String, u32, u32)]) {
        if self.all {
            self.all = false;
            self.cells = all_cells.iter().cloned().collect();
        }
        if take {
            self.cells.insert(key);
        } else {
            self.cells.remove(&key);
        }
    }

    pub fn set_all(&mut self, take: bool) {
        self.all = take;
        self.cells.clear();
    }
}

/// A conflicting cell the user left on the model value: a re-export keeps the book value in
/// the cell and the old export value in the baseline, so the next read finds the conflict again.
#[derive(Clone, Debug, PartialEq)]
pub struct KeptCell {
    pub(crate) sheet: String,
    pub(crate) id: i64,
    pub(crate) key: String,
    /// The book's value, in the units the baseline uses.
    pub(crate) book: BVal,
    pub(crate) base: BVal,
}

impl KeptCell {
    pub(crate) fn new(sheet: &str, id: Option<i64>, key: &str, book: BVal, base: BVal) -> Option<KeptCell> {
        Some(KeptCell {
            sheet: sheet.into(),
            id: id?,
            key: key.into(),
            book,
            base,
        })
    }
}

type KeepMap = HashMap<(String, i64, String), (BVal, BVal)>;

thread_local! {
    static KEEP: RefCell<KeepMap> = RefCell::new(HashMap::new());
}

/// Runs `f` (an export) so that the `cells` keep their book values and old baseline.
pub fn with_kept<T>(cells: &[KeptCell], f: impl FnOnce() -> T) -> T {
    let map: KeepMap = cells
        .iter()
        .map(|c| ((c.sheet.clone(), c.id, c.key.clone()), (c.book.clone(), c.base.clone())))
        .collect();
    struct Restore(Option<KeepMap>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(prev) = self.0.take() {
                KEEP.with(|k| *k.borrow_mut() = prev);
            }
        }
    }
    let _guard = Restore(Some(KEEP.with(|k| std::mem::replace(&mut *k.borrow_mut(), map))));
    f()
}

pub(crate) fn kept_on(sheet: &str) -> bool {
    KEEP.with(|k| k.borrow().keys().any(|x| x.0 == sheet))
}

/// (book value, baseline value) of a kept cell.
pub(crate) fn kept_cell(sheet: &str, id: i64, key: &str) -> Option<(BVal, BVal)> {
    KEEP.with(|k| k.borrow().get(&(sheet.to_string(), id, key.to_string())).cloned())
}

pub(crate) fn bval_v(b: &BVal) -> V {
    match b {
        BVal::N(x) => V::N(*x),
        BVal::S(s) => V::S(s.clone()),
        BVal::D(x, f) => V::D(*x, f),
        BVal::B(v) => V::B(*v),
        BVal::E(e) => V::E(e.clone()),
        BVal::Blank | BVal::Any => V::Blank,
    }
}

/// Typed book value of a kept cell, the inverse of `bval_v`.
pub(crate) fn v_bval(v: &V) -> BVal {
    match v {
        V::N(x) => BVal::N(*x),
        V::S(s) => BVal::S(s.clone()),
        V::D(x, f) => BVal::D(*x, f),
        V::B(b) => BVal::B(*b),
        V::E(e) => BVal::E(e.clone()),
        V::F(..) | V::Blank => BVal::Blank,
    }
}

/// Puts the kept cells' book values (`book`) or baseline values into the rows of a sheet.
pub(crate) fn overlay(sheet: &str, cols: &[Col], rows: &mut [Vec<V>], extra: &mut [XCol], book: bool) {
    let pick = |c: (BVal, BVal)| bval_v(if book { &c.0 } else { &c.1 });
    for row in rows.iter_mut() {
        let Some(V::N(id)) = row.first() else { continue };
        let id = *id as i64;
        for (j, c) in cols.iter().enumerate() {
            if let (Some(k), true) = (kept_cell(sheet, id, c.key), j < row.len()) {
                row[j] = pick(k);
            }
        }
    }
    for x in extra.iter_mut() {
        let key = extra_key(&x.header);
        for (i, cell) in x.cells.iter_mut().enumerate() {
            let Some(V::N(id)) = rows.get(i).and_then(|r| r.first()) else { continue };
            if let Some(k) = kept_cell(sheet, *id as i64, &key) {
                *cell = pick(k);
            }
        }
    }
}

thread_local! {
    static TAKE: RefCell<Take> = RefCell::new(Take::default());
}

/// Runs `f` with `take` as the user's choice for conflicts.
pub(crate) fn with_take<T>(take: &Take, f: impl FnOnce() -> T) -> T {
    let mut fresh = take.clone();
    fresh.claimed.clear();
    let prev = TAKE.with(|t| std::mem::replace(&mut *t.borrow_mut(), fresh));
    let r = f();
    TAKE.with(|t| *t.borrow_mut() = prev);
    r
}

/// Element an earlier Apply created for this id-less book row, if any. Tries the current
/// fingerprint, then `legacy` (records of Apply runs that hashed numbers as "3.0000"), then
/// the pre-fingerprint record; the hit is claimed under the key it was found by.
pub(crate) fn made(sheet: &str, row: u32, fp: &str, legacy: &str) -> Option<i64> {
    TAKE.with(|t| {
        let mut t = t.borrow_mut();
        let free = |t: &Take, f: &str, h: u32| !t.claimed.contains(&(sheet.to_string(), f.to_string(), h));
        let mut found = None;
        for (i, f) in [fp, legacy, ""].into_iter().enumerate() {
            if i == 1 && (f == fp || f.is_empty()) {
                continue;
            }
            let list = t.made.get(&(sheet.to_string(), f.to_string())).cloned().unwrap_or_default();
            let hit = if f.is_empty() {
                // before fingerprints only the row number identified a record
                list.iter().find(|h| h.0 == row && free(&*t, f, h.0))
            } else {
                list.iter().find(|h| h.0 == row && free(&*t, f, h.0)).or_else(|| list.iter().find(|h| free(&*t, f, h.0)))
            };
            if let Some(h) = hit {
                found = Some((f.to_string(), *h));
                break;
            }
        }
        let (f, h) = found?;
        t.claimed.insert((sheet.to_string(), f, h.0));
        Some(h.1)
    })
}

pub(crate) fn taken(sheet: &str, row: u32, col: u32) -> bool {
    TAKE.with(|t| t.borrow().has(sheet, row, col))
}

/// Writes the collected sheets as the hidden baseline sheet.
pub(crate) fn write(wb: &mut Workbook) -> Result<(), String> {
    let blocks = CAPTURE.with(|c| std::mem::take(&mut *c.borrow_mut()));
    let err = |e: rust_xlsxwriter::XlsxError| e.to_string();
    let ws = wb.add_worksheet();
    ws.set_name(BASELINE).map_err(err)?;
    let mut r = 0u32;
    for b in &blocks {
        ws.write_string(r, 0, "#").map_err(err)?;
        ws.write_string(r, 1, &b.sheet).map_err(err)?;
        for (j, k) in b.keys.iter().enumerate() {
            ws.write_string(r, 2 + j as u16, k).map_err(err)?;
        }
        r += 1;
        for (id, row) in &b.rows {
            ws.write_number(r, 0, *id as f64).map_err(err)?;
            for (j, v) in row.iter().enumerate().skip(1) {
                let c = 2 + j as u16;
                match v {
                    // as the cell holds it: a non-finite number is blank, cut text is its cut form
                    V::N(x) if !x.is_finite() => Ok(()),
                    V::N(x) => ws.write_number(r, c, *x).map(|_| ()),
                    V::S(s) => match super::cellsafe::clipped(s) {
                        Some(t) => ws.write_string(r, c, &t).map(|_| ()),
                        None => ws.write_string(r, c, s).map(|_| ()),
                    },
                    // a formula is marked, not compared: its result may change on any recalc
                    V::F(..) => ws.write_string(r, c, "=").map(|_| ()),
                    // typed kept cells compare as before: serial, 0/1, error text
                    V::D(x, _) => ws.write_number(r, c, *x).map(|_| ()),
                    V::B(b) => ws.write_number(r, c, f64::from(u8::from(*b))).map(|_| ()),
                    V::E(e) => ws.write_string(r, c, e).map(|_| ()),
                    V::Blank => Ok(()),
                }
                .map_err(err)?;
            }
            r += 1;
        }
    }
    ws.set_hidden(true);
    Ok(())
}

#[derive(Default)]
struct SheetBase {
    keys: Vec<String>,
    rows: HashMap<i64, Vec<BVal>>,
}

/// The baseline of a book.
#[derive(Default)]
pub(crate) struct Base {
    sheets: HashMap<String, SheetBase>,
}

impl Base {
    /// Sheets the export wrote with element rows.
    pub(crate) fn sheet_names(&self) -> impl Iterator<Item = &str> {
        self.sheets.keys().map(String::as_str)
    }

    /// Ids the export wrote on a sheet.
    pub(crate) fn ids(&self, sheet: &str) -> Vec<i64> {
        let mut v: Vec<i64> = self
            .sheets
            .get(sheet)
            .map(|s| s.rows.keys().copied().collect())
            .unwrap_or_default();
        v.sort_unstable();
        v
    }

    pub(crate) fn has_sheet(&self, sheet: &str) -> bool {
        self.sheets.contains_key(sheet)
    }

    /// The element was in the export.
    pub(crate) fn has_id(&self, sheet: &str, id: i64) -> bool {
        self.sheets.get(sheet).is_some_and(|s| s.rows.contains_key(&id))
    }

    pub(crate) fn get(&self, sheet: &str, id: i64, key: &str) -> Option<&BVal> {
        let s = self.sheets.get(sheet)?;
        let i = s.keys.iter().position(|k| k == key)?;
        s.rows.get(&id)?.get(i)
    }
}

/// None: the book has no baseline (made by another tool or an older version), or only a damaged one.
pub(crate) fn read<RS: Read + Seek>(wb: &mut Xlsx<RS>) -> Option<Base> {
    if super::verify::plain_read() || !wb.sheet_names().iter().any(|n| n == BASELINE) {
        return None;
    }
    let range = wb.worksheet_range(BASELINE).ok()?;
    if !super::no_base::intact(&range) {
        return None;
    }
    let mut base = Base::default();
    // (sheet, keys of the block being read)
    let mut cur: Option<(String, Vec<String>)> = None;
    for row in range.rows() {
        match row.first() {
            Some(Data::String(h)) if h == "#" => {
                let name = row.get(1).map(|d| d.to_string()).unwrap_or_default();
                let keys: Vec<String> = row
                    .iter()
                    .skip(2)
                    .map(|d| d.to_string())
                    .take_while(|k| !k.is_empty())
                    .collect();
                // an id-only block of a sheet already read: ids written into the book in place
                // after the export (`id_write`); its rows join the sheet, their values unknown
                let joins = keys == [ID_ONLY] && base.sheets.contains_key(&name);
                if !joins {
                    base.sheets.insert(
                        name.clone(),
                        SheetBase {
                            keys: keys.clone(),
                            rows: HashMap::new(),
                        },
                    );
                }
                cur = Some((name, keys));
            }
            Some(Data::Float(_) | Data::Int(_)) => {
                let Some((name, keys)) = cur.as_ref() else {
                    continue;
                };
                let Some(sb) = base.sheets.get_mut(name) else {
                    continue;
                };
                let id = match &row[0] {
                    Data::Float(f) => *f as i64,
                    Data::Int(i) => *i,
                    _ => continue,
                };
                let val = |j: usize| match row.get(2 + j) {
                    Some(Data::Float(f)) => BVal::N(*f),
                    Some(Data::Int(i)) => BVal::N(*i as f64),
                    Some(Data::String(s)) if s.starts_with('=') => BVal::Any,
                    Some(Data::String(s)) => BVal::S(s.clone()),
                    _ => BVal::Blank,
                };
                let vals = if *keys == sb.keys {
                    (0..sb.keys.len()).map(&val).collect()
                } else {
                    // not compared: the book value counts, as for a row without a baseline
                    sb.keys.iter().map(|k| keys.iter().position(|x| x == k).map_or(BVal::Any, &val)).collect()
                };
                sb.rows.insert(id, vals);
            }
            _ => {}
        }
    }
    Some(base)
}

#[cfg(test)]
mod applied_tests {
    use super::*;

    fn cell(v: f64) -> AppliedCell {
        AppliedCell::new("Walls", Some(7), "h", BVal::N(v)).unwrap()
    }

    fn base(t: &Take, model: f64) -> Option<BVal> {
        let file = BVal::N(3.0);
        let live = |b: &BVal| matches!(b, BVal::N(x) if (x - model).abs() < 1e-9);
        with_take(t, || base_now(Some(&file), "Walls", Some(7), "h", live))
    }

    #[test]
    fn newest_value_the_model_holds_is_the_base() {
        let mut t = Take::default();
        t.applied.record(&[cell(4.0)]);
        t.applied.record(&[cell(5.0)]);
        t.applied.record(&[cell(4.0)]);
        assert_eq!(t.applied.len(), 1);
        assert_eq!(base(&t, 4.0), Some(BVal::N(4.0)));
        assert_eq!(base(&t, 5.0), Some(BVal::N(5.0)), "an undo back to an older Apply");
        // undone past every Apply, or edited in the model: the export's value
        assert_eq!(base(&t, 3.5), Some(BVal::N(3.0)));
        assert_eq!(base(&Take::default(), 4.0), Some(BVal::N(3.0)));
        // outside the read nothing is installed
        assert_eq!(base_now(None, "Walls", Some(7), "h", |_| true), None);
    }
}
