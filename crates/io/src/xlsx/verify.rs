// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Read-back check of a freshly written book: the dry-run of the new file against the model
//! it came from must find nothing to do. `mismatches` lists what the dry-run report holds
//! besides the tolerated items; the first entry is the cell the caller names.
//!
//! Tolerated: read-only and computed columns (ignored or refused with a read-only reason),
//! cells the export cut or left blank (`ExportNote`), rows carrying a kept conflict cell
//! (they read back as the conflict on purpose), the user's own columns (proposals, and
//! columns a family sheet does not know), warnings.
use std::cell::Cell;
use std::collections::HashSet;

use super::baseline::KeptCell;
use super::cellsafe::{ExportNote, CLIPPED_REASON};
use super::{Report, MISSING};

thread_local! {
    static PLAIN: Cell<bool> = const { Cell::new(false) };
}

/// The import on this thread reads books as if they had no baseline (see `without_baseline`).
pub(crate) fn plain_read() -> bool {
    PLAIN.with(Cell::get)
}

/// Runs `f` with the baseline of every book ignored: each cell is then compared with the
/// model directly, so a cell that left the model wrong in both the sheet and the baseline is
/// named (with the baseline it only counts in `Report::model_newer`).
pub fn without_baseline<R>(f: impl FnOnce() -> R) -> R {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            PLAIN.with(|p| p.set(self.0));
        }
    }
    let _reset = Reset(PLAIN.with(|p| p.replace(true)));
    f()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MismatchKind {
    /// A cell reads back with another value than the model holds.
    Value,
    /// A row of the model reads back as missing.
    MissingRow,
    /// A row reads back that the model does not have.
    ExtraRow,
    /// A cell or row the read refused (type, format, required value).
    Refused,
    /// A sheet the export writes is missing.
    Sheet,
    /// Cells hold another value than the model with no cell named (`count` of them).
    Newer,
}

/// One difference between the written book and the model; `expected` is the model side.
#[derive(Clone, Debug, PartialEq)]
pub struct Mismatch {
    pub kind: MismatchKind,
    pub sheet: String,
    /// 1-based; 0 = not tied to a row.
    pub row: u32,
    /// 1-based; 0 = the whole row.
    pub col: u32,
    pub column: String,
    pub id: Option<i64>,
    pub expected: String,
    pub read: String,
    /// `Newer`: how many cells; else 1.
    pub count: usize,
}

impl Mismatch {
    pub fn address(&self) -> String {
        super::cell_ref(&self.sheet, self.row, self.col)
    }
}

/// English message templates (`{}` slots); the same texts are the `en` locale entries, the
/// UI translates a refusal by matching them.
pub const VERIFY_VALUE: (&str, &str) = (
    "xl.msg.base_value",
    "read-back check failed, the old workbook is kept: {} ({}) expected '{}', read '{}'",
);
pub const VERIFY_MISSING: (&str, &str) =
    ("xl.msg.base_missing", "read-back check failed, the old workbook is kept: sheet {} has no row for id {}");
pub const VERIFY_EXTRA: (&str, &str) =
    ("xl.msg.base_extra", "read-back check failed, the old workbook is kept: row {} is not in the model");
pub const VERIFY_REFUSED: (&str, &str) =
    ("xl.msg.base_refused", "read-back check failed, the old workbook is kept: {} ({}) was refused: {}");
pub const VERIFY_SHEET: (&str, &str) = (
    "xl.msg.base_sheet",
    "read-back check failed, the old workbook is kept: sheet {} is missing, empty or unreadable",
);
pub const VERIFY_NEWER: (&str, &str) =
    ("xl.msg.base_newer", "read-back check failed, the old workbook is kept: {} cells do not hold the model value");
pub const VERIFY_UNREADABLE: (&str, &str) =
    ("xl.msg.base_unreadable", "read-back check failed, the old workbook is kept: the new workbook cannot be read ({})");

pub const VERIFY_TEMPLATES: [(&str, &str); 7] =
    [VERIFY_VALUE, VERIFY_MISSING, VERIFY_EXTRA, VERIFY_REFUSED, VERIFY_SHEET, VERIFY_NEWER, VERIFY_UNREADABLE];

/// Longest value a message shows, in characters.
const SHOWN: usize = 60;

/// Both values cut to `SHOWN` characters around the first character where they differ.
pub fn shown_pair(a: &str, b: &str) -> (String, String) {
    let same = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let start = same.saturating_sub(SHOWN / 3);
    let cut = |s: &str| {
        let n = s.chars().count();
        if n <= SHOWN {
            return s.to_string();
        }
        let from = start.min(n.saturating_sub(SHOWN));
        let body: String = s.chars().skip(from).take(SHOWN).collect();
        let head = if from > 0 { "..." } else { "" };
        let tail = if from + SHOWN < n { "..." } else { "" };
        format!("{head}{body}{tail}")
    };
    (cut(a), cut(b))
}

impl Mismatch {
    /// The refusal text (English template, see the `VERIFY_*` constants).
    pub fn message(&self) -> String {
        use super::fill_template as fill;
        let at = self.address();
        match self.kind {
            MismatchKind::Value => {
                let (e, r) = shown_pair(&self.expected, &self.read);
                fill(VERIFY_VALUE.1, &[&at, &self.column, &e, &r])
            }
            MismatchKind::MissingRow => {
                let id = self.id.map_or_else(|| self.expected.clone(), |i| i.to_string());
                fill(VERIFY_MISSING.1, &[&super::cell_ref(&self.sheet, 0, 0), &id])
            }
            MismatchKind::ExtraRow => fill(VERIFY_EXTRA.1, &[&super::cell_ref(&self.sheet, self.row, 0)]),
            MismatchKind::Refused => {
                let (r, _) = shown_pair(&self.read, "");
                fill(VERIFY_REFUSED.1, &[&at, &self.column, &r])
            }
            MismatchKind::Sheet => fill(VERIFY_SHEET.1, &[&super::cell_ref(&self.sheet, 0, 0)]),
            MismatchKind::Newer => fill(VERIFY_NEWER.1, &[&self.count.to_string()]),
        }
    }
}

