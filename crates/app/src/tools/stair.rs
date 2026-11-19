// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stair (ST): click = flight start, click = flight end; preview "N risers of M";
//! alignment line centre/left/right; "Auto railing" — both edges in the same write.

use super::{commit, tup, Active, Rec};
use crate::editor::Editor;
use ddc_geom::stair::{Shape as StairShape, StairDef, StairError};
use ddc_geom::{v2, V2};
use ddc_model::{RailingHost, RailingSpec, StairSpec};

/// Tread depth of the preview and the written stair, m.
pub const TREAD: f64 = 0.28;

/// (risers placed, risers needed in total) for a flight of length `len`.
pub fn count(a: &Active, len: f64) -> (usize, usize) {
    let h = a.above.map_or(3.0, |l| l.1 - a.elevation);
    let total = (h / 0.18 - 1e-9).ceil().max(1.0) as usize;
    let placed = ((len / TREAD).floor() as usize + 1).min(total);
    (placed, total)
}

/// Midpoint of the bottom riser along the alignment line.
pub fn origin(a: &Active, p: V2, q: V2) -> V2 {
    let d = q - p;
    let l = d.len().max(1e-9);
    let left = v2(-d.y / l, d.x / l);
    let w = a.opts.width / 2.0;
    match a.opts.just {
        1 => p - left * w,
        2 => p + left * w,
        _ => p,
    }
}

pub fn spec(a: &Active, p: V2, q: V2) -> Option<StairSpec> {
    let (top, _) = a.above?;
    let d = q - p;
    let mut s = StairSpec::straight(a.level, top, tup(origin(a, p, q)), d.y.atan2(d.x));
    s.width = a.opts.width;
    s.tread = Some(TREAD);
    Some(s)
}

pub fn rec(a: &Active, p: V2, q: V2) -> Result<Rec, String> {
    let s = spec(a, p, q).ok_or_else(|| a.t("tool.err.no_level_above"))?;
    Ok(record(a, s))
}

/// One undo step: the stair and, when enabled, its railings.
fn record(a: &Active, s: StairSpec) -> Rec {
    let rail = a.opts.auto_rail;
    let level = a.level;
    Rec::new("stair", move |m| {
        let id = m.add_stair(&s)?;
        if rail {
            let tpl = RailingSpec::new(level, RailingHost::Stair { stair: id, side: 0 });
            m.add_stair_railings(id, &tpl)?;
            return Ok(2);
        }
        Ok(1)
    })
}

/// L and U shapes take three clicks.
pub fn is_multi(a: &Active) -> bool {
    a.opts.stair_shape != 0
}

fn cross(d: V2, v: V2) -> f64 {
    d.x * v.y - d.y * v.x
}

/// Risers of the first flight for a run `p`..`c`, checked against the total.
fn first_flight(a: &Active, p: V2, c: V2) -> Result<usize, String> {
    if a.above.is_none() {
        return Err(a.t("tool.err.no_level_above"));
    }
    let len = (c - p).len();
    let k1 = (len / TREAD + 1e-9).floor() as usize + 1;
    let total = count(a, len).1;
    if k1 < 2 || k1 + 2 > total {
        return Err(a.cat.tf(
            "tool.stair.err_split",
            &[("n", &k1.to_string()), ("m", &total.to_string())],
        ));
    }
    Ok(k1)
}

/// Spec of an L or U stair from start `p`, flight end `c`, and the side click `q`.
pub fn multi_spec(a: &Active, p: V2, c: V2, q: V2) -> Result<StairSpec, String> {
    let k1 = first_flight(a, p, c)?;
    let (top, _) = a.above.ok_or_else(|| a.t("tool.err.no_level_above"))?;
    let d = c - p;
    let left = cross(d, q - c) > 0.0;
    let mut s = StairSpec::straight(a.level, top, tup(origin(a, p, c)), d.y.atan2(d.x));
    s.width = a.opts.width;
    s.tread = Some(TREAD);
    s.landing = a.opts.landing;
    let first = Some(k1);
    s.shape = if a.opts.stair_shape == 2 {
        StairShape::U {
            left,
            first,
            gap: a.opts.gap,
        }
    } else {
        StairShape::L { left, first }
    };
    Ok(s)
}

/// Preview geometry: outlines of flights and landings, risers per flight, total.
pub struct Multi {
    pub rings: Vec<Vec<V2>>,
    pub risers: Vec<usize>,
    pub total: usize,
}

pub fn multi_layout(a: &Active, p: V2, c: V2, q: V2) -> Result<Multi, String> {
    let s = multi_spec(a, p, c, q)?;
    let top = a.above.map_or(a.elevation, |l| l.1);
    let def = StairDef {
        origin: v2(s.origin.0, s.origin.1),
        angle: s.angle,
        base_z: a.elevation,
        height: top - a.elevation,
        width: s.width,
        max_riser: s.max_riser,
        tread: s.tread,
        landing: s.landing,
        thickness: s.thickness,
        stringer: s.stringer,
        shape: s.shape,
    };
    let st = def.layout().map_err(|e| match e {
        StairError::Comfort { .. } => a.t("tool.stair.err_comfort"),
        StairError::Split { first, total } => a.cat.tf(
            "tool.stair.err_split",
            &[("n", &first.to_string()), ("m", &total.to_string())],
        ),
        StairError::Input(_) => a.t("tool.stair.err_input"),
    })?;
    let mut rings: Vec<Vec<V2>> = st.flights.iter().map(|f| f.ring(st.tread)).collect();
    rings.extend(st.landings.iter().map(|l| l.ring.clone()));
    Ok(Multi {
        rings,
        risers: st.flights.iter().map(|f| f.risers).collect(),
        total: st.risers,
    })
}

/// Click for L/U: the second click fixes the first flight (refused when too short),
/// the third picks the side and commits.
pub fn click_multi(ed: &mut Editor, a: &mut Active, q: V2) {
    let Some(&p0) = a.pts.first() else {
        a.pts.push(q);
        return;
    };
    if (q - *a.pts.last().unwrap_or(&p0)).len() < crate::wall_tool::MIN_LEN {
        ed.status = Some(a.t("tool.err.short"));
        return;
    }
    if a.pts.len() == 1 {
        match first_flight(a, p0, q) {
            Ok(_) => a.pts.push(q),
            Err(m) => ed.status = Some(m),
        }
        return;
    }
    let c = a.pts[1];
    let r = multi_spec(a, p0, c, q).map(|s| record(a, s));
    if commit(ed, a, r) {
        a.pts.clear();
    }
}
