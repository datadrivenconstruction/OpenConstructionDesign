// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Detailed bodies for the built-in 3D providers, at the 3D view's level of detail: railings with
//! round handrails, posts and balusters; tread nosings; true member sections where the exact
//! joined body is too costly; roof edges (parapet copings, gutters, rain heads and downpipes).

use std::collections::HashMap;

use ddc_geom::detail::{rail, roof, section, stair, Detail, Lod, Mat};
use ddc_geom::mesh::Mesh;
use ddc_geom::roof::LineKind;
use ddc_geom::{v2, v3, V2, V3};
use ddc_render::glam::Vec3;

use super::pick::{ids, push_mesh, Cat, Target};
use super::scene3d::{Part, TAG_RAILING};
use crate::editor::opening_fill::{STEEL, STONE};
use crate::editor::Editor;

pub fn lod(ed: &Editor) -> Lod {
    crate::view_gfx_ui::lod_3d(&ed.model)
}

fn push(p: &mut Part, d: &Detail, z0: f64) {
    for (_, m) in &d.parts {
        p.push(m, z0);
    }
}

/// A railing drawn from its path and sizes; `None` when the railing does not check out.
pub fn railing(ed: &Editor, id: i64) -> Option<Part> {
    let rl = ed.model.railing_geom(id).ok()?;
    let d = rail::build(&rl, lod(ed));
    if d.parts.is_empty() {
        return None;
    }
    let mut p = Part::new(Target::new(Cat::Railing, id), TAG_RAILING);
    push(&mut p, &d, ed.elevation);
    Some(p)
}

/// Nosings of the stair's treads, added to its body.
pub fn stair_nosings(ed: &Editor, id: i64, p: &mut Part) {
    if let Ok(s) = ed.model.stair_layout(id) {
        push(p, &stair::nosings(&s, ed.detail.nosing, lod(ed)), ed.elevation);
    }
}

pub fn member_shapes(m: &ddc_model::Model) -> HashMap<i64, ddc_model::MemberShape> {
    m.member_types().unwrap_or_default().into_iter().map(|t| (t.0, t.2)).collect()
}

/// Straight member a → b (scene frame) with its true section, the section's depth along `up`.
pub fn section_bar(s: &ddc_model::MemberShape, a: Vec3, b: Vec3, up: Vec3, lod: Lod) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let f = |q: Vec3| v3(q.x as f64, q.y as f64, q.z as f64);
    let mut m = Mesh::default();
    section::bar(&mut m, s, f(a), f(b), f(up), lod);
    let (mut pos, mut tris) = (vec![], vec![]);
    push_mesh(&mut pos, &mut tris, &m, 0.0);
    (pos, tris)
}

/// A straight wall in plan with its absolute base and top and its thickness.
#[derive(Clone, Copy, Debug)]
struct WallLine {
    a: V2,
    b: V2,
    base: f64,
    top: f64,
    t: f64,
}

fn walls(m: &ddc_model::Model) -> Vec<WallLine> {
    let sql = "SELECT w.ax, w.ay, w.bx, w.by, l.elevation + w.base_offset, w.height, \
               COALESCE((SELECT SUM(thickness) FROM wall_type_layer t WHERE t.wtype=w.wtype), 0.2) \
               FROM wall w JOIN level l ON l.id=w.level WHERE COALESCE(w.bulge, 0)=0";
    m.sql()
        .prepare(sql)
        .and_then(|mut st| {
            st.query_map([], |r| {
                let base: f64 = r.get(4)?;
                Ok(WallLine {
                    a: v2(r.get(0)?, r.get(1)?),
                    b: v2(r.get(2)?, r.get(3)?),
                    base,
                    top: base + r.get::<_, f64>(5)?,
                    t: r.get(6)?,
                })
            })?
            .collect()
        })
        .unwrap_or_default()
}

