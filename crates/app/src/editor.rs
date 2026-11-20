// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editor: model + "Wall" and "Door/Window" tools + history (Ctrl+Z / Ctrl+Y) + selection.
//! Each gesture is one model operation; redo replays the gesture afresh (new ids are reassigned).
//! Selection is shared by plan and 3D: `Sel` <-> scene element (`element_of` / `sel_of_element`).

use std::collections::{HashMap, HashSet};

use crate::opening_tool::{
    snap_along, symbol, validate, Flip, HostWall, Kind, OpOutcome, OpeningTool, OpeningType,
    PlaceError, Placed, Preview, Symbol,
};
use crate::snap::{closest_on_segment, dot, SnapWall};
use crate::status::Severity;
use crate::units::Units;
use crate::wall_tool::{Event, Key, Mode, Outcome, WallTool};
use ddc_geom::arc::{arc_bodies, CHORD_SAG};
use ddc_geom::layered::{layer_footprints, layer_solids, plain_pieces, Layer, Strip};
use ddc_geom::{v2, Tol, V2};
use ddc_model::{LayerSpec, Model, OpeningSpec, Swing, WallSpec};
use ddc_render::{Scene, SceneMesh};

#[cfg(test)]
thread_local! {
    /// Join scans done by `reload` (test hook for the per-level query bound).
    static CUT_QUERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub mod bodies;
pub mod busy;
pub mod cw;
pub mod delete;
pub mod delete_deps;
pub mod hist;
pub mod opening_fill;
pub mod levels;
pub mod ops_run;
pub mod pick_index;
mod preview_prims;
pub mod sched_ops;
pub mod sec;
pub mod stage;
pub mod structx;
#[cfg(test)]
mod addcommit;
pub(crate) mod scoped;
mod scoped_more;
#[cfg(test)]
mod import_steps;
pub use cw::{CurtainDefaults, CurtainItem, SlabItem};
pub use levels::RoomItem;

#[derive(Clone, Debug)]
pub struct WallType {
    pub id: i64,
    pub name: String,
    pub layers: Vec<f64>,
}

impl WallType {
    pub fn thickness(&self) -> f64 {
        self.layers.iter().sum()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Wall {
    pub id: i64,
    /// 0 is a straight wall, otherwise the arc bulge from `a` to `b`.
    pub bulge: f64,
    pub wall_type: i64,
    pub a: V2,
    pub b: V2,
    /// Bottom above the level; imported walls often sit off their level.
    pub base_offset: f64,
    pub height: f64,
    /// Plan area and volume (sum of layers, from `wall_qty`).
    pub plan_area: f64,
    pub volume: f64,
    /// Top polyline (u along the axis, height above the bottom); None means a flat top at `height`.
    pub top: Option<Vec<(f64, f64)>>,
    /// Bottom polyline (u, dz above the bottom): attachment to a floor slab or an elevation profile.
    pub base: Option<Vec<(f64, f64)>>,
    /// Lean and taper the geometry uses (plain under a top profile).
    pub lean: ddc_geom::wall_lean::Lean,
}

#[derive(Clone, Debug)]
struct Gesture {
    specs: Vec<WallSpec>,
    /// Per wall; empty means all straight walls.
    bulges: Vec<f64>,
}

/// The selected element: the same in plan and 3D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sel {
    Wall(i64),
    Opening(i64),
    /// A whole curtain wall (mullions and all panels).
    Curtain(i64),
    /// Panel `k` (layout cell index) of a curtain wall.
    Panel(i64, u32),
    Slab(i64),
    /// A room (plan only: it has no 3D body).
    Room(i64),
    /// A mass (of any level; in plan, its section at the plan height).
    Mass(i64),
    /// A run of an engineering network (with insulation and branches).
    Mep(i64),
    /// A component instance.
    Comp(i64),
    /// A view annotation (text, line, region, mark...), 2D only.
    Anno(i64),
    /// An element of another module's category: (category code, id); structural ones use `structx`.
    Ext(u16, i64),
    /// A level body as one element: an imported mesh (`mesh`, "Imported <category>"),
    /// a column, beam or ceiling (network runs are `Mep`).
    Body(bodies::BodyKind, i64),
}

/// Full description of an opening for replaying the operation.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OpSpec {
    wall: i64,
    along: f64,
    ty: OpeningType,
    flip: Flip,
}

impl OpSpec {
    fn model(&self) -> OpeningSpec {
        OpeningSpec {
            along: self.along,
            width: self.ty.width,
            sill: self.ty.sill,
            height: self.ty.height,
            kind: self.ty.kind.model_name().into(),
        }
    }

    fn swing(&self) -> Swing {
        Swing {
            side: self.flip.side,
            hinge: self.flip.hinge,
        }
    }
}

#[derive(Clone, Debug)]
enum Cmd {
    Add(Gesture, Vec<i64>),
    /// Delete: (table, id) of everything that went in one model step; redo follows new ids.
    Items(Vec<delete::Item>),
    AddOpening(OpSpec, i64),
    UpdateOpening(i64, OpSpec, OpSpec),
    /// Wall edit from properties (length, type, height): before and after.
    EditWall(i64, WallSpec, WallSpec),
    /// Bridge import (file, frames): one model operation, the untouched default level removed
    /// in it when the third field (tidy) is set; redo applies the same frames again.
    Import(
        std::path::PathBuf,
        std::sync::Arc<Vec<ddc_io::bridge::proto::Frame>>,
        bool,
    ),
    AddCurtains(Vec<ddc_model::CurtainSpec>, Vec<i64>),
    /// Curtain wall edit: the spec after.
    EditCurtain(i64, ddc_model::CurtainSpec),
    /// Cell panel (i, j): after.
    Panel(i64, u32, u32, ddc_model::PanelSpec),
    SlabSlope(i64, Option<ddc_model::SlabSlope>),
    /// Room (name, number, height): after.
    Room(i64, ddc_model::RoomSpec),
    /// Section/elevation: spec and id; edit (after); delete; elevations by compass sides.
    AddView(ddc_model::SectionSpec, i64),
    EditView(i64, ddc_model::SectionSpec),
    DeleteView(i64),
    Compass(Vec<i64>),
    /// Level: name, elevation, id (a replay gives a new id); edit (after); delete.
    LevelAdd(String, f64, i64),
    LevelEdit(i64, String, f64),
    LevelDelete(i64),
    /// XLSX import: an edit batch as one model operation; redo applies it again.
    Xlsx(std::sync::Arc<Vec<ddc_model::Edit>>),
    /// Edit operation (`modify_tools`): one model transaction; a replay calls it again.
    Op(OpFn),
    /// Envelope run by `run_ops`: n journal entries; redo replays the same envelope.
    Ops(std::sync::Arc<ddc_ops::Envelope>, u32),
    /// Model tool record (`tools`): n model operations make one undo step.
    Ext(crate::tools::Rec),
    /// Underlay edit: no model operation, undo and redo swap the state it stores.
    Underlay(crate::drawing::history::Step),
    /// Section box face drag (`view3d::boxdrag`): before, after; no model operation.
    SectionBox(ddc_render::Aabb, ddc_render::Aabb),
}

/// A replayable model operation for [`Editor::run_op`].
#[derive(Clone)]
pub struct OpFn(
    pub std::sync::Arc<dyn Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync>,
);

impl std::fmt::Debug for OpFn {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("OpFn")
    }
}

/// Dragging an opening along its wall.
#[derive(Clone, Debug)]
pub struct Drag {
    pub id: i64,
    pub host: HostWall,
    pub ty: OpeningType,
    pub from: f64,
    pub along: f64,
    /// Pointer offset from the opening's centre along the wall at press.
    pub grab: f64,
    pub moved: bool,
    pub error: Option<PlaceError>,
}

/// Rows an XLSX Apply creates in a linked book: (sheet, book row, fingerprint).
pub struct XlsxLink {
    pub book: std::path::PathBuf,
    pub rows: Vec<(String, u32, String)>,
}

pub struct Editor {
    pub model: Model,
    pub level: i64,
    pub types: Vec<WallType>,
    pub current_type: usize,
    pub height: f64,
    pub tool: WallTool,
    pub walls: Vec<Wall>,
    /// Wall layers in plan (outlines), per wall.
    pub plan: Vec<Vec<Vec<V2>>>,
    /// The plan was joined over straight walls without error (`scoped` may patch a part).
    plan_joined: bool,
    /// Level and schema of the last full reload: the scoped reload patches only those lists.
    scoped_key: Option<(i64, i64)>,
    pub selection: Vec<Sel>,
    /// Openings of the current floor (side/hinges are read from the model).
    pub openings: Vec<Placed>,
    /// Openings whose fill is a linked component: the plan draws the component, not the symbol.
    pub opening_filled: std::collections::HashSet<i64>,
    /// Detail rules of the model's project: door and window types, railings, roof edges.
    pub detail: ddc_geom::detail::rules::DetailRules,
    /// The "Door/Window" tool is active (otherwise the "Wall" tool in its own mode).
    pub opening_mode: bool,
    pub otool: OpeningTool,
    pub drag: Option<Drag>,
    pub status: Option<String>,
    /// Severity of the text last set through `say`; see `status.rs`.
    pub status_sev: Option<(crate::status::Severity, String)>,
    /// Grows on every `say`, so a repeated line toasts again.
    pub status_seq: u64,
    /// A destructive op just committed: its toast carries Undo for this entry (`offer_undo`).
    pub undo_offer: Option<u64>,
    /// A delete waiting for the user's answer: dependents were too many or on another level.
    pub pending_delete: Option<delete_deps::Pending>,
    /// Status bar pick filter: categories and pinned items a click and hover skip.
    pub pick_filter: crate::pick_rules::PickFilter,
    /// Command the status toast offers as its action button (Redo after Undo); read once.
    pub status_cmd: Option<&'static str>,
    /// History entry `status_cmd` acts on: the toast button undoes or redoes exactly this one.
    pub status_entry: Option<u64>,
    /// Rooms that lost their enclosure after wall edits; see `room_watch.rs`.
    pub room_watch: crate::room_watch::Watch,
    /// Tables the last undo kept for rows written after an Add (name, rows); see `hist.rs`.
    pub(crate) undo_kept: Vec<(String, i64)>,
    /// Grows on every model change (for rebuilding 3D).
    pub revision: u64,
    /// `levels()` result keyed by (revision, total_changes).
    pub(crate) levels_memo: std::sync::Mutex<Option<((u64, u64), Vec<(i64, String, f64)>)>>,
    /// Highest revision handed out, also when a level walk put an older one back.
    pub(crate) rev_top: u64,
    /// Curtain walls and floor slabs of the current level, glazing lines in plan.
    pub curtains: Vec<CurtainItem>,
    pub slabs: Vec<SlabItem>,
    pub curtain_lines: Vec<(i64, V2, V2)>,
    /// Elevation of the current level: 3D is built from it.
    pub elevation: f64,
    pub cw_defaults: CurtainDefaults,
    /// Model sections and elevations; the "Section" tool; dragging a mark grip.
    pub views: Vec<(i64, ddc_model::SectionSpec)>,
    /// Active mass command (face picking in 3D, outline in plan).
    pub mass: Option<Box<crate::mass_tools::MassTool>>,
    /// Mass sections at the current level's plan height: (mass, outlines).
    pub mass_plan: Vec<(i64, Vec<Vec<V2>>)>,
    pub vdrag: Option<sec::ViewDrag>,
    /// Element grips and input on temporary dimensions (`grips`).
    pub grip: crate::grips::State,
    /// Snap index of the view level, cached by (level, revision).
    pub snap_svc: crate::snap_svc::Service,
    /// The user's work plane for the plan and section views (`wp_views`); `None` on the level.
    pub work_plane: Option<crate::edit3d::plane::Shown>,
    /// Curtain wall boxes of the view level, cached by (level, revision).
    pub pick_cw: pick_index::PickCache,
    /// Rooms of the current level (outlines for picking by clicking inside).
    pub rooms: Vec<RoomItem>,
    /// Tags of the current level (a per-frame cache of `Model::tags`: hit-test and drawing).
    pub tags: Vec<ddc_model::Tag>,
    /// Imported bodies of the current level (plan outline, picking, editing).
    pub bodies: Vec<bodies::BodyItem>,
    /// Columns of lower levels that pass this level's elevation: drawn on its plan only (3D
    /// takes each body from its own level once).
    pub bodies_through: Vec<bodies::BodyItem>,
    /// Bodies of all levels, rebuilt only for rows written since the last reload.
    pub body_cache: bodies::BodyCache,
    /// 3D is assembled by `edit3d` providers: they draw roofs and bodies (`bodies`), not `scene`.
    pub scene_providers: bool,
    /// 3D of the active level only: other levels' parts carry no element number, so a
    /// snapshot that reads elements per level does not build them.
    pub scene_one_level: bool,
    /// Cut wall bodies by their geometric input, shared by the storeys of a typical floor.
    pub wall_cuts: crate::wall_cuts::WallCuts,
    /// Underlay: wall outlines of the level below (faded); a toggle.
    pub underlay: Vec<Vec<Vec<V2>>>,
    pub underlay_on: bool,
    /// Outline sketch mode and slab shape editing (entry via `sketch_mode::begin`, `shape_edit::begin`).
    pub sketch: Option<crate::sketch_mode::SketchMode>,
    pub shape: Option<crate::shape_edit::ShapeEditor>,
    /// Networks and components in 3D, the change journal for partial rebuilds (`mep3d`).
    pub big: crate::mep3d::Big,
    /// Terrain: tools, plan cache, windows (`crate::topo`).
    pub topo: crate::topo::State,
    /// Structural outlines of the level (schema 26) for picking in plan.
    pub struct_fp: structx::StructFp,
    undo: Vec<Cmd>,
    redo: Vec<Cmd>,
    /// Entry id of each `undo` / `redo` step, index for index: the `pushes` count that made it.
    undo_ids: Vec<u64>,
    redo_ids: Vec<u64>,
    /// Underlay steps undo or redo reached; `Drawings::sync_undo` applies them in order.
    underlay_due: Vec<crate::drawing::history::Step>,
    /// Section box undo or redo reached; `view3d::boxdrag::sync_undo` puts it on the view.
    box_due: Option<ddc_render::Aabb>,
    /// Total steps recorded (a new step clears "Redo"): history labels (`quick`).
    pub pushes: u64,
    /// Project file, working copy, dirty flag. After `model`: the file is deleted when
    /// the connection is already closed.
    pub project: crate::project::ProjectState,
    /// Thin-lines display toggle per view: view state outside the journal and the file.
    pub view_thin: std::collections::HashMap<ddc_model::ViewKey, bool>,
    /// Grows when `view_thin` changes, so cached view graphics rebuild.
    pub view_thin_rev: u64,
    /// Saved thin-lines preference for views without a session choice (mirrors `Prefs`).
    pub thin_pref: bool,
    /// Full plan axes preference, copied from `Prefs` every frame.
    pub axes_pref: bool,
    /// Interaction spine: the scope and the running tool session (`interact`).
    pub ix: crate::interact::Interaction,
}

fn p2(v: V2) -> (f64, f64) {
    (v.x, v.y)
}

impl Editor {
    /// An empty in-memory model: level 0 and three wall types.
    pub fn new_in_memory() -> Result<Self, ddc_model::Error> {
        Self::seeded(Model::open(":memory:")?)
    }

    /// The editor as the app starts it: in Select (`EditorApp::new` does the same). The bare
    /// `new_in_memory` keeps the wall tool armed; tests of app behaviour start from this.
    #[cfg(test)]
    pub(crate) fn new_as_app() -> Result<Self, ddc_model::Error> {
        let mut e = Self::new_in_memory()?;
        e.set_mode(Mode::Select);
        Ok(e)
    }

    /// Default level and wall types of a new project, named in the UI language of the moment;
    /// returns the level. Also used by the open worker, which builds a project before any
    /// editor sees it. Stored names are never renamed afterwards.
    pub(crate) fn seed_model(m: &mut Model) -> Result<i64, ddc_model::Error> {
        Self::seed_model_in(m, crate::ui_lang_now())
    }

    /// Seed with names in the given UI language; names are stored data, so existing files keep theirs.
    pub(crate) fn seed_model_in(m: &mut Model, ru: bool) -> Result<i64, ddc_model::Error> {
        let n = |k: &str| crate::tr_lang(ru, k, &[]);
        let level = m.add_level(&n("project_seed.seed_level"), 0.0)?;
        let defs: [(String, &[(&str, f64)]); 3] = [
            (n("project_seed.seed_wall_partition"), &[("gypsum", 0.1)]),
            (n("project_seed.seed_wall_masonry"), &[("masonry", 0.2)]),
            (
                n("project_seed.seed_wall_exterior"),
                &[
                    ("plaster", 0.02),
                    ("masonry", 0.25),
                    ("insulation", 0.1),
                    ("render", 0.01),
                ],
            ),
        ];
        for (name, ls) in defs {
            let specs: Vec<LayerSpec> = ls
                .iter()
                .map(|(m, t)| LayerSpec {
                    material: (*m).into(),
                    thickness: *t,
                })
                .collect();
            m.add_wall_type(&name, &specs)?;
        }
        Ok(level)
    }

