// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan preview: what `build` harvests on the worker from the scratch copy and the untouched base,
//! and how the UI thread overlays it on the plan and the 3D view. The live model is never touched:
//! changed and deleted elements are hidden through the view's `Vis`, the rest is drawn on top.

use super::plan::ElemDiff;
use super::ghost;
use super::rows;
use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::scene3d;
use crate::editor::bodies::{self, BodyKind};
use crate::editor::{Editor, Sel};
use crate::grips::Kind;
use crate::isolate;
use crate::scene_ids::AI_PREVIEW;
use crate::ui_kit::Tokens;
use ddc_geom::{v2, V2};
use ddc_model::{ElKey, RowKind};
use ddc_render::glam::Vec3;
use ddc_render::{tag_rgb, Aabb, SceneMesh};
use ddc_ui::diff_kind::DiffKind;
use ddc_ui::theme::{Mode, Tokens as DesignTokens};
use egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// Above this the preview shows the frame and a counter: the card still lists every element.
pub fn max_elements() -> usize {
    ddc_ai::limits::PREVIEW_ELEMENTS.get_us()
}
/// Vertices per merged mesh: one draw per kind and level keeps a large plan cheap.
const MESH_VERTICES: usize = 400_000;
/// Live geometry on another level cannot be hidden, so the shell sits just outside it.
const INFLATE_M: f32 = 0.005;
const HOT_BOXES: usize = 200;
/// Mirror of the canvas overlay sizes, one name each with the shared overlay module.
const OUTLINE_PX: f32 = 2.0;
const THIN_PX: f32 = 1.0;
const DASH: (f32, f32) = (6.0, 4.0);
const HATCH_STEP_PX: f32 = 7.0;
const HATCH_MIN_PX: f32 = 3.0;
const HATCH_FRAME_MAX: u32 = ddc_ai::limits::HATCH_FRAME.u32v();

static REV: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct PreviewMesh {
    pub kind: RowKind,
    pub level: i64,
    /// Absolute z: the view subtracts its active level.
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

#[derive(Clone, Debug)]
pub struct PlanMark {
    pub kind: RowKind,
    pub level: i64,
    pub key: ElKey,
    pub outline: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, Default)]
pub struct PreviewSet {
    pub rev: u64,
    pub meshes: Vec<PreviewMesh>,
    pub marks: Vec<PlanMark>,
    pub hide: HashSet<Sel>,
    pub hide_parts: HashSet<isolate::Part>,
    pub bounds: Option<Aabb>,
    /// Absolute box of every previewed element, for hover and zoom.
    pub boxes: HashMap<ElKey, Aabb>,
    /// Elements above `max_elements()`: in the card, without their own shape on the canvas.
    pub omitted: usize,
    /// Outline and box a changed element had before, for the dashed "was here" form.
    pub before_marks: Vec<PlanMark>,
    pub before_boxes: HashMap<ElKey, Aabb>,
    /// Change kind of every previewed element, for ghost boxes in 3D.
    pub kinds: HashMap<ElKey, RowKind>,
}

fn next_rev() -> u64 {
    REV.fetch_add(1, Ordering::Relaxed) + 1
}

/// Table of the model row behind a scene part.
fn table_of_cat(c: Cat) -> Option<&'static str> {
    Some(match c {
        Cat::Wall => "wall",
        Cat::Opening => "opening",
        Cat::Curtain | Cat::Panel => "curtain_wall",
        Cat::Slab => "slab",
        Cat::Room => "room",
        Cat::Mep => "mep_run",
        Cat::Comp => "comp",
        Cat::Mesh => "mesh",
        Cat::Column => "col",
        Cat::Beam => "beam",
        Cat::Ceiling => "ceiling",
        Cat::Roof => "roof",
        Cat::Stair => "stair",
        Cat::Railing => "railing",
        _ => return None,
    })
}

fn table_of_sel(s: Sel) -> Option<(&'static str, i64)> {
    Some(match s {
        Sel::Wall(id) => ("wall", id),
        Sel::Opening(id) => ("opening", id),
        Sel::Curtain(id) | Sel::Panel(id, _) => ("curtain_wall", id),
        Sel::Slab(id) => ("slab", id),
        Sel::Room(id) => ("room", id),
        Sel::Mep(id) => ("mep_run", id),
        Sel::Comp(id) => ("comp", id),
        Sel::Body(BodyKind::Column, id) => ("col", id),
        Sel::Body(BodyKind::Beam, id) => ("beam", id),
        Sel::Body(BodyKind::Ceiling, id) => ("ceiling", id),
        Sel::Body(BodyKind::Mesh, id) => ("mesh", id),
        Sel::Mass(_) | Sel::Ext(..) | Sel::Anno(_) => return None,
    })
}

/// The card key of a selected element; `None` for kinds the preview has no row for.
pub fn key_of(s: Sel) -> Option<ElKey> {
    table_of_sel(s).map(|(t, id)| (t.to_string(), id))
}

