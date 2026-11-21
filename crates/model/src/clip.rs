// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Clipboard: a snapshot of selected elements (rows of all their tables, lookups by name,
//! levels by elevation) and paste with renumbering into the same or another model as one journal entry.
//! Columns come from the schema, so new columns are copied without changes to this file.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{params_from_iter, types::Value, Connection, OptionalExtension};
use serde_json::{json, Value as J};

use crate::annot::{parse_ref, refs_text, DimRef};
use crate::mass::Form;
use crate::ops::{from_json, to_json, Inv, Snapshots};
use crate::{Error, Model};

mod mat;
pub(crate) mod reg;

const MARK: &str = "DDC-CLIP/1\n";
const GHOST_MAX: usize = 20_000;
/// OS clipboard text longer than this is not parsed: parsing runs on the UI thread.
pub const CLIP_TEXT_MAX: usize = 64 * 1024 * 1024;
/// From this many parents on, one pass over the table is cheaper than a query per parent.
const SCAN_AT: usize = 256;
const LEVEL_TOL: f64 = 1e-3;
const REHOST_DIST: f64 = 0.5;
const IDENT: [[f64; 2]; 2] = [[1.0, 0.0], [0.0, 1.0]];

/// Text columns with coordinates or references inside.
#[derive(Clone, Copy, PartialEq)]
enum Tx {
    Slope,
    PairsJson,
    Ring,
    Lines3,
    Points3,
    Sketch,
    Shape,
    DimRefs,
    Hosts,
    Walls,
    Form,
    /// `x,y;x,y`: ramp axis.
    CommaPairs,
    /// Annotation JSON: points under known keys.
    AnnoJson,
    /// JSON of a spline wall axis (control points).
    SplineAxis,
}

/// How a table takes part in the snapshot: parent, levels, lookups, coordinates.
struct Spec {
    t: &'static str,
    id: Option<&'static str>,
    /// (column, table): the row follows its parent; for a missing parent see `place`.
    owns: &'static [(&'static str, &'static str)],
    /// Optional references: a new copy, a live element of the same model, or NULL.
    refs: &'static [(&'static str, &'static str)],
    /// (kind column, reference column): the kind equals the table name unless `kinds` is given.
    poly: Option<(&'static str, &'static str)>,
    /// (kind, table) for `poly`, when kinds are not stored as table names.
    kinds: &'static [(&'static str, &'static str)],
    levels: &'static [&'static str],
    /// Top levels: must stay above the base after the move.
    tops: &'static [&'static str],
    dicts: &'static [(&'static str, &'static str)],
    xy: &'static [(&'static str, &'static str)],
    tx: &'static [(&'static str, Tx)],
    /// Outline vertices: adjacent rows of one parent are joined in the ghost preview.
    chain: bool,
}

const B: Spec = Spec {
    t: "",
    id: None,
    owns: &[],
    refs: &[],
    poly: None,
    kinds: &[],
    levels: &[],
    tops: &[],
    dicts: &[],
    xy: &[],
    tx: &[],
    chain: false,
};
const ID: Option<&str> = Some("id");
const L: &[&str] = &["level"];
const AB: &[(&str, &str)] = &[("ax", "ay"), ("bx", "by")];
const XY: &[(&str, &str)] = &[("x", "y")];

/// Dependency order: parent before child, roof and slab before wall.
const SPECS: &[Spec] = &[
    Spec {
        t: "roof",
        id: ID,
        levels: L,
        dicts: &[("stype", "slab_type")],
        xy: AB,
        ..B
    },
    Spec {
        t: "roof_vertex",
        owns: &[("roof", "roof")],
        xy: XY,
        chain: true,
        ..B
    },
    Spec {
        t: "roof_ext",
        owns: &[("roof", "roof")],
        refs: &[("join_roof", "roof")],
        ..B
    },
    Spec {
        t: "slab",
        id: ID,
        levels: L,
        dicts: &[("stype", "slab_type")],
        tx: &[("slope", Tx::Slope)],
        ..B
    },
    Spec {
        t: "slab_vertex",
        owns: &[("slab", "slab")],
        xy: XY,
        chain: true,
        ..B
    },
    Spec {
        t: "slab_shape",
        owns: &[("slab", "slab")],
        tx: &[("data", Tx::Shape)],
        ..B
    },
    Spec {
        t: "wall",
        id: ID,
        levels: L,
        dicts: &[("wtype", "wall_type")],
        refs: &[("top_roof", "roof")],
        xy: AB,
        ..B
    },
    Spec {
        t: "opening",
        id: ID,
        owns: &[("wall", "wall")],
        ..B
    },
    Spec {
        t: "wall_profile",
        owns: &[("wall", "wall")],
        ..B
    },
    Spec {
        t: "wall_join",
        owns: &[("wall", "wall")],
        ..B
    },
    Spec {
        t: "wall_sweep",
        id: ID,
        owns: &[("wall", "wall")],
        ..B
    },
    Spec {
        t: "wall_stack",
        owns: &[("wall", "wall")],
        dicts: &[("stack", "stack_type")],
        ..B
    },
    Spec {
        t: "wall_attach",
        owns: &[("wall", "wall")],
        refs: &[("slab", "slab")],
        ..B
    },
    Spec {
        t: "wall_attach_level",
        owns: &[("wall", "wall")],
        levels: L,
        ..B
    },
    Spec {
        t: "curtain_wall",
        id: ID,
        levels: L,
        xy: AB,
        ..B
    },
    Spec {
        t: "curtain_panel",
        owns: &[("cw", "curtain_wall")],
        ..B
    },
    Spec {
        t: "curtain_edit",
        owns: &[("cw", "curtain_wall")],
        ..B
    },
    Spec {
        t: "room",
        id: ID,
        levels: L,
        xy: &[("px", "py")],
        tx: &[("outline", Tx::PairsJson)],
        ..B
    },
    Spec {
        t: "room_sep",
        id: ID,
        levels: L,
        xy: AB,
        ..B
    },
    Spec {
        t: "mesh",
        id: ID,
        levels: L,
        ..B
    },
    Spec {
        t: "view_def",
        id: ID,
        xy: AB,
        ..B
    },
    Spec {
        t: "col",
        id: ID,
        levels: L,
        tops: &["top_level"],
        dicts: &[("mtype", "member_type")],
        xy: &[("x", "y"), ("tx", "ty")],
        ..B
    },
    Spec {
        t: "beam",
        id: ID,
        levels: L,
        dicts: &[("mtype", "member_type")],
        xy: AB,
        ..B
    },
    Spec {
        t: "skylight",
        id: ID,
        owns: &[("roof", "roof")],
        xy: XY,
        ..B
    },
    Spec {
        t: "soffit",
        id: ID,
        owns: &[("roof", "roof")],
        ..B
    },
    Spec {
        t: "sweep",
        id: ID,
        levels: L,
        tx: &[("hosts", Tx::Hosts)],
        ..B
    },
    Spec {
        t: "roof_taper",
        id: ID,
        levels: L,
        owns: &[("slab", "slab")],
        tx: &[("ring", Tx::PairsJson)],
        ..B
    },
    Spec {
        t: "roof_drain",
        id: ID,
        owns: &[("taper", "roof_taper")],
        xy: XY,
        ..B
    },
    Spec {
        t: "grid",
        id: ID,
        tops: &["base_level", "top_level"],
        xy: AB,
        ..B
    },
    Spec {
        t: "ref_plane",
        id: ID,
        xy: AB,
        ..B
    },
    Spec {
        t: "shaft",
        id: ID,
        levels: L,
        tops: &["top_level"],
        ..B
    },
    Spec {
        t: "shaft_vertex",
        owns: &[("shaft", "shaft")],
        xy: XY,
        chain: true,
        ..B
    },
    Spec {
        t: "ceiling",
        id: ID,
        levels: L,
        dicts: &[("stype", "slab_type")],
        ..B
    },
    Spec {
        t: "ceiling_vertex",
        owns: &[("ceiling", "ceiling")],
        xy: XY,
        chain: true,
        ..B
    },
    Spec {
        t: "stair",
        id: ID,
        levels: L,
        tops: &["top_level"],
        xy: XY,
        ..B
    },
    Spec {
        t: "railing",
        id: ID,
        levels: L,
        owns: &[("stair", "stair")],
        ..B
    },
    Spec {
        t: "railing_vertex",
        owns: &[("railing", "railing")],
        xy: XY,
        chain: true,
        ..B
    },
    Spec {
        t: "comp",
        id: ID,
        levels: L,
        dicts: &[("ctype", "comp_type")],
        owns: &[
            ("host_wall", "wall"),
            ("host_slab", "slab"),
            ("host_ceiling", "ceiling"),
        ],
        ..B
    },
    Spec {
        t: "mep_run",
        id: ID,
        levels: L,
        dicts: &[("system", "mep_system")],
        tx: &[("points", Tx::Points3)],
        ..B
    },
    Spec {
        t: "mass",
        id: ID,
        levels: L,
        tx: &[("form", Tx::Form)],
        ..B
    },
    Spec {
        t: "mass_paint",
        owns: &[("mass", "mass")],
        ..B
    },
    Spec {
        t: "mass_host",
        poly: Some(("kind", "ref")),
        levels: L,
        ..B
    },
    Spec {
        t: "solid",
        id: ID,
        levels: L,
        ..B
    },
    Spec {
        t: "solid_host",
        owns: &[("solid", "solid")],
        ..B
    },
    Spec {
        t: "host_opening",
        id: ID,
        poly: Some(("host_kind", "host")),
        ..B
    },
    Spec {
        t: "iso_footing",
        id: ID,
        levels: L,
        owns: &[("col", "col")],
        xy: XY,
        ..B
    },
    Spec {
        t: "strip_footing",
        id: ID,
        levels: L,
        owns: &[("wall", "wall")],
        xy: AB,
        ..B
    },
    Spec {
        t: "raft",
        id: ID,
        levels: L,
        ..B
    },
    Spec {
        t: "raft_vertex",
        owns: &[("raft", "raft")],
        xy: XY,
        chain: true,
        ..B
    },
    Spec {
        t: "brace",
        id: ID,
        levels: L,
        dicts: &[("mtype", "member_type")],
        xy: AB,
        ..B
    },
    Spec {
        t: "truss",
        id: ID,
        levels: L,
        dicts: &[("chord_type", "member_type"), ("web_type", "member_type")],
        xy: AB,
        ..B
    },
    Spec {
        t: "beam_system",
        id: ID,
        levels: L,
        dicts: &[("mtype", "member_type")],
        ..B
    },
    Spec {
        t: "beam_system_vertex",
        owns: &[("sys", "beam_system")],
        xy: XY,
        chain: true,
        ..B
    },
    Spec {
        t: "toposolid",
        id: ID,
        tx: &[("boundary", Tx::Ring), ("breaklines", Tx::Lines3)],
        ..B
    },
    Spec {
        t: "topo_point",
        owns: &[("topo", "toposolid")],
        xy: XY,
        ..B
    },
    Spec {
        t: "topo_region",
        id: ID,
        owns: &[("topo", "toposolid")],
        tx: &[("ring", Tx::Ring)],
        ..B
    },
    Spec {
        t: "topo_pad",
        id: ID,
        owns: &[("topo", "toposolid")],
        tx: &[("outline", Tx::Ring)],
        ..B
    },
    Spec {
        t: "d_line",
        id: ID,
        dicts: &[("layer", "d_layer"), ("block", "d_block")],
        xy: &[("x1", "y1"), ("x2", "y2")],
        ..B
    },
    Spec {
        t: "d_polyline",
        id: ID,
        dicts: &[("layer", "d_layer"), ("block", "d_block")],
        ..B
    },
    Spec {
        t: "d_vertex",
        owns: &[("polyline", "d_polyline")],
        xy: XY,
        chain: true,
        ..B
    },
    Spec {
        t: "d_arc",
        id: ID,
        dicts: &[("layer", "d_layer"), ("block", "d_block")],
        xy: &[("cx", "cy")],
        ..B
    },
    Spec {
        t: "d_text",
        id: ID,
        dicts: &[("layer", "d_layer"), ("block", "d_block")],
        xy: XY,
        ..B
    },
    Spec {
        t: "d_insert",
        id: ID,
        dicts: &[("layer", "d_layer"), ("def", "d_block"), ("block", "d_block")],
        xy: XY,
        ..B
    },
    Spec {
        t: "d_attr",
        owns: &[("insert_id", "d_insert")],
        ..B
    },
];

/// Attached rows: they come after all elements, including the track tables from `reg`.
const TAIL: &[Spec] = &[
    Spec {
        t: "prop",
        poly: Some(("kind", "ref")),
        ..B
    },
    Spec {
        t: "pparam_val",
        poly: Some(("kind", "ref")),
        dicts: &[("param", "pparam")],
        ..B
    },
    Spec {
        t: "sketch",
        poly: Some(("host_kind", "host")),
        tx: &[("data", Tx::Sketch)],
        ..B
    },
    Spec {
        t: "elem_phase",
        poly: Some(("kind", "ref")),
        dicts: &[("created", "phase"), ("demolished", "phase")],
        ..B
    },
    Spec {
        t: "datum_extent",
        id: ID,
        poly: Some(("kind", "ref")),
        xy: AB,
        ..B
    },
    Spec {
        t: "dim",
        id: ID,
        levels: L,
        tx: &[("refs", Tx::DimRefs)],
        ..B
    },
    Spec {
        t: "constraint_rule",
        id: ID,
        tx: &[("walls", Tx::Walls)],
        ..B
    },
    Spec {
        t: "geom_join",
        ..B
    },
];

/// Lookup: found by key, created with its children when moved to another model.
struct Dict {
    t: &'static str,
    key: &'static [&'static str],
    /// (column, lookup) pairs resolved before this one.
    fks: &'static [(&'static str, &'static str)],
    children: &'static [(
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
    )],
    create: bool,
}

