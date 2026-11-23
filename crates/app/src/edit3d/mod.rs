// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Direct editing in 3D: selection of all categories with hover highlight, grips per
//! category ([`grips`]; providers from other modules plug in via [`grips::Provider`]),
//! the move/rotate gizmo ([`gizmo`]), snaps ([`snap`]), work planes
//! ([`plane`]), creation of walls/slabs/columns/beams on a plane ([`create`]), window/crossing
//! selection box, measurement ([`measure`]), temporary dimensions with value entry.
//! One action is one undo step: a grip uses `run_op`, a compound move uses `apply_ext`.

pub mod create;
pub mod del3d;
pub mod detail3d;
pub mod exact;
pub mod gizmo;
pub mod grips;
pub mod math;
pub mod measure;
pub mod measure_saved;
pub mod mep_grips;
pub mod mesh_inst;
pub mod mirror_array;
pub mod paint;
pub mod pick;
pub mod plane;
pub mod plan_route;
pub mod ramp3d;
mod room3d;
pub mod scene3d;
pub mod snap;
pub mod struct3d;
pub mod wp_win;
mod zone_shaft3d;
#[cfg(test)]
mod tests;
pub mod xform;
#[cfg(test)]
mod xform_fuzz;

use std::sync::{Arc, OnceLock};

use ddc_render::glam::Vec3;
use ddc_render::{Aabb, Ray, Scene};
use ddc_ui::i18n::{Catalog, Lang};
use egui::{self, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};

use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::viewport::Viewport;
use grips::{Axis3, Grip3};
use math::{ray_line, ray_plane, round_to, Cam};
use pick::{Bodies, Cat, Src, Target};
use plane::WorkPlane;
use snap::{Snap, SnapKind};
use xform::Xform;

static CATALOG: OnceLock<Catalog> = OnceLock::new();

/// Label by key in the UI language.
pub fn tr(ru: bool, key: &str) -> String {
    let lang = if ru { Lang::Ru } else { Lang::En };
    CATALOG
        .get_or_init(|| Catalog::new(Lang::En))
        .t_in(lang, key)
}

/// Error text for the status line: `e3d.err.*` keys are translated, anything else is shown as is.
pub fn err_text(ru: bool, e: &ddc_model::Error) -> String {
    match e {
        ddc_model::Error::Invalid(k) if k.starts_with("e3d.err.") || k.starts_with("mep.err.") => tr(ru, k),
        e => e.to_string(),
    }
}

/// The view's section box (scene): selection, hover, snaps and the selection box ignore clipped geometry.
pub fn section_box(view: &Viewport) -> Option<Aabb> {
    view.section_box()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    /// Box: left to right is a window, right to left is a crossing.
    Box,
    Measure,
    Create(create::Kind),
    /// Work plane from the face under the cursor.
    Face,
    /// Work plane from three points.
    Pick3,
    Mirror,
    Array,
}

impl Tool {
    fn key(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Box => "box",
            Tool::Measure => "measure",
            Tool::Create(create::Kind::Wall) => "wall",
            Tool::Create(create::Kind::Floor) => "floor",
            Tool::Create(create::Kind::Column) => "column",
            Tool::Create(create::Kind::Beam) => "beam",
            Tool::Face => "face",
            Tool::Pick3 => "pick",
            Tool::Mirror => "mirror",
            Tool::Array => "array",
        }
    }

    fn snaps(self) -> bool {
        !matches!(self, Tool::Select | Tool::Box | Tool::Face)
    }

    /// Command the ribbon lights for the tool (resolved id: `mod.measure` aliases
    /// `e3d.measure`; mirror and array share the plan commands, see `mirror_array::route`).
    pub fn command(self) -> Option<&'static str> {
        Some(match self {
            Tool::Select => return None,
            Tool::Box => "e3d.box",
            Tool::Measure => "e3d.measure",
            Tool::Create(create::Kind::Wall) => "e3d.wall",
            Tool::Create(create::Kind::Floor) => "e3d.floor",
            Tool::Create(create::Kind::Column) => "e3d.column",
            Tool::Create(create::Kind::Beam) => "e3d.beam",
            Tool::Face => "e3d.wp_face",
            Tool::Pick3 => "e3d.wp_pick",
            Tool::Mirror => "mod.mirror",
            Tool::Array => "mod.array",
        })
    }
}

/// Keys taken from the shortcuts while 3D is busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key3 {
    Esc,
    Tab,
    Enter,
    Back,
    Char(char),
}

/// Action after drag handling (once the borrow of `drag` has ended).
enum After {
    None,
    Grip,
    Gizmo,
    Drop,
    Click,
    Box(Rect, bool),
}

#[derive(Clone, Debug)]
enum Drag3 {
    Grip { g: Grip3, to: Vec3, ok: bool },
    Gizmo { d: gizmo::Drag, x: Option<Xform> },
    Box { from: Pos2 },
}

#[derive(Default)]
pub struct Edit3d {
    pub tool: Tool,
    /// Show the gizmo at the selection.
    pub gizmo: bool,
    /// Snaps enabled (otherwise only the plane point).
    pub snaps_off: bool,
    pub mask: snap::Mask,
    /// Family profile of the active tool; narrows `mask` (one source with the plan).
    pub profile: crate::snap::SnapProfile,
    /// Selection in 3D: targets of the editor's shared selection and targets without `Sel` (roofs, columns, ...).
    pub sel: Vec<Target>,
    mirror: Vec<Sel>,
    pub hover: Option<Target>,
    /// Scene point under the cursor (body hit, else work plane); the 3D paste anchor.
    pub cursor_pt: Option<Vec3>,
    bodies: Option<Bodies>,
    grids: Vec<((f64, f64), (f64, f64))>,
    /// Exact curves and surfaces behind bodies (rebuilt with the bodies).
    shapes: Vec<exact::Shape3>,
    pub plane: WorkPlane,
    plane_user: bool,
    cycle: usize,
    cycle_at: Option<Pos2>,
    snap: Option<Snap>,
    drag: Option<Drag3>,
    typing: Option<String>,
    /// Value entry for a grip's temporary dimension (its role: the number changes after undo).
    edit_dim: Option<grips::Role3>,
    create: Option<create::Create>,
    measure: measure::Measure,
    ms: measure_saved::State,
    pick3: Vec<Vec3>,
    keys: Vec<Key3>,
    /// Scope last handed to the editor's interaction (`sync_scope`).
    ix_last: crate::interact::Scope,
    pub ru: bool,
    grip_cache: Option<(u64, Target, Vec<Grip3>)>,
    /// Targets of `scene3d` parts in the view scene (element `EXT_BASE + k`).
    ext: Vec<Target>,
    /// Scene part builds kept between frames (`scene3d::compose_with`).
    pub cache: scene3d::Cache,
    /// Scene build number: pick bodies are rebuilt for each one.
    gen: u64,
    /// The 3D view rectangle from the last frame (Tab is captured only over it).
    rect: Option<Rect>,
    /// Active level elevation the user's work plane is tied to.
    plane_elev: f64,
    /// Mirror and Array tools.
    ma: mirror_array::State,
    /// Cursor of the last frame in model coordinates (`interact::View3dAdapter`).
    pub pointer: Option<crate::interact::Pointer>,
    /// Plan tool armed for 3D creation on the level plane (`plan_route`), and the screen
    /// position of the last move sent to it.
    plan_tool: Option<create::Kind>,
    plan_last: Option<[f32; 2]>,
}

impl Edit3d {
    /// Scene target map from `scene3d::compose` (called when the scene is rebuilt).
    pub fn set_ext(&mut self, ext: Vec<Target>) {
        self.ext = ext;
        self.bodies = None;
        self.gen += 1;
    }

    /// Target of a provider part of the scene (None: not a provider part or no target).
    pub fn ext_target(&self, element: u32) -> Option<Target> {
        scene3d::target_of(element, &self.ext).flatten()
    }

