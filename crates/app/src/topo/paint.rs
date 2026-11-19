//! Plan: contours (thin base lines, thickened every `major` with labels), terrain boundary,
//! subregion fill, building pads and zero-work lines, points in edit mode, tool rubber band,
//! north arrow. The built terrain is cached by editor revision.

use super::{Tool, MATERIALS};
use crate::editor::Editor;
use crate::ui_kit::Tokens;
use ddc_geom::topo::{label_spots, Contour};
use ddc_geom::{v2, V2};
use ddc_model::{Model, PadReport, TopoBuilt};
use egui::{self, Color32, Pos2, Rect, Shape, Stroke};
use std::sync::Arc;

/// Label spacing along a thickened contour, m.
const LABEL_SPACING: f64 = 30.0;
/// Cap on the number of contour levels: for a larger elevation range the interval is coarsened.
const MAX_LEVELS: f64 = 400.0;

/// Built terrain for the plan and the properties window.
#[derive(Default)]
pub struct Item {
    pub id: i64,
    pub name: String,
    pub contours: Vec<Contour>,
    /// (point, text angle, elevation label).
    pub labels: Vec<(V2, f64, String)>,
    pub outline: Vec<Vec<V2>>,
    /// Subregion triangles with their material.
    pub fills: Vec<([V2; 3], String)>,
    /// Building pads: (id, name, outline).
    pub pads: Vec<(i64, String, Vec<V2>)>,
    pub daylight: Vec<Vec<V2>>,
    pub reports: Vec<PadReport>,
    pub points: Vec<(i64, (f64, f64, f64))>,
    /// Contour interval they were built with, m.
    pub interval: f64,
    pub error: Option<String>,
    /// Skipped pads and subregions (not built).
    pub warnings: Vec<String>,
    /// Built surface (shared with 3D and the model): contours at a new interval need no rebuild.
    pub built: Option<Arc<TopoBuilt>>,
}

pub struct Cache {
    pub rev: (u64, u64),
    /// (contour interval as bits, every major-th).
    pub key: (u64, u32),
    pub north: f64,
    pub items: Vec<Item>,
}

fn xy(p: ddc_geom::V3) -> V2 {
    v2(p.x, p.y)
}

fn build_item(m: &Model, id: i64, name: String, interval: f64, major: u32) -> Item {
    let mut it = Item {
        id,
        name,
        points: m.topo_points(id).unwrap_or_default(),
        ..Default::default()
    };
    it.pads = m
        .pads(id)
        .unwrap_or_default()
        .into_iter()
        .map(|(pid, s)| {
            (
                pid,
                s.name,
                s.outline.iter().map(|&(x, y)| v2(x, y)).collect(),
            )
        })
        .collect();
    let b = match m.topo_built_shared(id) {
        Ok(b) => b,
        Err(e) => {
            it.error = Some(e.to_string());
            return it;
        }
    };
    it.warnings = b.warnings.clone();
    it.built = Some(b.clone());
    contour_item(&mut it, interval, major);
    let tin = &b.tin;
    it.outline = tin
        .boundary_loops()
        .iter()
        .map(|l| l.iter().map(|&i| xy(tin.pts[i as usize])).collect())
        .collect();
    for (k, tr) in tin.tris.iter().enumerate() {
        let Some(mat) = b.material.get(k) else {
            continue;
        };
        if MATERIALS.contains(&mat.as_str()) {
            let p = |i: u32| xy(tin.pts[i as usize]);
            it.fills.push(([p(tr[0]), p(tr[1]), p(tr[2])], mat.clone()));
        }
    }
    for r in &b.pads {
        it.daylight.extend(
            r.daylight
                .iter()
                .map(|d| d.iter().map(|&(x, y)| v2(x, y)).collect::<Vec<_>>()),
        );
    }
    it.reports = b.pads.clone();
    it
}

/// Contours and labels from the element's built surface.
fn contour_item(it: &mut Item, interval: f64, major: u32) {
    it.contours.clear();
    it.labels.clear();
    let Some(b) = it.built.clone() else {
        return;
    };
    let tin = &b.tin;
    let mut step = interval;
    if let Some((lo, hi)) = tin.bounds() {
        if step > 0.0 && (hi.z - lo.z) / step > MAX_LEVELS {
            step *= ((hi.z - lo.z) / step / MAX_LEVELS).ceil();
        }
    }
    it.interval = step;
    if step > 0.0 {
        it.contours = tin.contours(step, major.max(1));
    }
    for c in it.contours.iter().filter(|c| c.major) {
        for (p, a) in label_spots(c, LABEL_SPACING) {
            it.labels.push((p, a, crate::units::fmt_num(c.z, 2)));
        }
    }
}

