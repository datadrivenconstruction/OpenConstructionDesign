// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The model in SQLite. Module map:
//! schema — DDL, format version, migrations · integrity — reference checks, reference-row deletion · ops — operation journal and undo · elements — elements/parameters ·
//! walls — wall types and instances, volumes via ddc-geom · slabs — floors (sloped top) ·
//! curtain — curtain walls (grid, panels, mullions) · rooms — rooms ·
//! drawing — 2D primitives, blocks · views — level plan at the cut height ·
//! sections — sections and elevations (view_def) · annot — dimension objects, auto-dimensioning ·
//! annot_text — view annotations (text, details, tags, clouds, legends) ·
//! structure — columns, beams, joints, deductions · roofs — roofs, wall attachment · struct_views — their views.
//! modify — move/copy/rotate/mirror/array/offset/trim/split/align · datum — grids, reference planes ·
//! datum_view — per-view extents and tags of grids/levels (2D/3D) ·
//! shafts — shafts, ceilings, floor openings · stairs — stairs, railings ·
//! components — parametric components (families, types, instances, library).
//! mass — in-place forms and solids (booleans, faces -> elements, floors).
//! wall_edit — joints, compound walls, profiles, vertical structure, curtain wall edits.
//! sketch — contour sketches, floor shape, elevation profile and wall attachment.
//! cuts — sketch-based openings in hosts, cut-outs (shafts + openings), wall top/bottom from attachments.
//! mep — engineering networks (systems, runs, graph and fittings, quantities, clashes) · mep3d — their per-part solids and sections.
//! changes — journal of changed rows for partial view rebuilds.
//! constraints — edit constraints for walls, grids and levels (lock, EQ, align, parallel, perpendicular) via ddc-solver.
//! sheets — sheets, revisions, viewports, on-sheet schedules, sheet sets.
//! phases — phases (created/demolished, view phase and filter, demolition) · params — project parameters.
//! fastload — pragmas and expression cache for bulk loading · meshcache — imported mesh instances and LODs.
//! source — import sources: base, 3-way re-import, conflicts, local edits · links — linked models.
//! merge — merge with a colleague's file · compare — version comparison · options — project options.
//! ai_plan — AI plan dry run: copies, comparison with base, rollback · ai_audit — AI journal and settings · checks — checks engine.

use rusqlite::Connection;

/// Counts the SQL statements a thread runs on any model connection (feature `test-hooks`).
#[cfg(feature = "test-hooks")]
pub mod query_count {
    use std::cell::{Cell, RefCell};

    thread_local! {
        static N: Cell<u64> = const { Cell::new(0) };
        static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn on_trace(e: rusqlite::trace::TraceEvent<'_>) {
        if let rusqlite::trace::TraceEvent::Stmt(_, sql) = e {
            hit(sql);
        }
    }

    fn hit(sql: &str) {
        N.with(|n| n.set(n.get() + 1));
        LOG.with(|l| {
            let mut l = l.borrow_mut();
            if l.len() < 64 {
                l.push(sql.chars().take(160).collect());
            }
        });
    }

    /// Statements run by this thread so far.
    pub fn count() -> u64 {
        N.with(Cell::get)
    }

    /// The first statements run since the last call (at most 64, 160 chars each); clears the log.
    pub fn take_log() -> Vec<String> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }
}