    /// Editor on an empty model `m`: level 0 and the wall types.
    pub(crate) fn seeded(mut m: Model) -> Result<Self, ddc_model::Error> {
        let level = Self::seed_model(&mut m)?;
        let mut e = Self {
            model: m,
            level,
            types: vec![],
            current_type: 1,
            height: 3.0,
            tool: WallTool::new(),
            walls: vec![],
            plan: vec![],
            plan_joined: false,
            scoped_key: None,
            selection: vec![],
            openings: vec![],
            opening_filled: Default::default(),
            detail: Default::default(),
            opening_mode: false,
            otool: OpeningTool::new(),
            drag: None,
            status: None,
            status_sev: None,
            status_seq: 0,
            undo_offer: None,
            pending_delete: None,
            pick_filter: Default::default(),
            room_watch: Default::default(),
            undo_kept: vec![],
            revision: 0,
            levels_memo: Default::default(),
            rev_top: 0,
            curtains: vec![],
            slabs: vec![],
            curtain_lines: vec![],
            elevation: 0.0,
            cw_defaults: CurtainDefaults::default(),
            views: vec![],
            mass: None,
            mass_plan: vec![],
            vdrag: None,
            grip: Default::default(),
            snap_svc: Default::default(),
            work_plane: None,
            pick_cw: Default::default(),
            rooms: vec![],
            tags: vec![],
            bodies: vec![],
            bodies_through: vec![],
            body_cache: Default::default(),
            scene_providers: false,
            scene_one_level: false,
            wall_cuts: Default::default(),
            underlay: vec![],
            underlay_on: true,
            sketch: None,
            shape: None,
            big: Default::default(),
            topo: Default::default(),
            struct_fp: Default::default(),
            undo: vec![],
            redo: vec![],
            undo_ids: vec![],
            redo_ids: vec![],
            underlay_due: vec![],
            box_due: None,
            pushes: 0,
            project: Default::default(),
            view_thin: Default::default(),
            view_thin_rev: 0,
            thin_pref: false,
            axes_pref: false,
            ix: Default::default(),
            status_cmd: None,
            status_entry: None,
        };
        e.reload()?;
        e.project.mark_clean(&e.model);
        // the wall tool it opens with is its session from the start
        crate::interact::Interaction::refresh(&mut e);
        Ok(e)
    }

    /// Replace the model (snapshot restore): edit history and selection are reset.
    pub fn replace_model(&mut self, m: Model) -> Result<(), ddc_model::Error> {
        self.swap_model(m, None)
    }

    /// `replace_model` that also shows `level`. The new model must load first: if it does not,
    /// the old model, its file, level and history all stay.
    pub(crate) fn swap_model(
        &mut self,
        m: Model,
        level: Option<i64>,
    ) -> Result<(), ddc_model::Error> {
        let (old, old_level) = (std::mem::replace(&mut self.model, m), self.level);
        self.fresh_revision();
        // room ids and the change journal belong to the old model: its flags must not carry over
        let old_watch = std::mem::take(&mut self.room_watch);
        if let Some(l) = level {
            self.level = l;
        }
        if let Err(e) = self.reload() {
            self.model = old;
            self.fresh_revision();
            self.level = old_level;
            self.room_watch = old_watch;
            let _ = self.reload();
            return Err(e);
        }
        crate::shell::xlsx_applied::on_model_replaced(&old, &self.model);
        drop(old);
        // whatever this is, it is not the open file: Save must not write it there
        self.project.detach(&self.model);
        crate::ai::mcp_bridge::on_model_replaced();
        crate::roof_edge_tools::reset();
        self.undo.clear();
        self.redo.clear();
        self.undo_ids.clear();
        self.redo_ids.clear();
        self.selection.clear();
        // a freshly opened model never starts inside a drawing tool
        self.set_mode(Mode::Select);
        self.revision += 1;
        crate::solid_tools::on_model_reset(self);
        crate::annot_text::on_model_reset();
        crate::shell::panels::browser::on_model_reset();
        crate::data_ui::on_model_reset();
        crate::view_gfx_ui::vv_win::on_model_reset();
        Ok(())
    }

    pub fn set_units(&mut self, u: Units) {
        self.tool.units = u;
        self.otool.units = u;
        crate::units::set_active_units(u);
    }

    pub fn current_type(&self) -> Option<&WallType> {
        self.types.get(self.current_type)
    }

    pub(crate) fn type_by_id(&self, id: i64) -> Option<&WallType> {
        self.types.iter().find(|t| t.id == id)
    }

