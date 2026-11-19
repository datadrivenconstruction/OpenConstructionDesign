// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The element contract (`docs/design/ELEMENT_CONTRACT.md`): one row per family, plain data and
//! fn pointers. C0 lists today's families as legacy rows and changes no behaviour; nothing reads
//! this table outside its tests until a family migrates, except the XLSX sheets (`sheets`).

use crate::edit3d::pick::Cat;
use crate::editor::bodies::BodyKind;
use crate::editor::{structx, Editor, OpFn, Sel};
use crate::scene_ids::{
    EXT_ANALYTICAL, EXT_CORE_BASE, EXT_MASS, EXT_REBAR, EXT_SOLID, EXT_STRUCT_BASE, EXT_TOPO,
};
use crate::{grips, schedule};
use ddc_model::{Error, Model, Target};
use std::sync::Arc;

pub(crate) mod stairs;

pub mod fitting;

#[cfg(test)]
mod budget;
#[cfg(test)]
mod datum_tests;
#[cfg(test)]
mod delete_tests;
#[cfg(test)]
mod delete_probe_tests;
#[cfg(test)]
pub(crate) mod matrix;
#[cfg(test)]
mod lifecycle;
pub mod sheets;

pub mod curtain;
pub mod mep;
pub mod rebar;
pub mod datum;
pub mod roof;
pub mod history;
pub mod joints_ui;
pub mod room_fam;
pub mod struct_grips;
pub mod struct_rows;
pub mod topo_fam;

pub mod panel_rule;

pub mod caps;

pub mod multi;

pub mod zone_shaft;

pub type Rings = Vec<Vec<(f64, f64)>>;

/// `No` holds the i18n key the user sees as the reason.
#[derive(Clone, Copy, Debug)]
pub enum Cap<T> {
    Yes(T),
    No(&'static str),
}

/// How a plan click reaches the family: a fill must not swallow clicks in the building.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Solid,
    EdgeOnly,
    Last,
}

#[derive(Clone, Copy, Debug)]
pub struct XfCaps {
    pub mv: Cap<()>,
    pub copy: Cap<()>,
    pub rotate: Cap<()>,
    pub mirror: Cap<()>,
    pub array: Cap<()>,
    pub delete: Cap<()>,
}

#[derive(Clone, Copy)]
pub struct Family {
    /// Key shared with `geom_props::Row.kind`, the clip spec and `delete_one`.
    pub table: &'static str,
    /// Set on migration. None also for legacy rows already on `Sel::Ext`: see [`Family::sel`].
    pub code: Option<u16>,
    /// None: no pick target yet (mass and topography number their own scene ids; annotation is
    /// plan only).
    pub cat3d: Option<Cat>,
    pub pick: Pick,
    pub footprints: fn(&Model, i64) -> Result<Vec<(i64, Rings)>, Error>,
    pub grips: Cap<grips::Kind>,
    pub target: Cap<fn(i64) -> Target>,
    pub xf: XfCaps,
    /// Non-numeric panel rows only.
    pub panel_extra: Option<fn(&mut egui::Ui, &mut Editor, i64)>,
    pub schedule: Cap<schedule::Cat>,
    /// XLSX sheet name.
    pub sheet: Cap<&'static str>,
    /// Snap kinds the family offers and whether its hits can be pinned.
    pub snap: crate::snap::SnapProfile,
}

/// A gap on a legacy row, never a design decision: the matrix treats it as a failing probe, so it
/// must sit in `GAPS`. Design refusals use their own key.
pub const NOT_YET: &str = "elem.no.not_yet";
pub const HOSTED: &str = "elem.no.hosted";
/// A zone has no geometry of its own: it follows the rooms it holds.
pub const ROOMS: &str = "elem.no.rooms";

