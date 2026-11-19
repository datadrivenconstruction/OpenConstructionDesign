// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawn 3D bodies of rooms and room separators. A room is a thin translucent pad over its
//! boundary with a post at its room point whose height is a fraction of the room height; the
//! full volume stays a pick-only body (`scene3d::rooms`). A separator is a low bar on its line.

use super::pick::{bar, elev, prism, Cat, Target};
use super::scene3d::Part;
use crate::editor::structx::code_of;
use crate::editor::Editor;
use ddc_render::glam::Vec3;

const ROOM_RGB: [u8; 3] = [0x4f, 0xa3, 0x6b];
const SEP_RGB: [u8; 3] = [0xd9, 0x5f, 0x3b];
/// Thickness of the pad and half width of the post, m.
const PAD: f64 = 0.01;
const POST: f64 = 0.06;
/// The post never grows past this, m.
const POST_MAX: f64 = 0.6;

pub(super) fn pads(ed: &Editor, level: i64) -> Vec<Part> {
    if level != ed.level {
        return vec![];
    }
    let m = &ed.model;
    let mut out = vec![];
    for r in &ed.rooms {
        let (at, h): ((f64, f64), f64) = m
            .sql()
            .query_row("SELECT px,py,height FROM room WHERE id=?1", [r.id], |x| {
                Ok(((x.get(0)?, x.get(1)?), x.get(2)?))
            })
            .unwrap_or(((0.0, 0.0), 2.7));
        let rings: Vec<Vec<(f64, f64)>> = r
            .rings
            .iter()
            .map(|ring| ring.iter().map(|p| (p.x, p.y)).collect())
            .collect();
        let mut p = Part::new(
            Target::new(Cat::Room, r.id),
            ddc_render::tag_rgb(ROOM_RGB, true),
        );
        if let Some(mesh) = prism(&rings, PAD, PAD) {
            p.push(&mesh, 0.0);
        }
        let post = vec![vec![
            (at.0 - POST, at.1 - POST),
            (at.0 + POST, at.1 - POST),
            (at.0 + POST, at.1 + POST),
            (at.0 - POST, at.1 + POST),
        ]];
        if let Some(mesh) = prism(&post, 0.0, (h * 0.2).min(POST_MAX)) {
            p.push(&mesh, 0.0);
        }
        if !p.is_empty() {
            out.push(p);
        }
    }
    out
}

pub(super) fn seps(ed: &Editor, level: i64) -> Vec<Part> {
    let Some(code) = code_of("room_sep") else {
        return vec![];
    };
    let m = &ed.model;
    let z = (elev(m, level) - ed.elevation) as f32 + 0.05;
    let mut out = vec![];
    for (id, s) in m.room_seps(level).unwrap_or_default() {
        let a = Vec3::new(s.a.0 as f32, s.a.1 as f32, z);
        let b = Vec3::new(s.b.0 as f32, s.b.1 as f32, z);
        let (pos, tris) = bar(a, b, 0.06, 0.1, Vec3::Z);
        let mut p = Part::new(
            Target::new(Cat::Ext(code), id),
            ddc_render::tag_rgb(SEP_RGB, true),
        );
        p.pos = pos;
        p.tris = tris;
        out.push(p);
    }
    out
}
