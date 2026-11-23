// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model tools (TOOLS_UI §1): column, beam, roof, ceiling, stair, railing, shaft, grid,
//! reference plane, component, networks (runs, fixtures, connections).
//! State machine Idle → Picking(n) → Preview → Commit; writing goes through `Editor::apply_ext` (one undo step).
//! Input — `handle` from `Editor::handle`; drawing and the option bar — `paint` from the plan.

#[path = "beam.rs"]
mod beam;
#[path = "ceiling.rs"]
mod ceiling;
#[path = "column.rs"]
mod column;
#[path = "component.rs"]
pub mod component;
#[path = "core_items.rs"]
mod core_items;
#[path = "demo.rs"]
mod demo;
#[path = "drawing_loop.rs"]
mod drawing_loop;
#[path = "floor.rs"]
mod floor;
#[path = "grid.rs"]
mod grid;
mod grid_bubble;
#[path = "host.rs"]
mod host;
#[path = "layer.rs"]
mod layer;
#[path = "level.rs"]
mod level;
#[path = "loop_shapes.rs"]
mod loop_shapes;
pub mod measure;
#[path = "mep.rs"]
pub(crate) mod mep;
#[path = "mep_color.rs"]
mod mep_color;
pub(crate) mod opt_field;
#[path = "railing.rs"]
mod railing;
#[path = "ramp.rs"]
mod ramp;
#[path = "refplane.rs"]
mod refplane;
#[path = "roof.rs"]
mod roof;
#[path = "room.rs"]
pub mod room;
#[path = "room_sep.rs"]
mod room_sep;
#[path = "shaft.rs"]
mod shaft;
#[path = "stair.rs"]
mod stair;
#[path = "structx.rs"]
pub mod structx;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
#[cfg(test)]
#[path = "tests_b2.rs"]
mod tests_b2;
#[cfg(test)]
#[path = "creation_fuzz.rs"]
mod creation_fuzz;
#[cfg(test)]
#[path = "tests_f2.rs"]
mod tests_f2;
#[cfg(test)]
#[path = "tests_rm4.rs"]
mod tests_rm4;
#[cfg(test)]
#[path = "tests_sk1.rs"]
mod tests_sk1;
#[path = "ui.rs"]
mod ui;
#[path = "zone.rs"]
mod zone;

pub use core_items::{core_rows, footprints as core_footprints, CoreEdit};
pub(crate) use core_items::{zone_anchor, MARK as ZONE_MARK};
pub use demo::demo;
pub(crate) use beam::{pick_rec as beam_pick_rec, tool_z as beam_tool_z};
pub(crate) use column::catalog_names as beam_catalog_names;
pub(crate) use grid::{is_horizontal, pick_names as grid_pick_names, pick_rec as grid_pick_rec};
pub(crate) use mep::size_label as mep_size_label;
pub(crate) use mep_color::{missing_label as mep_missing_label, swatch_color as mep_swatch_color, system_dropdown_w as mep_system_dropdown_w, system_items as mep_system_items, Mark as MepMark, SysRow as MepSysRow};
pub use layer::Layer;
pub use level::{overlay as level_overlay, place as level_place};
pub use ui::{paint, paint_layer};

use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::snap::{self, SnapKind};
use crate::temp_dims::{self, TempDim};
use crate::units::{parse_typed, Typed};
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model};
use ddc_ui::i18n::{Catalog, Lang};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Column,
    StructColumn,
    Beam,
    Roof,
    Ceiling,
    Stair,
    Railing,
    Shaft,
    Grid,
    RefPlane,
    Component,
    Duct,
    Pipe,
    Tray,
    Conduit,
    Terminal,
    Fixture,
    Light,
    Device,
    /// Mechanical and electrical equipment families only (`mep.equipment`).
    Equipment,
    Connect,
    Disconnect,
    Route,
    Footing,
    StripFooting,
    Raft,
    Brace,
    Truss,
    BeamSystem,
    /// Diagrid, gridshell or cable net on a dome, vault, hypar or plane over two corners.
    Gridshell,
    Floor,
    StructSlab,
    Ramp,
    RoomSep,
    Zone,
    Room,
}

/// How the tool collects points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Click = insert.
    Point,
    /// Two clicks (chain — the end becomes the next start).
    Two,
    /// Open polyline, Enter finishes.
    Path,
    /// Closed outline, Enter or a click on the first point.
    Loop,
}

impl Kind {
    pub const ALL: [Kind; 36] = [
        Kind::Column,
        Kind::StructColumn,
        Kind::Beam,
        Kind::Roof,
        Kind::Ceiling,
        Kind::Stair,
        Kind::Railing,
        Kind::Shaft,
        Kind::Grid,
        Kind::RefPlane,
        Kind::Component,
        Kind::Duct,
        Kind::Pipe,
        Kind::Tray,
        Kind::Conduit,
        Kind::Terminal,
        Kind::Fixture,
        Kind::Light,
        Kind::Device,
        Kind::Equipment,
        Kind::Connect,
        Kind::Disconnect,
        Kind::Route,
        Kind::Footing,
        Kind::StripFooting,
        Kind::Raft,
        Kind::Brace,
        Kind::Truss,
        Kind::BeamSystem,
        Kind::Gridshell,
        Kind::Floor,
        Kind::StructSlab,
        Kind::Ramp,
        Kind::RoomSep,
        Kind::Zone,
        Kind::Room,
    ];

