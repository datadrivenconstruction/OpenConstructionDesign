// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! XLSX sheets of the element table: every family with a `sheet` and no bespoke sheet code in
//! `ddc-io` gets one sheet, with one column per geometry row of the family.

use super::{Cap, FAMILIES};
use crate::geom_props::{rows, Row, Unit};
use ddc_io::xlsx::{FamDim, FamilyCol, FamilySheet, RowDelete};
use ddc_ui::i18n::{Catalog, Lang};
use std::sync::OnceLock;

/// Families whose sheets are written by `ddc-io` itself, with their own columns.
pub const BESPOKE: [&str; 5] = ["wall", "opening", "room", "slab", "curtain_wall"];

/// Families whose elements the workbook may not delete, with the reason (a report text).
/// Every other family goes through the model delete of the Delete key (`Model::delete_items`).
pub const NO_ROW_DELETE: [(&str, &str); 1] = [("anno", "annotations are deleted in the editor")];

/// A family whose Delete key refuses (`xf.delete` is `No`) refuses in the workbook too.
fn row_delete(table: &str) -> RowDelete {
    if let Some(x) = NO_ROW_DELETE.iter().find(|x| x.0 == table) {
        return RowDelete::Refused(x.1);
    }
    match FAMILIES.iter().find(|f| f.table == table).map(|f| f.xf.delete) {
        Some(Cap::No(_)) => RowDelete::Refused(EDITOR_ONLY),
        _ => RowDelete::Allowed,
    }
}

/// Report text for a family the editor itself refuses to delete.
pub const EDITOR_ONLY: &str = "this element is deleted in the editor only";

fn english(key: &str) -> String {
    static CAT: OnceLock<Catalog> = OnceLock::new();
    let t = CAT.get_or_init(|| Catalog::new(Lang::En)).t_in(Lang::En, key);
    if t.is_empty() || t == key {
        key.rsplit('.').next().unwrap_or(key).replace('_', " ")
    } else {
        t
    }
}

fn column(r: &Row, taken: &mut Vec<String>) -> FamilyCol {
    let mut label = english(r.key);
    if taken.contains(&label) {
        label = format!("{label} ({})", r.key);
    }
    taken.push(label.clone());
    let dim = match r.unit {
        Unit::Len => FamDim::Len,
        Unit::Deg => FamDim::Deg,
        Unit::Pct => FamDim::Pct,
        Unit::Count => FamDim::Choice(1000),
        Unit::Type(t) => FamDim::Type(t),
        Unit::Choice(keys) => FamDim::Choice(keys.len()),
    };
    let labels = match r.unit {
        Unit::Choice(keys) => keys.iter().map(|k| english(k)).collect(),
        _ => vec![],
    };
    FamilyCol {
        key: r.key,
        label,
        dim,
        labels,
        lo: r.lo,
        hi: r.hi,
        get: r.get,
        set: r.set,
    }
}

/// The generic sheets, in table order.
pub fn families() -> Vec<FamilySheet> {
    let all = rows();
    FAMILIES
        .iter()
        .filter(|f| !BESPOKE.contains(&f.table))
        .filter_map(|f| match f.sheet {
            Cap::Yes(sheet) => Some((f.table, sheet)),
            Cap::No(_) => None,
        })
        .map(|(table, sheet)| {
            let mut taken = vec![];
            let cols = all
                .iter()
                .filter(|r| r.kind == table)
                .map(|r| column(r, &mut taken))
                .collect();
            FamilySheet {
                sheet,
                table,
                cols,
                delete: row_delete(table),
            }
        })
        .collect()
}
