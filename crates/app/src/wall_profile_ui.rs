// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ctx.wall.profile` ("Edit profile"): the elevation outline of one wall. The outline is a
//! polygon in the wall's own frame (along the axis from its start, height above its base): drag
//! its points on the preview or type them, start from a rectangle, gable or slope. Apply writes
//! it as one undo step; "Remove profile" brings the plain rectangle back.

use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::status::Severity;
use crate::ui_kit as kit;
use ddc_geom::sketch::{ElevationProfile, SketchIssue};
use ddc_ui::theme::space;
use ddc_ui::widgets::Button;
use ddc_ui::Catalog;
use egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use std::cell::RefCell;
use std::sync::Arc;

/// Points snap to this step while dragged, m.
const SNAP: f64 = 0.01;
/// Pick radius of a point on the preview, points.
const PICK: f32 = 9.0;
/// Height of the preview, points.
const PREVIEW_H: f32 = 180.0;
/// Rows of points before the list scrolls.
const ROWS: usize = 6;

#[derive(Default)]
pub struct Win {
    pub open: bool,
    pub wall: i64,
    /// Axis length and height of the wall, m.
    pub len: f64,
    pub height: f64,
    pub pts: Vec<(f64, f64)>,
    /// The wall has a stored profile (Remove is on).
    pub had: bool,
    /// Rise of the gable and slope presets above the wall height, m.
    pub rise: f64,
    drag: Option<usize>,
    pub note: Option<String>,
}

thread_local! {
    static WIN: RefCell<Win> = RefCell::new(Win::default());
}