    /// Key of hints `hint.<key>.<step>` and labels `tool.<key>`.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Column => "column",
            Kind::StructColumn => "scolumn",
            Kind::Beam => "beam",
            Kind::Roof => "roof",
            Kind::Ceiling => "ceiling",
            Kind::Stair => "stair",
            Kind::Railing => "railing",
            Kind::Shaft => "shaft",
            Kind::Grid => "grid",
            Kind::RefPlane => "refplane",
            Kind::Component => "component",
            Kind::Duct => "duct",
            Kind::Pipe => "pipe",
            Kind::Tray => "tray",
            Kind::Conduit => "conduit",
            Kind::Terminal => "terminal",
            Kind::Fixture => "fixture",
            Kind::Light => "light",
            Kind::Device => "device",
            Kind::Equipment => "equipment",
            Kind::Connect => "connect",
            Kind::Disconnect => "disconnect",
            Kind::Route => "route",
            Kind::Footing => "footing",
            Kind::StripFooting => "strip_footing",
            Kind::Raft => "raft",
            Kind::Brace => "brace",
            Kind::Truss => "truss",
            Kind::BeamSystem => "beam_system",
            Kind::Gridshell => "gridshell",
            Kind::Floor => "floor",
            Kind::StructSlab => "struct_slab",
            Kind::Ramp => "ramp",
            Kind::RoomSep => "room_sep",
            Kind::Zone => "zone",
            Kind::Room => "room",
        }
    }

    /// Ribbon commands that start the tool.
    pub fn commands(self) -> &'static [&'static str] {
        match self {
            Kind::Column => &["arch.column"],
            Kind::StructColumn => &["str.column"],
            Kind::Beam => &["str.beam"],
            Kind::Roof => &["arch.roof"],
            Kind::Ceiling => &["arch.ceiling"],
            Kind::Stair => &["arch.stair"],
            Kind::Railing => &["arch.railing"],
            Kind::Shaft => &["arch.shaft"],
            Kind::Grid => &["arch.grid", "str.grid", "mgr.grid"],
            Kind::RefPlane => &["arch.refplane", "str.refplane"],
            Kind::Component => &["arch.component"],
            Kind::Duct => &["mep.duct"],
            Kind::Pipe => &["mep.pipe"],
            Kind::Tray => &["mep.tray"],
            Kind::Conduit => &["mep.conduit"],
            Kind::Terminal => &["mep.terminal"],
            Kind::Fixture => &["mep.fixture"],
            Kind::Light => &["mep.light"],
            Kind::Device => &["mep.device"],
            Kind::Equipment => &["mep.equipment"],
            Kind::Connect => &["mep.connect"],
            Kind::Disconnect => &["mep.disconnect"],
            Kind::Route => &["mep.route"],
            Kind::Footing => &["str.footing"],
            Kind::StripFooting => &["str.strip_footing"],
            Kind::Raft => &["str.raft"],
            Kind::Brace => &["str.brace"],
            Kind::Truss => &["str.truss"],
            Kind::Gridshell => &["str.gridshell"],
            Kind::BeamSystem => &["str.beam_system"],
            Kind::Floor => &["arch.floor"],
            Kind::StructSlab => &["str.slab"],
            Kind::Ramp => &["arch.ramp"],
            Kind::RoomSep => &["arch.room_sep"],
            Kind::Zone => &["arch.zone"],
            Kind::Room => &["arch.room"],
        }
    }

    /// Esc removes the last point; for the others — the whole set.
    fn steps_back(self) -> bool {
        matches!(
            self,
            Kind::Floor
                | Kind::StructSlab
                | Kind::Ramp
                | Kind::RoomSep
                | Kind::Zone
                | Kind::Stair
        )
    }

    pub fn shape(self) -> Shape {
        match self {
            Kind::Column | Kind::StructColumn | Kind::Component | Kind::Footing => Shape::Point,
            Kind::Beam | Kind::Grid | Kind::RefPlane | Kind::Stair => Shape::Two,
            Kind::StripFooting | Kind::Brace | Kind::Truss | Kind::Gridshell => Shape::Two,
            Kind::Railing | Kind::Duct | Kind::Pipe | Kind::Tray | Kind::Conduit => Shape::Path,
            Kind::Terminal | Kind::Fixture | Kind::Light | Kind::Device | Kind::Equipment => {
                Shape::Point
            }
            Kind::Connect | Kind::Disconnect => Shape::Point,
            Kind::Route => Shape::Two,
            Kind::Roof | Kind::Ceiling | Kind::Shaft => Shape::Loop,
            Kind::Raft | Kind::BeamSystem => Shape::Loop,
            Kind::Floor | Kind::StructSlab => Shape::Loop,
            Kind::Ramp => Shape::Path,
            Kind::RoomSep => Shape::Two,
            Kind::Zone | Kind::Room => Shape::Point,
        }
    }

    /// Network run through points.
    pub fn is_run(self) -> bool {
        matches!(self, Kind::Duct | Kind::Pipe | Kind::Tray | Kind::Conduit)
    }

    /// The cursor snaps to network connections and segments.
    pub fn snaps_network(self) -> bool {
        self.is_run() || matches!(self, Kind::Connect | Kind::Disconnect | Kind::Route)
    }

    /// The click is handled by `mep::click_point`.
    pub fn is_mep_point(self) -> bool {
        matches!(
            self,
            Kind::Terminal
                | Kind::Fixture
                | Kind::Light
                | Kind::Device
                | Kind::Equipment
                | Kind::Connect
                | Kind::Disconnect
        )
    }
}