    /// A tool or a drag owns the input (Esc, Tab, Enter, digits).
    pub fn busy(&self) -> bool {
        self.drag.is_some()
            || self.tool != Tool::Select
            || self.typing.is_some()
            || !self.measure.pts.is_empty()
    }

    fn accepts_text(&self) -> bool {
        matches!(self.drag, Some(Drag3::Grip { .. } | Drag3::Gizmo { .. }))
            || self.edit_dim.is_some()
            || self.tool == Tool::Array
            || self.create.as_ref().is_some_and(|c| !c.pts.is_empty())
    }

    /// The user's work plane in model coordinates; `None` while it is the active level.
    pub fn world_plane(&self) -> Option<plane::World> {
        self.plane_user.then(|| self.plane.to_world(self.plane_elev))
    }

    /// Sets the user's work plane from model coordinates; scene z counts from `elev`.
    pub fn set_world_plane(&mut self, w: &plane::World, source: plane::Source, elev: f64) {
        self.plane = w.to_scene(elev, source);
        self.plane_user = true;
        self.plane_elev = elev;
    }

    /// Snap profile of the active 3D tool: a column offers the column family's kinds.
    pub fn tool_profile(&self) -> crate::snap::SnapProfile {
        match self.tool {
            Tool::Create(create::Kind::Column) => {
                crate::elem::by_table("col").map_or(crate::snap::SnapProfile::ALL, |f| f.snap)
            }
            _ => crate::snap::SnapProfile::ALL,
        }
    }

    pub fn set_tool(&mut self, t: Tool) {
        self.tool = t;
        self.drag = None;
        self.typing = None;
        self.edit_dim = None;
        self.measure.clear();
        self.ms.picks.clear();
        self.pick3.clear();
        self.ma.clear();
        self.create = match t {
            Tool::Create(k) => Some(create::Create::new(k)),
            _ => None,
        };
    }

    /// The common scope the 3D tool and drag mean now.
    pub fn scope(&self) -> crate::interact::Scope {
        use crate::interact::{ModOp, Scope, Target as IxTarget, ToolId};
        match (&self.drag, self.tool) {
            (Some(Drag3::Grip { g, .. }), _) => match g.target.grip_kind() {
                // (3D grip roles are not numbered as plan roles yet: grip 0)
                Some(k) => Scope::GripDrag { target: IxTarget::Elem(k, g.target.id), grip: 0 },
                None => Scope::Moving { op: ModOp::Move },
            },
            (Some(Drag3::Gizmo { .. }), _) => Scope::Moving { op: ModOp::Move },
            (Some(Drag3::Box { .. }), _) | (None, Tool::Box) => Scope::BoxSelect,
            (None, Tool::Measure) => Scope::Measuring,
            (None, Tool::Create(_)) => Scope::Drafting {
                tool: ToolId(self.tool.command().unwrap_or("e3d")),
                step: if self.create.as_ref().is_none_or(|c| c.pts.is_empty()) { "first" } else { "next" },
            },
            (None, Tool::Face | Tool::Pick3) => Scope::Pointing,
            (None, Tool::Mirror) => Scope::Moving { op: ModOp::Mirror },
            (None, Tool::Array) => Scope::Moving { op: ModOp::Array },
            (None, Tool::Select) if !self.measure.pts.is_empty() => Scope::Measuring,
            (None, Tool::Select) => Scope::Idle,
        }
    }

    /// Bodies snapping leaves out while a drag runs: the scope's row of `SCOPE_TABLE` says
    /// which (the selection under a gizmo move, the element under a grip). Tools keep all.
    fn snap_exclude(&self, bodies: &Bodies) -> Vec<usize> {
        use crate::interact::hotset::SnapSkip;
        let dragged = match &self.drag {
            Some(Drag3::Grip { g, .. }) => Some(g.target),
            Some(_) => None,
            None => return vec![],
        };
        match (SnapSkip::of(&self.scope()), dragged) {
            (SnapSkip::None, _) => vec![],
            // the gizmo moves the selection; a grip without a plan kind moves its one element
            (SnapSkip::Selection, None) => self.sel.iter().filter_map(|t| bodies.of(*t)).collect(),
            (SnapSkip::Selection | SnapSkip::Target, Some(t)) => bodies.of(t).into_iter().collect(),
            (SnapSkip::Target, None) => vec![],
        }
    }

    /// Hand the scope to the editor; a session ended from outside (second Esc, another
    /// session) drops the 3D tool here.
    pub fn sync_scope(&mut self, ed: &mut Editor) {
        let want = self.scope();
        if !crate::interact::sessions::view3d::sync(ed, self.ix_last, want) {
            self.set_tool(Tool::Select);
            self.ix_last = crate::interact::Scope::Idle;
            return;
        }
        self.ix_last = want;
    }

    fn t(&self, key: &str) -> String {
        tr(self.ru, key)
    }

    fn set_sel(&mut self, ed: &mut Editor, ts: Vec<Target>) {
        ed.selection = ts.iter().filter_map(|t| t.sel()).collect();
        ed.grip.focus = ts
            .iter()
            // bodies (`Sel::Body`) are a plan selection, but plan grips still reach them through focus
            .filter(|t| t.sel().is_none_or(|s| matches!(s, Sel::Body(..))))
            .filter_map(|t| t.grip_kind().map(|k| (k, t.id)))
            .collect();
        self.mirror = ed.selection.clone();
        self.sel = ts;
        self.grip_cache = None;
        self.edit_dim = None;
    }

    fn grips(&mut self, ed: &Editor) -> Vec<Grip3> {
        let [t] = self.sel.as_slice() else {
            return vec![];
        };
        let t = *t;
        if let Some((rev, ct, g)) = &self.grip_cache {
            if *rev == ed.revision && *ct == t {
                return g.clone();
            }
        }
        let g = grips::grips_of(&ed.model, ed.elevation, t);
        self.grip_cache = Some((ed.revision, t, g.clone()));
        g
    }
}

/// Key capture before the shell shortcuts (called from `EditorApp::ui` before `shortcuts`).
pub fn pre_input(a: &mut EditorApp, ctx: &egui::Context) {
    let e = &mut a.view.e3;
    e.ru = a.sh.cat.lang == Lang::Ru;
    if !a.view3d || ctx.wants_keyboard_input() || !crate::ui_kit::canvas_owns_keys(ctx) {
        return;
    }
    let live = plan_route::live(&a.ed);
    let busy = e.busy() || live;
    let text = e.accepts_text() || live;
    let over = ctx
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|p| e.rect.is_some_and(|r| r.contains(p)));
    // Tab over the 3D view never reaches egui focus traversal: with a single candidate it
    // would jump to the ribbon and the next Tab would cycle tabs instead of elements
    let tab = busy || over;
    let mut keys = vec![];
    let mut clear_sel = false;
    ctx.input_mut(|i| {
        i.events.retain(|ev| match ev {
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } => {
                use egui::Key as K;
                let k = match key {
                    K::Escape => Some(Key3::Esc),
                    K::Tab => Some(Key3::Tab),
                    K::Enter => Some(Key3::Enter),
                    K::Backspace => Some(Key3::Back),
                    _ => None,
                };
                match k {
                    Some(Key3::Esc) if !busy => {
                        clear_sel |= *pressed;
                        true
                    }
                    Some(Key3::Tab) if !tab => true,
                    Some(k) if busy || (k == Key3::Tab) => {
                        if *pressed {
                            keys.push(k);
                        }
                        false
                    }
                    _ => {
                        let num = matches!(
                            key,
                            K::Num0
                                | K::Num1
                                | K::Num2
                                | K::Num3
                                | K::Num4
                                | K::Num5
                                | K::Num6
                                | K::Num7
                                | K::Num8
                                | K::Num9
                                | K::Period
                                | K::Comma
                                | K::Minus
                        );
                        !(text && num && !modifiers.command && !modifiers.alt)
                    }
                }
            }
            egui::Event::Text(t) if text => {
                for c in t.chars() {
                    if c.is_ascii_digit() || ".,-+/'\" ;".contains(c) {
                        keys.push(Key3::Char(c));
                    }
                }
                false
            }
            _ => true,
        })
    });
    if clear_sel {
        e.set_sel(&mut a.ed, vec![]);
    }
    e.keys.extend(keys);
}