const XF_ALL: XfCaps = XfCaps {
    mv: Cap::Yes(()),
    copy: Cap::Yes(()),
    rotate: Cap::Yes(()),
    mirror: Cap::Yes(()),
    array: Cap::Yes(()),
    delete: Cap::Yes(()),
};
const XF_NO_TARGET: XfCaps = XfCaps {
    mv: Cap::No(NOT_YET),
    copy: Cap::No(NOT_YET),
    rotate: Cap::No(NOT_YET),
    mirror: Cap::No(NOT_YET),
    array: Cap::No(NOT_YET),
    delete: Cap::Yes(()),
};
/// The form history has no rotated step: a mass moves, copies and arrays, and says why it cannot turn.
const MASS_TURN: &str = "elem.no.mass_turn";
const XF_MASS: XfCaps = XfCaps {
    rotate: Cap::No(MASS_TURN),
    mirror: Cap::No(MASS_TURN),
    ..XF_ALL
};
/// A hosted element moves and copies along its host only.
const XF_HOSTED: XfCaps = XfCaps {
    rotate: Cap::No(HOSTED),
    mirror: Cap::No(HOSTED),
    array: Cap::No(HOSTED),
    ..XF_ALL
};

/// Rebar follows its host and has no handles: every transform and grip refuses with the reason.
pub const REBAR_HOST: &str = "elem.no.rebar_host";
/// The analytical axis follows its host member.
pub const AXIS_HOST: &str = "elem.no.axis_host";
/// Annotations are view graphics, not counted elements: a design refusal, not a gap.
pub const ANNO_SCHEDULE: &str = "elem.no.anno_schedule";

const fn hosted_only(why: &'static str) -> XfCaps {
    XfCaps {
        mv: Cap::No(why),
        copy: Cap::No(why),
        rotate: Cap::No(why),
        mirror: Cap::No(why),
        array: Cap::No(why),
        delete: Cap::Yes(()),
    }
}

const fn target(f: fn(i64) -> Target) -> Cap<fn(i64) -> Target> {
    Cap::Yes(f)
}

/// Legacy families keep their outlines in `selection::outlines` until they migrate.
fn no_footprints(_: &Model, _: i64) -> Result<Vec<(i64, Rings)>, Error> {
    Ok(vec![])
}

fn footprints_of(m: &Model, level: i64, kind: &str) -> Result<Vec<(i64, Rings)>, Error> {
    let own = m.struct_footprints(level)?;
    let core = crate::tools::core_footprints(m, level);
    Ok(own
        .into_iter()
        .chain(core)
        .filter(|x| x.0 == kind)
        .map(|x| (x.1, x.2))
        .collect())
}

macro_rules! fp {
    ($kind:literal) => {
        |m: &Model, level: i64| footprints_of(m, level, $kind)
    };
}

const LEGACY: Family = Family {
    table: "",
    code: None,
    cat3d: None,
    pick: Pick::Solid,
    footprints: no_footprints,
    grips: Cap::No(NOT_YET),
    target: Cap::No(NOT_YET),
    xf: XF_NO_TARGET,
    panel_extra: None,
    schedule: Cap::No(NOT_YET),
    sheet: Cap::No(NOT_YET),
    snap: crate::snap::SnapProfile::ALL,
};

const fn ext(i: u16) -> Option<Cat> {
    Some(Cat::Ext(EXT_STRUCT_BASE + i))
}
const fn core_cat(i: u16) -> Option<Cat> {
    Some(Cat::Ext(EXT_CORE_BASE + i))
}

