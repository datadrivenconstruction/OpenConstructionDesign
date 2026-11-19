// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View graphics in the application: cache of resolved graphics per view (`layer`), drawing of overrides
//! and hatches, ribbon commands and windows: visibility and graphics (VV), templates, materials, object styles.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ddc_model::{
    material_default_hatch, DetailLevel, ElemPaint, Error, GfxOverride, Lod, Model, TemplateField,
    ViewKey, ViewPaint,
};
use ddc_render::{tag_rgb, Scene, TAG_GLASS, TAG_RGB, TAG_TRANSLUCENT};
use egui::{self, Color32, Pos2, Stroke};

use crate::edit3d::pick::Target;
use crate::editor::{Editor, OpFn, Sel};
use crate::grips::Kind;
use crate::isolate::Part;
use crate::phase_ui::sel_ref;
use crate::shell::EditorApp;

mod mat_mesh_ui;
mod mat_texture_ui;
pub(crate) mod materials_win;
mod rule_value;
mod template_win;
#[cfg(test)]
pub(crate) use template_win::layout_confirm as template_delete_layout;
mod vv_rows;
mod vv_table;
pub(crate) mod vv_win;

/// Width of all lines in "thin lines" mode, px.
const THIN: f32 = 0.75;
/// Spacing ratio of the back diagonal to the forward one: otherwise two adjacent hatches merge.
const BACK_STEP: f32 = 1.4;

/// Resolved graphics of one view, ready for drawing.
#[derive(Clone, Debug, Default)]
pub struct GfxLayer {
    pub paint: Arc<ViewPaint>,
    /// Changes only when what is visible or the overrides change; part of the 3D scene key.
    pub stamp: u64,
    /// Hidden by filters and categories; part of `Vis`.
    pub hide: HashSet<Sel>,
    pub hide_parts: HashSet<Part>,
    /// Project materials: name → (colour, hatch).
    pub mats: HashMap<String, (Option<Color32>, String)>,
    /// Wall-type hatch comes from the thickest layer; no entry — no hatch.
    wall_hatch: HashMap<i64, String>,
    /// 3D coverage categories (meshes of other levels without an element id).
    pub nav_hidden: Vec<String>,
    /// Halftone lines (view range and drawing quality styles), by (table, id).
    pub half: HashSet<(&'static str, i64)>,
    /// Thin lines: the session toggle of the view, else the value saved in the file.
    pub thin: bool,
}

/// Table of a 3D element without `Sel` (column, beam, ...).
pub(crate) fn kind_table(k: Kind) -> Option<&'static str> {
    Some(match k {
        Kind::Wall => "wall",
        Kind::Slab => "slab",
        Kind::Ceiling => "ceiling",
        Kind::Roof => "roof",
        Kind::Column => "col",
        Kind::Beam => "beam",
        Kind::Stair => "stair",
        Kind::Railing => "railing",
        Kind::Comp => "comp",
        Kind::Mep => "mep_run",
        _ => return None,
    })
}

const PART_KINDS: [Kind; 10] = [
    Kind::Wall,
    Kind::Slab,
    Kind::Ceiling,
    Kind::Roof,
    Kind::Column,
    Kind::Beam,
    Kind::Stair,
    Kind::Railing,
    Kind::Comp,
    Kind::Mep,
];

/// 3D coverage distinguishes only these categories; the rest fall into the common "other" group.
fn nav_category(c: &str) -> Option<&'static str> {
    Some(match c {
        "wall" => "wall",
        "door" => "door",
        "window" => "window",
        "curtain_wall" => "curtain",
        "slab" => "floor",
        "mep_run" => "mep",
        _ => return None,
    })
}