/// Option bar fields (used per tool).
#[derive(Clone, Debug, PartialEq)]
pub struct Opts {
    pub ty: usize,
    /// Bottom offset (column, shaft, roof base), m.
    pub base_off: f64,
    /// Top: offset from the top level or unconnected height, m.
    pub top_off: f64,
    pub rot_deg: f64,
    /// Axis elevation from the level (beam, networks), m.
    pub z: f64,
    pub chain: bool,
    pub overhang: f64,
    pub slope_deg: f64,
    /// Height (ceiling from the level, railing), m.
    pub height: f64,
    /// Width (flight, duct, tray), m; network section height, m.
    pub width: f64,
    pub depth: f64,
    pub dn: u32,
    /// Duct: round section (width is the diameter). Pipe: index into `PipeMaterial::ALL`.
    pub duct_round: bool,
    pub pipe_mat: u8,
    /// Network slope, %.
    pub slope: f64,
    /// Insulation of a new run, m.
    pub insulation: f64,
    /// Flight alignment: 0 centre, 1 left, 2 right.
    pub just: u8,
    pub auto_rail: bool,
    /// Stair shape: 0 straight, 1 L, 2 U; gap between the flights of a U stair, m.
    pub stair_shape: u8,
    pub gap: f64,
    /// Pick mode: ceiling — click inside a wall loop; railing — along a stair.
    pub pick: bool,
    /// Truss: panel count, web pattern (0 diagonal, 1 reverse, 2 triangular), gabled chord.
    pub panels: usize,
    pub web: u8,
    pub pitched: bool,
    /// Beam system: layout rule (index into `struct_props::LAYOUT_KEYS`), beam count
    /// for "fixed number", measured from the first picked outline edge.
    pub layout: u8,
    pub count: usize,
    pub first_edge: bool,
    /// Floor: outline from the selected walls, offset — `overhang`; Enter creates.
    pub walls: bool,
    /// Ramp: top is the next level up, otherwise height `top_off`.
    pub to_top: bool,
    /// Ramp: landing, maximum flight length, thickness, m.
    pub landing: f64,
    pub max_run: f64,
    pub thick: f64,
    /// Zone: name; empty — auto-name.
    pub name: String,
    /// Loop tools: sketch shape, polygon side count and inscribed/circumscribed.
    pub shape: loop_shapes::SketchShape,
    pub sides: usize,
    pub circum: bool,
}

impl Opts {
    fn for_kind(k: Kind, wall_h: f64) -> Opts {
        let mut o = Opts {
            ty: 0,
            base_off: 0.0,
            top_off: 0.0,
            rot_deg: 0.0,
            z: 0.0,
            chain: false,
            overhang: 0.5,
            slope_deg: 30.0,
            height: 2.7,
            width: 1.2,
            depth: 0.3,
            dn: 50,
            duct_round: false,
            pipe_mat: 0,
            slope: 0.0,
            insulation: 0.0,
            just: 0,
            auto_rail: true,
            stair_shape: 0,
            gap: 0.1,
            pick: false,
            panels: 6,
            web: 0,
            pitched: false,
            layout: 2,
            count: 5,
            first_edge: true,
            walls: false,
            to_top: false,
            landing: 1.5,
            max_run: 9.0,
            thick: 0.2,
            name: String::new(),
            shape: Default::default(),
            sides: 6,
            circum: false,
        };
        match k {
            Kind::Beam => {
                o.z = wall_h;
                o.chain = true;
            }
            Kind::Roof => o.base_off = wall_h,
            Kind::Room => o.height = wall_h,
            Kind::Ceiling => o.pick = true,
            Kind::Railing => o.height = 0.9,
            Kind::Grid => o.chain = false,
            Kind::Duct => {
                (o.width, o.depth, o.z) = (0.4, 0.25, 2.9);
            }
            Kind::Pipe => o.z = 2.6,
            Kind::Tray => {
                (o.width, o.depth, o.z) = (0.3, 0.1, 3.0);
            }
            Kind::Conduit => (o.dn, o.z) = (25, 3.0),
            Kind::Terminal | Kind::Light => o.z = 2.7,
            Kind::Device => o.z = 0.3,
            Kind::Equipment => {}
            Kind::Fixture | Kind::Connect | Kind::Disconnect => {}
            Kind::Route => {
                (o.width, o.depth, o.z) = (0.4, 0.25, 2.9);
            }
            Kind::Column | Kind::StructColumn | Kind::Shaft => o.top_off = wall_h,
            Kind::Floor | Kind::StructSlab => o.overhang = 0.0,
            Kind::Ramp => {
                o.slope = 100.0 / 12.0;
                o.top_off = 0.6;
            }
            Kind::RoomSep => o.chain = true,
            _ => structx::defaults(k, &mut o, wall_h),
        }
        o
    }
}

