// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Ceiling (CG): auto — click inside a closed wall loop; otherwise sketch the outline.

use super::{ensure_slab_type, tup, Active, Rec};
use crate::editor::Editor;
use ddc_geom::V2;
use ddc_model::CeilingSpec;

fn rec_rings(a: &Active, outer: Vec<(f64, f64)>, holes: Vec<Vec<(f64, f64)>>) -> Rec {
    let s = CeilingSpec {
        level: a.level,
        slab_type: 0,
        height: a.opts.height,
        outer,
        holes,
    };
    let ty = a.type_name().to_string();
    Rec::new("ceiling", move |m| {
        let (t, n) = ensure_slab_type(m, &ty)?;
        m.add_ceiling(&CeilingSpec {
            slab_type: t,
            ..s.clone()
        })?;
        Ok(n + 1)
    })
}

pub fn rec(a: &Active, pts: &[V2]) -> Result<Rec, String> {
    Ok(rec_rings(a, pts.iter().map(|p| tup(*p)).collect(), vec![]))
}

/// Closed wall region under the point (highlight and auto ceiling).
pub fn region_at(ed: &Editor, p: V2) -> Option<ddc_geom::rooms::Region> {
    ed.model
        .room_regions(ed.level)
        .ok()?
        .into_iter()
        .find(|r| r.contains(p))
}

pub fn pick(ed: &Editor, a: &Active, p: V2) -> Result<Rec, String> {
    let r = match region_at(ed, p) {
        Some(r) => r,
        None => super::drawing_loop::region(&ed.tool.underlay_segs, p)
            .map_err(|e| a.t(e.key()))?,
    };
    let r = super::drawing_loop::tidy(r).map_err(|e| a.t(e.key()))?;
    let ring = |v: &Vec<V2>| v.iter().map(|q| tup(*q)).collect::<Vec<_>>();
    Ok(rec_rings(
        a,
        ring(&r.outer),
        r.holes.iter().map(ring).collect(),
    ))
}