/// What the live view hides for this element, by its own selection and by its grip kind.
pub fn hide_of(key: &ElKey) -> (Option<Sel>, Option<isolate::Part>) {
    let id = key.1;
    let (sel, kind) = match key.0.as_str() {
        "wall" => (Some(Sel::Wall(id)), Some(Kind::Wall)),
        "opening" => (Some(Sel::Opening(id)), None),
        "curtain_wall" => (Some(Sel::Curtain(id)), None),
        "slab" => (Some(Sel::Slab(id)), Some(Kind::Slab)),
        "room" => (Some(Sel::Room(id)), None),
        "mep_run" => (Some(Sel::Mep(id)), Some(Kind::Mep)),
        "comp" => (Some(Sel::Comp(id)), Some(Kind::Comp)),
        "col" => (Some(Sel::Body(BodyKind::Column, id)), Some(Kind::Column)),
        "beam" => (Some(Sel::Body(BodyKind::Beam, id)), Some(Kind::Beam)),
        "ceiling" => (Some(Sel::Body(BodyKind::Ceiling, id)), Some(Kind::Ceiling)),
        "mesh" => (Some(Sel::Body(BodyKind::Mesh, id)), None),
        "roof" => (None, Some(Kind::Roof)),
        "stair" => (None, Some(Kind::Stair)),
        "railing" => (None, Some(Kind::Railing)),
        _ => (None, None),
    };
    (sel, kind.map(|k| (k, id)))
}

fn key_of_element(ed: &Editor, element: u32, targets: &[Target]) -> Option<ElKey> {
    if let Some(t) = scene3d::target_of(element, targets) {
        let t: Target = t?;
        return table_of_cat(t.cat).map(|table| (table.to_string(), t.id));
    }
    let (table, id) = table_of_sel(ed.sel_of_element(element)?)?;
    Some((table.to_string(), id))
}

/// Grows the shell about its own centre, each side by `by`.
fn inflate(positions: &mut [[f32; 3]], by: f32) {
    let mut b = Aabb::EMPTY;
    positions.iter().for_each(|p| b.grow(Vec3::from(*p)));
    if b.is_empty() {
        return;
    }
    let c = (b.min + b.max) * 0.5;
    let half = ((b.max - b.min) * 0.5).max(Vec3::splat(1e-4));
    let k = (half + Vec3::splat(by)) / half;
    for p in positions.iter_mut() {
        let q = c + (Vec3::from(*p) - c) * k;
        *p = q.to_array();
    }
}

#[derive(Default)]
struct Bodies {
    positions: Vec<[f32; 3]>,
    triangles: Vec<[u32; 3]>,
}

impl Bodies {
    fn push(&mut self, positions: impl Iterator<Item = [f32; 3]>, triangles: &[[u32; 3]]) {
        let base = self.positions.len() as u32;
        self.positions.extend(positions);
        self.triangles
            .extend(triangles.iter().map(|t| t.map(|i| i + base)));
    }
}

/// Puts one element's shell into the last mesh of its kind and level, or starts a new one.
fn merge(out: &mut Vec<PreviewMesh>, kind: RowKind, level: i64, mut body: Bodies) {
    if kind != RowKind::Created {
        inflate(&mut body.positions, INFLATE_M);
    }
    let slot = out.iter().rposition(|m| {
        m.kind == kind
            && m.level == level
            && m.positions.len() + body.positions.len() <= MESH_VERTICES
    });
    match slot {
        Some(i) => {
            let m = &mut out[i];
            let base = m.positions.len() as u32;
            m.positions.extend_from_slice(&body.positions);
            m.triangles
                .extend(body.triangles.iter().map(|t| t.map(|x| x + base)));
        }
        None => out.push(PreviewMesh {
            kind,
            level,
            positions: body.positions,
            triangles: body.triangles,
        }),
    }
}

fn hull_of(positions: &[[f32; 3]]) -> Vec<(f64, f64)> {
    let pts: Vec<V2> = positions
        .iter()
        .map(|p| v2(f64::from(p[0]), f64::from(p[1])))
        .collect();
    bodies::hull(pts).into_iter().map(|p| (p.x, p.y)).collect()
}

fn ring_area(r: &[V2]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| r[i].x * r[(i + 1) % n].y - r[(i + 1) % n].x * r[i].y)
        .sum::<f64>()
        .abs()
        / 2.0
}

/// Wall outline: joined layer contours for a straight wall, its largest layer for an arc whose
/// hull would be wrong.
fn wall_outline(ed: &Editor, i: usize) -> Vec<(f64, f64)> {
    let Some(layers) = ed.plan.get(i) else {
        return vec![];
    };
    let arc = ed.walls.get(i).is_some_and(|w| w.bulge != 0.0);
    if arc {
        let big = layers
            .iter()
            .max_by(|a, b| ring_area(a).total_cmp(&ring_area(b)));
        return big
            .map(|r| r.iter().map(|p| (p.x, p.y)).collect())
            .unwrap_or_default();
    }
    bodies::hull(layers.iter().flatten().copied().collect())
        .into_iter()
        .map(|p| (p.x, p.y))
        .collect()
}

