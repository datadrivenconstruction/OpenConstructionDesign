// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The requirements workbook: a template to fill in, reading it back with the sheet, row and
//! column of every refused cell, and the check report as a workbook (summary plus one sheet per
//! category). Words of the book (headers, categories, rule kinds, yes/no) come from the caller,
//! so a book made in one language reads in the other.

use std::path::Path;

use ddc_model::checks::{parse_requirement, ReqField, ReqText, Requirement};
use rust_xlsxwriter::utility::column_number_to_name;
use rust_xlsxwriter::{Color, DataValidation, Format, Workbook, Worksheet, XlsxError};

use crate::xlsx::{read_first_of, sheet_names, SheetCell};

/// Rows past this are refused: a requirements book is a list a person reads, not a dump.
pub const MAX_ROWS: usize = 5000;
/// The header is looked for in the first rows only; a title may stand above it.
const HEADER_SCAN: usize = 10;
/// Rows of the template's data validation below the header.
const TEMPLATE_ROWS: u32 = 500;

/// Localized words of a book.
#[derive(Clone, Debug, Default)]
pub struct Vocab {
    /// Header captions per column; the machine names (`category`, ...) always match too.
    pub headers: Vec<(ReqField, String)>,
    /// Cell words to rule words: categories (`door`), rule kinds (`one_of`), `yes` and `no`.
    pub words: Vec<(String, String)>,
    /// The requirements sheet's names: read first when the book has one.
    pub sheets: Vec<String>,
    /// Sheets never read as requirements (the example, the instructions).
    pub skip: Vec<String>,
}

/// A refused cell: where it is and why (`code` is a `req.err.*` message key).
#[derive(Clone, Debug, PartialEq)]
pub struct RowError {
    pub sheet: String,
    /// 1-based, as the spreadsheet app shows it.
    pub row: usize,
    /// Column letter; empty when the book has no such column.
    pub col: String,
    pub field: ReqField,
    pub code: &'static str,
    pub arg: String,
}

/// A row read as a requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct ReqRow {
    pub sheet: String,
    pub row: usize,
    pub req: Requirement,
}