pub fn color_of(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub fn fill_with(o: GfxOverride, base: Color32) -> Color32 {
    let c = o.fill.map_or(base, color_of);
    match o.transparency {
        Some(t) if t > 0 => c.gamma_multiply(1.0 - f32::from(t.min(100)) / 100.0),
        _ => c,
    }
}

/// 3D mesh tag with an override applied; None — unchanged. Transparency in 3D uses the glass pass.
fn mesh_tag(o: GfxOverride, tag: u32) -> Option<u32> {
    let clear = o.transparency.unwrap_or(0) > 0;
    match o.fill {
        Some(c) => Some(tag_rgb([(c >> 16) as u8, (c >> 8) as u8, c as u8], clear)),
        None if clear && tag & TAG_RGB != 0 => Some(tag | TAG_TRANSLUCENT),
        None if clear => Some(TAG_GLASS),
        None => None,
    }
}

/// Tag of a mesh from another level: None — hidden in the view.
fn tag_with(e: Option<ElemPaint>, tag: u32) -> Option<u32> {
    match e {
        Some(e) if e.hidden => None,
        Some(e) => Some(mesh_tag(e.over, tag).unwrap_or(tag)),
        None => Some(tag),
    }
}

impl GfxLayer {
    pub fn build(ed: &Editor, v: ViewKey) -> Self {
        let m = &ed.model;
        let paint = m.view_paint(v).unwrap_or_default();
        let mut hide = HashSet::new();
        let mut hide_parts = HashSet::new();
        if paint.any_hidden {
            for s in crate::selection::all(ed) {
                if sel_ref(s).is_some_and(|(t, id)| paint.hidden(t, id)) {
                    hide.insert(s);
                }
            }
            for ((table, id), e) in &paint.elems {
                let k = PART_KINDS.iter().find(|k| kind_table(**k) == Some(*table));
                if let (true, Some(k)) = (e.hidden, k) {
                    hide_parts.insert((*k, *id));
                }
            }
        }
        let mats: HashMap<String, (Option<Color32>, String)> = m
            .material_rows()
            .unwrap_or_default()
            .into_iter()
            .map(|x| (x.spec.name, (x.spec.color.map(color_of), x.spec.hatch)))
            .collect();
        let mut wall_hatch = HashMap::new();
        for ty in &ed.types {
            let names = m.wall_type_materials(ty.id).unwrap_or_default();
            // with equal thicknesses — the first layer, as in the plan overview
            let mut thickest: Option<(&String, &f64)> = None;
            for (n, w) in names.iter().zip(&ty.layers) {
                if thickest.is_none_or(|t| w > t.1) {
                    thickest = Some((n, w));
                }
            }
            let own = thickest.map_or("none", |(n, _)| {
                mats.get(n)
                    .map_or_else(|| material_default_hatch(n), |r| r.1.as_str())
            });
            let generic = thickest.is_none_or(|(n, _)| crate::section_view::mat_class(n) == 5);
            let hatch = match (own, generic) {
                ("none", true) if ty.name.starts_with("\u{41d}\u{430}\u{440}\u{443}\u{436}\u{43d}\u{430}\u{44f}") => "diagonal",
                ("none", true) => material_default_hatch(&ty.name),
                _ => own,
            };
            if hatch != "none" {
                wall_hatch.insert(ty.id, hatch.to_string());
            }
        }
        let nav_hidden = paint
            .hidden_cats
            .iter()
            .filter_map(|c| nav_category(c).map(str::to_string))
            .collect();
        let thin = thin_of(ed, v, paint.thin_lines);
        Self {
            thin,
            paint,
            stamp: 0,
            hide,
            hide_parts,
            mats,
            wall_hatch,
            nav_hidden,
            half: HashSet::new(),
        }
    }

    /// The same graphics with a pen width and halftone per element from the drawing quality
    /// styles; an explicit weight set in the view graphics wins.
    pub fn with_drawq(&self, looks: &HashMap<(&'static str, i64), (f32, bool)>) -> GfxLayer {
        let mut g = self.clone();
        let paint = Arc::make_mut(&mut g.paint);
        for (&k, &(px, half)) in looks {
            let e = paint.elems.entry(k).or_default();
            if e.over.line_weight.is_none() {
                e.over.line_weight = Some(px);
            }
            if half {
                g.half.insert(k);
            }
        }
        g
    }

    fn halftoned(&self, table: &str, id: i64, c: Color32) -> Color32 {
        let h: &HashSet<(&str, i64)> = &self.half;
        if h.contains(&(table, id)) {
            c.gamma_multiply(1.0 - ddc_model::drawq::HALFTONE_K)
        } else {
            c
        }
    }

    fn elem(&self, table: &str, id: i64) -> GfxOverride {
        self.paint
            .elem(table, id)
            .map_or_else(GfxOverride::default, |e| e.over)
    }

    /// Tag of another level's mesh by its element; None — hidden in the view.
    pub fn level_tag(&self, s: Option<Sel>, tag: u32) -> Option<u32> {
        tag_with(
            s.and_then(sel_ref)
                .and_then(|(t, id)| self.paint.elem(t, id)),
            tag,
        )
    }

    /// The same for a 3D provider part.
    pub fn part_tag(&self, target: Target, tag: u32) -> Option<u32> {
        let table = target.grip_kind().and_then(kind_table);
        tag_with(table.and_then(|t| self.paint.elem(t, target.id)), tag)
    }

    fn same_view(&self, o: &GfxLayer) -> bool {
        Arc::ptr_eq(&self.paint, &o.paint) && self.thin == o.thin && self.hide == o.hide && self.hide_parts == o.hide_parts
    }

    fn over(&self, s: Sel) -> GfxOverride {
        sel_ref(s).map_or_else(GfxOverride::default, |(t, id)| self.elem(t, id))
    }

    fn width_with(&self, o: GfxOverride, w: f32) -> f32 {
        match o.line_weight {
            Some(x) => x,
            None if self.thin => w.min(THIN),
            None => w,
        }
    }

    pub fn fill(&self, s: Sel, base: Color32) -> Color32 {
        fill_with(self.over(s), base)
    }
    pub fn line(&self, s: Sel, base: Color32) -> Color32 {
        let c = self.over(s).line.map_or(base, color_of);
        match sel_ref(s) {
            Some((t, id)) if !self.half.is_empty() => self.halftoned(t, id, c),
            _ => c,
        }
    }
    pub fn width(&self, s: Sel, w: f32) -> f32 {
        self.width_with(self.over(s), w)
    }

    // by table: this is how the section drawing identifies the element
    pub fn fill_at(&self, table: &str, id: i64, base: Color32) -> Color32 {
        fill_with(self.elem(table, id), base)
    }
    pub fn line_at(&self, table: &str, id: i64, base: Color32) -> Color32 {
        let c = self.elem(table, id).line.map_or(base, color_of);
        self.halftoned(table, id, c)
    }
    pub fn width_at(&self, table: &str, id: i64, w: f32) -> f32 {
        self.width_with(self.elem(table, id), w)
    }

    pub fn coarse(&self) -> bool {
        self.paint.detail == DetailLevel::Coarse
    }
    pub fn fine(&self) -> bool {
        self.paint.detail == DetailLevel::Fine
    }

    /// Wall-type hatch; at coarse detail there are no hatches.
    pub fn wall_hatch(&self, wall_type: i64) -> Option<&str> {
        if self.coarse() {
            return None;
        }
        self.wall_hatch.get(&wall_type).map(String::as_str)
    }
    pub fn mat_color(&self, material: &str) -> Option<Color32> {
        self.mats.get(material).and_then(|m| m.0)
    }
    /// Material hatch; for a name with no project entry — the default of its class.
    pub fn mat_hatch(&self, material: &str) -> &str {
        self.mats
            .get(material)
            .map_or_else(|| material_default_hatch(material), |m| m.1.as_str())
    }
}

#[derive(Default)]
struct Cache {
    key: (u64, u64, u64),
    layers: HashMap<ViewKey, Arc<GfxLayer>>,
    /// Previous view graphics: the stamp changes only if they differ.
    prev: HashMap<ViewKey, Arc<GfxLayer>>,
    next: u64,
}

thread_local! {
    static CACHE: std::cell::RefCell<Cache> = std::cell::RefCell::default();
    /// View and stamp for which the selection was already checked for hidden elements.
    static PRUNED: std::cell::Cell<Option<(ViewKey, u64)>> = const { std::cell::Cell::new(None) };
}

/// View graphics (cached until the next edit). Call when the editor lists are complete.
pub fn layer(ed: &Editor, v: ViewKey) -> Arc<GfxLayer> {
    // the assistant's temporary color by value sits on top; without one this is the base
    crate::ai::view_state::tinted(base_layer(ed, v))
}

fn base_layer(ed: &Editor, v: ViewKey) -> Arc<GfxLayer> {
    CACHE.with(|c| {
        let c = &mut *c.borrow_mut();
        let key = (ed.revision, ed.model.change_head(), ed.view_thin_rev);
        if c.key != key {
            c.key = key;
            c.prev.extend(c.layers.drain());
        }
        if let Some(l) = c.layers.get(&v) {
            return l.clone();
        }
        let mut l = GfxLayer::build(ed, v);
        l.stamp = match c.prev.get(&v) {
            Some(p) if p.same_view(&l) => p.stamp,
            _ => {
                c.next += 1;
                c.next
            }
        };
        let l = Arc::new(l);
        c.layers.insert(v, l.clone());
        l
    })
}

/// Project material names for selection in layers.
pub fn project_materials(ed: &Editor) -> Vec<String> {
    let mut v: Vec<String> = layer(ed, ViewKey::ThreeD).mats.keys().cloned().collect();
    v.sort();
    v
}

/// Component detail level of the 3D view.
pub fn lod_3d(m: &Model) -> Lod {
    m.view_lod(ViewKey::ThreeD).unwrap_or(Lod::Medium)
}

/// Override colour in the 3D scene. Meshes with a different colour are copied, the others are shared;
/// component placements share one mesh, so they do not get their own colour.
pub fn paint_scene(l: &GfxLayer, ed: &Editor, sc: Scene, targets: &[Target]) -> Scene {
    if l.paint.elems.values().all(|e| e.over.is_empty()) {
        return sc;
    }
    let boxes = sc.boxes().to_vec();
    let (cats, focus) = (sc.cats.clone(), sc.focus);
    let meshes = sc
        .meshes
        .into_iter()
        .map(|m| {
            let o = match ed.sel_of_element(m.element) {
                Some(s) => l.over(s),
                None => crate::edit3d::scene3d::target_of(m.element, targets)
                    .flatten()
                    .and_then(|t| Some((kind_table(t.grip_kind()?)?, t.id)))
                    .map_or_else(GfxOverride::default, |(t, id)| l.elem(t, id)),
            };
            match mesh_tag(o, m.tag) {
                Some(tag) => {
                    let mut c = (*m).clone();
                    c.tag = tag;
                    Arc::new(c)
                }
                None => m,
            }
        })
        .collect();
    let mut out = Scene::from_parts(meshes, boxes).with_instanced(sc.instanced);
    out.cats = cats;
    out.focus = focus;
    out
}

// ---- hatching ----

/// Hatch segments of outlines (even-odd) at an angle; spacing is constant on screen.
fn scan(rings: &[Vec<Pos2>], step: f32, angle: f32) -> Vec<(Pos2, Pos2)> {
    /// More lines than this means the outline is far off-screen at high zoom.
    const MAX_LINES: f32 = 4000.0;
    let (s, c) = angle.sin_cos();
    let rot: Vec<Vec<Pos2>> = rings
        .iter()
        .map(|r| {
            r.iter()
                .map(|p| Pos2::new(p.x * c + p.y * s, -p.x * s + p.y * c))
                .collect()
        })
        .collect();
    let back = |x: f32, y: f32| Pos2::new(x * c - y * s, x * s + y * c);
    let (mut y0, mut y1) = (f32::MAX, f32::MIN);
    for p in rot.iter().flatten() {
        y0 = y0.min(p.y);
        y1 = y1.max(p.y);
    }
    if step <= 0.0 || y1 <= y0 || (y1 - y0) / step > MAX_LINES {
        return vec![];
    }
    let mut out = vec![];
    let mut y = (y0 / step).ceil() * step;
    while y < y1 {
        let mut xs: Vec<f32> = vec![];
        for r in &rot {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                if (a.y <= y) != (b.y <= y) {
                    xs.push(a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x));
                }
            }
        }
        xs.sort_by(f32::total_cmp);
        for w in xs.chunks_exact(2) {
            out.push((back(w[0], y), back(w[1], y)));
        }
        y += step;
    }
    out
}

