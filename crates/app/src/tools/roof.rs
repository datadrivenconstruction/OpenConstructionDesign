// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roof by footprint (RF): polyline sketch, each edge has a "defines slope" flag (Space
//! toggles it on the last edge), Enter or a click on the first point finishes.

use super::{ccw, ensure_slab_type, floor, tup, Active, Opts, Rec};
use crate::editor::Editor;
use crate::sketch_mode::walls_error;
use crate::snap::closest_on_segment;
use ddc_geom::sketch::{walls_loop, WallsLoopError};
use ddc_geom::V2;
use ddc_model::{RoofKind, RoofSpec, SKETCH_SAG};
use ddc_ui::i18n::Catalog;

const MODES: [&str; 3] = ["core_tools.mode.contour", "core_tools.mode.pick", "core_tools.mode.walls"];

/// Build mode combo (sketch, click inside a wall loop, pick walls); the overhang is not part
/// of the footprint, the roof applies it itself.
pub fn mode_combo(ui: &mut egui::Ui, cat: &Catalog, o: &mut Opts) {
    let mode = if o.walls {
        2
    } else if o.pick {
        1
    } else {
        0
    };
    let mut next = mode;
    let items: Vec<String> = MODES.iter().map(|k| cat.t(k)).collect();
    super::opt_field::pick(ui, "roof_mode", &mut next, &items, false);
    if next != mode {
        o.pick = next == 1;
        o.walls = next == 2;
    }
}

/// A picking mode (walls, inside a loop) takes clicks itself: the sketch shape goes back to
/// points, as `loop_shapes::set_shape` turns picking off for a shape.
pub fn mode_switched(o: &mut Opts) {
    if o.pick || o.walls {
        o.shape = super::loop_shapes::SketchShape::Points;
    }
}

fn half_thickness(ed: &Editor, wall_type: i64) -> f64 {
    ed.types
        .iter()
        .find(|t| t.id == wall_type)
        .map_or(0.05, |t| t.thickness() / 2.0)
}

/// Outer faces of the picked walls (thickest picked wall sets the offset from the axes).
pub fn walls_ring(ed: &Editor, a: &Active) -> Result<Vec<V2>, WallsLoopError> {
    let picked: Vec<_> = a
        .picked
        .iter()
        .filter_map(|id| ed.walls.iter().find(|w| w.id == *id))
        .collect();
    let axes: Vec<(V2, V2)> = picked.iter().map(|w| (w.a, w.b)).collect();
    let half = picked
        .iter()
        .map(|w| half_thickness(ed, w.wall_type))
        .fold(0.0, f64::max);
    let curves = walls_loop(&axes, half)?;
    Ok(curves
        .iter()
        .flat_map(|c| c.tessellate(SKETCH_SAG))
        .collect())
}

pub fn from_walls(ed: &Editor, a: &Active) -> Result<Rec, String> {
    let wall = |i: usize| a.picked.get(i).copied().unwrap_or(0);
    let ring = walls_ring(ed, a).map_err(|e| walls_error(e, &wall))?;
    rec(a, &ring)
}

/// Inner-face ring of a wall region moved out to the outer faces of its walls, edge by edge.
fn outset(ed: &Editor, inner: &[V2]) -> Vec<V2> {
    let mut r = inner.to_vec();
    ccw(&mut r);
    let n = r.len();
    if n < 3 {
        return r;
    }
    let lines: Vec<(V2, V2)> = (0..n)
        .map(|i| {
            let (p, q) = (r[i], r[(i + 1) % n]);
            let d = q - p;
            let l = d.len().max(1e-12);
            let (dir, out) = (d * (1.0 / l), ddc_geom::v2(d.y / l, -d.x / l));
            let mid = (p + q) * 0.5;
            let off = ed
                .walls
                .iter()
                .filter(|w| w.bulge == 0.0)
                .filter_map(|w| {
                    let wd = w.b - w.a;
                    let wl = wd.len();
                    if wl < 1e-9 || (wd * (1.0 / wl)).cross(dir).abs() > 1e-3 {
                        return None;
                    }
                    let half = half_thickness(ed, w.wall_type);
                    let dist = (closest_on_segment(mid, w.a, w.b).0 - mid).len();
                    (dist <= 2.0 * half + 0.05).then_some((dist, dist + half))
                })
                .min_by(|x, y| x.0.total_cmp(&y.0))
                .map_or(0.0, |x| x.1);
            (p + out * off, dir)
        })
        .collect();
    (0..n)
        .map(|i| {
            let (p1, d1) = lines[(i + n - 1) % n];
            let (p2, d2) = lines[i];
            let den = d1.cross(d2);
            if den.abs() < 1e-12 {
                p2
            } else {
                p1 + d1 * ((p2 - p1).cross(d2) / den)
            }
        })
        .collect()
}

