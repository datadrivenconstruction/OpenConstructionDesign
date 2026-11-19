// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Worksheet cells read back as a plain grid, with the cell kind a caller must not take for a
//! value: an error (`#REF!`), a formula saved without its result, a formula with its result.

use calamine::{Data, Reader, Xlsx};
use std::collections::HashSet;
use std::path::Path;

type Book = Xlsx<std::io::BufReader<std::fs::File>>;

/// One cell read back from a worksheet.
#[derive(Clone, Debug, PartialEq)]
pub enum SheetCell {
    Empty,
    Num(f64),
    Text(String),
    Bool(bool),
    /// An error value (`#REF!`, `#DIV/0!`), typed or the result of a formula: never a value.
    Error(String),
    /// A formula saved without a result (a script or an app that does not recalculate wrote
    /// the book), as `=A1*2`. It reads as nothing, so it is never taken for a blank.
    Uncalc(String),
}

/// One worksheet as cells from A1: `grid[r][c]`, 0-based, rows may differ in length.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadSheet {
    pub name: String,
    pub grid: Vec<Vec<SheetCell>>,
    /// Cells `(row, col)` of `grid` holding a formula with its result: the value is the result.
    pub formulas: HashSet<(usize, usize)>,
}

fn open_book(path: &Path) -> Result<Book, String> {
    calamine::open_workbook(path).map_err(|e| format!("cannot open workbook: {e}"))
}

fn cell_of(d: &Data) -> SheetCell {
    match d {
        Data::Empty => SheetCell::Empty,
        Data::Int(i) => SheetCell::Num(*i as f64),
        Data::Float(f) => SheetCell::Num(*f),
        Data::Bool(b) => SheetCell::Bool(*b),
        Data::String(s) if s.trim().is_empty() => SheetCell::Empty,
        Data::String(s) => SheetCell::Text(s.clone()),
        Data::DateTime(t) => SheetCell::Num(t.as_f64()),
        Data::Error(e) => SheetCell::Error(e.to_string()),
        other => SheetCell::Text(other.to_string()),
    }
}

/// Sheet `name` as cells; None for a sheet without cells (a chart sheet).
fn sheet_cells(wb: &mut Book, name: &str) -> Result<Option<ReadSheet>, String> {
    let Ok(range) = wb.worksheet_range(name) else {
        return Ok(None);
    };
    // without its formulas a result would read as a typed value and a lost one as a blank
    let forms = wb
        .worksheet_formula(name)
        .map_err(|e| format!("cannot read the formulas of sheet {name}: {e}"))?;
    let (r0, c0) = range.start().unwrap_or((0, 0));
    let mut grid: Vec<Vec<SheetCell>> = vec![vec![]; r0 as usize];
    // a calculated "" (=IF(..;"")) is a result, not a missing one
    let mut text_blank = HashSet::new();
    for (i, row) in range.rows().enumerate() {
        let mut out = vec![SheetCell::Empty; c0 as usize];
        for (j, d) in row.iter().enumerate() {
            if matches!(d, Data::String(_)) {
                text_blank.insert((r0 as usize + i, c0 as usize + j));
            }
            out.push(cell_of(d));
        }
        grid.push(out);
    }
    let mut formulas = HashSet::new();
    if let Some((f0, g0)) = forms.start() {
        for (i, j, f) in forms.used_cells() {
            let f = f.trim().trim_start_matches('=').trim();
            if f.is_empty() {
                continue;
            }
            // a formula without a result may lie past the value range: the grid grows to it
            let (r, c) = (f0 as usize + i, g0 as usize + j);
            if grid.len() <= r {
                grid.resize(r + 1, vec![]);
            }
            let row = &mut grid[r];
            if row.len() <= c {
                row.resize(c + 1, SheetCell::Empty);
            }
            match &row[c] {
                SheetCell::Error(_) => {}
                SheetCell::Empty if !text_blank.contains(&(r, c)) => {
                    row[c] = SheetCell::Uncalc(format!("={f}"));
                }
                _ => {
                    formulas.insert((r, c));
                }
            }
        }
    }
    Ok(Some(ReadSheet {
        name: name.to_string(),
        grid,
        formulas,
    }))
}

/// Opens the book once; for each group of sheet names reads the first one the book has. A
/// group none of whose names exist reads nothing: no other sheet stands in for it. `stop` is
/// asked before each sheet; true ends the read with [`crate::job::CANCELLED`].
pub fn read_first_of(
    path: &Path,
    groups: &[Vec<String>],
    stop: &dyn Fn() -> bool,
) -> Result<Vec<Option<ReadSheet>>, String> {
    let mut wb = open_book(path)?;
    let names = wb.sheet_names();
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        if stop() {
            return Err(crate::job::CANCELLED.into());
        }
        let hit = g.iter().find(|w| names.iter().any(|n| n == *w));
        out.push(match hit {
            Some(n) => sheet_cells(&mut wb, n)?,
            None => None,
        });
    }
    Ok(out)
}

