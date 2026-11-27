// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! In-place masses and bodies in the editor: mass meshes for 3D (a face is a separate scene element,
//! painting is a palette color), the "Masses" tab commands (outline in plan, faces in 3D), push-pull
//! with live preview and distance input; each commit is one undo step.

use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use crate::tools::Rec;
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::V2;
use crate::edit3d::pick::Cat as Cat3;
use ddc_model::{BoolOp, FaceClass, Form, MassBody, MassSpec, Model, PushGoal};
use ddc_ops::{
    Actor, Cat as OpCat, Envelope, GeoOp, Group, Len, Op, PanelDiv, Ref as OpRef, ShellKind, ShellLayer,
    SurfaceBy, P3,
};
use ddc_render::glam::Vec3;
use ddc_render::SceneMesh;
use ddc_ui::i18n::{Catalog, Lang};
use egui::Pos2;
use std::sync::Arc;

/// Scene element numbers for masses: above walls, openings, curtain walls and slabs.
/// Element = MASS_BASE + (mass number << 12) + face number.
pub const MASS_BASE: u32 = crate::scene_ids::MASS_BASE;
const FACE_BITS: u32 = 12;
/// Above are the `mep3d` blocks (ARCH_BASE..): the ranges do not overlap (32768 masses).
pub const MASS_END: u32 = crate::scene_ids::MASS_END;

/// Palette color by face material (see render/mesh.wgsl).
pub fn material_tag(m: Option<&str>) -> u32 {
    match m {
        Some("glass") => 8,
        Some("metal") => 9,
        Some("panel") => 10,
        Some("concrete") => 11,
        Some(_) => 1,
        None => 2,
    }
}

fn slot_base(k: usize) -> u32 {
    MASS_BASE + ((k as u32) << FACE_BITS)
}

/// The last face number in a mass slot: shared by "extra" faces (they cannot be picked one by one).
const OVERFLOW: u32 = (1 << FACE_BITS) - 1;

/// Body meshes: one per face (element `base + label number`), shifted in z.
pub fn body_meshes(base: u32, b: &MassBody, dz: f64, paint: &dyn Fn(u32) -> u32) -> Vec<SceneMesh> {
    let tags = b.tags();
    let mut out: Vec<SceneMesh> = vec![];
    for p in &b.polys {
        let idx = tags.binary_search(&p.tag).unwrap_or(0) as u32;
        let (element, tag) = (base + idx.min(OVERFLOW), paint(p.tag));
        let k = match out
            .iter()
            .position(|m| m.element == element && m.tag == tag)
        {
            Some(k) => k,
            None => {
                out.push(SceneMesh {
                    element,
                    tag,
                    positions: vec![],
                    triangles: vec![],
                });
                out.len() - 1
            }
        };
        let m = &mut out[k];
        let o = m.positions.len() as u32;
        m.positions.extend(
            p.pts
                .iter()
                .map(|q| [q.x as f32, q.y as f32, (q.z - dz) as f32]),
        );
        for i in 1..p.pts.len() as u32 - 1 {
            m.triangles.push([o, o + i, o + i + 1]);
        }
    }
    out
}

/// The mass body for display: after void cuts and joins (otherwise the original).
fn shown_body(model: &Model, id: i64) -> Option<MassBody> {
    model
        .joined_body("mass", id)
        .or_else(|_| model.mass_body(id))
        .ok()
}

/// Palette colors of mass faces: (label, color).
fn paint_of(model: &Model, id: i64) -> Vec<(u32, u32)> {
    model
        .mass_faces(id)
        .unwrap_or_default()
        .iter()
        .map(|f| (f.tag, material_tag(f.material.as_deref())))
        .collect()
}

fn mass_meshes(
    model: &Model,
    k: usize,
    id: i64,
    body: Option<&MassBody>,
    paint: Option<&[(u32, u32)]>,
    dz: f64,
) -> Vec<SceneMesh> {
    let own;
    let b = match body {
        Some(b) => b,
        None => match shown_body(model, id) {
            Some(b) => {
                own = b;
                &own
            }
            None => return vec![],
        },
    };
    let fetched;
    let colors = match paint {
        Some(p) => p,
        None => {
            fetched = paint_of(model, id);
            &fetched
        }
    };
    let pick = |t: u32| {
        colors
            .iter()
            .find(|c| c.0 == t)
            .map_or(material_tag(None), |c| c.1)
    };
    body_meshes(slot_base(k), b, dz, &pick)
}

/// Meshes of all solid masses of the model relative to elevation `z0` of the current level (voids are not
/// drawn, they show through the cuts); masses are shown after the cuts.
pub fn scene_meshes(model: &Model, z0: f64, meshes: &mut Vec<Arc<SceneMesh>>) {
    let Ok(ids) = model.masses() else {
        return;
    };
    for (k, id) in ids.into_iter().enumerate() {
        if model.mass(id).is_ok_and(|s| !s.void) {
            meshes.extend(
                mass_meshes(model, k, id, None, None, z0)
                    .into_iter()
                    .map(Arc::new),
            );
        }
    }
}

/// Sections of all masses (and voids) by the plan plane at world elevation `z`: (mass, outlines).
pub fn plan_sections(model: &Model, z: f64) -> Vec<(i64, Vec<Vec<V2>>)> {
    model
        .masses()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            let rs = ddc_geom::mass::section_rings(&shown_body(model, id)?, z);
            (!rs.is_empty()).then_some((id, rs))
        })
        .collect()
}

/// A point inside the section (parity: islands and holes in any order).
pub fn inside_rings(rings: &[Vec<V2>], p: V2) -> bool {
    rings
        .iter()
        .filter(|r| ddc_geom::predicates::point_in_ring(p, r) > 0)
        .count()
        % 2
        == 1
}

/// The mass by scene element (any of its faces).
pub fn mass_of_element(model: &Model, e: u32) -> Option<i64> {
    let r = e.checked_sub(MASS_BASE).filter(|_| e < MASS_END)?;
    model.masses().ok()?.get((r >> FACE_BITS) as usize).copied()
}

/// The range of scene elements of a mass (highlighting the selected mass).
pub fn mass_range(model: &Model, id: i64) -> Option<(u32, u32)> {
    let k = model.masses().ok()?.iter().position(|x| *x == id)?;
    let lo = slot_base(k);
    Some((lo, lo + (1 << FACE_BITS)))
}

/// Mass and face label by scene element; "extra" faces of the shared slot give None.
pub fn decode(model: &Model, e: u32) -> Option<(i64, u32)> {
    let id = mass_of_element(model, e)?;
    let local = (e - MASS_BASE) & OVERFLOW;
    let tags = shown_body(model, id)?.tags();
    if local == OVERFLOW && tags.len() > OVERFLOW as usize {
        return None;
    }
    Some((id, *tags.get(local as usize)?))
}

/// Push-pull: pick a face, drag (live preview), commit.
#[derive(Clone, Debug, PartialEq)]
pub struct PushPull {
    pub mass: i64,
    pub face: u32,
    pub d: f64,
}

impl PushPull {
    pub fn new(mass: i64, face: u32) -> Self {
        PushPull { mass, face, d: 0.0 }
    }

    /// New offset; returns the preview body (the model does not change). Zero means no
    /// preview (the mass itself is shown).
    pub fn drag(&mut self, model: &Model, d: f64) -> Option<MassBody> {
        self.d = d;
        if d.abs() < 1e-9 {
            return None;
        }
        model.preview_push_pull(self.mass, self.face, d).ok()
    }