/// Plan hatch pattern by key from `MATERIAL_HATCHES` (drawn by `overlay::hatch`).
pub fn hatch_of_key(k: &str) -> crate::overlay::Hatch {
    use crate::overlay::Hatch;
    match k {
        "diagonal" => Hatch::Diagonal,
        "back_diagonal" => Hatch::BackDiagonal,
        "cross" => Hatch::Cross,
        "dots" => Hatch::Dots,
        _ => Hatch::None,
    }
}

/// Material hatch by key from `MATERIAL_HATCHES` (section: several rings, even-odd).
pub fn paint_hatch(p: &egui::Painter, rings: &[Vec<Pos2>], kind: &str, step: f32, st: Stroke) {
    use std::f32::consts::FRAC_PI_4;
    let lines = |angle: f32, step: f32| {
        for (a, b) in scan(rings, step, angle) {
            p.line_segment([a, b], st);
        }
    };
    match kind {
        "diagonal" => lines(FRAC_PI_4, step),
        "back_diagonal" => lines(-FRAC_PI_4, step * BACK_STEP),
        "cross" => {
            lines(-FRAC_PI_4, step);
            lines(FRAC_PI_4, step);
        }
        "dots" => {
            for (a, b) in scan(rings, step, 0.0) {
                let mut x = a.x.min(b.x) + step / 2.0;
                while x < a.x.max(b.x) {
                    p.circle_filled(Pos2::new(x, a.y), st.width, st.color);
                    x += step;
                }
            }
        }
        k if k.starts_with("lib:") => {
            crate::range_layer::paint_lib_hatch(p, rings, &k["lib:".len()..], st);
        }
        _ => {}
    }
}

// ---- commands ----

pub(crate) fn t(key: &str) -> String {
    crate::tr(key, &[])
}

/// A model edit as a single undo step; errors go to the status bar.
pub(crate) fn run(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), Error> + Send + Sync + 'static,
) -> bool {
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => true,
        Err(e) => {
            ed.status = Some(error_text(&e));
            false
        }
    }
}

/// Model rejections that are understandable without technical details — phrased in plain words.
fn error_text(e: &Error) -> String {
    error_text_in(crate::ui_lang_now(), e)
}

/// `error_text` in an explicit language; typed field refusals name the field, only unknown
/// errors fall back to `view_gfx.error`.
fn error_text_in(ru: bool, e: &Error) -> String {
    match e {
        Error::Field(f) => {
            let args = f.args();
            let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
            return crate::tr_lang(ru, f.key(), &args);
        }
        Error::Conflict(msg) => {
            for (mark, key) in [
                ("view template controls", "view_gfx.locked"),
                ("already exists", "view_gfx.name_taken"),
                ("is in use", "view_gfx.mat.delete_used"),
            ] {
                if msg.contains(mark) {
                    return crate::tr_lang(ru, key, &[]);
                }
            }
        }
        _ => {}
    }
    crate::tr_lang(ru, "view_gfx.error", &[("msg", &e.to_string())])
}