/// Active tool: collected points, cursor, options, temporary dimensions.
pub struct Active {
    pub kind: Kind,
    pub pts: Vec<V2>,
    /// Snapped cursor (or a point from typed input).
    pub cur: V2,
    pub opts: Opts,
    /// Type labels (selection — `opts.ty`).
    pub types: Vec<String>,
    /// Space: quarter turns (point elements) / flip (beam).
    pub quarter: u8,
    pub flip: bool,
    /// Roof: edge i defines the slope.
    pub slopes: Vec<bool>,
    pub dims: Vec<TempDim>,
    /// Temporary dimension open for input.
    pub edit: Option<usize>,
    pub cat: Catalog,
    pub level: i64,
    pub elevation: f64,
    /// Nearest level above: (id, elevation).
    pub above: Option<(i64, f64)>,
    pub invalid: Option<ddc_model::refusal::Refusal>,
    /// The pointer has been over the plan since the tool started; before that the preview
    /// would sit at the origin.
    pub seen: bool,
    /// Ceiling "auto": wall loop under the cursor.
    pub region: Vec<V2>,
    /// Collected elements: floor outline walls, zone rooms.
    pub picked: Vec<i64>,
    pub mep: mep::MepTool,
}

impl Active {
    pub fn new(kind: Kind, ed: &Editor, lang: Lang) -> Active {
        let levels = ed.model.level_list().unwrap_or_default();
        let above = levels
            .iter()
            .filter(|l| l.2 > ed.elevation + 1e-6)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|l| (l.0, l.2));
        let cat = Catalog::new(lang);
        Active {
            kind,
            pts: vec![],
            cur: v2(0.0, 0.0),
            opts: Opts::for_kind(kind, ed.height),
            types: match kind {
                Kind::Floor | Kind::StructSlab => floor::types(ed, kind),
                Kind::Component => component::type_labels(ed),
                _ => types(kind, &cat),
            },
            quarter: 0,
            flip: false,
            slopes: vec![],
            dims: vec![],
            edit: None,
            cat,
            level: ed.level,
            elevation: ed.elevation,
            above,
            invalid: None,
            seen: false,
            region: vec![],
            picked: vec![],
            mep: Default::default(),
        }
    }

    pub fn type_name(&self) -> &str {
        self.types.get(self.opts.ty).map_or("", |s| s.as_str())
    }

    /// Rotation of a point element, rad.
    pub fn rotation(&self) -> f64 {
        (self.opts.rot_deg + 90.0 * self.quarter as f64).to_radians()
    }

    pub fn t(&self, key: &str) -> String {
        self.cat.t(key)
    }

    /// Step for the hint `hint.<tool>.<step>`.
    pub fn step(&self) -> &'static str {
        if self.opts.walls {
            return "walls";
        }
        if self.opts.pick && matches!(self.kind, Kind::Floor | Kind::StructSlab | Kind::Roof) {
            return "pick";
        }
        match (self.kind.shape(), self.pts.len()) {
            (Shape::Point, _) => "place",
            (_, 0) => "first",
            (Shape::Two, 1) if self.kind == Kind::Stair && self.opts.stair_shape == 1 => "corner",
            (Shape::Two, 2) if self.kind == Kind::Stair => "side",
            (Shape::Two, _) => "end",
            _ => "next",
        }
    }

    pub fn hint(&self) -> String {
        self.t(&crate::shell::hint_key(self.kind.key(), self.step()))
    }
}

/// Default type names of a tool, in the language of the interface it was armed in.
fn types(kind: Kind, cat: &Catalog) -> Vec<String> {
    let with = |key: &str, tails: &[&str]| -> Vec<String> {
        let word = cat.t(key);
        tails.iter().map(|n| format!("{word} {n}")).collect()
    };
    match kind {
        Kind::Column => column::arch_names(cat),
        Kind::StructColumn | Kind::Beam => column::catalog_names(kind),
        Kind::Roof => with("agent_draft.type.roof", &["250", "300"]),
        Kind::Ceiling => with("agent_draft.type.ceiling", &["20", "50"]),
        Kind::Duct | Kind::Pipe | Kind::Tray | Kind::Conduit => vec![],
        Kind::Terminal | Kind::Fixture | Kind::Light | Kind::Device | Kind::Equipment => {
            mep::device_labels(kind)
        }
        Kind::Connect | Kind::Disconnect | Kind::Route => vec![],
        Kind::Stair => vec![cat.t("agent_draft.type.stair")],
        Kind::Railing => with("agent_draft.type.railing", &["900", "1100"]),
        Kind::Shaft => vec![cat.t("agent_draft.type.shaft")],
        Kind::Grid => vec![cat.t("agent_draft.type.axis")],
        Kind::RefPlane => vec![cat.t("agent_draft.type.refplane")],
        Kind::Footing
        | Kind::StripFooting
        | Kind::Raft
        | Kind::Brace
        | Kind::Truss
        | Kind::BeamSystem
        | Kind::Gridshell => structx::types(kind),
        // listed by `Active::new` from the model
        Kind::Floor
        | Kind::StructSlab
        | Kind::Component
        | Kind::Ramp
        | Kind::RoomSep
        | Kind::Zone
        | Kind::Room => vec![],
    }
}