/// The editor for wall `id`: its stored outline, else its rectangle.
pub fn win_for(ed: &Editor, id: i64) -> Result<Win, String> {
    let w = ed.walls.iter().find(|w| w.id == id).ok_or("wprof.err.no_wall")?;
    let len = ed.model.wall_length(id).map_err(|e| e.to_string())?;
    let stored = ed.model.wall_profile(id).map_err(|e| e.to_string())?;
    let had = stored.is_some();
    let pts = stored.map_or_else(|| ElevationProfile::rectangle(len, w.height).outer, |p| p.outer);
    Ok(Win {
        open: true,
        wall: id,
        len,
        height: w.height,
        pts,
        had,
        rise: (w.height / 3.0 * 100.0).round() / 100.0,
        drag: None,
        note: None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Preset {
    Rect,
    Gable,
    Slope,
}

/// Outline of a preset for a wall `len` long and `h` high; the top rises by `rise`.
pub fn preset(p: Preset, len: f64, h: f64, rise: f64) -> Vec<(f64, f64)> {
    match p {
        Preset::Rect => ElevationProfile::rectangle(len, h).outer,
        Preset::Gable => vec![(0.0, 0.0), (len, 0.0), (len, h), (len / 2.0, h + rise), (0.0, h)],
        Preset::Slope => vec![(0.0, 0.0), (len, 0.0), (len, h + rise), (0.0, h)],
    }
}

/// Why the outline cannot be written (an i18n key), None when it can.
pub fn problem(pts: &[(f64, f64)], len: f64) -> Option<&'static str> {
    if pts.iter().any(|p| !(p.0.is_finite() && p.1.is_finite())) {
        return Some("wprof.err.number");
    }
    if pts.iter().any(|p| p.0 < -1e-9 || p.0 > len + 1e-9) {
        return Some("wprof.err.outside");
    }
    let p = ElevationProfile { outer: pts.to_vec(), holes: vec![] };
    match p.validate() {
        Ok(()) if p.area() > 1e-6 => None,
        Ok(()) | Err(SketchIssue::Empty) | Err(SketchIssue::Degenerate { .. }) => {
            Some("wprof.err.few")
        }
        Err(_) => Some("wprof.err.cross"),
    }
}

/// Writes the outline of `w` (None removes it) as one undo step.
pub fn write(ed: &mut Editor, wall: i64, pts: Option<Vec<(f64, f64)>>) -> Result<(), String> {
    let p = pts.map(|outer| ElevationProfile { outer, holes: vec![] });
    ed.run_op(OpFn(Arc::new(move |m| m.set_wall_profile(wall, p.as_ref()))))
        .map_err(|e| e.to_string())
}

/// Inserts a point in the middle of the edge after point `k`.
pub fn split_edge(pts: &mut Vec<(f64, f64)>, k: usize) {
    let n = pts.len();
    if n == 0 {
        return;
    }
    let (a, b) = (pts[k % n], pts[(k + 1) % n]);
    pts.insert(k % n + 1, ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0));
}

fn snap(v: f64) -> f64 {
    (v / SNAP).round() * SNAP
}

/// Preview frame: the wall's box `len` by the highest point, fitted in `r` with a margin.
fn frame_of(w: &Win, r: Rect) -> impl Fn((f64, f64)) -> Pos2 + Copy {
    let top = w.pts.iter().map(|p| p.1).fold(w.height, f64::max).max(1e-3);
    let bot = w.pts.iter().map(|p| p.1).fold(0.0, f64::min);
    let m = 12.0;
    let sx = (r.width() - 2.0 * m) / w.len.max(1e-3) as f32;
    let sy = (r.height() - 2.0 * m) / (top - bot) as f32;
    let s = sx.min(sy);
    let x0 = r.center().x - s * w.len as f32 / 2.0;
    let y0 = r.center().y + s * (top + bot) as f32 / 2.0;
    move |(u, z): (f64, f64)| Pos2::new(x0 + s * u as f32, y0 - s * z as f32)
}

fn preview(ui: &mut egui::Ui, w: &mut Win) {
    let th = ddc_ui::theme::of(ui.ctx());
    let size = Vec2::new(ui.available_width(), PREVIEW_H);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let to = frame_of(w, r);
    let p = ui.painter_at(r);
    let sem = th.sem();
    p.rect_filled(r, th.radius("md"), sem.bg_sunken);
    let base = [to((0.0, 0.0)), to((w.len, 0.0)), to((w.len, w.height)), to((0.0, w.height))];
    p.add(egui::Shape::closed_line(base.to_vec(), Stroke::new(1.0, sem.border_strong)));
    let poly: Vec<Pos2> = w.pts.iter().map(|q| to(*q)).collect();
    let bad = problem(&w.pts, w.len).is_some();
    let line = if bad { sem.danger } else { sem.accent };
    if poly.len() >= 3 && !bad {
        // a concave outline is filled as a fan of its (valid, simple) polygon
        let fill = line.gamma_multiply(0.18);
        let mesh = fan(&poly, fill);
        p.add(egui::Shape::mesh(mesh));
    }
    p.add(egui::Shape::closed_line(poly.clone(), Stroke::new(2.0, line)));
    for (k, q) in poly.iter().enumerate() {
        let on = w.drag == Some(k);
        p.circle_filled(*q, if on { 5.0 } else { 4.0 }, if on { sem.accent } else { line });
        let f = th.font(ddc_ui::theme::Role::Caption);
        let at = *q + Vec2::new(6.0, -6.0);
        p.text(at, egui::Align2::LEFT_BOTTOM, (k + 1).to_string(), f, sem.text_secondary);
    }
    if resp.drag_started() {
        if let Some(at) = resp.interact_pointer_pos() {
            w.drag = poly
                .iter()
                .enumerate()
                .map(|(k, q)| (k, q.distance(at)))
                .filter(|x| x.1 <= PICK)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|x| x.0);
        }
    }
    if let (Some(k), Some(at)) = (w.drag, resp.interact_pointer_pos()) {
        if resp.dragged() {
            let o = to((0.0, 0.0));
            let one = to((1.0, 1.0));
            let (sx, sy) = ((one.x - o.x) as f64, (o.y - one.y) as f64);
            let u = snap(((at.x - o.x) as f64 / sx).clamp(0.0, w.len));
            let z = snap((o.y - at.y) as f64 / sy);
            if let Some(q) = w.pts.get_mut(k) {
                *q = (u, z);
            }
        }
    }
    if resp.drag_stopped() {
        w.drag = None;
    }
}

/// Triangle fan of a simple polygon for its fill; a concave outline is cut into ears.
pub fn fan(poly: &[Pos2], c: Color32) -> egui::Mesh {
    let mut m = egui::Mesh::default();
    let pts: Vec<ddc_geom::V2> =
        poly.iter().map(|p| ddc_geom::v2(f64::from(p.x), f64::from(p.y))).collect();
    for p in poly {
        m.colored_vertex(*p, c);
    }
    for t in ddc_geom::poly::triangulate(&pts) {
        m.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
    }
    m
}

fn point_rows(ui: &mut egui::Ui, w: &mut Win, ed: &Editor, cat: &Catalog) {
    let th = ddc_ui::theme::of(ui.ctx());
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let u = ed.tool.units;
    let (along, up) = (cat.t("wprof.along"), cat.t("wprof.height"));
    let mut drop = None;
    let n = w.pts.len();
    kit::scroll_list(ui, "wprof.pts", ROWS, th.d().control, |ui| {
        for k in 0..n {
            kit::fixed_row(ui, &format!("wprof.row{k}"), |ui| {
                ui.label((k + 1).to_string());
                let (mut a, mut z) = w.pts[k];
                let la = ui.label(&along).id;
                let ida = egui::Id::new(("wprof.u", k));
                if kit::len_field(ui, &th, ida, &mut a, u, 0.0, w.len, ru, la, &along) {
                    w.pts[k].0 = a;
                }
                let lz = ui.label(&up).id;
                let idz = egui::Id::new(("wprof.z", k));
                if kit::len_field(ui, &th, idz, &mut z, u, -100.0, 1000.0, ru, lz, &up) {
                    w.pts[k].1 = z;
                }
                let off = n <= 3;
                let tip = cat.t("wprof.remove_pt");
                let x = ui.add_enabled_ui(!off, |ui| {
                    Button::quiet("\u{d7}").small().tooltip(&tip, "").show(ui, &th)
                });
                if x.inner.clicked() {
                    drop = Some(k);
                }
            });
        }
    });
    if let Some(k) = drop {
        w.pts.remove(k);
    }
}