    /// An editor journal entry: one model operation, per-face elements are recomputed.
    pub fn rec(&self) -> Rec {
        let (mass, face, d) = (self.mass, self.face, self.d);
        Rec::new("undo.mass.push_pull", move |m| {
            m.edit_mass_form(mass, |b| Form::PushPull {
                base: b,
                face,
                d,
                keys: vec![],
            })?;
            Ok(1)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Extrude,
    Revolve,
    Sweep,
    Blend,
    SweptBlend,
    Void,
    PushPull,
    Fillet,
    Chamfer,
    Shell,
    Offset,
    SplitFace,
    PushRegion,
    Draft,
    Paint,
    Union,
    Subtract,
    Intersect,
    Join,
    Unjoin,
    Cut,
    WallByFace,
    CurtainByFace,
    SlabByFace,
    RoofByFace,
    Floors,
    /// Freeform surface over a plan rectangle (two corners), rising to the typed height.
    Freeform,
    /// Curtain system on a picked freeform surface, panels of the typed size.
    Panelize,
    /// Roof on a picked freeform surface, the typed thickness.
    SurfaceShell,
    /// Shell thickness of a picked freeform surface.
    Thicken,
}

/// Ribbon commands: id and command. The step hint is `hint.<id>.<step>` (see [`MassTool::step`]).
pub const COMMANDS: &[(&str, Cmd)] = &[
    ("mass.extrude", Cmd::Extrude),
    ("mass.revolve", Cmd::Revolve),
    ("mass.sweep", Cmd::Sweep),
    ("mass.blend", Cmd::Blend),
    ("mass.swept_blend", Cmd::SweptBlend),
    ("mass.void", Cmd::Void),
    ("mass.push_pull", Cmd::PushPull),
    ("mass.fillet", Cmd::Fillet),
    ("mass.chamfer", Cmd::Chamfer),
    ("mass.shell", Cmd::Shell),
    ("mass.offset", Cmd::Offset),
    ("mass.split_face", Cmd::SplitFace),
    ("mass.push_region", Cmd::PushRegion),
    ("mass.draft", Cmd::Draft),
    ("mass.paint", Cmd::Paint),
    ("mass.union", Cmd::Union),
    ("mass.subtract", Cmd::Subtract),
    ("mass.intersect", Cmd::Intersect),
    ("mass.join", Cmd::Join),
    ("mass.unjoin", Cmd::Unjoin),
    ("mass.cut", Cmd::Cut),
    ("mass.wall_by_face", Cmd::WallByFace),
    ("mass.curtain_by_face", Cmd::CurtainByFace),
    ("mass.slab_by_face", Cmd::SlabByFace),
    ("mass.roof_by_face", Cmd::RoofByFace),
    ("mass.floors", Cmd::Floors),
    ("mass.freeform", Cmd::Freeform),
    ("mass.panelize", Cmd::Panelize),
    ("mass.surface_shell", Cmd::SurfaceShell),
    ("mass.thicken", Cmd::Thicken),
];

impl Cmd {
    /// An outline or path is drawn in plan (the others pick in 3D).
    pub fn in_plan(self) -> bool {
        matches!(
            self,
            Cmd::Extrude
                | Cmd::Revolve
                | Cmd::Sweep
                | Cmd::Blend
                | Cmd::SweptBlend
                | Cmd::Void
                | Cmd::Freeform
        )
    }

    /// Works on a picked freeform surface (a 3D click or the 3D selection), then a typed value.
    pub fn on_surface(self) -> bool {
        matches!(self, Cmd::Panelize | Cmd::SurfaceShell | Cmd::Thicken)
    }

    /// Ribbon command id.
    pub fn id(self) -> &'static str {
        COMMANDS
            .iter()
            .find(|c| c.1 == self)
            .map_or("mass.extrude", |c| c.0)
    }

    /// The command picks several faces/elements in turn.
    fn multi(self) -> bool {
        matches!(
            self,
            Cmd::Fillet
                | Cmd::Chamfer
                | Cmd::Shell
                | Cmd::Offset
                | Cmd::Draft
                | Cmd::Union
                | Cmd::Subtract
                | Cmd::Intersect
                | Cmd::Join
                | Cmd::Unjoin
                | Cmd::Cut
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pick {
    /// A mass face and its scene element (highlight).
    Face {
        mass: i64,
        tag: u32,
        el: u32,
    },
    Elem(&'static str, i64),
}

impl Pick {
    fn target(self) -> (&'static str, i64) {
        match self {
            Pick::Face { mass, .. } => ("mass", mass),
            Pick::Elem(k, id) => (k, id),
        }
    }
}

pub struct MassTool {
    pub cmd: Cmd,
    /// Outline/path points in plan.
    pub pts: Vec<V2>,
    pub picks: Vec<Pick>,
    /// The typed number (height, distance, radius...).
    pub input: String,
    pub pp: Option<PushPull>,
    /// Push-pull face anchor and normal in scene coordinates.
    pub axis: Option<(Vec3, Vec3)>,
    /// The preview body (world coordinates) and a change number for rebuilding the scene.
    pub preview: Option<(i64, MassBody)>,
    pub rev: u64,
    /// Push-pull gesture start: (parameter along the normal under the cursor, offset before the gesture).
    pub drag0: Option<(f64, f64)>,
    /// Push-pull mass face colors (the preview does not re-read painting).
    pub paint: Vec<(u32, u32)>,
    /// Outline corners drawn on the work face (world coordinates).
    pub pts3: Vec<(f64, f64, f64)>,
    /// Camera ray of the last 3D click (scene coordinates).
    pub ray: Option<([f32; 3], [f32; 3])>,
    pub cat: Catalog,
}

impl MassTool {
    pub fn new(cmd: Cmd, lang: Lang) -> Self {
        MassTool {
            cmd,
            pts: vec![],
            picks: vec![],
            input: String::new(),
            pp: None,
            axis: None,
            preview: None,
            rev: 0,
            drag0: None,
            paint: vec![],
            pts3: vec![],
            ray: None,
            cat: Catalog::new(lang),
        }
    }

    fn value(&self, default: f64) -> f64 {
        metres(&self.input).unwrap_or(default)
    }

    pub fn t(&self, key: &str) -> String {
        self.cat.t(key)
    }

    /// Hint step `hint.<id>.<step>`.
    pub fn step(&self) -> &'static str {
        if self.cmd.in_plan() {
            return if self.pts.is_empty() { "first" } else { "next" };
        }
        if self.cmd == Cmd::PushPull {
            return if self.pp.is_some() { "drag" } else { "first" };
        }
        if self.cmd.on_surface() {
            return if self.picks.is_empty() { "first" } else { "value" };
        }
        if self.cmd == Cmd::PushRegion {
            return if self.pts3.is_empty() { "first" } else { "next" };
        }
        if matches!(self.cmd, Cmd::Fillet | Cmd::Chamfer) {
            // two faces per edge
            return if self.picks.len() % 2 == 1 {
                "next"
            } else {
                "first"
            };
        }
        if self.cmd.multi() && !self.picks.is_empty() {
            "next"
        } else {
            "first"
        }
    }
}

/// Text by catalog key of the active mass command.
fn tr(ed: &Editor, key: &str) -> String {
    ed.mass
        .as_ref()
        .map_or_else(|| key.to_string(), |t| t.t(key))
}

fn say(ed: &mut Editor, key: &str) {
    ed.status = Some(tr(ed, key));
}

/// The typed number, meters (a comma is the decimal mark).
fn metres(s: &str) -> Option<f64> {
    crate::units::parse_locale_number(s.trim(), crate::units::ui_decimal_mark()).ok()
}

/// Turn on a mass command (the step hint goes to the status bar via `tool_hint`).
pub fn arm(ed: &mut Editor, cmd: Cmd, lang: Lang) {
    // the session draws in plan; the wall stays unarmed
    ed.set_mode(Mode::Select);
    if cmd.in_plan() {
        ed.selection.clear();
    }
    ed.mass = Some(Box::new(MassTool::new(cmd, lang)));
    ed.status = None;
    crate::interact::Interaction::refresh(ed);
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

fn tup(p: &V2) -> (f64, f64) {
    (p.x, p.y)
}

fn ccw(mut r: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    let a: f64 = (0..r.len())
        .map(|i| {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            p.0 * q.1 - q.0 * p.1
        })
        .sum();
    if a < 0.0 {
        r.reverse();
    }
    r
}

fn commit(ed: &mut Editor, r: Rec) -> bool {
    match ed.apply_ext(r) {
        Ok(()) => {
            ed.status = None;
            true
        }
        Err(e) => {
            ed.status = Some(format!("{}: {e}", tr(ed, "status.mass.error")));
            false
        }
    }
}

/// `commit` as a heavy edit: the worker freezes each `(new history, old history)`
/// exactly as `add_mass`/`update_mass` will, which fills the form caches; the frame loop then
/// applies `r` (one undo step) without kernel work and runs `done`. False: nothing started.
pub(crate) fn commit_heavy(
    ed: &mut Editor,
    r: Rec,
    warm: Vec<(Form, Option<Form>)>,
    done: impl FnOnce(&mut Editor) + 'static,
) -> bool {
    let err = tr(ed, "status.mass.error");
    let err2 = err.clone();
    ed.start_heavy(
        "job.title.form",
        false,
        move |_| {
            for (f, old) in &warm {
                f.prepared(old.as_ref()).map_err(|e| match crate::heavy::err_text(e) {
                    c if c == crate::jobs::CANCELLED => c,
                    e => format!("{err}: {e}"),
                })?;
            }
            Ok(())
        },
        move |ed, ()| {
            ed.apply_ext(r).map_err(|e| format!("{err2}: {e}"))?;
            ed.status = None;
            done(ed);
            Ok(())
        },
    )
}

/// The current history of `mass` and `f` applied to it, for `commit_heavy`.
pub(crate) fn warm_edit(ed: &Editor, mass: i64, f: impl FnOnce(Box<Form>) -> Form) -> Vec<(Form, Option<Form>)> {
    match ed.model.mass(mass) {
        Ok(s) => vec![(f(Box::new(s.form.clone())), Some(s.form))],
        // the operation reports the missing form itself
        Err(_) => vec![],
    }
}

/// A slab type for per-face elements: the first in the project or a new one (id, model operations).
fn slab_type(m: &mut Model, name: &str) -> Result<(i64, usize), ddc_model::Error> {
    let id: Option<i64> = m
        .sql()
        .query_row("SELECT id FROM slab_type ORDER BY id LIMIT 1", [], |r| {
            r.get(0)
        })
        .ok();
    match id {
        Some(id) => Ok((id, 0)),
        None => crate::tools::ensure_slab_type(m, name),
    }
}

/// A "slab type (if missing) + element" write: on error the created type is rolled back,
/// the operation count is for undoing in one step.
fn with_slab_type(
    label: &'static str,
    name: String,
    f: impl Fn(&mut Model, i64) -> Result<(), ddc_model::Error> + 'static,
) -> Rec {
    Rec::new(label, move |m| {
        let (st, n) = slab_type(m, &name)?;
        match f(m, st) {
            Ok(()) => Ok(n + 1),
            Err(e) => {
                for _ in 0..n {
                    m.undo()?;
                }
                Err(e)
            }
        }
    })
}

/// A write step made of several model operations.
pub type Step = Box<dyn Fn(&mut Model) -> Result<(), ddc_model::Error>>;

/// A write of steps (one model operation each): an error rolls back those already done.
pub fn steps_rec(label: &'static str, steps: Vec<Step>) -> Rec {
    Rec::new(label, move |m| {
        for (k, s) in steps.iter().enumerate() {
            if let Err(e) = s(m) {
                for _ in 0..k {
                    m.undo()?;
                }
                return Err(e);
            }
        }
        Ok(steps.len())
    })
}

/// A mass from a plan outline/path (coordinates are from the level); an error is a message key.
fn plan_form(t: &MassTool) -> Result<(Form, bool), &'static str> {
    let pts: Vec<(f64, f64)> = t.pts.iter().map(tup).collect();
    let need = match t.cmd {
        Cmd::Revolve | Cmd::Sweep | Cmd::SweptBlend => 2,
        _ => 3,
    };
    if pts.len() < need {
        return Err("status.mass.few_points");
    }
    let path = |z: f64| pts.iter().map(|p| (p.0, p.1, z)).collect::<Vec<_>>();
    Ok(match t.cmd {
        Cmd::Extrude | Cmd::Void => (
            Form::Extrude {
                ring: ccw(pts),
                z0: 0.0,
                h: t.value(6.0),
                ids: vec![],
            },
            t.cmd == Cmd::Void,
        ),
        Cmd::Revolve => {
            let (c, e) = (t.pts[0], t.pts[1]);
            let r = (e - c).len().max(0.1);
            let h = t.value(r);
            let n = 8;
            let profile = (0..=n)
                .map(|i| {
                    let a = std::f64::consts::FRAC_PI_2 * i as f64 / n as f64;
                    (r * a.cos(), h * a.sin())
                })
                .chain([(0.0, 0.0)])
                .collect();
            (
                Form::Revolve {
                    profile,
                    axis: tup(&c),
                    angle: std::f64::consts::TAU,
                    seg: 32,
                    ids: vec![],
                },
                false,
            )
        }
        Cmd::Sweep => {
            let s = t.value(0.6) / 2.0;
            (
                Form::Sweep {
                    profile: rect(-s, 0.0, s, 2.0 * s),
                    path: path(0.0),
                    ids: vec![],
                },
                false,
            )
        }
        Cmd::Blend => {
            let ring = ccw(pts);
            let n = ring.len() as f64;
            let c = ring
                .iter()
                .fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n));
            let top = ring
                .iter()
                .map(|p| (c.0 + (p.0 - c.0) * 0.5, c.1 + (p.1 - c.1) * 0.5))
                .collect();
            (
                Form::Blend {
                    sections: vec![(ring, 0.0), (top, t.value(6.0))],
                    ids: vec![],
                },
                false,
            )
        }
        Cmd::SweptBlend => (
            Form::SweptBlend {
                p0: rect(-0.5, 0.0, 0.5, 1.0),
                p1: rect(-0.2, 0.0, 0.2, 0.4),
                path: path(0.0),
                ids: vec![],
            },
            false,
        ),
        _ => return Err("status.mass.not_plan"),
    })
}

/// One envelope of the editor's ops (`geometry.*`): one undo step; the command ends on success.
fn run_geo(ed: &mut Editor, title: &str, op: GeoOp) -> bool {
    let mut env = Envelope::new(Actor::User, title);
    env.groups.push(Group { id: 1, title: title.to_string(), ops: vec![Op::Geometry(op)], depends_on: vec![], why: String::new() });
    match ed.run_ops(env) {
        Ok(_) => {
            ed.mass = None;
            ed.set_mode(Mode::Select);
            true
        }
        Err(e) => {
            ed.status = Some(format!("{}: {e}", tr(ed, "status.mass.error")));
            false
        }
    }
}

/// Control net of the freeform cushion over the rectangle `a`-`b` at plan height `z`: corners
/// on it, the middle of the patch `rise` above (biquadratic: edge controls `rise`, centre twice).
pub fn cushion(a: V2, b: V2, z: f64, rise: f64) -> Vec<Vec<P3>> {
    let (x0, x1, y0, y1) = (a.x.min(b.x), a.x.max(b.x), a.y.min(b.y), a.y.max(b.y));
    let h = [[0.0, 1.0, 0.0], [1.0, 2.0, 1.0], [0.0, 1.0, 0.0]];
    (0..3)
        .map(|i| {
            (0..3)
                .map(|j| {
                    let x = x0 + (x1 - x0) * i as f64 / 2.0;
                    let y = y0 + (y1 - y0) * j as f64 / 2.0;
                    P3::m(x, y, z + rise * h[i][j])
                })
                .collect()
        })
        .collect()
}

fn finish_freeform(ed: &mut Editor) {
    let Some(t) = ed.mass.as_ref() else { return };
    let (Some(&a), Some(&b)) = (t.pts.first(), t.pts.get(1)) else {
        say(ed, "status.mass.few_points");
        return;
    };
    if (b.x - a.x).abs() < 0.5 || (b.y - a.y).abs() < 0.5 {
        say(ed, "status.mass.freeform_small");
        t_reset(ed);
        return;
    }
    let rise = t.value(3.0);
    let name = t.t("name.mass.freeform");
    let op = GeoOp::Freeform {
        slot: 0,
        level: Some(OpRef::id(OpCat::Level, ed.level)),
        surface: SurfaceBy::Points { rows: cushion(a, b, ed.elevation, rise), degree_u: Some(2), degree_v: Some(2), weights: None },
        thickness: None,
        side: None,
        trim: None,
        material: None,
        name: Some(name),
    };
    if !run_geo(ed, "mass.freeform", op) {
        t_reset(ed);
    }
}

/// The rectangle starts again after a refusal (the typed value stays).
fn t_reset(ed: &mut Editor) {
    if let Some(t) = ed.mass.as_mut() {
        t.pts.clear();
    }
}

/// A freeform surface picked for the command: the typed value comes next.
pub fn pick_surface(ed: &mut Editor, id: i64) {
    if let Some(t) = ed.mass.as_mut().filter(|t| t.cmd.on_surface()) {
        t.picks = vec![Pick::Elem("freeform", id)];
        t.input.clear();
        ed.status = None;
    }
}

/// Panels, roof or thickness on the picked surface with the typed value.
fn finish_surface(ed: &mut Editor) {
    let Some(t) = ed.mass.as_ref() else { return };
    let Some(id) = t.picks.iter().find_map(|p| match p {
        Pick::Elem("freeform", id) => Some(*id),
        _ => None,
    }) else {
        say(ed, "status.mass.pick_surface");
        return;
    };
    let host = OpRef::id(OpCat::Freeform, id);
    let (title, op) = match t.cmd {
        Cmd::Panelize => {
            let w = Len::m(t.value(1.5));
            let op = GeoOp::Panelize {
                slot: 0,
                host,
                columns: PanelDiv::Width { width: w },
                rows: PanelDiv::Width { width: w },
                pattern: None,
                planarize: None,
                mullion: None,
                glass: None,
                flat: None,
                cold: None,
                material: None,
                name: None,
            };
            ("mass.panelize", op)
        }
        Cmd::SurfaceShell => {
            let layer = ShellLayer { thickness: Len::m(t.value(0.25)), material: Some("Concrete".into()), name: None };
            let op = GeoOp::SurfaceShell {
                slot: 0,
                host,
                kind: ShellKind::Roof,
                layers: vec![layer],
                side: None,
                openings: None,
                name: None,
            };
            ("mass.surface_shell", op)
        }
        _ => ("mass.thicken", GeoOp::Thicken { target: host, thickness: Len::m(t.value(0.2)), side: None }),
    };
    run_geo(ed, title, op);
}

fn finish_plan(ed: &mut Editor) {
    let Some(t) = ed.mass.as_ref() else { return };
    if t.cmd == Cmd::Freeform {
        finish_freeform(ed);
        return;
    }
    match plan_form(t) {
        Ok((form, void)) => {
            let spec = MassSpec {
                level: ed.level,
                name: t.t(if void {
                    "name.mass.void"
                } else {
                    "name.mass.form"
                }),
                void,
                inplace: false,
                form,
            };
            // extrude, revolve, sweep, loft: the kernel runs on a worker
            let warm = vec![(spec.form.clone(), None)];
            let r = Rec::new("undo.mass.add", move |m| {
                m.add_mass(&spec)?;
                Ok(1)
            });
            commit_heavy(ed, r, warm, |ed| {
                ed.mass = None;
                ed.set_mode(Mode::Select);
            });
        }
        Err(k) => say(ed, k),
    }
    // on an error the contour stays (both the tool and the plan rubber band) for a fix
    if ed.mass.is_none() {
    }
}

/// A plan event for a mass command; false means no command or it works in 3D.
pub fn handle(ed: &mut Editor, e: Event) -> bool {
    let Some(t) = ed.mass.as_mut() else {
        return false;
    };
    if !t.cmd.in_plan() {
        return keys(ed, e);
    }
    match e {
        Event::Move(p) => ed.tool.aim(p, crate::wall_tool::Chain::of(&t.pts, ed.tool.snap_state.z, true)),
        Event::Click(p) => {
            t.pts.push(p);
            if matches!(t.cmd, Cmd::Revolve | Cmd::Freeform) && t.pts.len() == 2 {
                finish_plan(ed);
            }
        }
        Event::Press(_) | Event::Release(_) | Event::Key(Key::Tab) | Event::Key(Key::Arc) => {}
        Event::Char(c) if c.is_ascii_digit() || ".,-".contains(c) => t.input.push(c),
        Event::Char(_) => {}
        Event::Key(Key::Backspace) => {
            if t.input.pop().is_none() {
                t.pts.pop();
            }
        }
        Event::Key(Key::Escape) => cancel(ed),
        Event::Key(Key::Enter) | Event::Key(Key::Close) => finish_plan(ed),
    }
    true
}

/// Push-pull offset from the typed number (empty or not a number is 0) and a new preview.
fn typed(ed: &mut Editor) {
    let Some(t) = ed.mass.as_mut() else { return };
    let d = metres(&t.input).unwrap_or(0.0);
    if let Some(pp) = t.pp.as_mut() {
        pp.d = d;
    }
    t.drag0 = None;
    preview(ed);
}

/// Keys of a command that works in 3D (number input, Enter, Esc).
fn keys(ed: &mut Editor, e: Event) -> bool {
    let Some(t) = ed.mass.as_mut() else {
        return false;
    };
    match e {
        Event::Char(c) if c.is_ascii_digit() || ".,-".contains(c) => {
            t.input.push(c);
            typed(ed);
        }
        Event::Key(Key::Backspace) => {
            // nothing typed: the last corner of an outline on a face goes
            if t.input.pop().is_none() && t.pts3.pop().is_some() && t.pts3.is_empty() {
                t.picks.clear();
            }
            typed(ed);
        }
        Event::Key(Key::Escape) => cancel(ed),
        Event::Key(Key::Enter) => enter(ed),
        _ => return false,
    }
    true
}

pub fn cancel(ed: &mut Editor) {
    ed.mass = None;
    ed.set_mode(Mode::Select);
    // the scene shows the preview: rebuild it without
    ed.revision += 1;
}

fn enter(ed: &mut Editor) {
    let Some(t) = ed.mass.as_ref() else { return };
    let faces: Vec<(i64, u32)> = t
        .picks
        .iter()
        .filter_map(|p| match p {
            Pick::Face { mass, tag, .. } => Some((*mass, *tag)),
            _ => None,
        })
        .collect();
    match t.cmd {
        Cmd::Panelize | Cmd::SurfaceShell | Cmd::Thicken => finish_surface(ed),
        Cmd::PushPull => match t.pp.clone() {
            Some(pp) if pp.d.abs() >= 1e-9 => {
                if commit(ed, pp.rec()) {
                    ed.mass = None;
                }
            }
            Some(_) => say(ed, "status.mass.zero"),
            None => say(ed, "status.mass.pick_face"),
        },
        Cmd::Shell => {
            // open faces of one mass, or a closed shell of the selected mass
            let mass = faces.first().map(|f| f.0).or_else(|| {
                ed.selection.iter().find_map(|s| match s {
                    Sel::Mass(id) => Some(*id),
                    _ => None,
                })
            });
            let Some(mass) = mass else {
                say(ed, "status.mass.pick_face");
                return;
            };
            let tt = t.value(0.3);
            let open: Vec<u32> = faces.iter().filter(|f| f.0 == mass).map(|f| f.1).collect();
            edit_form(ed, mass, "undo.mass.shell", move |b| Form::Shell {
                base: b,
                t: tt,
                open: open.clone(),
                keys: vec![],
            });
        }
        Cmd::Fillet | Cmd::Chamfer => {
            let edges: Vec<(u32, u32)> = faces.chunks_exact(2).map(|c| (c[0].1, c[1].1)).collect();
            let Some(&(mass, _)) = faces.first().filter(|_| !edges.is_empty()) else {
                say(ed, "status.mass.pick_face");
                return;
            };
            let round = t.cmd == Cmd::Fillet;
            let size = t.value(if round { 0.5 } else { 0.3 });
            let label = if round {
                "undo.mass.fillet"
            } else {
                "undo.mass.chamfer"
            };
            edit_form(ed, mass, label, move |b| Form::Blends {
                base: b,
                edges: edges.clone(),
                round,
                size,
                seg: 8,
            });
        }
        Cmd::PushRegion => {
            let Some(&(mass, tag)) = faces.first() else {
                say(ed, "status.mass.pick_face");
                return;
            };
            if t.pts3.len() < 3 {
                say(ed, "status.mass.outline_short");
                return;
            }
            let d = t.value(1.0);
            let z0 = mass_z0(ed, mass);
            let region: Vec<(f64, f64, f64)> =
                t.pts3.iter().map(|p| (p.0, p.1, p.2 - z0)).collect();
            edit_form(ed, mass, "undo.mass.push_region", move |b| Form::PushTo {
                base: b,
                face: tag,
                region: region.clone(),
                to: PushGoal::Dist(d),
                keys: vec![],
            });
        }
        Cmd::Draft => {
            let Some(&(mass, _)) = faces.first() else {
                say(ed, "status.mass.pick_face");
                return;
            };
            let angle = t.value(5.0).to_radians();
            let z0 = mass_z0(ed, mass);
            let all = ed.model.mass_faces(mass).unwrap_or_default();
            // a picked horizontal face is the neutral plane, the base by default
            let zmin = all
                .iter()
                .flat_map(|f| f.outline.iter().flatten())
                .fold(f64::INFINITY, |m, p| m.min(p.2));
            let (mut at, mut pull) = ((0.0, 0.0, zmin - z0), (0.0, 0.0, 1.0));
            let mut tilt = vec![];
            for &(_, tag) in faces.iter().filter(|f| f.0 == mass) {
                let Some(f) = all.iter().find(|f| f.tag == tag) else {
                    continue;
                };
                if matches!(f.class, FaceClass::Up | FaceClass::Down) {
                    let c = centroid(f);
                    at = (c.0, c.1, c.2 - z0);
                    pull = f.normal;
                } else {
                    tilt.push(tag);
                }
            }
            if tilt.is_empty() || !zmin.is_finite() {
                say(ed, "status.mass.pick_face");
                return;
            }
            edit_form(ed, mass, "undo.mass.draft", move |b| Form::Draft {
                base: b,
                faces: tilt.clone(),
                at,
                pull,
                angle,
            });
        }
        Cmd::Offset => {
            let Some(&(mass, _)) = faces.first() else {
                say(ed, "status.mass.pick_face");
                return;
            };
            let d = t.value(0.5);
            let fs: Vec<u32> = faces.iter().filter(|f| f.0 == mass).map(|f| f.1).collect();
            edit_form(ed, mass, "undo.mass.offset", move |b| Form::Offset {
                base: b,
                faces: fs.clone(),
                d,
                keys: vec![],
            });
        }
        _ => {}
    }
}

fn edit_form(
    ed: &mut Editor,
    mass: i64,
    label: &'static str,
    f: impl Fn(Box<Form>) -> Form + 'static,
) {
    // push-pull, fillet, chamfer, shell, offset: the new history is frozen on a worker
    let warm = warm_edit(ed, mass, &f);
    let r = Rec::new(label, move |m| {
        m.edit_mass_form(mass, |b| f(b))?;
        Ok(1)
    });
    commit_heavy(ed, r, warm, |ed| ed.mass = None);
}

fn next_material(m: Option<&str>) -> Option<&'static str> {
    match m {
        None => Some("glass"),
        Some("glass") => Some("metal"),
        Some("metal") => Some("panel"),
        Some("panel") => Some("concrete"),
        _ => None,
    }
}

fn centroid(f: &ddc_model::MassFace) -> (f64, f64, f64) {
    let pts: Vec<_> = f.outline.iter().flatten().collect();
    let n = pts.len().max(1) as f64;
    pts.iter().fold((0.0, 0.0, 0.0), |a, p| {
        (a.0 + p.0 / n, a.1 + p.1 / n, a.2 + p.2 / n)
    })
}

/// Elevation of the mass's level: form coordinates are world minus it.
fn mass_z0(ed: &Editor, mass: i64) -> f64 {
    ed.model
        .mass(mass)
        .ok()
        .and_then(|s| ed.levels().into_iter().find(|l| l.0 == s.level).map(|l| l.2))
        .unwrap_or(0.0)
}

fn face_of(ed: &Editor, mass: i64, tag: u32) -> Option<ddc_model::MassFace> {
    ed.model
        .mass_faces(mass)
        .ok()
        .and_then(|fs| fs.into_iter().find(|f| f.tag == tag))
}

/// Where the camera ray (scene coordinates, z from `elev`) meets the plane of a flat face, in
/// world coordinates; on an axis-aligned face the in-plane coordinates snap to millimetres.
fn face_plane_hit(
    f: &ddc_model::MassFace,
    ray: ([f32; 3], [f32; 3]),
    elev: f64,
) -> Option<(f64, f64, f64)> {
    let n = [f.normal.0, f.normal.1, f.normal.2];
    let p0 = f.outline.first()?.first()?;
    let p0 = [p0.0, p0.1, p0.2];
    let o = [ray.0[0] as f64, ray.0[1] as f64, ray.0[2] as f64 + elev];
    let d = [ray.1[0] as f64, ray.1[1] as f64, ray.1[2] as f64];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let dn = dot(d, n);
    if dn.abs() < 1e-9 {
        return None;
    }
    let s = dot([p0[0] - o[0], p0[1] - o[1], p0[2] - o[2]], n) / dn;
    if !(s.is_finite() && s >= 0.0) {
        return None;
    }
    let mut p = [o[0] + d[0] * s, o[1] + d[1] * s, o[2] + d[2] * s];
    if n.iter().filter(|c| c.abs() > 1e-9).count() == 1 {
        for k in 0..3 {
            if n[k].abs() <= 1e-9 {
                p[k] = (p[k] * 1000.0).round() / 1000.0;
            }
        }
    }
    // exactly on the plane
    let off = dot([p[0] - p0[0], p[1] - p0[1], p[2] - p0[2]], n);
    Some((p[0] - n[0] * off, p[1] - n[1] * off, p[2] - n[2] * off))
}

/// Outline on a face: the first click picks the face (work plane) and the first corner, later
/// clicks add corners on that plane; the first corner again or a double click applies.
fn region_click(ed: &mut Editor, el: Option<u32>) {
    let Some(t) = ed.mass.as_ref() else { return };
    let (ray, drawing, first) = (t.ray, !t.pts3.is_empty(), t.picks.first().copied());
    let target = match first {
        Some(Pick::Face { mass, tag, el }) if drawing => Some((mass, tag, el)),
        _ => el.and_then(|e| decode(&ed.model, e).map(|(m, g)| (m, g, e))),
    };
    let Some((mass, tag, e)) = target else {
        say(ed, "status.mass.pick_face");
        return;
    };
    let Some(f) = face_of(ed, mass, tag) else {
        say(ed, "status.mass.pick_face");
        return;
    };
    if f.class == FaceClass::Curved {
        say(ed, "status.mass.curved");
        return;
    }
    let Some(p) = ray.and_then(|r| face_plane_hit(&f, r, ed.elevation)) else {
        say(ed, "status.mass.outline_off_face");
        return;
    };
    // closing distance: 2 % of the face size, at least 2 cm
    let dist = |a: (f64, f64, f64), b: (f64, f64, f64)| {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
    };
    let c = centroid(&f);
    let span = f.outline.iter().flatten().map(|q| dist(*q, c)).fold(0.0, f64::max);
    let snap = (0.04 * span).max(0.02);
    let near = |a, b| dist(a, b) < snap;
    ed.status = None;
    let t = ed.mass.as_mut().unwrap();
    if !drawing {
        t.picks = vec![Pick::Face { mass, tag, el: e }];
        t.pts3 = vec![p];
        return;
    }
    let closes = near(p, t.pts3[0]);
    let again = t.pts3.last().is_some_and(|q| near(p, *q));
    if t.pts3.len() >= 3 && (closes || again) {
        enter(ed);
    } else if !closes && !again {
        t.pts3.push(p);
    }
}

/// Outline being drawn on a face: corners, edges and the rubber band to the pointer.
pub fn overlay_3d(ui: &egui::Ui, ed: &Editor, view: &crate::viewport::Viewport) {
    use egui;
    let Some(t) = ed.mass.as_ref().filter(|t| !t.pts3.is_empty()) else {
        return;
    };
    let Some(rect) = view.frame_rect() else { return };
    let painter = ui.painter_at(rect);
    let ink = ui.visuals().selection.stroke.color;
    let scr: Vec<Pos2> = t
        .pts3
        .iter()
        .filter_map(|p| {
            let z = (p.2 - ed.elevation) as f32;
            view.screen_of(Vec3::new(p.0 as f32, p.1 as f32, z))
        })
        .collect();
    let stroke = egui::Stroke::new(2.0, ink);
    for w in scr.windows(2) {
        painter.line_segment([w[0], w[1]], stroke);
    }
    if let (Some(a), Some(p)) = (scr.last(), view.pointer) {
        painter.line_segment([*a, p], egui::Stroke::new(1.0, ink));
    }
    for (k, p) in scr.iter().enumerate() {
        let r = if k == 0 { 5.0 } else { 3.0 };
        painter.circle_stroke(*p, r, stroke);
    }
}

/// A 3D click with an active mass command; false means no command (normal picking).
pub fn click_3d(ed: &mut Editor, el: Option<u32>) -> bool {
    let Some(t) = ed.mass.as_ref() else {
        return false;
    };
    if t.cmd.in_plan() {
        return false;
    }
    if t.cmd == Cmd::PushRegion {
        region_click(ed, el);
        return true;
    }
    if t.cmd.on_surface() {
        // a freeform surface is taken in `after_show_3d`, which knows the scene's targets
        say(ed, "status.mass.pick_surface");
        return true;
    }
    let Some(e) = el else { return true };
    let pick = match decode(&ed.model, e) {
        Some((mass, tag)) => Pick::Face { mass, tag, el: e },
        None => match ed.sel_of_element(e) {
            Some(Sel::Wall(id)) => Pick::Elem("wall", id),
            Some(Sel::Slab(id)) => Pick::Elem("slab", id),
            Some(Sel::Curtain(id)) | Some(Sel::Panel(id, _)) => Pick::Elem("curtain_wall", id),
            Some(Sel::Mass(id)) => Pick::Elem("mass", id),
            _ => {
                say(ed, "status.mass.pick_element");
                return true;
            }
        },
    };
    let face = match pick {
        Pick::Face { mass, tag, .. } => Some((mass, tag)),
        Pick::Elem(..) => None,
    };
    let cmd = t.cmd;
    let st_name = t.t("name.mass.slab_type");
    let wt = ed.types.get(ed.current_type).map(|w| w.id);
    let one = |ed: &mut Editor, r: Rec| {
        if commit(ed, r) {
            ed.mass = None;
        }
    };
    let rec1 = |label: &'static str, f: Box<dyn Fn(&mut Model) -> Result<(), ddc_model::Error>>| {
        Rec::new(label, move |m| f(m).map(|_| 1))
    };
    match (cmd, face) {
        (Cmd::PushPull, Some((mass, tag))) => {
            let faces = ed.model.mass_faces(mass).unwrap_or_default();
            let Some(f) = faces.iter().find(|f| f.tag == tag) else {
                say(ed, "status.mass.pick_face");
                return true;
            };
            if f.class == FaceClass::Curved {
                say(ed, "status.mass.curved");
                return true;
            }
            // while dragging, a parallel face of the same mass is the target: push up to it
            // (kept parametric, the push follows that face when the history changes)
            if let Some(pp) = t.pp.clone().filter(|p| p.mass == mass && p.face != tag) {
                let n0 = faces.iter().find(|g| g.tag == pp.face).map(|g| g.normal);
                let parallel = n0.is_some_and(|n| {
                    let c = (
                        n.1 * f.normal.2 - n.2 * f.normal.1,
                        n.2 * f.normal.0 - n.0 * f.normal.2,
                        n.0 * f.normal.1 - n.1 * f.normal.0,
                    );
                    (c.0 * c.0 + c.1 * c.1 + c.2 * c.2).sqrt() < 1e-6
                });
                if parallel && pp.d.abs() > 1e-9 {
                    let face = pp.face;
                    edit_form(ed, mass, "undo.mass.push_pull", move |b| Form::PushTo {
                        base: b,
                        face,
                        region: vec![],
                        to: PushGoal::Face(tag),
                        keys: vec![],
                    });
                    return true;
                }
            }
            let c = centroid(f);
            let anchor = Vec3::new(c.0 as f32, c.1 as f32, (c.2 - ed.elevation) as f32);
            let nv = Vec3::new(f.normal.0 as f32, f.normal.1 as f32, f.normal.2 as f32);
            let paint: Vec<(u32, u32)> = faces
                .iter()
                .map(|f| (f.tag, material_tag(f.material.as_deref())))
                .collect();
            let t = ed.mass.as_mut().unwrap();
            // a new face starts afresh: no typed value, no stale preview
            t.picks = vec![pick];
            t.pp = Some(PushPull::new(mass, tag));
            t.axis = Some((anchor, nv));
            t.input.clear();
            t.drag0 = None;
            t.preview = None;
            t.paint = paint;
            t.rev += 1;
            ed.status = None;
        }
        (Cmd::WallByFace, Some((mass, tag))) => match wt {
            Some(wt) => one(
                ed,
                rec1(
                    "undo.mass.wall_by_face",
                    Box::new(move |m: &mut Model| m.wall_by_face(mass, tag, wt).map(|_| ())),
                ),
            ),
            None => say(ed, "status.mass.no_wall_type"),
        },
        (Cmd::CurtainByFace, Some((mass, tag))) => one(
            ed,
            rec1(
                "undo.mass.curtain_by_face",
                Box::new(move |m: &mut Model| m.curtain_by_face(mass, tag, 1.5).map(|_| ())),
            ),
        ),
        (Cmd::SlabByFace, Some((mass, tag))) => one(
            ed,
            with_slab_type("undo.mass.slab_by_face", st_name, move |m, st| {
                m.slab_by_face(mass, tag, st).map(|_| ())
            }),
        ),
        (Cmd::RoofByFace, Some((mass, tag))) => one(
            ed,
            with_slab_type("undo.mass.roof_by_face", st_name, move |m, st| {
                m.roof_by_face(mass, tag, st, 30.0).map(|_| ())
            }),
        ),
        (Cmd::Floors, Some((mass, _))) => one(
            ed,
            with_slab_type("undo.mass.floors", st_name, move |m, st| {
                m.floor_slabs(mass, st).map(|_| ())
            }),
        ),
        (Cmd::Paint, Some((mass, tag))) => {
            let cur = ed
                .model
                .mass_faces(mass)
                .ok()
                .and_then(|fs| fs.into_iter().find(|f| f.tag == tag))
                .and_then(|f| f.material);
            let next = next_material(cur.as_deref());
            ed.mass.as_mut().unwrap().picks.clear();
            // painting stays armed: click again for the next material
            let _ = commit(
                ed,
                Rec::new("undo.mass.paint", move |m| {
                    m.paint_face(mass, tag, next).map(|_| 1)
                }),
            );
        }
        (Cmd::SplitFace, Some((mass, tag))) => {
            let Some(f) = ed
                .model
                .mass_faces(mass)
                .ok()
                .and_then(|fs| fs.into_iter().find(|f| f.tag == tag))
            else {
                say(ed, "status.mass.pick_face");
                return true;
            };
            let c = centroid(&f);
            let z0 = ed.model.mass(mass).ok().and_then(|s| {
                ed.levels()
                    .into_iter()
                    .find(|l| l.0 == s.level)
                    .map(|l| l.2)
            });
            let at = (c.0, c.1, c.2 - z0.unwrap_or(0.0));
            // vertical faces split by a horizontal plane, others along x
            let dir = if f.class == FaceClass::Vertical {
                (0.0, 0.0, 1.0)
            } else {
                (1.0, 0.0, 0.0)
            };
            edit_form(ed, mass, "undo.mass.split_face", move |b| Form::SplitFace {
                base: b,
                face: tag,
                at,
                n: dir,
            });
        }
        (Cmd::Fillet | Cmd::Chamfer, Some((mass, _))) => {
            // face pairs name edges; Enter blends all picked edges in one step
            let t = ed.mass.as_mut().unwrap();
            t.picks
                .retain(|p| matches!(p, Pick::Face { mass: m, .. } if *m == mass));
            // double click on the last edge: its face clicked again applies
            if t.picks.last() == Some(&pick) {
                enter(ed);
            } else {
                t.picks.push(pick);
            }
        }
        (Cmd::Shell | Cmd::Offset | Cmd::Draft, Some((mass, _))) => {
            // faces of one mass only: a pick on another mass starts over
            let t = ed.mass.as_mut().unwrap();
            t.picks
                .retain(|p| matches!(p, Pick::Face { mass: m, .. } if *m == mass));
            // a second click on a picked face un-picks it
            if let Some(k) = t.picks.iter().position(|p| *p == pick) {
                t.picks.remove(k);
            } else {
                t.picks.push(pick);
            }
        }
        (Cmd::Union | Cmd::Subtract | Cmd::Intersect, Some((mass, _))) => {
            let t = ed.mass.as_mut().unwrap();
            match t.picks.first().map(|p| p.target()) {
                None => t.picks.push(pick),
                Some((_, a)) if a != mass => {
                    let op = match cmd {
                        Cmd::Union => BoolOp::Union,
                        Cmd::Subtract => BoolOp::Subtract,
                        _ => BoolOp::Intersect,
                    };
                    // the boolean runs on a worker; `combine_masses` gives `a` this history
                    let warm = match ed.model.mass(mass) {
                        Ok(sb) => warm_edit(ed, a, |fa| Form::Bool {
                            op,
                            a: fa,
                            b: Box::new(sb.form),
                        }),
                        Err(_) => vec![],
                    };
                    let r = rec1(
                        "undo.mass.combine",
                        Box::new(move |m: &mut Model| m.combine_masses(a, mass, op)),
                    );
                    commit_heavy(ed, r, warm, |ed| ed.mass = None);
                }
                Some(_) => {}
            }
        }
        (Cmd::Join | Cmd::Unjoin | Cmd::Cut, _) => {
            let t = ed.mass.as_mut().unwrap();
            match t.picks.first().map(|p| p.target()) {
                None => t.picks.push(pick),
                Some(a) if a != pick.target() => {
                    let b = pick.target();
                    let (label, f): (
                        &'static str,
                        Box<dyn Fn(&mut Model) -> Result<(), ddc_model::Error>>,
                    ) = match cmd {
                        Cmd::Join => (
                            "undo.mass.join",
                            Box::new(move |m: &mut Model| m.join_geometry(a, b)),
                        ),
                        Cmd::Unjoin => (
                            "undo.mass.unjoin",
                            Box::new(move |m: &mut Model| m.unjoin_geometry(a, b)),
                        ),
                        _ => (
                            "undo.mass.cut",
                            Box::new(move |m: &mut Model| m.cut_geometry(a, b)),
                        ),
                    };
                    one(ed, rec1(label, f));
                }
                Some(_) => {}
            }
        }
        _ => say(ed, "status.mass.pick_face"),
    }
    true
}

/// Recompute the push-pull preview body.
fn preview(ed: &mut Editor) {
    let Some(t) = ed.mass.as_mut() else { return };
    let Some(pp) = t.pp.as_mut() else { return };
    let d = pp.d;
    t.preview = pp.drag(&ed.model, d).map(|b| (pp.mass, b));
    t.rev += 1;
}

/// The ray point on a plane (anchor, normal); None if the ray is parallel to the plane or points backward.
fn hit_plane(ray: &ddc_render::Ray, p: Vec3, m: Vec3) -> Option<Vec3> {
    let den = ray.dir.dot(m);
    if den.abs() < 1e-5 {
        return None;
    }
    let t = (p - ray.origin).dot(m) / den;
    (t.is_finite() && t > 0.0).then(|| ray.at(t))
}

/// The parameter along the face normal under the cursor `pos`. The drag plane contains the line
/// (anchor, normal) and faces the camera; a face turned toward the camera uses an auxiliary
/// screen plane: the offset is along the screen vertical.
fn drag_param(view: &crate::viewport::Viewport, a: Vec3, n: Vec3, pos: Pos2) -> Option<f64> {
    let n = n.normalize_or_zero();
    let v = (a - view.camera.eye()).normalize_or_zero();
    if n == Vec3::ZERO || v == Vec3::ZERO {
        return None;
    }
    let ray = view.ray_at(pos)?;
    let s = if n.dot(v).abs() > 0.97 {
        let h = hit_plane(&ray, a, v)?;
        let s0 = view.screen_of(a)?;
        let up = view.ray_at(Pos2::new(s0.x, s0.y - 1.0))?;
        let u = (hit_plane(&up, a, v)? - a).normalize_or_zero();
        // dragging up pulls a face that looks at the camera towards the viewer
        let sign = if n.dot(v) < 0.0 { 1.0 } else { -1.0 };
        (h - a).dot(u) * sign
    } else {
        let m = (v - n * v.dot(n)).normalize_or_zero();
        (hit_plane(&ray, a, m)? - a).dot(n)
    };
    let s = s as f64;
    s.is_finite().then_some(s)
}

/// The largest offset jump per mouse event, m.
const MAX_STEP: f64 = 25.0;

/// Dragging in 3D: an offset along the face normal from the cursor ray meeting the drag
/// plane (5 cm step, no jumps); release commits.
pub fn drive_3d(ed: &mut Editor, view: &mut crate::viewport::Viewport) {
    let active = ed.mass.as_ref().is_some_and(|t| t.pp.is_some());
    view.capture = active;
    if !active {
        return;
    }
    let t = ed.mass.as_mut().unwrap();
    if view.grab.is_some() {
        let s = match (t.axis, view.pointer) {
            (Some((a, n)), Some(p)) => drag_param(view, a, n, p),
            _ => None,
        };
        if let (Some(s), Some(pp)) = (s, t.pp.as_mut()) {
            let (s0, d0) = *t.drag0.get_or_insert((s, pp.d));
            let d = ((d0 + s - s0) / 0.05).round() * 0.05;
            let d = d.clamp(pp.d - MAX_STEP, pp.d + MAX_STEP);
            if d.is_finite() && (pp.d - d).abs() > 1e-9 {
                pp.d = d;
                t.input = format!("{d:.2}");
                preview(ed);
            }
        }
    }
    if view.released {
        view.grab = None;
        view.released = false;
        if let Some(t) = ed.mass.as_mut() {
            t.drag0 = None;
        }
        // a release without a distance keeps the face picked and creates nothing
        if ed
            .mass
            .as_ref()
            .and_then(|t| t.pp.as_ref())
            .is_some_and(|pp| pp.d.abs() >= 1e-9)
        {
            enter(ed);
        }
    }
}

/// A 3D view with a mass command: a click picks a face/element, left-drag pulls a face;
/// `keys`: the view is alone (the plan does not read the keyboard), number, Enter, Esc go to the command.
pub fn show_3d(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    view: &mut crate::viewport::Viewport,
    scene: &ddc_render::Scene,
    keys: bool,
) {
    let click = view.show(ui, scene);
    if keys {
        keys_3d(ui, ed);
    }
    if let Some(el) = after_show_3d(ed, view, click) {
        ed.select_element(el);
    }
    overlay_3d(ui, ed, view);
}

/// Push-pull owns the drag: the left button does not go to orbit and the 3D tools.
pub fn capturing(ed: &Editor) -> bool {
    ed.mass.as_ref().is_some_and(|t| t.pp.is_some())
}

/// After `Viewport::show`: push-pull dragging and a click on a mass face.
/// Returns the click if the mass did not take it (normal picking follows).
pub fn after_show_3d(
    ed: &mut Editor,
    view: &mut crate::viewport::Viewport,
    click: Option<Option<u32>>,
) -> Option<Option<u32>> {
    drive_3d(ed, view);
    if let (Some(t), Some(_)) = (ed.mass.as_mut(), click) {
        t.ray = view.click_ray;
    }
    let click = crate::opening_tool::void_click(ed, view, click);
    if let (Some(t), Some(Some(el))) = (ed.mass.as_ref(), click) {
        let free = view.e3.ext_target(el).filter(|g| g.cat == Cat3::Ext(crate::scene_ids::EXT_FREEFORM));
        if let (true, Some(g)) = (t.cmd.on_surface(), free) {
            pick_surface(ed, g.id);
            return None;
        }
    }
    match click {
        Some(el) if click_3d(ed, el) => None,
        c => c,
    }
}

/// Mass command keys in 3D (numbers, Enter, Esc, Backspace).
pub fn keys_3d(ui: &mut egui::Ui, ed: &mut Editor) {
    let free = crate::ui_kit::canvas_owns_keys(ui.ctx());
    if free && ed.mass.is_some() && !ui.ctx().wants_keyboard_input() {
        for e in ui.input(|i| i.events.clone()) {
            let ev = match e {
                egui::Event::Text(s) => {
                    s.chars().for_each(|c| {
                        handle(ed, Event::Char(c));
                    });
                    continue;
                }
                egui::Event::Key {
                    key, pressed: true, ..
                } => match key {
                    egui::Key::Enter => Event::Key(Key::Enter),
                    egui::Key::Escape => Event::Key(Key::Escape),
                    egui::Key::Backspace => Event::Key(Key::Backspace),
                    _ => continue,
                },
                _ => continue,
            };
            handle(ed, ev);
        }
    }
}

/// Highlight: the face under the cursor (mass) and the command's picked faces.
pub fn hover_range(e: u32) -> Option<(u32, u32)> {
    (MASS_BASE..MASS_END).contains(&e).then_some((e, e + 1))
}

/// Highlight of the command's picked faces: face elements are not contiguous and the view has one
/// range, so the last picked face is highlighted.
pub fn picked_range(ed: &Editor) -> Option<(u32, u32)> {
    ed.mass.as_ref()?.picks.iter().rev().find_map(|p| match p {
        Pick::Face { el, .. } => Some((*el, el + 1)),
        _ => None,
    })
}

/// The scene with the preview body instead of the mass; `shown` is the mass replaced last time.
pub fn apply_preview(ed: &Editor, meshes: &mut Vec<Arc<SceneMesh>>, shown: &mut Option<i64>) {
    let ids = ed.model.masses().unwrap_or_default();
    let slot = |id: i64| ids.iter().position(|x| *x == id);
    let tool = ed.mass.as_ref();
    let now = tool.and_then(|t| t.preview.as_ref().map(|p| p.0));
    let mut replace = |id: i64, body: Option<&MassBody>, paint: Option<&[(u32, u32)]>| {
        let Some(k) = slot(id) else { return };
        let lo = slot_base(k);
        let hi = lo + (1 << FACE_BITS);
        meshes.retain(|m| m.element < lo || m.element >= hi);
        meshes.extend(
            mass_meshes(&ed.model, k, id, body, paint, ed.elevation)
                .into_iter()
                .map(Arc::new),
        );
    };
    // the same mass is replaced by the new preview below: no rebuild of its plain body
    if let Some(id) = shown.take().filter(|id| Some(*id) != now) {
        replace(id, None, None);
    }
    if let Some((t, (id, b))) = tool.and_then(|t| t.preview.as_ref().map(|p| (t, p))) {
        let paint = (!t.paint.is_empty()).then_some(t.paint.as_slice());
        replace(*id, Some(b), paint);
        *shown = Some(*id);
    }
}

pub fn preview_rev(ed: &Editor) -> u64 {
    ed.mass.as_ref().map_or(0, |t| t.rev)
}

macro_rules! handlers {
    ($($f:ident => $c:expr),* $(,)?) => {
        $(fn $f(app: &mut EditorApp) {
            let c = $c;
            app.view3d = !c.in_plan();
            arm(&mut app.ed, c, app.sh.cat.lang);
            if c.on_surface() {
                // a freeform already selected in 3D is the pick
                let sel = app.view.e3.sel.iter().find(|g| g.cat == Cat3::Ext(crate::scene_ids::EXT_FREEFORM));
                if let Some(g) = sel.copied() {
                    pick_surface(&mut app.ed, g.id);
                }
            }
        })*
        const HANDLERS: &[(Cmd, fn(&mut EditorApp))] = &[$(($c, $f)),*];
    };
}

handlers! {
    h_extrude => Cmd::Extrude,
    h_revolve => Cmd::Revolve,
    h_sweep => Cmd::Sweep,
    h_blend => Cmd::Blend,
    h_swept_blend => Cmd::SweptBlend,
    h_void => Cmd::Void,
    h_push_pull => Cmd::PushPull,
    h_fillet => Cmd::Fillet,
    h_chamfer => Cmd::Chamfer,
    h_shell => Cmd::Shell,
    h_offset => Cmd::Offset,
    h_split_face => Cmd::SplitFace,
    h_push_region => Cmd::PushRegion,
    h_draft => Cmd::Draft,
    h_paint => Cmd::Paint,
    h_union => Cmd::Union,
    h_subtract => Cmd::Subtract,
    h_intersect => Cmd::Intersect,
    h_join => Cmd::Join,
    h_unjoin => Cmd::Unjoin,
    h_cut => Cmd::Cut,
    h_wall_by_face => Cmd::WallByFace,
    h_curtain_by_face => Cmd::CurtainByFace,
    h_slab_by_face => Cmd::SlabByFace,
    h_roof_by_face => Cmd::RoofByFace,
    h_floors => Cmd::Floors,
    h_freeform => Cmd::Freeform,
    h_panelize => Cmd::Panelize,
    h_surface_shell => Cmd::SurfaceShell,
    h_thicken => Cmd::Thicken,
}

/// Registration of the "Masses" tab commands (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    for (id, cmd) in COMMANDS {
        if let Some((_, h)) = HANDLERS.iter().find(|h| h.0 == *cmd) {
            app.register_command(id, *h);
        }
    }
}