/// Rebuild the cache if the model or contour interval changed.
pub fn refresh(ed: &mut Editor) {
    let o = &ed.topo.opts;
    let key = (o.interval.to_bits(), o.major);
    let (interval, major) = (o.interval, o.major);
    let rev = ed.cache_key();
    if let Some(c) = ed.topo.cache.as_mut().filter(|c| c.rev == rev) {
        // only the contour interval changed: the surface is not rebuilt
        if c.key != key {
            c.key = key;
            for it in &mut c.items {
                contour_item(it, interval, major);
            }
        }
        return;
    }
    let list = ed.model.toposolids().unwrap_or_default();
    let items = list
        .into_iter()
        .map(|(id, name)| build_item(&ed.model, id, name, interval, major))
        .collect();
    let north = ed.model.site().map(|s| s.north_deg).unwrap_or(0.0);
    ed.topo.cache = Some(Cache {
        rev,
        key,
        north,
        items,
    });
}

/// Fill colour of a subregion material (plan; transparency set by a token).
pub fn mat_color(sem: &ddc_ui::sem::Sem, m: &str) -> Color32 {
    match m {
        "grass" => sem.canvas_mat_grass,
        "asphalt" => sem.canvas_mat_asphalt,
        "gravel" => sem.canvas_mat_gravel,
        "paving" => sem.canvas_mat_paving,
        "water" => sem.canvas_mat_water,
        _ => sem.canvas_mat_soil,
    }
}

fn polyline(p: &egui::Painter, pts: &[Pos2], closed: bool, st: Stroke) {
    if pts.len() < 2 {
        return;
    }
    if closed {
        p.add(Shape::closed_line(pts.to_vec(), st));
    } else {
        p.add(Shape::line(pts.to_vec(), st));
    }
}

fn label(p: &egui::Painter, at: Pos2, world_angle: f64, text: &str, col: Color32) {
    let font = ddc_ui::typo::font(ddc_ui::theme::Role::CanvasTag);
    let g = p.layout_no_wrap(text.to_string(), font, col);
    let ang = -(world_angle as f32);
    let (h, (sn, cs)) = (g.size() * 0.5, ang.sin_cos());
    let off = egui::vec2(-h.x * cs + h.y * sn, -h.x * sn - h.y * cs);
    p.add(egui::epaint::TextShape::new(at + off, g, col).with_angle(ang));
}

/// Draw the terrain and the tool in plan (called from `PlanView::show` before walls).
pub fn paint(
    p: &egui::Painter,
    rect: Rect,
    ed: &mut Editor,
    t: &Tokens,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    refresh(ed);
    let north_txt = ed.topo.t("topo.north");
    let act = super::active(ed);
    let s = &ed.topo;
    let Some(c) = &s.cache else {
        return;
    };
    let minor = Stroke::new(0.8, t.sem().canvas_contour_minor);
    let major = Stroke::new(1.5, t.sem().canvas_contour_major);
    let edge = Stroke::new(1.3, t.text_dim);
    for it in &c.items {
        if !it.fills.is_empty() {
            let mut mesh = egui::Mesh::default();
            for (tri, mat) in &it.fills {
                let col = mat_color(t.sem(), mat);
                let base = mesh.vertices.len() as u32;
                for q in tri {
                    mesh.colored_vertex(to_s(*q), col);
                }
                mesh.add_triangle(base, base + 1, base + 2);
            }
            p.add(Shape::mesh(mesh));
        }
        if !s.hide_contours {
            for cn in &it.contours {
                let pts: Vec<Pos2> = cn.pts.iter().map(|q| to_s(v2(q.x, q.y))).collect();
                polyline(p, &pts, cn.closed, if cn.major { major } else { minor });
            }
            for (q, a, txt) in &it.labels {
                let at = to_s(*q);
                if rect.contains(at) {
                    label(p, at, *a, txt, major.color);
                }
            }
        }
        for o in &it.outline {
            let pts: Vec<Pos2> = o.iter().map(|q| to_s(*q)).collect();
            polyline(p, &pts, true, edge);
        }
        let pad_st = Stroke::new(1.6, t.text);
        for (_, _, o) in &it.pads {
            let pts: Vec<Pos2> = o.iter().map(|q| to_s(*q)).collect();
            polyline(p, &pts, true, pad_st);
        }
        let day = Stroke::new(1.0, t.text_dim);
        for d in &it.daylight {
            let mut pts: Vec<Pos2> = d.iter().map(|q| to_s(*q)).collect();
            if let Some(&f) = pts.first() {
                pts.push(f);
            }
            p.extend(Shape::dashed_line(&pts, day, 6.0, 4.0));
        }
        let editing = matches!(s.tool, Some(Tool::Edit | Tool::Points)) && act == Some(it.id);
        if editing {
            for (i, q) in &it.points {
                let at = to_s(v2(q.0, q.1));
                if !rect.expand(8.0).contains(at) {
                    continue;
                }
                let picked = s.picked.contains(i);
                let r = egui::Rect::from_center_size(at, egui::vec2(6.0, 6.0));
                if picked {
                    p.rect_filled(r, 0.0, t.accent);
                } else {
                    p.rect_stroke(r, 0.0, Stroke::new(1.0, t.text), egui::StrokeKind::Middle);
                }
                if s.hover == Some(*i) {
                    p.circle_stroke(at, 7.0, Stroke::new(1.2, t.accent));
                    p.text(
                        at + egui::vec2(8.0, -8.0),
                        egui::Align2::LEFT_BOTTOM,
                        crate::units::fmt_num(q.2, 2),
                        egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
                        t.text,
                    );
                }
            }
        }
    }
    // point drag: previous and new position
    if let Some(d) = &s.drag {
        let a = to_s(v2(d.from.0, d.from.1));
        let b = to_s(v2(d.from.0, d.from.1) + (d.to - d.start));
        p.line_segment([a, b], Stroke::new(1.0, t.accent));
        p.circle_filled(b, 4.0, t.accent);
    }
    // tool rubber band
    if let Some(tool) = s.tool.filter(|k| *k != Tool::Edit) {
        let st = Stroke::new(1.4, t.accent);
        let mut pts: Vec<Pos2> = s.pts.iter().map(|q| to_s(q.0)).collect();
        if let Some(cur) = s.cur {
            pts.push(to_s(cur));
        }
        if tool == Tool::Points {
            for (q, z) in &s.pts {
                let at = to_s(*q);
                p.circle_filled(at, 3.5, t.accent);
                p.text(
                    at + egui::vec2(6.0, -6.0),
                    egui::Align2::LEFT_BOTTOM,
                    crate::units::fmt_num(*z, 2),
                    egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
                    t.text,
                );
            }
        } else {
            polyline(p, &pts, false, st);
            if s.pts.len() >= 2 {
                if let (Some(&f), Some(&l)) = (pts.first(), pts.last()) {
                    p.extend(Shape::dashed_line(&[l, f], st, 5.0, 4.0));
                }
            }
        }
    }
    if let (Some(cur), false) = (s.cur, s.input.is_empty()) {
        if s.tool.is_some() {
            p.text(
                to_s(cur) + egui::vec2(12.0, 12.0),
                egui::Align2::LEFT_TOP,
                &s.input,
                egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
                t.accent,
            );
        }
    }
    if !c.items.is_empty() || c.north != 0.0 {
        north_arrow(p, rect, c.north, &north_txt, t);
    }
}

