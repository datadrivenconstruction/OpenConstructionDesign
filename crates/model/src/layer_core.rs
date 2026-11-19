// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Structural core layer of layered types: where the data has no explicit flag, it is the thickest
//! layer that is not insulation (material conductivity below the project threshold
//! `insulation_lambda`, default 0.1). A roof ends with a finish at the bottom: the underside of
//! the structural layer sits above the underside of the whole roof by the finish thickness;
//! the wall core reaches the underside of the roof's structural layer, the wall finish reaches the
//! underside of the whole roof.

use rusqlite::Connection;

use crate::Error;

pub(crate) fn insulation_lambda(db: &Connection) -> f64 {
    crate::roof_rules::value(db, crate::roof_rules::INSULATION_LAMBDA)
}

fn conductivity(db: &Connection, material: &str) -> Option<f64> {
    db.query_row("SELECT conductivity FROM material WHERE name=?1", [material], |r| r.get(0))
        .ok()
        .flatten()
}

/// Index of the thickest non-insulating layer (the topmost on ties); None if there are no layers.
pub(crate) fn core_index(db: &Connection, layers: &[(String, f64)]) -> Option<usize> {
    let lambda = insulation_lambda(db);
    let pick = |skip_insulation: bool| {
        let mut best: Option<(usize, f64)> = None;
        for (i, (m, t)) in layers.iter().enumerate() {
            if skip_insulation && conductivity(db, m).is_some_and(|l| l < lambda) {
                continue;
            }
            if best.is_none_or(|b| *t > b.1 + 1e-12) {
                best = Some((i, *t));
            }
        }
        best.map(|b| b.0)
    };
    pick(true).or_else(|| pick(false))
}

/// Wall type layers (outside to inside): core by explicit flag, otherwise by the rule above.
pub(crate) fn wall_core(db: &Connection, wtype: i64) -> Result<Vec<bool>, Error> {
    let flags = crate::wall_edit::layer_meta(db, wtype)?;
    if flags.iter().any(|f| f.1) {
        return Ok(flags.iter().map(|f| f.1).collect());
    }
    let mut st = db.prepare_cached("SELECT material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos")?;
    let layers: Vec<(String, f64)> = st.query_map([wtype], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let core = core_index(db, &layers);
    Ok((0..layers.len()).map(|i| Some(i) == core).collect())
}

/// Roof thickness from the top down to the underside of the structural layer, inclusive (layers top to bottom).
pub(crate) fn roof_structure_depth(db: &Connection, stype: i64) -> Result<f64, Error> {
    let mut st = db.prepare_cached("SELECT material, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos")?;
    let layers: Vec<(String, f64)> = st.query_map([stype], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    Ok(match core_index(db, &layers) {
        Some(i) => layers[..=i].iter().map(|l| l.1).sum(),
        None => 0.0,
    })
}
