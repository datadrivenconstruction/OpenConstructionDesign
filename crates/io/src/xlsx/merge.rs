//! Merged cells of a sheet. A spreadsheet stores a merged value only in the top-left cell;
//! the covered cells read as empty. A merge down a data column fills its value down that
//! column; the other columns a merge covers are "not given" (the model value is kept, and the
//! preview says so). A merge that hides the id is reported and its rows are left out (and the
//! import deletes nothing on that sheet), a header merged across columns is refused.

use std::collections::HashSet;
use std::io::{Read, Seek};

use calamine::{Data, Dimensions, Xlsx};

use super::{fill_template, Ignored, Rejection, Report};

pub(super) const HEADER_TPL: &str = "header cells are merged across columns {}; unmerge them";
pub(super) const ID_TPL: &str = "merged cells cover the id column; rows {} ignored (unmerge them)";
pub(super) const COVER_TPL: &str = "merged into {} on rows {}; not read, the model value is kept";

/// What the merges did to the rows of one sheet.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Merged {
    /// First 1-based row of a merge that hides an id: the caller must not delete elements
    /// whose rows it cannot see.
    pub id_row: Option<u32>,
    /// (1-based row, index in the cells) of covered cells that were not given.
    pub covered: HashSet<(u32, usize)>,
}

/// Merged regions of one sheet (absolute 0-based rows and columns); single cells are dropped.
pub(super) fn regions<RS: Read + Seek>(wb: &mut Xlsx<RS>, sheet: &str) -> Vec<Dimensions> {
    match wb.merge_cells_by_sheet_name(sheet) {
        Ok(v) => v.into_iter().filter(|d| d.start != d.end).collect(),
        Err(_) => vec![],
    }
}

fn letters(col0: u32) -> String {
    let (mut n, mut s) = (col0 + 1, String::new());
    while n > 0 {
        s.insert(0, (b'A' + ((n - 1) % 26) as u8) as char);
        n = (n - 1) / 26;
    }
    s
}

fn blank(d: &Data) -> bool {
    match d {
        Data::Empty => true,
        Data::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// Header text of absolute column `c`, or its letters.
fn caption(head: &[String], c0: u32, c: u32) -> String {
    c.checked_sub(c0)
        .and_then(|i| head.get(i as usize))
        .filter(|h| !h.is_empty())
        .cloned()
        .unwrap_or_else(|| letters(c))
}

/// Marks the cells of columns `from..=ce` on rows `rs..=re` as not given and notes each named
/// column once.
#[allow(clippy::too_many_arguments)]
fn cover(
    sheet: &str,
    c0: u32,
    head: &[String],
    (rs, cs, re, ce): (u32, u32, u32, u32),
    from: u32,
    rows: &[(u32, Vec<Data>)],
    out: &mut Merged,
    rep: &mut Report,
) {
    let hit: Vec<u32> = rows.iter().map(|(r, _)| *r).filter(|r| (rs + 1..=re + 1).contains(r)).collect();
    if hit.is_empty() {
        return;
    }
    let span = if rs == re { format!("{}", rs + 1) } else { format!("{}..{}", rs + 1, re + 1) };
    for c in from.max(c0)..=ce {
        let i = (c - c0) as usize;
        for r in &hit {
            out.covered.insert((*r, i));
        }
        if head.get(i).is_some_and(|h| !h.is_empty()) {
            rep.ignored.push(Ignored {
                sheet: sheet.into(),
                row: rs + 1,
                col: c + 1,
                column: head[i].clone(),
                id: None,
                old: String::new(),
                new: String::new(),
                reason: fill_template(COVER_TPL, &[&caption(head, c0, cs), &span]),
            });
        }
    }
}

/// `rows` holds (1-based row number, cells) of the data rows; `hdr` is the 0-based header row,
/// `c0` the sheet's first column and `id_col` the index of the id column in the cells.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    sheet: &str,
    hdr: u32,
    c0: u32,
    head: &[String],
    id_col: Option<usize>,
    regs: &[Dimensions],
    rows: &mut Vec<(u32, Vec<Data>)>,
    rep: &mut Report,
) -> Merged {
    let mut out = Merged::default();
    let id_abs = id_col.map(|i| c0 + i as u32);
    for d in regs {
        let ((rs, cs), (re, ce)) = (d.start, d.end);
        if rs <= hdr && hdr <= re {
            if ce > cs {
                rep.rejected.push(Rejection {
                    sheet: sheet.into(),
                    row: hdr + 1,
                    col: cs + 1,
                    column: caption(head, c0, cs),
                    reason: fill_template(HEADER_TPL, &[&format!("{}:{}", letters(cs), letters(ce))]),
                });
            }
            continue;
        }
        if re < hdr {
            continue;
        }
        let reg = (rs, cs, re, ce);
        if let Some(ic) = id_abs.filter(|c| cs <= *c && *c <= ce) {
            // the id is readable only on the top row, and only when it is the top-left cell
            let id_seen = ic == cs;
            if id_seen {
                // the top row keeps its id; the cells merged with it were not given
                cover(sheet, c0, head, (rs, cs, rs, ce), cs + 1, rows, &mut out, rep);
            }
            if re == rs && id_seen {
                continue;
            }
            let (a, b) = (rs + 1, re + 1);
            out.id_row = Some(out.id_row.map_or(a, |m| m.min(a)));
            rep.ignored.push(Ignored {
                sheet: sheet.into(),
                row: a,
                col: ic + 1,
                column: "id".into(),
                id: None,
                old: String::new(),
                new: String::new(),
                reason: fill_template(ID_TPL, &[&format!("{a}..{b}")]),
            });
            // a hidden id would read as a new element; only a row showing its id stays
            rows.retain(|(r, _)| {
                let abs = r - 1;
                (abs == rs && id_seen) || abs < rs || abs > re
            });
            continue;
        }
        if cs < c0 {
            continue;
        }
        let cs_i = (cs - c0) as usize;
        // only a vertical span fills down, and only within its own column
        let v = rows
            .iter()
            .find(|(r, _)| *r == rs + 1)
            .and_then(|(_, cells)| cells.get(cs_i))
            .filter(|v| !blank(v))
            .cloned();
        if let Some(v) = v.filter(|_| re > rs) {
            for (r, cells) in rows.iter_mut() {
                let abs = *r - 1;
                if abs <= rs || abs > re {
                    continue;
                }
                if cells.len() <= cs_i {
                    cells.resize(cs_i + 1, Data::Empty);
                }
                cells[cs_i] = v.clone();
            }
        }
        if ce > cs {
            cover(sheet, c0, head, reg, cs + 1, rows, &mut out, rep);
        }
    }
    out
}
