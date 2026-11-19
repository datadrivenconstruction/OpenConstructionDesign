//! Limits of a sheet cell. Text over the cell limit is written cut, in a locked grey cell, with a
//! marker at the end; a number that is NaN or infinite is written as a blank locked cell. The
//! model keeps the full value. A cut cell read back is never applied (see `cut_cell`).

use std::cell::RefCell;

use rust_xlsxwriter::{Color, Format, Worksheet, XlsxError};

use super::export::V;

/// Longest text a cell holds, in UTF-16 units.
pub(crate) const MAX_CELL_UNITS: usize = 32767;
/// Ends every text cut to fit a cell.
pub(crate) const TRUNC_MARK: &str = " [...]";

/// `Ignored::reason` for an edit of a cell that holds cut text (English template, key
/// `xl.msg.cell_clipped` in the locale files).
pub const CLIPPED_REASON: &str = "text was cut to fit the cell limit; read-only, edit not applied";

fn units16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The form of `s` a cell can hold: `None` when it fits as is.
pub(crate) fn clipped(s: &str) -> Option<String> {
    if units16(s) <= MAX_CELL_UNITS {
        return None;
    }
    let budget = MAX_CELL_UNITS - TRUNC_MARK.len();
    let mut out = String::new();
    let mut n = 0;
    for ch in s.chars() {
        n += ch.len_utf16();
        if n > budget {
            break;
        }
        out.push(ch);
    }
    out.push_str(TRUNC_MARK);
    Some(out)
}

/// True for text that looks like the output of `clipped`, or fills the whole cell (a program
/// that cut the text itself): never a value to apply.
pub(crate) fn looks_clipped(s: &str) -> bool {
    let t = s.trim_end();
    units16(s) >= MAX_CELL_UNITS || (t.ends_with(TRUNC_MARK) && units16(t) + 1 >= MAX_CELL_UNITS)
}

/// Read side. `Some(true)`: `cell` is the cut form of the model text `cur` (unchanged, skip);
/// `Some(false)`: a cut cell that differs (refuse the edit, report); `None`: ordinary text.
pub(crate) fn cut_cell(cell: &str, cur: Option<&str>) -> Option<bool> {
    let form = cur.map(|c| clipped(c).unwrap_or_else(|| c.to_string()));
    let cut = cur.is_some_and(|c| clipped(c).is_some()) || looks_clipped(cell);
    if !cut {
        return None;
    }
    Some(form.is_some_and(|f| f.trim() == cell.trim()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportNoteKind {
    /// Text over the cell limit: written cut, in a locked grey cell, read-only on import.
    Clipped,
    /// NaN or infinity: written as a blank locked grey cell.
    NonFinite,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportNote {
    pub sheet: String,
    /// Cell, 1-based.
    pub row: u32,
    pub col: u32,
    pub id: Option<i64>,
    pub kind: ExportNoteKind,
}

impl ExportNote {
    pub fn address(&self) -> String {
        super::cell_ref(&self.sheet, self.row, self.col)
    }
}

#[derive(Default)]
struct Seen {
    sheet: String,
    notes: Vec<ExportNote>,
}

thread_local! {
    static SEEN: RefCell<Seen> = RefCell::new(Seen::default());
}

/// Starts an export: forgets the notes of the previous one.
pub(crate) fn begin() {
    SEEN.with(|s| *s.borrow_mut() = Seen::default());
}

/// Names the sheet the next cells belong to.
pub(crate) fn enter(sheet: &str) {
    SEEN.with(|s| s.borrow_mut().sheet = sheet.to_string());
}

/// Takes the notes of the last export on this thread: cells cut or left blank.
pub fn take_export_notes() -> Vec<ExportNote> {
    SEEN.with(|s| std::mem::take(&mut s.borrow_mut().notes))
}

/// The notes of the last export on this thread, left in place for `take_export_notes`.
pub fn export_notes() -> Vec<ExportNote> {
    SEEN.with(|s| s.borrow().notes.clone())
}

fn note(kind: ExportNoteKind, r: u32, c: u16, id: Option<i64>) {
    SEEN.with(|s| {
        let mut s = s.borrow_mut();
        let sheet = s.sheet.clone();
        s.notes.push(ExportNote { sheet, row: r + 1, col: u32::from(c) + 1, id, kind });
    });
}

fn grey() -> Format {
    Format::new()
        .set_font_color(Color::RGB(0x666666))
        .set_background_color(Color::RGB(0xEEEEEE))
}

/// Writes `v` when it does not fit a cell as is (cut text, non-finite number) and says so;
/// `None` when the caller writes it the usual way.
pub(crate) fn write_safe(
    ws: &mut Worksheet,
    r: u32,
    c: u16,
    v: &V,
    id: Option<i64>,
) -> Option<Result<(), XlsxError>> {
    match v {
        V::N(x) if !x.is_finite() => {
            note(ExportNoteKind::NonFinite, r, c, id);
            Some(ws.write_blank(r, c, &grey()).map(|_| ()))
        }
        V::S(s) => {
            let t = clipped(s)?;
            note(ExportNoteKind::Clipped, r, c, id);
            Some(ws.write_string_with_format(r, c, &t, &grey()).map(|_| ()))
        }
        _ => None,
    }
}