/// Model write for undo/redo: the closure performs n model operations and returns n.
#[derive(Clone)]
pub struct Rec {
    pub label: &'static str,
    f: Arc<dyn Fn(&mut Model) -> Result<usize, Error>>,
    /// Model operations in the last application (undo reverts this many).
    pub n: usize,
}

impl std::fmt::Debug for Rec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rec({}, {})", self.label, self.n)
    }
}

impl Rec {
    pub fn new(
        label: &'static str,
        f: impl Fn(&mut Model) -> Result<usize, Error> + 'static,
    ) -> Rec {
        Rec {
            label,
            f: Arc::new(f),
            n: 0,
        }
    }

    /// An error midway undoes every op already made; the ops merge into one record.
    pub fn apply(&self, m: &mut Model) -> Result<Rec, Error> {
        let (_, wrote) = m.step(|m| (self.f)(m))?;
        Ok(Rec {
            n: usize::from(wrote),
            ..self.clone()
        })
    }
}

/// Floor type by name; if absent, one is created with a single layer (thickness from the name, mm). (id, operations).
pub(crate) fn ensure_slab_type(m: &mut Model, name: &str) -> Result<(i64, usize), Error> {
    let found: Option<i64> = m
        .sql()
        .query_row("SELECT id FROM slab_type WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .ok();
    if let Some(id) = found {
        return Ok((id, 0));
    }
    let mm: f64 = name
        .rsplit(' ')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200.0);
    let layer = ddc_model::LayerSpec {
        material: "concrete".into(),
        thickness: mm / 1000.0,
    };
    Ok((m.add_slab_type(name, &[layer])?, 1))
}

pub(crate) fn tup(p: V2) -> (f64, f64) {
    (p.x, p.y)
}

pub(crate) fn ccw(ring: &mut [V2]) {
    let a: f64 = (0..ring.len())
        .map(|i| ring[i].cross(ring[(i + 1) % ring.len()]))
        .sum();
    if a < 0.0 {
        ring.reverse();
    }
}

/// Activate a tool (from a ribbon command or a test).
pub fn arm(ed: &mut Editor, kind: Kind, lang: Lang) {
    // arm through the session: the wall stays unarmed, the selection goes
    ed.set_mode(Mode::Select);
    ed.selection.clear();
    let a = Active::new(kind, ed, lang);
    crate::interact::sessions::model_tool::begin(ed, a);
    ed.tool.profile = match kind {
        Kind::StructColumn => crate::elem::by_table("col").map_or(crate::snap::SnapProfile::ALL, |f| f.snap),
        _ => crate::snap::SnapProfile::ALL,
    };
    ed.status = None;
}

macro_rules! handlers {
    ($($f:ident => $k:expr),* $(,)?) => {
        $(fn $f(app: &mut EditorApp) {
            let lang = app.sh.cat.lang;
            arm(&mut app.ed, $k, lang);
        })*
        const HANDLERS: &[(Kind, fn(&mut EditorApp))] = &[$(($k, $f)),*];
    };
}

handlers! {
    h_column => Kind::Column,
    h_scolumn => Kind::StructColumn,
    h_beam => Kind::Beam,
    h_roof => Kind::Roof,
    h_ceiling => Kind::Ceiling,
    h_stair => Kind::Stair,
    h_railing => Kind::Railing,
    h_shaft => Kind::Shaft,
    h_grid => Kind::Grid,
    h_refplane => Kind::RefPlane,
    h_component => Kind::Component,
    h_duct => Kind::Duct,
    h_pipe => Kind::Pipe,
    h_tray => Kind::Tray,
    h_conduit => Kind::Conduit,
    h_terminal => Kind::Terminal,
    h_fixture => Kind::Fixture,
    h_light => Kind::Light,
    h_device => Kind::Device,
    h_equipment => Kind::Equipment,
    h_connect => Kind::Connect,
    h_disconnect => Kind::Disconnect,
    h_route => Kind::Route,
    h_footing => Kind::Footing,
    h_strip_footing => Kind::StripFooting,
    h_raft => Kind::Raft,
    h_brace => Kind::Brace,
    h_truss => Kind::Truss,
    h_beam_system => Kind::BeamSystem,
    h_gridshell => Kind::Gridshell,
    h_floor => Kind::Floor,
    h_struct_slab => Kind::StructSlab,
    h_ramp => Kind::Ramp,
    h_room_sep => Kind::RoomSep,
    h_zone => Kind::Zone,
    h_room => Kind::Room,
}