/// 3D view with editing: replaces `view.show` + `select_element`.
pub fn show(ui: &mut egui::Ui, view: &mut Viewport, scene: &Scene, ed: &mut Editor) {
    let mut e = std::mem::take(&mut view.e3);
    let rect = ui.available_rect_before_wrap();
    e.frame(ui, view, scene, ed, rect);
    e.sync_scope(ed);
    view.e3 = e;
}

/// The 3D view as a view adapter: camera and rectangle of the frame, scene origin at `z0`
/// (elevation of the active level), work plane `plane` of the scene.
pub fn adapter(
    cam: &ddc_render::OrbitCamera,
    rect: Rect,
    z0: f64,
    plane: &WorkPlane,
) -> crate::interact::View3dAdapter {
    crate::interact::View3dAdapter::new(
        cam,
        [rect.min.x as f64, rect.min.y as f64],
        [rect.width() as f64, rect.height() as f64],
        z0,
        plane.to_model(z0),
    )
}

fn parse_value(s: &str, angle: bool, ed: &Editor) -> Option<f32> {
    if angle {
        crate::units::parse_locale_number(s.trim(), crate::units::ui_decimal_mark())
            .ok()
            .map(|v| v as f32)
    } else {
        crate::units::parse_edit_length(s, ed.tool.units).map(|v| v as f32)
    }
}

/// The click target while the scene index is still building: exact hits, the same filter
/// and Tab position as hover.
fn exact_target(
    ed: &Editor,
    bodies: &Bodies,
    scene: &Scene,
    ray: Option<&Ray>,
    q: &pick::Query,
    cycle: usize,
) -> Option<Target> {
    let hits = ray.map_or_else(Vec::new, |r| bodies.hits_in(scene, r, q));
    crate::pick_rules::choose(&crate::pick_rules::filter3(ed, hits), cycle).map(|h| h.target)
}

fn fmt_len(v: f32, ed: &Editor) -> String {
    crate::units::format_length(v as f64, ed.tool.units)
}

fn fmt_value(v: f32, angle: bool, ed: &Editor) -> String {
    if angle {
        format!("{v:.1}°")
    } else {
        fmt_len(v, ed)
    }
}

impl Edit3d {
    fn frame(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut Viewport,
        scene: &Scene,
        ed: &mut Editor,
        rect: Rect,
    ) {
        // a selection changed outside 3D (plan, browser) replaces our own
        if ed.selection != self.mirror {
            self.sel = ed
                .selection
                .iter()
                .filter_map(|s| Target::from_sel(*s))
                .collect();
            self.mirror = ed.selection.clone();
            self.grip_cache = None;
        }
        let key = pick::key(ed, scene, self.gen);
        if self.bodies.as_ref().and_then(|b| b.key) != Some(key) {
            // unchanged meshes and placements keep their bodies: an edit rebuilds only its own
            let mut b = Bodies::update(self.bodies.take(), ed, scene, &self.ext);
            b.key = Some(key);
            self.bodies = Some(b);
            self.grids = ed
                .model
                .grids()
                .unwrap_or_default()
                .into_iter()
                .filter(|g| g.spec.bulge.abs() < 1e-9)
                .map(|g| (g.spec.a, g.spec.b))
                .collect();
            self.shapes = exact::collect(ed);
            if let Some(b) = self.bodies.as_mut() {
                b.add_model_grids(ed);
            }
            self.sel.retain(|t| {
                t.sel().is_some() || self.bodies.as_ref().is_some_and(|b| b.of(*t).is_some())
            });
        }
        if !self.plane_user {
            self.plane = WorkPlane::level(ed.level, 0.0);
        } else if self.plane_elev != ed.elevation {
            // the plane is in the scene frame (z from the active level): a level change does not move it in the world
            self.plane.origin.z += (self.plane_elev - ed.elevation) as f32;
        }
        self.plane_elev = ed.elevation;
        self.rect = Some(rect);
        view.capture =
            self.tool == Tool::Box || self.drag.is_some() || crate::mass_tools::capturing(ed);
        let clicked = view.show(ui, scene);
        // push-pull and a click on a shape face come before 3D selection
        let clicked = crate::mass_tools::after_show_3d(ed, view, clicked);
        crate::mass_tools::overlay_3d(ui, ed, view);
        let mut bodies = self.bodies.take().unwrap_or_default();
        self.frame_inner(ui, view, scene, ed, rect, clicked, &mut bodies);
        self.bodies = Some(bodies);
    }