const DICTS: &[Dict] = &[
    Dict {
        t: "wall_type",
        key: &["name"],
        fks: &[],
        children: &[
            ("wall_type_layer", "wtype", &[]),
            ("wall_region", "wtype", &[]),
            ("wall_wrap", "wtype", &[]),
            ("wall_layer_flag", "wtype", &[]),
        ],
        create: true,
    },
    Dict {
        t: "slab_type",
        key: &["name"],
        fks: &[],
        children: &[("slab_type_layer", "stype", &[])],
        create: true,
    },
    Dict {
        t: "member_type",
        key: &["name"],
        fks: &[],
        children: &[],
        create: true,
    },
    Dict {
        t: "comp_family",
        key: &["name"],
        fks: &[],
        children: &[],
        create: true,
    },
    Dict {
        t: "comp_type",
        key: &["family", "name"],
        fks: &[("family", "comp_family")],
        children: &[],
        create: true,
    },
    Dict {
        t: "mep_system",
        key: &["name"],
        fks: &[],
        children: &[],
        create: true,
    },
    Dict {
        t: "stack_type",
        key: &["name"],
        fks: &[],
        children: &[("stack_part", "stack", &[("wtype", "wall_type")])],
        create: true,
    },
    Dict {
        t: "d_layer",
        key: &["name"],
        fks: &[],
        children: &[],
        create: true,
    },
    // block geometry lives in the same d_* tables: a block is carried over by name only
    Dict {
        t: "d_block",
        key: &["name"],
        fks: &[],
        children: &[],
        create: false,
    },
    Dict {
        t: "pparam",
        key: &["name"],
        fks: &[],
        children: &[("pparam_cat", "param", &[])],
        create: true,
    },
    Dict {
        t: "sweep_profile",
        key: &["name"],
        fks: &[],
        children: &[],
        create: true,
    },
    // stages of a foreign model are not created: without a name match the copy gets the current one
    Dict {
        t: "phase",
        key: &["name"],
        fks: &[],
        children: &[],
        create: false,
    },
];

fn specs() -> impl Iterator<Item = &'static Spec> {
    SPECS
        .iter()
        .chain(reg::LANE_SPECS.iter())
        .chain(TAIL.iter())
}

/// Every table of the registry; the element matrix checks that each one is accounted for.
pub fn clip_tables() -> Vec<&'static str> {
    specs().map(|s| s.t).collect()
}

fn dict_specs() -> impl Iterator<Item = &'static Dict> {
    DICTS.iter().chain(reg::LANE_DICTS.iter())
}

/// Table by the value of a kind column.
fn kind_table(s: &Spec, kind: &str) -> Option<&'static str> {
    if s.kinds.is_empty() {
        return specs().find(|x| x.t == kind).map(|x| x.t);
    }
    s.kinds.iter().find(|x| x.0 == kind).map(|x| x.1)
}

/// Constraints with no foreign levels or grids: carried over if all walls are in the snapshot.
const CONSTRAINT_KINDS: [&str; 8] = [
    "lock_len",
    "lock_dist",
    "eq",
    "align",
    "parallel",
    "perp",
    "dist",
    "dist_step",
];
/// Tables whose elements the geometry can join.
const JOIN_KINDS: [&str; 7] = [
    "wall",
    "slab",
    "roof",
    "curtain_wall",
    "col",
    "beam",
    "mass",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipRows {
    pub cols: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// A group instance captured whole in the snapshot: paste creates a new instance of the same group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipInst {
    pub group: String,
    pub def: String,
    pub level: i64,
    pub at: (f64, f64),
    pub members: Vec<(String, i64, i64)>,
}

/// Snapshot of the selection. Holds no references to the model and survives a project switch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Clip {
    pub project: String,
    pub base_level: Option<i64>,
    /// (id, name, elevation) of all referenced levels.
    pub levels: Vec<(i64, String, f64)>,
    pub tables: Vec<(String, ClipRows)>,
    pub dicts: Vec<(String, ClipRows)>,
    pub roots: Vec<(String, i64)>,
    /// (xmin, ymin, xmax, ymax); the paste anchor point is its centre.
    pub bbox: Option<(f64, f64, f64, f64)>,
    pub ghost: Vec<[f64; 4]>,
    /// Walls outside the snapshot that host standalone openings and components.
    pub hosts: Vec<ClipHost>,
    pub instances: Vec<ClipInst>,
    /// Model schema version the snapshot was taken from.
    pub schema: i64,
    /// Selected items the clipboard cannot handle: not in the snapshot, the caller reports them.
    pub unsupported: Vec<(String, i64)>,
    /// Group definition: dependent elements deliberately removed from it.
    pub except: Vec<(String, i64)>,
    /// Material rows, their images and mesh materials by name; used by a paste into another project.
    pub materials: Vec<(String, ClipRows)>,
}

/// (id, ax, ay, bx, by, level) of the host wall.
pub type ClipHost = (i64, f64, f64, f64, f64, Option<i64>);

/// What the OS text clipboard holds.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipText {
    /// Foreign text.
    Foreign,
    /// The snapshot is too large for the OS clipboard: a tag of the app instance holding it in memory.
    Marker(String),
    Clip(Box<Clip>),
}