/// Order follows the migration list of the contract.
pub const FAMILIES: &[Family] = &[
    Family {
        table: "roof",
        sheet: Cap::Yes("Roofs"),
        snap: crate::snap::SnapProfile::ALL,
        code: Some(structx::ROOF_CODE),
        cat3d: Some(Cat::Roof),
        pick: Pick::EdgeOnly,
        footprints: fp!("roof"),
        grips: Cap::Yes(grips::Kind::Roof),
        target: target(Target::Roof),
        xf: XF_ALL,
        panel_extra: Some(roof::panel),
        schedule: Cap::Yes(schedule::Cat::Roofs),
        ..LEGACY
    },
    Family {
        table: "stair",
        code: Some(structx::STAIR_CODE),
        sheet: Cap::Yes("Stairs"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Stair),
        footprints: fp!("stair"),
        grips: Cap::Yes(grips::Kind::Stair),
        target: target(Target::Stair),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Stairs),
        ..LEGACY
    },
    Family {
        table: "railing",
        code: Some(structx::RAILING_CODE),
        sheet: Cap::Yes("Railings"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Railing),
        pick: Pick::EdgeOnly,
        footprints: fp!("railing"),
        grips: Cap::Yes(grips::Kind::Railing),
        // a railing on a stair moves with the stair and refuses to go alone
        target: target(Target::Railing),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Railings),
        ..LEGACY
    },
    Family {
        table: "shaft",
        // position of "shaft" in structx::CORE_KINDS
        code: Some(structx::CORE_BASE + 6),
        sheet: Cap::Yes("Shafts"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: core_cat(6),
        footprints: fp!("shaft"),
        grips: Cap::Yes(grips::Kind::Shaft),
        target: target(Target::Shaft),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Shafts),
        ..LEGACY
    },
    // keeps the zone panel of the structural properties: rooms and name edit there
    Family {
        table: "zone",
        sheet: Cap::Yes("Zones"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: core_cat(2),
        pick: Pick::Last,
        footprints: fp!("zone"),
        grips: Cap::Yes(grips::Kind::Zone),
        target: target(Target::Zone),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Zones),
        ..LEGACY
    },
    Family {
        table: "grid",
        code: Some(structx::GRID_CODE),
        sheet: Cap::Yes("Grids"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Ext(structx::GRID_CODE)),
        pick: Pick::EdgeOnly,
        footprints: fp!("grid"),
        grips: Cap::Yes(grips::Kind::Grid),
        target: target(Target::Grid),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Grids),
        ..LEGACY
    },
    Family {
        table: "ref_plane",
        code: Some(structx::REF_PLANE_CODE),
        sheet: Cap::Yes("Reference planes"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Ext(structx::REF_PLANE_CODE)),
        pick: Pick::EdgeOnly,
        footprints: fp!("ref_plane"),
        grips: Cap::Yes(grips::Kind::RefPlane),
        target: target(Target::RefPlane),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::RefPlanes),
        ..LEGACY
    },
    Family {
        table: "col",
        sheet: Cap::Yes("Columns"),
        snap: crate::snap::SnapProfile::COLUMN,
        cat3d: Some(Cat::Column),
        grips: Cap::Yes(grips::Kind::Column),
        target: target(Target::Column),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Columns),
        ..LEGACY
    },
    Family {
        table: "beam",
        sheet: Cap::Yes("Beams"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Beam),
        grips: Cap::Yes(grips::Kind::Beam),
        target: target(Target::Beam),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Beams),
        ..LEGACY
    },
    Family {
        table: "ceiling",
        sheet: Cap::Yes("Ceilings"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Ceiling),
        grips: Cap::Yes(grips::Kind::Ceiling),
        target: target(Target::Ceiling),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Ceilings),
        ..LEGACY
    },
    Family {
        table: "mesh",
        sheet: Cap::Yes("Meshes"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Mesh),
        grips: Cap::Yes(grips::Kind::Mesh),
        target: target(Target::Mesh),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Meshes),
        ..LEGACY
    },
    Family {
        table: "opening",
        cat3d: Some(Cat::Opening),
        target: target(Target::Opening),
        xf: XF_HOSTED,
        schedule: Cap::Yes(schedule::Cat::Windows),
        sheet: Cap::Yes("Openings"),
        snap: crate::snap::SnapProfile::ALL,
        ..LEGACY
    },
    Family {
        table: "slab",
        cat3d: Some(Cat::Slab),
        grips: Cap::Yes(grips::Kind::Slab),
        target: target(Target::Slab),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Slabs),
        sheet: Cap::Yes("Slabs"),
        snap: crate::snap::SnapProfile::ALL,
        ..LEGACY
    },
    Family {
        table: "wall",
        cat3d: Some(Cat::Wall),
        grips: Cap::Yes(grips::Kind::Wall),
        target: target(Target::Wall),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Walls),
        sheet: Cap::Yes("Walls"),
        snap: crate::snap::SnapProfile::ALL,
        ..LEGACY
    },
    Family {
        table: "curtain_wall",
        cat3d: Some(Cat::Curtain),
        grips: Cap::Yes(grips::Kind::Curtain),
        target: target(Target::Curtain),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Curtains),
        sheet: Cap::Yes("Curtain walls"),
        snap: crate::snap::SnapProfile::ALL,
        ..LEGACY
    },
    Family {
        table: "mep_run",
        sheet: Cap::Yes("MEP runs"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Mep),
        grips: Cap::Yes(grips::Kind::Mep),
        target: target(Target::MepRun),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::MepRuns),
        ..LEGACY
    },
    Family {
        table: "comp",
        sheet: Cap::Yes("Components"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Comp),
        grips: Cap::Yes(grips::Kind::Comp),
        target: target(Target::Component),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Components),
        panel_extra: Some(joints_ui::panel),
        ..LEGACY
    },
    Family {
        table: "iso_footing",
        sheet: Cap::Yes("Isolated footings"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: ext(0),
        footprints: fp!("footing"),
        grips: Cap::Yes(grips::Kind::Footing),
        target: target(Target::Footing),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Footings),
        ..LEGACY
    },
    Family {
        table: "strip_footing",
        sheet: Cap::Yes("Strip footings"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: ext(1),
        footprints: fp!("strip_footing"),
        grips: Cap::Yes(grips::Kind::StripFooting),
        target: target(Target::StripFooting),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::StripFootings),
        ..LEGACY
    },
    Family {
        table: "raft",
        sheet: Cap::Yes("Rafts"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: ext(2),
        footprints: fp!("raft"),
        grips: Cap::Yes(grips::Kind::Raft),
        target: target(Target::Raft),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Rafts),
        ..LEGACY
    },
    Family {
        table: "brace",
        sheet: Cap::Yes("Braces"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: ext(3),
        footprints: fp!("brace"),
        grips: Cap::Yes(grips::Kind::Brace),
        target: target(Target::Brace),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Braces),
        ..LEGACY
    },
    Family {
        table: "truss",
        sheet: Cap::Yes("Trusses"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: ext(4),
        footprints: fp!("truss"),
        grips: Cap::Yes(grips::Kind::Truss),
        target: target(Target::Truss),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Trusses),
        ..LEGACY
    },
    Family {
        table: "beam_system",
        sheet: Cap::Yes("Beam systems"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: ext(5),
        footprints: fp!("beam_system"),
        grips: Cap::Yes(grips::Kind::BeamSystem),
        target: target(Target::BeamSystem),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::BeamSystems),
        ..LEGACY
    },
    Family {
        table: "ramp",
        sheet: Cap::Yes("Ramps"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: core_cat(0),
        footprints: fp!("ramp"),
        grips: Cap::Yes(grips::Kind::Ramp),
        target: target(Target::Ramp),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Ramps),
        ..LEGACY
    },
    Family {
        table: "rebar_set",
        sheet: Cap::Yes("Rebar sets"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Ext(EXT_REBAR)),
        pick: Pick::EdgeOnly,
        footprints: rebar::set_footprints,
        grips: Cap::No(REBAR_HOST),
        xf: hosted_only(REBAR_HOST),
        panel_extra: Some(rebar::panel_set),
        schedule: Cap::Yes(schedule::Cat::Rebar),
        ..LEGACY
    },
    Family {
        table: "analytical_adj",
        sheet: Cap::Yes("Analytical adjustments"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Ext(EXT_ANALYTICAL)),
        pick: Pick::EdgeOnly,
        footprints: rebar::member_footprints,
        grips: Cap::No(AXIS_HOST),
        xf: hosted_only(AXIS_HOST),
        panel_extra: Some(rebar::panel_axis),
        ..LEGACY
    },
    Family {
        table: "solid",
        sheet: Cap::Yes("Solids"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Ext(EXT_SOLID)),
        grips: Cap::Yes(grips::Kind::Solid),
        target: target(Target::Solid),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Solids),
        ..LEGACY
    },
    Family {
        table: "mass",
        sheet: Cap::Yes("Masses"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: Some(Cat::Ext(EXT_MASS)),
        grips: Cap::Yes(grips::Kind::Mass),
        target: target(Target::Mass),
        xf: XF_MASS,
        schedule: Cap::Yes(schedule::Cat::Masses),
        panel_extra: Some(history::panel),
        ..LEGACY
    },
    Family {
        table: "toposolid",
        code: Some(EXT_TOPO),
        sheet: Cap::Yes("Topography"),
        snap: crate::snap::SnapProfile::ALL,
        pick: Pick::Last,
        footprints: topo_fam::footprints,
        panel_extra: Some(topo_fam::panel),
        cat3d: Some(Cat::Ext(EXT_TOPO)),
        grips: Cap::Yes(grips::Kind::Topo),
        target: target(Target::Topo),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Topography),
        ..LEGACY
    },
    Family {
        table: "anno",
        sheet: Cap::Yes("Annotations"),
        snap: crate::snap::SnapProfile::ALL,
        pick: Pick::EdgeOnly,
        target: target(Target::Anno),
        xf: XF_ALL,
        schedule: Cap::No(ANNO_SCHEDULE),
        ..LEGACY
    },
    Family {
        table: "room",
        cat3d: Some(Cat::Room),
        pick: Pick::Last,
        grips: Cap::Yes(grips::Kind::Room),
        target: target(Target::Room),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::Rooms),
        sheet: Cap::Yes("Rooms"),
        snap: crate::snap::SnapProfile::ALL,
        ..LEGACY
    },
    Family {
        table: "room_sep",
        code: Some(structx::CORE_BASE + 1),
        sheet: Cap::Yes("Room separators"),
        snap: crate::snap::SnapProfile::ALL,
        cat3d: core_cat(1),
        pick: Pick::EdgeOnly,
        footprints: fp!("room_sep"),
        grips: Cap::Yes(grips::Kind::RoomSep),
        target: target(Target::RoomSep),
        xf: XF_ALL,
        schedule: Cap::Yes(schedule::Cat::RoomSeps),
        ..LEGACY
    },
];

