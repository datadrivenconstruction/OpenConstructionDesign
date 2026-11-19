//! Test stand-in for typing a new row into a spreadsheet program.

use std::path::Path;

use calamine::{open_workbook, Data, Reader, Xlsx};
use rust_xlsxwriter::Workbook;

use super::units;
use super::TableCell;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn number(d: &Data) -> Option<f64> {
    match d {
        Data::Float(x) => Some(*x),
        Data::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// Copies a book with one more row at the end of `sheet`: the row of `from_id` without its id
/// and read-only cells, with the `tweaks` (header, value) applied. Used by the Apply fuzz test.
#[doc(hidden)]
pub fn clone_row_as_new(
    src: &Path,
    dst: &Path,
    sheet: &str,
    from_id: i64,
    tweaks: &[(&str, TableCell)],
) -> Result<(), String> {
    let mut wb: Xlsx<_> = open_workbook(src).map_err(err)?;
    let mut out = Workbook::new();
    let mut hit = false;
    for name in wb.sheet_names() {
        let range = wb.worksheet_range(&name).map_err(err)?;
        let mut grid: Vec<Vec<Data>> = range.rows().map(|r| r.to_vec()).collect();
        if name == sheet {
            let head: Vec<String> = grid.first().map(|h| h.iter().map(|d| d.to_string()).collect()).unwrap_or_default();
            let from = grid
                .iter()
                .skip(1)
                .find(|r| r.first().and_then(number) == Some(from_id as f64))
                .cloned()
                .ok_or_else(|| format!("no row with id {from_id} in '{sheet}'"))?;
            let mut row = from;
            row.resize(head.len(), Data::Empty);
            row[0] = Data::Empty;
            for (j, h) in head.iter().enumerate() {
                if h.contains("read-only") {
                    row[j] = Data::Empty;
                }
            }
            for (h, v) in tweaks {
                let want = units::norm(h);
                let j = head
                    .iter()
                    .position(|x| units::norm(x) == want)
                    .ok_or_else(|| format!("no column '{h}'"))?;
                row[j] = match v {
                    TableCell::Num(x) | TableCell::Subtotal(_, x) => Data::Float(*x),
                    TableCell::Text(s) => Data::String(s.clone()),
                    TableCell::CountLabel { .. } => Data::Empty,
                };
            }
            grid.push(row);
            hit = true;
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
        return Err(format!("no sheet '{sheet}'"));
    }
    crate::fsutil::replace_durably(dst, |p| out.save(p).map_err(err))
}
