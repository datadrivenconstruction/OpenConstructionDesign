//! Columns of one sheet that carry the same field (a copied column). Which one stands left must
//! not decide what applies: equal cells apply once, a row where they differ keeps the model value
//! and is refused by name. A column that names the row cannot be left out cell by cell, so a
//! difference there stops the import.

use std::collections::HashSet;

use calamine::Data;

use super::msg::{fill_template, DUP_COL_DIFFERS, DUP_COL_ID, DUP_COL_NOTE};
use super::{cell_ref, units, Ignored, Rejection, Report};

/// One field and its columns: (index in the cells, factor of its header to model units).
pub(crate) struct Group {
    pub keep: (usize, f64),
    pub copies: Vec<(usize, f64)>,
    /// The column names the row (`id` and the like).
    pub names_row: bool,
}

/// The sheet the groups stand on.
pub(crate) struct Sheet<'a> {
    pub name: &'a str,
    /// Header row, 1-based.
    pub r0: u32,
    /// First column of the range, 0-based.
    pub c0: u32,
    pub head: &'a [String],
    pub rows: &'a [(u32, Vec<Data>)],
}

fn filled(d: &Data) -> bool {
    match d {
        Data::Empty => false,
        Data::String(s) => !s.trim().is_empty(),
        _ => true,
    }
}

fn number(d: &Data) -> Option<f64> {
    match d {
        Data::Float(x) => Some(*x),
        Data::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// Same value once each header's unit is applied; blank equals only blank.
fn same_cell(a: Option<&Data>, ka: f64, b: Option<&Data>, kb: f64) -> bool {
    match (a.filter(|d| filled(d)), b.filter(|d| filled(d))) {
        (None, None) => true,
        (Some(a), Some(b)) => match (number(a), number(b)) {
            (Some(x), Some(y)) => {
                let (x, y) = (x * ka, y * kb);
                (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0)
            }
            _ => units::same_text(&a.to_string(), &b.to_string()),
        },
        _ => false,
    }
}

/// Notes every copy and refuses each row where a copy differs. The cell of that row goes into
/// `covered`, so the readers leave the model value as they do under a merge. A cell already there
/// was not given and is not compared.
pub(crate) fn check(
    s: &Sheet<'_>,
    groups: &[Group],
    covered: &mut HashSet<(u32, usize)>,
    rep: &mut Report,
) {
    let col = |j: usize| s.c0 + j as u32 + 1;
    let name = |j: usize| s.head.get(j).map_or("", |h| h.trim()).to_string();
    for g in groups {
        let (keep, ka) = g.keep;
        for &(copy, kb) in &g.copies {
            let at_copy = cell_ref(s.name, s.r0, col(copy));
            let at_keep = cell_ref(s.name, s.r0, col(keep));
            rep.ignored.push(Ignored {
                sheet: s.name.into(),
                row: s.r0,
                col: col(copy),
                column: name(copy),
                id: None,
                old: String::new(),
                new: String::new(),
                reason: fill_template(
                    DUP_COL_NOTE.1,
                    &[&name(copy), &at_copy, &name(keep), &at_keep],
                ),
            });
            for (r, cells) in s.rows {
                let r = *r;
                if (covered.contains(&(r, keep)) && !g.names_row) || covered.contains(&(r, copy)) {
                    continue;
                }
                if same_cell(cells.get(keep), ka, cells.get(copy), kb) {
                    continue;
                }
                let tpl = if g.names_row { DUP_COL_ID.1 } else { DUP_COL_DIFFERS.1 };
                let (a, b) = (cell_ref(s.name, r, col(keep)), cell_ref(s.name, r, col(copy)));
                rep.rejected.push(Rejection {
                    sheet: s.name.into(),
                    // row 0 stops the whole import: the row itself is in doubt
                    row: if g.names_row { 0 } else { r },
                    col: col(keep),
                    column: name(keep),
                    reason: fill_template(tpl, &[&name(keep), &name(copy), &a, &b]),
                });
                covered.insert((r, keep));
            }
        }
    }
}