fn h_at_grids(app: &mut EditorApp) {
    let lang = app.sh.cat.lang;
    arm(&mut app.ed, Kind::StructColumn, lang);
    crate::interact::sessions::model_tool::with(&mut app.ed, |ed, a| column::at_grids(ed, a));
}

/// Registers the model tool commands in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    for (k, h) in HANDLERS {
        for id in k.commands() {
            app.register_command(id, *h);
        }
    }
    app.register_command("str.at_grids", h_at_grids);
    app.register_command("arch.room_auto", room::h_room_auto);
    host::register(app);
    structx::register(app);
    level::register(app);
    core_items::register(app);
}

/// Plan event for model tool `a` (its session keeps it); false — the tool ends.
pub fn step_active(ed: &mut Editor, a: &mut Active, e: Event) -> bool {
    step(ed, a, e)
}

fn step(ed: &mut Editor, a: &mut Active, e: Event) -> bool {
    match e {
        Event::Move(p) => track(ed, a, p),
        Event::Click(p) => {
            track(ed, a, p);
            let q = a.cur;
            click(ed, a, q);
        }
        Event::Press(_) | Event::Release(_) => {}
        Event::Char(' ') => space(a),
        Event::Char(c) => {
            if (c.is_ascii_digit() || ".,-+<@=*/()m".contains(c)) && ed.wall_draft().input.len() < 64 {
                ed.wall_draft_mut().input.push(c);
            }
        }
        Event::Key(Key::Tab) => {
            if !ed.tool.snaps.is_empty() {
                ed.tool.snap_idx = (ed.tool.snap_idx + 1) % ed.tool.snaps.len();
            }
            a.cur = ed.wall_snapped();
            refresh(ed, a);
        }
        Event::Key(Key::Backspace) => {
            if ed.wall_draft_mut().input.pop().is_none() {
                a.pts.pop();
                a.slopes.truncate(a.pts.len());
                a.mep.drop_last();
            }
        }
        Event::Key(Key::Escape) => {
            let typing = !ed.wall_draft().input.is_empty() || a.edit.is_some();
            if !typing && a.pts.is_empty() && a.picked.is_empty() {
                return false;
            }
            ed.wall_draft_mut().input.clear();
            if !(a.kind.steps_back() || (a.kind == Kind::Roof && a.opts.walls)) {
                a.pts.clear();
            } else if !typing {
                a.pts.pop();
                a.picked.pop();
            }
            a.slopes.truncate(a.pts.len());
            a.edit = None;
            a.mep.clear_path();
            refresh(ed, a);
        }
        Event::Key(Key::Enter) | Event::Key(Key::Close) => enter(ed, a),
        Event::Key(Key::Arc) => {
            if loop_shapes::supports(a.kind) {
                let s = a.opts.shape.next();
                loop_shapes::set_shape(a, s);
                refresh(ed, a);
            }
        }
    }
    true
}

/// Plan reference lines: grid lines, reference planes, wall axes.
fn datum_lines(ed: &Editor) -> Vec<(V2, V2, SnapKind)> {
    let l = &ed.tool.layer;
    l.grids
        .iter()
        .map(|g| (g.1, g.2, SnapKind::GridAxis))
        .chain(l.refs.iter().map(|r| (r.0, r.1, SnapKind::RefPlane)))
        .chain(l.seps.iter().map(|s| (s.0, s.1, SnapKind::WallAxis)))
        .chain(ed.walls.iter().map(|w| (w.a, w.b, SnapKind::WallAxis)))
        .collect()
}

fn track(ed: &mut Editor, a: &mut Active, p: V2) {
    ed.tool.layer.sync(&ed.model, ed.level, ed.revision);
    let z = ed.tool.snap_state.z;
    ed.tool.track_chain(p, crate::wall_tool::Chain::of(&a.pts, z, false));
    let extra = snap::datum_candidates(&datum_lines(ed), p, ed.tool.radius);
    snap::merge(&mut ed.tool.snaps, extra, p);
    ed.tool.snap_idx = 0;
    a.cur = ed.wall_snapped();
    a.seen = true;
    mep::snap(ed, a, p);
    refresh(ed, a);
}