/// Cells of one worksheet as a grid from A1: `grid[r][c]`, 0-based, rows may differ in length.
/// `sheet` picks the sheet by name; when absent or missing, the first worksheet is read.
/// Chart sheets are skipped. Returns the sheet name and the grid.
pub fn read_sheet_cells(
    path: &Path,
    sheet: Option<&str>,
) -> Result<(String, Vec<Vec<SheetCell>>), String> {
    let mut wb = open_book(path)?;
    let names = wb.sheet_names();
    let mut order: Vec<String> = sheet
        .filter(|s| names.iter().any(|n| n == s))
        .map(|s| vec![s.to_string()])
        .unwrap_or_default();
    order.extend(names.iter().cloned());
    for name in order {
        if let Some(s) = sheet_cells(&mut wb, &name)? {
            return Ok((s.name, s.grid));
        }
    }
    Err("the workbook has no worksheet".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_xlsxwriter::{Formula, Workbook};

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ddc_cells_{tag}_{}.xlsx", std::process::id()))
    }

    /// A cached `#REF!` becomes an error value, as a spreadsheet app saves it; an empty cached
    /// result is dropped, as a book nobody calculated keeps only the formula.
    fn fix_cells(s: &str) -> String {
        let mut out = String::new();
        for (i, part) in s.split("<c ").enumerate() {
            if i > 0 {
                out.push_str("<c ");
            }
            let p = if part.contains("<v>#REF!</v>") {
                part.replacen(" t=\"str\"", " t=\"e\"", 1)
            } else if part.contains("<v></v>") {
                part.replacen("<v></v>", "", 1).replacen(" t=\"str\"", "", 1)
            } else {
                part.to_string()
            };
            out.push_str(&p);
        }
        out
    }

    /// Copies a book through [`fix_cells`].
    fn drop_results(src: &Path, dst: &Path) {
        use std::io::{Read, Write};
        let mut z = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
        let mut out = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
        for i in 0..z.len() {
            let mut e = z.by_index(i).unwrap();
            let name = e.name().unwrap().into_owned();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            if name.starts_with("xl/worksheets/sheet") {
                buf = fix_cells(&String::from_utf8(buf).unwrap()).into_bytes();
            }
            out.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            out.write_all(&buf).unwrap();
        }
        out.finish().unwrap();
    }

    /// Sheets "Walls" (A1 Id, B1 Comments; B2 #REF!, B3 uncalculated in the last column,
    /// B4 =1+1 with its result) and "Other".
    fn book(tag: &str) -> std::path::PathBuf {
        let raw = tmp(&format!("{tag}_raw"));
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.set_name("Other").unwrap();
        ws.write_string(0, 0, "x").unwrap();
        let ws = wb.add_worksheet();
        ws.set_name("Walls").unwrap();
        ws.set_formula_result_default("");
        ws.write_string(0, 0, "Id").unwrap();
        ws.write_string(0, 1, "Comments").unwrap();
        for r in 1..4 {
            ws.write_number(r, 0, r as f64).unwrap();
        }
        ws.write_formula(1, 1, Formula::new("=#REF!+1").set_result("#REF!")).unwrap();
        ws.write_formula(2, 1, Formula::new("=A3*2").set_result("")).unwrap();
        ws.write_formula(3, 1, Formula::new("=1+1").set_result("2")).unwrap();
        wb.save(&raw).unwrap();
        let p = tmp(tag);
        drop_results(&raw, &p);
        let _ = std::fs::remove_file(&raw);
        p
    }

    #[test]
    fn error_uncalculated_and_calculated_cells_keep_their_kind() {
        let p = book("kinds");
        let got = read_first_of(&p, &[vec!["Walls".to_string()]], &|| false).unwrap();
        let s = got[0].as_ref().unwrap();
        assert_eq!(s.grid[1][1], SheetCell::Error("#REF!".into()), "{:?}", s.grid);
        assert_eq!(s.grid[2][1], SheetCell::Uncalc("=A3*2".into()), "{:?}", s.grid);
        assert_eq!(s.grid[3][1], SheetCell::Num(2.0), "{:?}", s.grid);
        assert!(s.formulas.contains(&(3, 1)) && !s.formulas.contains(&(2, 1)), "{:?}", s.formulas);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_sheet_name_reads_no_other_sheet() {
        let p = book("missing");
        let groups = vec![vec!["Doors".to_string(), "Fenster".to_string()], vec!["Other".into()]];
        let got = read_first_of(&p, &groups, &|| false).unwrap();
        assert_eq!(got[0], None, "no foreign sheet stands in");
        assert_eq!(got[1].as_ref().map(|s| s.name.as_str()), Some("Other"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn stop_ends_the_read_as_cancelled() {
        let p = book("stop");
        let e = read_first_of(&p, &[vec!["Walls".to_string()]], &|| true).unwrap_err();
        assert_eq!(e, crate::job::CANCELLED);
        let _ = std::fs::remove_file(&p);
    }
}