/// An opening has no contour of its own: its span on the host's axis, as thick as the host.
fn opening_outline(ed: &Editor, id: i64) -> Vec<(f64, f64)> {
    let Some(o) = ed.openings.iter().find(|o| o.id == id) else {
        return vec![];
    };
    let Some(i) = ed.walls.iter().position(|w| w.id == o.wall) else {
        return vec![];
    };
    let w = &ed.walls[i];
    let len = v2(w.b.x - w.a.x, w.b.y - w.a.y).len();
    if len < 1e-9 {
        return vec![];
    }
    let d = v2((w.b.x - w.a.x) / len, (w.b.y - w.a.y) / len);
    let n = v2(-d.y, d.x);
    let half = ed
        .types
        .iter()
        .find(|t| t.id == w.wall_type)
        .map_or(0.1, |t| t.thickness() / 2.0);
    let at = |along: f64, side: f64| {
        (
            w.a.x + d.x * along + n.x * side,
            w.a.y + d.y * along + n.y * side,
        )
    };
    vec![
        at(o.along, -half),
        at(o.along + o.width, -half),
        at(o.along + o.width, half),
        at(o.along, half),
    ]
}

fn outer_ring(rings: &[Vec<V2>]) -> Vec<(f64, f64)> {
    rings
        .first()
        .map(|r| r.iter().map(|p| (p.x, p.y)).collect())
        .unwrap_or_default()
}

/// Outlines that come from the element itself; the rest takes the hull of its mesh.
fn own_outline(ed: &Editor, key: &ElKey) -> Vec<(f64, f64)> {
    let id = key.1;
    match key.0.as_str() {
        "wall" => ed
            .walls
            .iter()
            .position(|w| w.id == id)
            .map(|i| wall_outline(ed, i))
            .unwrap_or_default(),
        "opening" => opening_outline(ed, id),
        "slab" => ed
            .slabs
            .iter()
            .find(|s| s.id == id)
            .map(|s| outer_ring(&s.rings))
            .unwrap_or_default(),
        "room" => ed
            .rooms
            .iter()
            .find(|r| r.id == id)
            .map(|r| outer_ring(&r.rings))
            .unwrap_or_default(),
        _ => vec![],
    }
}

fn grow_box(boxes: &mut HashMap<ElKey, Aabb>, key: &ElKey, b: &Aabb) {
    boxes.entry(key.clone()).or_insert(Aabb::EMPTY).union(b);
}

/// Every wanted element of `ed`, level by level: `set_level` changes which parts carry their own
/// numbers, everything else in the scene is `OTHER_LEVEL`.
fn harvest(ed: &mut Editor, wanted: &HashMap<ElKey, RowKind>, out: &mut PreviewSet) {
    if wanted.is_empty() {
        return;
    }
    let keep = ed.level;
    let span = ddc_model::perf::span("preview.rows");
    if ed.reload().is_err() {
        return;
    }
    let level_of: HashMap<ElKey, i64> = rows::rows(ed)
        .iter()
        .map(|r| ((rows::key_of(r).table().to_string(), r.id()), r.level))
        .filter(|(k, _)| wanted.contains_key(k))
        .collect();
    let mut levels: BTreeSet<i64> = level_of.values().copied().collect();
    if level_of.len() < wanted.len() {
        levels.insert(keep);
    }
    drop(span);
    // the one-level scenes below must not stay in the editor's live 3D cache
    let live = std::mem::take(&mut ed.big);
    for lv in levels {
        let span = ddc_model::perf::span("preview.level");
        ed.set_level(lv);
        drop(span);
        if ed.level != lv {
            continue;
        }
        let dz = ed.elevation as f32;
        // only this level's parts are read below; the others would be built for nothing
        let span = ddc_model::perf::span("preview.compose");
        ed.scene_one_level = true;
        let (scene, targets) = scene3d::compose(ed);
        ed.scene_one_level = false;
        drop(span);
        let _s = ddc_model::perf::span("preview.collect");
        let mut found: HashMap<ElKey, Bodies> = HashMap::new();
        for m in &scene.meshes {
            let Some(key) = key_of_element(ed, m.element, &targets) else {
                continue;
            };
            if !wanted.contains_key(&key) || level_of.get(&key).is_some_and(|l| *l != lv) {
                continue;
            }
            let shifted = m.positions.iter().map(|p| [p[0], p[1], p[2] + dz]);
            found.entry(key).or_default().push(shifted, &m.triangles);
        }
        for g in &scene.instanced {
            for inst in g.inst.iter() {
                let Some(key) = key_of_element(ed, inst.element, &targets) else {
                    continue;
                };
                if !wanted.contains_key(&key) || level_of.get(&key).is_some_and(|l| *l != lv) {
                    continue;
                }
                let placed = g.proto.positions.iter().map(|p| {
                    let q = inst.m.transform_point3(Vec3::from(*p));
                    [q.x, q.y, q.z + dz]
                });
                found
                    .entry(key)
                    .or_default()
                    .push(placed, &g.proto.triangles);
            }
        }
        let mut marked: HashSet<ElKey> = HashSet::new();
        for (key, body) in found {
            let kind = wanted[&key];
            let mut own = own_outline(ed, &key);
            if own.len() < 2 {
                own = hull_of(&body.positions);
            }
            let mut b = Aabb::EMPTY;
            body.positions.iter().for_each(|p| b.grow(Vec3::from(*p)));
            grow_box(&mut out.boxes, &key, &b);
            if own.len() >= 2 {
                out.marks.push(PlanMark {
                    kind,
                    level: lv,
                    key: key.clone(),
                    outline: own,
                });
            }
            marked.insert(key);
            merge(&mut out.meshes, kind, lv, body);
        }
        // rooms and other plan-only elements have no 3D body
        for (key, kind) in wanted {
            if marked.contains(key) || level_of.get(key).is_some_and(|l| *l != lv) {
                continue;
            }
            let own = own_outline(ed, key);
            if own.len() < 2 {
                continue;
            }
            let mut b = Aabb::EMPTY;
            for (x, y) in &own {
                b.grow(Vec3::new(*x as f32, *y as f32, dz));
                b.grow(Vec3::new(*x as f32, *y as f32, dz + ed.height as f32));
            }
            grow_box(&mut out.boxes, key, &b);
            out.marks.push(PlanMark {
                kind: *kind,
                level: lv,
                key: key.clone(),
                outline: own,
            });
        }
    }
    ed.big = live;
    ed.set_level(keep);
}