/// Resolved once per gesture, never per element or per frame.
pub fn by_table(table: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|f| f.table == table)
}

/// Table behind a `Sel::Ext` or `Cat::Ext` code.
fn ext_table(code: u16) -> Option<&'static str> {
    Some(match code {
        EXT_SOLID => "solid",
        EXT_MASS => "mass",
        EXT_TOPO => "toposolid",
        EXT_REBAR => "rebar_set",
        EXT_ANALYTICAL => "analytical_adj",
        _ => match structx::kind_of(code)? {
            "footing" => "iso_footing",
            kind => kind,
        },
    })
}

/// No wildcard on purpose: a new `Sel` variant does not compile until it names a family.
pub fn of_sel(s: Sel) -> Option<&'static Family> {
    by_table(match s {
        Sel::Wall(_) => "wall",
        Sel::Opening(_) => "opening",
        Sel::Curtain(_) | Sel::Panel(..) => "curtain_wall",
        Sel::Slab(_) => "slab",
        Sel::Room(_) => "room",
        Sel::Mass(_) => "mass",
        Sel::Mep(_) => "mep_run",
        Sel::Comp(_) => "comp",
        Sel::Anno(_) => "anno",
        Sel::Ext(c, _) => ext_table(c)?,
        Sel::Body(k, _) => k.table(),
    })
}

