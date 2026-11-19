// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Outline edges linked to walls (`ddc_model::EdgeLink`): «Pick walls» in sketch mode links
//! the edges it makes, «Whole loop» picks the closed run of walls under one click, «Unlink
//! edge» and «Re-pick» act on the edge under the cursor; floor commands unlink every edge or
//! accept edges whose wall was deleted. Lost edges are drawn in the warning colour on the plan.

use std::sync::Arc;

use ddc_geom::sketch::SketchCurve;
use ddc_geom::V2;
use ddc_model::{EdgeLink, LinkFace, LinkState, SketchHost};
use egui::{self, Pos2, Stroke};

use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::sketch_mode::{Draw, SketchMode};
use crate::ui_kit::Tokens;

const TOL: f64 = 1e-6;

pub fn register(app: &mut EditorApp) {
    app.register_command("sketch.link_loop", |a| {
        in_sketch(a, |m| {
            m.set_draw(Draw::PickWalls);
            m.loop_pick = true;
            m.repick = None;
            Ok("boundary_link.hint.loop")
        })
    });
    app.register_command("sketch.link_unlink", |a| in_sketch(a, unlink_hovered));
    app.register_command("sketch.link_repick", |a| in_sketch(a, arm_repick));
    app.register_command("ctx.floor.link_unlink", |a| floor(a, false));
    app.register_command("ctx.floor.link_accept", |a| floor(a, true));
}

fn in_sketch(a: &mut EditorApp, f: fn(&mut SketchMode) -> Result<&'static str, &'static str>) {
    let key = match a.ed.sketch.as_mut() {
        Some(m) => match f(m) {
            Ok(k) | Err(k) => k,
        },
        None => "boundary_link.need_sketch",
    };
    a.ed.status = Some(crate::tr(key, &[]));
}

fn hovered(m: &SketchMode) -> Result<usize, &'static str> {
    m.cursor
        .and_then(|c| m.curve_near(c))
        .ok_or("boundary_link.need_edge")
}

/// State of edge `i` as the sketch shows it now (picked links first, then the stored ones).
pub fn edge_state(m: &SketchMode, i: usize) -> Option<LinkState> {
    if m.links.iter().any(|l| l.edge == i) {
        return Some(LinkState::Linked);
    }
    if m.release.contains(&i) {
        return None;
    }
    m.linked.iter().find(|e| e.link.edge == i).map(|e| e.state)
}

fn unlink_hovered(m: &mut SketchMode) -> Result<&'static str, &'static str> {
    let i = hovered(m)?;
    if let Some(k) = m.links.iter().position(|l| l.edge == i) {
        m.links.remove(k);
        return Ok("boundary_link.unlinked");
    }
    if edge_state(m, i).is_some() {
        m.release.push(i);
        return Ok("boundary_link.unlinked");
    }
    Err("boundary_link.not_linked")
}

fn arm_repick(m: &mut SketchMode) -> Result<&'static str, &'static str> {
    let i = hovered(m)?;
    if !matches!(m.sketch.curves.get(i), Some(SketchCurve::Line { .. })) {
        return Err("boundary_link.need_straight");
    }
    m.set_draw(Draw::PickWalls);
    m.loop_pick = false;
    m.repick = Some(i);
    Ok("boundary_link.hint.repick")
}

/// Click of «Re-pick»: the armed edge follows the wall under `p` at its present distance.
pub fn repick(m: &mut SketchMode, p: V2, walls: &[(i64, V2, V2)]) -> Result<(), &'static str> {
    let Some(i) = m.repick.take() else {
        return Ok(());
    };
    let Some(&(id, ..)) = nearest(p, m.radius, walls) else {
        m.repick = Some(i);
        return Err("boundary_link.no_wall");
    };
    let Some(c) = m.sketch.curves.get(i).cloned() else {
        return Err("boundary_link.need_edge");
    };
    let l = links_for(&[c], i, &[id], walls);
    let Some(l) = l.first().copied() else {
        return Err("boundary_link.not_parallel");
    };
    m.links.retain(|k| k.edge != i);
    m.release.retain(|&k| k != i);
    m.links.push(l);
    Ok(())
}