/// Removes view-hidden elements from the selection; returns how many were removed.
fn prune_hidden(ed: &mut Editor, v: ViewKey) -> usize {
    let l = layer(ed, v);
    if !l.paint.any_hidden {
        return 0;
    }
    let before = ed.selection.len() + ed.grip.focus.len();
    ed.selection
        .retain(|s| !sel_ref(*s).is_some_and(|(t, id)| l.paint.hidden(t, id)));
    ed.grip.focus.retain(|p| !l.hide_parts.contains(p));
    before - ed.selection.len() - ed.grip.focus.len()
}

/// View-hidden elements are not deleted; the user is told so via the status bar.
fn note_kept(ed: &mut Editor, kept: usize) {
    if kept == 0 {
        return;
    }
    let note = crate::tr("view_gfx.delete.kept_hidden", &[("n", &kept.to_string())]);
    ed.status = Some(match ed.status.take() {
        Some(s) if !s.is_empty() => format!("{s}; {note}"),
        _ => note,
    });
}

fn delete_in_view(ed: &mut Editor, v: ViewKey, first: impl FnOnce(&mut Editor) -> bool) {
    let kept = prune_hidden(ed, v);
    if kept > 0 {
        ed.status = None;
    }
    if !first(&mut *ed) && (kept == 0 || !ed.selection.is_empty()) {
        ed.delete_selection();
    }
    note_kept(ed, kept);
}

/// Delete key in the plan.
pub(crate) fn delete_key(ed: &mut Editor) {
    let v = ViewKey::Plan(ed.level);
    delete_in_view(ed, v, crate::grips::delete_key);
}

/// Delete command: the editor selection and the 3D selection go in one undo step.
pub(crate) fn delete_visible(a: &mut EditorApp) {
    let v = active_view(a);
    let selected = a.ed.selection.clone();
    let mut kept = prune_hidden(&mut a.ed, v);
    // the 3D selection follows the editor's only while 3D is shown: elsewhere it is stale
    let mut extra = if a.view3d {
        crate::clip_tools::delete_extra(a)
    } else {
        Default::default()
    };
    if !extra.is_empty() {
        let l = layer(&a.ed, v);
        kept += extra.drop_hidden(|t, id| l.paint.hidden(t, id), &selected);
    }
    if kept > 0 {
        a.ed.status = None;
    }
    if (!a.ed.selection.is_empty() || !extra.is_empty() || kept == 0) && a.ed.delete_with(extra) {
        prune_3d_sel(a);
    }
    note_kept(&mut a.ed, kept);
}

/// After a delete only 3D picks that still exist stay selected (panels of live curtains).
pub(crate) fn prune_3d_sel(a: &mut EditorApp) {
    let ed = &a.ed;
    a.view.e3.sel.retain(|t| {
        matches!(t.sel(), Some(crate::editor::Sel::Panel(id, _)) if ed.curtain(id).is_some())
    });
}

pub(crate) fn active_view(a: &EditorApp) -> ViewKey {
    if a.view3d {
        ViewKey::ThreeD
    } else if let Some(id) = a.section {
        ViewKey::Section(id)
    } else {
        ViewKey::Plan(a.ed.level)
    }
}

pub(crate) fn view_label(ed: &Editor, v: ViewKey) -> String {
    view_label_in(crate::ui_is_ru(), ed, v)
}

/// As `view_label`, in an explicit language.
pub(crate) fn view_label_in(ru: bool, ed: &Editor, v: ViewKey) -> String {
    match v {
        ViewKey::ThreeD => crate::tr_lang(ru, "view_gfx.view.3d", &[]),
        ViewKey::Plan(l) => {
            let level = ed.model.level_list().unwrap_or_default();
            let name = level.iter().find(|x| x.0 == l).map_or("", |x| x.1.as_str());
            crate::tr_lang(ru, "view_gfx.view.plan", &[("name", name)])
        }
        ViewKey::Section(id) => {
            let defs = ed.model.view_defs().unwrap_or_default();
            defs.into_iter()
                .find(|d| d.0 == id)
                .map_or_else(|| id.to_string(), |d| d.1.name)
        }
    }
}

pub(crate) fn category_label(key: &str) -> String {
    t(&format!("view_gfx.cat.{key}"))
}

/// The field is locked by a linked template: message and rejection.
fn locked(a: &mut EditorApp, v: ViewKey, f: TemplateField) -> bool {
    let closed = a.ed.model.view_gfx(v).is_ok_and(|s| s.locked.contains(&f));
    if closed {
        a.ed.status = Some(t("view_gfx.locked"));
    }
    closed
}

fn h_detail(a: &mut EditorApp) {
    let v = active_view(a);
    if locked(a, v, TemplateField::Detail) {
        return;
    }
    let Ok(s) = a.ed.model.view_gfx(v) else {
        return;
    };
    let next = s.effective.detail.next();
    if run(&mut a.ed, move |m| m.edit_view_gfx(v, |g| g.detail = next)) {
        let name = t(&format!("view_gfx.detail.{}", next.key()));
        a.ed.status = Some(crate::tr("view_gfx.detail.status", &[("name", &name)]));
    }
}

/// Thin lines of a view: the session toggle, else the value the file carries.
pub(crate) fn thin_of(ed: &Editor, v: ViewKey, saved: bool) -> bool {
    ed.view_thin.get(&v).copied().unwrap_or(saved || ed.thin_pref)
}

/// Display toggle only: no model op, no undo step, no reload, works on a read-only file.
pub(crate) fn set_thin(ed: &mut Editor, v: ViewKey, on: bool) {
    ed.view_thin.insert(v, on);
    ed.view_thin_rev += 1;
}

/// Toggle for the view and remember it as the preference for the next session.
pub(crate) fn toggle_thin(ed: &mut Editor, prefs: &mut crate::prefs::Prefs, v: ViewKey, on: bool) {
    set_thin(ed, v, on);
    ed.thin_pref = on;
    prefs.thin_lines = on;
}

fn h_thin(a: &mut EditorApp) {
    let v = active_view(a);
    if locked(a, v, TemplateField::ThinLines) {
        return;
    }
    let Ok(s) = a.ed.model.view_gfx(v) else {
        return;
    };
    let on = !thin_of(&a.ed, v, s.effective.thin_lines);
    toggle_thin(&mut a.ed, &mut a.prefs, v, on);
    a.prefs.save();
    a.ed.status = Some(t(if on { "view_gfx.thin.on" } else { "view_gfx.thin.off" }));
}