/// No wildcard on purpose, as in [`of_sel`].
pub fn of_cat(c: Cat) -> Option<&'static Family> {
    by_table(match c {
        Cat::Wall => "wall",
        Cat::Opening => "opening",
        Cat::Curtain | Cat::Panel => "curtain_wall",
        Cat::Slab => "slab",
        Cat::Roof => "roof",
        Cat::Ceiling => "ceiling",
        Cat::Column => "col",
        Cat::Beam => "beam",
        Cat::Stair => "stair",
        Cat::Railing => "railing",
        Cat::Comp => "comp",
        Cat::Room => "room",
        Cat::Mep => "mep_run",
        Cat::Mesh => "mesh",
        Cat::Ext(code) => ext_table(code)?,
    })
}

impl Family {
    /// The editor selection of an element; None while the family has no way to be selected.
    pub fn sel(&self, id: i64) -> Option<Sel> {
        if let Some(c) = self.code {
            return Some(Sel::Ext(c, id));
        }
        Some(match self.table {
            "wall" => Sel::Wall(id),
            "opening" => Sel::Opening(id),
            "curtain_wall" => Sel::Curtain(id),
            "slab" => Sel::Slab(id),
            "room" => Sel::Room(id),
            "mass" => Sel::Mass(id),
            "mep_run" => Sel::Mep(id),
            "comp" => Sel::Comp(id),
            "anno" => Sel::Anno(id),
            "mesh" | "col" | "beam" | "ceiling" => Sel::Body(BodyKind::from_table(self.table)?, id),
            "solid" => Sel::Ext(EXT_SOLID, id),
            "rebar_set" => Sel::Ext(EXT_REBAR, id),
            "analytical_adj" => Sel::Ext(EXT_ANALYTICAL, id),
            "iso_footing" => return structx::sel("footing", id),
            t => return structx::sel(t, id),
        })
    }
}

