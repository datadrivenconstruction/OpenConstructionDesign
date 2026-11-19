// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Shaft (SH): outline sketch, Enter; cuts floors/ceilings/roofs from bottom to top.

use super::{ccw, tup, Active, Rec};
use ddc_geom::V2;
use ddc_model::ShaftSpec;

pub fn rec(a: &Active, pts: &[V2]) -> Result<Rec, String> {
    let mut ring = pts.to_vec();
    ccw(&mut ring);
    // level above — up to it; otherwise height above its own level
    let (top_level, top_offset) = match a.above {
        Some((id, _)) => (id, 0.0),
        None => (a.level, a.opts.top_off),
    };
    let s = ShaftSpec {
        level: a.level,
        top_level,
        base_offset: a.opts.base_off,
        top_offset,
        ring: ring.iter().map(|p| tup(*p)).collect(),
    };
    Ok(Rec::new("shaft", move |m| {
        m.add_shaft(&s)?;
        Ok(1)
    }))
}