/// Temporary dimensions and validity of the current preview.
fn refresh(ed: &Editor, a: &mut Active) {
    let refs: Vec<(V2, V2)> = datum_lines(ed).iter().map(|l| (l.0, l.1)).collect();
    a.dims = match (a.kind.shape(), a.pts.last()) {
        _ if matches!(a.kind, Kind::Zone | Kind::Room) || a.opts.walls => vec![],
        (Shape::Point, _) | (_, None) => temp_dims::for_point(a.cur, &refs),
        (_, Some(&l)) => temp_dims::for_segment(l, a.cur, &refs),
    };
    let old_region = std::mem::take(&mut a.region);
    a.region = match (a.kind, a.opts.pick, a.pts.is_empty()) {
        (Kind::Ceiling, true, true) => ceiling::region_at(ed, a.cur).map_or(vec![], |r| r.outer),
        (Kind::Room, _, _) => ceiling::region_at(ed, a.cur).map_or(vec![], |r| r.outer),
        (Kind::Floor | Kind::StructSlab, true, true) => {
            floor::region_at(ed, a.cur).map_or(vec![], |r| r.outer)
        }
        (Kind::Roof, true, true) if !a.opts.walls => roof::footprint_at(ed, a.cur).unwrap_or_default(),
        _ if a.opts.walls && a.kind == Kind::Roof => roof::walls_ring(ed, a).unwrap_or_default(),
        _ if a.opts.walls => floor::walls_ring(ed, a).unwrap_or_default(),
        _ => vec![],
    };
    if a.kind == Kind::Roof && a.region != old_region {
        a.slopes.clear();
    }
    if a.edit.is_some_and(|i| i >= a.dims.len()) {
        a.edit = None;
    }
    a.invalid = match a.kind {
        Kind::Stair if a.above.is_none() => Some(crate::refusals::level_none_above()),
        Kind::Component if a.types.is_empty() => Some(crate::refusals::component_no_library()),
        Kind::Ramp if a.opts.to_top && a.above.is_none() => Some(crate::refusals::level_none_above()),
        _ => None,
    };
}

/// Point from a typed value: open dimension, length/angle from the last point, or `dx;dy`.
fn typed_point(ed: &Editor, a: &Active) -> Option<V2> {
    let t = parse_typed(&ed.wall_draft().input, ed.tool.units, ed.tool.mark).ok()?;
    let dim = a.edit.or(match a.kind.shape() {
        Shape::Point => Some(0),
        _ => None,
    });
    if let (Some(i), Typed::Polar(Some(l), None)) = (dim, &t) {
        return a.dims.get(i).map(|d| d.retarget(*l));
    }
    if let (Typed::Polar(Some(l), None), Some(sn)) = (&t, ed.tool.snap()) {
        let acq = ed.tool.snap_state.acquired_points();
        if let Some(q) = crate::snap_core::point_along_guide(&acq, &sn, a.cur, *l) {
            return Some(q);
        }
    }
    let last = *a.pts.last()?;
    match t {
        Typed::Polar(Some(l), ang) => {
            let v = a.cur - last;
            let d = match ang {
                Some(g) => v2(g.to_radians().cos(), g.to_radians().sin()),
                None if v.len() > 1e-9 => v * (1.0 / v.len()),
                None => v2(1.0, 0.0),
            };
            Some(last + d * l)
        }
        Typed::Pair(dx, dy) => Some(last + v2(dx, dy)),
        Typed::Polar(None, _) => None,
    }
}

fn enter(ed: &mut Editor, a: &mut Active) {
    if !ed.wall_draft().input.is_empty() {
        match typed_point(ed, a) {
            Some(q) => {
                ed.wall_draft_mut().input.clear();
                a.edit = None;
                a.cur = q;
                click(ed, a, q);
            }
            None => {
                // the parser's reason when there is one: a refused comma says what to type
                let (s, u, mark) = (&ed.wall_draft().input, ed.tool.units, ed.tool.mark);
                let ru = mark == crate::units::DecimalMark::Comma;
                let why = parse_typed(s, u, mark).err().map(|e| crate::units::typed_error_text(&e, s, ru));
                let head = a.t("tool.err.input");
                ed.status = Some(match why {
                    Some(w) => format!("{head}: {w}"),
                    None => head,
                });
            }
        }
        return;
    }
    match a.kind.shape() {
        Shape::Path | Shape::Loop => finish(ed, a),
        Shape::Two => a.pts.clear(),
        Shape::Point if a.kind == Kind::Zone => {
            let r = zone::rec(a);
            if commit(ed, a, r) {
                a.picked.clear();
                // otherwise the next zone would get the same name
                a.opts.name.clear();
            }
        }
        Shape::Point => {}
    }
}

fn space(a: &mut Active) {
    match a.kind {
        Kind::Beam | Kind::Stair => a.flip = !a.flip,
        Kind::Roof if a.opts.walls || a.opts.pick => roof::toggle_hovered(a),
        Kind::Roof => {
            if let Some(s) = a.slopes.last_mut() {
                *s = !*s;
            }
        }
        _ => a.quarter = (a.quarter + 1) % 4,
    }
}