/// Window body (apart from the frame, for the layout probes). `true`: close.
pub fn body(ui: &mut egui::Ui, w: &mut Win, ed: &mut Editor, cat: &Catalog) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let ru = cat.lang == ddc_ui::Lang::Ru;
    let units = ed.tool.units;
    ui.add(egui::Label::new(cat.t("wprof.lead")).wrap());
    ui.add_space(space::XS);
    preview(ui, w);
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        for (p, k) in [
            (Preset::Rect, "wprof.rect"),
            (Preset::Gable, "wprof.gable"),
            (Preset::Slope, "wprof.slope"),
        ] {
            if kit::secondary_button(ui, cat.t(k)).clicked() {
                w.pts = preset(p, w.len, w.height, w.rise);
            }
        }
    });
    let rise = cat.t("wprof.rise");
    kit::labeled_row_id(ui, &rise, |ui, lid| {
        let id = egui::Id::new("wprof.rise");
        kit::len_field(ui, &th, id, &mut w.rise, units, -100.0, 100.0, ru, lid, &rise);
    });
    ui.add_space(space::XS);
    kit::section_title(ui, cat.t("wprof.points"));
    point_rows(ui, w, ed, cat);
    if kit::secondary_button(ui, cat.t("wprof.add_pt")).clicked() {
        // after the longest edge: the new point lands where there is room to drag it
        let n = w.pts.len();
        let k = (0..n)
            .max_by(|&i, &j| {
                let l = |i: usize| {
                    let (a, b) = (w.pts[i], w.pts[(i + 1) % n]);
                    (b.0 - a.0).hypot(b.1 - a.1)
                };
                l(i).total_cmp(&l(j))
            })
            .unwrap_or(0);
        split_edge(&mut w.pts, k);
    }
    let bad = problem(&w.pts, w.len);
    match bad {
        Some(k) => kit::warn_note(ui, cat.t(k)),
        None => {
            let p = ElevationProfile { outer: w.pts.clone(), holes: vec![] };
            let a = crate::unit_text::area(p.area(), ru, units);
            kit::props(ui, "wprof.area", |ui| kit::kv(ui, &cat.t("wprof.area"), a));
        }
    }
    if let Some(n) = &w.note {
        kit::error_note(ui, n);
    }
    let (mut go, mut remove, mut cancel) = (false, false, false);
    let block = bad.map(|k| cat.t(k));
    kit::dialog_footer(ui, |ui| {
        go = kit::primary_action(ui, cat.t("wprof.apply"), block.as_deref());
        remove = ui
            .add_enabled_ui(w.had, |ui| kit::secondary_button(ui, cat.t("wprof.remove")))
            .inner
            .on_disabled_hover_text(cat.t("wprof.none"))
            .clicked();
        cancel = kit::cancel_button(ui, cat.t("dlg.cancel")).clicked();
    });
    if go || remove {
        let pts = go.then(|| w.pts.clone());
        match write(ed, w.wall, pts) {
            Ok(()) => return true,
            Err(e) => w.note = Some(cat.tf("wprof.err.model", &[("e", &e)])),
        }
    }
    cancel
}

fn run(a: &mut EditorApp) {
    let walls: Vec<i64> = a
        .ed
        .selection
        .iter()
        .filter_map(|s| if let Sel::Wall(id) = s { Some(*id) } else { None })
        .collect();
    let [id] = walls[..] else {
        a.ed.say(Severity::Refusal, a.sh.cat.t("wprof.err.one"));
        return;
    };
    match win_for(&a.ed, id) {
        Ok(w) => WIN.with(|s| *s.borrow_mut() = w),
        Err(k) if k.starts_with("wprof.") => a.ed.say(Severity::Refusal, a.sh.cat.t(&k)),
        Err(e) => {
            let m = a.sh.cat.tf("wprof.err.model", &[("e", &e)]);
            a.ed.say(Severity::Refusal, m);
        }
    }
}