/// Footprint under the cursor: wall loop moved to the outer faces, else a closed drawing loop.
pub fn footprint_at(ed: &Editor, q: V2) -> Option<Vec<V2>> {
    match floor::region_at(ed, q) {
        Some(r) => Some(outset(ed, &r.outer)),
        None => super::drawing_loop::region(&ed.tool.underlay_segs, q)
            .ok()
            .map(|r| r.outer),
    }
}

/// Click inside a closed wall loop.
pub fn pick(ed: &Editor, a: &Active, q: V2) -> Result<Rec, String> {
    let ring = match floor::region_at(ed, q) {
        Some(r) => outset(ed, &r.outer),
        None => {
            super::drawing_loop::region(&ed.tool.underlay_segs, q)
                .map_err(|e| a.t(e.key()))?
                .outer
        }
    };
    rec(a, &ring)
}

/// Edge of `ring` closest to `p`.
fn hovered_edge(ring: &[V2], p: V2) -> Option<usize> {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let d = (closest_on_segment(p, ring[i], ring[(i + 1) % n]).0 - p).len();
            (d, i)
        })
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map(|x| x.1)
}

/// Space in pick/walls modes: flip the slope of the edge under the cursor.
pub fn toggle_hovered(a: &mut Active) {
    let n = a.region.len();
    let Some(i) = hovered_edge(&a.region, a.cur).filter(|_| n >= 3) else {
        return;
    };
    let mut f = flags(a, n);
    f[i] = !f[i];
    a.slopes = f;
}

/// Slope flags per outline edge (edge i: vertex i → i+1).
pub fn flags(a: &Active, n: usize) -> Vec<bool> {
    (0..n)
        .map(|i| a.slopes.get(i).copied().unwrap_or(true))
        .collect()
}

pub fn spec(a: &Active, pts: &[V2]) -> RoofSpec {
    let mut ring = pts.to_vec();
    let mut fl = flags(a, ring.len());
    let before = ring.clone();
    ccw(&mut ring);
    if ring != before {
        // winding is reversed: edge i (i→i+1) becomes edge n−2−i
        let n = fl.len();
        let old = fl.clone();
        for (i, f) in fl.iter_mut().enumerate() {
            *f = old[(2 * n - 2 - i) % n];
        }
    }
    RoofSpec {
        level: a.level,
        slab_type: 0,
        offset: a.opts.base_off,
        overhang: a.opts.overhang,
        fascia: 0.2,
        kind: RoofKind::Footprint {
            ring: ring.iter().map(|p| tup(*p)).collect(),
            slopes: fl.iter().map(|f| f.then_some(a.opts.slope_deg)).collect(),
            mansard: None,
        },
    }
}

pub fn rec(a: &Active, pts: &[V2]) -> Result<Rec, String> {
    let s = spec(a, pts);
    let ty = a.type_name().to_string();
    Ok(Rec::new("roof", move |m| {
        let (t, n) = ensure_slab_type(m, &ty)?;
        m.add_roof(&RoofSpec {
            slab_type: t,
            ..s.clone()
        })?;
        Ok(n + 1)
    }))
}