impl ReqRow {
    /// `Sheet!4`: where the requirement came from, kept with the stored rule.
    pub fn origin(&self) -> String {
        format!("{}!{}", self.sheet, self.row)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadBook {
    pub sheet: String,
    pub rows: Vec<ReqRow>,
    pub errors: Vec<RowError>,
    /// Column letter of each field found in the header.
    pub cols: Vec<(ReqField, String)>,
}

impl ReadBook {
    /// A refusal found after reading (a level the model lacks) at its row and column.
    pub fn error_at(
        &self,
        row: &ReqRow,
        field: ReqField,
        code: &'static str,
        arg: &str,
    ) -> RowError {
        let col = self.cols.iter().find(|c| c.0 == field);
        RowError {
            sheet: row.sheet.clone(),
            row: row.row,
            col: col.map(|c| c.1.clone()).unwrap_or_default(),
            field,
            code,
            arg: arg.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BookError {
    /// No sheet with a category and a parameter column.
    NoSheet,
    Read(String),
}

fn norm(s: &str) -> String {
    let t = s.trim().trim_end_matches('*').trim();
    let t = match t.find('(') {
        Some(i) if i > 0 => &t[..i],
        _ => t,
    };
    t.trim().to_lowercase()
}

fn field_of(head: &str, v: &Vocab) -> Option<ReqField> {
    let h = norm(head);
    if h.is_empty() {
        return None;
    }
    ReqField::ALL
        .into_iter()
        .find(|f| f.key() == h)
        .or_else(|| v.headers.iter().find(|(_, c)| norm(c) == h).map(|(f, _)| *f))
}

/// Cell text as a person typed it: whole numbers without `.0`, a ticked box as yes or no.
fn cell_text(c: &SheetCell) -> Result<String, (&'static str, String)> {
    match c {
        SheetCell::Empty => Ok(String::new()),
        SheetCell::Text(s) => Ok(s.trim().to_string()),
        SheetCell::Num(x) if x.fract() == 0.0 && x.abs() < 1e15 => Ok(format!("{}", *x as i64)),
        SheetCell::Num(x) => Ok(x.to_string()),
        SheetCell::Bool(b) => Ok(if *b { "yes" } else { "no" }.to_string()),
        SheetCell::Error(e) => Err(("req.err.cell", e.clone())),
        SheetCell::Uncalc(f) => Err(("req.err.formula", f.clone())),
    }
}

fn word(v: &Vocab, s: &str) -> String {
    let t = s.trim().to_lowercase();
    v.words
        .iter()
        .find(|(w, _)| w.trim().to_lowercase() == t)
        .map_or_else(|| s.to_string(), |(_, c)| c.clone())
}

fn letter(col: usize) -> String {
    column_number_to_name(u16::try_from(col).unwrap_or(u16::MAX))
}

/// Header row index and the column of each field.
fn header(grid: &[Vec<SheetCell>], v: &Vocab) -> Option<(usize, Vec<(ReqField, usize)>)> {
    for (r, row) in grid.iter().enumerate().take(HEADER_SCAN) {
        let mut cols: Vec<(ReqField, usize)> = vec![];
        for (c, cell) in row.iter().enumerate() {
            if let SheetCell::Text(t) = cell {
                if let Some(f) = field_of(t, v) {
                    if !cols.iter().any(|x| x.0 == f) {
                        cols.push((f, c));
                    }
                }
            }
        }
        let has = |f: ReqField| cols.iter().any(|x| x.0 == f);
        if has(ReqField::Category) && has(ReqField::Parameter) {
            return Some((r, cols));
        }
    }
    None
}

fn pick_sheet(path: &Path, v: &Vocab, want: Option<&str>) -> Result<Vec<String>, BookError> {
    let names = sheet_names(path).map_err(BookError::Read)?;
    if let Some(w) = want {
        return names
            .iter()
            .find(|n| n.as_str() == w)
            .map(|n| vec![n.clone()])
            .ok_or(BookError::NoSheet);
    }
    let same = |a: &str, b: &str| a.trim().to_lowercase() == b.trim().to_lowercase();
    if let Some(n) = names.iter().find(|n| v.sheets.iter().any(|s| same(s, n))) {
        return Ok(vec![n.clone()]);
    }
    Ok(names
        .into_iter()
        .filter(|n| !v.skip.iter().any(|s| same(s, n)))
        .collect())
}

/// Reads the requirements sheet (`sheet`, else the one named so, else the first with a
/// category and a parameter column). Every refused cell is listed; good rows are kept apart,
/// so the caller decides whether a book with errors may load at all.
pub fn read_requirements(
    path: &Path,
    v: &Vocab,
    sheet: Option<&str>,
) -> Result<ReadBook, BookError> {
    let candidates = pick_sheet(path, v, sheet)?;
    let groups: Vec<Vec<String>> = candidates.iter().map(|n| vec![n.clone()]).collect();
    let sheets = read_first_of(path, &groups, &|| false).map_err(BookError::Read)?;
    for s in sheets.into_iter().flatten() {
        let Some((head, cols)) = header(&s.grid, v) else {
            continue;
        };
        return Ok(read_rows(&s.name, &s.grid, head, &cols, v));
    }
    Err(BookError::NoSheet)
}

fn read_rows(
    sheet: &str,
    grid: &[Vec<SheetCell>],
    head: usize,
    cols: &[(ReqField, usize)],
    v: &Vocab,
) -> ReadBook {
    let mut out = ReadBook {
        sheet: sheet.to_string(),
        cols: cols.iter().map(|(f, c)| (*f, letter(*c))).collect(),
        ..ReadBook::default()
    };
    let col_of = |f: ReqField| cols.iter().find(|x| x.0 == f).map(|x| x.1);
    let at = |r: usize, f: ReqField, code: &'static str, arg: String| RowError {
        sheet: sheet.to_string(),
        row: r + 1,
        col: col_of(f).map(letter).unwrap_or_default(),
        field: f,
        code,
        arg,
    };
    for (r, row) in grid.iter().enumerate().skip(head + 1) {
        let mut t = ReqText::default();
        let mut bad = None;
        for (f, c) in cols {
            let text = match row.get(*c).map_or(Ok(String::new()), cell_text) {
                Ok(s) => s,
                Err((code, arg)) => {
                    bad.get_or_insert_with(|| at(r, *f, code, arg));
                    continue;
                }
            };
            let slot = match f {
                ReqField::Category => &mut t.cat,
                ReqField::Level => &mut t.level,
                ReqField::Type => &mut t.type_name,
                ReqField::Phase => &mut t.phase,
                ReqField::Parameter => &mut t.param,
                ReqField::Required => &mut t.required,
                ReqField::Rule => &mut t.kind,
                ReqField::Value => &mut t.value,
                ReqField::Unit => &mut t.unit,
            };
            *slot = match f {
                ReqField::Category | ReqField::Required | ReqField::Rule => word(v, &text),
                _ => text,
            };
        }
        let blank = bad.is_none()
            && [&t.cat, &t.level, &t.type_name, &t.phase, &t.param, &t.kind, &t.value, &t.unit]
                .iter()
                .all(|s| s.trim().is_empty());
        if blank {
            continue;
        }
        if out.rows.len() + out.errors.len() >= MAX_ROWS {
            out.errors
                .push(at(r, ReqField::Category, "req.err.too_many", MAX_ROWS.to_string()));
            break;
        }
        if let Some(e) = bad {
            out.errors.push(e);
            continue;
        }
        match parse_requirement(&t) {
            Ok(req) => out.rows.push(ReqRow {
                sheet: sheet.to_string(),
                row: r + 1,
                req,
            }),
            Err(e) => out.errors.push(at(r, e.field, e.code, e.arg)),
        }
    }
    out
}

/// Rows given as text rather than a book (the assistant's): row n of `sheet` is item n, and a
/// refusal names no column letter.
pub fn read_texts(sheet: &str, rows: &[ReqText], v: &Vocab) -> ReadBook {
    let mut out = ReadBook {
        sheet: sheet.to_string(),
        ..ReadBook::default()
    };
    for (i, t) in rows.iter().enumerate().take(MAX_ROWS) {
        let mut t = t.clone();
        t.cat = word(v, &t.cat);
        t.required = word(v, &t.required);
        t.kind = word(v, &t.kind);
        match parse_requirement(&t) {
            Ok(req) => out.rows.push(ReqRow {
                sheet: sheet.to_string(),
                row: i + 1,
                req,
            }),
            Err(e) => out.errors.push(RowError {
                sheet: sheet.to_string(),
                row: i + 1,
                col: String::new(),
                field: e.field,
                code: e.code,
                arg: e.arg,
            }),
        }
    }
    out
}

/// Texts of the template, already in the person's language.
#[derive(Clone, Debug, Default)]
pub struct TemplateText {
    pub sheet_req: String,
    pub sheet_example: String,
    pub sheet_help: String,
    /// One caption per column, in `ReqField::ALL` order.
    pub headers: Vec<String>,
    /// Example rows, nine cells each in `ReqField::ALL` order.
    pub example: Vec<Vec<String>>,
    /// Instruction lines: a line with a tab is a two-column row (term, explanation).
    pub help: Vec<String>,
    /// Choices offered in the rule, required and category cells (other words still read).
    pub kinds: Vec<String>,
    pub yes_no: Vec<String>,
    pub cats: Vec<String>,
}

fn err(e: XlsxError) -> String {
    e.to_string()
}

fn head_format() -> Format {
    Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xDDEBF7))
}

/// One cell of a written table: text (never a formula), a number, or nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TableCell<'a> {
    Text(&'a str),
    Num(f64),
    Empty,
}

/// What a row of a table holds; text rows and typed rows share one writer.
pub trait ToCell {
    fn cell(&self) -> TableCell<'_>;
}

impl ToCell for String {
    fn cell(&self) -> TableCell<'_> {
        TableCell::Text(self)
    }
}

impl TableCell<'_> {
    fn width(&self) -> usize {
        match self {
            TableCell::Text(s) => s.chars().count(),
            TableCell::Num(x) => x.to_string().len(),
            TableCell::Empty => 0,
        }
    }
}