pub fn open_for_layout(ed: &Editor, id: i64) {
    let w = win_for(ed, id).expect("wall");
    WIN.with(|s| *s.borrow_mut() = w);
}

/// Called every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut w = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !w.open {
        return;
    }
    // the wall went away (undo, delete): nothing left to edit
    if !a.ed.walls.iter().any(|x| x.id == w.wall) {
        return;
    }
    let (mut open, mut close) = (true, false);
    kit::Dialog::new("wall_profile", a.sh.cat.t("cmd.ctx.wall.profile"), kit::DialogSize::Form)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| close = body(ui, &mut w, &mut a.ed, &a.sh.cat));
    w.open = open && !close;
    WIN.with(|s| *s.borrow_mut() = w);
}

pub fn register(a: &mut EditorApp) {
    a.register_command("ctx.wall.profile", run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    fn editor() -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut ed);
        let id = ed.walls[0].id;
        (ed, id)
    }

    #[test]
    fn gable_is_written_in_one_undo_step_and_removed_again() {
        let (mut ed, id) = editor();
        let w = win_for(&ed, id).unwrap();
        assert!(!w.had);
        assert_eq!(w.pts, ElevationProfile::rectangle(w.len, w.height).outer);
        let g = preset(Preset::Gable, w.len, w.height, 1.5);
        assert_eq!(problem(&g, w.len), None);
        write(&mut ed, id, Some(g.clone())).unwrap();
        assert_eq!(ed.model.wall_profile(id).unwrap().unwrap().outer, g);
        let top = ed.walls.iter().find(|x| x.id == id).unwrap().top.clone().unwrap();
        let peak = top.iter().map(|p| p.1).fold(0.0, f64::max);
        assert!((peak - (w.height + 1.5)).abs() < 1e-6, "{top:?}");
        assert!(win_for(&ed, id).unwrap().had);
        ed.undo();
        assert!(ed.model.wall_profile(id).unwrap().is_none());
        ed.redo();
        write(&mut ed, id, None).unwrap();
        assert!(ed.model.wall_profile(id).unwrap().is_none());
    }

    #[test]
    fn bad_outlines_name_their_problem() {
        let len = 4.0;
        assert_eq!(problem(&[(0.0, 0.0), (4.0, 0.0)], len), Some("wprof.err.few"));
        let out = vec![(0.0, 0.0), (5.0, 0.0), (5.0, 3.0), (0.0, 3.0)];
        assert_eq!(problem(&out, len), Some("wprof.err.outside"));
        let bow = vec![(0.0, 0.0), (4.0, 3.0), (4.0, 0.0), (0.0, 3.0)];
        assert_eq!(problem(&bow, len), Some("wprof.err.cross"));
        let nan = vec![(0.0, 0.0), (4.0, f64::NAN), (0.0, 3.0)];
        assert_eq!(problem(&nan, len), Some("wprof.err.number"));
        assert_eq!(problem(&preset(Preset::Slope, len, 3.0, 1.0), len), None);
    }

    #[test]
    fn a_new_point_splits_an_edge_and_keeps_the_outline_valid() {
        let mut p = preset(Preset::Rect, 4.0, 3.0, 0.0);
        split_edge(&mut p, 2);
        assert_eq!(p[3], (2.0, 3.0));
        assert_eq!(p.len(), 5);
        assert_eq!(problem(&p, 4.0), None);
        split_edge(&mut p, 4);
        assert_eq!(p[5], (0.0, 1.5));
    }

    #[test]
    fn the_command_needs_one_wall() {
        let mut a = crate::layout_contract::app_in(Lang::En);
        crate::layout_contract::select_room_walls(&mut a);
        a.ed.selection.clear();
        assert!(a.run("ctx.wall.profile"));
        assert!(a.ed.status.clone().unwrap_or_default().to_lowercase().contains("one wall"));
        let id = a.ed.walls[0].id;
        a.ed.selection = vec![Sel::Wall(id)];
        a.run("ctx.wall.profile");
        assert!(WIN.with(|w| w.borrow().open && w.borrow().wall == id));
        WIN.with(|w| w.borrow_mut().open = false);
    }

    #[test]
    fn every_text_is_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in [
                "wprof.lead", "wprof.rect", "wprof.gable", "wprof.slope", "wprof.rise",
                "wprof.points", "wprof.along", "wprof.height", "wprof.remove_pt", "wprof.add_pt",
                "wprof.area", "wprof.apply", "wprof.remove", "wprof.none", "wprof.err.one",
                "wprof.err.no_wall", "wprof.err.number", "wprof.err.outside", "wprof.err.few",
                "wprof.err.cross", "wprof.err.model",
            ] {
                assert_ne!(cat.t(k), k, "{lang:?} {k}");
            }
        }
    }
}