/// True-north arrow in the top-right corner (angle from the project's +Y, counter-clockwise).
fn north_arrow(p: &egui::Painter, rect: Rect, north_deg: f64, txt: &str, t: &Tokens) {
    let o = rect.right_top() + egui::vec2(-34.0, 46.0);
    let a = north_deg.to_radians();
    let d = egui::vec2(-(a.sin() as f32), -(a.cos() as f32));
    let n = egui::vec2(-d.y, d.x);
    let (tip, tail) = (o + d * 18.0, o - d * 14.0);
    let st = Stroke::new(1.4, t.text);
    p.line_segment([tail, tip], st);
    p.add(Shape::convex_polygon(
        vec![tip, o + n * 6.0, o - n * 6.0],
        t.text,
        Stroke::NONE,
    ));
    p.text(
        tip + d * 9.0,
        egui::Align2::CENTER_CENTER,
        txt,
        egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
        t.text,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::sem::sem;
    use ddc_ui::theme::{Mode, Tokens as UiTokens};

    const MATS: [&str; 6] = ["grass", "asphalt", "gravel", "paving", "water", "soil"];

    #[test]
    fn topo_mat_colors_from_tokens_both_themes() {
        for dark in [false, true] {
            let s = sem(dark);
            assert_eq!(mat_color(s, "grass"), s.canvas_mat_grass);
            assert_eq!(mat_color(s, "water"), s.canvas_mat_water);
            assert_eq!(mat_color(s, "unknown"), s.canvas_mat_soil);
        }
        assert_ne!(mat_color(sem(false), "grass"), mat_color(sem(true), "grass"));
    }

    #[test]
    fn topo_fill_distinct_from_paper() {
        for dark in [false, true] {
            let s = sem(dark);
            let mode = if dark { Mode::Dark } else { Mode::Light };
            let paper = UiTokens::embedded().unwrap().color(mode, "canvas.paper").unwrap();
            for m in MATS {
                let c = mat_color(s, m);
                assert!(c.a() > 0 && c.a() < 255, "{m} must stay translucent");
                let f = ddc_ui::theme::over_rgba(c, paper);
                let d = (f.r() as i32 - paper.r() as i32).abs()
                    + (f.g() as i32 - paper.g() as i32).abs()
                    + (f.b() as i32 - paper.b() as i32).abs();
                assert!(d >= 30, "{dark} {m} fill too close to paper: {d}");
            }
        }
    }
}