/// `after` is the scratch copy with the plan on it, `before` the untouched base, needed only
/// when the plan changes or deletes. Both may switch level and come back to the one they had.
pub fn build(after: &mut Editor, before: Option<&mut Editor>, diff: &[ElemDiff]) -> PreviewSet {
    let mut out = PreviewSet {
        rev: next_rev(),
        ..PreviewSet::default()
    };
    let elems: Vec<&ElemDiff> = diff
        .iter()
        .filter(|e| rows::ELEMENT_TABLES.contains(&e.key.0.as_str()))
        .collect();
    let max = max_elements();
    out.omitted = elems.len().saturating_sub(max);
    let mut now: HashMap<ElKey, RowKind> = HashMap::new();
    let mut gone: HashMap<ElKey, RowKind> = HashMap::new();
    for e in elems.iter().take(max) {
        out.kinds.insert(e.key.clone(), e.kind);
        match e.kind {
            RowKind::Deleted => {
                gone.insert(e.key.clone(), e.kind);
            }
            _ => {
                now.insert(e.key.clone(), e.kind);
            }
        }
        if e.kind != RowKind::Created {
            let (sel, part) = hide_of(&e.key);
            out.hide.extend(sel);
            out.hide_parts.extend(part);
        }
    }
    harvest(after, &now, &mut out);
    if let Some(before) = before {
        harvest(before, &gone, &mut out);
        let was: HashMap<ElKey, RowKind> = now
            .iter()
            .filter(|(_, k)| **k == RowKind::Changed)
            .map(|(key, k)| (key.clone(), *k))
            .collect();
        let mut prior = PreviewSet::default();
        harvest(before, &was, &mut prior);
        out.before_marks = prior.marks;
        out.before_boxes = prior.boxes;
    }
    let mut all = Aabb::EMPTY;
    out.boxes.values().for_each(|b| all.union(b));
    out.bounds = (!all.is_empty()).then_some(all);
    out
}

// ---- UI thread ----

/// Palette names of the shared overlay module, read from the same tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub ai_created: Color32,
    pub ai_changed: Color32,
    pub ai_deleted: Color32,
    pub selection: Color32,
}

/// The shared change kind (`ddc_ui::diff_kind`) of a plan row.
pub const fn diff_kind(k: RowKind) -> DiffKind {
    match k {
        RowKind::Created => DiffKind::Added,
        RowKind::Changed => DiffKind::Changed,
        RowKind::Deleted => DiffKind::Deleted,
    }
}

impl Palette {
    pub fn ai(&self, k: RowKind) -> Color32 {
        match diff_kind(k) {
            DiffKind::Added => self.ai_created,
            DiffKind::Changed => self.ai_changed,
            _ => self.ai_deleted,
        }
    }
}

/// Legend sign of a change kind.
pub const fn sign(k: RowKind) -> &'static str {
    diff_kind(k).sign()
}

pub fn pal(dark: bool) -> &'static Palette {
    static PALS: OnceLock<[Palette; 2]> = OnceLock::new();
    let all = PALS.get_or_init(|| {
        let t = DesignTokens::embedded().expect("embedded tokens are checked by tests");
        let of = |mode: Mode| {
            let c = |name: &str| {
                t.color(mode, name)
                    .expect("preview colours are in tokens.json")
            };
            Palette {
                ai_created: c(DiffKind::Added.token()),
                ai_changed: c(DiffKind::Changed.token()),
                ai_deleted: c(DiffKind::Deleted.token()),
                selection: c("selection"),
            }
        };
        [of(Mode::Light), of(Mode::Dark)]
    });
    &all[usize::from(dark)]
}

#[derive(Default)]
struct Shown {
    set: Option<Arc<PreviewSet>>,
    dark: bool,
    hot: HashSet<ElKey>,
    /// Element of the picked XLSX preview row and the frame that said so.
    row_hot: Option<(ElKey, u64)>,
    in_3d: bool,
    /// B is held: the model as it is, without the draft on top.
    before: bool,
}

thread_local! {
    static SHOWN: RefCell<Shown> = RefCell::new(Shown::default());
}

/// Once a frame from the panel: the set the views overlay, `None` when there is nothing to show.
pub fn publish(set: Option<Arc<PreviewSet>>, dark: bool) {
    SHOWN.with(|s| {
        let mut s = s.borrow_mut();
        s.set = set.filter(|p| !p.meshes.is_empty() || !p.marks.is_empty() || !p.hide.is_empty());
        s.dark = dark;
    });
}