/// Where the snapshot levels land.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlaceLevel {
    /// The same levels; in another model, matched by elevation, missing ones are created.
    Same,
    /// The lowest snapshot level lands on this one, the rest by elevation difference.
    Base(i64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PasteOpts {
    pub dx: f64,
    pub dy: f64,
    pub level: PlaceLevel,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PasteReport {
    /// New copies of the selected elements: (table, id).
    pub created: Vec<(String, i64)>,
    /// What could not be placed: (table, old id, reason).
    pub skipped: Vec<(String, i64, String)>,
    /// Material names created in the target project by a paste from another one.
    pub materials_created: Vec<String>,
    /// Names that already exist in the target with other properties; the target row was kept.
    pub materials_differ: Vec<String>,
}

impl Clip {
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Snapshot centre: the point that lands under the cursor on paste.
    pub fn center(&self) -> (f64, f64) {
        self.bbox
            .map_or((0.0, 0.0), |b| ((b.0 + b.2) * 0.5, (b.1 + b.3) * 0.5))
    }

    pub(crate) fn rows(&self, t: &str) -> Option<&ClipRows> {
        self.tables.iter().find(|(n, _)| n == t).map(|(_, r)| r)
    }

    fn dict_rows(&self, t: &str) -> Option<&ClipRows> {
        self.dicts.iter().find(|(n, _)| n == t).map(|(_, r)| r)
    }

    /// Text for the system clipboard: a tag and JSON; another app instance reads it on its own.
    pub fn to_text(&self) -> String {
        let tables = |v: &[(String, ClipRows)]| -> J {
            J::Array(v.iter().map(|(n, r)| json!([n, rows_json(r)])).collect())
        };
        let body = json!({
            "project": self.project,
            "base": self.base_level,
            "levels": self.levels.iter().map(|l| json!([l.0, l.1, l.2])).collect::<Vec<_>>(),
            "tables": tables(&self.tables),
            "dicts": tables(&self.dicts),
            "materials": tables(&self.materials),
            "roots": self.roots.iter().map(|r| json!([r.0, r.1])).collect::<Vec<_>>(),
            "bbox": self.bbox.map(|b| vec![b.0, b.1, b.2, b.3]),
            "ghost": self.ghost.iter().map(|g| g.to_vec()).collect::<Vec<_>>(),
            "hosts": self.hosts.iter().map(|h| json!([h.0, h.1, h.2, h.3, h.4, h.5])).collect::<Vec<_>>(),
            "schema": self.schema,
            "unsupported": self.unsupported.iter().map(|r| json!([r.0, r.1])).collect::<Vec<_>>(),
            "except": self.except.iter().map(|r| json!([r.0, r.1])).collect::<Vec<_>>(),
            "instances": self.instances.iter().map(|i| json!({
                "group": i.group, "def": i.def, "level": i.level, "at": [i.at.0, i.at.1],
                "members": i.members.iter().map(|m| json!([m.0, m.1, m.2])).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        format!("{MARK}{body}")
    }

    /// System clipboard text in place of the snapshot: another app instance cannot paste it.
    pub fn marker(tag: &str) -> String {
        format!("{MARK}#{tag}")
    }

    /// `None`: the text is not from our clipboard or is corrupted. For group definitions from the model itself.
    pub fn from_text(s: &str) -> Option<Clip> {
        Self::parse_body(s.strip_prefix(MARK)?).ok()
    }

    /// Parses system clipboard text: foreign text is not an error, our corrupted text is.
    pub fn parse(s: &str) -> Result<ClipText, Error> {
        let Some(body) = s.strip_prefix(MARK) else {
            return Ok(ClipText::Foreign);
        };
        if body.trim().is_empty() {
            return Ok(ClipText::Marker(String::new()));
        }
        if let Some(tag) = body.strip_prefix('#') {
            return Ok(ClipText::Marker(tag.trim().to_string()));
        }
        if s.len() > CLIP_TEXT_MAX {
            return invalid("clipboard: the payload is too large");
        }
        Self::parse_body(body).map(|c| ClipText::Clip(Box::new(c)))
    }

    fn parse_body(s: &str) -> Result<Clip, Error> {
        let damaged = || Error::Invalid("clipboard: the payload is damaged".to_string());
        let body: J = serde_json::from_str(s).map_err(|_| damaged())?;
        let mut clip = Self::from_body(&body).ok_or_else(damaged)?;
        if clip.schema > crate::schema::VERSION {
            return invalid("clipboard: copied from a newer version of the application");
        }
        clip.ghost.truncate(GHOST_MAX);
        Ok(clip)
    }

    fn from_body(body: &J) -> Option<Clip> {
        let pairs = |k: &str| -> Option<Vec<(String, i64)>> {
            match body.get(k).and_then(J::as_array) {
                Some(v) => v
                    .iter()
                    .map(|r| Some((r.get(0)?.as_str()?.to_string(), r.get(1)?.as_i64()?)))
                    .collect(),
                None => Some(vec![]),
            }
        };
        let tables = |k: &str| -> Option<Vec<(String, ClipRows)>> {
            body.get(k)?
                .as_array()?
                .iter()
                .map(|e| Some((e.get(0)?.as_str()?.to_string(), rows_from(e.get(1)?)?)))
                .collect()
        };
        let f = |v: &J| v.as_f64();
        Some(Clip {
            project: body.get("project")?.as_str()?.to_string(),
            base_level: body.get("base").and_then(J::as_i64),
            levels: body
                .get("levels")?
                .as_array()?
                .iter()
                .map(|l| {
                    Some((
                        l.get(0)?.as_i64()?,
                        l.get(1)?.as_str()?.to_string(),
                        f(l.get(2)?)?,
                    ))
                })
                .collect::<Option<_>>()?,
            tables: tables("tables")?,
            dicts: tables("dicts")?,
            materials: match body.get("materials") {
                Some(_) => tables("materials")?,
                None => vec![],
            },
            roots: body
                .get("roots")?
                .as_array()?
                .iter()
                .map(|r| Some((r.get(0)?.as_str()?.to_string(), r.get(1)?.as_i64()?)))
                .collect::<Option<_>>()?,
            bbox: body
                .get("bbox")
                .and_then(J::as_array)
                .and_then(|b| Some((f(b.first()?)?, f(b.get(1)?)?, f(b.get(2)?)?, f(b.get(3)?)?))),
            ghost: body
                .get("ghost")?
                .as_array()?
                .iter()
                .map(|g| Some([f(g.get(0)?)?, f(g.get(1)?)?, f(g.get(2)?)?, f(g.get(3)?)?]))
                .collect::<Option<_>>()?,
            hosts: body
                .get("hosts")?
                .as_array()?
                .iter()
                .map(|h| {
                    let level = h.get(5).and_then(J::as_i64);
                    Some((
                        h.get(0)?.as_i64()?,
                        f(h.get(1)?)?,
                        f(h.get(2)?)?,
                        f(h.get(3)?)?,
                        f(h.get(4)?)?,
                        level,
                    ))
                })
                .collect::<Option<_>>()?,
            schema: body.get("schema").and_then(J::as_i64).unwrap_or(0),
            unsupported: pairs("unsupported")?,
            except: pairs("except")?,
            instances: body
                .get("instances")?
                .as_array()?
                .iter()
                .map(|i| {
                    let at = i.get("at")?;
                    Some(ClipInst {
                        group: i.get("group")?.as_str()?.to_string(),
                        def: i.get("def")?.as_str()?.to_string(),
                        level: i.get("level")?.as_i64()?,
                        at: (f(at.get(0)?)?, f(at.get(1)?)?),
                        members: i
                            .get("members")?
                            .as_array()?
                            .iter()
                            .map(|m| {
                                Some((
                                    m.get(0)?.as_str()?.to_string(),
                                    m.get(1)?.as_i64()?,
                                    m.get(2)?.as_i64()?,
                                ))
                            })
                            .collect::<Option<_>>()?,
                    })
                })
                .collect::<Option<_>>()?,
        })
    }
}

fn rows_json(r: &ClipRows) -> J {
    json!({
        "cols": r.cols,
        "rows": r.rows.iter().map(|x| x.iter().map(to_json).collect::<Vec<J>>()).collect::<Vec<_>>(),
    })
}

/// A row of a different length than `cols` rejects the whole snapshot: paste indexes rows by column.
fn rows_from(j: &J) -> Option<ClipRows> {
    let cols: Vec<String> = j
        .get("cols")?
        .as_array()?
        .iter()
        .map(|c| c.as_str().map(str::to_string))
        .collect::<Option<_>>()?;
    let rows: Vec<Vec<Value>> = j
        .get("rows")?
        .as_array()?
        .iter()
        .map(|r| {
            r.as_array()
                .filter(|v| v.len() == cols.len())
                .map(|v| v.iter().map(from_json).collect::<Vec<Value>>())
        })
        .collect::<Option<_>>()?;
    Some(ClipRows { cols, rows })
}

fn invalid<T>(m: &str) -> Result<T, Error> {
    Err(Error::Invalid(m.to_string()))
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        _ => None,
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Real(f) => Some(*f),
        Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn text(v: &Value) -> Option<&str> {
    match v {
        Value::Text(s) => Some(s.as_str()),
        _ => None,
    }
}

fn has_table(db: &Connection, t: &str) -> Result<bool, Error> {
    let n: i64 = db
        .prepare_cached("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1")?
        .query_row([t], |r| r.get(0))?;
    Ok(n > 0)
}

/// (column name, NOT NULL) in schema order.
fn columns(db: &Connection, t: &str) -> Result<Vec<(String, bool)>, Error> {
    let mut st = db.prepare(&format!("PRAGMA table_info({t})"))?;
    let v = st
        .query_map([], |r| {
            Ok((r.get::<_, String>(1)?, r.get::<_, i64>(3)? != 0))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

fn select_rows(
    db: &Connection,
    sql: &str,
    n: usize,
    args: &[Value],
) -> Result<Vec<Vec<Value>>, Error> {
    let mut st = db.prepare_cached(sql)?;
    let rows = st.query_map(params_from_iter(args.iter()), |r| {
        (0..n)
            .map(|i| r.get::<_, Value>(i))
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn ids_where(db: &Connection, t: &str, idc: &str, col: &str, v: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare_cached(&format!(
        "SELECT {idc} FROM {t} WHERE {col}=?1 ORDER BY {idc}"
    ))?;
    let rows = st.query_map([v], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Model token: the file path; for an in-memory model, random per connection (temporary table).
fn project_token(db: &Connection) -> Result<String, Error> {
    let path: String = db.query_row(
        "SELECT file FROM pragma_database_list WHERE name='main'",
        [],
        |r| r.get::<_, Option<String>>(0).map(Option::unwrap_or_default),
    )?;
    if !path.is_empty() {
        return Ok(path);
    }
    static NEXT: AtomicU64 = AtomicU64::new(1);
    db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS clip_token(t TEXT NOT NULL)")?;
    let have: Option<String> = db
        .query_row("SELECT t FROM temp.clip_token LIMIT 1", [], |r| r.get(0))
        .ok();
    if let Some(t) = have {
        return Ok(t);
    }
    let t = format!(
        "mem-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    db.execute("INSERT INTO temp.clip_token(t) VALUES (?1)", [&t])?;
    Ok(t)
}

fn spec(t: &str) -> Option<&'static Spec> {
    specs().find(|s| s.t == t)
}

/// Kinds `Model::delete_imported` takes in one call.
const BATCHED_BODIES: [&str; 5] = ["mesh", "col", "beam", "mep_run", "ceiling"];

/// The row is there: false also for a table the registry doesn't know, so it stays untouched.
fn row_exists(db: &Connection, t: &str, id: i64) -> Result<bool, Error> {
    let Some(c) = spec(t).and_then(|s| s.id) else {
        return Ok(false);
    };
    let n: i64 = db.query_row(&format!("SELECT count(*) FROM {t} WHERE {c}=?1"), [id], |r| {
        r.get(0)
    })?;
    Ok(n > 0)
}

/// Walls from a constraint's text; `None` if it is not a list of numbers.
fn parse_ids(s: &str) -> Option<Vec<i64>> {
    s.split_whitespace().map(|x| x.parse().ok()).collect()
}

/// Tables and ids mentioned in a dimension or tag reference.
fn ref_targets(tok: &str) -> Vec<(&'static str, i64)> {
    if let Some(rest) = tok.strip_prefix("H ") {
        let mut it = rest.split_whitespace();
        let t = match it.next() {
            Some("w") => "wall",
            Some("o") => "opening",
            Some("r") => "room",
            Some("c") => "comp",
            _ => return vec![],
        };
        return it
            .next()
            .and_then(|v| v.parse().ok())
            .map_or(vec![], |id| vec![(t, id)]);
    }
    match parse_ref(tok) {
        Some(DimRef::Point(_)) | None => vec![],
        Some(DimRef::WallEnd { wall, .. })
        | Some(DimRef::AxisEnd { wall, .. })
        | Some(DimRef::AxisLine { wall })
        | Some(DimRef::ArcFace { wall, .. })
        | Some(DimRef::WallLevel { wall, .. }) => vec![("wall", wall)],
        Some(DimRef::OpeningEdge { opening, .. }) => vec![("opening", opening)],
        Some(DimRef::GridAt { grid, wall }) => vec![("grid", grid), ("wall", wall)],
    }
}

pub(crate) type IdSet = HashMap<&'static str, BTreeSet<i64>>;

pub(crate) fn contains(set: &IdSet, t: &str, id: i64) -> bool {
    set.get(t).is_some_and(|s| s.contains(&id))
}

/// Snapshot option: a group definition excludes dimensions and constraints.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct SnapOpts {
    pub relations: bool,
}

/// The selection and everything depending on it: openings, components, edges, cuts.
/// `except` stays out of the set even if its host pulls it in.
fn closure(
    db: &Connection,
    items: &[(&str, i64)],
    o: SnapOpts,
    except: &[(&str, i64)],
) -> Result<IdSet, Error> {
    let mut set = IdSet::new();
    for (k, id) in items {
        if let Some(s) = spec(k) {
            set.entry(s.t).or_default().insert(*id);
        }
    }
    let out = |t: &str, id: i64| except.iter().any(|x| x.0 == t && x.1 == id);
    for s in specs() {
        let Some(idc) = s.id else { continue };
        if !has_table(db, s.t)? {
            continue;
        }
        for (col, parent) in s.owns {
            let parents: BTreeSet<i64> = set.get(parent).cloned().unwrap_or_default();
            if parents.len() >= SCAN_AT {
                let mut st =
                    db.prepare_cached(&format!("SELECT {idc},{col} FROM {} ORDER BY {idc}", s.t))?;
                let rows = st
                    .query_map([], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                for (id, p) in rows {
                    if p.is_some_and(|p| parents.contains(&p)) && !out(s.t, id) {
                        set.entry(s.t).or_default().insert(id);
                    }
                }
                continue;
            }
            for p in parents {
                for id in ids_where(db, s.t, idc, col, p)? {
                    if !out(s.t, id) {
                        set.entry(s.t).or_default().insert(id);
                    }
                }
            }
        }
        if let Some((kc, rc)) = s.poly {
            let mut st = db.prepare(&format!("SELECT {idc},{kc},{rc} FROM {}", s.t))?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for (id, k, r) in rows {
                if kind_table(s, &k).is_some_and(|t| contains(&set, t, r)) {
                    set.entry(s.t).or_default().insert(id);
                }
            }
        }
        if s.t == "sweep" {
            let mut st = db.prepare("SELECT id,host_kind,hosts FROM sweep")?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for (id, k, h) in rows {
                let hosts: Vec<i64> = serde_json::from_str(&h).unwrap_or_default();
                if !hosts.is_empty() && hosts.iter().all(|x| contains(&set, &k, *x)) {
                    set.entry("sweep").or_default().insert(id);
                }
            }
        }
    }
    if o.relations {
        let mut st = db.prepare("SELECT id,refs,auto FROM dim")?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, refs, auto) in rows {
            let t: Vec<(&str, i64)> = refs.split(';').flat_map(ref_targets).collect();
            if auto == 0 && !t.is_empty() && t.iter().all(|(k, i)| contains(&set, k, *i)) {
                set.entry("dim").or_default().insert(id);
            }
        }
        let mut st = db.prepare("SELECT id,kind,walls FROM constraint_rule")?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, k, w) in rows {
            let all = parse_ids(&w)
                .is_some_and(|v| !v.is_empty() && v.iter().all(|x| contains(&set, "wall", *x)));
            if CONSTRAINT_KINDS.contains(&k.as_str()) && all {
                set.entry("constraint_rule").or_default().insert(id);
            }
        }
    }
    Ok(set)
}

fn read_table(db: &Connection, s: &Spec, set: &IdSet, o: SnapOpts) -> Result<ClipRows, Error> {
    let cols: Vec<String> = columns(db, s.t)?.into_iter().map(|c| c.0).collect();
    let n = cols.len();
    let list = cols.join(",");
    let mut rows = vec![];
    if s.t == "geom_join" {
        if !o.relations {
            return Ok(ClipRows { cols, rows });
        }
        let all = select_rows(
            db,
            &format!("SELECT {list} FROM geom_join ORDER BY rowid"),
            n,
            &[],
        )?;
        let at = |c: &str| cols.iter().position(|x| x == c);
        if let (Some(wk), Some(w), Some(lk), Some(l)) = (
            at("winner_kind"),
            at("winner"),
            at("loser_kind"),
            at("loser"),
        ) {
            for r in all {
                let inside = |k: usize, i: usize| {
                    text(&r[k])
                        .zip(int(&r[i]))
                        .is_some_and(|(t, v)| contains(set, t, v))
                };
                if inside(wk, w) && inside(lk, l) {
                    rows.push(r);
                }
            }
        }
    } else if let Some(idc) = s.id {
        let sql = format!("SELECT {list} FROM {} WHERE {idc}=?1", s.t);
        for id in set.get(s.t).into_iter().flatten() {
            rows.extend(select_rows(db, &sql, n, &[Value::Integer(*id)])?);
        }
    } else if let Some((kc, rc)) = s.poly {
        let massc = cols.iter().position(|c| c == "mass");
        // a shape attachment is needed only together with the shape itself
        let wanted = |r: &[Value]| {
            s.t != "mass_host"
                || massc
                    .and_then(|m| int(&r[m]))
                    .is_some_and(|m| contains(set, "mass", m))
        };
        let total: usize = set.values().map(BTreeSet::len).sum();
        if total >= SCAN_AT {
            let at = |c: &str| cols.iter().position(|x| x == c);
            if let (Some(ki), Some(ri)) = (at(kc), at(rc)) {
                let all = select_rows(
                    db,
                    &format!("SELECT {list} FROM {} ORDER BY rowid", s.t),
                    n,
                    &[],
                )?;
                rows.extend(all.into_iter().filter(|r| {
                    let inside = text(&r[ki]).zip(int(&r[ri])).is_some_and(|(k, v)| {
                        kind_table(s, k).is_some_and(|t| contains(set, t, v))
                    });
                    inside && wanted(r)
                }));
            }
        } else {
            let sql = format!(
                "SELECT {list} FROM {} WHERE {kc}=?1 AND {rc}=?2 ORDER BY rowid",
                s.t
            );
            // hash order would reorder the rows between two snapshots of the same model
            let mut tabs: Vec<_> = set.iter().collect();
            tabs.sort_by_key(|(t, _)| **t);
            for (t, ids) in tabs {
                let names: Vec<&str> = if s.kinds.is_empty() {
                    vec![*t]
                } else {
                    s.kinds.iter().filter(|x| x.1 == *t).map(|x| x.0).collect()
                };
                for kind in names {
                    for id in ids {
                        let got = select_rows(
                            db,
                            &sql,
                            n,
                            &[Value::Text(kind.to_string()), Value::Integer(*id)],
                        )?;
                        rows.extend(got.into_iter().filter(|r| wanted(r)));
                    }
                }
            }
        }
    } else if let Some((col, parent)) = s.owns.first() {
        let parents = set.get(parent);
        if parents.is_some_and(|p| p.len() >= SCAN_AT) {
            if let (Some(p), Some(ci)) = (parents, cols.iter().position(|x| x == col)) {
                let all = select_rows(
                    db,
                    &format!("SELECT {list} FROM {} ORDER BY rowid", s.t),
                    n,
                    &[],
                )?;
                rows.extend(
                    all.into_iter()
                        .filter(|r| int(&r[ci]).is_some_and(|v| p.contains(&v))),
                );
                // stable: vertices of one outline remain neighbours in their original order
                rows.sort_by_key(|r| int(&r[ci]));
            }
        } else {
            let sql = format!("SELECT {list} FROM {} WHERE {col}=?1 ORDER BY rowid", s.t);
            for id in parents.into_iter().flatten() {
                rows.extend(select_rows(db, &sql, n, &[Value::Integer(*id)])?);
            }
        }
    }
    Ok(ClipRows { cols, rows })
}

/// Snapshot: writes nothing to the model file.
pub(crate) fn snapshot(db: &Connection, items: &[(&str, i64)], o: SnapOpts) -> Result<Clip, Error> {
    snapshot_except(db, items, o, &[])
}

/// Snapshot without `except`: an opening removed from the group does not come back with its wall.
pub(crate) fn snapshot_except(
    db: &Connection,
    items: &[(&str, i64)],
    o: SnapOpts,
    except: &[(&str, i64)],
) -> Result<Clip, Error> {
    if items.is_empty() {
        return invalid("copy: nothing selected");
    }
    let (known, unknown): (Vec<(&str, i64)>, Vec<(&str, i64)>) = items
        .iter()
        .copied()
        .partition(|(k, _)| spec(k).is_some_and(|s| s.id.is_some()));
    if known.is_empty() {
        return invalid("copy: these elements cannot be copied");
    }
    // a root whose row is gone (deleted, undone) is no element: it never becomes a root
    let mut live = Vec::with_capacity(known.len());
    for (k, id) in known {
        if row_exists(db, k, id)? {
            live.push((k, id));
        }
    }
    let known = live;
    if known.is_empty() {
        return invalid("copy: the selected elements no longer exist");
    }
    // a spline wall is copied whole: its row and every piece of its arc chain
    let mut all = known.clone();
    for &(k, id) in &known {
        for it in crate::wall_spline::group_items(db, k, id)? {
            if !all.contains(&it) {
                all.push(it);
            }
        }
    }
    let items = all.as_slice();
    let set = closure(db, items, o, except)?;
    let mut clip = Clip {
        project: project_token(db)?,
        roots: items.iter().map(|(k, i)| ((*k).to_string(), *i)).collect(),
        schema: crate::schema::VERSION,
        unsupported: unknown
            .iter()
            .map(|(k, i)| ((*k).to_string(), *i))
            .collect(),
        ..Default::default()
    };
    let mut lv: BTreeSet<i64> = BTreeSet::new();
    let mut used: HashMap<&'static str, BTreeSet<i64>> = HashMap::new();
    let mut swept: BTreeSet<String> = BTreeSet::new();
    for s in specs() {
        if !has_table(db, s.t)? {
            continue;
        }
        let r = read_table(db, s, &set, o)?;
        if r.rows.is_empty() {
            continue;
        }
        let at = |c: &str| r.cols.iter().position(|x| x == c);
        for row in &r.rows {
            for c in s.levels.iter().chain(s.tops) {
                if let Some(v) = at(c).and_then(|i| int(&row[i])) {
                    lv.insert(v);
                }
            }
            for (c, d) in s.dicts {
                if let Some(v) = at(c).and_then(|i| int(&row[i])) {
                    used.entry(*d).or_default().insert(v);
                }
            }
            if s.t == "sweep" {
                if let Some(p) = at("profile").and_then(|i| text(&row[i])) {
                    swept.insert(p.to_string());
                }
            }
            // a plan view is a level
            if s.t == "anno" && at("view_kind").and_then(|i| text(&row[i])) == Some("plan") {
                if let Some(v) = at("view_id").and_then(|i| int(&row[i])) {
                    lv.insert(v);
                }
            }
        }
        clip.tables.push((s.t.to_string(), r));
    }
    read_dicts(db, &mut clip, &mut used, &swept)?;
    // level of the host wall: a standalone opening uses it to find a wall on the target floor
    read_hosts(db, &mut clip, &set)?;
    lv.extend(clip.hosts.iter().filter_map(|h| h.5));
    let mut st = db.prepare("SELECT name,elevation FROM level WHERE id=?1")?;
    for id in lv {
        let got: Option<(String, f64)> = st
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        if let Some((n, e)) = got {
            clip.levels.push((id, n, e));
        }
    }
    drop(st);
    clip.base_level = base_of(&clip, items);
    draw_ghost(&mut clip);
    mat::snapshot(db, &mut clip)?;
    if o.relations {
        crate::groups::snapshot_instances(db, &mut clip, &set)?;
    }
    Ok(clip)
}

/// Lowest level among the selection: the paste anchor for other floors.
fn base_of(clip: &Clip, items: &[(&str, i64)]) -> Option<i64> {
    let elev: HashMap<i64, f64> = clip.levels.iter().map(|l| (l.0, l.2)).collect();
    let mut chosen: HashMap<&str, HashSet<i64>> = HashMap::new();
    for (k, id) in items {
        chosen.entry(*k).or_default().insert(*id);
    }
    let mut best: Option<(f64, i64)> = None;
    for (k, ids) in &chosen {
        let Some(s) = spec(k) else { continue };
        let (Some(r), Some(lc)) = (clip.rows(s.t), s.levels.first()) else {
            continue;
        };
        let (Some(ic), Some(li)) = (
            r.cols.iter().position(|c| Some(c.as_str()) == s.id),
            r.cols.iter().position(|c| c == lc),
        ) else {
            continue;
        };
        for row in r
            .rows
            .iter()
            .filter(|x| int(&x[ic]).is_some_and(|v| ids.contains(&v)))
        {
            if let Some((l, e)) = int(&row[li]).and_then(|l| elev.get(&l).map(|e| (l, *e))) {
                if best.map_or(true, |b| e < b.0 || (e == b.0 && l < b.1)) {
                    best = Some((e, l));
                }
            }
        }
    }
    best.map(|b| b.1).or_else(|| {
        clip.levels
            .iter()
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|l| l.0)
    })
}

fn read_dicts(
    db: &Connection,
    clip: &mut Clip,
    used: &mut HashMap<&'static str, BTreeSet<i64>>,
    swept: &BTreeSet<String>,
) -> Result<(), Error> {
    // children need other lookups: the assembly composition and the type family
    if let Some(stacks) = used.get("stack_type").cloned() {
        if has_table(db, "stack_part")? {
            for s in stacks {
                for w in ids_where(db, "stack_part", "wtype", "stack", s)? {
                    used.entry("wall_type").or_default().insert(w);
                }
            }
        }
    }
    if let Some(types) = used.get("comp_type").cloned() {
        for c in types {
            for f in ids_where(db, "comp_type", "family", "id", c)? {
                used.entry("comp_family").or_default().insert(f);
            }
        }
    }
    for d in dict_specs() {
        if !has_table(db, d.t)? {
            continue;
        }
        let cols: Vec<String> = columns(db, d.t)?.into_iter().map(|c| c.0).collect();
        let sql = format!("SELECT {} FROM {} WHERE id=?1", cols.join(","), d.t);
        let mut rows = vec![];
        if let Some(ids) = used.get(d.t) {
            for id in ids {
                rows.extend(select_rows(db, &sql, cols.len(), &[Value::Integer(*id)])?);
            }
        }
        if d.t == "sweep_profile" {
            let sql = format!("SELECT {} FROM sweep_profile WHERE name=?1", cols.join(","));
            for p in swept {
                rows.extend(select_rows(
                    db,
                    &sql,
                    cols.len(),
                    &[Value::Text(p.clone())],
                )?);
            }
        }
        if rows.is_empty() {
            continue;
        }
        let ids: Vec<i64> = {
            let i = cols.iter().position(|c| c == "id").unwrap_or(0);
            rows.iter().filter_map(|r| int(&r[i])).collect()
        };
        clip.dicts.push((d.t.to_string(), ClipRows { cols, rows }));
        for (child, col, _) in d.children {
            if !has_table(db, child)? {
                continue;
            }
            let cols: Vec<String> = columns(db, child)?.into_iter().map(|c| c.0).collect();
            let sql = format!(
                "SELECT {} FROM {child} WHERE {col}=?1 ORDER BY rowid",
                cols.join(",")
            );
            let mut rows = vec![];
            for id in &ids {
                rows.extend(select_rows(db, &sql, cols.len(), &[Value::Integer(*id)])?);
            }
            if !rows.is_empty() {
                clip.dicts
                    .push(((*child).to_string(), ClipRows { cols, rows }));
            }
        }
    }
    Ok(())
}

/// Axes of host walls left outside the snapshot: a standalone opening uses them to find a new wall.
fn read_hosts(db: &Connection, clip: &mut Clip, set: &IdSet) -> Result<(), Error> {
    let mut hosts = BTreeSet::new();
    for (t, c) in [("opening", "wall"), ("comp", "host_wall")] {
        if let Some(r) = clip.rows(t) {
            if let Some(i) = r.cols.iter().position(|x| x == c) {
                hosts.extend(
                    r.rows
                        .iter()
                        .filter_map(|x| int(&x[i]))
                        .filter(|w| !contains(set, "wall", *w)),
                );
            }
        }
    }
    let mut st = db.prepare("SELECT ax,ay,bx,by,level FROM wall WHERE id=?1")?;
    for w in hosts {
        let (a, b, c, d, l): (f64, f64, f64, f64, Option<i64>) = st.query_row([w], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        clip.hosts.push((w, a, b, c, d, l));
    }
    Ok(())
}

fn host_point(h: &ClipHost, along: f64) -> Option<(f64, f64)> {
    let (ux, uy) = (h.3 - h.1, h.4 - h.2);
    let len = ux.hypot(uy);
    (len > 1e-9).then(|| (h.1 + ux / len * along, h.2 + uy / len * along))
}

/// Polylines of a text column: they give the bounding box and ghost to elements without x/y columns.
fn tx_points(tx: Tx, s: &str) -> Vec<Vec<(f64, f64)>> {
    let spaced = |p: &str| -> Option<(f64, f64)> {
        let mut it = p.split_whitespace().map(|x| x.parse::<f64>().ok());
        Some((it.next()??, it.next()??))
    };
    match tx {
        Tx::Points3 | Tx::Lines3 => s
            .split('|')
            .map(|l| l.split(';').filter_map(|p| spaced(p)).collect())
            .collect(),
        Tx::Ring => vec![s.split(';').filter_map(|p| spaced(p)).collect()],
        Tx::CommaPairs => vec![s
            .split(';')
            .filter_map(|p| {
                let (x, y) = p.split_once(',')?;
                Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?))
            })
            .collect()],
        Tx::PairsJson => vec![serde_json::from_str(s).unwrap_or_default()],
        Tx::AnnoJson => {
            let mut out: Vec<(f64, f64)> = vec![];
            if let Ok(mut v) = serde_json::from_str::<J>(s) {
                walk_anno(&mut v, &mut |p: &mut J| {
                    if let Some(q) = p
                        .get(0)
                        .and_then(J::as_f64)
                        .zip(p.get(1).and_then(J::as_f64))
                    {
                        out.push(q);
                    }
                });
            }
            vec![out]
        }
        _ => vec![],
    }
}

/// Every data point of an annotation; the text field `head` and an empty `elbow` do not count as points.
fn walk_anno(v: &mut J, f: &mut dyn FnMut(&mut J)) {
    match v {
        J::Array(a) => {
            for x in a {
                walk_anno(x, f);
            }
        }
        J::Object(o) => {
            for (k, x) in o.iter_mut() {
                match k.as_str() {
                    "pos" | "at" | "to" | "elbow" | "head" if x.is_array() => f(x),
                    "pts" | "ring" => {
                        if let J::Array(a) = x {
                            for p in a {
                                f(p);
                            }
                        }
                    }
                    "rings" => {
                        if let J::Array(a) = x {
                            for r in a {
                                if let J::Array(ps) = r {
                                    for p in ps {
                                        f(p);
                                    }
                                }
                            }
                        }
                    }
                    "leaders" => walk_anno(x, f),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn translate_anno(s: &str, dx: f64, dy: f64) -> Option<String> {
    let mut v: J = serde_json::from_str(s).ok()?;
    walk_anno(&mut v, &mut |p: &mut J| {
        if let (Some(x), Some(y)) = (p.get(0).and_then(J::as_f64), p.get(1).and_then(J::as_f64)) {
            *p = json!([x + dx, y + dy]);
        }
    });
    Some(v.to_string())
}

fn translate_comma(s: &str, dx: f64, dy: f64) -> Option<String> {
    let mut out = vec![];
    for p in s.split(';').filter(|p| !p.trim().is_empty()) {
        let (x, y) = p.split_once(',')?;
        let (x, y) = (x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?);
        out.push(format!("{},{}", x + dx, y + dy));
    }
    Some(out.join(";"))
}

fn draw_ghost(clip: &mut Clip) {
    let mut g: Vec<[f64; 4]> = vec![];
    let mut bb: Option<(f64, f64, f64, f64)> = None;
    let mut add = |x: f64, y: f64| {
        bb = Some(bb.map_or((x, y, x, y), |b| {
            (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y))
        }));
    };
    let host_by_id: HashMap<i64, &ClipHost> = clip.hosts.iter().map(|h| (h.0, h)).collect();
    for (t, r) in &clip.tables {
        let Some(s) = spec(t) else { continue };
        let at = |c: &str| r.cols.iter().position(|x| x == c);
        let pair = |row: &[Value], p: &(&str, &str)| -> Option<(f64, f64)> {
            Some((num(&row[at(p.0)?])?, num(&row[at(p.1)?])?))
        };
        if s.chain {
            let owner = s.owns.first().and_then(|o| at(o.0));
            let mut run: Vec<(f64, f64)> = vec![];
            let mut cur: Option<i64> = None;
            let closed = !matches!(s.t, "railing_vertex");
            let flush = |run: &mut Vec<(f64, f64)>, g: &mut Vec<[f64; 4]>| {
                for w in run.windows(2) {
                    g.push([w[0].0, w[0].1, w[1].0, w[1].1]);
                }
                if closed && run.len() > 2 {
                    let (a, b) = (run[run.len() - 1], run[0]);
                    g.push([a.0, a.1, b.0, b.1]);
                }
                run.clear();
            };
            for row in &r.rows {
                let o = owner.and_then(|i| int(&row[i]));
                if o != cur {
                    flush(&mut run, &mut g);
                    cur = o;
                }
                if let Some(p) = s.xy.first().and_then(|xy| pair(row, xy)) {
                    add(p.0, p.1);
                    run.push(p);
                }
            }
            flush(&mut run, &mut g);
            continue;
        }
        if matches!(s.t, "topo_point" | "datum_extent") {
            continue;
        }
        // openings and components: position along the host wall or absolute in plan
        if matches!(s.t, "opening" | "comp") {
            let (wc, ac) = if s.t == "opening" {
                ("wall", "along")
            } else {
                ("host_wall", "x")
            };
            for row in &r.rows {
                let along = at(ac).and_then(|i| num(&row[i])).unwrap_or(0.0);
                let host = at(wc).and_then(|i| int(&row[i]));
                let p = match host {
                    Some(w) => host_by_id.get(&w).and_then(|h| host_point(h, along)),
                    None => pair(row, &("x", "y")),
                };
                if let Some(p) = p {
                    add(p.0, p.1);
                    const D: f64 = 0.15;
                    g.push([p.0 - D, p.1, p.0 + D, p.1]);
                    g.push([p.0, p.1 - D, p.0, p.1 + D]);
                }
            }
            continue;
        }
        if s.t == "mesh" {
            if let Some(i) = at("positions") {
                for row in &r.rows {
                    if let Value::Blob(b) = &row[i] {
                        let mut lo = (f64::MAX, f64::MAX);
                        let mut hi = (f64::MIN, f64::MIN);
                        for c in b.chunks_exact(24) {
                            let v = |k: usize| {
                                f64::from_le_bytes(c[k..k + 8].try_into().unwrap_or([0; 8]))
                            };
                            lo = (lo.0.min(v(0)), lo.1.min(v(8)));
                            hi = (hi.0.max(v(0)), hi.1.max(v(8)));
                        }
                        if lo.0 <= hi.0 {
                            add(lo.0, lo.1);
                            add(hi.0, hi.1);
                            g.push([lo.0, lo.1, hi.0, lo.1]);
                            g.push([hi.0, lo.1, hi.0, hi.1]);
                            g.push([hi.0, hi.1, lo.0, hi.1]);
                            g.push([lo.0, hi.1, lo.0, lo.1]);
                        }
                    }
                }
            }
            continue;
        }
        for (c, tx) in s.tx {
            let Some(i) = at(c) else { continue };
            for row in &r.rows {
                let Some(body) = text(&row[i]) else { continue };
                for line in tx_points(*tx, body) {
                    for p in &line {
                        add(p.0, p.1);
                    }
                    for w in line.windows(2) {
                        g.push([w[0].0, w[0].1, w[1].0, w[1].1]);
                    }
                }
            }
        }
        for row in &r.rows {
            let pts: Vec<(f64, f64)> = s.xy.iter().filter_map(|p| pair(row, p)).collect();
            for p in &pts {
                add(p.0, p.1);
            }
            match pts.as_slice() {
                [a, b, ..] if s.t != "col" => g.push([a.0, a.1, b.0, b.1]),
                [a, ..] => {
                    const D: f64 = 0.15;
                    g.push([a.0 - D, a.1, a.0 + D, a.1]);
                    g.push([a.0, a.1 - D, a.0, a.1 + D]);
                }
                [] => {}
            }
        }
    }
    g.truncate(GHOST_MAX);
    drop(host_by_id);
    clip.ghost = g;
    clip.bbox = bb;
}

/// Snapshot elements with their ids: members of a group instance (dimensions and constraints excluded).
pub(crate) fn members(clip: &Clip) -> Vec<(String, i64)> {
    let mut out = vec![];
    for (t, r) in &clip.tables {
        let Some(s) = spec(t) else { continue };
        let Some(idc) = s.id else { continue };
        if matches!(s.t, "dim" | "constraint_rule" | "datum_extent") {
            continue;
        }
        let Some(i) = r.cols.iter().position(|c| c == idc) else {
            continue;
        };
        out.extend(
            r.rows
                .iter()
                .filter_map(|x| int(&x[i]))
                .map(|id| (t.clone(), id)),
        );
    }
    out
}

/// Plan point of a snapshot element: the first coordinate pair of the row or the first outline vertex.
pub(crate) fn points(clip: &Clip) -> HashMap<(String, i64), (f64, f64)> {
    let mut out = HashMap::new();
    for (t, r) in &clip.tables {
        let Some(s) = spec(t) else { continue };
        let at = |c: &str| r.cols.iter().position(|x| x == c);
        let Some(first) =
            s.xy.first()
                .copied()
                .or((s.t == "comp").then_some(("x", "y")))
        else {
            // without x/y columns the position is in text: a route, a ramp path, a terrain outline
            let Some(i) =
                s.id.and_then(|c| at(c))
                    .filter(|_| !matches!(s.t, "dim" | "constraint_rule"))
            else {
                continue;
            };
            for (c, tx) in s.tx {
                let Some(ci) = at(c) else { continue };
                for row in &r.rows {
                    let got =
                        text(&row[ci]).and_then(|b| tx_points(*tx, b).into_iter().flatten().next());
                    if let (Some(p), Some(id)) = (got, int(&row[i])) {
                        out.entry((t.clone(), id)).or_insert(p);
                    }
                }
            }
            continue;
        };
        let (Some(xi), Some(yi)) = (at(first.0), at(first.1)) else {
            continue;
        };
        let free_comp = at("host_wall");
        for row in &r.rows {
            if s.t == "comp" && free_comp.and_then(|i| int(&row[i])).is_some() {
                continue;
            }
            let (Some(x), Some(y)) = (num(&row[xi]), num(&row[yi])) else {
                continue;
            };
            if let Some(i) = s.id.and_then(|c| at(c)) {
                if let Some(id) = int(&row[i]) {
                    out.insert((t.clone(), id), (x, y));
                }
            } else if let (true, Some((c, parent))) = (s.chain, s.owns.first()) {
                if let Some(o) = at(c).and_then(|i| int(&row[i])) {
                    out.entry(((*parent).to_string(), o)).or_insert((x, y));
                }
            }
        }
    }
    out
}

/// Tables paste may write to: for marking the rowids of one journal entry.
pub(crate) fn write_tables(db: &Connection) -> Result<Vec<&'static str>, Error> {
    let mut out = vec!["level", "el_group", "group_inst", "group_member"];
    for t in specs()
        .map(|s| s.t)
        .chain(dict_specs().map(|d| d.t))
        .chain(dict_specs().flat_map(|d| d.children.iter().map(|c| c.0)))
    {
        if !out.contains(&t) && has_table(db, t)? {
            out.push(t);
        }
    }
    // a paste into another project adds material rows; undo drops them with the elements
    for t in ["material", "material_image", "mesh_material"] {
        if has_table(db, t)? {
            out.push(t);
        }
    }
    Ok(out)
}

fn translate_pairs(s: &str, dx: f64, dy: f64) -> Option<String> {
    let v: Vec<(f64, f64)> = serde_json::from_str(s).ok()?;
    let v: Vec<(f64, f64)> = v.into_iter().map(|p| (p.0 + dx, p.1 + dy)).collect();
    serde_json::to_string(&v).ok()
}

fn translate_ring(s: &str, dx: f64, dy: f64) -> Option<String> {
    let mut out = vec![];
    for p in s.split(';').filter(|p| !p.trim().is_empty()) {
        let v: Vec<f64> = p
            .split_whitespace()
            .map(|x| x.parse().ok())
            .collect::<Option<_>>()?;
        out.push(format!("{} {}", v.first()? + dx, v.get(1)? + dy));
    }
    Some(out.join(";"))
}

fn translate_lines3(s: &str, dx: f64, dy: f64, dz: f64) -> Option<String> {
    let mut lines = vec![];
    for l in s.split('|').filter(|l| !l.trim().is_empty()) {
        let mut out = vec![];
        for p in l.split(';').filter(|p| !p.trim().is_empty()) {
            let v: Vec<f64> = p
                .split_whitespace()
                .map(|x| x.parse().ok())
                .collect::<Option<_>>()?;
            out.push(format!(
                "{:?} {:?} {:?}",
                v.first()? + dx,
                v.get(1)? + dy,
                v.get(2)? + dz
            ));
        }
        lines.push(out.join(";"));
    }
    Some(lines.join("|"))
}

/// Sketch curves: kind letter and plan coordinates (`L`, `S`: all pairs; `A`, `E`: the centre).
fn translate_sketch(s: &str, dx: f64, dy: f64) -> Option<String> {
    let mut out = vec![];
    for line in s.lines().filter(|l| !l.trim().is_empty()) {
        let mut it = line.split_whitespace();
        let k = it.next()?;
        let mut v: Vec<f64> = it.map(|x| x.parse().ok()).collect::<Option<_>>()?;
        let pairs = if matches!(k, "A" | "E") {
            2.min(v.len())
        } else {
            v.len()
        };
        for (i, x) in v.iter_mut().take(pairs).enumerate() {
            *x += if i % 2 == 0 { dx } else { dy };
        }
        let nums: Vec<String> = v.iter().map(f64::to_string).collect();
        out.push(format!("{k} {}", nums.join(" ")));
    }
    Some(out.join("\n"))
}

fn translate_shape(s: &str, dx: f64, dy: f64) -> Option<String> {
    let mut out = vec![];
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let k = it.next()?;
        let v: Vec<f64> = it.map(|x| x.parse().ok()).collect::<Option<_>>()?;
        out.push(match (k, v.as_slice()) {
            ("P", [x, y, z]) => format!("P {} {} {z}", x + dx, y + dy),
            ("S", [ax, ay, bx, by, za, zb]) => {
                format!(
                    "S {} {} {} {} {za} {zb}",
                    ax + dx,
                    ay + dy,
                    bx + dx,
                    by + dy
                )
            }
            _ => line.to_string(),
        });
    }
    Some(out.join("\n"))
}

fn translate_form(s: &str, dx: f64, dy: f64) -> Result<String, Error> {
    let f = Form::from_json(&serde_json::from_str::<J>(s)?)?;
    let f = match f {
        Form::Move { base, d } => Form::Move {
            base,
            d: (d.0 + dx, d.1 + dy, d.2),
        },
        f => Form::Move {
            base: Box::new(f),
            d: (dx, dy, 0.0),
        },
    };
    Ok(f.to_json().to_string())
}

/// Paste run: levels, lookups and new ids for one set of offsets.
pub(crate) struct Paster<'a> {
    db: &'a Connection,
    clip: &'a Clip,
    dx: f64,
    dy: f64,
    same_project: bool,
    /// Paste without a move: a reference to a live element outside the snapshot stays valid.
    in_place: bool,
    /// The same levels of the same model: optional references to live elements are kept.
    keep_refs: bool,
    levels: HashMap<i64, i64>,
    src_elev: HashMap<i64, f64>,
    dicts: HashMap<&'static str, HashMap<i64, i64>>,
    /// Old id → new; pre-filled rows are updated rather than inserted.
    pub map: HashMap<String, HashMap<i64, i64>>,
    existing: HashSet<(String, i64)>,
    /// Snapshot rows this group instance deliberately lacks: not recreated.
    absent: HashSet<(String, i64)>,
    hosts: HashMap<i64, &'a ClipHost>,
    /// Target lookups found by name: their children are left alone.
    kept: HashSet<(&'static str, i64)>,
    pub report: PasteReport,
}

impl<'a> Paster<'a> {
    pub(crate) fn new(db: &'a Connection, clip: &'a Clip, o: &PasteOpts) -> Result<Self, Error> {
        let same_project = clip.project == project_token(db)?;
        let mut p = Paster {
            db,
            clip,
            dx: o.dx,
            dy: o.dy,
            same_project,
            in_place: false,
            keep_refs: false,
            levels: HashMap::new(),
            src_elev: clip.levels.iter().map(|l| (l.0, l.2)).collect(),
            dicts: HashMap::new(),
            map: HashMap::new(),
            existing: HashSet::new(),
            absent: HashSet::new(),
            hosts: clip.hosts.iter().map(|h| (h.0, h)).collect(),
            kept: HashSet::new(),
            report: PasteReport::default(),
        };
        p.map_levels(o.level)?;
        p.keep_refs = same_project && p.levels.iter().all(|(a, b)| a == b);
        p.in_place = p.keep_refs && o.dx == 0.0 && o.dy == 0.0;
        p.map_dicts()?;
        Ok(p)
    }

    /// Where the snapshot level landed.
    pub(crate) fn level_of(&self, old: i64) -> Option<i64> {
        self.levels.get(&old).copied()
    }

    /// Update snapshot row `old` in place of live `new` without creating a copy.
    pub(crate) fn prefill(&mut self, t: &str, old: i64, new: i64) {
        self.map.entry(t.to_string()).or_default().insert(old, new);
        self.existing.insert((t.to_string(), old));
    }

    /// Do not create snapshot row `old`: it was deleted in this group instance.
    pub(crate) fn omit(&mut self, t: &str, old: i64) {
        self.absent.insert((t.to_string(), old));
    }

    fn all_levels(&self) -> Result<Vec<(i64, f64)>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT id,elevation FROM level ORDER BY elevation,id")?;
        let v = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    fn map_levels(&mut self, how: PlaceLevel) -> Result<(), Error> {
        let clip = self.clip;
        match how {
            PlaceLevel::Same => {
                for (id, name, e) in &clip.levels {
                    let all = self.all_levels()?;
                    let got = if self.same_project && all.iter().any(|l| l.0 == *id) {
                        Some(*id)
                    } else {
                        all.iter()
                            .find(|l| (l.1 - e).abs() <= LEVEL_TOL)
                            .map(|l| l.0)
                    };
                    let to = match got {
                        Some(t) => t,
                        None => {
                            let name = self.free_name("level", "name", name)?;
                            self.db.execute(
                                "INSERT INTO level(name,elevation) VALUES (?1,?2)",
                                rusqlite::params![name, e],
                            )?;
                            self.db.last_insert_rowid()
                        }
                    };
                    self.levels.insert(*id, to);
                }
            }
            PlaceLevel::Base(target) => {
                let all = self.all_levels()?;
                let Some(te) = all.iter().find(|l| l.0 == target).map(|l| l.1) else {
                    return invalid("paste: no such level");
                };
                let base = clip.base_level.and_then(|b| self.src_elev.get(&b).copied());
                let shift = base.map_or(0.0, |b| te - b);
                for (id, _, e) in &clip.levels {
                    if Some(*id) == clip.base_level {
                        self.levels.insert(*id, target);
                        continue;
                    }
                    let want = e + shift;
                    let near = all
                        .iter()
                        .min_by(|a, b| (a.1 - want).abs().total_cmp(&(b.1 - want).abs()))
                        .map(|l| l.0);
                    if let Some(n) = near {
                        self.levels.insert(*id, n);
                    }
                }
            }
        }
        Ok(())
    }

    fn free_name(&self, t: &str, col: &str, name: &str) -> Result<String, Error> {
        let mut st = self
            .db
            .prepare_cached(&format!("SELECT count(*) FROM {t} WHERE {col}=?1"))?;
        crate::free_name(name, |n| format!("{name}-{n}"), |cand| {
            Ok::<_, Error>(st.query_row([cand], |r| r.get::<_, i64>(0))? > 0)
        })
    }

    fn map_dicts(&mut self) -> Result<(), Error> {
        let clip = self.clip;
        for d in dict_specs() {
            let Some(r) = clip.dict_rows(d.t) else {
                continue;
            };
            if !has_table(self.db, d.t)? {
                continue;
            }
            let at = |c: &str| r.cols.iter().position(|x| x == c);
            let Some(idc) = at("id") else { continue };
            let target: Vec<String> = columns(self.db, d.t)?.into_iter().map(|c| c.0).collect();
            for row in &r.rows {
                let Some(old) = int(&row[idc]) else { continue };
                let mut vals = row.clone();
                for (c, dict) in d.fks {
                    if let Some(i) = at(c) {
                        let m = int(&vals[i])
                            .and_then(|v| self.dicts.get(dict).and_then(|x| x.get(&v)).copied());
                        vals[i] = m.map_or(Value::Null, Value::Integer);
                    }
                }
                let new = self.resolve_dict(d, &r.cols, &vals, old, &target)?;
                if let Some(n) = new {
                    self.dicts.entry(d.t).or_default().insert(old, n);
                    let creating = n != old || !self.same_project;
                    if creating {
                        self.copy_children(d, old, n)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// A live lookup row or a new copy; `None` if it cannot be created.
    fn resolve_dict(
        &mut self,
        d: &'static Dict,
        cols: &[String],
        vals: &[Value],
        old: i64,
        target: &[String],
    ) -> Result<Option<i64>, Error> {
        let keys: Vec<usize> = d
            .key
            .iter()
            .filter_map(|k| cols.iter().position(|c| c == k))
            .collect();
        if keys.len() != d.key.len() {
            return Ok(None);
        }
        if self.same_project {
            let n: i64 = self.db.query_row(
                &format!("SELECT count(*) FROM {} WHERE id=?1", d.t),
                [old],
                |r| r.get(0),
            )?;
            if n > 0 {
                return Ok(Some(old));
            }
        }
        let clause = d
            .key
            .iter()
            .enumerate()
            .map(|(i, k)| format!("{k}=?{}", i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let args: Vec<&Value> = keys.iter().map(|i| &vals[*i]).collect();
        let found: Option<i64> = self
            .db
            .query_row(
                &format!("SELECT id FROM {} WHERE {clause}", d.t),
                params_from_iter(args.iter()),
                |r| r.get(0),
            )
            .ok();
        if let Some(f) = found {
            self.kept.insert((d.t, f));
            return Ok(Some(f));
        }
        if !d.create {
            return Ok(None);
        }
        let keep: Vec<usize> = cols
            .iter()
            .enumerate()
            .filter(|(_, c)| c.as_str() != "id" && target.contains(*c))
            .map(|(i, _)| i)
            .collect();
        let names: Vec<&str> = keep.iter().map(|i| cols[*i].as_str()).collect();
        let marks = vec!["?"; names.len()].join(",");
        let args: Vec<&Value> = keep.iter().map(|i| &vals[*i]).collect();
        self.db.execute(
            &format!("INSERT INTO {}({}) VALUES ({marks})", d.t, names.join(",")),
            params_from_iter(args.iter()),
        )?;
        Ok(Some(self.db.last_insert_rowid()))
    }

    fn copy_children(&mut self, d: &'static Dict, old: i64, new: i64) -> Result<(), Error> {
        if self.kept.contains(&(d.t, new)) {
            return Ok(());
        }
        let clip = self.clip;
        for (child, col, fks) in d.children {
            let Some(r) = clip.dict_rows(child) else {
                continue;
            };
            if !has_table(self.db, child)? {
                continue;
            }
            let Some(ci) = r.cols.iter().position(|c| c == col) else {
                continue;
            };
            let target: Vec<String> = columns(self.db, child)?.into_iter().map(|c| c.0).collect();
            let keep: Vec<usize> = (0..r.cols.len())
                .filter(|i| target.contains(&r.cols[*i]))
                .collect();
            let names: Vec<&str> = keep.iter().map(|i| r.cols[*i].as_str()).collect();
            let mut insert = self.db.prepare_cached(&format!(
                "INSERT INTO {child}({}) VALUES ({})",
                names.join(","),
                vec!["?"; names.len()].join(",")
            ))?;
            for row in r.rows.iter().filter(|x| int(&x[ci]) == Some(old)) {
                let mut vals = row.clone();
                vals[ci] = Value::Integer(new);
                let mut lost = false;
                for (c, dict) in fks.iter() {
                    if let Some(i) = r.cols.iter().position(|x| x == c) {
                        match int(&vals[i])
                            .and_then(|v| self.dicts.get(dict).and_then(|x| x.get(&v)).copied())
                        {
                            Some(n) => vals[i] = Value::Integer(n),
                            None => lost = true,
                        }
                    }
                }
                if lost {
                    continue;
                }
                let args: Vec<&Value> = keep.iter().map(|i| &vals[*i]).collect();
                insert.execute(params_from_iter(args.iter()))?;
            }
        }
        Ok(())
    }

    fn lookup(&self, t: &str, id: i64) -> Option<i64> {
        self.map.get(t).and_then(|m| m.get(&id)).copied()
    }

    fn alive(&self, t: &str, id: i64) -> Result<bool, Error> {
        let n: i64 = self
            .db
            .prepare_cached(&format!("SELECT count(*) FROM {t} WHERE id=?1"))?
            .query_row([id], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// A new copy, or a live element on the same levels of the same model, or nothing.
    fn resolve(&self, t: &str, id: i64) -> Result<Option<i64>, Error> {
        if let Some(n) = self.lookup(t, id) {
            return Ok(Some(n));
        }
        if self.keep_refs && has_table(self.db, t)? && self.alive(t, id)? {
            return Ok(Some(id));
        }
        Ok(None)
    }

    /// For a host: after a shift the live element is no longer under the row, so a reference to it is wrong.
    fn resolve_owner(&self, t: &str, id: i64) -> Result<Option<i64>, Error> {
        if let Some(n) = self.lookup(t, id) {
            return Ok(Some(n));
        }
        if self.in_place && has_table(self.db, t)? && self.alive(t, id)? {
            return Ok(Some(id));
        }
        Ok(None)
    }

    fn elev_of(&self, level: i64) -> Result<Option<f64>, Error> {
        let e = self
            .db
            .prepare_cached("SELECT elevation FROM level WHERE id=?1")?
            .query_row([level], |r| r.get::<_, f64>(0))
            .ok();
        Ok(e)
    }

    fn map_dim_refs(&self, s: &str) -> Result<Option<String>, Error> {
        let mut out = vec![];
        for tok in s.split(';').filter(|t| !t.trim().is_empty()) {
            if let Some(rest) = tok.strip_prefix("H ") {
                let mut it = rest.split_whitespace();
                let (Some(k), Some(id)) =
                    (it.next(), it.next().and_then(|v| v.parse::<i64>().ok()))
                else {
                    return Ok(None);
                };
                let Some((t, _)) = ref_targets(tok).first().copied() else {
                    return Ok(None);
                };
                let Some(n) = self.resolve(t, id)? else {
                    return Ok(None);
                };
                out.push(format!("H {k} {n}"));
                continue;
            }
            let Some(r) = parse_ref(tok) else {
                out.push(tok.to_string());
                continue;
            };
            let w = |v: i64| self.resolve("wall", v);
            let m = match r {
                DimRef::Point((x, y)) => Some(DimRef::Point((x + self.dx, y + self.dy))),
                DimRef::WallEnd { wall, face, end } => {
                    w(wall)?.map(|wall| DimRef::WallEnd { wall, face, end })
                }
                DimRef::OpeningEdge {
                    opening,
                    edge,
                    face,
                } => self
                    .resolve("opening", opening)?
                    .map(|opening| DimRef::OpeningEdge {
                        opening,
                        edge,
                        face,
                    }),
                DimRef::AxisEnd { wall, end } => w(wall)?.map(|wall| DimRef::AxisEnd { wall, end }),
                DimRef::AxisLine { wall } => w(wall)?.map(|wall| DimRef::AxisLine { wall }),
                DimRef::ArcFace { wall, face } => {
                    w(wall)?.map(|wall| DimRef::ArcFace { wall, face })
                }
                DimRef::WallLevel { wall, top } => {
                    w(wall)?.map(|wall| DimRef::WallLevel { wall, top })
                }
                DimRef::GridAt { grid, wall } => match (self.resolve("grid", grid)?, w(wall)?) {
                    (Some(grid), Some(wall)) => Some(DimRef::GridAt { grid, wall }),
                    _ => None,
                },
            };
            let Some(m) = m else { return Ok(None) };
            out.push(refs_text(&[m]));
        }
        Ok(Some(out.join(";")))
    }

    /// Wall of the level under point `p` with room for width `w`: (id, along the axis).
    fn find_wall(
        &self,
        level: i64,
        p: (f64, f64),
        w: f64,
        check_overlap: bool,
    ) -> Result<Option<(i64, f64)>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT id,ax,ay,bx,by FROM wall WHERE level=?1 AND COALESCE(bulge,0)=0 ORDER BY id",
        )?;
        let walls = st
            .query_map([level], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut best: Option<(f64, i64, f64)> = None;
        for (id, ax, ay, bx, by) in walls {
            let (ux, uy) = (bx - ax, by - ay);
            let len = ux.hypot(uy);
            if len < 1e-9 {
                continue;
            }
            let t = ((p.0 - ax) * ux + (p.1 - ay) * uy) / len;
            let d = ((p.0 - ax) * uy - (p.1 - ay) * ux).abs() / len;
            let start = t - w * 0.5;
            if d > REHOST_DIST || start < -1e-9 || start + w > len + 1e-9 {
                continue;
            }
            if check_overlap {
                let mut o = self
                    .db
                    .prepare_cached("SELECT along,width FROM opening WHERE wall=?1")?;
                let taken = o
                    .query_map([id], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                if taken.iter().any(|(a, b)| start < a + b && a < &(start + w)) {
                    continue;
                }
            }
            if best.map_or(true, |b| d < b.0) {
                best = Some((d, id, start));
            }
        }
        Ok(best.map(|b| (b.1, b.2)))
    }

    fn src_point(&self, wall: i64, along: f64) -> Option<(f64, f64)> {
        host_point(self.hosts.get(&wall)?, along)
    }

    /// Slab or ceiling of the level whose outline contains the point.
    fn find_area(&self, t: &str, level: i64, p: (f64, f64)) -> Result<Option<i64>, Error> {
        let vt = format!("{t}_vertex");
        let mut st = self
            .db
            .prepare_cached(&format!("SELECT id FROM {t} WHERE level=?1 ORDER BY id"))?;
        let ids = st
            .query_map([level], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            let mut v = self.db.prepare_cached(&format!(
                "SELECT x,y FROM {vt} WHERE {t}=?1 AND ring=0 ORDER BY idx"
            ))?;
            let ring = v
                .query_map([id], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut inside = false;
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0)
                {
                    inside = !inside;
                }
            }
            if inside {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    fn skip(&mut self, t: &str, id: i64, why: &str) {
        self.report
            .skipped
            .push((t.to_string(), id, why.to_string()));
    }

    /// Snapshot row in target coordinates and ids; `None` if it cannot be placed.
    fn place(
        &mut self,
        s: &Spec,
        cols: &[String],
        notnull: &HashMap<String, bool>,
        row: &[Value],
    ) -> Result<Option<Vec<Value>>, Error> {
        let at = |c: &str| cols.iter().position(|x| x == c);
        let required = |c: &str| notnull.get(c).copied().unwrap_or(false);
        let id =
            s.id.and_then(|c| at(c))
                .and_then(|i| int(&row[i]))
                .unwrap_or(0);
        let mut v = row.to_vec();
        // levels
        let (mut src, mut dst) = (None, None);
        for c in s.levels {
            let Some(i) = at(c) else { continue };
            let Some(old) = int(&v[i]) else { continue };
            match self.levels.get(&old).copied() {
                Some(n) => {
                    v[i] = Value::Integer(n);
                    if src.is_none() {
                        src = Some(old);
                        dst = Some(n);
                    }
                }
                None if required(c) => return Ok(None),
                None => v[i] = Value::Null,
            }
        }
        let base_elev = match dst {
            Some(d) => self.elev_of(d)?,
            None => None,
        };
        for c in s.tops {
            let Some(i) = at(c) else { continue };
            let Some(old) = int(&v[i]) else { continue };
            let mapped = self.levels.get(&old).copied();
            let above = match (mapped, base_elev) {
                (Some(m), Some(b)) => self.elev_of(m)?.is_some_and(|e| e > b + 1e-6),
                (Some(_), None) => true,
                _ => false,
            };
            let next = if above {
                mapped
            } else {
                let b = base_elev.unwrap_or(f64::MIN);
                let all = self.all_levels()?;
                let up = all.iter().find(|l| l.1 > b + 1e-6).map(|l| l.0);
                up
            };
            match next {
                Some(n) => v[i] = Value::Integer(n),
                None if required(c) => return Ok(None),
                None => v[i] = Value::Null,
            }
        }
        let dz = match (src.and_then(|l| self.src_elev.get(&l).copied()), base_elev) {
            (Some(a), Some(b)) => b - a,
            _ => 0.0,
        };
        // lookups
        for (c, d) in s.dicts {
            let Some(i) = at(c) else { continue };
            let Some(old) = int(&v[i]) else { continue };
            match self.dicts.get(d).and_then(|m| m.get(&old)).copied() {
                Some(n) => v[i] = Value::Integer(n),
                None if required(c) => return Ok(None),
                None => v[i] = Value::Null,
            }
        }
        // hosts
        let (dx, dy) = (self.dx, self.dy);
        let mut hosted_local = false;
        if s.t == "comp" {
            hosted_local = at("host_wall").and_then(|i| int(&v[i])).is_some();
            if !self.place_comp(cols, &mut v, dst)? {
                self.skip(s.t, id, "host");
                return Ok(None);
            }
        } else if s.t == "opening" {
            if !self.place_opening(cols, &mut v)? {
                self.skip(s.t, id, "host");
                return Ok(None);
            }
        } else {
            for (c, parent) in s.owns {
                let Some(i) = at(c) else { continue };
                let Some(old) = int(&v[i]) else { continue };
                match self.resolve_owner(parent, old)? {
                    Some(n) => v[i] = Value::Integer(n),
                    None if required(c) || s.t == "railing" => {
                        if s.id.is_some() {
                            self.skip(s.t, id, "host");
                        }
                        return Ok(None);
                    }
                    None => v[i] = Value::Null,
                }
            }
        }
        for (c, parent) in s.refs {
            let Some(i) = at(c) else { continue };
            let Some(old) = int(&v[i]) else { continue };
            match self.resolve(parent, old)? {
                Some(n) => v[i] = Value::Integer(n),
                None if required(c) => return Ok(None),
                None => v[i] = Value::Null,
            }
        }
        if let Some((kc, rc)) = s.poly {
            let (Some(ki), Some(ri)) = (at(kc), at(rc)) else {
                return Ok(None);
            };
            let (Some(k), Some(old)) = (text(&v[ki]).map(str::to_string), int(&v[ri])) else {
                return Ok(None);
            };
            let Some(n) = kind_table(s, &k).and_then(|t| self.lookup(t, old)) else {
                if s.id.is_some() {
                    self.skip(s.t, id, "host");
                }
                return Ok(None);
            };
            v[ri] = Value::Integer(n);
        }
        match s.t {
            "mass_host" => {
                let Some(i) = at("mass") else { return Ok(None) };
                let Some(n) = int(&v[i]).and_then(|m| self.lookup("mass", m)) else {
                    return Ok(None);
                };
                v[i] = Value::Integer(n);
            }
            "solid_host" => {
                let (Some(ki), Some(ri)) = (at("kind"), at("ref")) else {
                    return Ok(None);
                };
                let (Some(k), Some(old)) = (text(&v[ki]).map(str::to_string), int(&v[ri])) else {
                    return Ok(None);
                };
                let Some(n) = self.resolve(&k, old)? else {
                    return Ok(None);
                };
                v[ri] = Value::Integer(n);
            }
            "brace" => {
                for (hc, rc) in [("host_a", "ref_a"), ("host_b", "ref_b")] {
                    let (Some(hi), Some(ri)) = (at(hc), at(rc)) else {
                        continue;
                    };
                    let (Some(h), Some(old)) = (text(&v[hi]).map(str::to_string), int(&v[ri]))
                    else {
                        continue;
                    };
                    match self.resolve(&h, old)? {
                        Some(n) => v[ri] = Value::Integer(n),
                        None => {
                            v[hi] = Value::Null;
                            v[ri] = Value::Null;
                        }
                    }
                }
            }
            "geom_join" => {
                for (kc, rc) in [("winner_kind", "winner"), ("loser_kind", "loser")] {
                    let (Some(ki), Some(ri)) = (at(kc), at(rc)) else {
                        return Ok(None);
                    };
                    let (Some(k), Some(old)) = (text(&v[ki]).map(str::to_string), int(&v[ri]))
                    else {
                        return Ok(None);
                    };
                    let Some(n) = self.lookup(&k, old) else {
                        return Ok(None);
                    };
                    v[ri] = Value::Integer(n);
                }
            }
            "grid" => {
                if let Some(i) = at("name") {
                    if let Some(n) = text(&v[i]).map(str::to_string) {
                        // a live axis of a group instance keeps its own name
                        let live = self.existing.contains(&("grid".to_string(), id));
                        let name = match self.lookup("grid", id).filter(|_| live) {
                            Some(l) => self
                                .db
                                .prepare_cached("SELECT name FROM grid WHERE id=?1")?
                                .query_row([l], |r| r.get::<_, String>(0))?,
                            None => self.free_name("grid", "name", &n)?,
                        };
                        v[i] = Value::Text(name);
                    }
                }
            }
            "anno" => {
                let (Some(ki), Some(ri)) = (at("view_kind"), at("view_id")) else {
                    return Ok(None);
                };
                let plan = text(&v[ki]) == Some("plan");
                let mapped = int(&v[ri]).and_then(|l| self.levels.get(&l).copied());
                match mapped {
                    Some(n) if plan => v[ri] = Value::Integer(n),
                    _ if !plan && self.same_project => {}
                    _ => {
                        self.skip(s.t, id, "view");
                        return Ok(None);
                    }
                }
            }
            _ => {}
        }
        // coordinates
        if !hosted_local {
            for (xc, yc) in s.xy {
                if let (Some(xi), Some(yi)) = (at(xc), at(yc)) {
                    if let Some(x) = num(&v[xi]) {
                        v[xi] = Value::Real(x + dx);
                    }
                    if let Some(y) = num(&v[yi]) {
                        v[yi] = Value::Real(y + dy);
                    }
                }
            }
        }
        if s.t == "comp" && !hosted_local {
            if let (Some(xi), Some(yi)) = (at("x"), at("y")) {
                if let (Some(x), Some(y)) = (num(&v[xi]), num(&v[yi])) {
                    v[xi] = Value::Real(x + dx);
                    v[yi] = Value::Real(y + dy);
                }
            }
        }
        for (c, tx) in s.tx {
            let Some(i) = at(c) else { continue };
            let Some(t) = text(&v[i]).map(str::to_string) else {
                continue;
            };
            if t.trim().is_empty() {
                continue;
            }
            let new = match tx {
                Tx::Slope => {
                    let mut p = crate::slabs::parse_slope(&t)?;
                    p.p0.x += dx;
                    p.p0.y += dy;
                    Some(crate::slabs::slope_text(&p))
                }
                Tx::PairsJson => translate_pairs(&t, dx, dy),
                Tx::Ring => translate_ring(&t, dx, dy),
                Tx::Lines3 => translate_lines3(&t, dx, dy, 0.0),
                Tx::Points3 => translate_lines3(&t, dx, dy, dz),
                Tx::Sketch => translate_sketch(&t, dx, dy),
                Tx::Shape => translate_shape(&t, dx, dy),
                Tx::Form if dx == 0.0 && dy == 0.0 => Some(t.clone()),
                Tx::Form => Some(translate_form(&t, dx, dy)?),
                Tx::CommaPairs => translate_comma(&t, dx, dy),
                Tx::AnnoJson => translate_anno(&t, dx, dy),
                Tx::SplineAxis => crate::wall_spline::translate_axis(&t, dx, dy),
                Tx::DimRefs => self.map_dim_refs(&t)?,
                Tx::Hosts => {
                    let hosts: Vec<i64> = serde_json::from_str(&t).unwrap_or_default();
                    let k = at("host_kind")
                        .and_then(|i| text(&v[i]))
                        .unwrap_or("")
                        .to_string();
                    let m: Option<Vec<i64>> = hosts.iter().map(|h| self.lookup(&k, *h)).collect();
                    m.filter(|x| !x.is_empty())
                        .and_then(|x| serde_json::to_string(&x).ok())
                }
                Tx::Walls => {
                    let m: Option<Vec<i64>> = parse_ids(&t)
                        .and_then(|w| w.iter().map(|x| self.lookup("wall", *x)).collect());
                    m.map(|x| x.iter().map(i64::to_string).collect::<Vec<_>>().join(" "))
                }
            };
            match new {
                Some(n) => v[i] = Value::Text(n),
                None => {
                    if s.id.is_some() && !matches!(tx, Tx::Walls) {
                        self.skip(s.t, id, "refs");
                    }
                    return Ok(None);
                }
            }
        }
        Ok(Some(v))
    }

    /// Standalone opening: the wall in the snapshot or the nearest suitable one on the target level.
    fn place_opening(&mut self, cols: &[String], v: &mut [Value]) -> Result<bool, Error> {
        let at = |c: &str| cols.iter().position(|x| x == c);
        let (Some(wi), Some(ai), Some(wd)) = (at("wall"), at("along"), at("width")) else {
            return Ok(false);
        };
        let Some(old) = int(&v[wi]) else {
            return Ok(false);
        };
        if let Some(n) = self.lookup("wall", old) {
            v[wi] = Value::Integer(n);
            return Ok(true);
        }
        let (Some(along), Some(width)) = (num(&v[ai]), num(&v[wd])) else {
            return Ok(false);
        };
        let Some(p) = self.src_point(old, along + width * 0.5) else {
            return Ok(false);
        };
        let Some(level) = self.host_level(old) else {
            return Ok(false);
        };
        let Some((w, a)) = self.find_wall(level, (p.0 + self.dx, p.1 + self.dy), width, true)?
        else {
            return Ok(false);
        };
        v[wi] = Value::Integer(w);
        v[ai] = Value::Real(a);
        Ok(true)
    }

    /// Target level for an opening without a wall: where its wall's level landed, otherwise the snapshot anchor.
    fn host_level(&self, wall: i64) -> Option<i64> {
        let own = self.hosts.get(&wall).and_then(|h| h.5);
        own.and_then(|l| self.levels.get(&l).copied()).or_else(|| {
            self.clip
                .base_level
                .and_then(|b| self.levels.get(&b).copied())
        })
    }

    fn place_comp(
        &mut self,
        cols: &[String],
        v: &mut [Value],
        dst: Option<i64>,
    ) -> Result<bool, Error> {
        let at = |c: &str| cols.iter().position(|x| x == c);
        let (Some(xi), Some(yi)) = (at("x"), at("y")) else {
            return Ok(false);
        };
        let (Some(x), Some(y)) = (num(&v[xi]), num(&v[yi])) else {
            return Ok(false);
        };
        if let Some(i) = at("host_wall") {
            if let Some(old) = int(&v[i]) {
                if let Some(n) = self.lookup("wall", old) {
                    v[i] = Value::Integer(n);
                    return Ok(true);
                }
                if self.in_place && self.alive("wall", old)? {
                    return Ok(true);
                }
                let (Some(p), Some(level)) = (self.src_point(old, x), dst) else {
                    return Ok(false);
                };
                let Some((w, a)) =
                    self.find_wall(level, (p.0 + self.dx, p.1 + self.dy), 0.0, false)?
                else {
                    return Ok(false);
                };
                v[i] = Value::Integer(w);
                v[xi] = Value::Real(a);
                return Ok(true);
            }
        }
        for (c, t) in [("host_slab", "slab"), ("host_ceiling", "ceiling")] {
            let Some(i) = at(c) else { continue };
            let Some(old) = int(&v[i]) else { continue };
            let n = match self.resolve_owner(t, old)? {
                Some(n) => Some(n),
                None => match dst {
                    Some(level) => self.find_area(t, level, (x + self.dx, y + self.dy))?,
                    None => None,
                },
            };
            let Some(n) = n else { return Ok(false) };
            v[i] = Value::Integer(n);
        }
        Ok(true)
    }

    /// Deletes id-less child rows of live elements: the instance being updated gets them anew.
    fn clear_children(&self, s: &Spec, snaps: &mut Snapshots) -> Result<(), Error> {
        let owner_col: Option<(&str, &str)> = s.owns.first().copied();
        for (t, m) in &self.map {
            for (old, new) in m {
                if !self.existing.contains(&(t.clone(), *old)) {
                    continue;
                }
                if s.t == "mass_host" {
                    // a shape attachment outside the instance is not its own: the snapshot neither carries nor restores it
                    let own: Vec<i64> = self
                        .map
                        .get("mass")
                        .map_or(vec![], |m| m.values().copied().collect());
                    let rows: Vec<(i64, i64)> = {
                        let mut st = self.db.prepare_cached(
                            "SELECT rowid,mass FROM mass_host WHERE kind=?1 AND ref=?2",
                        )?;
                        let v = st
                            .query_map(rusqlite::params![t, new], |r| Ok((r.get(0)?, r.get(1)?)))?
                            .collect::<Result<Vec<_>, _>>()?;
                        v
                    };
                    for (rowid, _) in rows.into_iter().filter(|x| own.contains(&x.1)) {
                        snaps.save(self.db, "mass_host", "rowid", rowid)?;
                        self.db
                            .execute("DELETE FROM mass_host WHERE rowid=?1", [rowid])?;
                    }
                } else if let Some((kc, rc)) = s.poly {
                    let n: i64 = self.db.query_row(
                        &format!("SELECT count(*) FROM {} WHERE {kc}=?1 AND {rc}=?2", s.t),
                        rusqlite::params![t, new],
                        |r| r.get(0),
                    )?;
                    if n > 0 {
                        snaps.save(self.db, s.t, rc, *new)?;
                        snaps.push_front(
                            &format!("DELETE FROM {} WHERE {kc}=?1 AND {rc}=?2", s.t),
                            vec![Value::Text(t.clone()), Value::Integer(*new)],
                        );
                        self.db.execute(
                            &format!("DELETE FROM {} WHERE {kc}=?1 AND {rc}=?2", s.t),
                            rusqlite::params![t, new],
                        )?;
                    }
                } else if let Some((col, parent)) = owner_col {
                    if parent == t {
                        snaps.save(self.db, s.t, col, *new)?;
                        snaps.push_front(
                            &format!("DELETE FROM {} WHERE {col}=?1", s.t),
                            vec![Value::Integer(*new)],
                        );
                        self.db
                            .execute(&format!("DELETE FROM {} WHERE {col}=?1", s.t), [*new])?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Walks all snapshot tables. `snaps` is needed only when updating live rows.
    /// Insert and update are prepared once per table: everything runs in one `op` transaction.
    pub(crate) fn run(&mut self, snaps: &mut Snapshots) -> Result<(), Error> {
        let clip = self.clip;
        let db: &'a Connection = self.db;
        for s in specs() {
            let Some(r) = clip.rows(s.t) else { continue };
            if !has_table(db, s.t)? {
                continue;
            }
            let target = columns(db, s.t)?;
            let notnull: HashMap<String, bool> = target.iter().cloned().collect();
            let keep: Vec<usize> = (0..r.cols.len())
                .filter(|i| notnull.contains_key(&r.cols[*i]))
                .collect();
            let idc = s.id.and_then(|c| r.cols.iter().position(|x| x == c));
            let names: Vec<&str> = keep
                .iter()
                .filter(|i| Some(**i) != idc)
                .map(|i| r.cols[*i].as_str())
                .collect();
            if s.id.is_none() && !self.existing.is_empty() {
                self.clear_children(s, snaps)?;
            }
            if names.is_empty() {
                continue;
            }
            let or_ignore = if s.poly.is_some() || s.t == "geom_join" {
                "OR IGNORE "
            } else {
                ""
            };
            let mut insert = db.prepare(&format!(
                "INSERT {or_ignore}INTO {}({}) VALUES ({})",
                s.t,
                names.join(","),
                vec!["?"; names.len()].join(",")
            ))?;
            let mut update = match s.id {
                Some(c) if self.existing.iter().any(|e| e.0 == s.t) => {
                    let set = names
                        .iter()
                        .map(|n| format!("{n}=?"))
                        .collect::<Vec<_>>()
                        .join(",");
                    Some(db.prepare(&format!("UPDATE {} SET {set} WHERE {c}=?", s.t))?)
                }
                _ => None,
            };
            for row in &r.rows {
                let old = idc.and_then(|i| int(&row[i]));
                if old.is_some_and(|o| self.absent.contains(&(s.t.to_string(), o))) {
                    continue;
                }
                let Some(mut v) = self.place(s, &r.cols, &notnull, row)? else {
                    continue;
                };
                let args: Vec<Value> = keep
                    .iter()
                    .filter(|i| Some(**i) != idc)
                    .map(|i| std::mem::replace(&mut v[*i], Value::Null))
                    .collect();
                let live = old.filter(|o| self.existing.contains(&(s.t.to_string(), *o)));
                let (new, is_new) = match (live, s.id, update.as_mut()) {
                    (Some(o), Some(c), Some(u)) => {
                        let n = self.lookup(s.t, o).unwrap_or(o);
                        snaps.save(db, s.t, c, n)?;
                        let mut a = args;
                        a.push(Value::Integer(n));
                        u.execute(params_from_iter(a.iter()))?;
                        (n, false)
                    }
                    _ => {
                        if insert.execute(params_from_iter(args.iter()))? == 0 {
                            continue;
                        }
                        (db.last_insert_rowid(), true)
                    }
                };
                let Some(o) = old else { continue };
                if is_new {
                    self.map.entry(s.t.to_string()).or_default().insert(o, new);
                }
                self.after_row(s, row, &r.cols, new)?;
            }
        }
        let told: HashSet<(String, i64)> = self
            .report
            .skipped
            .iter()
            .map(|x| (x.0.clone(), x.1))
            .collect();
        for (t, id) in &clip.roots {
            match self.lookup(t, *id) {
                Some(n) => self.report.created.push((t.clone(), n)),
                None => {
                    let key = (t.clone(), *id);
                    if !told.contains(&key) && !self.absent.contains(&key) {
                        self.report
                            .skipped
                            .push((t.clone(), *id, "host".to_string()));
                    }
                }
            }
        }
        Ok(())
    }

    /// Bodies with coordinates inside blobs and definitions are carried over by their own functions.
    fn after_row(&self, s: &Spec, row: &[Value], cols: &[String], new: i64) -> Result<(), Error> {
        let (dx, dy) = (self.dx, self.dy);
        match s.t {
            "mesh" => {
                let dz = match cols
                    .iter()
                    .position(|c| c == "level")
                    .and_then(|i| int(&row[i]))
                {
                    Some(old) => match (self.src_elev.get(&old), self.levels.get(&old)) {
                        (Some(a), Some(n)) => self.elev_of(*n)?.map_or(0.0, |b| b - a),
                        _ => 0.0,
                    },
                    None => 0.0,
                };
                if dx != 0.0 || dy != 0.0 || dz != 0.0 {
                    crate::meshes::xf_mesh(self.db, new, IDENT, (dx, dy), dz)?;
                }
            }
            "solid" if dx != 0.0 || dy != 0.0 => {
                crate::solids::transform(self.db, new, IDENT, (dx, dy))?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn paste_in(
    db: &Connection,
    snaps: &mut Snapshots,
    clip: &Clip,
    o: &PasteOpts,
) -> Result<PasteReport, Error> {
    let mut p = Paster::new(db, clip, o)?;
    p.paste_materials()?;
    p.run(snaps)?;
    p.paste_mesh_materials()?;
    crate::groups::paste_instances(db, clip, &p, o)?;
    Ok(p.report)
}

impl Model {
    /// Snapshot of the selection with dependent rows; the model file is unchanged.
    pub fn clip_copy(&self, items: &[(&str, i64)]) -> Result<Clip, Error> {
        snapshot(&self.db, items, SnapOpts { relations: true })
    }

    /// Snapshot and delete: one undo step.
    pub fn clip_cut(&mut self, items: &[(&str, i64)]) -> Result<Clip, Error> {
        let clip = self.clip_copy(items)?;
        self.delete_items("wall_cut", items)?;
        Ok(clip)
    }

    /// Pastes a snapshot as one journal entry; an empty paste is an error with no undo step.
    pub fn clip_paste(&mut self, clip: &Clip, o: &PasteOpts) -> Result<PasteReport, Error> {
        self.clip_paste_many(clip, std::slice::from_ref(o))
    }

    /// Paste onto selected levels: copy a floor onto several floors at once, one undo step.
    pub fn clip_paste_levels(&mut self, clip: &Clip, levels: &[i64]) -> Result<PasteReport, Error> {
        // on the source floor of the same model the copy would land on top of the original
        let same = clip.project == project_token(&self.db)?;
        let opts: Vec<PasteOpts> = levels
            .iter()
            .filter(|l| !(same && clip.base_level == Some(**l)))
            .map(|l| PasteOpts {
                dx: 0.0,
                dy: 0.0,
                level: PlaceLevel::Base(*l),
            })
            .collect();
        if opts.is_empty() && !levels.is_empty() {
            return invalid("paste: the elements are already on this level");
        }
        self.clip_paste_many(clip, &opts)
    }

    fn clip_paste_many(&mut self, clip: &Clip, opts: &[PasteOpts]) -> Result<PasteReport, Error> {
        if clip.is_empty() || opts.is_empty() {
            return invalid("paste: clipboard is empty");
        }
        let tables = write_tables(&self.db)?;
        self.batch_in("wall_paste", &tables, |db, s| {
            let mut all = PasteReport::default();
            for o in opts {
                let r = paste_in(db, s, clip, o)?;
                all.created.extend(r.created);
                all.skipped.extend(r.skipped);
                for (dst, src) in [
                    (&mut all.materials_created, r.materials_created),
                    (&mut all.materials_differ, r.materials_differ),
                ] {
                    for n in src {
                        if !dst.contains(&n) {
                            dst.push(n);
                        }
                    }
                }
            }
            if all.created.is_empty() {
                return invalid("paste: no element could be placed");
            }
            crate::constraints::verify(db)?;
            Ok(all)
        })
    }

    /// Deletes a heterogeneous selection in one undo step; items already removed by cascade are skipped.
    pub fn delete_items(&mut self, kind: &str, items: &[(&str, i64)]) -> Result<(), Error> {
        let items: Vec<(String, i64)> = items.iter().map(|(k, i)| ((*k).to_string(), *i)).collect();
        self.compound(kind, |m| {
            // batches first: joins and quantities rebuild once, not once per element
            let mut walls: Vec<i64> = vec![];
            for (t, id) in &items {
                if t == "wall" && row_exists(&m.db, t, *id)? {
                    walls.push(*id);
                }
            }
            walls.sort_unstable();
            walls.dedup();
            if !walls.is_empty() {
                m.delete_walls(&walls)?;
            }
            let mut bodies: Vec<(&str, i64)> = vec![];
            for (t, id) in &items {
                if BATCHED_BODIES.contains(&t.as_str()) && row_exists(&m.db, t, *id)? {
                    bodies.push((t.as_str(), *id));
                }
            }
            bodies.sort_unstable();
            bodies.dedup();
            if !bodies.is_empty() {
                m.delete_imported(&bodies)?;
            }
            for (t, id) in &items {
                if t != "wall"
                    && !BATCHED_BODIES.contains(&t.as_str())
                    && row_exists(&m.db, t, *id)?
                {
                    m.delete_one(t, *id)?;
                }
            }
            Ok(())
        })
    }

    fn delete_one(&mut self, t: &str, id: i64) -> Result<(), Error> {
        use crate::drawing::Entity;
        match t {
            "wall" => self.delete_walls(&[id]),
            "opening" => self.delete_opening(id),
            "curtain_wall" => self.delete_curtain_wall(id),
            "slab" => self.delete_slab(id),
            "room" => self.delete_room(id),
            "room_sep" => self.delete_room_sep(id),
            "zone" => self.delete_zone(id),
            "view_def" => self.delete_view_def(id),
            "mesh" | "col" | "beam" | "mep_run" | "ceiling" => self.delete_imported(&[(t, id)]),
            "comp" => self.delete_component(id),
            "roof" => self.delete_roof(id),
            "shaft" => self.delete_shaft(id),
            "zone" => self.delete_zone(id),
            "stair" => self.delete_stair(id),
            "railing" => self.delete_railing(id),
            "iso_footing" => self.delete_struct("footing", id),
            "strip_footing" | "raft" | "brace" | "truss" | "beam_system" => {
                self.delete_struct(t, id)
            }
            "toposolid" => self.delete_toposolid(id),
            "topo_region" => self.delete_topo_region(id),
            "topo_pad" => self.delete_pad(id),
            "grid" => self.delete_grid(id),
            "ref_plane" => self.delete_ref_plane(id),
            "solid" => self.delete_solid(id),
            "freeform" => self.delete_freeform(id),
            "panel_system" => self.delete_panel_system(id),
            "shell_elem" => self.delete_shell_elem(id),
            "rebar_set" => self.delete_rebar(&[id]),
            "mass" => self.delete_mass(id),
            "dim" => {
                let tag: String =
                    self.db
                        .query_row("SELECT kind FROM dim WHERE id=?1", [id], |r| r.get(0))?;
                if tag == "tag" {
                    self.delete_tag(id)
                } else {
                    self.delete_dim(id)
                }
            }
            "skylight" | "soffit" | "sweep" => self.delete_roof_edge(t, id),
            "roof_taper" => self.delete_roof_edge("taper", id),
            "host_opening" => self.delete_host_opening(id),
            "d_line" => self.delete_entity(Entity::Line, id),
            "d_polyline" => self.delete_entity(Entity::Polyline, id),
            "d_arc" => self.delete_entity(Entity::Arc, id),
            "d_text" => self.delete_entity(Entity::Text, id),
            "d_insert" => self.delete_entity(Entity::Insert, id),
            _ => self.delete_registered(t, id),
        }
    }

    /// Registry-driven deletion: the row and its dependents, for tables without their own delete operation.
    fn delete_registered(&mut self, t: &str, id: i64) -> Result<(), Error> {
        let Some((s, idc)) = spec(t).and_then(|s| s.id.map(|c| (s, c))) else {
            return invalid("delete: element kind has no delete");
        };
        self.op("wall_delete_element", |db| {
            let mut inv = Inv::default();
            for c in specs().filter(|c| c.id.is_none()) {
                if !has_table(db, c.t)? {
                    continue;
                }
                for (col, parent) in c.owns {
                    if *parent == s.t {
                        inv = inv.restore(db, c.t, col, id)?;
                        db.execute(&format!("DELETE FROM {} WHERE {col}=?1", c.t), [id])?;
                    }
                }
                if let Some((kc, rc)) = c.poly {
                    inv = inv.restore(db, c.t, rc, id)?;
                    db.execute(
                        &format!("DELETE FROM {} WHERE {rc}=?1 AND {kc}=?2", c.t),
                        rusqlite::params![id, s.t],
                    )?;
                }
            }
            for (child, col, parent) in reg::DELETE_CASCADE {
                if *parent == s.t && has_table(db, child)? {
                    inv = inv.restore(db, child, col, id)?;
                    db.execute(&format!("DELETE FROM {child} WHERE {col}=?1"), [id])?;
                }
            }
            inv = inv.restore(db, s.t, idc, id)?;
            db.execute(&format!("DELETE FROM {} WHERE {idc}=?1", s.t), [id])?;
            Ok(((), id, inv))
        })
    }

    /// Copies type and parameters from a sample onto targets of the same category. Returns the number changed.
    pub fn match_properties(
        &mut self,
        sample: (&str, i64),
        targets: &[(&str, i64)],
    ) -> Result<usize, Error> {
        let (t, sid) = sample;
        let Some(cols) = match_columns(t) else {
            return invalid("match properties: this category has no matchable properties");
        };
        let (t, cols): (String, Vec<&'static str>) = (t.to_string(), cols.to_vec());
        let targets: Vec<i64> = targets
            .iter()
            .filter(|(k, i)| *k == t && *i != sid)
            .map(|x| x.1)
            .collect();
        if targets.is_empty() {
            return invalid("match properties: no target of the same category");
        }
        self.batch_in("wall_match", &["pparam_val"], |db, s| {
            let have: Vec<String> = columns(db, &t)?.into_iter().map(|c| c.0).collect();
            let cols: Vec<&str> = cols.iter().copied().filter(|c| have.iter().any(|h| h == c)).collect();
            if cols.is_empty() {
                return invalid("match properties: no columns");
            }
            let sql = format!("SELECT {} FROM {t} WHERE id=?1", cols.join(","));
            let sampled = select_rows(db, &sql, cols.len(), &[Value::Integer(sid)])?;
            let Some(vals) = sampled.into_iter().next() else {
                return invalid("match properties: sample is gone");
            };
            let update = format!(
                "UPDATE {t} SET {} WHERE id=?",
                cols.iter().map(|c| format!("{c}=?")).collect::<Vec<_>>().join(",")
            );
            let params: Vec<(i64, Value)> = {
                let mut st = db.prepare("SELECT param,value FROM pparam_val WHERE kind=?1 AND ref=?2 ORDER BY rowid")?;
                let v = st
                    .query_map(rusqlite::params![t, sid], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Value>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                v
            };
            let mut n = 0;
            for id in &targets {
                if !match_fits(db, &t, *id, &cols, &vals)? {
                    continue;
                }
                s.save(db, &t, "id", *id)?;
                let mut a = vals.clone();
                a.push(Value::Integer(*id));
                n += db.execute(&update, params_from_iter(a.iter()))?;
                for (p, v) in &params {
                    // a locked value of the target is kept
                    if crate::groups::is_locked_param(db, &t, *id, *p)? {
                        continue;
                    }
                    s.save(db, "pparam_val", "ref", *id)?;
                    db.execute(
                        "INSERT OR REPLACE INTO pparam_val(param,kind,ref,value) VALUES (?1,?2,?3,?4)",
                        rusqlite::params![p, t, id, v],
                    )?;
                }
            }
            if n == 0 {
                return invalid("match properties: no target accepts these properties");
            }
            crate::constraints::verify(db)?;
            Ok(n)
        })
    }

    /// Joins geometry of all intersecting pairs in the selection; cut priority is by category.
    pub fn join_selected(&mut self, items: &[(&str, i64)]) -> Result<usize, Error> {
        let pairs = self.overlapping_pairs(items)?;
        let done = self.joined_pairs()?;
        let todo: Vec<_> = pairs
            .into_iter()
            .filter(|(a, b)| !pair_in(&done, a, b))
            .collect();
        if todo.is_empty() {
            return invalid("join: no intersecting elements left to join");
        }
        let n = todo.len();
        self.compound("mass_join", |m| {
            for (a, b) in &todo {
                m.join_geometry((a.0.as_str(), a.1), (b.0.as_str(), b.1))?;
            }
            Ok(n)
        })
    }

    /// Removes joins between the selected elements.
    pub fn unjoin_selected(&mut self, items: &[(&str, i64)]) -> Result<usize, Error> {
        let done = self.joined_pairs()?;
        let inside = |p: &(String, i64)| items.iter().any(|(k, i)| *k == p.0 && *i == p.1);
        let todo: Vec<_> = done
            .into_iter()
            .filter(|(a, b)| inside(a) && inside(b))
            .collect();
        if todo.is_empty() {
            return invalid("unjoin: selected elements are not joined");
        }
        let n = todo.len();
        self.compound("mass_unjoin", |m| {
            for (a, b) in &todo {
                m.unjoin_geometry((a.0.as_str(), a.1), (b.0.as_str(), b.1))?;
            }
            Ok(n)
        })
    }

    /// Changes the cut order in joined pairs of the selection.
    pub fn switch_joins(&mut self, items: &[(&str, i64)]) -> Result<usize, Error> {
        let done = self.joined_pairs()?;
        let inside = |p: &(String, i64)| items.iter().any(|(k, i)| *k == p.0 && *i == p.1);
        let todo: Vec<_> = done
            .into_iter()
            .filter(|(a, b)| inside(a) && inside(b))
            .collect();
        if todo.is_empty() {
            return invalid("switch join: selected elements are not joined");
        }
        let n = todo.len();
        self.compound("mass_join", |m| {
            for (a, b) in &todo {
                m.switch_join((a.0.as_str(), a.1), (b.0.as_str(), b.1))?;
            }
            Ok(n)
        })
    }

    fn joined_pairs(&self) -> Result<Vec<((String, i64), (String, i64))>, Error> {
        self.joins()
    }

    fn overlapping_pairs(
        &self,
        items: &[(&str, i64)],
    ) -> Result<Vec<((String, i64), (String, i64))>, Error> {
        let mut boxes = vec![];
        for (k, id) in items.iter().filter(|(k, _)| JOIN_KINDS.contains(k)) {
            let body = self.joined_body(k, *id)?;
            let mut lo = [f64::MAX; 3];
            let mut hi = [f64::MIN; 3];
            for p in body.polys.iter().flat_map(|p| p.pts.iter()) {
                for (i, v) in [p.x, p.y, p.z].into_iter().enumerate() {
                    lo[i] = lo[i].min(v);
                    hi[i] = hi[i].max(v);
                }
            }
            if lo[0] <= hi[0] {
                boxes.push((((*k).to_string(), *id), lo, hi));
            }
        }
        let mut out = vec![];
        for i in 0..boxes.len() {
            for j in i + 1..boxes.len() {
                let (a, b) = (&boxes[i], &boxes[j]);
                if (0..3).all(|k| a.1[k] <= b.2[k] + 1e-3 && b.1[k] <= a.2[k] + 1e-3) {
                    out.push((a.0.clone(), b.0.clone()));
                }
            }
        }
        Ok(out)
    }
}

fn pair_in(done: &[((String, i64), (String, i64))], a: &(String, i64), b: &(String, i64)) -> bool {
    done.iter()
        .any(|(x, y)| (x == a && y == b) || (x == b && y == a))
}

/// Category "type and parameters" columns: geometry and placement are not carried over.
fn match_columns(t: &str) -> Option<&'static [&'static str]> {
    Some(match t {
        "wall" => &["wtype", "base_offset", "height", "offset0"],
        "slab" => &["stype", "offset"],
        "roof" => &["stype", "offset", "overhang", "fascia"],
        "ceiling" => &["stype", "height"],
        "col" => &[
            "mtype",
            "structural",
            "base_offset",
            "top_offset",
            "top_level",
        ],
        "beam" => &[
            "mtype",
            "yjust",
            "zjust",
            "rotation",
            "join_start",
            "join_end",
        ],
        "opening" => &["width", "sill", "height", "kind"],
        "comp" => &["ctype", "vals"],
        "room" => &["height"],
        "mep_run" => &["system", "shape", "width", "height", "diameter"],
        "stair" => &["width", "riser", "tread"],
        "railing" => &["height", "offset", "material"],
        "solid" => &["material"],
        "iso_footing" => &["ex", "ey", "round", "steps", "top_offset", "material"],
        "strip_footing" => &["width", "depth", "material"],
        "raft" => &["thickness", "material"],
        "brace" => &["mtype", "rotation"],
        "truss" => &["height", "heel", "panels", "chord_type", "web_type"],
        "beam_system" => &["mtype", "angle", "spacing", "rule", "end_gap", "zjust"],
        _ => return None,
    })
}

/// An opening must fit in the wall; a component must stay in a family with the same host kind.
fn match_fits(
    db: &Connection,
    t: &str,
    id: i64,
    cols: &[&str],
    vals: &[Value],
) -> Result<bool, Error> {
    let at = |c: &str| cols.iter().position(|x| *x == c);
    match t {
        "opening" => {
            let (along, ax, ay, bx, by): (f64, f64, f64, f64, f64) = db.query_row(
                "SELECT o.along,w.ax,w.ay,w.bx,w.by FROM opening o JOIN wall w ON w.id=o.wall WHERE o.id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let width = at("width").and_then(|i| num(&vals[i])).unwrap_or(0.0);
            Ok(along + width <= (bx - ax).hypot(by - ay) + 1e-9)
        }
        "comp" => {
            let Some(c) = at("ctype").and_then(|i| int(&vals[i])) else {
                return Ok(false);
            };
            let n: i64 = db.query_row(
                "SELECT count(*) FROM comp x JOIN comp_type a ON a.id=x.ctype JOIN comp_family f ON f.id=a.family, \
                 comp_type b JOIN comp_family g ON g.id=b.family \
                 WHERE x.id=?1 AND b.id=?2 AND f.host=g.host",
                [id, c],
                |r| r.get(0),
            )?;
            Ok(n > 0)
        }
        _ => Ok(true),
    }
}