mod ai_audit;
mod ai_revert;
/// Tool refusal by path: root `Refusal` is the AI revert one, `Fix` the checks one.
pub mod refusal;
mod ai_option;
mod ai_plan;
mod analytical;
mod analytical_keys;
mod annot;
mod measure;
#[cfg(test)]
mod test_ru;
#[cfg(test)]
mod measure_tests;
#[cfg(test)]
mod ddcf_library_tests;
mod annot_text;
mod boundary_link;
mod bulk;
mod calc_store;
mod changes;
pub mod checks;
mod clip;
mod compare;
mod reveal_order;
mod compare_keys;
mod comp_export;
mod components;
pub mod content_pack;
mod constraints;
mod curtain;
mod cuts;
mod datum;
mod datum_view;
pub mod drawq;
mod datum_xf;
mod drawing;
mod elements;
mod family_define;
mod family_edit;
mod fastload;
mod file;
mod find_text;
mod finite;
mod footings;
mod framing;
mod geo_req;
mod geo_convert;
mod groups;
mod history;
mod journal;
mod imported;
mod keys;
mod instance_set;
mod integrity;
pub mod links;
mod mass;
pub mod mat_mech;
mod material_texture;
pub mod material_look;
#[cfg(test)]
mod material_look_tests;
mod field_err;
mod materials;
mod mep;
mod mep3d;
mod mep_fit;
pub mod merge;
mod mesh_edit;
mod meshcache;
mod meshes;
mod modify;
mod names;
mod op_revert;
mod ops;
mod opening_comp;
mod ops_apply;
mod opt_metrics;
mod options;
mod params;
pub mod num_text;
mod outline_spikes;
pub mod perf;
mod phases;
mod phase_dates;
mod journal_walk;
pub mod xlsx_rec;
mod project;
mod project_info;
mod project_logo;
mod purge;
mod ramps;
mod rebar;
mod rebar_section;
pub mod reason;
mod regen;
mod layer_core;
mod roof_auto;
pub use roof_auto::sync_roof_walls;
mod roof_rules;
pub use roof_rules::{rule_info as roof_rule_info, RoofRule, RULES as ROOF_RULES};
#[cfg(test)]
mod roof_auto_tests;
mod roof_edge;
mod roof_xf;
mod roofs;
mod rooms;
mod saved_filters;
mod schedule_defs;
mod schema;
mod sections;
mod shafts;
mod sheets;
mod sketch;
mod slabs;
mod snapshot;
mod sql_guard;
mod stage;
mod stage_err;
mod solids;
pub mod freeform;
pub mod panel_system;
pub mod shell_elem;
pub mod source;
mod stairs;
mod stats;
mod steel_conn;
mod steel_frame;
mod gridshell;
mod struct_ext;
mod struct_xf;
mod struct_views;
mod structure;
mod topo;
mod types;
mod underlay_store;
mod view_gfx;
mod views;
mod views_axes;
mod views_inner;
mod area_std;
mod finishes;
mod wall_edit;
mod zones;
pub use wall_edit::WallChange;
pub mod value_src;
pub mod depth;
pub use depth::{DepthSettings, Discipline, ElemDepth};
pub mod recipe;
pub use recipe::{Recipe, RecipeHeld, RecipePart};

/// Process id for unique names and ids; 0 in the browser, where `std::process::id` panics
/// (the clock and counters keep names apart there).
pub fn process_id() -> u32 {
    if cfg!(target_arch = "wasm32") {
        0
    } else {
        std::process::id()
    }
}

/// Temp folder; empty in the browser, where `std::env::temp_dir` panics (a file op there then
/// fails with an error instead of stopping the editor).
pub fn temp_dir() -> std::path::PathBuf {
    if cfg!(target_arch = "wasm32") {
        std::path::PathBuf::new()
    } else {
        std::env::temp_dir()
    }
}
/// Parametric feature timelines of masses (param1).
pub mod feature_tl;
/// Assembly joints between component instances (param1, schema 53).
pub mod joints;
pub use joints::{AsmDof, JointRow, JointSpec};
pub use value_src::{SrcKind, ValueSrc};
mod views3d;
mod walls;
mod wall_spline;
mod work_planes;
pub use work_planes::{NamedPlane, WORK_PLANES_KEY};
mod trial;
#[cfg(test)]
mod qa_core_tests;
#[cfg(test)]
mod qa_methods_tests;
#[cfg(test)]
mod qa_ufcs_tests;
#[cfg(test)]
mod qa_mass_tests;
#[cfg(test)]
mod mass_direct_tests;
#[cfg(test)]
mod qa_mep_tests;
#[cfg(test)]
mod qa_source_tests;
#[cfg(test)]
mod qa_misc_tests;
mod topic_clip;
mod topics;
pub mod wall_types;
pub mod wtime;
pub use wall_types::{
    read_type_pack, Align, Conflict, ImportFate, ImportReport, LayerFn, TypeLayer, TypeLibStep,
    TypePack, TypeUse,
};