/// What the card row under the pointer or the keyboard cursor points at.
pub fn set_hot(keys: HashSet<ElKey>) {
    SHOWN.with(|s| s.borrow_mut().hot = keys);
}

/// The XLSX preview's picked row, lit like a card row. The window says it every frame it is
/// open; it lapses a frame after the window stops (closed, folded, no row picked).
pub fn set_row_hot(ctx: &egui::Context, key: Option<ElKey>) {
    let at = ctx.cumulative_pass_nr();
    SHOWN.with(|s| s.borrow_mut().row_hot = key.map(|k| (k, at)));
}

/// Everything lit this frame: the card row and the picked XLSX row.
pub fn lit(ctx: &egui::Context) -> HashSet<ElKey> {
    let now = ctx.cumulative_pass_nr();
    SHOWN.with(|s| {
        let s = s.borrow();
        let mut all = s.hot.clone();
        if let Some((k, at)) = &s.row_hot {
            if now <= at + 2 {
                all.insert(k.clone());
            }
        }
        all
    })
}

/// Hold-B state from the panel: while true the views show the model as it is.
pub fn set_before(on: bool) {
    SHOWN.with(|s| s.borrow_mut().before = on);
}

pub fn before_mode() -> bool {
    SHOWN.with(|s| {
        let s = s.borrow();
        s.before && s.set.is_some()
    })
}

/// The 3D view is on screen in this frame.
pub fn note_3d() {
    SHOWN.with(|s| s.borrow_mut().in_3d = true);
}

fn shown() -> Option<Arc<PreviewSet>> {
    SHOWN.with(|s| s.borrow().set.clone())
}

/// Part of the scene key: 0 without a preview. The theme is in it because the colours are baked
/// into the meshes.
pub fn rev() -> u64 {
    SHOWN.with(|s| {
        let s = s.borrow();
        s.set
            .as_ref()
            .map_or(0, |p| p.rev << 2 | u64::from(s.before) << 1 | u64::from(s.dark))
    })
}

/// The live versions of changed and deleted elements stay out of the 3D scene, except while
/// Before is held.
pub fn hide_into(vis: &mut isolate::Vis) {
    if before_mode() {
        return;
    }
    if let Some(p) = shown() {
        vis.hidden.extend(p.hide.iter().copied());
        vis.hidden_parts.extend(p.hide_parts.iter().copied());
    }
}

/// Puts the translucent shells on the base scene, always after dropping the previous ones: the
/// base may already carry them.
pub fn apply(ed: &Editor, meshes: &mut Vec<Arc<SceneMesh>>) {
    meshes.retain(|m| m.element != AI_PREVIEW);
    if before_mode() {
        return;
    }
    let Some(p) = shown() else {
        return;
    };
    let dark = SHOWN.with(|s| s.borrow().dark);
    let dz = ed.elevation as f32;
    for m in &p.meshes {
        let c = pal(dark).ai(m.kind);
        meshes.push(Arc::new(SceneMesh {
            element: AI_PREVIEW,
            tag: tag_rgb([c.r(), c.g(), c.b()], true),
            positions: m
                .positions
                .iter()
                .map(|q| [q[0], q[1], q[2] - dz])
                .collect(),
            triangles: m.triangles.clone(),
        }));
    }
}

/// Diagonal hatch of a ring by scan lines in the rotated frame `u = x + y`; mirror of the shared
/// overlay hatch, the same budget per frame.
fn hatch(p: &Painter, ring: &[Pos2], st: Stroke, clip: Rect, budget: &mut u32) {
    if ring.len() < 3 {
        return;
    }
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut bb = Rect::NOTHING;
    for q in ring {
        lo = lo.min(q.x + q.y);
        hi = hi.max(q.x + q.y);
        bb.extend_with(*q);
    }
    if !bb.intersects(clip) || bb.width().min(bb.height()) < HATCH_MIN_PX {
        return;
    }
    let mut u = (lo / HATCH_STEP_PX).ceil() * HATCH_STEP_PX;
    let mut hits: Vec<f32> = Vec::with_capacity(8);
    while u <= hi && *budget > 0 {
        hits.clear();
        for i in 0..ring.len() {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            let (ua, ub) = (a.x + a.y, b.x + b.y);
            if (ua <= u) != (ub <= u) {
                let k = (u - ua) / (ub - ua);
                hits.push(a.x + (b.x - a.x) * k);
            }
        }
        hits.sort_by(f32::total_cmp);
        for pair in hits.chunks_exact(2) {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            p.line_segment(
                [
                    Pos2::new(pair[0], u - pair[0]),
                    Pos2::new(pair[1], u - pair[1]),
                ],
                st,
            );
        }
        u += HATCH_STEP_PX;
    }
}

fn closed(ring: &[Pos2]) -> Vec<Pos2> {
    let mut line = Vec::with_capacity(ring.len() + 1);
    line.extend_from_slice(ring);
    if ring.len() > 2 {
        line.push(ring[0]);
    }
    line
}

fn outline(p: &Painter, line: Vec<Pos2>, st: Stroke, form: ghost::LineForm) {
    match form {
        ghost::LineForm::Solid => {
            p.add(Shape::line(line, st));
        }
        ghost::LineForm::Dashed => {
            p.extend(Shape::dashed_line(&line, st, DASH.0, DASH.1));
        }
    }
}