/// View categories for the selected elements.
fn selected_categories(ed: &Editor) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for s in ed.selection.iter().copied() {
        let Some((table, id)) = sel_ref(s) else {
            continue;
        };
        let cat = match ed.model.category_of(table, id) {
            Ok(Some(c)) => Some(c),
            _ => ddc_model::GFX_CATEGORIES
                .iter()
                .find(|g| g.0 == table)
                .map(|g| g.0),
        };
        if let Some(c) = cat.filter(|c| !out.iter().any(|x| x == c)) {
            out.push(c.to_string());
        }
    }
    out
}

/// Permanently hide the categories of the selected elements in the view; restore them in the VV window.
fn h_hide_cat(a: &mut EditorApp) {
    let cats = selected_categories(&a.ed);
    if cats.is_empty() {
        a.ed.status = Some(t("view_gfx.hide_cat.no_sel"));
        return;
    }
    let v = active_view(a);
    if locked(a, v, TemplateField::Categories) {
        return;
    }
    let names = cats
        .iter()
        .map(|c| category_label(c))
        .collect::<Vec<_>>()
        .join(", ");
    let done = run(&mut a.ed, move |m| {
        m.edit_view_gfx(v, |g| {
            for c in &cats {
                g.categories.entry(c.clone()).or_default().visible = false;
            }
        })
    });
    if done {
        prune_hidden(&mut a.ed, v);
        a.ed.status = Some(crate::tr("view_gfx.hide_cat.done", &[("name", &names)]));
    }
}

/// Opens the visibility and graphics window (the plan's empty-state card).
pub(crate) fn open_vv() {
    vv_win::open();
}

pub fn register(app: &mut EditorApp) {
    app.register_command("view.filters", |_| vv_win::open());
    app.register_command("mgr.object_styles", |_| vv_win::open_styles());
    app.register_command("view.template", |_| template_win::open());
    app.register_command("mgr.materials", |_| materials_win::open());
    app.register_command("view.detail_level", h_detail);
    app.register_command("view.thin_lines", h_thin);
    app.register_command("view.hide_cat", h_hide_cat);
}

/// Windows; called every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let v = active_view(a);
    // hidden via a window or template, or switched to another view: invisible elements must not stay selected
    let now = Some((v, layer(&a.ed, v).stamp));
    if PRUNED.with(|c| c.replace(now)) != now {
        prune_hidden(&mut a.ed, v);
    }
    vv_win::window(ctx, &mut a.ed, v);
    vv_win::styles_window(ctx, &mut a.ed);
    template_win::window(ctx, &mut a.ed, v, a.sh.cat.lang == ddc_ui::Lang::Ru);
    materials_win::window(ctx, &mut a.ed);
}

/// Opens the visibility and graphics window for the layout contract.
#[cfg(test)]
pub(crate) fn open_vv_for_layout(filters: bool) {
    vv_win::open_for_layout(filters);
}

/// Opens the view templates window for the layout contract.
#[cfg(test)]
pub(crate) fn open_templates_for_layout(ed: &Editor, ru: bool) {
    template_win::open_for_layout(ed, ru);
}

/// Draws the window `key` ("view.styles" or "view.materials") headless for the layout contract.
#[cfg(test)]
pub(crate) fn layout_probe(key: &str, ed: &mut Editor) -> crate::layout_contract::Probe {
    if key == "view.styles" {
        vv_win::open_styles();
        crate::layout_contract::probe(key, |ctx| vv_win::styles_window(ctx, ed))
    } else {
        materials_win::open_for_layout(ed);
        crate::layout_contract::probe(key, |ctx| materials_win::window(ctx, ed))
    }
}

// ---- shared window fields ----

pub(crate) fn rgb_of(c: Color32) -> u32 {
    (u32::from(c.r()) << 16) | (u32::from(c.g()) << 8) | u32::from(c.b())
}

pub(crate) fn primary(ui: &mut egui::Ui, enabled: bool, text: &str) -> bool {
    let fill = ui.visuals().selection.bg_fill;
    let color = ui.visuals().selection.stroke.color;
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(text).color(color)).fill(fill),
    )
    .clicked()
}

/// Colour override: a dash means unset.
pub(crate) fn opt_color(ui: &mut egui::Ui, c: &mut Option<u32>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| match *c {
        Some(v) => {
            let mut rgb = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                *c = Some(rgb_of(Color32::from_rgb(rgb[0], rgb[1], rgb[2])));
                changed = true;
            }
            if ddc_ui::widgets::close_button(ui, &t("view_gfx.over.clear")).clicked() {
                *c = None;
                changed = true;
            }
        }
        None => {
            let th = ddc_ui::theme::of(ui.ctx());
            if ddc_ui::widgets::Button::quiet("\u{2014}")
                .show(ui, &th)
                .on_hover_text(t("view_gfx.over.set"))
                .clicked()
            {
                *c = Some(rgb_of(ui.visuals().text_color()));
                changed = true;
            }
        }
    });
    changed
}

/// Four override cells: line, weight, fill, transparency.
pub(crate) fn over_cells(ui: &mut egui::Ui, o: &mut GfxOverride) -> bool {
    over_cells_marked(ui, o, &mut |_, _| {})
}