/// A sheet table: bold shaded headings, columns as wide as their longest cell, the heading row
/// frozen. Numbers stay numbers; text is written as typed, a leading `=` included.
pub fn write_table<C: ToCell>(
    ws: &mut Worksheet,
    headers: &[String],
    rows: &[Vec<C>],
) -> Result<(), String> {
    let head = head_format();
    for (j, h) in headers.iter().enumerate() {
        let c = j as u16;
        ws.write_string_with_format(0, c, h, &head).map_err(err)?;
        let longest = rows
            .iter()
            .filter_map(|r| r.get(j))
            .map(|s| s.cell().width())
            .chain([h.chars().count()])
            .max()
            .unwrap_or(8);
        ws.set_column_width(c, (longest as f64 + 3.0).clamp(10.0, 48.0)).map_err(err)?;
    }
    for (i, row) in rows.iter().enumerate() {
        for (j, s) in row.iter().enumerate() {
            let (r, c) = (i as u32 + 1, j as u16);
            match s.cell() {
                TableCell::Text(t) => ws.write_string(r, c, t).map(|_| ()),
                TableCell::Num(x) => ws.write_number(r, c, x).map(|_| ()),
                TableCell::Empty => Ok(()),
            }
            .map_err(err)?;
        }
    }
    ws.set_freeze_panes(1, 0).map_err(err)?;
    Ok(())
}