/// The draft form of one element: body tint, then its outline (solid for added and changed,
/// dashed for a deleted ghost). A ring that is not convex takes a hatch instead of a tint.
fn mark(
    p: &Painter,
    ring: &[Pos2],
    k: RowKind,
    clip: Rect,
    ov: &Palette,
    budget: &mut u32,
    (hot, pulse): (bool, f32),
) {
    let Some(st) = ghost::ghost_style(diff_kind(k), hot, false) else {
        return;
    };
    let col = ov.ai(k);
    let fill = ghost::fill_color(col, &st, pulse);
    if fill.a() > 0 {
        if ghost::is_convex(ring) {
            p.add(Shape::convex_polygon(ring.to_vec(), fill, Stroke::NONE));
        } else {
            let hatch_col = col.gamma_multiply((st.fill * 3.0 * pulse).min(1.0));
            hatch(p, ring, Stroke::new(THIN_PX, hatch_col), clip, budget);
        }
    }
    outline(p, closed(ring), Stroke::new(st.width_px, col), st.line);
}

/// On the plan: the draft form per kind (`ghost_style`) and the dashed before outline of changed
/// ones. The hot element gets the selection colour over its own form, an edge arrow when off screen.
pub fn paint_plan(p: &Painter, rect: Rect, ed: &Editor, _t: &Tokens, to_s: &dyn Fn(V2) -> Pos2) {
    let ov = pal(p.ctx().style().visuals.dark_mode);
    let hot = lit(p.ctx());
    paint_loose_hot(p, ed, &hot, shown().as_deref(), ov, to_s);
    let Some(set) = shown() else {
        return;
    };
    // Before held: the live plan is all there is
    if before_mode() {
        return;
    }
    let mut budget = HATCH_FRAME_MAX;
    let pulse = if hot.is_empty() {
        // one pulse when the ghost appears (flat with reduced motion)
        let (now, calm) = (p.ctx().input(|i| i.time), p.ctx().style().animation_time <= 0.0);
        let key = egui::Id::new("ddc.ghost.appear");
        let born = p.ctx().data_mut(|d| {
            let (at, last): (f64, f64) = d.get_temp(key).unwrap_or((now, now));
            // a gap of frames without a draft starts a new appearance
            let at = if now - last > 0.5 { now } else { at };
            d.insert_temp(key, (at, now));
            at
        });
        if !calm && now - born < ghost::APPEAR_S {
            p.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        }
        ghost::appear_pulse(now - born, calm)
    } else {
        p.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        ghost::pulse(p.ctx().input(|i| i.time))
    };
    let mut ring: Vec<Pos2> = Vec::new();
    let mut arrows: Vec<ghost::EdgeArrow> = Vec::new();
    let screen_ring = |ring: &mut Vec<Pos2>, m: &PlanMark| {
        ring.clear();
        ring.extend(m.outline.iter().map(|(x, y)| to_s(v2(*x, *y))));
        let mut bb = Rect::NOTHING;
        ring.iter().for_each(|q| bb.extend_with(*q));
        bb
    };
    // where a changed element stood before: dashed, under the new form
    for m in set.before_marks.iter().filter(|m| m.level == ed.level) {
        let is_hot = hot.contains(&m.key);
        let Some(st) = ghost::before_outline_style(is_hot, false) else {
            continue;
        };
        let bb = screen_ring(&mut ring, m);
        if bb.expand(OUTLINE_PX).intersects(rect) {
            let col = ov.ai_changed.gamma_multiply(0.7);
            outline(p, closed(&ring), Stroke::new(st.width_px, col), st.line);
        }
    }
    for m in set.marks.iter().filter(|m| m.level == ed.level) {
        let is_hot = hot.contains(&m.key);
        let bb = screen_ring(&mut ring, m);
        if !bb.expand(OUTLINE_PX).intersects(rect) {
            if is_hot && arrows.len() < HOT_BOXES {
                arrows.extend(ghost::edge_arrow(rect, bb.center(), ghost::ARROW_INSET_PX));
            }
            continue;
        }
        mark(p, &ring, m.kind, rect, ov, &mut budget, (is_hot, pulse));
        if is_hot {
            p.add(Shape::line(
                closed(&ring),
                Stroke::new(OUTLINE_PX + THIN_PX, ov.selection),
            ));
        }
    }
    for a in &arrows {
        ghost::paint_arrow(p, a, ov.selection);
    }
    if set.omitted > 0 {
        if let Some(b) = &set.bounds {
            let (a, c) = (
                to_s(v2(f64::from(b.min.x), f64::from(b.min.y))),
                to_s(v2(f64::from(b.max.x), f64::from(b.max.y))),
            );
            let frame = Rect::from_two_pos(a, c);
            let line = vec![
                frame.left_top(),
                frame.right_top(),
                frame.right_bottom(),
                frame.left_bottom(),
                frame.left_top(),
            ];
            p.extend(Shape::dashed_line(
                &line,
                Stroke::new(THIN_PX, ov.ai_changed),
                DASH.0,
                DASH.1,
            ));
        }
    }
}