/// Signed distance of `p` to the left of the axis a -> b.
fn left_dist(p: V2, a: V2, b: V2) -> Option<f64> {
    let d = b - a;
    let l = d.len();
    (l > TOL).then(|| d.cross(p - a) / l)
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 > 0.0 {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + d * t)).len()
}

fn nearest(p: V2, radius: f64, walls: &[(i64, V2, V2)]) -> Option<&(i64, V2, V2)> {
    walls
        .iter()
        .map(|w| (w, seg_dist(p, w.1, w.2)))
        .filter(|&(_, d)| d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(w, _)| w)
}

/// Links for `curves` (sketch indices from `base`) made from the `picked` walls: a straight
/// curve parallel to a picked axis follows the nearest such wall at its present distance.
pub fn links_for(
    curves: &[SketchCurve],
    base: usize,
    picked: &[i64],
    walls: &[(i64, V2, V2)],
) -> Vec<EdgeLink> {
    let mut out = vec![];
    for (k, c) in curves.iter().enumerate() {
        let &SketchCurve::Line { a, b } = c else {
            continue;
        };
        let hit = picked
            .iter()
            .filter_map(|id| walls.iter().find(|w| w.0 == *id))
            .filter_map(|&(id, wa, wb)| {
                let (da, db) = (left_dist(a, wa, wb)?, left_dist(b, wa, wb)?);
                ((da - db).abs() < TOL).then_some((id, (da + db) / 2.0))
            })
            .min_by(|x, y| x.1.abs().total_cmp(&y.1.abs()));
        if let Some((wall, offset)) = hit {
            out.push(EdgeLink {
                edge: base + k,
                wall,
                face: LinkFace::Axis,
                offset,
            });
        }
    }
    out
}

/// Straight walls joined end to end with the wall under `p`; walls with a loose end are left
/// out, so a closed run remains for «Pick walls».
pub fn loop_at(p: V2, radius: f64, walls: &[(i64, V2, V2)]) -> Vec<i64> {
    let Some(&first) = nearest(p, radius, walls) else {
        return vec![];
    };
    let meets = |q: V2, w: &(i64, V2, V2)| seg_dist(q, w.1, w.2) < 1e-3;
    let mut set = vec![first];
    let mut i = 0;
    while i < set.len() {
        let w = set[i];
        for o in walls {
            let joined = [o.1, o.2].iter().any(|&q| meets(q, &w))
                || [w.1, w.2].iter().any(|&q| meets(q, o));
            if joined && !set.iter().any(|s| s.0 == o.0) {
                set.push(*o);
            }
        }
        i += 1;
    }
    // a loose end touches no other wall of the run
    loop {
        let loose = set.iter().position(|w| {
            [w.1, w.2]
                .iter()
                .any(|&q| !set.iter().any(|o| o.0 != w.0 && meets(q, o)))
        });
        match loose {
            Some(k) => {
                set.remove(k);
            }
            None => break,
        }
    }
    set.iter().map(|w| w.0).collect()
}

fn floor(a: &mut EditorApp, accept: bool) {
    let ed = &mut a.ed;
    let Some(id) = ed.selection.iter().find_map(|s| match s {
        Sel::Slab(i) => Some(*i),
        _ => None,
    }) else {
        ed.status = Some(crate::tr("boundary_link.need_floor", &[]));
        return;
    };
    let host = SketchHost::Slab(id);
    let f = move |m: &mut ddc_model::Model| {
        let r = if accept {
            m.accept_lost_edges(host)
        } else {
            m.unlink_boundary_edges(host, &[])
        };
        r.map(|_| ())
    };
    let ok = if accept { "boundary_link.accepted" } else { "boundary_link.unlinked" };
    ed.status = Some(match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => crate::tr(ok, &[]),
        Err(e) => format!("{}: {e}", crate::tr("boundary_link.failed", &[])),
    });
}

/// Link marks in sketch mode: a short tick across a linked edge, warning colour and a note
/// on an edge whose wall was deleted or which cannot follow.
pub fn paint_links(p: &egui::Painter, m: &SketchMode, t: &Tokens, to_s: &dyn Fn(V2) -> Pos2) {
    for (i, c) in m.sketch.curves.iter().enumerate() {
        let &SketchCurve::Line { a, b } = c else {
            continue;
        };
        match edge_state(m, i) {
            Some(LinkState::Linked) => tick(p, a, b, t.accent, to_s),
            Some(st) => warn_edge(p, a, b, st, t, to_s),
            None => {}
        }
    }
}