pub use topic_clip::{clip_box, ClipBox, ClipPlane};
pub use topics::{
    now_secs, Orbit, Topic, TopicBcf, TopicComment, TopicDoc, TopicDocFile, TopicEdit, TopicElem,
    TopicImport, TopicRow, Viewpoint, DOC_MAX as TOPIC_DOC_MAX, PRIORITIES as TOPIC_PRIORITIES,
    SNAPSHOT_MAX, STATUSES as TOPIC_STATUSES,
};
pub use ai_audit::{set_audit_scrubber, AuditEntry, CHAT_ROWS};
pub use calc_store::{
    check_load, check_override, check_src, key_kind, AnOverride, CalcRun, Load, LoadCase,
    LoadCombo, MatMech, CALC_KEEP_UNPINNED, CALC_TABLES, COMBO_STATES, LOAD_CATEGORIES,
    LOAD_FRAMES, LOAD_KINDS, RUN_STATUSES,
};
pub use op_revert::RevertCheck;
pub use underlay_store::{saved_underlays, UnderlayRow};
pub use ai_revert::{AiBatch, MadeBy, Refusal, RevertError};
pub use refusal::RefusalCode;
pub use opt_metrics::{
    option_copy_metrics, Cost, CostLine, FindingCounts, MetricCell, MetricsRow, OptionMetrics, RoomTypeArea, Units,
    CURRENCY_KEY, UNITS, UNIT_COST_KEY,
};
pub use ai_option::{materialize_options, Lower, OptionCopy, OptionMode, OptionRec, OptionStatus, OptionStore, RecipeStore, Replay};
pub use stage::{clean_stale_stages, stage_copy, stage_path, StageMarks};
pub use names::free_name;
pub use ai_plan::{
    fnv1a, fnv1a_more, is_memory_copy, CellDiff, CopyStore, MaterialQty, MemCopies, ModelDiff,
    PlanCopies, RowDiff, RowKind, FNV1A_SEED, PLAN_IGNORED,
};
pub use analytical::{AnIssue, AnMember, AnPanel, AnSection, AnSupport, Analytical, AnalyticalAdj};
pub use analytical_keys::{member_key, node_anchor, panel_key, station, OverrideUse};
pub use measure::{
    Anchor as MeasureAnchor, MeasureInfo, MeasureKind, MeasureSpec, MeasureState, Pin as MeasurePin, Side as MeasureSide,
    Reason as MeasureReason, Value as MeasureValue,
};
pub use annot::{
    next_mark, tag_size, Annot, AnnotLoss, DimGeom, DimKind, DimRef, DimSpec, Tag, TagCat, TagHost,
    TagSpec, TagText,
};
pub use annot_text::{
    dash_segments, stroke_hit, strokes_bbox, Anno, AnnoCtx, AnnoShape, AnnoStroke, AnnoView,
    AnnoXf, DetailDef, DetailFit, DetailStroke, DraftingDef, Leader, LeaderHead, LegendDef,
    LegendPlace, LegendRow, LegendSwatch, LineStyle, RegionFill, SpotKind, SpotValue, TextAlign,
    TextNote, TextStyle,
};
pub use boundary_link::{
    rebuild as rebuild_linked_outline, BoundaryWarning, EdgeLink, LinkFace, LinkState,
    LinkedEdge,
};
pub use annot_text::AnnoBatchItem;
pub use bulk::{new_level_ref, Edit};
pub use find_text::{
    find_spans, replace_report, replaced_text, FindField, FindHit, FindOpts, HitKey, ReplaceReport,
};
pub use changes::{Changes, ContentMark, EditState};
pub use clip::{
    clip_tables, Clip, ClipHost, ClipInst, ClipRows, ClipText, PasteOpts, PasteReport, PlaceLevel, CLIP_TEXT_MAX,
};
pub use reveal_order::{
    build_order as reveal_build_order, shift_of, stage_of as reveal_stage_of, Change as RevealChange,
    Stage as RevealStage, Step as RevealStep,
};
pub use compare::{
    compare_file, prepare_compare, DiffKind, ElementDiff, FieldChange, FieldGroup, Spot,
    VersionDiff,
};
pub use family_define::{family_def_from_json, Defined};
pub use comp_export::{
    resolved_params, unit_of, value_text, CompParam, CompRecord, CLASSIFICATION_PREFIX,
};
pub use components::{
    family_into, library, library_sources, plant_level, type_into, Crowd, CompHost, CompInstance, CompQty, CompScene,
    CompSpec, Placement, ScheduleRow, LIBRARY,
};
pub use constraints::dist::{
    fit_count as dist_fit_count, layout as dist_layout, DistMode, DistRule, Distribution,
    Member as DistMember, Slot as DistSlot, SlotConflict,
};
pub use constraints::{ConstraintKind, ConstraintSpec, Ref as ConstraintRef};
pub use curtain::{
    check_curtain, grid as curtain_grid, parse_section, section_text, CurtainLayout, CurtainQty,
    CurtainSpec, GridLines, LineSections, PanelKind, PanelSpec,
};
pub use cuts::{HostOpening, OpeningHost};
pub use datum::{Grid, GridSpec, Snap};
pub use datum_view::{DatumExtent, DatumKind, LEVEL_DEFAULT, MODEL_VIEW};
pub use ddc_geom::analytical::{Release as AnRelease, Tolerances as AnTolerances};
pub use ddc_geom::component::{
    Category as CompCategory, Family, Hosting, Kind as ParamKind, Lod, Value as ParamValue,
};
pub use ddc_geom::csg::{Body as MassBody, BoolOp};
pub use ddc_geom::curtain::Section as MullionSection;
pub use ddc_geom::dim::Linear as DimLinear;
pub use ddc_geom::footing::Step as FootingStep;
pub use ddc_geom::gsolid::{
    Form as SolidForm, Frame as SolidFrame, Path3 as SolidPath, Section as SolidSection,
};
pub use ddc_geom::member::{Just, Shape as MemberShape};
pub use ddc_geom::mep::{
    Detail as MepDetail, Domain as MepDomain, FittingKind, PipeMaterial, PlanPrim as MepPrim,
    Section as MepSection, Shape as MepShape, SystemKind,
};
pub use ddc_geom::rebar::{
    Bar as RebarGeom, Hook as RebarHook, PathLayout as RebarPath, ShapeCode as RebarShape,
};
pub use ddc_geom::slope::Plane as SlabSlope;
pub use ddc_geom::stair::{
    RailingLayout, RailingQty, Shape as StairShape, Stair, StairPlan, StairQty,
};
pub use drawing::{Entity, Style, Vertex};
pub use family_edit::{CatalogLoad, DocSave, FamilyLoad, TypeOverwrite};
pub use fastload::{with_bulk, Cached};
pub use file::{
    archive_snapshot, check_snapshot, snapshot_infos, snapshots_of, SnapInfo, TableDiff,
    KEEP_SNAPSHOTS,
};
pub use footings::{FootingSpec, RaftSpec, StripAxis, StripFootingSpec};
pub use framing::{
    Anchor, BeamLayout, BeamSystemSpec, BraceSpec, MemberHost, TrussChord, TrussSpec, TrussWeb,
};
pub use geo_req::{
    GeoBuilt, GeoMeta, GeoPlan, GeoReq, LoftReq, LoftSection, PathReq, ProfileReq, SweepReq,
    SweepStyle,
};
pub use geo_convert::{ConvertRequest, ConvertTo, Loss, LossReport};
pub use groups::{GroupInfo, InstInfo};
pub use history::JournalEntry;
pub use journal::Provenance;
pub use ddc_ops;
pub use imported::{
    mesh_blobs, put_beam, put_ceiling, put_column, put_roof, ImportedMesh, PropWrite,
    WALL_COMPUTED_MARKER, WALL_IS_EXTERNAL_KEY,
};
pub use instance_set::{
    read_instance_set_file, read_proto_levels_file, InstLevel, InstProto, InstRow, InstanceSet,
};
pub use integrity::Issue;
pub use mass::{join_priority, FaceClass, Form, MassFace, MassHost, MassRow, MassSpec, PushGoal};
pub use mass::{BoolLink, BoolRequest, VoidRequest};
pub use materials::{
    is_lib_hatch, material_default_hatch, material_defaults, Material, MaterialSpec,
    BUILTIN_MATERIALS, LIB_HATCH_PREFIX, MATERIAL_HATCHES,
};
pub use material_texture::{
    is_image_bytes, MaterialTexture, MaterialTextureRow, MeshScope, TextureSource, BUILTIN_TEXTURES,
    TEXTURE_EMBED_MAX,
    TEXTURE_SCALE_MAX, TEXTURE_SCALE_MIN,
};
pub use mep::replace_mep_run;
pub use mep::{
    insert_mep_run, is_mep, parse_points, points_text, ClashFix, ClashHit, ClashInput, ClashItem,
    ClashMerge, ClashParams, ClashSide, ClashStatus, ElKey, Fitting, MepClash, MepNetwork, MepNode,
    MepPlanPrim, MepQty, MepSystem, RunSpec, SizeRow, WorldPort, CLASH_CATS,
};
pub use mep::{
    nearest_dn, run_slope_pct, start_z, with_dn, with_duct_shape, with_elevation, with_material,
    with_slope as with_run_slope, DuctShape,
};
pub use mep::{
    routed_points, DeviceSpec, MepDeviceRow, MepHit, MepIssue, MepRunFlow, MepSnapSet,
    MepSystemCheck, Placed as MepPlaced, PortLink, RouteEnd, RouteSpec,
};
pub use mep::{CableInput, CableRules, Conductor, RuleFinding, Standard};
pub use mep::{auto_size, preview_rules, CableSized, Feeder, SingleLine};
pub use mep::{take_follow_problems, FollowGhost};
pub use mep::{run_axis_length, NewSize as MepNewSize};
pub use mep::{
    cable_mesh, wire_bow, Built as MepBuilt, CABLE_DRAWN_R, CableTrace, Circuit, CircuitDevice, CircuitWire, CircuitIssue, CircuitKind, CircuitReport,
    CircuitSpec, MepBuild, NetReq, PanelSchedule, Planned as MepPlanned, RouteFrom, RouteMetrics,
    SysRef, VARIANTS as ROUTE_VARIANTS,
};
pub use mep3d::{segments as mep_segments, MepBody, MepPart, RunInfo};
pub use mep_fit::{fitting_key, FittingRef};
pub use mesh_edit::{
    recognize_mesh_data, ConvertReport, Converted, MeshInfo, MeshPlan, MeshPlanCache, MeshRow,
    CONVERTED_FROM, MERGED_FROM,
};
pub use meshcache::{
    build_mesh_cache, build_mesh_cache_file, CacheStats, Placed, Proto, ProtoInfo,
};
pub use modify::{AlignEdge, ModifyError, Reference, Spacing, Target, MAX_DIVIDE};
pub use modify::{
    path_preview, surface_preview, GridDiv, PathCount, PatternFace, PatternPlacement,
    PatternPreview, SurfaceUp,
};
pub use keys::{minted_key, KeySource, KEY_PROP, MINTED_PROP};
pub use ops::Snapshots;
pub use ops_apply::{ref_table, OptionApplyError, Receipt, Rejection, WorkerCopy};
pub use options::{AcceptReport, DesignOption, HiddenByOption, OptionSet};
pub use num_text::{parse_invariant, parse_stored};
pub use params::{
    new_guid, parse_shared, parse_value, shared_text, PVal, ParamDef, ParamSet, ParamSpec,
    ParamType, CATEGORIES as PARAM_CATEGORIES, SHARED_HEADER, TYPE_OF as PARAM_TYPE_OF,
};
pub use journal_walk::{JournalWalk, WalkStep};
pub use phase_dates::{parse_dates, Date, DATES_KEY};
pub use phases::{
    find_phase, status_of, std_phase_en, Demolition, ElemPhase, Phase, PhaseFilter, PhaseIndex,
    PhaseShow, PhaseStatus, ViewKey, ViewPhase, DEFAULT_PHASES, PHASED,
};
pub use project::{
    autosave_file, open_project_copy, probe_project, remove_db_files, replace_file,
    save_project_file, LockError, OpenedCopy, ProjectError, ProjectLock, PROJECT_EXT, RENAME_WAIT,
};
pub use project_info::{read_info as read_project_info, write_in_batch as write_project_info_in_batch, ProjectInfo};
pub use project_logo::{png_size, MAX_LOGO_BYTES};
pub use purge::{PurgeItem, PURGE_TABLES};
pub use saved_filters::{LEGACY_FILTER_MARK, SAVED_FILTERS_KEY};
pub use schedule_defs::SCHEDULE_DEFS_KEY;
pub use ramps::{put_ramp, RampSpec};
pub use rebar::{
    default_covers as rebar_default_covers, layout_fits as rebar_layout_fits,
    linear_host as rebar_linear_host, Faces as RebarFaces, LongPos as RebarPos, RebarBar,
    RebarBuilt, RebarLayout, RebarPlan, RebarRow, RebarSetRow, RebarSetSpec, RebarType,
    REBAR_HOSTS,
};
pub use roof_edge::{Body as EdgeBody, SkylightSpec, SweepHost, SweepSpec, TaperSpec};
pub use roofs::{RoofKind, RoofQuantities, RoofSpec};
pub use rooms::{region_holder, room_closers, Boundary, RoomQty, RoomSpec, SepSpec};
pub use sections::{compass_elevations_around, SectionSpec, ViewKind, COMPASS};
pub use shafts::{CeilingSpec, ShaftCut, ShaftSpec};
pub use field_err::FieldErr;
pub use stage_err::StageErr;
pub use sheets::{
    increment_number, DsetApplied, DsetPlan, DsetRef, DsetSchedule, DsetSheet, DsetViewport,
    DSET_FIELD, DSET_LEVELS, DSET_SET, MarkPrefix, Revision, SchedulePlace, ScheduleSplit,
    SheetDef, SheetErr,
    SheetKind,
    ViewPlacement, ViewRef,
    ViewportDef, WallTypeLegend, DEFAULT_COLUMN_GAP, DEFAULT_CUT, PAPERS, SCALES, STANDARDS,
};
pub use sketch::{
    parse_shape, parse_sketch, shape_text, sketch_text, SketchHost, WallSide, SKETCH_SAG,
};
pub use slabs::{parse_slope, slope_text, SlabQty, SlabSpec};
pub use freeform::{FreeformMesh, FreeformQty, FreeformSpec, ShellSide};
pub use solids::{
    form_json, form_of, frame_json, put_solid, SolidCut, SolidGroup, SolidIfc, SolidQty, SolidSpec,
    HOST_KINDS, SOLID_IFC_CLASSES,
};
pub use panel_system::{PanelLayout, PanelQty, PanelSystemGeom, PanelSystemSpec};
pub use shell_elem::{LayerQty, ShellElemSpec, ShellKind, ShellLayer, ShellMesh, ShellQty};
pub use stairs::{put_railing, put_stair, RailingHost, RailingSpec, StairSpec};
pub use stats::{
    category_counts, CategoryTotal, LevelTotals, Totals, COUNTED_CATEGORIES, SOLID_CATEGORIES,
    SUMMARY_CATEGORIES,
};
pub use struct_ext::{density, StructBody, StructQty, TakeoffRow, STRUCT_KINDS};
pub use struct_views::{StructClass, StructPrim};
pub use steel_conn::{steel_conn_rebuilds, ConnSkip, SteelConnReport, SteelConnSpec, STEEL_MEMBER_KINDS};
pub use steel_frame::SteelFrameIds;
pub use gridshell::{
    FormFind, FormFindReport, GridForm, GridKind, GridPattern, GridShellNode, GridShellReport,
    GridShellSpec, GridSupports, NODE_GAP,
};
pub use structure::{BeamJoin, BeamSpec, ColumnSpec, MemberQuantity};
pub use topo::{
    crs_code_valid, fill_site_in_batch, suggest_utm, PadReport, PadSpec, SiteProps, TopoBuilt,
    TopoPiece, TopoRegion, TopoSpec,
};
pub use types::{copy_type_data, MepResize, TypeTable};
pub use view_gfx::{
    prim_ref, rule_number, rule_quantity, CatGfx, DetailLevel, ElemPaint, FilterRule, FilterUse,
    GfxOverride, RangeIssue, RuleOp, TemplateField, ViewFilter, ViewFilterSpec, ViewGfx,
    ViewGfxState, ViewPaint, ViewRange, ViewTemplate, GFX_CATEGORIES,
};
pub use views::{GridAxis, LineClass, PlanView, PlanViewSpec, Side, ViewPrim};
pub use views_inner::{INNER_CHAIN, RADIUS_CHAIN};
pub use area_std::{AreaMethod, AreaReport, RoomArea, StoreyArea, UseArea};
pub use finishes::{FinishSource, RoomFinish, FINISH_PARAMS};
pub use views3d::{CategoryRule, View3dCamera, View3dSpec};
pub use walls::{
    check_bulge, join_tolerant, parse_top_profile, top_profile_text, ElementWarning, LayerSpec,
    Lean, OpeningSpec, Swing, WallSpec,
};
pub use wall_spline::{SplineLayerQty, SplineWallSpec, MAX_PIECES, SPLINE_TOL, SPLINE_TOL_RANGE};
pub use zones::ZoneSpec;