/// Hot elements the preview has no mark for (no plan at all, or the context selection chip):
/// their own outline in the selection colour, so hovering works without a plan.
fn paint_loose_hot(
    p: &Painter,
    ed: &Editor,
    hot: &HashSet<ElKey>,
    set: Option<&PreviewSet>,
    ov: &Palette,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    for key in hot.iter().take(HOT_BOXES) {
        let marked = set.is_some_and(|s| s.marks.iter().any(|m| m.key == *key && m.level == ed.level));
        if marked {
            continue;
        }
        let own = own_outline(ed, key);
        if own.len() < 2 {
            continue;
        }
        let mut line: Vec<Pos2> = own.iter().map(|(x, y)| to_s(v2(*x, *y))).collect();
        if line.len() > 2 {
            line.push(line[0]);
        }
        p.add(Shape::line(line, Stroke::new(OUTLINE_PX + THIN_PX, ov.selection)));
    }
}

const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 3),
    (3, 2),
    (2, 0),
    (4, 5),
    (5, 7),
    (7, 6),
    (6, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

fn draw_box(p: &Painter, view: &crate::viewport::Viewport, b: &Aabb, dz: f32, st: Stroke) {
    draw_box_form(p, view, b, dz, st, ghost::LineForm::Solid);
}

fn draw_box_form(
    p: &Painter,
    view: &crate::viewport::Viewport,
    b: &Aabb,
    dz: f32,
    st: Stroke,
    form: ghost::LineForm,
) {
    let corner = |i: usize| {
        let x = if i & 1 == 0 { b.min.x } else { b.max.x };
        let y = if i & 2 == 0 { b.min.y } else { b.max.y };
        let z = if i & 4 == 0 { b.min.z } else { b.max.z };
        view.screen_of(Vec3::new(x, y, z - dz))
    };
    for (a, c) in BOX_EDGES {
        if let (Some(a), Some(c)) = (corner(a), corner(c)) {
            match form {
                ghost::LineForm::Solid => {
                    p.line_segment([a, c], st);
                }
                ghost::LineForm::Dashed => {
                    p.extend(Shape::dashed_line(&[a, c], st, DASH.0, DASH.1));
                }
            }
        }
    }
}

/// Screen point of a box centre, for the edge arrow.
fn box_center(view: &crate::viewport::Viewport, b: &Aabb, dz: f32) -> Option<Pos2> {
    let c = (b.min + b.max) * 0.5;
    view.screen_of(Vec3::new(c.x, c.y, c.z - dz))
}

/// In 3D the hot element gets a box drawn over the frame: a scene rebuild per hovered row would be
/// a hitch on a big model.
pub fn paint_3d(ctx: &egui::Context, view: &crate::viewport::Viewport, ed: &Editor) {
    let in_3d = SHOWN.with(|s| std::mem::take(&mut s.borrow_mut().in_3d));
    let (Some(set), true, Some(rect)) = (shown(), in_3d, view.frame_rect()) else {
        return;
    };
    if before_mode() {
        return;
    }
    let hot = lit(ctx);
    let ov = pal(ctx.style().visuals.dark_mode);
    let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("ddc.ai.preview3d"));
    // painted without an area: entered into the canvas order, or it would cover every window
    ddc_ui::z::canvas(ctx, layer);
    let p = Painter::new(ctx.clone(), layer, rect);
    let dz = ed.elevation as f32;
    // deleted: a dashed ghost box where it stood; changed: the dashed box it had before
    let mut ghosts = 0;
    for (key, kind) in &set.kinds {
        let (b, col) = match kind {
            RowKind::Deleted => (set.boxes.get(key), ov.ai_deleted),
            RowKind::Changed => (set.before_boxes.get(key), ov.ai_changed.gamma_multiply(0.7)),
            RowKind::Created => (None, ov.ai_created),
        };
        let Some(b) = b else {
            continue;
        };
        if ghosts >= ghost::GHOST_BOXES {
            break;
        }
        ghosts += 1;
        let is_hot = hot.contains(key);
        let Some(st) = ghost::before_outline_style(is_hot, false) else {
            continue;
        };
        let w = if *kind == RowKind::Deleted {
            OUTLINE_PX
        } else {
            st.width_px
        };
        draw_box_form(&p, view, b, dz, Stroke::new(w, col), ghost::LineForm::Dashed);
    }
    for key in hot.iter().take(HOT_BOXES) {
        if let Some(b) = set.boxes.get(key) {
            draw_box(&p, view, b, dz, Stroke::new(OUTLINE_PX, ov.selection));
            if let Some(c) = box_center(view, b, dz) {
                if let Some(a) = ghost::edge_arrow(rect, c, ghost::ARROW_INSET_PX) {
                    ghost::paint_arrow(&p, &a, ov.selection);
                }
            }
        }
    }
    if !hot.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
    if set.omitted > 0 {
        if let Some(b) = &set.bounds {
            draw_box(&p, view, b, dz, Stroke::new(THIN_PX, ov.ai_changed));
        }
    }
}

