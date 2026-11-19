// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Room separator: a line by two clicks, chained by default.

use super::{tup, Active, Rec};
use ddc_geom::V2;
use ddc_model::SepSpec;

pub fn rec(a: &Active, p0: V2, q: V2) -> Result<Rec, String> {
    let s = SepSpec {
        level: a.level,
        a: tup(p0),
        b: tup(q),
    };
    Ok(Rec::new("room_sep", move |m| {
        m.add_room_sep(&s)?;
        Ok(1)
    }))
}