pub type ElementId = i64;

/// File format version written by this build.
pub const SCHEMA_VERSION: i64 = schema::VERSION;

/// Full recompute of derived tables (tests; operations and undo go through `regen::logged`).
#[cfg(test)]
pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    regen::full(db).map(|_| ())
}

/// Operation kinds after which derived tables are recomputed.
pub(crate) fn derives(kind: &str) -> bool {
    // curtain wall axes bound rooms; panel edits do not move them;
    // grid ops move walls locked to the grid (align_grid / lock_dist_grid)
    [
        "wall",
        "slab",
        "room",
        "level",
        "curtain",
        "dim",
        "measure",
        "col",
        "beam",
        "roof",
        "member",
        "shaft",
        "ceiling",
        "stair",
        "railing",
        "comp",
        "grid",
        "mass",
        "host_opening",
        "footing",
        "solid",
        "steel_conn",
        "material_edit",
        "material_replace",
    ]
    .iter()
    .any(|p| kind.starts_with(p))
        && kind != "curtain_panel"
        || kind.starts_with("constraint")
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Modify(#[from] ModifyError),
    #[error("geometry: {0}")]
    Geom(String),
    /// The edit would degenerate the element (zero length, self-intersection); nothing was written.
    #[error("degenerate: {0}")]
    Degenerate(String),
    #[error("{kind} {id} is still referenced by {}", fmt_refs(.by))]
    Referenced {
        kind: &'static str,
        id: i64,
        by: Vec<(&'static str, i64)>,
    },
    #[error("opening is not inside its {kind} {host}")]
    OpeningOutsideHost { kind: &'static str, host: i64 },
    #[error("file schema version {found} is newer than supported {supported}")]
    NewerVersion { found: i64, supported: i64 },
    /// The edit would violate constraints; the text names them (conflict report).
    #[error("{0}")]
    Conflict(String),
    /// A sheet edit refused; the typed reason localizes without internal ids.
    #[error("{0}")]
    Sheet(sheets::SheetErr),
    /// A material, template or filter edit refused; the typed reason names the field.
    #[error("{0}")]
    Field(field_err::FieldErr),
    /// A staged Add refused; the typed reason localizes without internal text.
    #[error("{0}")]
    Stage(stage_err::StageErr),
    #[error("model file is damaged: {0}")]
    Corrupt(String),
    /// The work was stopped by its cancel token (`ddc_geom::cancel`); nothing was written.
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn fmt_refs(by: &[(&str, i64)]) -> String {
    let v: Vec<String> = by.iter().map(|(t, i)| format!("{t} {i}")).collect();
    v.join(", ")
}

/// The model file. Every change writes an operation with inverse SQL commands, so it is undoable.
pub struct Model {
    pub(crate) db: Connection,
    tracker: std::sync::Arc<std::sync::Mutex<changes::Tracker>>,
    /// Structural assembly for views (struct_ext), reset by any write.
    pub(crate) struct_cache: struct_ext::Cache,
    /// Cache of roof edge solids until the next file change.
    pub(crate) memo: roof_edge::Memo,
    /// Index for partial recompute of derived tables (`regen`).
    pub(crate) regen: regen::Index,
    /// Rebar bars until the next file change.
    pub(crate) rebar_cache: rebar::Cache,
    /// View graphics driven by the change journal: editing one element does not recompute all of them.
    pub(crate) paint_cache: view_gfx::PaintCache,
    /// Crowd level of plants last used (`components::plant_level`), the memory of its hysteresis.
    pub(crate) plant_rank: [std::sync::atomic::AtomicU8; 2],
    /// Process-wide identity of this open document (`trial::next_serial`).
    pub(crate) serial: u64,
}

impl Model {
    pub fn open(path: &str) -> Result<Self, Error> {
        Self::open_with(path, true)
    }

    /// Own fresh copy of an already verified file (`PlanCopies::work`): skips the full `quick_check`,
    /// which takes minutes on a large file.
    pub fn open_copy(path: &str) -> Result<Self, Error> {
        Self::open_with(path, false)
    }

    fn open_with(path: &str, check: bool) -> Result<Self, Error> {
        let db = Connection::open(path)?;
        // a worker connection (mesh cache) may hold the write lock for a batch: wait, not fail
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        let on_disk = path != ":memory:" && !path.is_empty();
        if on_disk && check {
            // not a database or corrupt pages: a clear error before any writes
            file::quick(&db).map_err(|e| match e {
                Error::Db(d) => Error::Corrupt(d.to_string()),
                e => e,
            })?;
            if let Some((t, row)) = finite::first_non_finite(&db)? {
                return Err(Error::Corrupt(format!("non-finite number in {t}, row {row}")));
            }
        }
        Self::adopt_conn(db, on_disk.then_some(path), check)
    }

    /// A model held in memory from the bytes of a `.ddc` file (browser build: no file system).
    /// Same checks as `open`; the model is not tied to a path.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let corrupt = |e: Error| match e {
            Error::Db(d) => Error::Corrupt(d.to_string()),
            e => e,
        };
        if !bytes.starts_with(b"SQLite format 3\0") || bytes.len() < 100 {
            return Err(Error::Corrupt("not a model file".into()));
        }
        let mut buf = bytes.to_vec();
        // header bytes 18/19 = 2 mark a WAL file, which an in-memory database cannot open
        for b in &mut buf[18..20] {
            if *b == 2 {
                *b = 1;
            }
        }
        let mut db = Connection::open_in_memory()?;
        db.deserialize_read_exact(rusqlite::MAIN_DB, &buf[..], buf.len(), false)
            .map_err(|e| corrupt(e.into()))?;
        file::quick(&db).map_err(corrupt)?;
        if let Some((t, row)) = finite::first_non_finite(&db)? {
            return Err(Error::Corrupt(format!("non-finite number in {t}, row {row}")));
        }
        Self::adopt_conn(db, None, true)
    }

    /// The whole model as the bytes of a `.ddc` file (committed state; `from_bytes` reads it back).
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        // serialize needs a &mut connection; take it from an online in-memory copy
        let mut db = self.worker_copy()?.0;
        Ok(db.serialize(rusqlite::MAIN_DB)?.to_vec())
    }

    /// View-only (browser on a tablet or phone): SQLite itself refuses every write
    /// (`PRAGMA query_only`), so no tool, script or AI step can change the model.
    pub fn set_query_only(&self, on: bool) -> Result<(), Error> {
        self.db.pragma_update(None, "query_only", on)?;
        Ok(())
    }

    pub fn query_only(&self) -> bool {
        self.db.pragma_query_value(None, "query_only", |r| r.get::<_, bool>(0)).unwrap_or(false)
    }

    /// Wraps an open connection (file or in-memory copy); `check` false keeps a copy row for row.
    pub(crate) fn adopt_conn(
        mut db: Connection,
        path: Option<&str>,
        check: bool,
    ) -> Result<Self, Error> {
        // the bundled SQLite has no math functions; the check queries use sqrt
        db.create_scalar_function(
            "sqrt",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| Ok(ctx.get::<Option<f64>>(0)?.filter(|x| *x >= 0.0).map(f64::sqrt)),
        )?;
        #[cfg(feature = "test-hooks")]
        db.trace_v2(rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT, Some(query_count::on_trace))?;
        materials::COPY_OPEN.with(|c| c.set(!check));
        let up = schema::upgrade(&mut db, path);
        materials::COPY_OPEN.with(|c| c.set(false));
        up?;
        if let (Some(p), true) = (path, check) {
            stage::clean_stale_stages(p);
        }
        phases::track(&db)?;
        groups::pin_triggers(&db)?;
        mep::clash_track(&db)?;
        let tracker = changes::install(&mut db)?;
        Ok(Self {
            db,
            tracker,
            struct_cache: Default::default(),
            memo: Default::default(),
            regen: Default::default(),
            rebar_cache: Default::default(),
            paint_cache: Default::default(),
            plant_rank: Default::default(),
            serial: trial::next_serial(),
        })
    }

    /// File format version (`meta.schema_version`).
    pub fn schema_version(&self) -> Result<i64, Error> {
        schema::stored_version(&self.db)?.ok_or_else(|| Error::Invalid("no schema_version".into()))
    }

    /// Connection for arbitrary SQL queries over the data (write through model methods, otherwise there is no undo).
    pub fn sql(&self) -> &Connection {
        &self.db
    }
}