    /// A library card released over the view: the instance at the point under the pointer, on
    /// the wall, floor or ceiling hit there (else by the family's hosting in plan).
    fn drop_component(&mut self, ui: &egui::Ui, ed: &mut Editor, pointer: Option<Pos2>, on: Option<Target>) {
        use crate::tools::component::{place, HostHint, Pick};
        let ctx = ui.ctx();
        if pointer.is_none() || !egui::DragAndDrop::has_payload_of_type::<Pick>(ctx) {
            return;
        }
        ctx.set_cursor_icon(egui::CursorIcon::Copy);
        if !ui.input(|i| i.pointer.any_released()) {
            return;
        }
        let (Some(p), Some(at)) = (egui::DragAndDrop::take_payload::<Pick>(ctx), self.cursor_pt) else {
            return;
        };
        let hint = match on {
            Some(Target { cat: Cat::Wall, id, .. }) => HostHint::Wall(id),
            Some(Target { cat: Cat::Slab, id, .. }) => HostHint::Slab(id),
            Some(Target { cat: Cat::Ceiling, id, .. }) => HostHint::Ceiling(id),
            _ => HostHint::Auto,
        };
        let q = ddc_geom::v2(at.x as f64, at.y as f64);
        if let Err(e) = place(ed, &p, q, hint) {
            ed.status = Some(e);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn frame_inner(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut Viewport,
        scene: &Scene,
        ed: &mut Editor,
        rect: Rect,
        clicked: Option<Option<u32>>,
        bodies: &mut Bodies,
    ) {
        let cam = Cam::new(&view.camera, rect);
        let painter = ui.painter_at(rect);
        let ov = crate::overlay::pal(view.dark);
        let accent = ov.selection;
        let (fg, bg) = (ov.ink, ov.plate);
        let mods: Modifiers = ui.input(|i| i.modifiers);
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| rect.contains(*p) && ui.rect_contains_pointer(rect));
        let z0 = ed.elevation;
        let ad = adapter(&view.camera, rect, z0, &plan_route::plane_for(ed, self.plane));
        self.pointer = pointer.map(|p| crate::interact::ViewAdapter::pointer(&ad, [p.x, p.y]));

        // Tab cycles candidates in place; moving the cursor resets that first
        if let (Some(p), Some(q)) = (pointer, self.cycle_at) {
            if (p - q).length() > 4.0 {
                self.cycle = 0;
                self.cycle_at = None;
            }
        }
        // the window lost focus: no button release will arrive, so the drag is cancelled
        if self.drag.is_some() && !ui.input(|i| i.focused) {
            self.drag = None;
            self.typing = None;
        }
        let keys = std::mem::take(&mut self.keys);
        let mut enter = false;
        // creation on the level plane: the plan tool owns the keys
        let route = match self.tool {
            Tool::Create(k) if plan_route::on_level(&self.plane) => Some(k),
            _ => None,
        };
        if self.plan_tool.is_some() && self.plan_tool != route {
            plan_route::disarm(ed);
            self.plan_tool = None;
            self.plan_last = None;
        }
        if let Some(k) = route.filter(|k| self.plan_tool != Some(*k)) {
            if crate::interact::script::arm(ed, plan_route::hotkey(k)) {
                self.plan_tool = Some(k);
            }
        }
        // any plan tool armed from the ribbon (door, room, stair, a modify session…) reads the
        // same pointer and keys as in the plan (`plan_route::live`)
        let hosted = plan_route::live(ed) && matches!(self.tool, Tool::Select);
        for k in keys {
            if hosted {
                plan_route::key(ed, k);
                continue;
            }
            if self.plan_tool.is_some() {
                if plan_route::key(ed, k) {
                    self.plan_tool = None;
                    self.plan_last = None;
                    self.set_tool(Tool::Select);
                }
                continue;
            }
            match k {
                Key3::Esc => self.escape(),
                Key3::Tab => {
                    self.cycle += 1;
                    self.cycle_at = pointer;
                }
                Key3::Enter => enter = true,
                Key3::Back => {
                    if let Some(t) = &mut self.typing {
                        t.pop();
                    }
                }
                Key3::Char(c) => self.typing.get_or_insert_with(String::new).push(c),
            }
        }

        let ray = pointer.map(|p| cam.ray(p));
        let index = view.index.clone();
        let q = pick::Query {
            index: index.as_deref(),
            clip: section_box(view),
            full: false,
        };
        // the large-scene index is being built: click and box compute exactly; a one-off delay beats a miss
        let pending = crate::spatial::index_pending(index.as_deref(), scene);
        let exact = pick::Query { full: true, ..q };
        // one ray hit per frame, shared by hover and snaps
        let ray_hits = match pointer {
            Some(p) => bodies.hits_px(scene, &cam, p, &q, pick::GRID_PX),
            None => vec![],
        };
        // hover, Tab and click share one list: the status bar filter and pins apply to all
        let hits = match &self.drag {
            None | Some(Drag3::Box { .. }) => crate::pick_rules::filter3(ed, ray_hits.clone()),
            _ => vec![],
        };
        let hit = crate::pick_rules::choose(&hits, self.cycle);
        self.hover = hit.as_ref().map(|h| h.target);
        self.cursor_pt = hit
            .map(|h| h.point)
            .or_else(|| ray.as_ref().and_then(|r| self.plane.hit(r)));
        self.drop_component(ui, ed, pointer, hit.as_ref().map(|h| h.target));
        view.hovered = hit.as_ref().and_then(|h| match &bodies.list[h.body].src {
            Src::Scene(ms) => ms.first().map(|&i| scene.meshes[i].element),
            Src::Inst { g, k } => scene
                .instanced
                .get(*g)
                .and_then(|grp| grp.inst.get(*k))
                .map(|i| i.element),
            Src::Own { .. } => None,
        });

        // snap for tools and drags
        let exclude = self.snap_exclude(bodies);
        let want_snap = self.tool.snaps()
            || matches!(self.drag, Some(Drag3::Grip { .. } | Drag3::Gizmo { .. }));
        let step_at = |p: Vec3| math::nice_step(cam.m_per_px(p) * 8.0);
        self.snap = None;
        let mut snap_list: Vec<Snap> = vec![];
        if let (true, Some(p)) = (want_snap, pointer) {
            let wp = self.plane;
            let step = wp.hit(&cam.ray(p)).map_or(0.1, step_at);
            let mask = if self.snaps_off {
                snap::Mask(SnapKind::Plane.bit())
            } else {
                self.profile.mask3(self.mask)
            };
            let inp = snap::Input {
                cam: &cam,
                pos: p,
                plane: Some(&wp),
                step,
                radius: snap::RADIUS,
                mask,
                grids: &self.grids,
                exclude: &exclude,
                hits: Some(&ray_hits),
                query: q,
                shapes: &self.shapes,
                from: match &self.drag {
                    Some(Drag3::Grip { g, .. }) => Some(g.pos),
                    _ if self.tool == Tool::Measure => self.measure.pts.last().copied(),
                    _ => None,
                },
            };
            snap_list = snap::candidates(bodies, scene, &inp);
            // in tools without a drag, Tab cycles snaps
            self.snap = snap::pick(
                &snap_list,
                if self.drag.is_some() || self.tool.snaps() {
                    self.cycle
                } else {
                    0
                },
            );
        }

        // ---- selection, outlines ----
        let sel_bodies: Vec<usize> = self.sel.iter().filter_map(|t| bodies.of(*t)).collect();
        let moving = match &self.drag {
            Some(Drag3::Gizmo { x: Some(x), .. }) => Some(*x),
            _ => None,
        };
        for &i in &sel_bodies {
            let f = &bodies.feature(i, scene).edges;
            let s = Stroke::new(crate::overlay::OUTLINE_PX, accent);
            match moving {
                Some(x) => {
                    paint::edges(
                        &painter,
                        &cam,
                        f,
                        Stroke::new(1.0, accent.gamma_multiply(0.35)),
                        |p| p,
                    );
                    paint::edges(&painter, &cam, f, s, |p| x.point(p));
                }
                None => paint::edges(&painter, &cam, f, s, |p| p),
            }
        }
        if let Some(h) = &hit {
            if self.tool == Tool::Select || self.tool == Tool::Box || self.tool == Tool::Face {
                if !sel_bodies.contains(&h.body) {
                    let f = &bodies.feature(h.body, scene).edges;
                    paint::edges(
                        &painter,
                        &cam,
                        f,
                        Stroke::new(crate::overlay::OUTLINE_PX, ov.hover),
                        |p| p,
                    );
                }
                if let Some(p) = pointer {
                    let name = self.t(&format!("e3d.cat.{}", h.target.cat.key()));
                    let txt = if hits.len() > 1 {
                        format!(
                            "{name} · {}/{} (Tab)",
                            self.cycle % hits.len() + 1,
                            hits.len()
                        )
                    } else {
                        name
                    };
                    painter.text(
                        p + Vec2::new(14.0, 14.0),
                        egui::Align2::LEFT_TOP,
                        txt,
                        egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
                        fg,
                    );
                }
            }
        }

        // ---- grips ----
        let mut over_handle = false;
        let gs = if self.tool == Tool::Select {
            self.grips(ed)
        } else {
            vec![]
        };
        let grip_drag = matches!(self.drag, Some(Drag3::Grip { .. }));
        for (k, g) in gs.iter().enumerate() {
            let Some(at) = cam.to_screen(g.pos) else {
                continue;
            };
            let resp = ui
                .interact(
                    Rect::from_center_size(at, Vec2::splat(14.0)),
                    ui.id().with(("e3d_grip", k)),
                    Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::Grab);
            over_handle |= resp.hovered() || resp.dragged();
            if resp.drag_started() && self.drag.is_none() {
                // Alt / Shift held at the press pick the provider's other drag (e.g. weight)
                let g = grips::variant(&ed.model, ed.elevation, g, mods.alt, mods.shift);
                self.drag = Some(Drag3::Grip {
                    to: g.pos,
                    g,
                    ok: true,
                });
                self.typing = None;
                self.edit_dim = None;
            }
            let glyph = match g.axis {
                Axis3::Line(_) => paint::Glyph::Arrow,
                Axis3::Ring { .. } => paint::Glyph::Circle,
                Axis3::Plane(_) => paint::Glyph::Square,
            };
            let dir = match g.axis {
                Axis3::Line(d) => cam
                    .to_screen(g.pos + d * cam.m_per_px(g.pos) * 20.0)
                    .map_or(Vec2::ZERO, |b| b - at),
                _ => Vec2::ZERO,
            };
            if !grip_drag {
                paint::grip(&painter, at, glyph, dir, resp.hovered(), accent, ov);
            }
            if resp.drag_stopped() {
                self.finish_grip(ed);
            }
        }
        // the grip went beyond the view edge or behind the camera: it has no widget, so the release is handled here
        if matches!(self.drag, Some(Drag3::Grip { .. })) && !ui.input(|i| i.pointer.primary_down())
        {
            self.finish_grip(ed);
        }
        // temporary dimensions of the selection: a click enters a value
        if self.drag.is_none() {
            for (k, g) in gs.iter().enumerate() {
                if matches!(g.axis, Axis3::Plane(_)) || matches!(g.role, grips::Role3::Plan(_)) {
                    continue;
                }
                let v = g.value(g.pos);
                let angle = matches!(g.axis, Axis3::Ring { .. });
                let txt = if self.edit_dim.as_ref() == Some(&g.role) {
                    format!("{}▏", self.typing.clone().unwrap_or_default())
                } else {
                    fmt_value(v, angle, ed)
                };
                let r = if angle {
                    cam.to_screen(g.pos).map(|p| {
                        paint::label(
                            &painter,
                            p + Vec2::new(0.0, -16.0),
                            &txt,
                            fg,
                            bg,
                            Some(accent),
                        )
                    })
                } else {
                    paint::dim(&painter, &cam, g.anchor, g.pos, &txt, fg, bg)
                };
                if let Some(r) = r {
                    let resp = ui.interact(r, ui.id().with(("e3d_dim", k)), Sense::click());
                    over_handle |= resp.hovered();
                    if resp.clicked() {
                        self.edit_dim = Some(g.role.clone());
                        self.typing = Some(String::new());
                    }
                }
            }
            if let (Some(role), true) = (self.edit_dim.clone(), enter) {
                if let (Some(g), Some(s)) = (
                    gs.iter().find(|g| g.role == role).cloned(),
                    self.typing.take(),
                ) {
                    let angle = matches!(g.axis, Axis3::Ring { .. });
                    if let Some(v) = parse_value(&s, angle, ed) {
                        let to = g.typed(v, g.pos);
                        self.commit_grip(ed, g, to);
                    }
                }
                self.edit_dim = None;
                enter = false;
            }
        }

        // ---- gizmo ----
        let origin = self.gizmo_origin(bodies);
        // tilt (X/Y rings) exists only for beams and columns
        let rings = if !self.sel.is_empty()
            && self
                .sel
                .iter()
                .all(|t| matches!(t.cat, Cat::Beam | Cat::Column))
        {
            gizmo::RINGS_ALL
        } else {
            gizmo::RINGS_Z
        };
        if let (true, Some(o), Tool::Select) = (self.gizmo, origin, self.tool) {
            let shape = gizmo::Shape::new(
                &cam,
                match &self.drag {
                    Some(Drag3::Gizmo { d, .. }) => d.origin,
                    _ => o,
                },
            );
            let hot = match &self.drag {
                Some(Drag3::Gizmo { d, .. }) => Some(d.handle),
                Some(_) => None,
                None => pointer.and_then(|p| shape.hit(&cam, p, rings)),
            };
            let active = matches!(self.drag, Some(Drag3::Gizmo { .. }));
            // an ongoing drag keeps the widget even outside the view
            let at = if active {
                pointer.or(ui.input(|i| i.pointer.interact_pos()))
            } else {
                pointer
            };
            if let (Some(p), true) = (at, hot.is_some() || active) {
                let resp = ui.interact(
                    Rect::from_center_size(p, Vec2::splat(16.0)),
                    ui.id().with("e3d_gizmo"),
                    Sense::drag(),
                );
                over_handle = true;
                if resp.drag_started() && self.drag.is_none() {
                    let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
                    if let Some(h) = shape.hit(&cam, press, rings) {
                        if let Some(d) = gizmo::Drag::begin(h, o, &cam.ray(press)) {
                            self.drag = Some(Drag3::Gizmo { d, x: None });
                            self.typing = None;
                        }
                    }
                }
                if resp.drag_stopped() {
                    self.finish_gizmo(ed, mods.ctrl || mods.alt);
                }
            } else if active && !ui.input(|i| i.pointer.primary_down()) {
                // the cursor left the view: a release there also ends the drag
                self.finish_gizmo(ed, mods.ctrl || mods.alt);
            }
            paint::gizmo(&painter, &cam, &shape, hot, rings, ov);
        }

        // ---- drag progress ----
        let snap_p = self.snap.map(|s| s.p);
        let mut after = After::None;
        match &mut self.drag {
            Some(Drag3::Grip { g, to, ok }) => {
                if let Some(r) = &ray {
                    let step = step_at(g.pos);
                    let typed = self
                        .typing
                        .as_deref()
                        .and_then(|s| parse_value(s, matches!(g.axis, Axis3::Ring { .. }), ed));
                    let p = match typed {
                        Some(v) => Some(g.typed(v, *to)),
                        None => grip_to(g, r, self.snap.as_ref(), mods.ctrl, step, mods.shift),
                    };
                    if let Some(p) = p {
                        *to = p;
                    }
                }
                let pv = grips::preview(&ed.model, z0, g, *to);
                *ok = pv.is_ok();
                let col = if *ok { ov.preview } else { ov.error };
                for l in pv.unwrap_or_default() {
                    paint::polyline(&painter, &cam, &l, Stroke::new(2.0, col));
                }
                if let Some(at) = cam.to_screen(*to) {
                    paint::grip(
                        &painter,
                        at,
                        paint::Glyph::Square,
                        Vec2::ZERO,
                        true,
                        col,
                        ov,
                    );
                    if !*ok {
                        paint::bad(&painter, at, ov);
                    }
                }
                let angle = matches!(g.axis, Axis3::Ring { .. });
                let v = g.value(*to);
                let txt = fmt_value(v, angle, ed);
                if angle {
                    if let Some(c) = cam.to_screen(g.anchor) {
                        paint::label(
                            &painter,
                            c + Vec2::new(0.0, -18.0),
                            &txt,
                            fg,
                            bg,
                            Some(accent),
                        );
                    }
                } else {
                    paint::dim(&painter, &cam, g.anchor, *to, &txt, fg, bg);
                }
                if enter {
                    enter = false;
                    after = After::Grip;
                }
            }
            Some(Drag3::Gizmo { d, x }) => {
                if let Some(r) = &ray {
                    let typed = self
                        .typing
                        .as_deref()
                        .and_then(|s| parse_value(s, d.is_rotate(), ed));
                    let step = step_at(d.origin);
                    let ang = if mods.shift { 15.0 } else { 5.0 };
                    let cur = d.xform(r, snap_p, step, ang);
                    *x = match (typed, cur) {
                        (Some(v), Some(c)) => Some(d.typed(&c, v)),
                        (Some(v), None) => Some(d.typed(&Xform::Move(Vec3::ZERO), v)),
                        (None, c) => c.or(*x),
                    };
                }
                if let Some(xf) = *x {
                    let txt = fmt_value(gizmo::Drag::value(&xf), d.is_rotate(), ed);
                    match xf {
                        Xform::Move(v) => {
                            paint::dim(&painter, &cam, d.origin, d.origin + v, &txt, fg, bg);
                        }
                        _ => {
                            if let Some(c) = cam.to_screen(d.origin) {
                                paint::label(
                                    &painter,
                                    c + Vec2::new(0.0, -22.0),
                                    &txt,
                                    fg,
                                    bg,
                                    Some(accent),
                                );
                            }
                        }
                    }
                }
                if enter {
                    enter = false;
                    after = After::Gizmo;
                }
            }
            Some(Drag3::Box { from }) => {
                let from = *from;
                if let Some(p) = pointer.or(ui.input(|i| i.pointer.interact_pos())) {
                    paint::rubber(&painter, from, p, ov);
                    if ui.input(|i| i.pointer.primary_released()) {
                        after = if (p - from).length() > 4.0 {
                            After::Box(Rect::from_two_pos(from, p), p.x < from.x)
                        } else {
                            After::Click
                        };
                    }
                } else if !ui.input(|i| i.pointer.primary_down()) {
                    after = After::Drop;
                }
            }
            None => {}
        }
        match after {
            After::None => {}
            After::Grip => self.finish_grip(ed),
            After::Gizmo => self.finish_gizmo(ed, mods.ctrl || mods.alt),
            After::Drop => self.drag = None,
            After::Click => {
                self.drag = None;
                let t = if pending {
                    exact_target(ed, bodies, scene, ray.as_ref(), &exact, self.cycle)
                } else {
                    hit.as_ref().map(|h| h.target)
                };
                self.click_select(ed, t, mods.ctrl);
            }
            After::Box(r, crossing) => {
                self.drag = None;
                let mut ts =
                    bodies.in_box_in(scene, &cam, r, crossing, if pending { &exact } else { &q });
                if mods.ctrl {
                    for t in std::mem::take(&mut self.sel) {
                        if !ts.contains(&t) {
                            ts.push(t);
                        }
                    }
                }
                self.set_sel(ed, ts);
            }
        }

        // selection box: a press on empty space in the view
        // except a press on the view cube or the section-box grips
        if self.tool == Tool::Box && self.drag.is_none() && !over_handle && !view.overlay_hover {
            if let Some(p) = pointer {
                if ui.input(|i| i.pointer.primary_pressed()) {
                    self.drag = Some(Drag3::Box { from: p });
                }
            }
        }

        // ---- tool clicks ----
        let click = clicked.is_some() && !over_handle;
        let pt = self.snap.map(|s| s.p);
        measure_saved::draw3d(&painter, &cam, ed, fg, bg, ov.warning);
        // an armed plan tool takes the pointer, not the pick
        // (tool-first picking stays the 3D pick: the level plane cannot reach other levels)
        let routed = plan_route::takes_pointer(ed);
        match self.tool {
            Tool::Select if click && !routed => {
                let t = if pending {
                    exact_target(ed, bodies, scene, ray.as_ref(), &exact, self.cycle)
                } else {
                    hit.as_ref().map(|h| h.target)
                };
                self.click_select(ed, t, mods.ctrl);
                let picked = ed.selection.clone();
                if let Some(m) = crate::interact::sessions::modify::tool_mut(ed).filter(|m| m.selecting) {
                    m.targets = picked;
                }
            }
            Tool::Measure => {
                let chain = self.ms.mode == measure_saved::Mode::Chain;
                // saved measurements of features; hold Q for the quick lens (writes nothing)
                let lens_on = ui.input(|i| i.key_down(eframe::egui::Key::Q));
                if !chain || lens_on {
                    let feat = hit
                        .as_ref()
                        .and_then(|h| measure_saved::feature_of_hit(&ed.model, h));
                    let mode = if chain { measure_saved::Mode::Clear } else { self.ms.mode };
                    if let (false, true, Some(f)) = (chain, click, &feat) {
                        let spec = self.ms.picks.click(
                            &ed.model,
                            mode,
                            ddc_model::ViewKey::ThreeD,
                            f.anchor.clone(),
                        );
                        if let Some(spec) = spec {
                            if let Err(e) = ed.apply_ext(crate::tools::Rec::new("measure_add", move |m| {
                                m.add_measure(&spec).map(|_| 1)
                            })) {
                                let m = self.err(&e);
                                ed.say(crate::status::Severity::Error, m);
                            }
                        }
                    }
                    if lens_on {
                        let lens = measure_saved::Lens::hover(
                            &ed.model,
                            mode,
                            ddc_model::ViewKey::ThreeD,
                            &self.ms.picks,
                            feat.as_ref(),
                        );
                        if let (measure_saved::Lens::Hover(v), Some(p), Some(k)) =
                            (lens, pointer, mode.kind())
                        {
                            // a lone face reads as its area
                            let k = if self.ms.picks.anchors.len() + 1 == mode.arity() { k } else { ddc_model::MeasureKind::Area };
                            let txt = crate::tools::measure::value_text(ed, k, v);
                            paint::label(&painter, p + Vec2::new(24.0, 24.0), &txt, fg, bg, Some(ov.snap));
                        }
                    }
                }
                if let (true, true, Some(p)) = (chain, click, pt) {
                    self.measure.click(p);
                }
                // right click starts a new chain
                if pointer.is_some() && ui.input(|i| i.pointer.secondary_clicked()) {
                    self.measure.clear();
                    self.ms.picks.clear();
                }
                let cur = if self.measure.pts.is_empty() {
                    None
                } else {
                    pt
                };
                for s in self.measure.segs(cur) {
                    paint::seg(&painter, &cam, s.a, s.b, Stroke::new(2.0, ov.snap));
                    let mut txt = format!(
                        "{}  Δx {} Δy {} Δz {}",
                        fmt_len(s.len, ed),
                        fmt_len(s.d.x, ed),
                        fmt_len(s.d.y, ed),
                        fmt_len(s.d.z, ed)
                    );
                    if s.slope.abs() > 0.05 {
                        txt.push_str(&format!("  {} {:.1}°", self.t("e3d.slope"), s.slope));
                    }
                    paint::dim(&painter, &cam, s.a, s.b, &txt, fg, bg);
                }
                // what is under the cursor, by the pick query: measuring never selects
                if let (true, Some(p), Some(s)) = (
                    self.measure.pts.is_empty(),
                    pointer,
                    self.hover.and_then(|t| t.sel()),
                ) {
                    let txt = crate::pick_rules::describe(ed, &crate::pick_rules::here_of(ed, s));
                    paint::label(&painter, p + Vec2::new(24.0, 24.0), &txt, fg, bg, None);
                }
                if self.measure.segs(cur).len() > 1 {
                    if let Some(p) = pointer {
                        let txt = format!(
                            "{} {}",
                            self.t("e3d.total"),
                            fmt_len(self.measure.total(cur), ed)
                        );
                        paint::label(
                            &painter,
                            p + Vec2::new(24.0, 24.0),
                            &txt,
                            fg,
                            bg,
                            Some(ov.snap),
                        );
                    }
                }
            }
            Tool::Select if routed => {
                if let Some(p) = self.pointer {
                    plan_route::pointer(ed, p, &mut self.plan_last, click);
                }
            }
            Tool::Create(_) if self.plan_tool.is_some() => {
                if let Some(p) = self.pointer {
                    plan_route::pointer(ed, p, &mut self.plan_last, click);
                }
            }
            Tool::Create(_) => {
                let wp = self.plane;
                let on_plane = pt.map(|p| wp.project(p));
                let typed = self
                    .typing
                    .as_deref()
                    .and_then(|s| parse_value(s, false, ed));
                let mut target = on_plane;
                if let (Some(c), Some(v)) = (&self.create, typed) {
                    if let (Some(&last), Some(cur)) = (c.pts.last(), on_plane) {
                        let dir = (cur - last).normalize_or(wp.u);
                        target = Some(last + dir * v);
                    }
                }
                if let Some(c) = &self.create {
                    if let Some(cur) = target {
                        let pv = c.preview(cur);
                        paint::polyline(&painter, &cam, &pv, Stroke::new(2.0, ov.preview));
                        if let (Some(&last), true) = (c.pts.last(), c.kind != create::Kind::Column)
                        {
                            let txt = self.typing.clone().map_or_else(
                                || fmt_len((cur - last).length(), ed),
                                |t| format!("{t}▏"),
                            );
                            paint::dim(&painter, &cam, last, cur, &txt, fg, bg);
                        }
                    }
                }
                let mut ctx = create::Ctx::of(ed);
                // outline closing is in screen pixels, at any scale
                if let Some(p) = target {
                    ctx.close_tol = cam.m_per_px(p) * 8.0;
                }
                let mut step = None;
                if let (true, Some(p)) = (click || (enter && typed.is_some()), target) {
                    step = self.create.as_mut().map(|c| c.click(&ctx, &wp, p));
                    self.typing = None;
                } else if enter {
                    step = self.create.as_mut().map(|c| c.finish(&ctx, &wp));
                }
                match step {
                    Some(Ok(create::Step::Done(n))) => {
                        if let Err(e) = create::commit(ed, n) {
                            ed.status = Some(self.err(&e));
                        }
                    }
                    Some(Err(e)) => ed.status = Some(self.err(&e)),
                    _ => {}
                }
            }
            Tool::Mirror | Tool::Array => {
                let fx = mirror_array::Fx {
                    ui: &mut *ui,
                    painter: &painter,
                    cam: &cam,
                    scene,
                    bodies: &mut *bodies,
                    ed: &mut *ed,
                    rect,
                    pointer,
                    pt,
                    hover: hit.as_ref().map(|h| h.target),
                    click,
                    enter,
                    alt: mods.alt,
                    shift: mods.shift,
                    ov,
                };
                self.ma_frame(fx);
            }
            Tool::Face if click => {
                if let Some(h) = &hit {
                    // on an exact surface: its tangent plane at the picked point
                    let tp = self
                        .shapes
                        .iter()
                        .find(|s| s.target == h.target)
                        .and_then(|s| s.tangent_plane(h.point, h.normal));
                    let (p, n) = tp.unwrap_or((h.point, h.normal));
                    self.plane = WorkPlane::face(p, n);
                    self.plane_user = true;
                    ed.status = Some(self.t("e3d.wp.set"));
                    self.set_tool(Tool::Select);
                }
            }
            Tool::Pick3 => {
                if let (true, Some(p)) = (click, pt) {
                    self.pick3.push(p);
                    if self.pick3.len() == 3 {
                        let [a, b, c] = [self.pick3[0], self.pick3[1], self.pick3[2]];
                        let n = (b - a).cross(c - a);
                        if n.length() > 1e-6 {
                            self.plane = WorkPlane::face(a, n.normalize());
                            self.plane_user = true;
                            ed.status = Some(self.t("e3d.wp.set"));
                        } else {
                            ed.status = Some(self.t("e3d.wp.collinear"));
                        }
                        self.set_tool(Tool::Select);
                    }
                }
                let mut l = self.pick3.clone();
                if let Some(p) = pt {
                    l.push(p);
                }
                paint::polyline(&painter, &cam, &l, Stroke::new(1.5, ov.preview));
            }
            _ => {}
        }

        // ---- work plane, snap, input, hint ----
        if matches!(
            self.tool,
            Tool::Create(_) | Tool::Measure | Tool::Pick3 | Tool::Mirror | Tool::Array
        ) {
            if let Some(p) = pointer.and_then(|p| self.plane.hit(&cam.ray(p))) {
                paint::plane_grid(&painter, &cam, &self.plane, p, step_at(p), fg);
            }
        }
        // tool preview of the editor (session, else the wall draft of the plan) as volumes
        let prims = ed.preview_prims();
        if !prims.0.is_empty() {
            let units = ed.tool.units;
            let ad = adapter(&view.camera, rect, z0, &plan_route::plane_for(ed, self.plane));
            let draws = crate::interact::preview::draws(&prims.0, &ad);
            crate::overlay::paint_preview(&painter, &draws, ov, &|v| {
                crate::units::display_length(v, units)
            });
        }
        if let (Some(s), Some(_)) = (self.snap, pointer) {
            if let Some(at) = cam.to_screen(s.p) {
                let n = snap_list.len();
                let mut label = self.t(&format!("e3d.snap.{}", s.kind.key()));
                if n > 1 {
                    label = format!("{label} · Tab {}/{n}", self.cycle % n + 1);
                }
                paint::snap(&painter, at, s.kind, &label, ov);
            }
        }
        if let (Some(t), Some(p), None) = (&self.typing, pointer, &self.edit_dim) {
            paint::input_box(&painter, p, t, ov);
        }
        let hint = self.t(&format!("e3d.hint.{}", self.tool.key()));
        if self.tool != Tool::Select || self.gizmo {
            paint::note(
                &painter,
                rect.left_top() + Vec2::new(12.0, 12.0),
                &hint,
                fg,
                bg,
            );
        }
        if self.drag.is_some() || self.tool != Tool::Select {
            ui.ctx().request_repaint();
        }
    }

    /// Status-line text for an edit error.
    fn err(&self, e: &ddc_model::Error) -> String {
        format!("{}: {}", self.t("e3d.err"), err_text(self.ru, e))
    }

    fn escape(&mut self) {
        // everything at once: a cancelled drag leaves no input field or key capture behind
        let drag = self.drag.take().is_some();
        let typing = self.typing.take().is_some();
        let dim = self.edit_dim.take().is_some();
        if drag || typing || dim {
            return;
        }
        if let Some(c) = &mut self.create {
            if !c.pts.is_empty() {
                c.pts.clear();
                return;
            }
        }
        if !self.measure.pts.is_empty() || !self.ms.picks.anchors.is_empty() {
            self.measure.clear();
            self.ms.picks.clear();
            return;
        }
        if !self.pick3.is_empty() {
            self.pick3.clear();
            return;
        }
        if self.ma.clear_points() {
            return;
        }
        self.set_tool(Tool::Select);
    }

    fn click_select(&mut self, ed: &mut Editor, t: Option<Target>, add: bool) {
        let mut ts = if add { self.sel.clone() } else { vec![] };
        if let Some(t) = t {
            if let Some(i) = ts.iter().position(|x| *x == t) {
                ts.remove(i);
            } else {
                ts.push(t);
            }
        }
        self.set_sel(ed, ts);
    }

    fn gizmo_origin(&self, bodies: &Bodies) -> Option<Vec3> {
        let mut b = ddc_render::Aabb::EMPTY;
        for t in &self.sel {
            if let Some(i) = bodies.of(*t) {
                b.union(&bodies.list[i].aabb);
            }
        }
        (!b.is_empty()).then(|| {
            Vec3::new(
                (b.min.x + b.max.x) * 0.5,
                (b.min.y + b.max.y) * 0.5,
                b.min.z,
            )
        })
    }

    fn finish_grip(&mut self, ed: &mut Editor) {
        if !matches!(self.drag, Some(Drag3::Grip { .. })) {
            return;
        }
        let Some(Drag3::Grip { g, to, ok }) = self.drag.take() else {
            return;
        };
        self.typing = None;
        if ok && (to - g.pos).length() > 1e-6 {
            self.commit_grip(ed, g, to);
        }
    }

    fn commit_grip(&mut self, ed: &mut Editor, g: Grip3, to: Vec3) {
        let z0 = ed.elevation;
        let r = ed.run_op(OpFn(Arc::new(move |m: &mut ddc_model::Model| {
            grips::apply(m, z0, &g, to)
        })));
        if let Err(e) = r {
            ed.status = Some(self.err(&e));
        }
        self.grip_cache = None;
    }

    fn finish_gizmo(&mut self, ed: &mut Editor, copy: bool) {
        if !matches!(self.drag, Some(Drag3::Gizmo { .. })) {
            return;
        }
        let Some(Drag3::Gizmo { x, .. }) = self.drag.take() else {
            return;
        };
        self.typing = None;
        let Some(x) = x.filter(|x| !x.is_identity()) else {
            return;
        };
        self.commit_xform(ed, x, copy);
    }

    /// One transform of the selection as one undo step; a refusal goes to the status line.
    pub(crate) fn commit_xform(&mut self, ed: &mut Editor, x: Xform, copy: bool) {
        let ts = self.sel.clone();
        let note = xform::partial_note(&ts, &x, copy);
        let z0 = ed.elevation;
        let label = if copy { "edit3d_copy" } else { "edit3d_xform" };
        let r = ed.apply_ext(crate::tools::Rec::new(label, move |m| {
            xform::apply(m, z0, &ts, &x, copy)
        }));
        match r {
            // nothing was done: no undo step and no message
            Err(ddc_model::Error::Invalid(k)) if k == "e3d.err.noop" => {}
            Err(e) => {
                let m = self.err(&e);
                ed.say(crate::status::Severity::Error, m);
            }
            Ok(()) => {
                if let Some(n) = note {
                    ed.say(crate::status::Severity::Warning, n);
                }
            }
        }
        self.grip_cache = None;
    }
}

/// Grip point under the ray with constraint, snap and step.
pub fn grip_to(
    g: &Grip3,
    r: &Ray,
    snap: Option<&Snap>,
    ctrl: bool,
    step: f32,
    shift: bool,
) -> Option<Vec3> {
    let on_line = |d: Vec3| -> Option<Vec3> {
        let p = match snap {
            Some(s) => g.pos + d * (s.p - g.pos).dot(d),
            None => g.pos + d * ray_line(r, g.pos, d)?,
        };
        Some(if snap.is_none() && step > 0.0 {
            g.typed(round_to(g.value(p), step), p)
        } else {
            p
        })
    };
    match g.axis {
        Axis3::Line(d) => on_line(d),
        Axis3::Plane(_) if ctrl && g.lifts() => {
            let p = match snap {
                Some(s) => g.pos.with_z(s.p.z),
                None => g.pos + Vec3::Z * ray_line(r, g.pos, Vec3::Z)?,
            };
            Some(if snap.is_none() && step > 0.0 {
                p.with_z(round_to(p.z, step))
            } else {
                p
            })
        }
        Axis3::Plane(n) => {
            let p = match snap {
                Some(s) => s.p - n * (s.p - g.pos).dot(n),
                None => ray_plane(r, g.pos, n)?.1,
            };
            Some(if snap.is_none() && step > 0.0 && n.z.abs() > 0.999 {
                Vec3::new(round_to(p.x, step), round_to(p.y, step), p.z)
            } else {
                p
            })
        }
        Axis3::Ring { center, axis, .. } => {
            let p = match snap {
                Some(s) => s.p - axis * (s.p - center).dot(axis),
                None => ray_plane(r, center, axis)?.1,
            };
            let a = g.value(p);
            let a = if snap.is_none() {
                round_to(a, if shift { 15.0 } else { 5.0 })
            } else {
                a
            };
            Some(g.typed(a, p))
        }
    }
}

// ---- commands ----

fn open(a: &mut EditorApp, t: Tool) {
    a.view3d = true;
    let e = &mut a.view.e3;
    let t = if e.tool == t && t != Tool::Select {
        Tool::Select
    } else {
        t
    };
    e.set_tool(t);
}

fn wp_ref(a: &mut EditorApp) {
    a.view3d = true;
    let rps = a.ed.model.ref_planes().unwrap_or_default();
    let e = &mut a.view.e3;
    let cur = match e.plane.source {
        plane::Source::Ref(id) if e.plane_user => rps.iter().position(|r| r.0 == id),
        _ => None,
    };
    let next = cur.map_or(0, |i| i + 1);
    match rps
        .get(next)
        .or(rps.first())
        .and_then(|r| WorkPlane::vertical(r.0, r.1, r.2))
    {
        Some(mut wp) => {
            wp.origin.z = 0.0;
            e.plane = wp;
            e.plane_user = true;
            e.plane_elev = a.ed.elevation;
            a.ed.status = Some(tr(e.ru, "e3d.wp.set"));
        }
        None => a.ed.status = Some(tr(e.ru, "e3d.wp.no_ref")),
    }
}

/// Module commands: the ribbon "Modify" tab, and the "3D" and "Work plane" groups.
pub fn register(a: &mut EditorApp) {
    struct3d::register();
    ramp3d::register();
    crate::solid_edit3d::install();
    crate::freeform_edit3d::install();
    crate::panel_system_edit3d::install();
    crate::shell_elem_edit3d::install();
    a.register_command("e3d.gizmo", |a| {
        a.view3d = true;
        let e = &mut a.view.e3;
        e.gizmo = !e.gizmo;
        e.set_tool(Tool::Select);
    });
    a.register_command("e3d.box", |a| open(a, Tool::Box));
    a.register_command("e3d.measure", |a| open(a, Tool::Measure));
    a.register_command("e3d.measure.distance", |a| {
        a.view.e3.ms.mode = measure_saved::Mode::Distance;
        if a.view.e3.tool != Tool::Measure {
            open(a, Tool::Measure);
        }
    });
    a.register_command("e3d.measure.gap", |a| {
        a.view.e3.ms.mode = measure_saved::Mode::Clear;
        if a.view.e3.tool != Tool::Measure {
            open(a, Tool::Measure);
        }
    });
    a.register_command("e3d.measure.angle", |a| {
        a.view.e3.ms.mode = measure_saved::Mode::Angle;
        if a.view.e3.tool != Tool::Measure {
            open(a, Tool::Measure);
        }
    });
    a.register_command("e3d.measure.area", |a| {
        a.view.e3.ms.mode = measure_saved::Mode::Area;
        if a.view.e3.tool != Tool::Measure {
            open(a, Tool::Measure);
        }
    });
    a.register_command("e3d.measure.volume", |a| {
        a.view.e3.ms.mode = measure_saved::Mode::Volume;
        if a.view.e3.tool != Tool::Measure {
            open(a, Tool::Measure);
        }
    });
    a.register_command("e3d.measure.slope", |a| {
        a.view.e3.ms.mode = measure_saved::Mode::Slope;
        if a.view.e3.tool != Tool::Measure {
            open(a, Tool::Measure);
        }
    });
    a.register_command("e3d.measure.chain", |a| {
        a.view.e3.ms.mode = measure_saved::Mode::Chain;
        if a.view.e3.tool != Tool::Measure {
            open(a, Tool::Measure);
        }
    });
    a.register_command("e3d.snap", |a| {
        // one switch for plan and 3D: the same toggle as F3 in the status bar
        crate::shell::status_bar::h_snap(a);
        let e = &mut a.view.e3;
        a.ed.status = Some(tr(
            e.ru,
            if e.snaps_off {
                "e3d.snap.off"
            } else {
                "e3d.snap.on"
            },
        ));
    });
    a.register_command("e3d.wp_level", |a| {
        a.view3d = true;
        let e = &mut a.view.e3;
        e.plane_user = false;
        e.plane = WorkPlane::level(a.ed.level, 0.0);
        a.ed.status = Some(tr(e.ru, "e3d.wp.set"));
    });
    a.register_command("e3d.wp_face", |a| open(a, Tool::Face));
    a.register_command("e3d.wp_ref", wp_ref);
    a.register_command("e3d.wp_pick", |a| open(a, Tool::Pick3));
    wp_win::register(a);
    a.register_command("e3d.wall", |a| open(a, Tool::Create(create::Kind::Wall)));
    a.register_command("e3d.floor", |a| open(a, Tool::Create(create::Kind::Floor)));
    a.register_command("e3d.column", |a| {
        open(a, Tool::Create(create::Kind::Column))
    });
    a.register_command("e3d.beam", |a| open(a, Tool::Create(create::Kind::Beam)));
}

/// Module command ids (for data checks).
pub const COMMANDS: [&str; 19] = [
    "e3d.gizmo",
    "e3d.box",
    "e3d.measure",
    "e3d.measure.distance",
    "e3d.measure.gap",
    "e3d.measure.angle",
    "e3d.measure.area",
    "e3d.measure.volume",
    "e3d.measure.slope",
    "e3d.measure.chain",
    "e3d.snap",
    "e3d.wp_level",
    "e3d.wp_face",
    "e3d.wp_ref",
    "e3d.wp_pick",
    "e3d.wall",
    "e3d.floor",
    "e3d.column",
    "e3d.beam",
];
