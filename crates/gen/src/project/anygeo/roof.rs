// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The roof check: every roof of the project is built by the weighted straight skeleton the
//! model uses (hip, gable and shed are edge slopes of one ring, flat is the drainage fall on
//! every edge), so a footprint whose roof would not close is seen at generation time.

use crate::project::{GProject, PRoof};
use ddc_geom::roof::{Extrusion, Footprint};
use ddc_geom::v2;

/// Whether the roof closes over its ring; a vault, whether its profile draws.
pub(crate) fn closes(r: &PRoof) -> bool {
    if let Some(v) = &r.vault {
        let e = Extrusion { a: v2(v.a.0, v.a.1), b: v2(v.b.0, v.b.1), profile: v.profile.clone(), depth: v.depth };
        return e.build(r.elevation).is_ok_and(|roof| !roof.faces.is_empty());
    }
    let n = r.ring.len();
    let slopes = if r.edges.len() == n { r.edges.clone() } else { vec![Some(r.slope); n] };
    let f = Footprint {
        ring: r.ring.iter().map(|p| v2(p.0, p.1)).collect(),
        slopes,
        base_z: r.elevation,
        overhang: r.overhang,
        mansard: None,
    };
    f.build().is_ok_and(|roof| !roof.faces.is_empty())
}

/// How many roofs of the project close: all of them on a sound project.
pub(crate) fn skeleton_ok(p: &GProject) -> usize {
    std::iter::once(&p.roof).chain(&p.extra_roofs).filter(|r| closes(r)).count()
}