fn click(ed: &mut Editor, a: &mut Active, q: V2) {
    if let Some(r) = a.invalid.clone() {
        ed.refuse(r);
        return;
    }
    if a.kind == Kind::Zone {
        return zone::toggle(ed, a, q);
    }
    if a.kind == Kind::Room {
        let r = room::pick(ed, a, q);
        commit(ed, a, r);
        return;
    }
    if a.opts.walls {
        a.pts.clear();
        a.slopes.clear();
        floor::toggle_wall(ed, a, q);
        return refresh(ed, a);
    }
    match a.kind.shape() {
        Shape::Point => {
            if a.kind.is_mep_point() {
                return mep::click_point(ed, a, q);
            }
            let r = match a.kind {
                Kind::Component => component::rec(ed, a, q),
                Kind::Footing => structx::footing(ed, a, q),
                _ => column::rec(a, &[q]),
            };
            commit(ed, a, r);
        }
        Shape::Two => {
            if a.kind == Kind::Route {
                return mep::route_click(ed, a, q);
            }
            if a.kind == Kind::StripFooting && a.opts.pick {
                let r = structx::strip_pick(ed, a, q);
                commit(ed, a, r);
                return;
            }
            if a.kind == Kind::Stair && stair::is_multi(a) {
                return stair::click_multi(ed, a, q);
            }
            let Some(&p0) = a.pts.first() else {
                a.pts.push(q);
                return;
            };
            if (q - p0).len() < crate::wall_tool::MIN_LEN {
                ed.status = Some(a.t("tool.err.short"));
                return;
            }
            let (s, e) = if a.flip { (q, p0) } else { (p0, q) };
            let r = match a.kind {
                Kind::Beam => beam::rec(a, s, e),
                Kind::Grid => grid::rec(a, p0, q),
                Kind::RefPlane => refplane::rec(a, p0, q),
                Kind::StripFooting => structx::strip_free(a, p0, q),
                Kind::Brace => structx::brace(ed, a, s, e),
                Kind::Truss => structx::truss(a, s, e),
                Kind::Gridshell => structx::gridshell(a, p0, q),
                Kind::RoomSep => room_sep::rec(a, p0, q),
                _ => stair::rec(a, p0, q),
            };
            let ok = commit(ed, a, r);
            a.pts.clear();
            if ok && a.opts.chain {
                a.pts.push(q);
            }
        }
        Shape::Path | Shape::Loop => {
            if loop_shapes::active(a) {
                return loop_shapes::click(ed, a, q);
            }
            if a.pts.is_empty() && a.opts.pick {
                let r = match a.kind {
                    Kind::Ceiling => ceiling::pick(ed, a, q),
                    Kind::Railing => railing::pick(ed, a, q),
                    Kind::Floor | Kind::StructSlab => floor::pick(ed, a, q),
                    Kind::Roof => roof::pick(ed, a, q),
                    _ => Err(a.t("tool.err.pick")),
                };
                commit(ed, a, r);
                return;
            }
            let close = a.kind.shape() == Shape::Loop
                && a.pts.len() >= 3
                && (q - a.pts[0]).len() <= ed.tool.radius.max(1e-3);
            if close {
                return finish(ed, a);
            }
            if a.pts
                .last()
                .is_some_and(|l| (*l - q).len() < crate::wall_tool::MIN_LEN)
            {
                return;
            }
            let ends = a.kind.is_run() && mep::on_point(a);
            a.pts.push(q);
            a.slopes.push(true);
            if ends {
                finish(ed, a);
            }
        }
    }
}

fn finish(ed: &mut Editor, a: &mut Active) {
    if a.opts.walls {
        let r = if a.kind == Kind::Roof {
            roof::from_walls(ed, a)
        } else {
            floor::from_walls(ed, a)
        };
        if commit(ed, a, r) {
            a.picked.clear();
            a.region.clear();
            a.slopes.clear();
        }
        return;
    }
    let need = if a.kind.shape() == Shape::Loop { 3 } else { 2 };
    if a.pts.len() < need {
        ed.status = Some(a.t(if need == 3 {
            "tool.err.open_loop"
        } else {
            "tool.err.short"
        }));
        return;
    }
    let pts = a.pts.clone();
    if a.kind.is_run() {
        if mep::draw(ed, a, &pts) {
            a.pts.clear();
            a.slopes.clear();
        }
        return;
    }
    let r = match a.kind {
        Kind::Roof => roof::rec(a, &pts),
        Kind::Ceiling => ceiling::rec(a, &pts),
        Kind::Shaft => shaft::rec(a, &pts),
        Kind::Railing => railing::rec(a, &pts),
        Kind::Raft => structx::raft(a, &pts),
        Kind::Floor | Kind::StructSlab => floor::rec(a, &pts),
        Kind::Ramp => ramp::rec(a, &pts),
        _ => structx::system(a, &pts),
    };
    if commit(ed, a, r) {
        a.pts.clear();
        a.slopes.clear();
    }
}

/// Write (one undo step); the message goes to the status bar. true — written.
fn commit(ed: &mut Editor, a: &Active, r: Result<Rec, String>) -> bool {
    let r = match r {
        Ok(r) => r,
        Err(m) => {
            ed.status = Some(m);
            return false;
        }
    };
    match ed.apply_ext(r) {
        Ok(()) => {
            ed.status = Some(a.cat.tf(
                "tool.done",
                &[("what", &a.t(&format!("tool.{}", a.kind.key())))],
            ));
            ed.tool.layer.sync(&ed.model, ed.level, ed.revision);
            true
        }
        Err(e) => {
            ed.status = Some(format!("{}: {e}", a.t("tool.err.model")));
            false
        }
    }
}
