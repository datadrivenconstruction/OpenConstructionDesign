//! The work plane in the views other than 3D: its trace on the plan level and on a section or
//! elevation, dashed in the accent colour with the plane's name, and plan snaps along it
//! (`snap_svc`). Only a plane the user set is shown; the default plane is the level itself.

use crate::edit3d::plane::Shown;
use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::ui_kit::Tokens;
use ddc_geom::{v2, V2};
use ddc_model::SectionSpec;
use eframe::egui::{self, Pos2, Rect, Stroke};

/// Dash and gap of the trace, points.
const DASH: (f32, f32) = (10.0, 5.0);

/// Keeps `ed.work_plane` equal to the 3D editor's plane; the name is read again after a model
/// edit (a renamed level or reference plane).
pub fn sync(a: &mut EditorApp) {
    let (rev, source) = (a.ed.revision, a.view.e3.plane.source);
    // a plane on a level, a grid axis or a saved plane moves with it after a model edit
    let edited = a.ed.work_plane.as_ref().is_none_or(|s| s.rev != rev);
    if edited && a.view.e3.world_plane().is_some() {
        if let Some(w) = crate::edit3d::wp_win::resolve(&a.ed, source) {
            a.view.e3.set_world_plane(&w, source, a.ed.elevation);
        }
    }
    let Some(plane) = a.view.e3.world_plane() else {
        a.ed.work_plane = None;
        return;
    };
    let same = |s: &Shown| s.plane == plane && s.rev == rev && s.source == source;
    if a.ed.work_plane.as_ref().is_some_and(same) {
        return;
    }
    let name = crate::edit3d::wp_win::label(&a.view.e3, &a.ed, &a.sh.cat);
    a.ed.work_plane = Some(Shown { plane, name, source, rev });
}

/// Trace segment on the plan of the view level inside the box `lo`..`hi`.
pub fn plan_segment(ed: &Editor, lo: V2, hi: V2) -> Option<(V2, V2)> {
    ed.work_plane.as_ref()?.plane.plan_trace(ed.elevation, lo, hi)
}

/// Plan snap line of the work plane near `p`: the trace inside `reach` of it.
pub fn snap_line(ed: &Editor, p: V2, reach: f64) -> Option<(V2, V2)> {
    let r = v2(reach, reach);
    plan_segment(ed, p - r, p + r)
}

/// Trace segment on section `s` inside the box `lo`..`hi` of section coordinates.
pub fn section_segment(ed: &Editor, s: &SectionSpec, lo: V2, hi: V2) -> Option<(V2, V2)> {
    let a = v2(s.a.0, s.a.1);
    let b = v2(s.b.0, s.b.1);
    ed.work_plane.as_ref()?.plane.section_trace(a, b, lo, hi)
}

fn paint(p: &egui::Painter, a: Pos2, b: Pos2, name: &str, t: &Tokens) {
    let st = Stroke::new(1.5, t.accent);
    p.extend(egui::Shape::dashed_line(&[a, b], st, DASH.0, DASH.1));
    // the name sits at the visible end nearer the top left, inside the view
    let at = if (a.x, a.y) <= (b.x, b.y) { a } else { b };
    let clip = p.clip_rect().shrink(4.0);
    let font = egui::FontId::proportional(12.0);
    let g = p.layout_no_wrap(name.to_string(), font, t.accent);
    let size = g.size();
    let mut pos = at + egui::vec2(6.0, -size.y - 4.0);
    pos.x = pos.x.clamp(clip.left(), (clip.right() - size.x).max(clip.left()));
    pos.y = pos.y.clamp(clip.top(), (clip.bottom() - size.y).max(clip.top()));
    let plate = Rect::from_min_size(pos, size).expand(2.0);
    p.rect_filled(plate, 2.0, t.paper.gamma_multiply(0.85));
    p.galley(pos, g, t.accent);
}

/// The trace on the plan; `to_s`/`to_w` map plan metres and screen points.
pub fn paint_plan(
    p: &egui::Painter,
    ed: &Editor,
    rect: Rect,
    to_s: &dyn Fn(V2) -> Pos2,
    to_w: &dyn Fn(Pos2) -> V2,
    t: &Tokens,
) {
    let Some(s) = &ed.work_plane else { return };
    let (c0, c1) = (to_w(rect.left_top()), to_w(rect.right_bottom()));
    let lo = v2(c0.x.min(c1.x), c0.y.min(c1.y));
    let hi = v2(c0.x.max(c1.x), c0.y.max(c1.y));
    if let Some((a, b)) = plan_segment(ed, lo, hi) {
        paint(p, to_s(a), to_s(b), &s.name, t);
    }
}

/// The trace on the read-only plan of `level` (a pane on a level that is not active);
/// `to_s`/`to_w` map plan metres and screen points.
pub fn paint_level(
    p: &egui::Painter,
    ed: &Editor,
    level: i64,
    rect: Rect,
    to_s: &dyn Fn((f64, f64)) -> Pos2,
    to_w: &dyn Fn(Pos2) -> V2,
    t: &Tokens,
) {
    let Some(s) = &ed.work_plane else { return };
    let Some(z) = ed.levels().into_iter().find(|l| l.0 == level).map(|l| l.2) else {
        return;
    };
    let (c0, c1) = (to_w(rect.left_top()), to_w(rect.right_bottom()));
    let lo = v2(c0.x.min(c1.x), c0.y.min(c1.y));
    let hi = v2(c0.x.max(c1.x), c0.y.max(c1.y));
    if let Some((a, b)) = s.plane.plan_trace(z, lo, hi) {
        paint(p, to_s((a.x, a.y)), to_s((b.x, b.y)), &s.name, t);
    }
}

/// The trace on section `id`; `to_s`/`to_w` map section coordinates (s, z) and screen points.
pub fn paint_section(
    p: &egui::Painter,
    ed: &Editor,
    id: i64,
    rect: Rect,
    to_s: &dyn Fn((f64, f64)) -> Pos2,
    to_w: &dyn Fn(Pos2) -> V2,
    t: &Tokens,
) {
    let (Some(s), Some(spec)) = (&ed.work_plane, ed.view_spec(id)) else {
        return;
    };
    let (c0, c1) = (to_w(rect.left_top()), to_w(rect.right_bottom()));
    let lo = v2(c0.x.min(c1.x), c0.y.min(c1.y));
    let hi = v2(c0.x.max(c1.x), c0.y.max(c1.y));
    if let Some((a, b)) = section_segment(ed, spec, lo, hi) {
        paint(p, to_s((a.x, a.y)), to_s((b.x, b.y)), &s.name, t);
    }
}

#[cfg(test)]
#[path = "wp_views_tests.rs"]
mod tests;
