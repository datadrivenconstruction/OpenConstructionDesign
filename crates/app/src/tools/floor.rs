// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Floor and structural slab: outline by clicks, click inside a wall loop, wall selection,
//! sketch. Writing — type by name, slab, slope: all in one undo step.

use super::opt_field;
use super::{ccw, ensure_slab_type, tup, Active, Kind, Opts, Rec};
use crate::editor::Editor;
use crate::units::Units;
use crate::sketch_mode::{begin_slab, walls_error, NewSlab};
use crate::snap::closest_on_segment;
use crate::wall_tool::Mode;
use ddc_geom::sketch::{walls_loop, SketchCurve, WallsLoopError};
use ddc_geom::slope::Plane;
use ddc_geom::V2;
use ddc_model::{SlabSpec, SKETCH_SAG};
use ddc_ui::i18n::Catalog;
use egui::{self, Pos2, Stroke};

const MODES: [&str; 3] = ["core_tools.mode.contour", "core_tools.mode.pick", "core_tools.mode.walls"];

/// Standard view types and types already defined in the model.
pub fn types(ed: &Editor, kind: Kind) -> Vec<String> {
    let base = crate::tr(
        if kind == Kind::StructSlab {
            "core_tools.type.struct_slab"
        } else {
            "core_tools.type.floor"
        },
        &[],
    );
    let mut out: Vec<String> = [200, 250, 300]
        .iter()
        .map(|mm| format!("{base} {mm}"))
        .collect();
    if let Ok(mut st) = ed
        .model
        .sql()
        .prepare("SELECT name FROM slab_type ORDER BY id")
    {
        let names: Vec<String> = st
            .query_map([], |r| r.get(0))
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        for n in names {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// Slope gradient from options: fraction per metre along direction `rot_deg`.
fn gradient(o: &Opts) -> Option<(f64, f64)> {
    if o.slope.abs() < 1e-9 {
        return None;
    }
    let (k, r) = (o.slope / 100.0, o.rot_deg.to_radians());
    Some((k * r.cos(), k * r.sin()))
}

fn slab_rec(a: &Active, mut outer: Vec<V2>, holes: Vec<Vec<V2>>) -> Rec {
    ccw(&mut outer);
    let plane = gradient(&a.opts).and_then(|(gx, gy)| Plane::lowest_on(&outer, gx, gy));
    let ring: Vec<(f64, f64)> = outer.iter().map(|p| tup(*p)).collect();
    let holes: Vec<Vec<(f64, f64)>> = holes
        .iter()
        .map(|h| h.iter().map(|p| tup(*p)).collect())
        .collect();
    let (level, offset, name) = (a.level, a.opts.base_off, a.type_name().to_string());
    let label = if a.kind == Kind::StructSlab {
        "struct_slab"
    } else {
        "floor"
    };
    Rec::new(label, move |m| {
        let (slab_type, n) = ensure_slab_type(m, &name)?;
        let spec = SlabSpec {
            level,
            slab_type,
            offset,
            outer: ring.clone(),
            holes: holes.clone(),
        };
        let id = match m.add_slab(&spec) {
            Ok(id) => id,
            Err(e) => {
                for _ in 0..n {
                    m.undo()?;
                }
                return Err(e);
            }
        };
        let Some(pl) = plane else {
            return Ok(n + 1);
        };
        if let Err(e) = m.set_slab_slope(id, Some(pl)) {
            for _ in 0..n + 1 {
                m.undo()?;
            }
            return Err(e);
        }
        Ok(n + 2)
    })
}

pub fn rec(a: &Active, pts: &[V2]) -> Result<Rec, String> {
    Ok(slab_rec(a, pts.to_vec(), vec![]))
}

/// Wall loop under the point: a room separator does not split the floor.
pub fn region_at(ed: &Editor, p: V2) -> Option<ddc_geom::rooms::Region> {
    ed.model
        .wall_regions(ed.level)
        .ok()?
        .into_iter()
        .find(|r| r.contains(p))
}

/// Click inside a closed wall loop: slab with its holes.
pub fn pick(ed: &Editor, a: &Active, q: V2) -> Result<Rec, String> {
    let r = match region_at(ed, q) {
        Some(r) => r,
        None => super::drawing_loop::region(&ed.tool.underlay_segs, q)
            .map_err(|e| a.t(e.key()))?,
    };
    let r = super::drawing_loop::tidy(r).map_err(|e| a.t(e.key()))?;
    Ok(slab_rec(a, r.outer, r.holes))
}

/// Nearest straight wall within snap radius: add to the outline or remove.
pub fn toggle_wall(ed: &mut Editor, a: &mut Active, q: V2) {
    let radius = ed.tool.radius.max(1e-3);
    let hit = ed
        .walls
        .iter()
        .filter(|w| w.bulge == 0.0)
        .map(|w| ((closest_on_segment(q, w.a, w.b).0 - q).len(), w.id))
        .filter(|h| h.0 <= radius)
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map(|h| h.1);
    let Some(id) = hit else {
        let arc = crate::modify_tools::wall_at(ed, q, radius)
            .is_some_and(|id| ed.walls.iter().any(|w| w.id == id && w.bulge != 0.0));
        ed.status = Some(a.t(if arc {
            "tool.err.pick_arc"
        } else {
            "tool.err.pick"
        }));
        return;
    };
    match a.picked.iter().position(|x| *x == id) {
        Some(i) => {
            a.picked.remove(i);
        }
        None => a.picked.push(id),
    }
}

/// Outline from the selected walls.
fn walls_curves(ed: &Editor, a: &Active) -> Result<Vec<SketchCurve>, WallsLoopError> {
    let axes: Vec<(V2, V2)> = a
        .picked
        .iter()
        .filter_map(|id| ed.walls.iter().find(|w| w.id == *id))
        .map(|w| (w.a, w.b))
        .collect();
    walls_loop(&axes, a.opts.overhang)
}

fn tessellated(curves: &[SketchCurve]) -> Vec<V2> {
    curves
        .iter()
        .flat_map(|c| c.tessellate(SKETCH_SAG))
        .collect()
}

/// Outline from the selected walls; None — the walls do not close.
pub fn walls_ring(ed: &Editor, a: &Active) -> Option<Vec<V2>> {
    walls_curves(ed, a).ok().map(|c| tessellated(&c))
}

pub fn from_walls(ed: &Editor, a: &Active) -> Result<Rec, String> {
    let wall = |i: usize| a.picked.get(i).copied().unwrap_or(0);
    let curves = walls_curves(ed, a).map_err(|e| walls_error(e, &wall))?;
    Ok(slab_rec(a, tessellated(&curves), vec![]))
}

/// Changing the build method discards work in progress: wall selection and the other method's points.
pub fn mode_changed(picked: &mut Vec<i64>, pts: &mut Vec<V2>) {
    picked.clear();
    pts.clear();
}

/// Highlight of the selected outline walls.
pub fn highlight_walls(
    p: &egui::Painter,
    ed: &Editor,
    a: &Active,
    st: Stroke,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    for id in &a.picked {
        if let Some(w) = ed.walls.iter().find(|w| w.id == *id) {
            p.line_segment(
                [to_s(w.a), to_s(w.b)],
                Stroke::new(st.width * 2.0, st.color),
            );
        }
    }
}

/// Option bar fields; true — "Sketch" was pressed.
pub fn options(ui: &mut egui::Ui, cat: &Catalog, o: &mut Opts, u: Units) -> bool {
    let mode = if o.walls {
        2
    } else if o.pick {
        1
    } else {
        0
    };
    let mut next = mode;
    let names: Vec<String> = MODES.iter().map(|k| cat.t(k)).collect();
    opt_field::pick(ui, "floor_mode", &mut next, &names, false);
    if next != mode {
        o.pick = next == 1;
        o.walls = next == 2;
    }
    opt_field::len(ui, &cat.t("opt.base_off"), &mut o.base_off, (-5.0, 50.0), u);
    opt_field::percent(ui, &cat.t("opt.slope_pct"), &mut o.slope, (0.0, 100.0));
    if o.slope > 0.0 {
        opt_field::angle(ui, &cat.t("opt.direction"), &mut o.rot_deg, opt_field::FULL_TURN);
    }
    if o.walls {
        opt_field::len(ui, &cat.t("core_tools.offset"), &mut o.overhang, (-2.0, 2.0), u);
    }
    ui.button(cat.t("core_tools.sketch")).clicked()
}

/// Switch to sketch: the tool is released and the outline is finished there.
pub fn open_sketch(ed: &mut Editor, a: &Active) {
    let slab = NewSlab {
        level: a.level,
        type_name: a.type_name().to_string(),
        offset: a.opts.base_off,
        slope: gradient(&a.opts),
    };
    ed.set_mode(Mode::Select);
    begin_slab(ed, slab);
}