/// Rebar sets and analytical members: hosted families that take the element paths of this module
/// (panel, delete, selection) without a code in their row yet.
pub fn is_hosted_ext(code: u16) -> bool {
    matches!(code, EXT_REBAR | EXT_ANALYTICAL)
}

/// The migrated family of a selection item: its row names the code the item carries.
fn migrated(s: Sel) -> Option<(&'static Family, i64)> {
    match s {
        Sel::Ext(c, id) => of_sel(s)
            .filter(|f| f.code == Some(c) || is_hosted_ext(c))
            .map(|f| (f, id)),
        // a mass keeps its own selection variant but shares the panel and delete route
        Sel::Mass(id) => by_table("mass").map(|f| (f, id)),
        _ => None,
    }
}

/// Why a move, copy, rotate, mirror or array of these selection items is refused, when some of
/// them are hosted and nothing else can take the action.
pub fn hosted_refusal(targets: &[Sel]) -> Option<String> {
    targets.iter().find_map(|s| match (s, of_sel(*s)) {
        (Sel::Ext(c, _), Some(f)) if is_hosted_ext(*c) => match f.xf.mv {
            Cap::No(key) => Some(crate::tr(key, &[])),
            Cap::Yes(()) => None,
        },
        _ => None,
    })
}

/// Plan grips of a selected item of a migrated family.
pub fn grip_target(s: Sel) -> Option<(grips::Kind, i64)> {
    // a legacy structural code keeps its own panel and delete, but gets the family's grips
    let (f, id) = match (migrated(s), s) {
        (Some(x), _) => x,
        (None, Sel::Ext(_, id)) => (of_sel(s)?, id),
        _ => return None,
    };
    match f.grips {
        Cap::Yes(k) => Some((k, id)),
        Cap::No(_) => None,
    }
}

/// Properties panel of the first selected item of a migrated family: one header, the rows of
/// `geom_props`, then the family's own extra part. False: nothing of the kind is selected.
pub fn show(ui: &mut egui::Ui, ed: &mut Editor) -> bool {
    let Some((f, id)) = ed.selection.iter().find_map(|s| migrated(*s)) else {
        return false;
    };
    let one = crate::tr(&format!("elem.{}.one", f.table), &[]);
    let host = rebar::axis_host(f, id).map(|(kind, host)| {
        format!("{} #{host}", crate::tr(&format!("rebar.host.{kind}"), &[]))
    });
    crate::ui_kit::element_header(ui, f.table, &one, host.as_deref().unwrap_or(""), id);
    crate::geom_props::show(ui, ed, f.table, id);
    if let Some(extra) = f.panel_extra {
        extra(ui, ed, id);
    }
    true
}