/// Refusal text for a book the check could not read at all.
pub fn unreadable_message(e: &str) -> String {
    let (e, _) = shown_pair(e, "");
    super::fill_template(VERIFY_UNREADABLE.1, &[&e])
}

fn read_only_reason(r: &str) -> bool {
    r == CLIPPED_REASON || r.contains("read-only")
}

/// An ignored item that means the row is not the model's.
fn extra_row_reason(r: &str) -> bool {
    r.ends_with("no longer exists; row ignored")
        || r.starts_with("row is not in the model")
        || r.starts_with("row already created as")
}

/// What may differ on purpose.
#[derive(Default)]
pub struct Tolerated {
    /// (sheet, id) of rows with a kept conflict cell.
    rows: HashSet<(String, i64)>,
    /// (sheet, row, col) of cells the export cut or blanked.
    cells: HashSet<(String, u32, u32)>,
}

impl Tolerated {
    pub fn new(kept: &[KeptCell], notes: &[ExportNote]) -> Tolerated {
        Tolerated {
            rows: kept.iter().map(|k| (k.sheet.clone(), k.id)).collect(),
            cells: notes.iter().map(|n| (n.sheet.clone(), n.row, n.col)).collect(),
        }
    }

    fn row(&self, sheet: &str, id: Option<i64>) -> bool {
        id.is_some_and(|i| self.rows.contains(&(sheet.to_string(), i)))
    }

    fn cell(&self, sheet: &str, row: u32, col: u32) -> bool {
        self.cells.contains(&(sheet.to_string(), row, col))
    }

    pub fn has_cells(&self) -> bool {
        !self.cells.is_empty()
    }
}

/// Every difference the dry-run `rep` of a new book shows, sheets first, then cells in the
/// order the read met them. `model_newer` is not listed here (see `newer`).
pub fn mismatches(rep: &Report, tol: &Tolerated) -> Vec<Mismatch> {
    let mut out = Vec::new();
    let mk = |kind, sheet: &str, row, col, column: &str, id, expected: &str, read: &str| Mismatch {
        kind,
        sheet: sheet.to_string(),
        row,
        col,
        column: column.to_string(),
        id,
        expected: expected.to_string(),
        read: read.to_string(),
        count: 1,
    };
    for s in &rep.sheet_issues {
        out.push(mk(MismatchKind::Sheet, &s.sheet, 0, 0, "", None, "", ""));
    }
    let mut cells = Vec::new();
    for r in &rep.rejected {
        if read_only_reason(&r.reason) || tol.cell(&r.sheet, r.row, r.col) {
            continue;
        }
        cells.push(mk(MismatchKind::Refused, &r.sheet, r.row, r.col, &r.column, None, "", &r.reason));
    }
    for c in &rep.changes {
        if tol.row(&c.sheet, c.id) || tol.cell(&c.sheet, c.row, c.col) {
            continue;
        }
        let kind = match c.kind() {
            "added" => MismatchKind::ExtraRow,
            "deleted" => MismatchKind::MissingRow,
            _ => MismatchKind::Value,
        };
        let read = if c.new == MISSING { "" } else { c.new.as_str() };
        cells.push(mk(kind, &c.sheet, c.row, c.col, &c.column, c.id, &c.old, read));
    }
    for c in &rep.conflicts {
        if tol.row(&c.sheet, c.id) || tol.cell(&c.sheet, c.row, c.col) {
            continue;
        }
        cells.push(mk(MismatchKind::Value, &c.sheet, c.row, c.col, &c.column, c.id, &c.model, &c.book));
    }
    for m in &rep.matches {
        cells.push(mk(MismatchKind::ExtraRow, &m.sheet, m.row, 0, "id", Some(m.id), "", &m.key));
    }
    for i in &rep.ignored {
        if extra_row_reason(&i.reason) && !tol.row(&i.sheet, i.id) {
            cells.push(mk(MismatchKind::ExtraRow, &i.sheet, i.row, 0, "id", i.id, "", &i.reason));
        }
    }
    // the first cell of the book: sheets in the order the read met them, then row and column
    let order: Vec<String> = cells.iter().fold(Vec::new(), |mut v, m| {
        if !v.contains(&m.sheet) {
            v.push(m.sheet.clone());
        }
        v
    });
    let at = |m: &Mismatch| order.iter().position(|s| *s == m.sheet).unwrap_or(usize::MAX);
    cells.sort_by_key(|m| (at(m), m.row == 0, m.row, m.col));
    out.extend(cells);
    for r in &rep.removed {
        if !tol.row(&r.sheet, Some(r.id)) {
            out.push(mk(MismatchKind::MissingRow, &r.sheet, 0, 0, "id", Some(r.id), &r.id.to_string(), ""));
        }
    }
    out
}

/// `model_newer` cells as one item: cells that kept a value the model no longer has; the
/// caller names them with a read `without_baseline`.
pub fn newer(rep: &Report) -> Option<Mismatch> {
    (rep.model_newer > 0).then(|| Mismatch {
        kind: MismatchKind::Newer,
        sheet: String::new(),
        row: 0,
        col: 0,
        column: String::new(),
        id: None,
        expected: String::new(),
        read: String::new(),
        count: rep.model_newer,
    })
}