/// As `over_cells`; `mark(ui, k)` runs before cell `k` (0..4), where `ui` sits at its left edge.
pub(super) fn over_cells_marked(
    ui: &mut egui::Ui,
    o: &mut GfxOverride,
    mark: &mut dyn FnMut(&egui::Ui, usize),
) -> bool {
    mark(ui, 0);
    let mut changed = opt_color(ui, &mut o.line);
    mark(ui, 1);
    ui.horizontal(|ui| match o.line_weight {
        Some(mut w) => {
            if ui
                .add(egui::DragValue::new(&mut w).range(0.25..=8.0).speed(0.05))
                .changed()
            {
                o.line_weight = Some(w);
                changed = true;
            }
            if ddc_ui::widgets::close_button(ui, &t("view_gfx.over.clear")).clicked() {
                o.line_weight = None;
                changed = true;
            }
        }
        None => {
            let th = ddc_ui::theme::of(ui.ctx());
            if ddc_ui::widgets::Button::quiet("\u{2014}")
                .show(ui, &th)
                .on_hover_text(t("view_gfx.over.set"))
                .clicked()
            {
                o.line_weight = Some(1.0);
                changed = true;
            }
        }
    });
    mark(ui, 2);
    changed |= opt_color(ui, &mut o.fill);
    mark(ui, 3);
    ui.horizontal(|ui| match o.transparency {
        Some(mut v) => {
            if ui
                .add(egui::DragValue::new(&mut v).range(0..=100).suffix(" %"))
                .changed()
            {
                o.transparency = Some(v);
                changed = true;
            }
            if ddc_ui::widgets::close_button(ui, &t("view_gfx.over.clear")).clicked() {
                o.transparency = None;
                changed = true;
            }
        }
        None => {
            let th = ddc_ui::theme::of(ui.ctx());
            if ddc_ui::widgets::Button::quiet("\u{2014}")
                .show(ui, &th)
                .on_hover_text(t("view_gfx.over.set"))
                .clicked()
            {
                o.transparency = Some(50);
                changed = true;
            }
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{FilterRule, FilterUse, MaterialSpec, RuleOp, ViewFilterSpec};

    fn ed() -> Editor {
        Editor::new_in_memory().unwrap()
    }

    fn add_wall(e: &mut Editor) {
        e.commit_walls(vec![(ddc_geom::v2(0.0, 0.0), ddc_geom::v2(4.0, 0.0))]);
    }

    #[test]
    fn toggle_updates_prefs() {
        let mut e = ed();
        let mut p = crate::prefs::Prefs::default();
        let v = ViewKey::Plan(e.level);
        toggle_thin(&mut e, &mut p, v, true);
        assert!(p.thin_lines && e.thin_pref && thin_of(&e, v, false));
        toggle_thin(&mut e, &mut p, v, false);
        assert!(!p.thin_lines && !thin_of(&e, v, false));
    }

    #[test]
    fn thin_toggle_leaves_change_head_and_dirty_unchanged() {
        let mut e = ed();
        let v = ViewKey::Plan(e.level);
        let (head, dirty, rev) = (e.model.change_head(), e.is_dirty(), e.revision);
        set_thin(&mut e, v, true);
        assert_eq!(e.model.change_head(), head);
        assert_eq!(e.is_dirty(), dirty);
        assert_eq!(e.revision, rev, "no model reload");
        assert!(layer(&e, v).thin);
        assert!(!e.model.view_gfx(v).unwrap().effective.thin_lines, "never written back");
        assert!((layer(&e, v).width(Sel::Wall(1), 3.0) - THIN).abs() < 1e-6);
    }

    #[test]
    fn undo_after_wall_add_and_thin_toggle_removes_wall() {
        let mut e = ed();
        let v = ViewKey::Plan(e.level);
        add_wall(&mut e);
        assert_eq!(e.walls.len(), 1);
        set_thin(&mut e, v, true);
        e.undo();
        assert_eq!(e.walls.len(), 0, "undo reverts the wall, not the toggle");
        assert!(layer(&e, v).thin, "display state stays");
    }

    #[test]
    fn thin_toggle_works_on_read_only_project() {
        let mut e = ed();
        e.project.set_read_only_for_test();
        let v = ViewKey::Plan(e.level);
        let head = e.model.change_head();
        set_thin(&mut e, v, true);
        assert!(layer(&e, v).thin);
        assert_eq!(e.model.change_head(), head);
        assert!(e.status.is_none());
    }

    #[test]
    fn thin_state_survives_view_switch() {
        let mut e = ed();
        let plan = ViewKey::Plan(e.level);
        set_thin(&mut e, plan, true);
        assert!(!layer(&e, ViewKey::ThreeD).thin, "other views keep their own value");
        set_thin(&mut e, ViewKey::ThreeD, true);
        assert!(layer(&e, plan).thin);
        set_thin(&mut e, plan, false);
        assert!(!layer(&e, plan).thin);
        assert!(layer(&e, ViewKey::ThreeD).thin);
    }

    /// An app with one wall, picked in 3D only.
    fn wall_picked_in_3d() -> EditorApp {
        let mut e = ed();
        crate::modify_tools::tests::walls(&mut e, &[((0.0, 0.0), (4.0, 0.0))]);
        let wall = Sel::Wall(e.walls[0].id);
        let mut a = EditorApp::new(e, None, "test".into());
        a.view.e3.sel = vec![Target::from_sel(wall).unwrap()];
        a
    }

    #[test]
    fn delete3d_confirm_prunes_sel() {
        let mut a = wall_picked_in_3d();
        a.ed.selection = vec![Sel::Wall(a.ed.walls[0].id)];
        a.ed.delete_selection();
        assert!(a.ed.walls.is_empty());
        assert!(!a.view.e3.sel.is_empty(), "the dialog path has not pruned yet");
        prune_3d_sel(&mut a);
        assert!(a.view.e3.sel.is_empty());
    }

    #[test]
    fn delete_takes_the_visible_3d_selection() {
        let mut a = wall_picked_in_3d();
        a.view3d = true;
        delete_visible(&mut a);
        assert!(a.ed.walls.is_empty(), "{:?}", a.ed.status);
    }

    #[test]
    fn delete_keeps_a_3d_pick_the_view_hides() {
        let mut a = wall_picked_in_3d();
        a.view3d = true;
        let steps = a.ed.undo_len();
        a.ed.model
            .set_view_category_visible(ViewKey::ThreeD, "wall", false)
            .unwrap();
        a.ed.reload().unwrap();
        delete_visible(&mut a);
        assert_eq!(a.ed.walls.len(), 1, "{:?}", a.ed.status);
        assert_eq!(a.ed.undo_len(), steps);
        assert!(a.ed.status.is_some(), "the user is told what stayed");
    }

    #[test]
    fn delete_outside_3d_leaves_a_stale_3d_pick() {
        let mut a = wall_picked_in_3d();
        a.view3d = false;
        let steps = a.ed.undo_len();
        delete_visible(&mut a);
        assert_eq!(a.ed.walls.len(), 1, "{:?}", a.ed.status);
        assert_eq!(a.ed.undo_len(), steps);
    }

    #[test]
    fn scan_fills_the_ring_between_edges() {
        let square = vec![
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ];
        let lines = scan(&[square.clone()], 2.0, 0.0);
        assert_eq!(lines.len(), 5, "y = 0, 2, 4, 6, 8");
        for (a, b) in &lines {
            assert!((a.x - 0.0).abs() < 1e-3 && (b.x - 10.0).abs() < 1e-3);
        }
        // an even-odd hole splits the line in two
        let hole = vec![
            Pos2::new(4.0, 3.0),
            Pos2::new(6.0, 3.0),
            Pos2::new(6.0, 5.0),
            Pos2::new(4.0, 5.0),
        ];
        assert_eq!(scan(&[square.clone(), hole], 2.0, 0.0).len(), 6);
        assert!(scan(&[square], 0.0, 0.0).is_empty());
    }

    #[test]
    fn override_colors_and_scene_tags() {
        assert_eq!(rgb_of(color_of(0x12_34_56)), 0x12_34_56);
        let o = GfxOverride {
            fill: Some(0xFF0000),
            ..Default::default()
        };
        assert_eq!(fill_with(o, Color32::BLUE), color_of(0xFF0000));
        assert_eq!(mesh_tag(o, 1), Some(tag_rgb([255, 0, 0], false)));
        assert_eq!(mesh_tag(GfxOverride::default(), 1), None);
        let clear = GfxOverride {
            transparency: Some(40),
            ..o
        };
        assert_eq!(mesh_tag(clear, 1), Some(tag_rgb([255, 0, 0], true)));
    }

    /// A filter rule hides the wall in plan and removes it from the frame via `Vis`.
    #[test]
    fn filter_hides_walls_in_the_view_layer() {
        let mut e = ed();
        let lv = e.level;
        let t0 = e.types[0].id;
        let id = e
            .model
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t0,
                offset0: None,
            })
            .unwrap();
        e.reload().unwrap();
        let v = ViewKey::Plan(lv);
        let f = e
            .model
            .add_view_filter(&ViewFilterSpec {
                name: "F".into(),
                categories: vec!["wall".into()],
                any: false,
                rules: vec![FilterRule {
                    param: "id".into(),
                    op: RuleOp::Eq,
                    value: id.to_string(),
                }],
            })
            .unwrap();
        assert!(run(&mut e, move |m| m.edit_view_gfx(v, |g| {
            g.filters = vec![FilterUse {
                filter: f,
                enabled: true,
                visible: false,
                over: GfxOverride::default(),
            }];
        })));
        let l = GfxLayer::build(&e, v);
        assert!(l.hide.contains(&Sel::Wall(id)));
        assert!(l.hide_parts.contains(&(Kind::Wall, id)));
        let vis = crate::isolate::Vis {
            gfx: Arc::new(l),
            ..Default::default()
        };
        assert!(vis.any());
        assert!(!vis.visible(&e, Sel::Wall(id)));
    }

    fn wall(e: &mut Editor, y: f64) -> i64 {
        let (level, wall_type) = (e.level, e.types[0].id);
        let id = e
            .model
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, y),
                b: (4.0, y),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type,
                offset0: None,
            })
            .unwrap();
        e.reload().unwrap();
        id
    }

    fn hide_wall(e: &mut Editor, id: i64, v: ViewKey) {
        let f = e
            .model
            .add_view_filter(&ViewFilterSpec {
                name: "F".into(),
                categories: vec!["wall".into()],
                any: false,
                rules: vec![FilterRule {
                    param: "id".into(),
                    op: RuleOp::Eq,
                    value: id.to_string(),
                }],
            })
            .unwrap();
        assert!(run(e, move |m| m.edit_view_gfx(v, |g| {
            g.filters = vec![FilterUse {
                filter: f,
                enabled: true,
                visible: false,
                over: GfxOverride::default(),
            }];
        })));
    }

    #[test]
    fn delete_keeps_what_the_view_hides_and_says_so() {
        let mut e = ed();
        let (a, b) = (wall(&mut e, 0.0), wall(&mut e, 5.0));
        let v = ViewKey::Plan(e.level);
        hide_wall(&mut e, a, v);
        e.selection = vec![Sel::Wall(a), Sel::Wall(b)];
        delete_in_view(&mut e, v, |_| false);
        assert!(e.walls.iter().any(|w| w.id == a), "hidden wall stays");
        assert!(!e.walls.iter().any(|w| w.id == b), "visible wall goes");
        assert!(e.status.is_some());
        // only hidden elements are selected: nothing to delete
        e.selection = vec![Sel::Wall(a)];
        e.status = None;
        delete_in_view(&mut e, v, |_| false);
        assert!(e.walls.iter().any(|w| w.id == a));
        assert!(e.selection.is_empty());
        assert!(e.status.is_some());
    }

    #[test]
    fn stamp_follows_visibility_not_every_edit() {
        let mut e = ed();
        let a = wall(&mut e, 0.0);
        let v = ViewKey::Plan(e.level);
        let first = layer(&e, v).stamp;
        assert_ne!(first, 0);
        wall(&mut e, 5.0);
        assert_eq!(
            layer(&e, v).stamp,
            first,
            "an edit with the same graphics keeps the scene key"
        );
        hide_wall(&mut e, a, v);
        assert_ne!(layer(&e, v).stamp, first);
    }

    #[test]
    fn other_level_meshes_follow_the_view() {
        let mut p = ViewPaint {
            any_hidden: true,
            ..Default::default()
        };
        p.elems.insert(
            ("wall", 1),
            ElemPaint {
                hidden: true,
                over: GfxOverride::default(),
            },
        );
        let red = GfxOverride {
            fill: Some(0xFF0000),
            ..Default::default()
        };
        p.elems.insert(
            ("wall", 2),
            ElemPaint {
                hidden: false,
                over: red,
            },
        );
        let l = GfxLayer {
            paint: Arc::new(p),
            ..Default::default()
        };
        assert_eq!(l.level_tag(Some(Sel::Wall(1)), 7), None);
        assert_eq!(
            l.level_tag(Some(Sel::Wall(2)), 7),
            Some(tag_rgb([255, 0, 0], false))
        );
        assert_eq!(l.level_tag(Some(Sel::Wall(3)), 7), Some(7));
        assert_eq!(l.level_tag(None, 7), Some(7));
    }

    #[test]
    fn hatch_of_a_material_without_a_row_is_its_class_default() {
        let l = GfxLayer::default();
        let brick = crate::app2_fx::fx("gfx_brick");
        assert_eq!(l.mat_hatch(&brick), material_default_hatch(&brick));
    }

    #[test]
    fn model_refusals_read_as_plain_messages() {
        let conflict = |s: &str| error_text(&Error::Conflict(s.to_string()));
        assert_eq!(
            conflict("view_filter 'A' already exists"),
            t("view_gfx.name_taken")
        );
        assert_ne!(conflict("something else"), t("view_gfx.name_taken"));
    }

    fn mat(name: &str, color: Option<u32>, hatch: &str, density: Option<f64>) -> MaterialSpec {
        MaterialSpec {
            name: name.into(),
            color,
            hatch: hatch.into(),
            density,
            conductivity: None,
            specific_heat: None,
            note: String::new(),
        }
    }

    fn texts(e: &Error) -> (String, String) {
        let (en, ru) = (error_text_in(false, e), error_text_in(true, e));
        for s in [&en, &ru] {
            assert!(!s.contains("view_gfx.err"), "missing catalog key: {s}");
            assert!(!s.contains('{'), "unfilled placeholder: {s}");
            assert!(!s.contains("Not done") && !s.contains("view_gfx.error"), "{s}");
        }
        assert_ne!(en, ru);
        (en, ru)
    }

    #[test]
    fn every_material_refusal_has_ru_and_en_text() {
        let mut e = ed();
        let m = &mut e.model;
        let ok = ddc_model::MATERIAL_HATCHES[0];
        let bad = [
            mat("", None, ok, None),
            mat("M", Some(0x100_0000), ok, None),
            mat("M", None, "no_such_hatch", None),
            mat("M", None, ok, Some(-1.0)),
        ];
        let mut seen = Vec::new();
        for s in &bad {
            let e = m.add_material(s).unwrap_err();
            assert!(matches!(e, Error::Field(_)), "{e:?}");
            seen.push(texts(&e).0);
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), bad.len(), "each refusal has its own text");
        let hatch = texts(&Error::Field(ddc_model::FieldErr::MaterialHatch("zz".into())));
        assert!(hatch.0.contains("zz") && hatch.1.contains("zz"));
    }

    #[test]
    fn template_empty_name_is_localized() {
        let mut e = ed();
        let m = &mut e.model;
        let e = m.add_view_template("  ", &[], &Default::default()).unwrap_err();
        assert!(matches!(e, Error::Field(ddc_model::FieldErr::TemplateName)));
        let (en, ru) = texts(&e);
        assert!(!en.contains("template name is empty"));
        assert!(!ru.is_empty());
    }

    #[test]
    fn unknown_error_keeps_f5_error() {
        let e = Error::Invalid("odd thing".into());
        let (en, ru) = (error_text_in(false, &e), error_text_in(true, &e));
        assert_eq!(en, crate::tr_lang(false, "view_gfx.error", &[("msg", "invalid: odd thing")]));
        assert_eq!(ru, crate::tr_lang(true, "view_gfx.error", &[("msg", "invalid: odd thing")]));
    }

    #[test]
    fn filter_refusal_localized() {
        let mut e = ed();
        let m = &mut e.model;
        let mut s = ViewFilterSpec {
            name: String::new(),
            categories: vec![],
            any: false,
            rules: vec![],
        };
        let mut all = Vec::new();
        all.push(m.add_view_filter(&s).unwrap_err());
        s.name = "F".into();
        s.categories = vec!["no_such_category".into()];
        all.push(m.add_view_filter(&s).unwrap_err());
        s.categories.clear();
        s.rules = vec![FilterRule { param: " ".into(), op: RuleOp::Eq, value: String::new() }];
        all.push(m.add_view_filter(&s).unwrap_err());
        for e in &all {
            assert!(matches!(e, Error::Field(_)), "{e:?}");
            texts(e);
        }
    }

    /// Edit template under the lock note opens the templates window: by pointer, and by
    /// Tab then Enter while the tab itself is locked.
    fn edit_template_opens_the_window(lang: ddc_ui::Lang) {
        use crate::layout_contract::{app_in, driven_ctx, pass, Probe};
        let ru = lang == ddc_ui::Lang::Ru;
        crate::lang_on_this_thread(ru);
        let label = ddc_ui::Catalog::new(lang).t("view_template.edit_template");
        for by_tab in [false, true] {
            template_win::reset();
            let mut app = app_in(lang);
            let plan = ViewKey::Plan(app.ed.level);
            let all = ddc_model::TemplateField::ALL;
            let t = app.ed.model.template_from_view("Plans", plan, &all).unwrap();
            app.ed.model.link_view_template(Some(t), &[plan]).unwrap();
            vv_win::open_for_layout(false);
            let (ctx, mut th) = driven_ctx();
            let wid = egui::Id::new(("ddc.dialog", "view.vv"));
            let mut step = |ev: Vec<egui::Event>| -> Probe {
                pass(&ctx, &mut th, wid, ev, &mut |c| frame(&mut app, c))
            };
            let mut p = Probe::default();
            for _ in 0..4 {
                p = step(vec![]);
            }
            let tag = format!("{lang:?} by_tab={by_tab}");
            let find = |p: &Probe| p.controls.iter().find(|c| c.label == label).cloned();
            let btn = find(&p).unwrap_or_else(|| panic!("{tag}: no Edit template button"));
            assert!(!template_win::is_open(), "{tag}: the templates window starts closed");
            let key = |key: egui::Key| -> Vec<egui::Event> {
                [true, false]
                    .into_iter()
                    .map(|pressed| egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    })
                    .collect()
            };
            if by_tab {
                let mut reached = false;
                for _ in 0..80 {
                    let p = step(key(egui::Key::Tab));
                    let focus = ctx.memory(|m| m.focused()).map(|i| i.value());
                    if find(&p).map(|c| c.id) == focus && focus.is_some() {
                        reached = true;
                        break;
                    }
                }
                assert!(reached, "{tag}: Tab never reached Edit template");
                assert!(!template_win::is_open(), "{tag}: Tab alone opens nothing");
                step(key(egui::Key::Enter));
            } else {
                let at = btn.rect.center();
                let ev = |ev| egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: ev,
                    modifiers: egui::Modifiers::NONE,
                };
                step(vec![egui::Event::PointerMoved(at)]);
                step(vec![ev(true)]);
                step(vec![ev(false)]);
            }
            step(vec![]);
            assert!(template_win::is_open(), "{tag}: Edit template did not open the window");
        }
    }

    #[test]
    fn edit_template_opens_the_window_en() {
        edit_template_opens_the_window(ddc_ui::Lang::En);
    }

    #[test]
    fn edit_template_opens_the_window_ru() {
        edit_template_opens_the_window(ddc_ui::Lang::Ru);
    }
}