/// The box of these elements, and a level that shows them: for "zoom to".
pub fn focus_of(set: &PreviewSet, keys: &[ElKey]) -> Option<(Aabb, Option<i64>)> {
    let mut b = Aabb::EMPTY;
    let mut level = None;
    for k in keys {
        if let Some(x) = set.boxes.get(k) {
            b.union(x);
        }
        level = level.or_else(|| set.marks.iter().find(|m| m.key == *k).map(|m| m.level));
    }
    (!b.is_empty()).then_some((b, level))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflate_grows_every_side_by_the_amount() {
        let mut p = [[0.0, 0.0, 0.0], [2.0, 4.0, 6.0]];
        inflate(&mut p, 0.005);
        for a in 0..3 {
            assert!((p[0][a] + 0.005).abs() < 1e-5, "{p:?}");
        }
        assert!((p[1][0] - 2.005).abs() < 1e-5, "{p:?}");
        assert!((p[1][2] - 6.005).abs() < 1e-5, "{p:?}");
    }

    #[test]
    fn inflate_keeps_a_flat_shell_finite() {
        let mut p = [[1.0, 1.0, 3.0], [2.0, 1.0, 3.0]];
        inflate(&mut p, 0.005);
        assert!(p.iter().flatten().all(|x| x.is_finite()));
    }

    #[test]
    fn merge_shifts_the_triangle_indices() {
        let mut out = vec![];
        let body = || Bodies {
            positions: vec![[0.0; 3]; 3],
            triangles: vec![[0, 1, 2]],
        };
        merge(&mut out, RowKind::Created, 1, body());
        merge(&mut out, RowKind::Created, 1, body());
        merge(&mut out, RowKind::Created, 2, body());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].triangles[1], [3, 4, 5]);
    }

    #[test]
    fn every_element_table_can_be_hidden() {
        for t in rows::ELEMENT_TABLES {
            let (sel, part) = hide_of(&(t.to_string(), 7));
            assert!(sel.is_some() || part.is_some(), "{t}");
        }
    }

    #[test]
    fn signs_match_the_legend() {
        assert_eq!(sign(RowKind::Created), "+");
        assert_eq!(sign(RowKind::Changed), "~");
        assert_eq!(sign(RowKind::Deleted), "−");
    }

    #[test]
    fn preview_colours_differ_in_both_themes() {
        for dark in [false, true] {
            let o = pal(dark);
            assert_ne!(o.ai_created, o.ai_changed);
            assert_ne!(o.ai_changed, o.ai_deleted);
        }
    }

    #[test]
    fn rev_is_zero_without_a_preview() {
        publish(None, false);
        assert_eq!(rev(), 0);
    }

    #[test]
    fn hide_goes_into_the_view_set() {
        let mut set = PreviewSet {
            rev: next_rev(),
            ..PreviewSet::default()
        };
        set.hide.insert(Sel::Wall(5));
        set.hide_parts.insert((Kind::Roof, 9));
        publish(Some(Arc::new(set)), false);
        let mut vis = isolate::Vis::default();
        hide_into(&mut vis);
        assert!(vis.hidden.contains(&Sel::Wall(5)));
        assert!(vis.hidden_parts.contains(&(Kind::Roof, 9)));
        publish(None, false);
    }

    fn draft_with_wall(id: i64) -> PreviewSet {
        let mut set = PreviewSet {
            rev: next_rev(),
            ..PreviewSet::default()
        };
        set.hide.insert(Sel::Wall(id));
        set
    }

    #[test]
    fn hovering_a_card_row_makes_its_elements_hot() {
        let k = ("wall".to_string(), 3);
        set_hot(crate::ai::ui::logic::hot_set([k.clone()]));
        assert!(SHOWN.with(|s| s.borrow().hot.contains(&k)));
        set_hot(HashSet::new());
        assert!(SHOWN.with(|s| s.borrow().hot.is_empty()));
    }

    #[test]
    fn before_held_shows_the_model_as_it_is() {
        publish(Some(Arc::new(draft_with_wall(5))), false);
        let draft = rev();
        let mut vis = isolate::Vis::default();
        hide_into(&mut vis);
        assert!(vis.hidden.contains(&Sel::Wall(5)));

        set_before(true);
        assert!(before_mode());
        assert_ne!(rev(), draft, "the scene is rebuilt when B goes down");
        let mut vis = isolate::Vis::default();
        hide_into(&mut vis);
        assert!(vis.hidden.is_empty(), "live elements are not hidden in Before");

        set_before(false);
        assert_eq!(rev(), draft, "release returns to the draft");
        publish(None, false);
    }

    #[test]
    fn before_without_a_draft_is_nothing() {
        publish(None, false);
        set_before(true);
        assert!(!before_mode());
        assert_eq!(rev(), 0);
        set_before(false);
    }

    #[test]
    fn focus_joins_the_boxes_and_takes_a_level() {
        let mut set = PreviewSet::default();
        let k = ("wall".to_string(), 1);
        set.boxes.insert(
            k.clone(),
            Aabb {
                min: Vec3::ZERO,
                max: Vec3::ONE,
            },
        );
        set.marks.push(PlanMark {
            kind: RowKind::Changed,
            level: 4,
            key: k.clone(),
            outline: vec![],
        });
        let (b, level) = focus_of(&set, &[k]).unwrap();
        assert_eq!((b.max, level), (Vec3::ONE, Some(4)));
        assert!(focus_of(&set, &[("wall".to_string(), 2)]).is_none());
    }
}