/// Finished ground: the lowest level at or above zero.
fn ground(m: &ddc_model::Model) -> f64 {
    m.sql()
        .query_row("SELECT MIN(elevation) FROM level WHERE elevation > -0.01", [], |r| r.get::<_, Option<f64>>(0))
        .ok()
        .flatten()
        .unwrap_or(0.0)
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let ab = b - a;
    let t = if ab.dot(ab) < 1e-12 { 0.0 } else { ((p - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0) };
    (p - (a + ab * t)).len()
}

/// Edge details of one roof from its outline lines (absolute z), the walls and the ground:
/// copings on the parapets around it, rain heads and downpipes through them; gutters and
/// downpipes on the eaves of a roof without parapets.
fn edges_of(lines: &[(V3, V3, LineKind)], overhang: f64, walls: &[WallLine], taken: &mut [bool], ground: f64, r: &ddc_geom::detail::rules::DetailRules, lod: Lod) -> Detail {
    let mut d = Detail::default();
    if lines.is_empty() {
        return d;
    }
    let h = |p: V3| v2(p.x, p.y);
    let n = (2 * lines.len()) as f64;
    let c = lines.iter().fold(v2(0.0, 0.0), |s, l| s + h(l.0) + h(l.1)) * (1.0 / n);
    let top = lines.iter().map(|l| l.0.z.max(l.1.z)).fold(f64::MIN, f64::max);
    let outward = |a: V2, b: V2| {
        let dir = (b - a) * (1.0 / (b - a).len().max(1e-12));
        let nn = v2(dir.y, -dir.x);
        if nn.dot((a + b) * 0.5 - c) >= 0.0 {
            nn
        } else {
            -nn
        }
    };
    let outline: Vec<(V2, V2)> = lines
        .iter()
        .filter(|l| matches!(l.2, LineKind::Eave | LineKind::Rake))
        .map(|l| (h(l.0), h(l.1)))
        .collect();
    // a parapet: a wall along the outline that starts below the roof and stands above it
    let mut parapets: Vec<WallLine> = vec![];
    for (i, w) in walls.iter().enumerate() {
        if taken[i] || w.top < top + 0.15 || w.top > top + 3.0 || w.base > top - 0.5 || (w.b - w.a).len() < 0.05 {
            continue;
        }
        let near = |p: V2| outline.iter().any(|(a, b)| seg_dist(p, *a, *b) <= w.t / 2.0 + overhang + 0.05);
        if near(w.a) && near(w.b) {
            taken[i] = true;
            parapets.push(*w);
        }
    }
    for w in &parapets {
        let mid = (w.a + w.b) * 0.5;
        roof::coping(&mut d, w.a, w.b, w.top, w.t, r.coping_overhang, c - mid, lod);
    }
    if !parapets.is_empty() {
        let edges: Vec<(V2, V2, V2)> = parapets.iter().map(|w| (w.a, w.b, outward(w.a, w.b))).collect();
        for (p, o) in roof::spread(&edges, r.downpipe_spacing) {
            let t = parapets.iter().find(|w| seg_dist(p, w.a, w.b) < 1e-6).map_or(0.2, |w| w.t);
            let face = p + o * (t / 2.0);
            // the outlet runs through the parapet just above the roof surface
            let z = top + 0.1;
            roof::rain_head(&mut d, face, o, z, lod);
            let at = face + o * (roof::PIPE_OFF + roof::PIPE_R);
            roof::downpipe(&mut d, v3(at.x, at.y, z - 0.25), face, o, ground, lod);
        }
    } else {
        let eaves: Vec<(V3, V3, V2)> = lines
            .iter()
            .filter(|l| l.2 == LineKind::Eave)
            .map(|l| (l.0, l.1, outward(h(l.0), h(l.1))))
            .collect();
        let drop = v3(0.0, 0.0, -0.03);
        for (a, b, o) in &eaves {
            roof::gutter(&mut d, *a + drop, *b + drop, *o, lod);
        }
        let plan: Vec<(V2, V2, V2)> = eaves.iter().map(|(a, b, o)| (h(*a), h(*b), *o)).collect();
        for (p, o) in roof::spread(&plan, r.downpipe_spacing) {
            let Some((a, b, _)) = eaves.iter().find(|(a, b, _)| seg_dist(p, h(*a), h(*b)) < 1e-6) else {
                continue;
            };
            let l = (h(*b) - h(*a)).len().max(1e-9);
            let z = a.z + (b.z - a.z) * ((p - h(*a)).len() / l) - 0.03;
            let outlet = p + o * roof::GUTTER_R;
            // the wall face under the eave, the overhang in from the roof edge
            let face = p - o * overhang;
            roof::downpipe(&mut d, v3(outlet.x, outlet.y, z - roof::GUTTER_R), face, o, ground, lod);
        }
    }
    d
}

/// Edge details of the level's roofs, as parts of each roof: stone copings, metal drainage.
pub fn roof_edges(ed: &Editor, lv: i64) -> Vec<Part> {
    let m = &ed.model;
    let roofs = ids(m, "SELECT id FROM roof WHERE level=?1", lv);
    if roofs.is_empty() {
        return vec![];
    }
    let (walls, ground, lod) = (walls(m), ground(m), lod(ed));
    let mut taken = vec![false; walls.len()];
    let mut out = vec![];
    for id in roofs {
        let (Ok(g), Ok(spec)) = (m.roof_geometry(id), m.roof(id)) else { continue };
        let e = edges_of(&g.edges(), spec.overhang, &walls, &mut taken, ground, &ed.detail, lod);
        for (mat, mesh) in &e.parts {
            let rgb = if *mat == Mat::Stone { STONE } else { STEEL };
            let mut p = Part::new(Target::new(Cat::Roof, id), ddc_render::tag_rgb(rgb, false));
            p.push(mesh, ed.elevation);
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::detail::rules::DetailRules;

    fn ring(w: f64, d: f64, z: f64) -> Vec<(V3, V3, LineKind)> {
        let p = [v3(0.0, 0.0, z), v3(w, 0.0, z), v3(w, d, z), v3(0.0, d, z)];
        (0..4).map(|i| (p[i], p[(i + 1) % 4], LineKind::Eave)).collect()
    }

    fn copings(d: &Detail) -> u32 {
        d.parts.iter().filter(|p| p.0 == Mat::Stone).map(|p| p.1.face_of.last().map_or(0, |f| f + 1)).sum()
    }

    fn wall(a: V2, b: V2, base: f64, top: f64) -> WallLine {
        WallLine { a, b, base, top, t: 0.3 }
    }

    fn box_walls(base: f64, top: f64) -> Vec<WallLine> {
        let p = [v2(0.0, 0.0), v2(20.0, 0.0), v2(20.0, 10.0), v2(0.0, 10.0)];
        (0..4).map(|i| wall(p[i], p[(i + 1) % 4], base, top)).collect()
    }

    #[test]
    fn a_flat_roof_behind_parapets_gets_copings_rain_heads_and_downpipes() {
        let r = DetailRules::default();
        let mut walls = box_walls(6.0, 9.9);
        // an inner wall under the roof and a tower wall standing on it stay bare
        walls.push(wall(v2(10.0, 0.0), v2(10.0, 10.0), 6.0, 9.0));
        walls.push(wall(v2(0.0, 0.0), v2(5.0, 0.0), 9.3, 12.0));
        let mut taken = vec![false; walls.len()];
        for lod in [Lod::Coarse, Lod::Medium, Lod::Fine] {
            taken.iter_mut().for_each(|t| *t = false);
            let e = edges_of(&ring(20.0, 10.0, 9.3), 0.0, &walls, &mut taken, 0.0, &r, lod);
            assert_eq!(copings(&e), 4, "{lod:?}");
            assert!(e.is_closed());
            let metal = e.parts.iter().find(|p| p.0 == Mat::Metal).unwrap();
            let (lo, hi) = (metal.1.positions.iter().map(|p| p.z).fold(f64::MAX, f64::min), metal.1.positions.iter().map(|p| p.z).fold(f64::MIN, f64::max));
            assert!(lo < 0.3 && hi > 9.3, "{lod:?}: downpipes to the ground {lo} {hi}");
            // 60 m of parapet, a downpipe per 20 m: outside the walls
            assert!(metal.1.positions.iter().all(|p| p.x < -0.1 || p.x > 20.1 || p.y < -0.1 || p.y > 10.1 || p.z > 9.2));
        }
        // a wall already coped by a neighbour roof is not coped twice
        let e = edges_of(&ring(20.0, 10.0, 9.3), 0.0, &walls, &mut taken, 0.0, &r, Lod::Medium);
        assert_eq!(copings(&e), 0);
    }

    #[test]
    fn a_pitched_roof_gets_gutters_on_its_eaves_and_downpipes_per_rule_length() {
        let r = DetailRules::default();
        let mut lines = ring(16.0, 8.0, 6.0);
        lines.push((v3(4.0, 4.0, 8.0), v3(12.0, 4.0, 8.0), LineKind::Ridge));
        let walls = box_walls(3.0, 6.0);
        let mut taken = vec![false; walls.len()];
        let e = edges_of(&lines, 0.4, &walls, &mut taken, 0.0, &r, Lod::Medium);
        assert_eq!(copings(&e), 0);
        assert!(e.is_closed() && !e.is_plain_box());
        let few = DetailRules { downpipe_spacing: 40.0, ..r };
        let mut t2 = vec![false; walls.len()];
        let fewer = edges_of(&lines, 0.4, &walls, &mut t2, 0.0, &few, Lod::Medium);
        assert!(fewer.shells() < e.shells(), "fewer downpipes for a longer share");
    }

    #[test]
    fn member_sections_are_not_blocks() {
        let s = ddc_model::MemberShape::I { h: 0.3, b: 0.15, tw: 0.007, tf: 0.011 };
        let (pos, tris) = section_bar(&s, Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0), Vec3::X, Lod::Coarse);
        assert!(pos.len() > 8 && tris.len() > 12);
    }
}