/// Demo: a tower (push-pull, fillet, an atrium void), a revolved dome, a canopy
/// by sweep with a transition; walls and a curtain wall by faces, slabs by floors, painting.
pub fn demo_scene(ed: &mut Editor) -> Result<(), String> {
    use ddc_geom::csg::ftag;
    let e = |e: ddc_model::Error| e.to_string();
    let m = &mut ed.model;
    let l0 = ed.level;
    for k in 1..4 {
        m.add_level(&format!("\u{42d}\u{442}\u{430}\u{436} {}", k + 1), 3.5 * k as f64)
            .map_err(e)?;
    }
    // op 1 extrude (1:1 top, 1:4 the inner step face), op 2 push/pull, op 3 fillet
    let tower = Form::Fillet {
        base: Form::PushPull {
            base: Form::Extrude {
                ring: vec![
                    (0., 0.),
                    (14., 0.),
                    (14., 6.),
                    (8., 6.),
                    (8., 10.),
                    (0., 10.),
                ],
                z0: 0.0,
                h: 10.5,
                ids: vec![],
            }
            .base(),
            face: ftag(1, 1),
            d: 3.5,
            keys: vec![],
        }
        .base(),
        faces: (ftag(1, 1), ftag(1, 4)),
        r: 1.5,
        seg: 10,
    };
    let t = m
        .add_mass(&MassSpec {
            level: l0,
            name: "\u{41a}\u{43e}\u{440}\u{43f}\u{443}\u{441}".into(),
            void: false,
            inplace: false,
            form: tower,
        })
        .map_err(e)?;
    let oculus: Vec<(f64, f64)> = (0..8)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / 8.0;
            (24.0 + 0.6 * a.cos(), 5.0 + 0.6 * a.sin())
        })
        .collect();
    let dome = Form::Bool {
        op: BoolOp::Subtract,
        a: Form::Revolve {
            profile: (0..=8)
                .map(|i| {
                    let a = std::f64::consts::FRAC_PI_2 * i as f64 / 8.0;
                    (3.0 * a.cos(), 3.0 * a.sin())
                })
                .chain([(0.0, 0.0)])
                .collect(),
            axis: (24.0, 5.0),
            angle: std::f64::consts::TAU,
            seg: 32,
            ids: vec![],
        }
        .base(),
        b: Form::Extrude {
            ring: oculus,
            z0: 2.0,
            h: 2.0,
            ids: vec![],
        }
        .base(),
    };
    m.add_mass(&MassSpec {
        level: l0,
        name: "\u{41a}\u{443}\u{43f}\u{43e}\u{43b}".into(),
        void: false,
        inplace: true,
        form: dome,
    })
    .map_err(e)?;
    let canopy = Form::SweptBlend {
        p0: rect(-0.15, -2.0, 0.15, 2.0),
        p1: rect(-0.1, -0.6, 0.1, 0.6),
        path: vec![(17., -2., 0.), (17., 4., 3.5), (17., 10., 4.5)],
        ids: vec![],
    };
    m.add_mass(&MassSpec {
        level: l0,
        name: "\u{41d}\u{430}\u{432}\u{435}\u{441}".into(),
        void: false,
        inplace: true,
        form: canopy,
    })
    .map_err(e)?;
    let void = m
        .add_mass(&MassSpec {
            level: l0,
            name: "\u{410}\u{442}\u{440}\u{438}\u{443}\u{43c}".into(),
            void: true,
            inplace: true,
            form: Form::Cuboid {
                lo: (2.0, 2.0, 3.4),
                hi: (6.0, 8.0, 15.0),
            },
        })
        .map_err(e)?;
    let st = m
        .add_slab_type(
            "\u{41f}\u{43b}\u{438}\u{442}\u{430} 200",
            &[ddc_model::LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .map_err(e)?;
    let wt = m
        .add_wall_type(
            "\u{41a}\u{43b}\u{430}\u{434}\u{43a}\u{430} 300",
            &[ddc_model::LayerSpec {
                material: "brick".into(),
                thickness: 0.3,
            }],
        )
        .map_err(e)?;
    let faces = m.mass_faces(t).map_err(e)?;
    let find = |f: &dyn Fn(&ddc_model::MassFace) -> bool, what: &str| {
        faces
            .iter()
            .find(|x| f(x))
            .map(|x| x.tag)
            .ok_or(format!("no {what} face"))
    };
    let south = find(&|f| f.normal.1 < -0.9, "south")?;
    let east = find(&|f| f.normal.0 > 0.9, "east")?;
    let west = find(&|f| f.normal.0 < -0.9, "west")?;
    m.curtain_by_face(t, south, 1.75).map_err(e)?;
    m.wall_by_face(t, east, wt).map_err(e)?;
    for s in m.floor_slabs(t, st).map_err(e)? {
        m.cut_geometry(("mass", void), ("slab", s)).map_err(e)?;
    }
    let next = m.next_mass_op(t).map_err(e)?;
    m.edit_mass_form(t, |b| Form::SplitFace {
        base: b,
        face: west,
        at: (0.0, 5.0, 0.0),
        n: (0.0, 1.0, 0.0),
    })
    .map_err(e)?;
    m.paint_face(t, ftag(next, 1), Some("glass")).map_err(e)?;
    ed.reload().map_err(e)?;
    Ok(())
}

/// Snapshot: push-pull of the demo scene tower top by 3 m (preview, no commit).
pub fn demo_preview(ed: &mut Editor, lang: Lang) -> Option<()> {
    let id = *ed.model.masses().ok()?.first()?;
    let tags = shown_body(&ed.model, id)?.tags();
    let idx = tags.iter().position(|t| *t == ddc_geom::csg::ftag(1, 1))?;
    arm(ed, Cmd::PushPull, lang);
    click_3d(ed, Some(slot_base(0) + idx as u32));
    handle(ed, Event::Char('3'));
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::csg::ftag;
    use ddc_geom::v2;

    fn boxed(ed: &mut Editor) -> i64 {
        let l = ed.level;
        ed.model
            .add_mass(&MassSpec {
                level: l,
                name: "Box".into(),
                void: false,
                inplace: false,
                form: Form::Cuboid {
                    lo: (0., 0., 0.),
                    hi: (2., 2., 2.),
                },
            })
            .unwrap()
    }

    fn typed_enter(ed: &mut Editor, s: &str) {
        s.chars().for_each(|c| {
            handle(ed, Event::Char(c));
        });
        handle(ed, Event::Key(Key::Enter));
    }

    /// Freeform over two corners at the typed rise, then thickness, roof and panels on it:
    /// each command is one undo step through the ops path.
    #[test]
    fn freeform_commands_create_and_undo() {
        crate::ops_exec::link();
        let mut ed = Editor::new_in_memory().unwrap();
        arm(&mut ed, Cmd::Freeform, Lang::En);
        "4".chars().for_each(|c| {
            handle(&mut ed, Event::Char(c));
        });
        handle(&mut ed, Event::Click(v2(0.0, 0.0)));
        handle(&mut ed, Event::Click(v2(20.0, 12.0)));
        assert!(ed.mass.is_none(), "{:?}", ed.status);
        let ff = ed.model.freeforms(None).unwrap();
        assert_eq!(ff.len(), 1);
        let (id, spec) = &ff[0];
        let top = spec.surface.point(0.5, 0.5);
        assert!((top.z - 4.0).abs() < 1e-6 && (top.x - 10.0).abs() < 1e-6, "{top:?}");
        let id = *id;
        // Enter before a pick asks for the surface and writes nothing
        arm(&mut ed, Cmd::Thicken, Lang::En);
        assert_eq!(ed.mass.as_ref().unwrap().step(), "first");
        handle(&mut ed, Event::Key(Key::Enter));
        assert!(ed.mass.is_some() && ed.model.freeform(id).unwrap().thickness == 0.0);
        pick_surface(&mut ed, id);
        assert_eq!(ed.mass.as_ref().unwrap().step(), "value");
        typed_enter(&mut ed, "0.3");
        assert!((ed.model.freeform(id).unwrap().thickness - 0.3).abs() < 1e-9, "{:?}", ed.status);
        ed.undo();
        assert_eq!(ed.model.freeform(id).unwrap().thickness, 0.0);
        arm(&mut ed, Cmd::SurfaceShell, Lang::En);
        pick_surface(&mut ed, id);
        typed_enter(&mut ed, "0.25");
        assert_eq!(ed.model.shell_elems(None).unwrap().len(), 1, "{:?}", ed.status);
        arm(&mut ed, Cmd::Panelize, Lang::En);
        pick_surface(&mut ed, id);
        typed_enter(&mut ed, "2");
        assert_eq!(ed.model.panel_systems_on(id).unwrap().len(), 1, "{:?}", ed.status);
        ed.undo();
        assert!(ed.model.panel_systems_on(id).unwrap().is_empty());
        ed.undo();
        assert!(ed.model.shell_elems(None).unwrap().is_empty());
        ed.undo();
        assert!(ed.model.freeforms(None).unwrap().is_empty());
    }

    #[test]
    fn push_pull_preview_does_not_touch_the_model_until_commit() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        let mut pp = PushPull::new(id, ftag(1, 1));
        let prev = pp.drag(&ed.model, 1.0).unwrap();
        assert!((prev.volume() - 12.0).abs() < 1e-9);
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
        ed.apply_ext(pp.rec()).unwrap();
        assert!((ed.model.mass_body(id).unwrap().volume() - 12.0).abs() < 1e-9);
        ed.undo();
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
        let mut meshes = vec![];
        scene_meshes(&ed.model, 0.0, &mut meshes);
        // one scene element per face; a hover picks the face back
        assert_eq!(meshes.len(), 6);
        assert!(meshes.iter().all(|m| m.element >= MASS_BASE));
        let top = meshes
            .iter()
            .find(|m| m.positions.iter().all(|p| p[2] == 2.0))
            .unwrap();
        assert_eq!(decode(&ed.model, top.element), Some((id, ftag(1, 1))));
    }

    #[test]
    fn push_pull_tool_previews_typed_distance_and_commits_one_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        let mut meshes = vec![];
        scene_meshes(&ed.model, 0.0, &mut meshes);
        let top = meshes
            .iter()
            .find(|m| m.positions.iter().all(|p| p[2] == 2.0))
            .unwrap()
            .element;
        arm(&mut ed, Cmd::PushPull, Lang::En);
        assert!(click_3d(&mut ed, Some(top)));
        for c in "1.5".chars() {
            ed.handle(Event::Char(c));
        }
        let t = ed.mass.as_ref().unwrap();
        assert!((t.preview.as_ref().unwrap().1.volume() - 14.0).abs() < 1e-9);
        let mut shown = None;
        apply_preview(&ed, &mut meshes, &mut shown);
        assert_eq!(shown, Some(id));
        assert!(meshes
            .iter()
            .any(|m| m.positions.iter().any(|p| p[2] == 3.5)));
        ed.handle(Event::Key(Key::Enter));
        assert!(ed.mass.is_none());
        assert!((ed.model.mass_body(id).unwrap().volume() - 14.0).abs() < 1e-9);
        ed.undo();
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
    }

    fn face_el(ed: &Editor, id: i64, tag: u32) -> u32 {
        let k = ed.model.masses().unwrap().iter().position(|x| *x == id).unwrap();
        let tags = shown_body(&ed.model, id).unwrap().tags();
        slot_base(k) + tags.iter().position(|t| *t == tag).unwrap() as u32
    }

    /// A 3D click on face `tag` of mass `id`.
    fn face_click(ed: &mut Editor, id: i64, tag: u32) -> bool {
        let el = face_el(ed, id, tag);
        click_3d(ed, Some(el))
    }

    #[test]
    fn fillet_tool_blends_all_picked_edges_in_one_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        arm(&mut ed, Cmd::Fillet, Lang::En);
        for (k, tag) in [1, 2, 1, 3].into_iter().enumerate() {
            let el = face_el(&ed, id, ftag(1, tag));
            assert!(click_3d(&mut ed, Some(el)));
            let want = if k % 2 == 0 { "next" } else { "first" };
            assert_eq!(ed.mass.as_ref().unwrap().step(), want);
        }
        for c in "0.4".chars() {
            ed.handle(Event::Char(c));
        }
        ed.handle(Event::Key(Key::Enter));
        assert!(ed.mass.is_none());
        let r: f64 = 0.4;
        let pi = std::f64::consts::PI;
        // two rounded top edges meeting in a mitre (8-step arcs)
        let want = 8.0 - 4.0 * r * r * (1.0 - pi / 4.0) + r.powi(3) * (5.0 / 3.0 - pi / 2.0);
        let v = ed.model.mass_body(id).unwrap().volume();
        assert!((v - want).abs() < 1e-2, "{v} {want}");
        ed.undo();
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn push_region_draws_an_outline_on_the_face_and_closes_on_the_first_corner() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        arm(&mut ed, Cmd::PushRegion, Lang::En);
        let top = face_el(&ed, id, ftag(1, 1));
        let z = (10.0 - ed.elevation) as f32;
        let click = |ed: &mut Editor, x: f32, y: f32| {
            ed.mass.as_mut().unwrap().ray = Some(([x, y, z], [0.0, 0.0, -1.0]));
            assert!(click_3d(ed, Some(top)));
        };
        click(&mut ed, 0.5, 0.5);
        assert_eq!(ed.mass.as_ref().unwrap().step(), "next");
        for (x, y) in [(1.5, 0.5), (1.5, 1.5), (0.5, 1.5)] {
            click(&mut ed, x, y);
        }
        assert_eq!(ed.mass.as_ref().unwrap().pts3.len(), 4);
        for c in "0.5".chars() {
            ed.handle(Event::Char(c));
        }
        // the first corner again closes the outline and applies
        click(&mut ed, 0.501, 0.499);
        assert!(ed.mass.is_none());
        let v = ed.model.mass_body(id).unwrap().volume();
        assert!((v - 8.5).abs() < 1e-9, "{v}");
        ed.undo();
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn draft_tool_tilts_picked_sides_about_the_base() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        arm(&mut ed, Cmd::Draft, Lang::En);
        for k in 2..6 {
            assert!(face_click(&mut ed, id, ftag(1, k)));
        }
        for c in "10".chars() {
            ed.handle(Event::Char(c));
        }
        ed.handle(Event::Key(Key::Enter));
        assert!(ed.mass.is_none());
        let t = 10f64.to_radians().tan();
        let (a1, a2) = (4.0, (2.0 - 4.0 * t).powi(2));
        let want = 2.0 / 3.0 * (a1 + a2 + (a1 * a2).sqrt());
        let v = ed.model.mass_body(id).unwrap().volume();
        assert!((v - want).abs() < 1e-9, "{v} {want}");
        ed.undo();
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn chamfer_double_click_on_the_last_face_applies() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        arm(&mut ed, Cmd::Chamfer, Lang::En);
        for tag in [1, 2, 1, 3] {
            assert!(face_click(&mut ed, id, ftag(1, tag)));
        }
        assert!(ed.mass.is_some());
        // the second click of a double click lands on the same face
        assert!(face_click(&mut ed, id, ftag(1, 3)));
        assert!(ed.mass.is_none());
        let d: f64 = 0.3;
        // two chamfered top edges, mitred where they meet
        let want = 8.0 - 2.0 * 2.0 * d * d / 2.0 + d.powi(3) / 3.0;
        let v = ed.model.mass_body(id).unwrap().volume();
        assert!((v - want).abs() < 1e-6, "{v} {want}");
        ed.undo();
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn shell_tool_removes_picked_faces_with_typed_thickness() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        arm(&mut ed, Cmd::Shell, Lang::En);
        // roof and one side picked, the side clicked again is kept
        for tag in [1, 2, 2] {
            assert!(face_click(&mut ed, id, ftag(1, tag)));
        }
        assert_eq!(ed.mass.as_ref().unwrap().picks.len(), 1);
        for c in "0.2".chars() {
            ed.handle(Event::Char(c));
        }
        ed.handle(Event::Key(Key::Enter));
        assert!(ed.mass.is_none());
        let b = ed.model.mass_body(id).unwrap();
        b.validate(ddc_geom::Tol::default()).unwrap();
        let v = b.volume();
        assert!((v - (8.0 - 1.6 * 1.6 * 1.8)).abs() < 1e-9, "{v}");
        ed.undo();
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn push_pull_click_on_a_parallel_face_pushes_up_to_it() {
        let mut ed = Editor::new_in_memory().unwrap();
        let l = ed.level;
        let cub = |x: f64, h: f64| {
            Form::Cuboid {
                lo: (x, 0., 0.),
                hi: (x + 2., 2., h),
            }
            .base()
        };
        let id = ed
            .model
            .add_mass(&MassSpec {
                level: l,
                name: "Pair".into(),
                void: false,
                inplace: false,
                form: Form::Bool {
                    op: BoolOp::Union,
                    a: cub(0., 2.),
                    b: cub(4., 4.),
                },
            })
            .unwrap();
        arm(&mut ed, Cmd::PushPull, Lang::En);
        assert!(face_click(&mut ed, id, ftag(1, 1)));
        ed.handle(Event::Char('1'));
        assert!(face_click(&mut ed, id, ftag(2, 1)));
        assert!(ed.mass.is_none());
        assert!((ed.model.mass_body(id).unwrap().volume() - 32.0).abs() < 1e-9);
        ed.undo();
        assert!((ed.model.mass_body(id).unwrap().volume() - 24.0).abs() < 1e-9);
    }

    #[test]
    fn plan_tools_create_forms_and_face_tools_build_elements() {
        let mut ed = Editor::new_in_memory().unwrap();
        arm(&mut ed, Cmd::Extrude, Lang::En);
        for p in [v2(0., 0.), v2(6., 0.), v2(6., 4.), v2(0., 4.)] {
            ed.handle(Event::Click(p));
        }
        ed.handle(Event::Char('9'));
        ed.handle(Event::Key(Key::Enter));
        let ids = ed.model.masses().unwrap();
        assert_eq!(ids.len(), 1);
        let b = ed.model.mass_body(ids[0]).unwrap();
        assert!((b.volume() - 216.0).abs() < 1e-9, "{}", b.volume());
        // wall by the south face from a 3D click
        let mut meshes = vec![];
        scene_meshes(&ed.model, 0.0, &mut meshes);
        let south = meshes
            .iter()
            .find(|m| m.positions.iter().all(|p| p[1] == 0.0))
            .unwrap()
            .element;
        arm(&mut ed, Cmd::WallByFace, Lang::En);
        assert!(click_3d(&mut ed, Some(south)));
        assert_eq!(ed.walls.len(), 1);
        ed.undo();
        assert!(ed.walls.is_empty());
        arm(&mut ed, Cmd::Paint, Lang::En);
        assert!(click_3d(&mut ed, Some(south)));
        let f = ed.model.mass_faces(ids[0]).unwrap();
        assert!(f.iter().any(|f| f.material.as_deref() == Some("glass")));
    }

    #[test]
    fn every_ribbon_command_has_a_handler() {
        for (id, cmd) in COMMANDS {
            assert!(HANDLERS.iter().any(|h| h.0 == *cmd), "{id}");
        }
        // 26 + freeform, panelize, surface shell and thicken
        assert_eq!(COMMANDS.len(), 30);
    }

    #[test]
    fn demo_scene_builds() {
        let mut ed = Editor::new_in_memory().unwrap();
        demo_scene(&mut ed).unwrap();
        let mut meshes = vec![];
        scene_meshes(&ed.model, 0.0, &mut meshes);
        assert!(meshes.iter().any(|m| m.tag == 8), "painted glass face");
        let rows = ed.model.mass_schedule().unwrap();
        assert_eq!(rows.iter().filter(|r| r.name == crate::app2_fx::fx("mass_body_name")).count(), 4);
        assert!(rows.iter().all(|r| r.area > 0.0));
        assert!(ed.model.cut_elements().unwrap().len() >= 3);
        assert!(ed
            .model
            .mass_hosts(ed.model.masses().unwrap()[0])
            .unwrap()
            .iter()
            .all(|h| !h.orphan));
    }

    fn top_face(ed: &Editor) -> u32 {
        let mut meshes = vec![];
        scene_meshes(&ed.model, 0.0, &mut meshes);
        meshes
            .iter()
            .find(|m| m.positions.iter().all(|p| p[2] == 2.0))
            .unwrap()
            .element
    }

    fn count(ed: &Editor, table: &str) -> i64 {
        ed.model
            .sql()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn push_pull_backspace_recomputes_distance_and_preview() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        let top = top_face(&ed);
        arm(&mut ed, Cmd::PushPull, Lang::En);
        assert!(click_3d(&mut ed, Some(top)));
        ed.handle(Event::Char('1'));
        ed.handle(Event::Char('5'));
        ed.handle(Event::Key(Key::Backspace));
        let t = ed.mass.as_ref().unwrap();
        assert!((t.pp.as_ref().unwrap().d - 1.0).abs() < 1e-12);
        assert!((t.preview.as_ref().unwrap().1.volume() - 12.0).abs() < 1e-9);
        // everything erased: zero distance, no preview, Enter creates nothing
        ed.handle(Event::Key(Key::Backspace));
        assert!(ed.mass.as_ref().unwrap().preview.is_none());
        ed.handle(Event::Key(Key::Enter));
        assert!(ed.mass.is_some());
        assert!(!ed.can_undo());
        // a new face pick starts afresh
        ed.handle(Event::Char('2'));
        assert!(click_3d(&mut ed, Some(top)));
        let t = ed.mass.as_ref().unwrap();
        assert!(t.input.is_empty() && t.preview.is_none());
        assert_eq!(t.pp.as_ref().unwrap().d, 0.0);
        assert!((ed.model.mass_body(id).unwrap().volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn mass_and_wall_go_in_one_delete() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        crate::modify_tools::tests::walls(&mut ed, &[((40.0, 0.0), (44.0, 0.0))]);
        let wall = ed.walls[0].id;
        ed.selection = vec![Sel::Mass(id), Sel::Wall(wall)];
        let steps = ed.undo_len();
        ed.delete_selection();
        assert!(ed.model.masses().unwrap().is_empty());
        assert!(ed.walls.is_empty());
        assert_eq!(ed.undo_len(), steps + 1);
        ed.undo();
        assert_eq!(ed.model.masses().unwrap().len(), 1);
        assert_eq!(ed.walls.len(), 1);
    }

    #[test]
    fn mass_selection_deletes_and_undoes_in_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = boxed(&mut ed);
        ed.reload().unwrap();
        assert!(ed.mass_plan.iter().any(|m| m.0 == id), "plan section");
        ed.select_element(Some(top_face(&ed)));
        assert_eq!(ed.selection, vec![Sel::Mass(id)]);
        assert_eq!(
            ed.highlight(),
            Some((slot_base(0), slot_base(0) + (1 << FACE_BITS)))
        );
        ed.delete_selection();
        assert!(ed.model.masses().unwrap().is_empty());
        assert!(ed.selection.is_empty());
        assert!(ed.can_undo());
        ed.undo();
        assert!(!ed.can_undo());
        assert_eq!(ed.model.masses().unwrap().len(), 1);
        assert!(ed
            .mass_plan
            .iter()
            .any(|m| m.0 == ed.model.masses().unwrap()[0]));
    }

    #[test]
    fn slab_by_face_without_slab_type_undoes_in_one_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        boxed(&mut ed);
        if count(&ed, "slab_type") > 0 {
            ed.model.sql().execute("DELETE FROM slab_type", []).unwrap();
        }
        let top = top_face(&ed);
        arm(&mut ed, Cmd::SlabByFace, Lang::En);
        assert!(click_3d(&mut ed, Some(top)));
        assert_eq!(count(&ed, "slab"), 1, "{:?}", ed.status);
        assert_eq!(count(&ed, "slab_type"), 1);
        ed.undo();
        assert!(!ed.can_undo());
        assert_eq!(count(&ed, "slab"), 0);
        assert_eq!(
            count(&ed, "slab_type"),
            0,
            "type created by the command is undone too"
        );
        assert_eq!(ed.model.masses().unwrap().len(), 1);
    }

    #[test]
    fn mass_strings_have_keys_in_both_languages() {
        let cat = Catalog::new(Lang::En);
        let mut keys: Vec<String> = vec![];
        for (id, cmd) in COMMANDS {
            let mut t = MassTool::new(*cmd, Lang::En);
            keys.push(format!("hint.{id}.{}", t.step()));
            if cmd.in_plan() {
                t.pts.push(v2(0.0, 0.0));
            } else if *cmd == Cmd::PushPull {
                t.pp = Some(PushPull::new(1, 1));
            } else {
                t.picks.push(Pick::Elem("wall", 1));
            }
            keys.push(format!("hint.{id}.{}", t.step()));
        }
        let src = include_str!("mass_tools.rs");
        for pre in ["\"status.mass.", "\"undo.mass.", "\"name.mass."] {
            for part in src.split(pre).skip(1) {
                let k = part.split('"').next().unwrap_or("");
                // empty: the prefix list of this test itself
                if !k.is_empty() {
                    keys.push(format!("{}{k}", &pre[1..]));
                }
            }
        }
        for k in keys {
            for l in [Lang::Ru, Lang::En] {
                assert!(cat.table(l).contains_key(&k), "{l:?}: {k}");
            }
        }
    }
}