/// Delete for a selection that holds a migrated family: every kind goes in one compound, so one
/// undo restores all of it. Sub-elements and the kinds with their own delete flow refuse with a
/// reason and change nothing. False: the selection holds no migrated item, the old path runs.
pub fn delete_selection(ed: &mut Editor) -> bool {
    delete_checked(ed, false)
}

/// The answer "delete all" to the confirm dialog: the same delete, without asking again.
pub(crate) fn delete_confirmed(ed: &mut Editor) -> bool {
    delete_checked(ed, true)
}

fn delete_checked(ed: &mut Editor, confirmed: bool) -> bool {
    // a room has no code of its own, yet it deletes through the same compound as its tags
    let routed = |s: &Sel| {
        migrated(*s).is_some()
            || matches!(s, Sel::Room(_) | Sel::Mass(_))
            || matches!(s, Sel::Ext(c, _) if *c == EXT_SOLID)
    };
    if !ed.selection.iter().any(routed) {
        return false;
    }
    // panels and annotations are never deleted here: the rest goes, these are reported
    let (left, rest): (Vec<Sel>, Vec<Sel>) = ed
        .selection
        .iter()
        .copied()
        .partition(|s| matches!(s, Sel::Panel(..) | Sel::Anno(_)));
    if rest.is_empty() {
        panel_rule::offer(&left);
        let all_panels = left.iter().all(|s| matches!(s, Sel::Panel(..)));
        let msg = if all_panels {
            crate::tr("elem.panel_refused", &[])
        } else {
            let why = crate::tr("elem.no.mixed", &[]);
            crate::tr("elem.not_deleted_what", &[("what", why.as_str())])
        };
        ed.say(crate::status::Severity::Refusal, msg);
        return true;
    }
    let items = match rebar::delete_items(ed, &rest) {
        Ok(v) => v,
        Err(why) => {
            ed.status = Some(crate::tr("elem.not_deleted_what", &[("what", why.as_str())]));
            return true;
        }
    };
    let deps = crate::editor::delete_deps::collect_exact(&ed.model, &items, ed.level);
    if !confirmed && deps.needs_confirm() {
        ed.pending_delete = Some(crate::editor::delete_deps::Pending {
            extra: crate::editor::delete::Extra {
                items: items.clone(),
                ..Default::default()
            },
            deps,
            sel: ed.selection.clone(),
            revision: ed.revision,
            via_elem: true,
        });
        return true;
    }
    let left_names = panel_rule::kinds(ed, &left);
    let what = match ed.selection.as_slice() {
        [one] => of_sel(*one).map_or_else(
            || items.len().to_string(),
            |f| crate::tr(&format!("elem.{}.one", f.table), &[]),
        ),
        _ => items.len().to_string(),
    };
    // railings of a deleted stair go first so a stair is never blocked by its own railing
    let (list, _) = stairs::delete_list(&ed.model, items);
    let op = OpFn(Arc::new(move |m: &mut Model| {
        let refs: Vec<(&str, i64)> = list.iter().map(|x| (x.0.as_str(), x.1)).collect();
        m.delete_items("elem_delete", &refs)
    }));
    let (sev, text) = match ed.run_op(op) {
        Ok(()) => {
            ed.selection = left.clone();
            panel_rule::offer(&left);
            let mut t = crate::tr("elem.deleted_what", &[("what", what.as_str())]);
            if !left.is_empty() {
                let k = crate::tr("elem.left_in_place", &[("kinds", left_names.as_str())]);
                t = format!("{t}. {k}");
            }
            t.push_str(&deps.also());
            let warn = deps.other_level > 0 || deps.rooms > 0;
            let sev = if warn {
                crate::status::Severity::Warning
            } else {
                crate::status::Severity::Info
            };
            (sev, t)
        }
        Err(e) => (
            crate::status::Severity::Error,
            crate::tr("elem.not_deleted_what", &[("what", e.to_string().as_str())]),
        ),
    };
    ed.say(sev, text);
    true
}