    /// Openings of every level, lowest level first, then by id.
    pub(crate) fn openings_all(&self) -> Result<Vec<Placed>, ddc_model::Error> {
        let db = self.model.sql();
        let mut st = db.prepare(&format!(
            "{OPENING_SQL} JOIN level l ON l.id=w.level ORDER BY l.elevation, o.id"
        ))?;
        let rows = st.query_map([], opening_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Walls of `level` from the model.
    pub(crate) fn walls_on(&self, level: i64) -> Result<Vec<Wall>, ddc_model::Error> {
        let db = self.model.sql();
        let mut st = db.prepare(&format!("{WALL_SQL} WHERE w.level=?1 ORDER BY w.id"))?;
        let walls = st
            .query_map([level], wall_row)?
            .collect::<Result<Vec<_>, _>>()?;
        self.with_bases(walls)
    }

    /// Base profiles of the straight walls (`walls_on` and the scoped reload).
    pub(super) fn with_bases(&self, mut walls: Vec<Wall>) -> Result<Vec<Wall>, ddc_model::Error> {
        let ids: Vec<i64> = walls
            .iter()
            .filter(|w| w.bulge == 0.0)
            .map(|w| w.id)
            .collect();
        let bases = self.model.wall_base_profiles(&ids)?;
        for (w, b) in walls.iter_mut().filter(|w| w.bulge == 0.0).zip(bases) {
            w.base = b;
        }
        Ok(walls)
    }
    /// Re-reads types, walls and quantities from the model; refreshes snaps and the plan.
    pub fn reload(&mut self) -> Result<(), ddc_model::Error> {
        self.scoped_key = None;
        self.reload_model()?;
        self.scoped_key = scoped::key(self);
        self.struct_reload();
        self.revision += 1;
        crate::room_watch::after_reload(self);
        Ok(())
    }

    fn reload_model(&mut self) -> Result<(), ddc_model::Error> {
        self.ensure_level()?;
        let db = self.model.sql();
        let mut st = db.prepare(
            "SELECT t.id, t.name, l.thickness FROM wall_type t \
             JOIN wall_type_layer l ON l.wtype=t.id ORDER BY t.id, l.pos",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut types: Vec<WallType> = vec![];
        for r in rows {
            let (id, name, th): (i64, String, f64) = r?;
            match types.last_mut() {
                Some(t) if t.id == id => t.layers.push(th),
                _ => types.push(WallType {
                    id,
                    name,
                    layers: vec![th],
                }),
            }
        }
        drop(st);
        let walls = self.walls_on(self.level)?;
        let mut st = db.prepare(&format!("{OPENING_SQL} WHERE w.level=?1 ORDER BY o.id"))?;
        let rows = st.query_map([self.level], opening_row)?;
        let openings = rows.collect::<Result<Vec<_>, _>>()?;
        drop(st);
        let mut st = db.prepare("SELECT opening FROM opening_comp")?;
        let filled = st
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        drop(st);
        self.opening_filled = filled;
        self.detail = opening_fill::rules(&self.model);
        // the drawing type is an index: keep it on the same type if the list shifted
        let cur = self.types.get(self.current_type).map(|t| t.id);
        self.types = types;
        self.current_type = cur
            .and_then(|id| self.types.iter().position(|t| t.id == id))
            .unwrap_or_else(|| self.current_type.min(self.types.len().saturating_sub(1)));
        self.walls = walls;
        self.openings = openings;
        let l = cw::load(&self.model, self.level);
        (
            self.curtains,
            self.slabs,
            self.curtain_lines,
            self.elevation,
        ) = (l.curtains, l.slabs, l.lines, l.elevation);
        let wset: HashSet<i64> = self.walls.iter().map(|w| w.id).collect();
        let oset: HashSet<i64> = self.openings.iter().map(|o| o.id).collect();
        let cells: HashMap<i64, usize> = self
            .curtains
            .iter()
            .map(|c| (c.id, c.layout.layout.cells.len()))
            .collect();
        let sset: HashSet<i64> = self.slabs.iter().map(|x| x.id).collect();
        let m = &self.model;
        self.selection.retain(|s| match s {
            Sel::Wall(id) => wset.contains(id),
            Sel::Opening(id) => oset.contains(id),
            Sel::Curtain(id) => cells.contains_key(id),
            Sel::Panel(id, k) => cells.get(id).is_some_and(|n| (*k as usize) < *n),
            Sel::Slab(id) => sset.contains(id),
            Sel::Room(_) | Sel::Ext(..) | Sel::Body(..) => true,
            Sel::Mass(_) => true,
            // networks and components are not in the editor lists: check they are alive in the model (undo of an add)
            Sel::Mep(id) => m.mep_run(*id).is_ok(),
            Sel::Comp(id) => m.component(*id).is_ok(),
            Sel::Anno(id) => m.anno_view(*id).is_ok(),
        });
        self.otool.walls = self.walls.iter().map(|w| self.host(w)).collect();
        self.otool.openings = self.openings.clone();
        self.otool.preview = None;
        self.tool.walls = self
            .walls
            .iter()
            .map(|w| SnapWall {
                a: w.a,
                b: w.b,
                half: self
                    .type_by_id(w.wall_type)
                    .map_or(0.0, |t| t.thickness() / 2.0),
            })
            .collect();
        let mut strips = self.strips(&self.walls);
        let cut_rel = crate::plan_range::range(self).cut;
        // a leaning wall is drawn by its horizontal section at the cut height
        let leaning = self.walls.iter().any(|w| !w.lean.is_plain());
        for (s, w) in strips.iter_mut().zip(&self.walls) {
            if !w.lean.is_plain() {
                let z = (cut_rel - w.base_offset).clamp(0.0, w.height);
                *s = ddc_geom::wall_lean::strip_at(s, w.bulge, &w.lean, z);
            }
        }
        let bulges: Vec<f64> = self.walls.iter().map(|w| w.bulge).collect();
        let prefs = self.join_prefs();
        (self.plan, self.plan_joined) = footprints_joined(&strips, &bulges, &prefs, strips.len());
        // the scoped reload rebuilds footprints at the base: not for leaning walls
        self.plan_joined &= !leaning;
        let z = self.elevation + cut_rel;
        // one join scan per reload; only cut walls pay for a section
        #[cfg(test)]
        CUT_QUERIES.with(|c| c.set(c.get() + 1));
        let cut = scoped::cut_walls(&self.model);
        let ids: Vec<i64> = self.walls.iter().map(|w| w.id).collect();
        for (i, id) in ids.into_iter().enumerate() {
            if let Some(rs) = scoped::cut_plan(&self.model, cut.as_ref(), id, z) {
                self.plan[i] = rs;
            }
        }
        self.reload_rest(z)?;
        self.underlay = self.underlay_rings()?;
        Ok(())
    }

    /// Lists of `reload_model` that do not depend on single wall rows: masses, views, rooms,
    /// tags, bodies (the scoped reload loads them the same way).
    fn reload_rest(&mut self, z: f64) -> Result<(), ddc_model::Error> {
        self.mass_plan = crate::mass_tools::plan_sections(&self.model, z);
        let ms: HashSet<i64> = self.model.masses()?.into_iter().collect();
        self.selection
            .retain(|s| !matches!(s, Sel::Mass(id) if !ms.contains(id)));
        self.views = self.model.view_defs()?;
        self.rooms = levels::load_rooms(&self.model, self.level);
        self.tags = self.model.tags(self.level).unwrap_or_default();
        self.bodies = self.body_cache.level(&self.model, self.level);
        self.bodies_through = self.body_cache.through(self.level, self.elevation);
        let rset: HashSet<i64> = self.rooms.iter().map(|r| r.id).collect();
        let bset: HashSet<(_, i64)> = self.bodies.iter().map(|b| (b.kind, b.id)).collect();
        self.selection.retain(|s| match s {
            Sel::Room(id) => rset.contains(id),
            Sel::Body(k, id) => bset.contains(&(*k, *id)),
            _ => true,
        });
        Ok(())
    }

    fn host(&self, w: &Wall) -> HostWall {
        HostWall {
            id: w.id,
            a: w.a,
            b: w.b,
            height: w.height,
            half: self
                .type_by_id(w.wall_type)
                .map_or(0.05, |t| t.thickness() / 2.0),
            run: ddc_geom::arc::axis_length(w.a, w.b, w.bulge),
        }
    }

    pub fn host_of(&self, wall: i64) -> Option<HostWall> {
        self.walls
            .iter()
            .find(|w| w.id == wall)
            .map(|w| self.host(w))
    }

    pub fn opening(&self, id: i64) -> Option<&Placed> {
        self.openings.iter().find(|o| o.id == id)
    }

    /// Plan symbols of all floor openings (accounting for dragging) and ids.
    pub fn opening_symbols(&self) -> Vec<(i64, Symbol)> {
        self.openings
            .iter()
            .filter(|o| !self.opening_filled.contains(&o.id))
            .filter_map(|o| {
                let h = self.host_of(o.wall)?;
                let along = match &self.drag {
                    Some(d) if d.id == o.id => d.along,
                    _ => o.along,
                };
                Some((o.id, symbol(&h, along, o.width, o.kind, o.flip, &self.detail)))
            })
            .collect()
    }

    /// Preview of the opening being placed: symbol and error text (if any).
    pub fn opening_preview(&self) -> Option<(Symbol, Option<String>)> {
        if !self.opening_mode {
            return None;
        }
        let Preview {
            wall, along, error, ..
        } = self.otool.preview.as_ref()?;
        let h = self.otool.walls.get(*wall)?;
        let t = &self.otool.ty;
        Some((
            symbol(h, *along, t.width, t.kind, self.otool.flip, &self.detail),
            error.as_ref().map(|e| e.to_string()),
        ))
    }

    /// The "Door/Window" tool with type `ty`.
    pub fn start_opening(&mut self, ty: OpeningType) {
        crate::topo::reset(&mut self.topo);
        crate::interact::sessions::section::stop(self);
        crate::interact::sessions::model_tool::stop(self);
        self.tool.set_mode(Mode::Select);
        self.tool.curtain = false;
        self.drag = None;
        self.otool.ty = ty;
        self.otool.reset();
        self.opening_mode = true;
        self.selection.clear();
        // the wall session becomes the opening one (it does not draw)
        self.ix.disarm();
        crate::interact::Interaction::refresh(self);
    }

    pub(crate) fn handle_opening(&mut self, e: Event) {
        match self.otool.handle(e) {
            OpOutcome::Nothing => {}
            OpOutcome::Exit => self.set_mode(Mode::Select),
            OpOutcome::Error(m) => match self.otool.refusal() {
                Some(r) => self.refuse(r),
                None => self.status = Some(m),
            },
            OpOutcome::Place { wall, along, flip } => {
                let spec = OpSpec {
                    wall,
                    along,
                    ty: self.otool.ty,
                    flip,
                };
                match self.apply_add_opening(&spec) {
                    Ok(id) => {
                        self.say(crate::status::Severity::Info, crate::tr("edit_status.ed.opening_placed", &[("id", id.to_string().as_str())]));
                        self.push(Cmd::AddOpening(spec, id));
                    }
                    Err(e) => self.say(crate::status::Severity::Refusal, crate::tr("edit_status.ed.opening_not_placed", &[("e", e.to_string().as_str())])),
                }
            }
        }
    }

    /// `done` plus a warning when the view refresh after a committed op fails; never an error.
    fn with_stale(&mut self, done: String) -> String {
        let stale = self.reload().err().map(|e| e.to_string());
        stale_join(done, stale.as_deref())
    }

    /// Undo entries made so far and the ids of those undo can still take back, oldest first.
    pub(crate) fn undo_entries(&self) -> (u64, &[u64]) {
        (self.pushes, &self.undo_ids)
    }

    fn push(&mut self, c: Cmd) {
        self.pushes += 1;
        self.undo.push(c);
        self.undo_ids.push(self.pushes);
        self.redo.clear();
        self.redo_ids.clear();
    }

    fn apply_add_opening(&mut self, s: &OpSpec) -> Result<i64, ddc_model::Error> {
        let id = self.model.add_opening_with(s.wall, &s.model(), s.swing())?;
        self.reload()?;
        Ok(id)
    }

    fn spec_of(&self, o: &Placed) -> OpSpec {
        OpSpec {
            wall: o.wall,
            along: o.along,
            ty: OpeningType {
                kind: o.kind,
                width: o.width,
                height: o.height,
                sill: o.sill,
            },
            flip: o.flip,
        }
    }

    /// The opening under a plan point: within the wall width and thickness (+ capture radius).
    pub fn opening_at(&self, p: V2) -> Option<i64> {
        self.openings.iter().find_map(|o| {
            let h = self.host_of(o.wall)?;
            let s = h.along_of(p);
            let n = dot(p - h.a, h.normal()).abs();
            let r = self.tool.radius;
            (s >= o.along - r && s <= o.along + o.width + r && n <= h.half + r).then_some(o.id)
        })
    }

    /// Ok carries the view-refresh error text of a committed update, if any.
    fn update_opening_op(&mut self, id: i64, after: OpSpec) -> Result<Option<String>, String> {
        let o = *self.opening(id).ok_or_else(|| crate::tr("edst.no_opening", &[]))?;
        let h = self.host_of(o.wall).ok_or_else(|| crate::tr("edst.no_opening_wall", &[]))?;
        validate(&h, &self.openings, Some(id), after.along, &after.ty)
            .map_err(|e| e.to_string())?;
        let before = self.spec_of(&o);
        if before == after {
            return Ok(None);
        }
        self.model
            .update_opening_with(id, &after.model(), after.swing())
            .map_err(|e| e.to_string())?;
        self.push(Cmd::UpdateOpening(id, before, after));
        Ok(self.reload().err().map(|e| e.to_string()))
    }

    /// Size edit from the properties panel: one undoable operation.
    pub fn set_opening_dims(&mut self, id: i64, width: f64, height: f64, sill: f64) {
        let Some(o) = self.opening(id).copied() else {
            return;
        };
        let mut after = self.spec_of(&o);
        after.ty.width = width;
        after.ty.height = height;
        after.ty.sill = sill;
        match self.update_opening_op(id, after) {
            Ok(st) => {
                let sev = if st.is_some() { Severity::Warning } else { Severity::Info };
                let msg = stale_join(crate::tr("edst.opening_changed", &[("id", &id.to_string())]), st.as_deref());
                self.say(sev, msg);
            }
            Err(e) => self.say(Severity::Refusal, crate::tr("edst.not_changed", &[("e", &e.to_string())])),
        }
    }

    /// Wall edit from the properties panel: length (from the start, straight walls with no top profile),
    /// type, height: one undoable operation.
    pub fn edit_wall(
        &mut self,
        id: i64,
        len: Option<f64>,
        wtype: Option<i64>,
        height: Option<f64>,
    ) {
        let Some(w) = self.walls.iter().find(|w| w.id == id).cloned() else {
            return;
        };
        let before = match self.wall_specs(&[id]) {
            Ok(mut v) => v.remove(0),
            Err(e) => {
                self.say(crate::status::Severity::Error, crate::tr("edit_status.ed.not_changed", &[("e", e.to_string().as_str())]));
                return;
            }
        };
        let mut after = before.clone();
        if let Some(l) = len {
            let d = w.b - w.a;
            let cur = d.len();
            if w.bulge != 0.0 || (w.top.is_some() && !self.model.wall_top_auto(id)) || cur <= 0.0 || !(l > 0.01) {
                self.say(crate::status::Severity::Refusal, crate::tr("edit_status.ed.not_changed_len", &[]));
                return;
            }
            let b = w.a + d * (l / cur);
            // nanometre grid: a typed 7.25 m reads back as 7.25, not 7.250000000000001
            let snap = |v: f64| (v * 1e9).round() / 1e9;
            after.b = (snap(b.x), snap(b.y));
        }
        if let Some(t) = wtype {
            after.wall_type = t;
        }
        if let Some(h) = height {
            if !(h > 0.0) {
                self.say(crate::status::Severity::Refusal, crate::tr("edit_status.ed.not_changed_height", &[]));
                return;
            }
            after.height = h;
        }
        if after == before {
            return;
        }
        let top_reset = w.top.is_some() && (after.height - before.height).abs() > 1e-9;
        match self.write_wall(id, &after) {
            Ok(()) => {
                let m = if top_reset {
                    crate::tr("geom.top_reset", &[])
                } else {
                    crate::tr("edit_status.ed.wall_changed", &[("id", id.to_string().as_str())])
                };
                self.say(crate::status::Severity::Info, m);
                self.push(Cmd::EditWall(id, before, after));
            }
            Err(e) => self.say(crate::status::Severity::Error, crate::tr("edit_status.ed.not_changed", &[("e", e.to_string().as_str())])),
        }
    }

    fn write_wall(&mut self, id: i64, s: &WallSpec) -> Result<(), ddc_model::Error> {
        self.model.batch_walls_edit("wall_edit", |db, snaps| {
            snaps.save(db, "wall", "id", id)?;
            db.execute(
                // a stored top polyline pins the 3D top, so a new height must replace it
                "UPDATE wall SET bx=?2, by=?3, wtype=?4, height=?5, \
                 top_profile=CASE WHEN ABS(?5-height)>1e-9 THEN NULL ELSE top_profile END, \
                 top_roof=CASE WHEN ABS(?5-height)>1e-9 THEN NULL ELSE top_roof END, \
                 top_core=CASE WHEN ABS(?5-height)>1e-9 THEN NULL ELSE top_core END, \
                 top_auto=CASE WHEN ABS(?5-height)>1e-9 AND top_auto=1 THEN 2 ELSE top_auto END \
                 WHERE id=?1",
                (id, s.b.0, s.b.1, s.wall_type, s.height),
            )?;
            // a dragged end re-trims a wall the roof rule leads (same undo step)
            ddc_model::sync_roof_walls(db, snaps, &[id])?;
            Ok(())
        })?;
        self.reload()
    }

    /// Swing side and hinges of the selected door: one undoable model operation.
    pub fn set_opening_flip(&mut self, id: i64, flip: Flip) {
        let Some(o) = self.opening(id).copied() else {
            return;
        };
        let after = OpSpec {
            flip,
            ..self.spec_of(&o)
        };
        match self.update_opening_op(id, after) {
            Ok(st) => {
                let sev = if st.is_some() { Severity::Warning } else { Severity::Info };
                let msg = stale_join(crate::tr("edst.opening_changed", &[("id", &id.to_string())]), st.as_deref());
                self.say(sev, msg);
            }
            Err(e) => self.say(Severity::Refusal, crate::tr("edst.not_changed", &[("e", &e.to_string())])),
        }
    }

    /// Dragging the selected opening in select mode; true means the event was consumed.
    pub(crate) fn handle_drag(&mut self, e: Event) -> bool {
        match e {
            Event::Press(p) if self.drag.is_none() => {
                let Some(o) = self.opening_at(p).and_then(|id| self.opening(id)).copied() else {
                    return false;
                };
                if !self.selection.contains(&Sel::Opening(o.id)) {
                    return false;
                }
                let Some(host) = self.host_of(o.wall) else {
                    return false;
                };
                self.drag = Some(Drag {
                    id: o.id,
                    host,
                    ty: self.spec_of(&o).ty,
                    from: o.along,
                    along: o.along,
                    grab: host.along_of(p) - (o.along + self.spec_of(&o).ty.width / 2.0),
                    moved: false,
                    error: None,
                });
                true
            }
            Event::Move(p) => {
                let (r, off, step) = (self.tool.radius, self.otool.end_offset, self.otool.step);
                let Some(d) = self.drag.as_mut() else {
                    return false;
                };
                let s = d.host.along_of(p) - d.grab;
                d.moved = true;
                d.along = snap_along(d.host.len(), s, d.ty.width, off, step, r).0;
                d.error = validate(&d.host, &self.openings, Some(d.id), d.along, &d.ty).err();
                true
            }
            Event::Key(Key::Escape) if self.drag.is_some() => {
                self.drag = None;
                self.say(crate::status::Severity::Info, crate::tr("edit_status.ed.move_cancelled", &[]));
                true
            }
            Event::Release(p) if self.drag.is_some() => {
                if self.drag.as_ref().is_some_and(|d| d.moved) {
                    self.handle_drag(Event::Move(p));
                }
                let Some(d) = self.drag.take() else {
                    return true;
                };
                if !d.moved {
                    return true;
                }
                if let Some(e) = d.error {
                    self.say(crate::status::Severity::Refusal, crate::tr("edit_status.ed.not_moved", &[("e", e.to_string().as_str())]));
                } else if (d.along - d.from).abs() > 1e-12 {
                    if let Some(o) = self.opening(d.id).copied() {
                        let after = OpSpec {
                            along: d.along,
                            ..self.spec_of(&o)
                        };
                        match self.update_opening_op(d.id, after) {
                            Ok(st) => {
                                let sev = if st.is_some() { Severity::Warning } else { Severity::Info };
                                let msg = stale_join(crate::tr("edst.opening_moved", &[("id", &d.id.to_string())]), st.as_deref());
                                self.say(sev, msg);
                            }
                            Err(e) => self.say(Severity::Refusal, crate::tr("edst.not_moved", &[("e", &e.to_string())])),
                        }
                    }
                }
                true
            }
            _ => self.drag.is_some(),
        }
    }

    fn strip_of(&self, wtype: i64, a: V2, b: V2, base: f64, height: f64) -> Strip {
        let flags = self.model.wall_layer_flags(wtype).unwrap_or_default();
        self.strip_flags(&flags, wtype, a, b, base, height)
    }

    /// Strips of `ws`: the layer flags are read once per wall type, not once per wall.
    pub(super) fn strips(&self, ws: &[Wall]) -> Vec<Strip> {
        let mut flags: std::collections::HashMap<i64, Vec<(bool, u8)>> = Default::default();
        ws.iter()
            .map(|w| {
                let f = flags.entry(w.wall_type).or_insert_with(|| {
                    self.model.wall_layer_flags(w.wall_type).unwrap_or_default()
                });
                self.strip_flags(f, w.wall_type, w.a, w.b, w.base_offset, w.height)
            })
            .collect()
    }

    fn strip_flags(
        &self,
        flags: &[(bool, u8)],
        wtype: i64,
        a: V2,
        b: V2,
        base: f64,
        height: f64,
    ) -> Strip {
        let layers: Vec<Layer> = self
            .type_by_id(wtype)
            .map_or(vec![0.1], |t| t.layers.clone())
            .into_iter()
            .enumerate()
            .map(|(k, t)| {
                Layer::with_priority(t, flags.get(k).map_or(Layer::DEFAULT_PRIORITY, |f| f.1))
            })
            .collect();
        let total: f64 = layers.iter().map(|l| l.thickness).sum();
        Strip {
            a,
            b,
            layers,
            offset0: -total / 2.0,
            base,
            height,
        }
    }

    fn strip(&self, w: &Wall) -> Strip {
        self.strip_of(w.wall_type, w.a, w.b, w.base_offset, w.height)
    }

    /// Wall joint edits (in `walls` order).
    pub fn join_prefs(&self) -> Vec<ddc_geom::layered::JoinPrefs> {
        let ids: Vec<i64> = self.walls.iter().map(|w| w.id).collect();
        self.model
            .join_prefs(&ids)
            .unwrap_or_else(|_| vec![[None, None]; ids.len()])
    }

    /// Bodies of wall `i`'s projecting profiles (a groove is only in quantities) and of the
    /// tiers of a composite wall; world coordinates.
    fn edit_meshes(&self, i: usize, s: &Strip) -> (Vec<ddc_geom::mesh::Mesh>, bool) {
        use ddc_geom::wall_edit::{FacadeHole, SweepOn};
        let tol = Tol::default();
        let w = &self.walls[i];
        let mut out = vec![];
        let len = s.length();
        if len <= 1e-9 || w.bulge != 0.0 {
            return (out, false);
        }
        let (d, n) = (s.dir(), s.normal());
        let total: f64 = s.layers.iter().map(|l| l.thickness).sum();
        // a profile in the plane (normal offset, z), extruded along the axis over [s0, s1]
        let push = |out: &mut Vec<ddc_geom::mesh::Mesh>, poly: &[V2], s0: f64, s1: f64, z0: f64| {
            let Ok(p) = ddc_geom::Profile::new(poly.to_vec(), vec![], tol) else {
                return;
            };
            let Ok(mut m) = ddc_geom::Solid::extrude(&p, s1 - s0, tol).triangulate() else {
                return;
            };
            for q in m.positions.iter_mut() {
                let xy = s.a + d * (s0 + q.z) + n * q.x;
                *q = ddc_geom::v3(xy.x, xy.y, s.base + z0 + q.y);
            }
            // (normal, z, along) = n × up · d: with negative orientation the winding is reversed
            if n.cross(d) > 0.0 {
                for t in m.triangles.iter_mut() {
                    t.swap(1, 2);
                }
            }
            out.push(m);
        };
        let holes: Vec<FacadeHole> = self
            .openings
            .iter()
            .filter(|o| o.wall == w.id)
            .map(|o| (o.along, o.along + o.width, o.sill, o.sill + o.height))
            .collect();
        for (_, sw) in self.model.wall_sweeps(w.id).unwrap_or_default() {
            if sw.reveal {
                continue;
            }
            let on = SweepOn {
                sweep: &sw,
                len,
                h: s.height,
                holes: &holes,
            };
            let (a, b) = on.run();
            if b - a <= 1e-9 {
                continue;
            }
            let face = if sw.left {
                s.offset0 + total
            } else {
                s.offset0
            };
            let sign = if sw.left { -1.0 } else { 1.0 };
            // u > 0 is into the wall; in world space the offset = face + sign·u
            let sec: Vec<V2> = on
                .section()
                .iter()
                .map(|q| ddc_geom::v2(face + sign * q.x.min(0.0), q.y))
                .collect();
            push(&mut out, &sec, a, b, 0.0);
        }
        let tiers = self.model.wall_stack_tiers(w.id, s.height).ok().flatten();
        let stacked = tiers.is_some();
        for (t, _) in tiers.unwrap_or_default() {
            let o = t.offset;
            let r = vec![
                ddc_geom::v2(o - t.thickness / 2.0, t.z0),
                ddc_geom::v2(o + t.thickness / 2.0, t.z0),
                ddc_geom::v2(o + t.thickness / 2.0, t.z1),
                ddc_geom::v2(o - t.thickness / 2.0, t.z1),
            ];
            push(&mut out, &r, 0.0, len, 0.0);
        }
        (out, stacked)
    }

    /// Draft layer outlines joined with existing walls.
    pub fn preview_plan(&self) -> Vec<Vec<V2>> {
        let Some(t) = self.current_type() else {
            return vec![];
        };
        let draft = self.draft_preview();
        if draft.is_empty() {
            return vec![];
        }
        let near = |a: V2, b: V2| {
            draft.iter().any(|&(p, q)| {
                [p, q]
                    .iter()
                    .any(|&x| (closest_on_segment(x, a, b).0 - x).len() < 1.0)
            })
        };
        let mut strips: Vec<Strip> = draft
            .iter()
            .map(|&(a, b)| self.strip_of(t.id, a, b, 0.0, self.height))
            .collect();
        let n = strips.len();
        let mut bulges = vec![0.0; n];
        let mut prefs = vec![[None, None]; n];
        let all = self.join_prefs();
        for (k, w) in self
            .walls
            .iter()
            .enumerate()
            .filter(|(_, w)| near(w.a, w.b))
        {
            strips.push(self.strip(w));
            bulges.push(w.bulge);
            prefs.push(all.get(k).copied().unwrap_or([None, None]));
        }
        footprints(&strips, &bulges, &prefs, n)
            .into_iter()
            .flatten()
            .collect()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// A tool event; a commit writes one model operation. Routed by the interaction
    /// spine (`interact::Interaction::update`): a tool session, or selection without one.
    pub fn handle(&mut self, e: Event) {
        crate::interact::Interaction::update(self, e.into());
    }

    /// Input from a view adapter (plan, 3D, section): the same route as `handle`, with the
    /// pointer in model coordinates.
    pub fn input(&mut self, i: crate::interact::Input) {
        // a hosted family sees the element under the pointer
        let i = match self.opening_mode {
            true => i.map_pointer(|p| Some(crate::interact::host::attach(self, p))),
            false => Some(i),
        };
        if let Some(i) = i {
            crate::interact::Interaction::update(self, i);
        }
    }

    /// The wall tool and select mode, last of the selection hooks: Esc out of an idle tool, line pick, then the
    /// wall state machine and its outcome.
    pub(crate) fn handle_wall_tool(&mut self, e: Event) {
        if matches!(e, Event::Key(Key::Escape)) && self.esc_leaves_tool() {
            self.set_mode(Mode::Select);
            return;
        }
        if self.wall_mode() == Mode::PickLine
            && self.tool.lp.kind != crate::line_pick::LineKind::Wall
        {
            return crate::line_pick::handle(self, e);
        }
        match self.tool.handle_in(self.ix.draft_mut(), e) {
            Outcome::Nothing => {}
            Outcome::Error(m) if m == "wall too short" => self.refuse(crate::refusals::wall_too_short()),
            Outcome::Error(m) => self.status = Some(m),
            Outcome::Pick(p) => self.pick(p),
            Outcome::Arc { a, b, bulge } if self.tool.curtain => {
                self.add_curtains(&[(a, b, bulge)])
            }
            Outcome::Commit(segs) if self.tool.curtain => {
                let segs: Vec<_> = segs.into_iter().map(|(a, b)| (a, b, 0.0)).collect();
                self.add_curtains(&segs)
            }
            Outcome::Spline(_) if self.tool.curtain => {
                self.status = Some(crate::tr("spline.wt.no_curtain", &[]))
            }
            Outcome::Arc { a, b, bulge } => self.commit_arc_wall(a, b, bulge),
            Outcome::Spline(pts) => self.commit_spline_wall(&pts),
            Outcome::Commit(segs) => self.commit_walls_or_pinned(segs),
        }
    }

    /// Walls of the current type along segments: one undo command.
    /// Goes through `run_ops` so the journal holds the forward envelope (actor User);
    /// `Cmd::Add` only when the envelope is refused, with the reason in the status.
    pub fn commit_walls(&mut self, segs: Vec<(V2, V2)>) {
        let Some(t) = self.current_type() else {
            self.say(crate::status::Severity::Refusal, crate::tr("edit_status.ed.no_wall_type", &[]));
            return;
        };
        let (level, height, wall_type) = (self.level, self.height, t.id);
        let mut refused = None;
        if let Some(env) = self.wall_envelope(&segs) {
            let steps = self.undo.len();
            match self.run_ops(env) {
                Ok(r) => {
                    let n = r.created.len().to_string();
                    self.status = Some(crate::tr("edst.walls_added", &[("n", &n)]));
                    return;
                }
                // the step is recorded and only the reload failed: a fallback would add twice
                Err(e) if self.undo.len() > steps => {
                    self.status =
                        Some(crate::tr("edst.walls_not_built", &[("e", &e.to_string())]));
                    return;
                }
                Err(e) => refused = Some(e.to_string()),
            }
        }
        let g = Gesture {
            bulges: vec![],
            specs: segs
                .into_iter()
                .map(|(a, b)| WallSpec {
                    a: p2(a),
                    b: p2(b),
                    level,
                    base_offset: 0.0,
                    height,
                    wall_type,
                    offset0: None,
                })
                .collect(),
        };
        match self.apply_add(&g) {
            Ok(ids) => {
                let n = ids.len().to_string();
                match &refused {
                    Some(why) => self.say(
                        Severity::Warning,
                        crate::tr("edst.walls_added_unjournaled", &[("n", &n), ("e", why)]),
                    ),
                    None => self.say(Severity::Info, crate::tr("edst.walls_added", &[("n", &n)])),
                }
                self.push(Cmd::Add(g, ids));
            }
            Err(e) => self.say(crate::status::Severity::Error, crate::tr("edit_status.ed.walls_not_built", &[("e", e.to_string().as_str())])),
        }
    }

    fn apply_add(&mut self, g: &Gesture) -> Result<Vec<i64>, ddc_model::Error> {
        let ids = match (g.specs.as_slice(), g.bulges.as_slice()) {
            ([s], [b]) if *b != 0.0 => vec![self.model.add_arc_wall(s, *b)?],
            _ => {
                let ids = self.model.add_walls(&g.specs)?;
                for (id, b) in ids.iter().zip(&g.bulges) {
                    if *b != 0.0 {
                        self.model.set_wall_bulge(*id, *b)?;
                    }
                }
                ids
            }
        };
        self.reload()?;
        Ok(ids)
    }

    fn pick(&mut self, p: V2) {
        self.selection = crate::pick_rules::target_at(self, p).into_iter().collect();
    }

    /// What a click at `p` selects, among items `ok` admits. Pure: hover, measure and the
    /// context menu ask the same question without touching the selection.
    pub fn pick_at(&self, p: V2, ok: &dyn Fn(Sel) -> bool) -> Option<Sel> {
        if let Some(id) = self.opening_at(p).filter(|&id| ok(Sel::Opening(id))) {
            return Some(Sel::Opening(id));
        }
        let f = |c: Option<(Sel, f64)>| c.filter(|c| ok(c.0));
        let far = |s: Sel| (s, f64::MAX);
        let cw = f(self.cw_pick(p).filter(|c| !matches!(c.0, Sel::Slab(_))))
            .or_else(|| f(self.struct_pick(p)))
            .or_else(|| f(self.mass_pick(p).map(|id| far(Sel::Mass(id)))))
            .or_else(|| f(self.body_pick(p, false).map(|(k, id)| far(Sel::Body(k, id)))))
            .or_else(|| f(self.hosted_pick(p)))
            .or_else(|| f(self.room_pick(p).map(|id| far(Sel::Room(id)))))
            .or_else(|| {
                f(self
                    .ceiling_pick(p)
                    .map(|id| far(Sel::Body(bodies::BodyKind::Ceiling, id))))
            })
            // terrain and pads cover whole rooms: picked only where nothing else is
            .or_else(|| f(self.body_pick(p, true).map(|(k, id)| far(Sel::Body(k, id)))))
            .or_else(|| f(self.cw_pick(p)))
            // the terrain under the whole site: only where nothing else is
            .or_else(|| f(self.topo_pick(p).map(far)));
        let cls = crate::plan_range::classes(self);
        let hit = self
            .walls
            .iter()
            .enumerate()
            // distance first: only the few walls under the cursor reach the (costlier) view
            // and filter predicates, so a hover over a big plan stays cheap
            .filter_map(|(i, w)| {
                let half = self
                    .type_by_id(w.wall_type)
                    .map_or(0.05, |t| t.thickness() / 2.0);
                let d = (closest_on_segment(p, w.a, w.b).0 - p).len() - half;
                (d <= self.tool.radius).then_some((i, w.id, d))
            })
            // a wall outside the view range is not drawn, so it is not picked either
            .filter(|(i, id, _)| {
                cls.wall(*i) != crate::plan_range::RangeClass::Hidden && ok(Sel::Wall(*id))
            })
            .map(|(_, id, d)| (id, d))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match (hit, cw) {
            (Some(h), Some((c, d))) if d < h.1 => Some(c),
            (Some(h), _) => Some(Sel::Wall(h.0)),
            (None, Some((c, _))) => Some(c),
            (None, None) => None,
        }
    }

    /// The mass whose section contains the plan point (the last drawn is on top).
    fn mass_pick(&self, p: V2) -> Option<i64> {
        self.mass_plan
            .iter()
            .rev()
            .find(|(_, rs)| crate::mass_tools::inside_rings(rs, p))
            .map(|m| m.0)
    }

    /// Full wall specs from the model (any floor).
    fn wall_specs(&self, ids: &[i64]) -> Result<Vec<WallSpec>, ddc_model::Error> {
        let mut st = self.model.sql().prepare(
            "SELECT ax,ay,bx,by,level,base_offset,height,wtype,offset0 FROM wall WHERE id=?1",
        )?;
        ids.iter()
            .map(|&id| {
                Ok(st.query_row([id], |r| {
                    Ok(WallSpec {
                        a: (r.get(0)?, r.get(1)?),
                        b: (r.get(2)?, r.get(3)?),
                        level: r.get(4)?,
                        base_offset: r.get(5)?,
                        height: r.get(6)?,
                        wall_type: r.get(7)?,
                        offset0: r.get(8)?,
                    })
                })?)
            })
            .collect()
    }

    pub fn undo(&mut self) {
        self.undo_kept.clear();
        self.grip.last = None;
        // an undone nudge must not be merged into by the next arrow press
        self.grip.burst = None;
        crate::roof_edge_tools::reset();
        if self.drag.take().is_some() {
            // the opening slide was the grip session's: end it with the drag
            crate::interact::sessions::grip::stop(self);
            return;
        }
        if crate::solid_tools::undo_draft(self)
            || crate::annot_text::undo_draft(self)
        {
            return;
        }
        if self.drawing() {
            self.reset_draft();
            return;
        }
        if crate::topo::undo_point(self) {
            return;
        }
        let Some((entry, c)) = self.pop_undo() else {
            self.say(crate::status::Severity::Refusal, crate::tr("edit_status.ed.nothing_undo", &[]));
            return;
        };
        if let Cmd::Underlay(s) = &c {
            self.underlay_due.push(s.clone());
            self.status = Some(crate::tr("dlg.dw.st.undone", &[]));
            self.put_redo(entry, c);
            return;
        }
        if let Cmd::SectionBox(before, _) = &c {
            self.box_due = Some(*before);
            let done = hist::done_text(false, &hist::step_name(&c));
            self.say(crate::status::Severity::Info, done);
            self.status_cmd = Some("edit.redo");
            self.status_entry = Some(entry);
            self.put_redo(entry, c);
            return;
        }
        let n = match &c {
            Cmd::Ext(r) => r.n,
            Cmd::Ops(_, n) => *n as usize,
            _ => 1,
        };
        let mut kept = vec![];
        let h = self.model.change_head();
        match (0..n)
            .try_for_each(|_| {
                self.model.undo()?;
                kept.extend(self.model.undo_kept());
                Ok::<(), ddc_model::Error>(())
            })
            .and_then(|_| self.reload_since(h))
        {
            Ok(()) => {
                let done = hist::done_text(false, &hist::step_name(&c));
                self.say(crate::status::Severity::Info, stage::undo_status(done, &kept));
                self.status_cmd = Some("edit.redo");
                self.status_entry = Some(entry);
                self.undo_kept = kept;
                self.put_redo(entry, c);
                crate::solid_tools::on_model_reset(self);
                crate::annot_text::on_model_reset();
            }
            Err(e) => {
                self.say(crate::status::Severity::Error, crate::tr("edit_status.ed.undo_failed", &[("e", e.to_string().as_str())]));
                self.put_undo(entry, c);
            }
        }
    }

    pub fn redo(&mut self) {
        self.grip.last = None;
        self.grip.burst = None;
        crate::roof_edge_tools::reset();
        let Some((entry, c)) = self.pop_redo() else {
            self.say(crate::status::Severity::Refusal, crate::tr("edit_status.ed.nothing_redo", &[]));
            return;
        };
        let mut reload_failed = None;
        // a committed import whose view refresh failed: redone with a warning, never "failed"
        let mut redo_stale: Option<String> = None;
        let head = self.model.change_head();
        let r = match &c {
            Cmd::Add(g, old) => self.apply_add(g).map(|new| {
                // replaying the gesture yields new ids: remap them in the remaining commands
                let map = |id: &mut i64| {
                    if let Some(k) = old.iter().position(|o| o == id) {
                        *id = new[k];
                    }
                };
                for cmd in &mut self.redo {
                    match cmd {
                        Cmd::Items(items) => delete::remap(items, "wall", map),
                        Cmd::AddOpening(s, _) => map(&mut s.wall),
                        Cmd::UpdateOpening(_, a, b) => {
                            map(&mut a.wall);
                            map(&mut b.wall);
                        }
                        Cmd::EditWall(id, ..) => map(id),
                        _ => {}
                    }
                }
                Cmd::Add(g.clone(), new)
            }),
            Cmd::AddOpening(s, old) => self.apply_add_opening(s).map(|new| {
                let map = |id: &mut i64| {
                    if id == old {
                        *id = new;
                    }
                };
                for cmd in &mut self.redo {
                    match cmd {
                        Cmd::UpdateOpening(id, ..) => map(id),
                        Cmd::Items(items) => delete::remap(items, "opening", map),
                        _ => {}
                    }
                }
                Cmd::AddOpening(*s, new)
            }),
            Cmd::UpdateOpening(id, before, after) => self
                .model
                .update_opening_with(*id, &after.model(), after.swing())
                .and_then(|_| self.reload())
                .map(|_| Cmd::UpdateOpening(*id, *before, *after)),
            Cmd::Items(items) => self.redo_items(items).map(|failed| {
                reload_failed = failed;
                Cmd::Items(items.clone())
            }),
            Cmd::EditWall(id, before, after) => self
                .write_wall(*id, after)
                .map(|_| Cmd::EditWall(*id, before.clone(), after.clone())),
            Cmd::Import(input, frames, tidy) => {
                match self.run_import(input, frames, *tidy, |e| e.reload()) {
                    Ok((_, st)) => {
                        redo_stale = st;
                        Ok(Cmd::Import(input.clone(), frames.clone(), *tidy))
                    }
                    Err(e) => Err(ddc_model::Error::Invalid(e)),
                }
            }
            Cmd::Xlsx(edits) => self
                .model
                .apply_edits(edits)
                .and_then(|_| self.reload())
                .map(|_| Cmd::Xlsx(edits.clone())),
            Cmd::Op(f) => self
                .model
                .step(|m| (f.0)(m))
                .and_then(|_| self.reload_since(head))
                .map(|_| Cmd::Op(f.clone())),
            Cmd::Ops(env, _) => self.replay_ops(env),
            Cmd::Ext(r) => r
                .apply(&mut self.model)
                .and_then(|r| self.reload().map(|_| Cmd::Ext(r))),
            Cmd::Underlay(s) => {
                self.underlay_due.push(s.clone());
                Ok(Cmd::Underlay(s.clone()))
            }
            Cmd::SectionBox(before, after) => {
                self.box_due = Some(*after);
                Ok(Cmd::SectionBox(*before, *after))
            }
            other => self
                .sec_redo(other)
                .or_else(|| self.level_redo(other))
                .or_else(|| self.cw_redo(other))
                .expect("curtain/slab/view command"),
        };
        match r {
            Ok(done) => {
                if reload_failed.is_none() {
                    self.status_cmd = Some("edit.undo");
                    self.status_entry = Some(entry);
                }
                let s = match reload_failed {
                    Some(e) => crate::tr("elem.reload_failed", &[("e", &e.to_string())]),
                    None => hist::done_text(true, &hist::step_name(&done)),
                };
                self.status = Some(stale_join(s, redo_stale.as_deref()));
                self.put_undo(entry, done);
            }
            Err(e) => {
                self.say(crate::status::Severity::Error, crate::tr("edit_status.ed.redo_failed", &[("e", e.to_string().as_str())]));
                self.put_redo(entry, c);
            }
        }
    }

    /// An edit operation as one undo step; if it fails the model is unchanged and no step is recorded.
    pub fn run_op(&mut self, f: OpFn) -> Result<(), ddc_model::Error> {
        let h = self.model.change_head();
        let watch = ddc_model::perf::span("editor.run_op.room_watch");
        let dups = crate::room_watch::snapshot(&self.model);
        drop(watch);
        ddc_model::take_follow_problems();
        // a refused op undoes all it wrote; its journal records merge into one undo step
        let step = ddc_model::perf::span("editor.run_op.model");
        let ((), wrote) = self.model.step(|m| (f.0)(m))?;
        drop(step);
        let _after = ddc_model::perf::span("editor.run_op.reload");
        // the op is in the model journal: the undo entry goes first, a failed reload keeps it;
        // no record: no undo step, which would undo an earlier edit
        if wrote {
            self.push(Cmd::Op(f));
        }
        let mut note = None;
        crate::mep_follow::report(&mut note);
        crate::room_watch::note(&mut note, &self.model, &dups);
        let r = if crate::mep3d::only_mep(&self.model, h) {
            self.revision += 1;
            // fittings follow their runs
            self.struct_reload();
            Ok(())
        } else {
            self.reload_since(h)
        };
        // set after the reload so nothing it does can drop the note
        if note.is_some() {
            self.status = note;
        }
        self.settle_status();
        r
    }

    /// Takes back the AI step written after op `top` when its check failed: the model goes back
    /// and the step leaves the history, so Redo never offers it again.
    pub fn take_back_ai(&mut self, top: i64) -> Result<(), String> {
        let h = self.model.change_head();
        let n = self.model.rollback_to(top).map_err(|e| e.to_string())?;
        if n > 0 && matches!(self.undo.last(), Some(Cmd::Op(_))) {
            self.pop_undo();
        }
        self.reload_since(h).map_err(|e| e.to_string())
    }

    /// Model tool record: apply, re-read, one undo step.
    pub fn apply_ext(&mut self, r: crate::tools::Rec) -> Result<(), ddc_model::Error> {
        let r = r.apply(&mut self.model)?;
        // the ops are in the model journal: the undo entry goes first, a failed reload keeps it;
        // no model operation (nothing changed): no undo step that would undo an earlier edit
        if r.n > 0 {
            self.push(Cmd::Ext(r));
        }
        self.reload()
    }

    /// Edit batch from an XLSX workbook (after the preview): one model operation, one undo.
    pub fn apply_xlsx(
        &mut self,
        edits: Vec<ddc_model::Edit>,
        summary: &str,
    ) -> Result<(), String> {
        // a pinned element does not move from a book edit: skipped, and said so
        let (edits, pinned) = self.model.split_pinned(&edits).map_err(|e| e.to_string())?;
        // a locked parameter value is kept: the edit is dropped and counted
        let (edits, locked) = self.model.split_locked(&edits).map_err(|e| e.to_string())?;
        if edits.is_empty() {
            self.say(crate::status::Severity::Info, crate::tr("edit_status.ed.no_changes", &[]));
            self.note_pinned(pinned.len());
            self.note_locked(locked.len());
            return Ok(());
        }
        let dups = crate::room_watch::snapshot(&self.model);
        if let Err(e) = self.model.apply_edits(&edits) {
            let e = e.to_string();
            self.status = Some(crate::tr("edst.spreadsheet_failed", &[("e", &e)]));
            return Err(e);
        }
        // the edits are in the model journal: the undo entry goes first, a failed reload keeps it
        self.push(Cmd::Xlsx(std::sync::Arc::new(edits)));
        let r = self.reload().map_err(|e| e.to_string());
        match &r {
            Ok(()) => {
                self.say(
                    Severity::Info,
                    crate::tr("edst.spreadsheet_imported", &[("summary", &summary.to_string())]),
                );
                self.note_pinned(pinned.len());
                crate::room_watch::note(&mut self.status, &self.model, &dups);
                self.note_locked(locked.len());
            }
            Err(e) => self.say(crate::status::Severity::Error, crate::tr("edit_status.ed.spreadsheet_failed", &[("e", e.to_string().as_str())])),
        }
        r
    }

    /// Like `apply_xlsx`, plus the cell edits of the family sheets: one operation list, one undo.
    pub fn apply_xlsx_props(
        &mut self,
        edits: Vec<ddc_model::Edit>,
        props: Vec<ddc_io::xlsx::PropEdit>,
        summary: &str,
    ) -> Result<(), String> {
        self.apply_xlsx_linked(edits, props, summary, None)
    }

    /// `apply_xlsx_props` that also remembers the rows it creates in `link`'s book. The record
    /// is written inside the redoable step, so a redo remembers the rows again.
    pub fn apply_xlsx_linked(
        &mut self,
        edits: Vec<ddc_model::Edit>,
        props: Vec<ddc_io::xlsx::PropEdit>,
        summary: &str,
        link: Option<std::sync::Arc<XlsxLink>>,
    ) -> Result<(), String> {
        if props.is_empty() && link.is_none() {
            return self.apply_xlsx(edits, summary);
        }
        let (edits, mut pinned) = self.model.split_pinned(&edits).map_err(|e| e.to_string())?;
        // family sheet rows obey the same pins as the main sheets
        let (props, pinned_props) = ddc_io::xlsx::split_pinned_props(&self.model, &props)?;
        for p in pinned_props {
            if !pinned.contains(&p) {
                pinned.push(p);
            }
        }
        let (edits, locked) = self.model.split_locked(&edits).map_err(|e| e.to_string())?;
        let (edits, props) = (std::sync::Arc::new(edits), std::sync::Arc::new(props));
        let rec = crate::tools::Rec::new("xlsx", move |m| {
            let before = link.as_ref().map(|_| crate::shell::xlsx_links::snapshot(m));
            let mut n = 0;
            if !edits.is_empty() {
                m.apply_edits(&edits)?;
                n = 1;
            }
            // a failing props pass also takes back the edits: Rec::apply is one model step
            let k = ddc_io::xlsx::apply_props(m, &props).map_err(ddc_model::Error::Invalid)?;
            if let (Some(l), Some(b)) = (&link, &before) {
                if n + k > 0 {
                    crate::shell::xlsx_links::record(m, &l.book, b, &l.rows);
                }
            }
            Ok(n + k)
        });
        let dups = crate::room_watch::snapshot(&self.model);
        let r = self.apply_ext(rec).map_err(|e| e.to_string());
        self.status = Some(match &r {
            Ok(()) => crate::tr("elem.spreadsheet.applied", &[("what", summary)]),
            Err(e) => crate::tr("elem.spreadsheet.failed", &[("e", e.as_str())]),
        });
        if r.is_ok() {
            self.note_pinned(pinned.len());
            crate::room_watch::note(&mut self.status, &self.model, &dups);
            self.note_locked(locked.len());
        }
        r
    }

    /// Takes back the XLSX step written after op `top` when its check failed: the model goes
    /// back and the step leaves the history, so Redo never offers it again.
    pub fn take_back_xlsx(&mut self, top: i64) -> Result<(), String> {
        let h = self.model.change_head();
        let n = self.model.rollback_to(top).map_err(|e| e.to_string())?;
        if n > 0 && matches!(self.undo.last(), Some(Cmd::Ext(_) | Cmd::Xlsx(_))) {
            self.pop_undo();
        }
        self.reload_since(h).map_err(|e| e.to_string())
    }

    /// Status tail: how many locked parameter values a batch left alone.
    fn note_locked(&mut self, n: usize) {
        if n > 0 {
            let t = crate::tr("locked_values.kept", &[("n", &n.to_string())]);
            let s = self.status.take().unwrap_or_default();
            self.status = Some(if s.is_empty() { t } else { format!("{s} · {t}") });
        }
    }

    /// Status tail: how many pinned elements a workbook import left alone.
    fn note_pinned(&mut self, n: usize) {
        if n > 0 {
            let t = crate::tr("edst.spreadsheet_pinned", &[("n", &n.to_string())]);
            let s = self.status.take().unwrap_or_default();
            let m = format!("{s} · {t}");
            // keep the severity said for the text this note extends
            if let Some((_, said)) = self.status_sev.as_mut().filter(|(_, said)| *said == s) {
                *said = m.clone();
            }
            self.status = Some(m);
        }
    }

    /// Applies frames collected by the background thread (`bridge::collect_trusted`): one undo.
    /// Redo applies the same frames again; the bridge is not run again.
    pub fn apply_import(
        &mut self,
        input: &std::path::Path,
        frames: Vec<ddc_io::bridge::proto::Frame>,
    ) -> Result<ddc_io::bridge::Report, String> {
        self.apply_import_with(input, frames, |e| e.reload())
    }

    fn apply_import_with(
        &mut self,
        input: &std::path::Path,
        frames: Vec<ddc_io::bridge::proto::Frame>,
        reload: impl FnOnce(&mut Self) -> Result<(), ddc_model::Error>,
    ) -> Result<ddc_io::bridge::Report, String> {
        self.import_shared(input, std::sync::Arc::new(frames), true, reload)
    }

    /// Frames arrive from the worker already shared: the history keeps that same `Arc`, nothing
    /// is copied. `tidy` also removes the untouched default level in the same model operation:
    /// either way the Add is one operation and one undo.
    pub fn apply_import_arc(
        &mut self,
        input: &std::path::Path,
        frames: std::sync::Arc<Vec<ddc_io::bridge::proto::Frame>>,
        tidy: bool,
    ) -> Result<ddc_io::bridge::Report, String> {
        self.import_shared(input, frames, tidy, |e| e.reload())
    }

    fn import_shared(
        &mut self,
        input: &std::path::Path,
        frames: std::sync::Arc<Vec<ddc_io::bridge::proto::Frame>>,
        tidy: bool,
        reload: impl FnOnce(&mut Self) -> Result<(), ddc_model::Error>,
    ) -> Result<ddc_io::bridge::Report, String> {
        let r = self.run_import(input, &frames, tidy, reload);
        match &r {
            Ok((rep, stale)) => {
                // committed: the undo entry always exists, a stale view is only a warning
                self.push(Cmd::Import(input.into(), frames, tidy));
                self.set_mode(Mode::Select);
                let mut s = crate::import::summary(&self.model, rep);
                if let Some(e) = stale {
                    s = format!("{s} · {}", crate::tr("edst.import_view_stale", &[("e", e)]));
                }
                if let Some(w) = import_check_note(&self.model) {
                    s = format!("{s} · {w}");
                }
                self.status = Some(s);
            }
            Err(e) => self.say(
                crate::status::Severity::Error,
                crate::tr("edit_status.ed.import_failed", &[("e", e.as_str())]),
            ),
        }
        r.map(|(rep, _)| rep)
    }

    /// Import and a view of the result: one model operation (one journal record, one undo); with
    /// `tidy` the untouched default level goes in it too. The imported level with the most walls
    /// becomes active. Also returns the view refresh error, if any: the import itself is
    /// committed then.
    fn run_import(
        &mut self,
        input: &std::path::Path,
        frames: &[ddc_io::bridge::proto::Frame],
        tidy: bool,
        reload: impl FnOnce(&mut Self) -> Result<(), ddc_model::Error>,
    ) -> Result<(ddc_io::bridge::Report, Option<String>), String> {
        let old = self.level;
        let mut failure = None;
        let res = self.model.atomic("wall_import_bridge", |m| {
            let r = ddc_io::bridge::import_frames_kept(m, input, frames).map_err(|e| {
                let msg = e.to_string();
                failure = Some(msg.clone());
                ddc_model::Error::Invalid(msg)
            })?;
            let first = imported_level(m);
            // only the untouched default level goes: a level the user made (or renamed) stays;
            // refused while anything stands on it (its own savepoint rolls back): it stays
            if let Some(l) = first {
                if tidy && l != old && default_level(m, old) {
                    let _ = m.delete_level(old);
                }
            }
            Ok((r, first))
        });
        let (r, first) = match (res, failure) {
            (Ok(v), _) => v,
            (Err(_), Some(msg)) => return Err(msg),
            (Err(e), None) => return Err(e.to_string()),
        };
        if let Some(l) = first {
            self.level = l;
        }
        let stale = reload(self).err().map(|e| e.to_string());
        Ok((r, stale))
    }

    /// Esc with nothing left to cancel in a drawing tool goes back to Select.
    pub fn esc_leaves_tool(&self) -> bool {
        // an armed pick set is cancelled first (line_pick), the next Esc leaves the tool
        let armed = self.wall_mode() == Mode::PickLine && self.tool.lp.pick_chain;
        // a model tool with points placed steps back one point first
        let placing = self.model_tool().is_some_and(|t| !t.pts.is_empty());
        self.tool_armed() && self.wall_idle() && !armed && !placing
    }

    pub fn set_mode(&mut self, m: Mode) {
        self.tool.profile = crate::snap::SnapProfile::ALL;
        self.tool.pin_armed = None;
        crate::topo::reset(&mut self.topo);
        crate::solid_tools::cancel();
        crate::annot_text::cancel(self);
        self.opening_mode = false;
        self.tool.curtain = false;
        self.drag = None;
        self.mass = None;
        self.vdrag = None;
        self.otool.reset();
        crate::interact::sessions::stop_bridges(self);
        self.ix.disarm();
        // a new mode starts a new draft (a running session keeps going with it)
        *self.ix.draft_mut() = Default::default();
        self.tool.set_mode(m);
        if m != Mode::Select {
            self.selection.clear();
            // a drawing tool replaces the pending roof-edge pick (it would win the scope)
            crate::roof_edge_tools::reset();
        }
        crate::interact::Interaction::refresh(self);
    }

    pub fn total_area(&self) -> f64 {
        self.walls.iter().map(|w| w.plan_area).sum()
    }

    pub fn total_volume(&self) -> f64 {
        self.walls.iter().map(|w| w.volume).sum()
    }

    /// Scene element of the selection: a wall is its index, an opening is `walls.len()` + the opening index.
    pub fn element_of(&self, s: Sel) -> Option<u32> {
        self.element_range(s).map(|r| r.0)
    }

    /// Scene elements of the selection [start, end): a curtain wall is its mullions and all panels.
    pub fn element_range(&self, s: Sel) -> Option<(u32, u32)> {
        if let Sel::Mass(id) = s {
            return crate::mass_tools::mass_range(&self.model, id);
        }
        if let Some(r) = self.big.range(s) {
            return Some(r);
        }
        if let Some(r) = self.cw_range(s) {
            return Some(r);
        }
        match s {
            Sel::Wall(id) => self.walls.iter().position(|w| w.id == id),
            Sel::Opening(id) => self
                .openings
                .iter()
                .position(|o| o.id == id)
                .map(|k| self.walls.len() + k),
            _ => None,
        }
        .map(|i| (i as u32, i as u32 + 1))
    }

    pub fn sel_of_element(&self, e: u32) -> Option<Sel> {
        if (crate::mass_tools::MASS_BASE..crate::mass_tools::MASS_END).contains(&e) {
            return crate::mass_tools::mass_of_element(&self.model, e).map(Sel::Mass);
        }
        if let Some(s) = self.big.sel_of(e) {
            return Some(s);
        }
        let i = e as usize;
        match self.walls.get(i) {
            Some(w) => Some(Sel::Wall(w.id)),
            None => match i.checked_sub(self.walls.len())? {
                k if k < self.openings.len() => Some(Sel::Opening(self.openings[k].id)),
                _ => self.cw_sel_of(e),
            },
        }
    }

    /// Selection from 3D: a click on a scene element or on empty space.
    pub fn select_element(&mut self, e: Option<u32>) {
        if crate::solid_tools::select_scene(self, e) {
            return;
        }
        self.selection = e.and_then(|e| self.sel_of_element(e)).into_iter().collect();
    }

    /// Scene element for highlighting in 3D.
    pub fn highlight(&self) -> Option<(u32, u32)> {
        if let Some(r) = crate::solid_tools::highlight(self) {
            return Some(r);
        }
        self.selection.first().and_then(|s| self.element_range(*s))
    }

    /// Scene elements of the whole selection, merged (`sel3d`); a picked solid when empty.
    pub fn highlight_all(&self) -> Vec<(u32, u32)> {
        if let Some(r) = crate::solid_tools::highlight(self) {
            return vec![r];
        }
        crate::sel3d::ranges_of(&self.selection, |s| self.element_range(s))
    }

    /// The model's 3D scene: layer bodies; an element is a wall index in `walls`, then openings.
    pub fn scene(&self) -> Scene {
        let (scene, issues) = self.scene_report();
        for m in issues {
            eprintln!("3D: {m}");
        }
        scene
    }

    /// Floor slabs of all levels as cutters, heights from the current level elevation;
    /// sloped ones as strips along the slope.
    fn slab_cutters(&self) -> Vec<ddc_geom::cut::Cutter> {
        let db = self.model.sql();
        let Ok(mut st) = db.prepare(
            "SELECT s.id, lv.elevation + s.offset - (SELECT elevation FROM level WHERE id=?1),              (SELECT SUM(thickness) FROM slab_type_layer WHERE stype=s.stype)              FROM slab s JOIN level lv ON lv.id=s.level ORDER BY s.id",
        ) else {
            return vec![];
        };
        let rows: Vec<(i64, f64, f64)> = st
            .query_map([self.level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rs| rs.filter_map(Result::ok).collect())
            .unwrap_or_default();
        let ring = |r: &[(f64, f64)]| r.iter().map(|&(x, y)| v2(x, y)).collect::<Vec<_>>();
        rows.into_iter()
            .filter_map(|(id, top, t)| {
                let rs: Vec<Vec<V2>> = self
                    .model
                    .slab_rings(id)
                    .ok()?
                    .iter()
                    .map(|r| ring(r))
                    .collect();
                let slope = self.model.slab_slope(id).ok().flatten();
                Some(cw::slab_cutters(&rs, top, t, slope))
            })
            .flatten()
            .collect()
    }

    /// The scene and a problem list: a piece that could not be cut is shown without cuts.
    pub fn scene_report(&self) -> (Scene, Vec<String>) {
        let tol = Tol::default();
        let strips: Vec<Strip> = self.walls.iter().map(|w| self.strip(w)).collect();
        let prefs = self.join_prefs();
        let lod = crate::view_gfx_ui::lod_3d(&self.model);
        let mut meshes = vec![];
        let mut issues = vec![];
        let ops: Vec<Vec<ddc_geom::layered::Opening>> = self
            .walls
            .iter()
            .map(|w| {
                self.openings
                    .iter()
                    .filter(|o| o.wall == w.id)
                    .map(|o| ddc_geom::layered::Opening {
                        along: o.along,
                        width: o.width,
                        sill: o.sill,
                        height: o.height,
                    })
                    .collect()
            })
            .collect();
        let span = ddc_model::perf::span("scene.cutters");
        let mut slabs = self.slab_cutters();
        slabs.extend(crate::roof_edge_tools::reveal_cutters(self));
        slabs.extend(crate::solid_tools::wall_cutters(self));
        drop(span);
        let span = ddc_model::perf::span("scene.cut");
        let wc = self
            .wall_cuts
            .get(&strips, &prefs, &ops, slabs, self.height, tol);
        drop(span);
        let span = ddc_model::perf::span("scene.wall_meshes");
        let (fps, fillers, cut) = (&wc.fps, &wc.fillers, &wc.cut);
        let bulges: Vec<f64> = self.walls.iter().map(|w| w.bulge).collect();
        let arcs = if bulges.iter().any(|&b| b != 0.0) {
            ddc_geom::arc::arc_walls_with(&strips, &bulges, &prefs, tol).ok()
        } else {
            None
        };
        for e in &cut.skipped {
            issues.push(crate::tr("edit_status.diag.cutter", &[("e", format!("{e:?}").as_str())]));
        }
        let mesh = |element: u32, tag: u32, m: &ddc_geom::mesh::Mesh| SceneMesh {
            element,
            tag,
            positions: m
                .positions
                .iter()
                .map(|p| [p.x as f32, p.y as f32, p.z as f32])
                .collect(),
            triangles: m.triangles.clone(),
        };
        // leaning walls: the join solved again at the top; each piece is lofted base→top
        let leans: Vec<ddc_geom::wall_lean::Lean> = self.walls.iter().map(|w| w.lean).collect();
        let lean_any = leans.iter().any(|l| !l.is_plain());
        let (ts, tb) = if lean_any {
            ddc_geom::wall_lean::top_strips(&strips, &bulges, &leans)
        } else {
            (vec![], vec![])
        };
        // arc walls: the top join solved with the concentric arcs of the top
        let top_arcs = match &arcs {
            Some(_) if lean_any => ddc_geom::arc::arc_walls_with(&ts, &tb, &prefs, tol).ok(),
            _ => None,
        };
        let top_fps: Vec<Vec<Vec<ddc_geom::Profile>>> = if lean_any {
            let tj = ddc_geom::layered::join_with(&ts, &prefs, tol).ok();
            ts.iter()
                .enumerate()
                .map(|(i, t)| {
                    let plain = plain_pieces(t);
                    let pc = tj.as_ref().map_or(&plain, |j| &j.pieces[i]);
                    layer_footprints(t, pc, i, tol).unwrap_or_default()
                })
                .collect()
        } else {
            vec![]
        };
        let texl = crate::mat_tex::layers(self);
        let mut finishes: HashMap<i64, Vec<Option<[u8; 3]>>> = HashMap::new();
        for (i, s) in strips.iter().enumerate() {
            let (extra, stacked) = self.edit_meshes(i, s);
            for m in &extra {
                meshes.push(mesh(i as u32, 4, m));
            }
            if stacked {
                continue;
            }
            let mut plain: Option<Vec<Vec<ddc_geom::Solid>>> = None;
            let arc_cut = match (&arcs, bulges[i] != 0.0) {
                (Some(w), true) => match arc_bodies(s, bulges[i], &w.rings[i], &ops[i], tol) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        issues.push(crate::tr("edit_status.diag.arc", &[("id", self.walls[i].id.to_string().as_str()), ("e", format!("{e:?}").as_str())]));
                        None
                    }
                },
                _ => None,
            };
            let layers = arc_cut.as_ref().unwrap_or(&cut.walls[i]);
            let wt = self.walls[i].wall_type;
            let mut wtag = |l: usize| {
                let t = texl.wall(wt, l);
                if t != l as u32 {
                    return t;
                }
                let f = finishes.entry(wt).or_insert_with(|| {
                    let ms = self.model.wall_type_materials(wt).unwrap_or_default();
                    ms.iter().map(|m| opening_fill::finish_rgb(m)).collect()
                });
                match f.get(l).copied().flatten() {
                    Some(c) => ddc_render::tag_rgb(c, false),
                    None => t,
                }
            };
            // sloped/stepped top: the body's upper vertices sit on the polyline
            let top = self.walls[i].top.as_ref();
            let arc_ax = (bulges[i] != 0.0)
                .then(|| ddc_geom::arc::ArcAxis::new(s.a, s.b, bulges[i], tol).ok())
                .flatten();
            let base = self.walls[i].base.as_ref().filter(|_| bulges[i] == 0.0);
            // the wall core under a roof reaches the bottom of its bearing layer, the finish reaches the roof bottom
            let core_top = self.model.wall_top_core(self.walls[i].id).ok().flatten().filter(|_| bulges[i] == 0.0);
            let core = if core_top.is_some() { self.model.wall_core_layers(wt).unwrap_or_default() } else { vec![] };
            let shaped = |m: &ddc_geom::mesh::Mesh, l: usize| {
                let mut m = m.clone();
                let t = match (&core_top, core.get(l)) {
                    (Some(c), Some(true)) => Some(c),
                    _ => top,
                };
                if let Some(t) = t {
                    match &arc_ax {
                        Some(ax) => ddc_geom::top::apply_arc(&mut m, ax, s.base, s.height, t),
                        None => ddc_geom::top::apply(&mut m, s.a, s.dir(), s.base, s.height, t),
                    }
                }
                if let Some(b) = base {
                    ddc_geom::top::apply_base(&mut m, s.a, s.dir(), s.base, b);
                }
                m
            };
            // cut by a void or a joined element: each layer piece minus the cutters (level-relative)
            let wid = self.walls[i].id;
            let cutters: Vec<ddc_geom::csg::Body> = match self.model.cutter_bodies("wall", wid) {
                Ok(cs) => cs
                    .into_iter()
                    .map(|c| c.translated(ddc_geom::math::v3(0.0, 0.0, -self.elevation)))
                    .collect(),
                Err(e) => {
                    issues.push(crate::tr("edit_status.diag.cut", &[("id", wid.to_string().as_str()), ("e", e.to_string().as_str())]));
                    vec![]
                }
            };
            let carve = |m: ddc_geom::mesh::Mesh, issues: &mut Vec<String>| {
                if cutters.is_empty() {
                    return m;
                }
                let mut body = ddc_geom::csg::Body::from_mesh(&m, 0);
                for c in &cutters {
                    body = body.subtract(c, tol);
                }
                let out = body.to_mesh();
                if out.triangles.is_empty() {
                    issues.push(crate::tr("edit_status.diag.cut_empty", &[("id", wid.to_string().as_str())]));
                    m
                } else {
                    out
                }
            };
            if !leans[i].is_plain() {
                let lm = match (&arcs, bulges[i] != 0.0) {
                    (Some(w), true) => {
                        let top = top_arcs.as_ref().map(|t| &t.rings[i][..]);
                        let b = &w.rings[i];
                        ddc_geom::wall_lean::ring_meshes(s, bulges[i], &leans[i], b, top, &ops[i], tol)
                    }
                    _ => {
                        let top = top_fps.get(i).filter(|t| !t.is_empty()).map(|t| &t[..]);
                        ddc_geom::wall_lean::layer_meshes(s, &leans[i], &fps[i], top, &ops[i], tol)
                    }
                };
                for (l, m) in lm {
                    let m = carve(m, &mut issues);
                    meshes.push(mesh(i as u32, wtag(l), &m));
                }
                continue;
            }
            for (l, bodies) in layers.iter().enumerate() {
                for (k, b) in bodies.iter().enumerate() {
                    match b {
                        Ok(b) => {
                            let m = carve(shaped(&b.mesh, l), &mut issues);
                            meshes.push(mesh(i as u32, wtag(l), &m));
                        }
                        Err(e) => {
                            issues.push(crate::tr(
                                "edit_status.diag.piece",
                                &[
                                    ("id", self.walls[i].id.to_string().as_str()),
                                    ("l", l.to_string().as_str()),
                                    ("k", k.to_string().as_str()),
                                    ("e", format!("{e:?}").as_str()),
                                ],
                            ));
                            let so = plain.get_or_insert_with(|| layer_solids(s, &fps[i], tol));
                            if let Some(Ok(m)) =
                                so.get(l).and_then(|v| v.get(k)).map(|x| x.triangulate())
                            {
                                meshes.push(mesh(i as u32, wtag(l), &m));
                            }
                        }
                    }
                }
            }
        }
        for (f, b) in fillers.iter().zip(&cut.fillers) {
            match b {
                Ok(b) => meshes.push(mesh(crate::scene_ids::EDITOR_MISC, 3, &b.mesh)),
                Err(e) => {
                    issues.push(crate::tr("edit_status.diag.filler", &[("e", format!("{e:?}").as_str())]));
                    if let Ok(p) = ddc_geom::Profile::new(f.ring.clone(), vec![], tol) {
                        if let Ok(m) = ddc_geom::Solid::extrude(&p, f.zt - f.zb, tol).triangulate()
                        {
                            meshes.push(mesh(crate::scene_ids::EDITOR_MISC, 3, &m));
                        }
                    }
                }
            }
        }
        // the opening is cut in the wall; the fill is frame, leaf/glass, hardware, sills
        for (k, o) in self.openings.iter().enumerate() {
            if o.kind == Kind::Void {
                continue;
            }
            let Some(h) = self.host_of(o.wall) else {
                continue;
            };
            let el = (self.walls.len() + k) as u32;
            let start = meshes.len();
            // in a leaning wall the fill turns about its sill line with the normal cut
            let tilt = self
                .walls
                .iter()
                .position(|w| w.id == o.wall)
                .filter(|&wi| !leans[wi].is_plain());
            let tilt_from = |meshes: &mut Vec<SceneMesh>| {
                let Some(wi) = tilt else { return };
                for m in &mut meshes[start..] {
                    for p in &mut m.positions {
                        let q = ddc_geom::wall_lean::tilt(
                            &strips[wi],
                            bulges[wi],
                            &leans[wi],
                            ddc_geom::v3(p[0] as f64, p[1] as f64, p[2] as f64),
                            o.sill,
                        );
                        *p = [q.x as f32, q.y as f32, q.z as f32];
                    }
                }
            };
            if !self.opening_filled.contains(&o.id) && h.half > 0.01 {
                meshes.extend(opening_fill::meshes(&h, o, el, lod, &self.detail));
                tilt_from(&mut meshes);
                continue;
            }
            let t = h.half.min(0.02);
            let (s0, s1) = (o.along, o.along + o.width);
            let poly = vec![h.at(s0, -t), h.at(s1, -t), h.at(s1, t), h.at(s0, t)];
            let Ok(p) = ddc_geom::Profile::new(poly, vec![], tol) else {
                continue;
            };
            let solid = ddc_geom::Solid::extrude(&p, o.height, tol).translated_z(o.sill);
            if let Ok(m) = solid.triangulate() {
                meshes.push(SceneMesh {
                    element: (self.walls.len() + k) as u32,
                    tag: if o.kind == Kind::Door { 5 } else { 6 },
                    positions: m
                        .positions
                        .iter()
                        .map(|p| [p.x as f32, p.y as f32, p.z as f32])
                        .collect(),
                    triangles: m.triangles,
                });
                tilt_from(&mut meshes);
            }
        }
        drop(span);
        let _rest = ddc_model::perf::span("scene.rest");
        crate::roof_edge_tools::scene_meshes(self, &mut meshes, &mut issues);
        self.cw_meshes(&mut meshes, &mut issues);
        // with the providers the solid provider draws them (repeated ones as instances)
        if !(self.scene_providers && crate::edit3d::scene3d::provided("solid")) {
            crate::solid_tools::scene_meshes(self, &mut meshes, &mut issues);
        }
        (Scene::new(meshes), issues)
    }
}

/// Columns of `opening_row` (openings with their host wall `w`); callers add the WHERE clause.
const OPENING_SQL: &str = "SELECT o.id,o.wall,o.along,o.width,o.sill,o.height,o.kind,\
     o.swing_side,o.swing_hinge FROM opening o JOIN wall w ON w.id=o.wall";

fn opening_row(r: &rusqlite::Row) -> rusqlite::Result<Placed> {
    let kind: String = r.get(6)?;
    Ok(Placed {
        id: r.get(0)?,
        wall: r.get(1)?,
        along: r.get(2)?,
        width: r.get(3)?,
        sill: r.get(4)?,
        height: r.get(5)?,
        kind: Kind::from_model(&kind),
        flip: Flip {
            side: r.get(7)?,
            hinge: r.get(8)?,
        },
    })
}

/// Columns of `wall_row`; callers add the WHERE clause.
const WALL_SQL: &str = "SELECT w.id, w.wtype, w.ax, w.ay, w.bx, w.by, w.height, w.bulge, \
     COALESCE((SELECT SUM(plan_area) FROM wall_qty q WHERE q.wall=w.id),0), \
     COALESCE((SELECT SUM(volume) FROM wall_qty q WHERE q.wall=w.id),0), w.top_profile, \
     w.base_offset, w.lean, w.top_scale FROM wall w";

fn wall_row(r: &rusqlite::Row) -> rusqlite::Result<Wall> {
    let top: Option<Vec<(f64, f64)>> = r
        .get::<_, Option<String>>(10)?
        .as_deref()
        .and_then(ddc_model::parse_top_profile);
    let bulge: f64 = r.get(7)?;
    let lean = ddc_geom::wall_lean::Lean::new(r.get(12)?, r.get(13)?);
    let lean = if top.is_some() || !lean.valid() {
        ddc_geom::wall_lean::Lean::PLAIN
    } else {
        lean
    };
    Ok(Wall {
        lean,
        id: r.get(0)?,
        wall_type: r.get(1)?,
        a: v2(r.get(2)?, r.get(3)?),
        b: v2(r.get(4)?, r.get(5)?),
        base_offset: r.get(11)?,
        height: r.get(6)?,
        bulge,
        plan_area: r.get(8)?,
        volume: r.get(9)?,
        top,
        base: None,
    })
}

/// Layer outlines of the first `n` strips after joining all of them; without a join, straight ends.
fn footprints(
    strips: &[Strip],
    bulges: &[f64],
    prefs: &[ddc_geom::layered::JoinPrefs],
    n: usize,
) -> Vec<Vec<Vec<V2>>> {
    footprints_joined(strips, bulges, prefs, n).0
}

/// `footprints` and whether all strips are straight and joined without error: only then
/// may the scoped reload (`scoped`) rebuild a part of the plan.
fn footprints_joined(
    strips: &[Strip],
    bulges: &[f64],
    prefs: &[ddc_geom::layered::JoinPrefs],
    n: usize,
) -> (Vec<Vec<Vec<V2>>>, bool) {
    let tol = Tol::default();
    let arcs = bulges.iter().any(|&b| b != 0.0);
    if arcs {
        if let Ok(w) = ddc_geom::arc::arc_walls_with(strips, bulges, prefs, tol) {
            let rings = w
                .rings
                .iter()
                .take(n)
                .map(|layers| {
                    layers
                        .iter()
                        .flatten()
                        .map(|r| r.tessellate(CHORD_SAG))
                        .collect()
                })
                .collect();
            return (rings, false);
        }
    }
    let joined = ddc_geom::layered::join_with(strips, prefs, tol).ok();
    let rings = strips
        .iter()
        .take(n)
        .enumerate()
        .map(|(i, s)| wall_footprint(s, joined.as_ref(), i))
        .collect();
    (rings, !arcs && joined.is_some())
}

/// Layer outlines of strip `i` of a join (plain ends without one).
fn wall_footprint(s: &Strip, joined: Option<&ddc_geom::layered::Joined>, i: usize) -> Vec<Vec<V2>> {
    let tol = Tol::default();
    let plain = plain_pieces(s);
    let pieces = joined.map_or(&plain, |j| &j.pieces[i]);
    layer_footprints(s, pieces, i, tol)
        .map(|fp| fp.into_iter().flatten().map(|p| p.outer).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wall_tool::Key;

    #[test]
    fn import_check_note_names_the_first_finding() {
        let mut m = Model::open(":memory:").unwrap();
        assert_eq!(import_check_note(&m), None);
        let l = m.add_level("L", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[ddc_model::LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        m.add_opening(
            w,
            &ddc_model::OpeningSpec {
                along: 1.0,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
        )
        .unwrap();
        m.set_wall_top_profile(w, Some(&[(0.0, 1.0), (6.0, 1.0)]))
            .unwrap();
        let note = import_check_note(&m).expect("a finding");
        assert!(note.contains("above wall top"), "{note}");
        assert!(!note.contains("sketch_hints."), "raw key: {note}");
    }

    fn seed_names(lang: ddc_ui::i18n::Lang) -> (Model, Vec<String>) {
        let mut m = Model::open(":memory:").unwrap();
        Editor::seed_model_in(&mut m, matches!(lang, ddc_ui::i18n::Lang::Ru)).unwrap();
        let names = m.level_list().unwrap().into_iter().map(|l| l.1).collect();
        (m, names)
    }

    #[test]
    fn new_project_en_has_english_level_names() {
        let (_, n) = seed_names(ddc_ui::i18n::Lang::En);
        assert_eq!(n, ["Level 1"]);
    }

    #[test]
    fn new_project_ru_has_russian_level_names() {
        let (_, n) = seed_names(ddc_ui::i18n::Lang::Ru);
        assert_eq!(n, ["\u{42d}\u{442}\u{430}\u{436} 1"]);
    }

    #[test]
    fn unknown_language_falls_back_to_english() {
        // a key absent from a language table resolves to the English text
        assert_eq!(
            crate::tr_in(ddc_ui::i18n::Lang::En, "edit_status.seed.wt_masonry", &[]),
            "Masonry 200"
        );
        let missing = crate::tr_in(ddc_ui::i18n::Lang::Ru, "st1.seed.no_such_key", &[]);
        assert_eq!(missing, "st1.seed.no_such_key");
    }

    #[test]
    fn reopen_keeps_names_after_language_switch() {
        // a project seeded in English, opened under another UI language; the global UI
        // language is not flipped here: other tests read it in parallel
        let (m, before) = seed_names(ddc_ui::i18n::Lang::En);
        let mut e = Editor::new_in_memory().unwrap();
        e.level = m.level_list().unwrap()[0].0;
        e.model = m;
        e.reload().unwrap();
        let after: Vec<String> = e.model.level_list().unwrap().into_iter().map(|l| l.1).collect();
        assert_eq!(after, before);
    }

    fn typed(e: &mut Editor, s: &str) {
        for c in s.chars() {
            e.handle(Event::Char(c));
        }
        e.handle(Event::Key(Key::Enter));
    }

    /// Scenario: a 6x4 m room by axes typed as numbers, 200 mm type, 3 m high.
    fn draw_room(e: &mut Editor) {
        e.current_type = 1;
        e.handle(Event::Click(v2(0.0, 0.0)));
        e.handle(Event::Move(v2(1.0, 0.02)));
        typed(e, "6000");
        e.handle(Event::Move(v2(6.03, 1.0)));
        typed(e, "4000");
        e.handle(Event::Move(v2(3.0, 4.04)));
        typed(e, "6000");
        e.handle(Event::Char('C'));
    }

    pub(crate) fn import_frames_one_wall() -> Vec<ddc_io::bridge::proto::Frame> {
        use ddc_io::bridge::proto::{self, Curve, Element, Frame, Top};
        vec![
            Frame::Hello(proto::Hello {
                protocol: proto::PROTOCOL,
                bridge: "b".into(),
                version: "1".into(),
                formats: vec!["rvt".into()],
                capabilities: vec![],
            }),
            Frame::Units(proto::Units { length_to_m: 1.0 }),
            Frame::Levels(vec![proto::Level {
                guid: "L1".into(),
                name: "L1".into(),
                elevation: 0.0,
            }]),
            Frame::Types(vec![proto::Type::Wall {
                guid: "T1".into(),
                name: "T".into(),
                layers: vec![proto::Layer {
                    material: "m".into(),
                    thickness: 0.2,
                }],
            }]),
            Frame::Elements(vec![Element::Wall {
                guid: "W1".into(),
                level: "L1".into(),
                wall_type: "T1".into(),
                axis: Curve::Line {
                    a: (0.0, 0.0),
                    b: (4.0, 0.0),
                },
                base_offset: 0.0,
                top: Top::Unconnected { height: 3.0 },
                offset0: None,
            }]),
            Frame::End(proto::Stats {
                elements: 1,
                warnings: 0,
            }),
        ]
    }

    fn import_with_failing_reload(e: &mut Editor) -> Result<ddc_io::bridge::Report, String> {
        let p = std::env::temp_dir().join("ddc_im1_reload_fail.rvt");
        std::fs::write(&p, b"x").unwrap();
        e.apply_import_with(&p, import_frames_one_wall(), |_| {
            Err(ddc_model::Error::Invalid("injected".into()))
        })
    }

    #[test]
    fn import_reload_fail_keeps_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        let n = e.undo.len();
        assert!(import_with_failing_reload(&mut e).is_ok());
        assert_eq!(e.undo.len(), n + 1, "committed import has its undo step");
        assert!(matches!(e.undo.last(), Some(Cmd::Import(..))));
        e.undo();
        e.reload().unwrap();
        assert!(e.walls.is_empty(), "undo removes the imported wall");
    }

    #[test]
    fn import_reload_fail_status_is_warning() {
        let mut e = Editor::new_in_memory().unwrap();
        assert!(import_with_failing_reload(&mut e).is_ok(), "not an Err");
        let s = e.status.clone().unwrap();
        assert!(s.contains("injected"), "{s}");
        assert!(!s.contains(&crate::tr("edst.import_failed", &[("e", "injected")])), "{s}");
    }

    fn count_rows(e: &Editor, sql: &str) -> i64 {
        e.model.sql().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn level_rows(e: &Editor) -> i64 {
        count_rows(e, "SELECT count(*) FROM level")
    }

    fn op_fn(
        f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
    ) -> OpFn {
        OpFn(std::sync::Arc::new(f))
    }

    fn refused() -> ddc_model::Error {
        ddc_model::Error::Invalid("refused".into())
    }

    #[test]
    fn refused_midway_rolls_back_rows() {
        let mut e = Editor::new_in_memory().unwrap();
        let (n, top) = (level_rows(&e), e.model.op_top().unwrap());
        let r = e.run_op(op_fn(|m| {
            m.add_level("A", 3.0)?;
            m.add_level("B", 6.0)?;
            Err(refused())
        }));
        assert!(r.is_err());
        assert_eq!(level_rows(&e), n, "rows of a refused op stay");
        assert_eq!(e.model.op_top().unwrap(), top, "journal of a refused op stays");
        // a committed atomic inside a refused op goes as well
        let r = e.run_op(op_fn(|m| {
            m.atomic("level_plan", |m| m.add_level("C", 9.0).map(|_| ()))?;
            Err(refused())
        }));
        assert!(r.is_err());
        assert_eq!(level_rows(&e), n);
        assert_eq!(e.model.op_top().unwrap(), top);
        e.run_op(op_fn(|m| m.add_level("D", 3.0).map(|_| ())))
            .unwrap();
        assert_eq!(level_rows(&e), n + 1, "the model still takes edits");
    }

    #[test]
    fn refused_midway_leaves_undo_stack() {
        let mut e = Editor::new_in_memory().unwrap();
        e.run_op(op_fn(|m| m.add_level("A", 3.0).map(|_| ())))
            .unwrap();
        let (u, n) = (e.undo.len(), level_rows(&e));
        let r = e.run_op(op_fn(|m| {
            m.add_level("B", 6.0)?;
            Err(refused())
        }));
        assert!(r.is_err());
        assert_eq!(e.undo.len(), u, "a refused op pushes no undo step");
        e.undo();
        assert_eq!(level_rows(&e), n - 1, "undo takes back the earlier edit");
        let u = e.undo.len();
        e.run_op(op_fn(|_| Ok(()))).unwrap();
        assert_eq!(e.undo.len(), u, "an op that writes nothing pushes no step");
    }

    #[test]
    fn success_still_one_undo_step() {
        let mut e = Editor::new_in_memory().unwrap();
        let (u, n, top) = (e.undo.len(), level_rows(&e), e.model.op_top().unwrap());
        e.run_op(op_fn(|m| {
            m.add_level("A", 3.0)?;
            m.atomic("level_plan", |m| m.add_level("B", 6.0).map(|_| ()))?;
            m.add_level("C", 9.0).map(|_| ())
        }))
        .unwrap();
        assert_eq!(level_rows(&e), n + 3);
        assert_eq!(e.undo.len(), u + 1, "one user action, one undo step");
        assert_eq!(e.model.op_top().unwrap(), top + 1, "one journal record");
        e.undo();
        assert_eq!(level_rows(&e), n, "one undo takes back the whole op");
        e.redo();
        assert_eq!(level_rows(&e), n + 3, "redo runs it again as one step");
        assert_eq!(e.undo.len(), u + 1);
        assert_eq!(e.model.op_top().unwrap(), top + 1);
    }

    /// Ops that attach a file (merge with a colleague's model) cannot run in a transaction.
    #[test]
    fn op_step_holds_no_transaction() {
        let mut e = Editor::new_in_memory().unwrap();
        let n = level_rows(&e);
        e.run_op(op_fn(|m| {
            m.sql()
                .execute_batch("ATTACH DATABASE ':memory:' AS ro2x; DETACH DATABASE ro2x;")?;
            m.add_level("A", 3.0).map(|_| ())
        }))
        .unwrap();
        assert_eq!(level_rows(&e), n + 1);
        assert!(e.model.sql().is_autocommit());
    }

    #[test]
    fn import_is_one_undo_step() {
        let mut e = Editor::new_in_memory().unwrap();
        let (u, n, top, old) = (
            e.undo.len(),
            level_rows(&e),
            e.model.op_top().unwrap(),
            e.level,
        );
        let walls = "SELECT count(*) FROM wall";
        let w = count_rows(&e, walls);
        let p = std::env::temp_dir().join("ddc_ro2_one_step.rvt");
        std::fs::write(&p, b"x").unwrap();
        e.apply_import(&p, import_frames_one_wall()).unwrap();
        assert_eq!(count_rows(&e, walls), w + 1);
        assert_eq!(level_rows(&e), n, "imported level in, empty default level out");
        assert_ne!(e.level, old, "the imported level is active");
        assert_eq!(e.undo.len(), u + 1, "one user action, one undo step");
        assert_eq!(e.model.op_top().unwrap(), top + 1, "import and level removal: one record");
        e.undo();
        assert_eq!(count_rows(&e, walls), w);
        let back = format!("SELECT count(*) FROM level WHERE id={old}");
        assert_eq!(count_rows(&e, &back), 1, "one undo brings the default level back");
        e.redo();
        assert_eq!(count_rows(&e, walls), w + 1);
        assert_eq!(e.undo.len(), u + 1);
    }

    #[test]
    fn level_remove_refused_changes_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let (u, n, top, old) = (
            e.undo.len(),
            level_rows(&e),
            e.model.op_top().unwrap(),
            e.level,
        );
        let walls = "SELECT count(*) FROM wall";
        let w = count_rows(&e, walls);
        let keep = format!(
            "CREATE TEMP TRIGGER ro2_keep BEFORE DELETE ON level WHEN old.id={old} \
             BEGIN SELECT RAISE(ABORT, 'refused'); END"
        );
        e.model.sql().execute_batch(&keep).unwrap();
        let p = std::env::temp_dir().join("ddc_ro2_keep.rvt");
        std::fs::write(&p, b"x").unwrap();
        e.apply_import(&p, import_frames_one_wall()).unwrap();
        let here = format!("SELECT count(*) FROM level WHERE id={old}");
        assert_eq!(count_rows(&e, &here), 1, "the default level stays");
        assert_eq!(level_rows(&e), n + 1, "imported level in, nothing else out");
        assert_eq!(count_rows(&e, walls), w + 1, "the import itself is kept");
        assert_eq!(e.undo.len(), u + 1, "still one undo step");
        assert_eq!(e.model.op_top().unwrap(), top + 1, "the refused removal leaves no record");
        e.model.sql().execute_batch("DROP TRIGGER ro2_keep").unwrap();
        e.undo();
        assert_eq!(level_rows(&e), n);
        assert_eq!(count_rows(&e, walls), w);
        assert_eq!(count_rows(&e, &here), 1);
    }

    /// A view row the loader rejects: `reload` fails while every model write still works.
    pub(crate) fn break_views(e: &mut Editor) {
        e.model
            .sql()
            .execute_batch(
                "INSERT INTO view_def(kind,name,ax,ay,bx,by,depth,far_clip)                  VALUES ('bogus','x',0,0,1,0,1,0)",
            )
            .unwrap();
        assert!(e.reload().is_err());
    }

    #[test]
    fn redo_import_reload_fail_is_warning_kept_on_stack() {
        let mut e = Editor::new_in_memory().unwrap();
        assert!(import_with_failing_reload(&mut e).is_ok());
        e.undo();
        e.reload().unwrap();
        assert!(e.walls.is_empty());
        break_views(&mut e);
        e.redo();
        let s = e.status.clone().unwrap();
        assert!(!s.contains(&crate::tr("edst.redo_failed", &[("e", "")])), "{s}");
        let done = hist::done_text(true, &hist::step_name(e.undo.last().unwrap()));
        assert!(s.starts_with(&done), "{s}");
        assert!(s.contains(&crate::tr("edst.view_stale", &[("e", "")])), "{s}");
        assert!(matches!(e.undo.last(), Some(Cmd::Import(..))), "kept on the undo stack");
        assert!(e.redo.is_empty(), "not parked for a second import");
        e.model.sql().execute_batch("DELETE FROM view_def").unwrap();
        e.reload().unwrap();
        assert_eq!(e.walls.len(), 1, "the redo committed the import once");
    }

    #[test]
    fn update_opening_stale_keeps_done() {
        let mut e = Editor::new_in_memory().unwrap();
        let t = e.types[1].id;
        let w = e
            .model
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level: e.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        e.reload().unwrap();
        let spec = OpSpec {
            wall: w,
            along: 2.0,
            ty: OpeningType::door(),
            flip: Flip::default(),
        };
        let id = e.apply_add_opening(&spec).unwrap();
        break_views(&mut e);
        e.set_opening_dims(id, 1.0, 2.0, 0.0);
        let s = e.status.clone().unwrap();
        let done = crate::tr("edst.opening_changed", &[("id", &id.to_string())]);
        assert!(s.starts_with(&done), "{s}");
        assert!(s.contains(&crate::tr("edst.view_stale", &[("e", "")])), "{s}");
        assert!(matches!(e.undo.last(), Some(Cmd::UpdateOpening(..))));
    }

    #[test]
    fn run_op_keeps_the_undo_step_when_reload_fails() {
        let mut e = Editor::new_in_memory().unwrap();
        let n = e.undo.len();
        let f = OpFn(std::sync::Arc::new(
            |m: &mut Model| -> Result<(), ddc_model::Error> {
                m.add_level("ZZ", 9.0)?;
                m.sql()
                    .execute_batch("ALTER TABLE wall_type_layer RENAME TO zz_layer")?;
                Ok(())
            },
        ));
        assert!(e.run_op(f).is_err(), "reload reads the renamed table");
        assert_eq!(
            e.undo.len(),
            n + 1,
            "the model has the op: so does the stack"
        );
    }

    #[test]
    fn room_6x4_typed_exact_quantities_undo_redo() {
        let mut e = Editor::new_in_memory().unwrap();
        draw_room(&mut e);
        assert_eq!(e.walls.len(), 4, "status: {:?}", e.status);
        let close = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} != {b}");
        close(e.total_area(), 6.2 * 4.2 - 5.8 * 3.8);
        close(e.total_area(), 4.0);
        close(e.total_volume(), 12.0);
        let mut ls: Vec<f64> = e.walls.iter().map(|w| (w.b - w.a).len()).collect();
        ls.sort_by(f64::total_cmp);
        for (l, x) in ls.iter().zip([4.0, 4.0, 6.0, 6.0]) {
            close(*l, x);
        }
        for w in &e.walls {
            assert!(w.plan_area > 0.0);
            close(w.volume, w.plan_area * 3.0);
        }
        assert_eq!(e.plan.len(), 4);
        // one gesture is one undo
        e.undo();
        assert_eq!(e.walls.len(), 0);
        e.redo();
        assert_eq!(e.walls.len(), 4);
        close(e.total_volume(), 12.0);
        assert!(!e.can_redo());
    }

    #[test]
    fn rect_mode_and_delete_with_undo_redo_id_remap() {
        let mut e = Editor::new_in_memory().unwrap();
        e.set_mode(Mode::Rect);
        e.handle(Event::Press(v2(0.0, 0.0)));
        e.handle(Event::Release(v2(6.0, 4.0)));
        assert_eq!(e.walls.len(), 4);
        assert!((e.total_volume() - 12.0).abs() < 1e-9);
        e.set_mode(Mode::Select);
        e.handle(Event::Click(v2(3.0, 0.05)));
        assert_eq!(e.selection.len(), 1);
        let Sel::Wall(picked) = e.selection[0] else {
            panic!("wall expected")
        };
        assert!(e
            .walls
            .iter()
            .any(|w| w.id == picked && w.a.y == 0.0 && w.b.y == 0.0));
        e.delete_selection();
        assert_eq!(e.walls.len(), 3);
        e.undo(); // restore the wall
        e.undo(); // remove the rectangle
        assert_eq!(e.walls.len(), 0);
        e.redo(); // the rectangle with new ids
        e.redo(); // deletion of the reassigned wall
        assert_eq!(e.walls.len(), 3, "status: {:?}", e.status);
        assert!(!e.walls.iter().any(|w| w.a.y == 0.0 && w.b.y == 0.0));
        // a new gesture clears redo
        e.undo();
        e.set_mode(Mode::Polyline);
        e.handle(Event::Click(v2(10.0, 0.0)));
        e.handle(Event::Click(v2(12.0, 0.0)));
        e.handle(Event::Key(Key::Escape));
        assert!(!e.can_redo());
        assert_eq!(e.walls.len(), 5);
    }

    #[test]
    fn preview_has_joined_outlines_and_scene_builds() {
        let mut e = Editor::new_in_memory().unwrap();
        e.handle(Event::Click(v2(0.0, 0.0)));
        e.handle(Event::Click(v2(4.0, 0.0)));
        e.handle(Event::Move(v2(4.0, 3.0)));
        let pv = e.preview_plan();
        assert_eq!(pv.len(), 2);
        // L corner: the outer corner of the first outline reaches x = 4.1
        let maxx = pv.iter().flatten().map(|p| p.x).fold(f64::MIN, f64::max);
        assert!((maxx - 4.1).abs() < 1e-9, "{maxx}");
        e.handle(Event::Key(Key::Escape)); // the rubber-band segment is not committed
        assert_eq!(e.walls.len(), 1);
        let s = e.scene();
        assert!(!s.meshes.is_empty());
        assert!((s.bounds.unwrap().max.z - 3.0).abs() < 1e-5);
    }

    #[test]
    fn undo_while_drawing_drops_draft_and_empty_history_is_message() {
        let mut e = Editor::new_in_memory().unwrap();
        e.undo();
        assert_eq!(e.status.as_deref(), Some(crate::tr("edit_status.ed.nothing_undo", &[]).as_str()));
        e.handle(Event::Click(v2(0.0, 0.0)));
        e.undo();
        assert!(!e.drawing());
        e.redo();
        assert_eq!(e.status.as_deref(), Some(crate::tr("edit_status.ed.nothing_redo", &[]).as_str()));
    }

    fn specs_at(e: &Editor, level: i64) -> Vec<(i64, i64, f64, f64, f64, f64)> {
        let mut st = e
            .model
            .sql()
            .prepare(
                "SELECT level,wtype,ax,bx,height,base_offset FROM wall WHERE level=?1 ORDER BY ax,ay",
            )
            .unwrap();
        st.query_map([level], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
    }

    #[test]
    fn redo_replays_exact_spec_after_level_switch() {
        let mut e = Editor::new_in_memory().unwrap();
        let l1 = e.level;
        e.height = 2.7;
        e.handle(Event::Click(v2(0.0, 0.0)));
        e.handle(Event::Click(v2(4.0, 0.0)));
        e.handle(Event::Key(Key::Escape));
        let before = specs_at(&e, l1);
        assert_eq!(before.len(), 1);
        e.undo();
        let l2 = e.model.add_level("Floor 2", 3.0).unwrap();
        e.level = l2;
        e.height = 5.0;
        e.reload().unwrap();
        e.redo();
        assert_eq!(specs_at(&e, l1), before);
        assert!(specs_at(&e, l2).is_empty());
    }

    #[test]
    fn delete_redo_after_level_switch_removes_original() {
        let mut e = Editor::new_in_memory().unwrap();
        let l1 = e.level;
        e.handle(Event::Click(v2(0.0, 0.0)));
        e.handle(Event::Click(v2(4.0, 0.0)));
        e.handle(Event::Key(Key::Escape));
        e.set_mode(Mode::Select);
        e.handle(Event::Click(v2(2.0, 0.0)));
        e.delete_selection();
        e.undo();
        let before = specs_at(&e, l1);
        assert_eq!(before.len(), 1);
        e.level = e.model.add_level("Floor 2", 3.0).unwrap();
        e.reload().unwrap();
        e.redo();
        assert!(specs_at(&e, l1).is_empty(), "{:?}", e.status);
        e.undo();
        assert_eq!(specs_at(&e, l1), before);
    }

    fn wall_where(e: &Editor, f: impl Fn(&Wall) -> bool) -> Wall {
        e.walls.iter().find(|w| f(w)).cloned().expect("wall")
    }

    fn south(e: &Editor) -> Wall {
        wall_where(e, |w| w.a.y == 0.0 && w.b.y == 0.0)
    }

    fn east(e: &Editor) -> Wall {
        wall_where(e, |w| w.a.x == 6.0 && w.b.x == 6.0)
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    /// A door at the centre of the south wall, a window typed as a distance from the east wall's end.
    fn place_door_and_window(e: &mut Editor) -> (i64, i64) {
        draw_room(e);
        assert_eq!(e.walls.len(), 4, "{:?}", e.status);
        e.start_opening(OpeningType::door());
        e.handle(Event::Move(v2(3.02, 0.03)));
        let pv = e.otool.preview.clone().expect("preview over wall");
        close(pv.along, 2.55);
        assert_eq!(pv.snap, crate::opening_tool::AlongSnap::Center);
        e.handle(Event::Click(v2(3.02, 0.03)));
        assert_eq!(e.openings.len(), 1, "{:?}", e.status);
        let door = e.openings[0].id;
        e.start_opening(OpeningType::window());
        e.handle(Event::Move(v2(6.0, 0.6)));
        typed(e, "1000");
        assert_eq!(e.openings.len(), 2, "{:?}", e.status);
        let win = e.openings[1].id;
        (door, win)
    }

    #[test]
    fn openings_scenario_exact_volumes_overlap_undo_redo() {
        let mut e = Editor::new_in_memory().unwrap();
        draw_room(&mut e);
        let (vs, ve) = (south(&e).volume, east(&e).volume);
        e.undo();
        let (door, win) = place_door_and_window(&mut e);
        let d = *e.opening(door).unwrap();
        assert_eq!((d.wall, d.kind), (south(&e).id, Kind::Door));
        close(d.along, 2.55);
        let w = *e.opening(win).unwrap();
        assert_eq!((w.wall, w.kind), (east(&e).id, Kind::Window));
        close(w.along, 1.0);
        close(w.sill, 0.9);
        close(south(&e).volume, vs - 0.9 * 2.1 * 0.2);
        close(east(&e).volume, ve - 1.2 * 1.4 * 0.2);
        close(e.total_volume(), 11.286);
        // intersection, going past the wall, above the wall: refused, the model is unchanged
        e.start_opening(OpeningType::door());
        e.handle(Event::Click(v2(2.45, 0.0)));
        assert!(
            e.status.as_deref().unwrap().contains(&crate::app2_fx::fx("ed_overlaps")),
            "{:?}",
            e.status
        );
        let tall = OpeningType {
            height: 3.5,
            ..OpeningType::door()
        };
        e.start_opening(tall);
        e.handle(Event::Click(v2(1.0, 4.0)));
        assert!(
            e.status.as_deref().unwrap().contains(&crate::app2_fx::fx("ed_above_wall")),
            "{:?}",
            e.status
        );
        e.start_opening(OpeningType::door());
        e.handle(Event::Move(v2(1.0, 4.0)));
        typed(&mut e, "5500");
        assert!(
            e.status.as_deref().unwrap().contains(&crate::app2_fx::fx("ed_outside")),
            "{:?}",
            e.status
        );
        e.handle(Event::Click(v2(3.0, 2.0)));
        let no_wall = crate::opening_tool::PlaceError::NoWall.to_string();
        assert_eq!(e.status.as_deref(), Some(no_wall.as_str()));
        assert_eq!(e.openings.len(), 2);
        close(e.total_volume(), 11.286);
        // undo/redo one opening at a time
        e.undo();
        close(e.total_volume(), 12.0 - 0.378);
        e.undo();
        close(e.total_volume(), 12.0);
        e.redo();
        e.redo();
        close(e.total_volume(), 11.286);
        // roll back to an empty plan and replay with wall ids reassigned
        e.undo();
        e.undo();
        e.undo();
        assert!(e.walls.is_empty());
        e.redo();
        e.redo();
        e.redo();
        assert_eq!(e.openings.len(), 2, "{:?}", e.status);
        close(e.total_volume(), 11.286);
        assert!(!e.can_redo());
    }

    #[test]
    fn door_swing_lives_in_model_and_is_undoable() {
        let mut e = Editor::new_in_memory().unwrap();
        draw_room(&mut e);
        e.start_opening(OpeningType::door());
        e.handle(Event::Char(' '));
        e.handle(Event::Char('f'));
        e.handle(Event::Click(v2(3.02, 0.03)));
        let door = e.openings[0].id;
        let sw = ddc_model::Swing {
            side: true,
            hinge: true,
        };
        assert_eq!(e.model.opening_swing(door).unwrap(), sw);
        assert_eq!(
            e.openings[0].flip,
            Flip {
                side: true,
                hinge: true
            }
        );
        // a fresh editor over the same model data would read the same flags
        e.reload().unwrap();
        assert_eq!(
            e.openings[0].flip,
            Flip {
                side: true,
                hinge: true
            }
        );
        e.set_opening_flip(
            door,
            Flip {
                side: false,
                hinge: true,
            },
        );
        assert_eq!(
            e.model.opening_swing(door).unwrap(),
            ddc_model::Swing {
                side: false,
                hinge: true
            }
        );
        e.undo();
        assert_eq!(e.model.opening_swing(door).unwrap(), sw);
        e.redo();
        assert!(!e.openings[0].flip.side && e.openings[0].flip.hinge);
        // moving the door keeps its swing
        e.set_opening_dims(door, 1.0, 2.1, 0.0);
        assert!(e.openings[0].flip.hinge && !e.openings[0].flip.side);
        e.undo();
        e.undo();
        e.undo();
        assert!(e.openings.is_empty());
        e.redo();
        assert_eq!(e.model.opening_swing(e.openings[0].id).unwrap(), sw);
    }

    #[test]
    fn openings_scenario_drag_props_delete_one_op_each() {
        let mut e = Editor::new_in_memory().unwrap();
        let (door, _) = place_door_and_window(&mut e);
        let vs = south(&e).volume;
        e.set_mode(Mode::Select);
        e.handle(Event::Click(v2(3.0, 0.0)));
        assert_eq!(e.selection, vec![Sel::Opening(door)]);
        e.handle(Event::Press(v2(3.0, 0.0)));
        e.handle(Event::Move(v2(4.03, 0.2)));
        assert!(e.drag.is_some());
        e.handle(Event::Release(v2(4.03, 0.2)));
        close(e.opening(door).unwrap().along, 3.6);
        close(south(&e).volume, vs);
        e.undo();
        close(e.opening(door).unwrap().along, 2.55);
        // Esc during a drag: no operation
        e.handle(Event::Press(v2(3.0, 0.0)));
        e.handle(Event::Move(v2(1.0, 0.0)));
        e.handle(Event::Key(Key::Escape));
        e.handle(Event::Release(v2(1.0, 0.0)));
        close(e.opening(door).unwrap().along, 2.55);
        // properties: width 1.0 gives a deduction of 1.0·2.1·0.2
        e.set_opening_dims(door, 1.0, 2.1, 0.0);
        close(south(&e).volume, vs + 0.378 - 0.42);
        e.set_opening_dims(door, 1.0, 3.2, 0.0);
        assert!(e.status.as_deref().unwrap().starts_with(crate::tr("edit_status.ed.not_changed", &[("e", "")]).trim_end()));
        e.undo();
        close(south(&e).volume, vs);
        e.redo();
        close(south(&e).volume, vs + 0.378 - 0.42);
        e.undo();
        // Delete of the selected opening
        e.handle(Event::Click(v2(3.0, 0.0)));
        e.delete_selection();
        assert!(e.opening(door).is_none());
        close(south(&e).volume, vs + 0.378);
        e.undo();
        close(south(&e).volume, vs);
        e.redo();
        assert!(e.opening(door).is_none());
    }

    #[test]
    fn opening_click_on_edge_is_noop_and_keeps_grab() {
        let mut e = Editor::new_in_memory().unwrap();
        let (door, _) = place_door_and_window(&mut e);
        e.set_mode(Mode::Select);
        e.handle(Event::Click(v2(3.0, 0.0)));
        let a0 = e.opening(door).unwrap().along;
        let n = e.undo.len();
        e.handle(Event::Press(v2(3.0, 0.0)));
        e.handle(Event::Release(v2(3.0, 0.0)));
        close(e.opening(door).unwrap().along, a0);
        assert_eq!(e.undo.len(), n);
        // small move keeps the grab offset: shift equals pointer shift (before snapping)
        e.handle(Event::Press(v2(3.0, 0.0)));
        e.handle(Event::Move(v2(3.0, 0.0)));
        close(e.drag.as_ref().unwrap().along, a0);
    }

    #[test]
    fn highlight_all_covers_every_selected_item() {
        let mut e = Editor::new_in_memory().unwrap();
        let (door, _) = place_door_and_window(&mut e);
        let w: Vec<Sel> = e.walls.iter().map(|w| Sel::Wall(w.id)).collect();
        // two walls and the door: three separate ranges
        e.selection = vec![w[2], Sel::Opening(door), w[0]];
        assert_eq!(e.highlight_all(), vec![(0, 1), (2, 3), (4, 5)]);
        // three walls, two of them neighbours in the scene: one range for them
        e.selection = vec![w[0], w[1], w[3]];
        assert_eq!(e.highlight_all(), vec![(0, 2), (3, 4)]);
        // first-only highlight stays for framing and the section box
        assert_eq!(e.highlight(), Some((0, 1)));
        e.selection.clear();
        assert!(e.highlight_all().is_empty());
    }

    #[test]
    fn selection_sync_plan_and_3d() {
        use ddc_render::glam::Vec3;
        let mut e = Editor::new_in_memory().unwrap();
        let (door, win) = place_door_and_window(&mut e);
        e.set_mode(Mode::Select);
        e.handle(Event::Click(v2(3.0, 0.0)));
        let (el, end) = e.highlight().expect("door in scene");
        assert_eq!((el, end), (4, 5));
        assert_eq!(e.sel_of_element(el), Some(Sel::Opening(door)));
        let scene = e.scene();
        let ray = |x: f32, y: f32, z: f32, d: Vec3| ddc_render::Ray {
            origin: Vec3::new(x, y, z),
            dir: d,
        };
        // a ray from the south at 1 m height: at the door it hits the opening, further left the south wall
        let h = scene.pick(&ray(3.0, -5.0, 1.0, Vec3::Y)).unwrap();
        e.select_element(Some(h.element));
        assert_eq!(e.selection, vec![Sel::Opening(door)]);
        let h = scene.pick(&ray(1.0, -5.0, 1.0, Vec3::Y)).unwrap();
        e.select_element(Some(h.element));
        assert_eq!(e.selection, vec![Sel::Wall(south(&e).id)]);
        // a window from the east: z = 1.5 is inside [0.9, 2.3]
        let h = scene.pick(&ray(10.0, 1.6, 1.5, -Vec3::X)).unwrap();
        e.select_element(Some(h.element));
        assert_eq!(e.selection, vec![Sel::Opening(win)]);
        assert_eq!(e.highlight(), Some((h.element, h.element + 1)));
        e.select_element(None);
        assert!(e.selection.is_empty());
        assert_eq!(e.sel_of_element(99), None);
        assert_eq!(e.sel_of_element(u32::MAX - 1), None);
    }

    fn scene_volume(s: &Scene, pick: impl Fn(u32) -> bool) -> f64 {
        let mut v = 0.0;
        for m in s.meshes.iter().filter(|m| pick(m.element)) {
            let p = |i: u32| m.positions[i as usize].map(f64::from);
            for t in &m.triangles {
                let [a, b, c] = t.map(p);
                v += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]))
                    / 6.0;
            }
        }
        v
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-4, "{a} != {b}");
    }

    #[test]
    fn slab_cuts_walls_in_scene() {
        let mut e = Editor::new_in_memory().unwrap();
        draw_room(&mut e);
        let walls = |e: &Editor| scene_volume(&e.scene_report().0, |el| el < 4);
        near(walls(&e), 12.0);
        let st = e
            .model
            .add_slab_type(
                "Slab 200",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let sq = |x0: f64, x1: f64| vec![(x0, -1.), (x1, -1.), (x1, 5.), (x0, 5.)];
        e.model
            .add_slab(&ddc_model::SlabSpec {
                level: e.level,
                slab_type: st,
                offset: 1.6,
                outer: sq(-1., 3.),
                holes: vec![],
            })
            .unwrap();
        let (sc, issues) = e.scene_report();
        assert!(issues.is_empty(), "{issues:?}");
        // in the half-plane x <= 3 the wall plan area is 3.1·0.2·2 + 0.2·3.8 = 2.0 m²
        near(scene_volume(&sc, |el| el < 4), 12.0 - 0.2 * 2.0);
    }

    #[test]
    fn bad_opening_does_not_drop_other_cutouts() {
        let mut e = Editor::new_in_memory().unwrap();
        place_door_and_window(&mut e);
        let mut bad = e.openings[1].clone();
        bad.width = f64::NAN;
        e.openings.push(bad);
        let (sc, issues) = e.scene_report();
        assert_eq!(issues.len(), 1, "{issues:?}");
        near(scene_volume(&sc, |el| el < 4), 12.0 - 0.378 - 0.336);
    }

    fn spaced_walls(e: &mut Editor, n: usize) -> Vec<i64> {
        let t = e.types[1].id;
        let specs: Vec<_> = (0..n)
            .map(|i| ddc_model::WallSpec {
                a: (0., i as f64 * 10.),
                b: (5., i as f64 * 10.),
                level: e.level,
                base_offset: 0.0,
                height: e.height,
                wall_type: t,
                offset0: None,
            })
            .collect();
        e.model.add_walls(&specs).unwrap()
    }

    #[test]
    fn reload_select_all_10k_walls_keeps_selection() {
        let mut e = Editor::new_in_memory().unwrap();
        let ids = spaced_walls(&mut e, 2000);
        e.reload().unwrap();
        e.selection = ids.iter().map(|&i| Sel::Wall(i)).collect();
        e.selection.push(Sel::Wall(-1));
        e.reload().unwrap();
        assert_eq!(e.selection.len(), ids.len());
        assert!(!e.selection.contains(&Sel::Wall(-1)));
    }

    #[test]
    fn reload_cutter_queries_once_per_level() {
        let mut e = Editor::new_in_memory().unwrap();
        spaced_walls(&mut e, 10);
        let count = |e: &mut Editor| {
            CUT_QUERIES.with(|c| c.set(0));
            e.reload().unwrap();
            CUT_QUERIES.with(|c| c.get())
        };
        let few = count(&mut e);
        spaced_walls(&mut e, 300);
        let many = count(&mut e);
        assert_eq!(few, 1);
        assert_eq!(many, few);
    }

    #[test]
    fn reload_cut_plan_matches_per_wall_path() {
        let mut e = Editor::new_in_memory().unwrap();
        let t = e.types[1].id;
        let spec = |a: (f64, f64), b: (f64, f64)| ddc_model::WallSpec {
            a,
            b,
            level: e.level,
            base_offset: 0.0,
            height: e.height,
            wall_type: t,
            offset0: None,
        };
        let ids = e
            .model
            .add_walls(&[
                spec((0., 0.), (5., 0.)),
                spec((2., -2.), (2., 2.)),
                spec((0., 20.), (5., 20.)),
            ])
            .unwrap();
        e.model
            .join_geometry(("wall", ids[0]), ("wall", ids[1]))
            .unwrap();
        e.reload().unwrap();
        let z = e.elevation + 1.2;
        let strips: Vec<Strip> = e.walls.iter().map(|w| e.strip(w)).collect();
        let bulges: Vec<f64> = e.walls.iter().map(|w| w.bulge).collect();
        let mut want = footprints(&strips, &bulges, &e.join_prefs(), strips.len());
        let mut cut_walls = 0;
        for (i, w) in e.walls.iter().enumerate() {
            if let Ok(Some(rs)) = e.model.cut_section("wall", w.id, z) {
                cut_walls += 1;
                want[i] = rs
                    .into_iter()
                    .filter(|r| ddc_geom::poly::area(r) > 0.0)
                    .collect();
            }
        }
        assert!(cut_walls >= 1, "the join must cut one wall");
        assert_eq!(e.plan, want);
    }

    #[test]
    fn door_at_node_cuts_filler() {
        let mut e = Editor::new_in_memory().unwrap();
        let t = e.types[1].id;
        let spec = |b: (f64, f64)| ddc_model::WallSpec {
            a: (0., 0.),
            b,
            level: e.level,
            base_offset: 0.0,
            height: e.height,
            wall_type: t,
            offset0: None,
        };
        e.model
            .add_walls(&[spec((5., 0.)), spec((-3., 3.)), spec((-3., -3.))])
            .unwrap();
        e.reload().unwrap();
        let tol = Tol::default();
        let strips: Vec<Strip> = e.walls.iter().map(|w| e.strip(w)).collect();
        let j = ddc_geom::layered::join(&strips, tol).unwrap();
        assert_eq!(j.fillers.len(), 1);
        let h = e.height;
        let fa = ddc_geom::profile::signed_area(&j.fillers[0]).abs();
        let host = e.walls.iter().position(|w| w.b == v2(5., 0.)).unwrap();
        let mut door = Placed {
            id: 0,
            wall: e.walls[host].id,
            along: 0.0,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: Kind::Door,
            flip: Flip::default(),
        };
        // the opening extends past the joint axis: the part of the fill with x <= 0.9 lies in the host thickness strip
        door.along = -0.5;
        door.width = 1.4;
        e.openings.push(door);
        let (sc, issues) = e.scene_report();
        assert!(issues.is_empty(), "{issues:?}");
        let clip = clip_box(&j.fillers[0], -0.5, 0.9, -0.1, 0.1);
        assert!(clip > 0.0);
        near(
            scene_volume(&sc, |el| el == u32::MAX - 1),
            fa * h - clip * 2.1,
        );
    }

    /// Area of a convex polygon intersected with the rectangle [x0,x1]×[y0,y1].
    fn clip_box(poly: &[V2], x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
        let mut p = poly.to_vec();
        let planes: [(V2, V2); 4] = [
            (v2(1., 0.), v2(x0, 0.)),
            (v2(-1., 0.), v2(x1, 0.)),
            (v2(0., 1.), v2(0., y0)),
            (v2(0., -1.), v2(0., y1)),
        ];
        for (n, o) in planes {
            let d = |q: V2| dot(n, q - o);
            let mut out = vec![];
            for i in 0..p.len() {
                let (a, b) = (p[i], p[(i + 1) % p.len()]);
                if d(a) >= 0.0 {
                    out.push(a);
                }
                if d(a) * d(b) < 0.0 {
                    out.push(a + (b - a) * (d(a) / (d(a) - d(b))));
                }
            }
            p = out;
            if p.len() < 3 {
                return 0.0;
            }
        }
        ddc_geom::profile::signed_area(&p).abs()
    }
}

/// `done` plus the view-stale warning when the refresh after a committed op failed.
pub(crate) fn stale_join(done: String, stale: Option<&str>) -> String {
    match stale {
        None => done,
        Some(e) => format!("{done} · {}", crate::tr("edst.view_stale", &[("e", e)])),
    }
}

/// Default level of a new file not tied to an import: the seed name in either UI language, at elevation 0.
pub(crate) fn default_level(m: &ddc_model::Model, id: i64) -> bool {
    let (ru, en) = (
        crate::tr_lang(true, "project_seed.seed_level", &[]),
        crate::tr_lang(false, "project_seed.seed_level", &[]),
    );
    m.sql()
        .query_row(
            "SELECT name IN (?2, ?3) AND elevation=0              AND NOT EXISTS(SELECT 1 FROM ext_ref WHERE kind='level' AND ref=level.id)              FROM level WHERE id=?1",
            rusqlite::params![id, ru, en],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false)
}

/// The imported level with the most walls (then slabs and bodies; on a tie, the lowest).
pub(crate) fn imported_level(m: &ddc_model::Model) -> Option<i64> {
    m.sql()
        .query_row(
            "SELECT l.id FROM level l JOIN ext_ref e ON e.kind='level' AND e.ref=l.id \
             ORDER BY (SELECT count(*) FROM wall WHERE level=l.id) DESC, \
             (SELECT count(*) FROM slab WHERE level=l.id) \
             + (SELECT count(*) FROM mesh WHERE level=l.id) DESC, l.elevation, l.id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
}

/// After an import: model-check findings as one status note (count and the first problem);
/// they are warnings — the imported model stays.
pub(crate) fn import_check_note(m: &ddc_model::Model) -> Option<String> {
    let iss = m.check_integrity().ok()?;
    let first = iss.first()?;
    Some(crate::tr(
        "sketch_hints.import_warn",
        &[
            ("n", &iss.len().to_string()),
            ("first", &format!("{} {}: {}", first.table, first.id, first.problem)),
        ],
    ))
}