/// A book of one sheet `name` holding the table, saved to `path`.
pub fn save_table<C: ToCell>(
    path: &Path,
    name: &str,
    headers: &[String],
    rows: &[Vec<C>],
) -> Result<(), String> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name(name).map_err(err)?;
    write_table(ws, headers, rows)?;
    wb.save(path).map_err(err)
}

/// A drop-down that offers words without refusing others; skipped when the list is too long.
fn offer(ws: &mut Worksheet, col: u16, items: &[String]) -> Result<(), String> {
    let total: usize = items.iter().map(|s| s.chars().count() + 1).sum();
    if items.is_empty() || total > 255 || items.iter().any(|s| s.contains(',')) {
        return Ok(());
    }
    let dv = DataValidation::new()
        .allow_list_strings(items)
        .map_err(err)?
        .show_error_message(false);
    ws.add_data_validation(1, col, TEMPLATE_ROWS, col, &dv).map_err(err)?;
    Ok(())
}

fn col_index(f: ReqField) -> u16 {
    ReqField::ALL.iter().position(|x| *x == f).unwrap_or(0) as u16
}

/// The template: an empty requirements sheet, a filled example and the instructions.
pub fn write_template(path: &Path, t: &TemplateText) -> Result<(), String> {
    if t.headers.len() != ReqField::ALL.len() {
        return Err("template: one header per column".into());
    }
    let mut wb = Workbook::new();
    for (name, rows) in [(&t.sheet_req, &[][..]), (&t.sheet_example, &t.example[..])] {
        let ws = wb.add_worksheet();
        ws.set_name(name.as_str()).map_err(err)?;
        write_table(ws, &t.headers, rows)?;
        offer(ws, col_index(ReqField::Category), &t.cats)?;
        offer(ws, col_index(ReqField::Required), &t.yes_no)?;
        offer(ws, col_index(ReqField::Rule), &t.kinds)?;
    }
    let ws = wb.add_worksheet();
    ws.set_name(t.sheet_help.as_str()).map_err(err)?;
    let bold = Format::new().set_bold();
    let wrap = Format::new().set_text_wrap();
    ws.set_column_width(0, 24.0).map_err(err)?;
    ws.set_column_width(1, 90.0).map_err(err)?;
    for (i, line) in t.help.iter().enumerate() {
        let r = i as u32;
        match line.split_once('\t') {
            Some((term, text)) => {
                ws.write_string_with_format(r, 0, term, &bold).map_err(err)?;
                ws.write_string_with_format(r, 1, text, &wrap).map_err(err)?;
            }
            None if i == 0 => {
                ws.write_string_with_format(r, 0, line, &bold).map_err(err)?;
            }
            None => {
                ws.write_string_with_format(r, 0, line, &wrap).map_err(err)?;
            }
        }
    }
    crate::fsutil::replace_durably(path, |p| wb.save(p).map_err(err))
}

