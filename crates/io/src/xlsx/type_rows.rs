// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Book rows that edit a type rather than one element: a type sheet row, or a layer row of a
//! type. Every element of that type takes the change, so the preview counts them and Apply asks
//! for a tick. The model table of the type is named, the count itself is the caller's.

use super::{Report, LAYERS, MEMBER_TYPES, SLAB_LAYERS, SLAB_TYPES, TYPES};

/// Row key of a layer (`layer_key`) back to its type id.
fn layer_type(key: i64) -> i64 {
    key.div_euclid(1_000_000)
}

/// Type table and type id a row of `sheet` with row id `id` edits; None for an element sheet.
pub fn type_row(sheet: &str, id: i64) -> Option<(&'static str, i64)> {
    Some(match sheet {
        s if s == TYPES => ("wall_type", id),
        s if s == LAYERS => ("wall_type", layer_type(id)),
        s if s == SLAB_TYPES => ("slab_type", id),
        s if s == SLAB_LAYERS => ("slab_type", layer_type(id)),
        s if s == MEMBER_TYPES => ("member_type", id),
        _ => return None,
    })
}

/// Types the report changes, each once (a renamed type and two of its layers are one type),
/// sorted. Taken conflicts are in `changes`; untaken ones write nothing and are left out.
pub fn changed_types(rep: &Report) -> Vec<(&'static str, i64)> {
    let mut v: Vec<(&'static str, i64)> = rep
        .changes
        .iter()
        .filter(|c| c.kind() != "added")
        .filter_map(|c| type_row(&c.sheet, c.id?))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[cfg(test)]
#[path = "tests_type_rows.rs"]
mod tests;
