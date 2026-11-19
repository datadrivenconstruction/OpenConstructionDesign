// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D bodies of shafts and zones. A shaft is a translucent prism from its base to its top, drawn
//! once at its base level. A zone has no volume: a small pad marks it under its first room, at
//! the same point as its plan marker.

use super::pick::{elev, prism, Cat, Target};
use super::scene3d::Part;
use crate::editor::structx::code_of;
use crate::editor::Editor;

const SHAFT_RGB: [u8; 3] = [0x5f, 0x9e, 0xc9];
const ZONE_RGB: [u8; 3] = [0xd9, 0x8a, 0x2b];
/// Thickness of the zone pad, m.
const PAD: f64 = 0.05;

pub(super) fn shafts(ed: &Editor, level: i64) -> Vec<Part> {
    let Some(code) = code_of("shaft") else {
        return vec![];
    };
    let m = &ed.model;
    let ids: Vec<i64> = m
        .sql()
        .prepare("SELECT id FROM shaft WHERE level=?1 ORDER BY id")
        .and_then(|mut st| st.query_map([level], |r| r.get(0))?.collect())
        .unwrap_or_default();
    let mut out = vec![];
    for id in ids {
        let Ok(s) = m.shaft(id) else { continue };
        let z0 = elev(m, s.level) + s.base_offset;
        let z1 = elev(m, s.top_level) + s.top_offset;
        let Some(mesh) = prism(&[s.ring], z0, z1 - z0) else {
            continue;
        };
        let tag = ddc_render::tag_rgb(SHAFT_RGB, true);
        let mut p = Part::new(Target::new(Cat::Ext(code), id), tag);
        p.push(&mesh, ed.elevation);
        if !p.is_empty() {
            out.push(p);
        }
    }
    out
}

pub(super) fn zones(ed: &Editor, level: i64) -> Vec<Part> {
    let Some(code) = code_of("zone") else {
        return vec![];
    };
    let m = &ed.model;
    let h = crate::tools::ZONE_MARK;
    let z0 = elev(m, level);
    let mut out = vec![];
    for (id, z) in m.zones().unwrap_or_default() {
        let Some((x, y)) = crate::tools::zone_anchor(m, &z, level) else {
            continue;
        };
        let ring = vec![(x - h, y - h), (x + h, y - h), (x + h, y + h), (x - h, y + h)];
        let Some(mesh) = prism(&[ring], z0, PAD) else {
            continue;
        };
        let mut p = Part::new(
            Target::new(Cat::Ext(code), id),
            ddc_render::tag_rgb(ZONE_RGB, false),
        );
        p.push(&mesh, ed.elevation);
        if !p.is_empty() {
            out.push(p);
        }
    }
    out
}