/// A report cell.
#[derive(Clone, Debug, PartialEq)]
pub enum RCell {
    Text(String),
    Num(f64),
}

/// One sheet of the report: an optional note line above the table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReportSheet {
    pub name: String,
    pub note: Option<String>,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<RCell>>,
}

/// A sheet name the format accepts: no `[]:*?/\`, at most 31 characters, unique, not empty.
pub fn sheet_name(want: &str, taken: &[String]) -> String {
    let clean: String = want
        .chars()
        .map(|c| if "[]:*?/\\".contains(c) { ' ' } else { c })
        .collect();
    let clean = clean.trim().trim_matches('\'').trim();
    let base: String = if clean.is_empty() { "Sheet" } else { clean }.chars().take(31).collect();
    let free = |n: &str| !taken.iter().any(|t| t.to_lowercase() == n.to_lowercase());
    if free(&base) {
        return base;
    }
    for i in 2.. {
        let tail = format!(" {i}");
        let head: String = base.chars().take(31 - tail.chars().count()).collect();
        let n = format!("{}{tail}", head.trim_end());
        if free(&n) {
            return n;
        }
    }
    unreachable!("an unbounded counter always finds a free name")
}

/// The check report: sheets in order, the first being the summary.
pub fn write_report(path: &Path, sheets: &[ReportSheet]) -> Result<(), String> {
    if sheets.is_empty() {
        return Err("report: no sheets".into());
    }
    let mut wb = Workbook::new();
    let head = head_format();
    let note = Format::new().set_italic().set_font_color(Color::RGB(0x555555));
    let mut taken: Vec<String> = vec![];
    for s in sheets {
        let name = sheet_name(&s.name, &taken);
        taken.push(name.clone());
        let ws = wb.add_worksheet();
        ws.set_name(name.as_str()).map_err(err)?;
        let mut top = 0u32;
        if let Some(n) = &s.note {
            ws.write_string_with_format(0, 0, n, &note).map_err(err)?;
            top = 2;
        }
        for (j, h) in s.headers.iter().enumerate() {
            ws.write_string_with_format(top, j as u16, h, &head).map_err(err)?;
            let longest = s
                .rows
                .iter()
                .filter_map(|r| match r.get(j) {
                    Some(RCell::Text(t)) => Some(t.chars().count()),
                    Some(RCell::Num(_)) => Some(8),
                    None => None,
                })
                .chain([h.chars().count()])
                .max()
                .unwrap_or(8);
            ws.set_column_width(j as u16, (longest as f64 + 3.0).clamp(8.0, 60.0))
                .map_err(err)?;
        }
        for (i, row) in s.rows.iter().enumerate() {
            let r = top + 1 + i as u32;
            for (j, c) in row.iter().enumerate() {
                match c {
                    RCell::Text(t) => ws.write_string(r, j as u16, t).map_err(err)?,
                    RCell::Num(x) => ws.write_number(r, j as u16, *x).map_err(err)?,
                };
            }
        }
        ws.set_freeze_panes(top + 1, 0).map_err(err)?;
    }
    crate::fsutil::replace_durably(path, |p| wb.save(p).map_err(err))
}

#[cfg(test)]
#[path = "requirements_tests.rs"]
mod tests;