/// Plan overlay outside sketch mode: edges of this level whose source needs attention.
pub fn paint_warnings(p: &egui::Painter, ed: &Editor, t: &Tokens, to_s: &dyn Fn(V2) -> Pos2) {
    if ed.sketch.is_some() {
        return;
    }
    let Ok(ws) = ed.model.boundary_warnings(ed.level) else {
        return;
    };
    for w in ws {
        warn_edge(p, w.a, w.b, w.state, t, to_s);
    }
}

fn tick(p: &egui::Painter, a: V2, b: V2, col: egui::Color32, to_s: &dyn Fn(V2) -> Pos2) {
    let (sa, sb) = (to_s(a), to_s(b));
    let d = sb - sa;
    let l = d.length();
    if l < 24.0 {
        return;
    }
    let mid = sa + d * 0.5;
    let n = egui::vec2(-d.y, d.x) * (5.0 / l);
    p.line_segment([mid - n, mid + n], Stroke::new(1.5, col));
}

fn warn_edge(
    p: &egui::Painter,
    a: V2,
    b: V2,
    st: LinkState,
    t: &Tokens,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let col = t.ov().warning;
    p.line_segment([to_s(a), to_s(b)], Stroke::new(3.0, col));
    let key = match st {
        LinkState::SourceLost => "boundary_link.lost",
        _ => "boundary_link.blocked",
    };
    let mid = (a + b) * 0.5;
    p.text(
        to_s(mid) + egui::vec2(0.0, -10.0),
        egui::Align2::CENTER_BOTTOM,
        crate::tr(key, &[]),
        egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
        col,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;

    fn room() -> Vec<(i64, V2, V2)> {
        let c = [v2(0.0, 0.0), v2(6.0, 0.0), v2(6.0, 4.0), v2(0.0, 4.0)];
        (0..4).map(|k| (k as i64 + 1, c[k], c[(k + 1) % 4])).collect()
    }

    #[test]
    fn picked_walls_link_each_edge_to_its_own_wall() {
        let walls = room();
        let axes: Vec<(V2, V2)> = walls.iter().map(|w| (w.1, w.2)).collect();
        let cs = ddc_geom::sketch::walls_loop(&axes, 0.1).unwrap();
        let ls = links_for(&cs, 3, &[1, 2, 3, 4], &walls);
        assert_eq!(ls.len(), 4);
        for l in &ls {
            assert!(l.edge >= 3 && l.face == LinkFace::Axis);
            // the near wall at the pick offset, never the parallel one across the room
            assert!((l.offset.abs() - 0.1).abs() < 1e-9, "{l:?}");
        }
        let mut w: Vec<i64> = ls.iter().map(|l| l.wall).collect();
        w.sort_unstable();
        assert_eq!(w, vec![1, 2, 3, 4]);
    }

    #[test]
    fn whole_loop_in_one_click_skips_a_loose_wall() {
        let mut walls = room();
        walls.push((9, v2(6.0, 2.0), v2(9.0, 2.0)));
        let mut got = loop_at(v2(3.0, 0.05), 0.2, &walls);
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3, 4]);
        assert!(loop_at(v2(3.0, 2.0), 0.2, &walls).is_empty());
    }

    #[test]
    fn repick_needs_a_parallel_wall_and_keeps_the_edge() {
        let walls = room();
        let mut m = SketchMode::from_ring(
            SketchHost::Slab(1),
            &[v2(0.0, 0.0), v2(6.0, 0.0), v2(6.0, 4.0), v2(0.0, 4.0)],
        );
        m.repick = Some(1);
        // wall 1 runs along x: not parallel to edge 1 (x = 6)
        assert_eq!(repick(&mut m, v2(3.0, 0.0), &walls), Err("boundary_link.not_parallel"));
        m.repick = Some(1);
        assert_eq!(repick(&mut m, v2(6.0, 2.0), &walls), Ok(()));
        assert_eq!(m.links.len(), 1);
        assert_eq!((m.links[0].edge, m.links[0].wall), (1, 2));
        assert_eq!(edge_state(&m, 1), Some(LinkState::Linked));
    }
}
