//! DDL. Tables without a prefix are the core; `wall*`/`level` are walls; `d_*` is the 2D drawing.
//! `wall_qty`, `slab_qty`, `room_qty` are derived quantities (regenerated, not written to the journal).

use rusqlite::{params, Connection, OptionalExtension};

use crate::Error;

/// File format version. 1 — journal without `op.inv`; 2 — with inverse SQL commands;
/// 3 — slabs and rooms (the 2->3 migration is additive: `SCHEMA` creates the new tables);
/// 4 — door swing side/hinges (`opening.swing_*`) and room separators `room_sep` (additive);
/// 5 — arc wall axis (`wall.axis_kind`, `wall.bulge`, additive);
/// 6 — external references `ext_ref`, meshes `mesh`, properties `prop` (import via the bridge, additive);
/// 7 — `ext_ref.source` (bridge + source file): guid is unique per (source, guid) pair; old rows
/// get source = '' (moved preserving rowid);
/// 8 — wall top polyline `wall.top_profile` (NULL is a flat top, additive);
/// 9 — curtain walls `curtain_wall`/`curtain_panel`, slab top slope `slab.slope` (additive);
/// 10 — curtain wall: grid line/segment sections `curtain_wall.mullion_lines`, polyline top
/// `curtain_wall.top_profile` (NULL means none, additive);
/// 11 — sections and elevations `view_def` (additive);
/// 12 — dimension objects `dim` and their derived geometry `dim_geo` (additive);
/// 13 — columns `col`, beams `beam`, section types `member_type`, roofs `roof`/`roof_vertex`,
/// derived `member_qty`/`roof_qty`, wall-top-to-roof attachment `wall.top_roof` (additive);
/// 14 — grids `grid`, reference planes `ref_plane`, shafts `shaft`, ceilings `ceiling`, stairs
/// `stair`, railings `railing` and their derived quantities (additive);
/// 15 — components: families `comp_family`, types `comp_type`, instances `comp`, derived
/// `comp_qty` (additive);
/// 16 — MEP networks: systems `mep_system`, runs `mep_run` (additive);
/// 17 — edit constraints `constraint_rule` (dimension lock, EQ, alignment, parallel, perpendicular; additive);
/// 18 — per-view extents and bubbles of grids/levels `datum_extent`; constraints on grids and levels (additive);
/// 19 — masses `mass`, paint `mass_paint`, host bindings `mass_host` (with an "orphan" flag),
/// joins `geom_join`, derived `mass_qty`/`mass_cache`/`join_cache` (additive; the step
/// checks tables and columns, so the merge order with step 18 does not matter);
/// unnumbered — sketches `sketch`, slab shape `slab_shape`, elevation profile `wall_profile`,
/// wall attachment `wall_attach` (`sketch::ensure`: tables by name, on every open);
/// 20 — `room.outline`: an imported room outline (additive, `add_room_outline`);
/// 21 — sheets `sheet`, revisions `sheet_rev`, viewports `viewport`, schedules on a sheet
/// `sheet_schedule`, sets `sheet_set`/`sheet_set_item` (additive, by table presence);
/// 22 — phases `phase`/`elem_phase`/`view_phase`, project parameters `pparam`/`pparam_cat`/`pparam_val`
/// (additive, existence checks);
/// 23 — typed solids `solid`, `solid_host`, derived `solid_qty`, `solid_cut` (additive,
/// `solids::DDL`; later columns `solid.ifc_class`/`ifc_type` by presence, `solids::migrate`);
/// 24 — derived mesh cache `mesh_proto`/`mesh_geo` (instances, LOD; additive, see meshcache);
/// 25 — terrain `toposolid`/`topo_point`/`topo_region`/`topo_pad`, site `site_info` (additive,
/// by table presence);
/// 26 — foundations `iso_footing`/`strip_footing`/`raft`, braces `brace`, trusses `truss`, beam
/// systems `beam_system`, derived `struct_qty` (`struct_ext::DDL`, additive);
/// 27 — roofs and edges: `roof_ext`, `sweep_profile`, `sweep`, `skylight`, `soffit`, `roof_taper`,
/// `roof_drain` (additive, `roof_edge::ensure`);
/// 28 — import sources `src_*` (last-import base, conflicts, parameters, views) and linked
/// models `model_link` (additive, `source::migrate`/`links::migrate`);
/// 29 — family editor: drafts `fam_doc`, thumbnail cache `comp_thumb` (additive,
/// `family_edit::migrate`);
/// 30 — reinforcement `rebar_type`/`rebar_set`/`rebar_cover`, analytical adjustments `analytical_adj`
/// (additive, `rebar::migrate`);
/// 31 — AI journal and settings `ai_audit`/`ai_policy` (additive, `ai_audit::migrate`);
/// 32 — ramps `ramp`, zones `zone`/`zone_room` (additive, `ramps::migrate`/`zones::migrate`);
/// 33 — groups and pinning: `el_group`, `group_inst`, `group_member`, `el_pin`
/// (`groups::migrate`);
/// 34 — view annotations `anno`, styles `anno_text_style`/`anno_line_style`, detail elements `anno_detail_def`,
/// view scale `anno_view_scale`, legends `anno_legend*` (additive, `annot_text::migrate`);
/// 35 — materials `material` (layer texts stay a reference by name), view graphics `view_gfx`,
/// templates `view_template`, filters `view_filter`, object styles `object_style` (additive,
/// `materials::migrate`/`view_gfx::migrate`);
/// 36 — sheet output: schedule splitting `sheet_schedule.max_h/col_gap`, view number
/// `viewport.detail` (additive, `sheets::output::migrate`);
/// 37 — manual networks: component connections `mep_port` (additive, `mep::migrate_ports`);
/// 38 — design options `design_option_set`/`design_option`/`option_member`/`view_option`, merge
/// with a colleague's file `merge_peer`/`merge_map`, project tag `meta.project_uid` (additive,
/// `options::migrate`).
/// 39 — memory of the source file: hash, stat, relative path, `src_target` (additive,
/// `source::file::migrate`, called from `source::migrate`).
/// 40 - journal provenance `op.actor`/`op.title`/`op.why`/`op.src` (additive, old rows NULL,
/// `journal::migrate`).
/// 41 - AI design option recipes `ai_option` (additive, `ai_option::migrate`).
/// 42 - linked boolean forms `mass_bool` (additive, `mass::bool_link::migrate`).
/// 43 - parameter locks `param_lock` (additive, `groups::migrate_locks`).
/// 44 - structural analysis: `an_override`, `load_case`, `load`, `load_combo(_term)`,
/// `calc_settings`, runs `calc_run`/`calc_blob`, material E, G, nu, fy, alpha (additive,
/// `calc_store::migrate`).
/// 45 - issues `issue`/`issue_elem`/`issue_comment` (BCF topics; additive, `topics::migrate`);
/// later the exchange fields `issue.bcf` and carried documents `issue_doc`.
/// 46 - value provenance `value_src` (additive, `value_src::migrate`).
/// 47 - geometry recipes `recipe`/`recipe_part` (additive, `recipe::migrate`); later the
/// instance key `recipe.ikey`, minted on open for rows without one.
/// 48 - electrical circuits `elec_circuit`/`elec_circuit_dev` (additive, `mep::migrate_circuits`).
/// 49 - site coordinate system `site_info.crs_code/crs_name/crs_vertical` (additive columns,
/// `topo::migrate`).
/// 50 - doors and windows as components: links `opening_comp` from a wall opening to its
/// component instance (additive, `opening_comp::migrate`).
/// 51 - design depth `elem_depth` (D1..D5 + discipline per element) and the project setting
/// `depth_cfg` (additive, `depth::migrate`; 50 is taken by the component library).
/// 52 - leaning and tapered walls `wall.lean` (radians), `wall.top_scale` (top/base thickness)
/// (additive, defaults 0 and 1, `add_wall_lean`).
/// 53 - assembly joints `asm_joint` between component instances (additive, `joints::migrate`;
/// 52 is taken by wall lean).
/// 54 - freeform design: NURBS patches `freeform`, curtain systems on them `panel_system`, layered
/// shells `shell_elem`, curved walls `wall_spline`/`wall_spline_piece`, grid shells
/// `gridshell`/`gridshell_node`/`gridshell_bar` (additive, `migrate_54_freeform`).
/// 56 - saved measurements `measure` with derived `measure_geo`/`measure_ref` (additive,
/// `measure::migrate`).
/// Steps added by parallel branches merge here; each additive migration checks presence rather than `found < N`.
pub(crate) const STEPS: &[i64] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47,
    48, 49, 50, 51, 52, 53, 54, 56,
];
pub(crate) const VERSION: i64 = max_step(STEPS);

const fn max_step(s: &[i64]) -> i64 {
    let mut i = 0;
    let mut m = 0;
    while i < s.len() {
        if s[i] > m {
            m = s[i];
        }
        i += 1;
    }
    m
}

/// Schema 19 (masses): run by the [`migrate_19_masses`] step, `IF NOT EXISTS` makes it idempotent.
const MASS_SCHEMA: &str = "
-- \u{424}\u{43e}\u{440}\u{43c}\u{430}: form — \u{438}\u{441}\u{442}\u{43e}\u{440}\u{438}\u{44f} \u{43f}\u{43e}\u{441}\u{442}\u{440}\u{43e}\u{435}\u{43d}\u{438}\u{44f} (JSON), \u{43a}\u{43e}\u{43e}\u{440}\u{434}\u{438}\u{43d}\u{430}\u{442}\u{44b} \u{43e}\u{442} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f}. \u{41f}\u{440}\u{438}\u{432}\u{44f}\u{437}\u{43a}\u{430}: role face|floor,
-- face — \u{43c}\u{435}\u{442}\u{43a}\u{430} \u{433}\u{440}\u{430}\u{43d}\u{438}, sig — \u{432}\u{438}\u{434} \u{441}\u{43e}\u{437}\u{434}\u{430}\u{432}\u{448}\u{435}\u{439} \u{435}\u{451} \u{43e}\u{43f}\u{435}\u{440}\u{430}\u{446}\u{438}\u{438}, level — \u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c} \u{44d}\u{442}\u{430}\u{436}\u{430}, orphan — \u{433}\u{440}\u{430}\u{43d}\u{44c} \u{43f}\u{440}\u{43e}\u{43f}\u{430}\u{43b}\u{430}.
CREATE TABLE IF NOT EXISTS mass(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, name TEXT NOT NULL,
    void INTEGER NOT NULL DEFAULT 0, inplace INTEGER NOT NULL DEFAULT 0, form TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS mass_paint(mass INTEGER NOT NULL, face INTEGER NOT NULL, material TEXT NOT NULL,
    PRIMARY KEY(mass, face));
CREATE TABLE IF NOT EXISTS mass_host(kind TEXT NOT NULL, ref INTEGER NOT NULL, mass INTEGER NOT NULL,
    face INTEGER NOT NULL, role TEXT NOT NULL DEFAULT 'face', level INTEGER, sig TEXT,
    orphan INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(kind, ref));
CREATE INDEX IF NOT EXISTS mass_host_mass ON mass_host(mass);
CREATE TABLE IF NOT EXISTS geom_join(winner_kind TEXT NOT NULL, winner INTEGER NOT NULL,
    loser_kind TEXT NOT NULL, loser INTEGER NOT NULL, PRIMARY KEY(winner_kind, winner, loser_kind, loser));
CREATE TABLE IF NOT EXISTS mass_qty(mass INTEGER NOT NULL, level INTEGER NOT NULL, area REAL NOT NULL,
    perimeter REAL NOT NULL, PRIMARY KEY(mass, level));
CREATE TABLE IF NOT EXISTS mass_cache(mass INTEGER PRIMARY KEY, key TEXT NOT NULL);
-- \u{414}\u{43e}\u{43b}\u{438} \u{43e}\u{431}\u{44a}\u{451}\u{43c}\u{430} \u{447}\u{430}\u{441}\u{442}\u{435}\u{439} \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{430} \u{43f}\u{43e}\u{441}\u{43b}\u{435} \u{432}\u{44b}\u{440}\u{435}\u{437}\u{43e}\u{432} (\u{43a}\u{43b}\u{44e}\u{447} — \u{433}\u{435}\u{43e}\u{43c}\u{435}\u{442}\u{440}\u{438}\u{44f} \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{430} \u{438} \u{432}\u{441}\u{435}\u{445} \u{440}\u{435}\u{437}\u{430}\u{43a}\u{43e}\u{432}).
CREATE TABLE IF NOT EXISTS join_cache(kind TEXT NOT NULL, ref INTEGER NOT NULL, key TEXT NOT NULL,
    ratios TEXT NOT NULL, PRIMARY KEY(kind, ref));
";

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS element(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, alive INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS param(element INTEGER NOT NULL, name TEXT NOT NULL, value REAL, PRIMARY KEY(element, name));
CREATE TABLE IF NOT EXISTS op(seq INTEGER PRIMARY KEY, element INTEGER NOT NULL, kind TEXT NOT NULL,
    name TEXT, old REAL, new REAL, inv TEXT NOT NULL DEFAULT '[]');

CREATE TABLE IF NOT EXISTS level(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, elevation REAL NOT NULL);
CREATE TABLE IF NOT EXISTS wall_type(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS wall_type_layer(wtype INTEGER NOT NULL, pos INTEGER NOT NULL,
    material TEXT NOT NULL, thickness REAL NOT NULL, PRIMARY KEY(wtype, pos));
CREATE TABLE IF NOT EXISTS wall(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, wtype INTEGER NOT NULL,
    ax REAL NOT NULL, ay REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL,
    base_offset REAL NOT NULL, height REAL NOT NULL, offset0 REAL);
CREATE TABLE IF NOT EXISTS opening(id INTEGER PRIMARY KEY, wall INTEGER NOT NULL, along REAL NOT NULL,
    width REAL NOT NULL, sill REAL NOT NULL, height REAL NOT NULL, kind TEXT NOT NULL);
-- \u{41f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{435}: \u{432}\u{435}\u{440}\u{445} = \u{43e}\u{442}\u{43c}\u{435}\u{442}\u{43a}\u{430} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f} + offset, \u{441}\u{43b}\u{43e}\u{438} (pos 0 \u{441}\u{432}\u{435}\u{440}\u{445}\u{443}) \u{438}\u{434}\u{443}\u{442} \u{432}\u{43d}\u{438}\u{437}.
-- slab_vertex.ring: 0 — \u{432}\u{43d}\u{435}\u{448}\u{43d}\u{438}\u{439} \u{43a}\u{43e}\u{43d}\u{442}\u{443}\u{440} (\u{43f}\u{440}\u{43e}\u{442}\u{438}\u{432} \u{447}\u{430}\u{441}\u{43e}\u{432}\u{43e}\u{439}), 1.. — \u{43e}\u{442}\u{432}\u{435}\u{440}\u{441}\u{442}\u{438}\u{44f} (\u{43f}\u{43e} \u{447}\u{430}\u{441}\u{43e}\u{432}\u{43e}\u{439}).
CREATE TABLE IF NOT EXISTS slab_type(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS slab_type_layer(stype INTEGER NOT NULL, pos INTEGER NOT NULL,
    material TEXT NOT NULL, thickness REAL NOT NULL, PRIMARY KEY(stype, pos));
CREATE TABLE IF NOT EXISTS slab(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, stype INTEGER NOT NULL,
    offset REAL NOT NULL);
CREATE TABLE IF NOT EXISTS slab_vertex(slab INTEGER NOT NULL, ring INTEGER NOT NULL, idx INTEGER NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, PRIMARY KEY(slab, ring, idx));
CREATE TABLE IF NOT EXISTS slab_qty(slab INTEGER NOT NULL, pos INTEGER NOT NULL, material TEXT NOT NULL,
    area REAL NOT NULL, volume REAL NOT NULL, PRIMARY KEY(slab, pos));
-- \u{41d}\u{430}\u{432}\u{435}\u{441}\u{43d}\u{430}\u{44f} \u{441}\u{442}\u{435}\u{43d}\u{430}: ugrid/vgrid — `s \u{448}\u{430}\u{433}` \u{438}\u{43b}\u{438} `x \u{43f}\u{43e}\u{437}…`; \u{438}\u{43c}\u{43f}\u{43e}\u{441}\u{442} — '' | `r \u{448}\u{438}\u{440}\u{438}\u{43d}\u{430} \u{433}\u{43b}\u{443}\u{431}\u{438}\u{43d}\u{430}` | `c \u{434}\u{438}\u{430}\u{43c}\u{435}\u{442}\u{440}`.
-- curtain_panel — \u{442}\u{43e}\u{43b}\u{44c}\u{43a}\u{43e} \u{44f}\u{447}\u{435}\u{439}\u{43a}\u{438}, \u{43e}\u{442}\u{43b}\u{438}\u{447}\u{43d}\u{44b}\u{435} \u{43e}\u{442} \u{43e}\u{441}\u{442}\u{435}\u{43a}\u{43b}\u{451}\u{43d}\u{43d}\u{43e}\u{439} (kind glazed|solid|door|window|empty).
CREATE TABLE IF NOT EXISTS curtain_wall(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, name TEXT NOT NULL,
    ax REAL NOT NULL, ay REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL, bulge REAL NOT NULL,
    base_offset REAL NOT NULL, height REAL NOT NULL, ugrid TEXT NOT NULL, vgrid TEXT NOT NULL,
    mullion_in TEXT NOT NULL, mullion_border TEXT NOT NULL, mullion_lines TEXT, top_profile TEXT);
CREATE TABLE IF NOT EXISTS curtain_panel(cw INTEGER NOT NULL, i INTEGER NOT NULL, j INTEGER NOT NULL,
    kind TEXT NOT NULL, name TEXT NOT NULL, swing_side INTEGER NOT NULL, swing_hinge INTEGER NOT NULL,
    PRIMARY KEY(cw, i, j));
-- \u{41f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{435} \u{437}\u{430}\u{434}\u{430}\u{451}\u{442}\u{441}\u{44f} \u{442}\u{43e}\u{447}\u{43a}\u{43e}\u{439} \u{432}\u{43d}\u{443}\u{442}\u{440}\u{438}; room_qty — \u{43f}\u{440}\u{43e}\u{438}\u{437}\u{432}\u{43e}\u{434}\u{43d}\u{430}\u{44f}, \u{43d}\u{435}\u{442} \u{441}\u{442}\u{440}\u{43e}\u{43a}\u{438} = \u{43d}\u{435} \u{437}\u{430}\u{43c}\u{43a}\u{43d}\u{443}\u{442}\u{43e} \u{441}\u{442}\u{435}\u{43d}\u{430}\u{43c}\u{438}.
CREATE TABLE IF NOT EXISTS room(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, name TEXT NOT NULL,
    number TEXT NOT NULL, px REAL NOT NULL, py REAL NOT NULL, height REAL NOT NULL);
-- \u{420}\u{430}\u{437}\u{434}\u{435}\u{43b}\u{438}\u{442}\u{435}\u{43b}\u{44c} \u{43f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{439}: \u{43e}\u{442}\u{440}\u{435}\u{437}\u{43e}\u{43a} \u{43d}\u{430} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{435}, \u{437}\u{430}\u{43c}\u{44b}\u{43a}\u{430}\u{44e}\u{449}\u{438}\u{439} \u{43f}\u{43e}\u{43c}\u{435}\u{449}\u{435}\u{43d}\u{438}\u{435} \u{442}\u{430}\u{43c}, \u{433}\u{434}\u{435} \u{43d}\u{435}\u{442} \u{441}\u{442}\u{435}\u{43d}\u{44b}.
CREATE TABLE IF NOT EXISTS room_sep(id INTEGER PRIMARY KEY, level INTEGER NOT NULL,
    ax REAL NOT NULL, ay REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL);
CREATE TABLE IF NOT EXISTS room_qty(room INTEGER PRIMARY KEY, area REAL NOT NULL, perimeter REAL NOT NULL,
    volume REAL NOT NULL, rings TEXT NOT NULL);
-- \u{418}\u{43c}\u{43f}\u{43e}\u{440}\u{442}: \u{441}\u{442}\u{430}\u{431}\u{438}\u{43b}\u{44c}\u{43d}\u{44b}\u{439} \u{432}\u{43d}\u{435}\u{448}\u{43d}\u{438}\u{439} id → \u{441}\u{442}\u{440}\u{43e}\u{43a}\u{430} \u{43c}\u{43e}\u{434}\u{435}\u{43b}\u{438} (kind = \u{438}\u{43c}\u{44f} \u{442}\u{430}\u{431}\u{43b}\u{438}\u{446}\u{44b}); \u{441}\u{435}\u{442}\u{43a}\u{430} \u{441} \u{43a}\u{430}\u{442}\u{435}\u{433}\u{43e}\u{440}\u{438}\u{435}\u{439};
-- \u{441}\u{432}\u{43e}\u{439}\u{441}\u{442}\u{432}\u{430} \u{43b}\u{44e}\u{431}\u{43e}\u{439} \u{441}\u{442}\u{440}\u{43e}\u{43a}\u{438} (value — bool/int/real/text, unit — \u{441}\u{442}\u{440}\u{43e}\u{43a}\u{430} \u{435}\u{434}\u{438}\u{43d}\u{438}\u{446}\u{44b} \u{438}\u{43b}\u{438} '').
CREATE TABLE IF NOT EXISTS ext_ref(source TEXT NOT NULL DEFAULT '', guid TEXT NOT NULL, kind TEXT NOT NULL,
    ref INTEGER NOT NULL, PRIMARY KEY(source, guid));
CREATE TABLE IF NOT EXISTS mesh(id INTEGER PRIMARY KEY, level INTEGER, category TEXT NOT NULL,
    name TEXT NOT NULL, positions BLOB NOT NULL, indices BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS prop(kind TEXT NOT NULL, ref INTEGER NOT NULL, key TEXT NOT NULL,
    value, unit TEXT NOT NULL, PRIMARY KEY(kind, ref, key));
CREATE TABLE IF NOT EXISTS wall_qty(wall INTEGER NOT NULL, pos INTEGER NOT NULL, material TEXT NOT NULL,
    plan_area REAL NOT NULL, volume REAL NOT NULL, PRIMARY KEY(wall, pos));

CREATE TABLE IF NOT EXISTS d_layer(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    color INTEGER NOT NULL, linetype TEXT NOT NULL, weight REAL NOT NULL);
CREATE TABLE IF NOT EXISTS d_ltype(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    description TEXT NOT NULL, length REAL NOT NULL, pattern TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS d_block(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, bx REAL NOT NULL, by REAL NOT NULL);
CREATE TABLE IF NOT EXISTS d_attdef(block INTEGER NOT NULL, tag TEXT NOT NULL, default_value TEXT NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, height REAL NOT NULL, PRIMARY KEY(block, tag));
-- block: NULL = \u{43f}\u{440}\u{43e}\u{441}\u{442}\u{440}\u{430}\u{43d}\u{441}\u{442}\u{432}\u{43e} \u{43c}\u{43e}\u{434}\u{435}\u{43b}\u{438}, \u{438}\u{43d}\u{430}\u{447}\u{435} \u{441}\u{43e}\u{434}\u{435}\u{440}\u{436}\u{438}\u{43c}\u{43e}\u{435} \u{43e}\u{43f}\u{440}\u{435}\u{434}\u{435}\u{43b}\u{435}\u{43d}\u{438}\u{44f} \u{431}\u{43b}\u{43e}\u{43a}\u{430}.
CREATE TABLE IF NOT EXISTS d_line(id INTEGER PRIMARY KEY, layer INTEGER NOT NULL, block INTEGER,
    x1 REAL NOT NULL, y1 REAL NOT NULL, x2 REAL NOT NULL, y2 REAL NOT NULL);
CREATE TABLE IF NOT EXISTS d_polyline(id INTEGER PRIMARY KEY, layer INTEGER NOT NULL, block INTEGER, closed INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS d_vertex(polyline INTEGER NOT NULL, idx INTEGER NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, bulge REAL NOT NULL, PRIMARY KEY(polyline, idx));
CREATE TABLE IF NOT EXISTS d_arc(id INTEGER PRIMARY KEY, layer INTEGER NOT NULL, block INTEGER,
    cx REAL NOT NULL, cy REAL NOT NULL, r REAL NOT NULL, a0 REAL NOT NULL, a1 REAL NOT NULL);
CREATE TABLE IF NOT EXISTS d_text(id INTEGER PRIMARY KEY, layer INTEGER NOT NULL, block INTEGER,
    x REAL NOT NULL, y REAL NOT NULL, height REAL NOT NULL, rotation REAL NOT NULL, content TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS d_insert(id INTEGER PRIMARY KEY, layer INTEGER NOT NULL, def INTEGER NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, sx REAL NOT NULL, sy REAL NOT NULL, rotation REAL NOT NULL);
CREATE TABLE IF NOT EXISTS d_attr(insert_id INTEGER NOT NULL, tag TEXT NOT NULL, value TEXT NOT NULL,
    PRIMARY KEY(insert_id, tag));
CREATE INDEX IF NOT EXISTS d_line_layer ON d_line(layer);
-- \u{420}\u{430}\u{437}\u{440}\u{435}\u{437}/\u{444}\u{430}\u{441}\u{430}\u{434}: \u{43b}\u{438}\u{43d}\u{438}\u{44f} a→b, \u{432}\u{437}\u{433}\u{43b}\u{44f}\u{434} \u{432}\u{43b}\u{435}\u{432}\u{43e} \u{43e}\u{442} \u{43d}\u{435}\u{451}; kind section|elevation; far_clip 0/1.
-- \u{420}\u{430}\u{437}\u{43c}\u{435}\u{440}: refs — \u{441}\u{441}\u{44b}\u{43b}\u{43a}\u{438} \u{447}\u{435}\u{440}\u{435}\u{437} ';' (\u{441}\u{43c}. annot::DimRef), dx/dy — \u{43d}\u{430}\u{43f}\u{440}\u{430}\u{432}\u{43b}\u{435}\u{43d}\u{438}\u{435} (0,0 — \u{43f}\u{43e} \u{442}\u{43e}\u{447}\u{43a}\u{430}\u{43c}).
CREATE TABLE IF NOT EXISTS dim(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, kind TEXT NOT NULL,
    refs TEXT NOT NULL, dx REAL NOT NULL, dy REAL NOT NULL, side REAL NOT NULL, offset REAL NOT NULL,
    tier INTEGER NOT NULL, auto INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS dim_geo(dim INTEGER PRIMARY KEY, orphan INTEGER NOT NULL, geom TEXT NOT NULL);
-- \u{421}\u{442}\u{435}\u{440}\u{436}\u{43d}\u{438}: \u{441}\u{435}\u{447}\u{435}\u{43d}\u{438}\u{435} — \u{442}\u{435}\u{43a}\u{441}\u{442} ddc_geom::member::Shape; \u{43a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430}: \u{432}\u{435}\u{440}\u{445} = \u{43e}\u{442}\u{43c}\u{435}\u{442}\u{43a}\u{430} top_level (\u{438}\u{43b}\u{438} level)
-- + top_offset, (tx, ty) — \u{432}\u{435}\u{440}\u{445} \u{43d}\u{430}\u{43a}\u{43b}\u{43e}\u{43d}\u{43d}\u{43e}\u{439}; \u{431}\u{430}\u{43b}\u{43a}\u{430}: z \u{43a}\u{43e}\u{43d}\u{446}\u{43e}\u{432} \u{43e}\u{442} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f}, join_* — auto|butt|miter|cope|none.
CREATE TABLE IF NOT EXISTS member_type(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    shape TEXT NOT NULL, material TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS col(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, top_level INTEGER,
    mtype INTEGER NOT NULL, structural INTEGER NOT NULL, x REAL NOT NULL, y REAL NOT NULL,
    base_offset REAL NOT NULL, top_offset REAL NOT NULL, rotation REAL NOT NULL, tx REAL, ty REAL);
CREATE TABLE IF NOT EXISTS beam(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, mtype INTEGER NOT NULL,
    ax REAL NOT NULL, ay REAL NOT NULL, az REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL,
    bz REAL NOT NULL, bulge REAL NOT NULL, yjust INTEGER NOT NULL, zjust INTEGER NOT NULL,
    rotation REAL NOT NULL, start_ext REAL NOT NULL, end_ext REAL NOT NULL,
    join_start TEXT NOT NULL, join_end TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS member_qty(kind TEXT NOT NULL, ref INTEGER NOT NULL, length REAL NOT NULL,
    volume REAL NOT NULL, surface REAL NOT NULL, cut REAL NOT NULL, PRIMARY KEY(kind, ref));
-- \u{41a}\u{440}\u{44b}\u{448}\u{430}: kind footprint (roof_vertex — \u{43a}\u{43e}\u{43d}\u{442}\u{443}\u{440}, slope° \u{440}\u{435}\u{431}\u{440}\u{430}, NULL — \u{444}\u{440}\u{43e}\u{43d}\u{442}\u{43e}\u{43d}; mansard_h NULL — \u{43d}\u{435}\u{442},
-- mansard_deg NULL — \u{43f}\u{430}\u{43b}\u{443}\u{431}\u{430}) | extrusion (roof_vertex — \u{43f}\u{440}\u{43e}\u{444}\u{438}\u{43b}\u{44c} u z \u{43d}\u{430}\u{434} a→b, depth \u{432}\u{43b}\u{435}\u{432}\u{43e}).
-- \u{421}\u{43b}\u{43e}\u{438} — slab_type; offset — \u{43e}\u{442}\u{43c}\u{435}\u{442}\u{43a}\u{430} \u{43f}\u{43b}\u{43e}\u{441}\u{43a}\u{43e}\u{441}\u{442}\u{435}\u{439} \u{43d}\u{430}\u{434} \u{43b}\u{438}\u{43d}\u{438}\u{435}\u{439} \u{43e}\u{43f}\u{438}\u{440}\u{430}\u{43d}\u{438}\u{44f} \u{43e}\u{442} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f}.
CREATE TABLE IF NOT EXISTS roof(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, stype INTEGER NOT NULL,
    kind TEXT NOT NULL, offset REAL NOT NULL, overhang REAL NOT NULL, fascia REAL NOT NULL,
    mansard_h REAL, mansard_deg REAL, ax REAL, ay REAL, bx REAL, by REAL, depth REAL);
CREATE TABLE IF NOT EXISTS roof_vertex(roof INTEGER NOT NULL, idx INTEGER NOT NULL, x REAL NOT NULL,
    y REAL NOT NULL, slope REAL, PRIMARY KEY(roof, idx));
CREATE TABLE IF NOT EXISTS roof_qty(roof INTEGER NOT NULL, pos INTEGER NOT NULL, material TEXT NOT NULL,
    area REAL NOT NULL, volume REAL NOT NULL, PRIMARY KEY(roof, pos));
CREATE TABLE IF NOT EXISTS view_def(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
    ax REAL NOT NULL, ay REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL, depth REAL NOT NULL,
    far_clip INTEGER NOT NULL);
-- \u{41e}\u{441}\u{44c}: \u{43e}\u{442}\u{440}\u{435}\u{437}\u{43e}\u{43a} \u{438}\u{43b}\u{438} \u{434}\u{443}\u{433}\u{430} (bulge = tan(\u{443}\u{433}\u{43e}\u{43b}/4)); \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{438} NULL — \u{431}\u{435}\u{437} \u{43e}\u{433}\u{440}\u{430}\u{43d}\u{438}\u{447}\u{435}\u{43d}\u{438}\u{44f}; bubbles: \u{431}\u{438}\u{442} 0 — \u{43f}\u{443}\u{437}\u{44b}\u{440}\u{44c}
-- \u{432} \u{43d}\u{430}\u{447}\u{430}\u{43b}\u{435}, \u{431}\u{438}\u{442} 1 — \u{432} \u{43a}\u{43e}\u{43d}\u{446}\u{435}.
CREATE TABLE IF NOT EXISTS grid(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    ax REAL NOT NULL, ay REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL, bulge REAL NOT NULL,
    base_level INTEGER, top_level INTEGER, bubbles INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ref_plane(id INTEGER PRIMARY KEY, name TEXT NOT NULL,
    ax REAL NOT NULL, ay REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL);
-- \u{428}\u{430}\u{445}\u{442}\u{430}: \u{432}\u{435}\u{440}\u{442}\u{438}\u{43a}\u{430}\u{43b}\u{44c}\u{43d}\u{44b}\u{439} \u{432}\u{44b}\u{440}\u{435}\u{437} \u{43f}\u{43e} \u{43a}\u{43e}\u{43d}\u{442}\u{443}\u{440}\u{443} \u{43e}\u{442} level+base_offset \u{434}\u{43e} top_level+top_offset.
CREATE TABLE IF NOT EXISTS shaft(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, top_level INTEGER NOT NULL,
    base_offset REAL NOT NULL, top_offset REAL NOT NULL);
CREATE TABLE IF NOT EXISTS shaft_vertex(shaft INTEGER NOT NULL, idx INTEGER NOT NULL, x REAL NOT NULL,
    y REAL NOT NULL, PRIMARY KEY(shaft, idx));
-- \u{41f}\u{440}\u{43e}\u{438}\u{437}\u{432}\u{43e}\u{434}\u{43d}\u{430}\u{44f}: \u{447}\u{442}\u{43e} \u{440}\u{435}\u{436}\u{435}\u{442} \u{448}\u{430}\u{445}\u{442}\u{430} (kind slab|roof|beam|ceiling, \u{43f}\u{43b}\u{43e}\u{449}\u{430}\u{434}\u{44c} \u{43f}\u{43b}\u{430}\u{43d}\u{430}, \u{432}\u{44b}\u{447}\u{442}\u{435}\u{43d}\u{43d}\u{44b}\u{439} \u{43e}\u{431}\u{44a}\u{451}\u{43c}).
CREATE TABLE IF NOT EXISTS shaft_cut(shaft INTEGER NOT NULL, kind TEXT NOT NULL, ref INTEGER NOT NULL,
    area REAL NOT NULL, volume REAL NOT NULL, PRIMARY KEY(shaft, kind, ref));
-- \u{41f}\u{43e}\u{442}\u{43e}\u{43b}\u{43e}\u{43a}: \u{43d}\u{438}\u{437} = \u{43e}\u{442}\u{43c}\u{435}\u{442}\u{43a}\u{430} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f} + height, \u{441}\u{43b}\u{43e}\u{438} slab_type (pos 0 \u{441}\u{432}\u{435}\u{440}\u{445}\u{443}); ring 0 — \u{43a}\u{43e}\u{43d}\u{442}\u{443}\u{440}, 1.. — \u{43e}\u{442}\u{432}\u{435}\u{440}\u{441}\u{442}\u{438}\u{44f}.
CREATE TABLE IF NOT EXISTS ceiling(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, stype INTEGER NOT NULL,
    height REAL NOT NULL);
CREATE TABLE IF NOT EXISTS ceiling_vertex(ceiling INTEGER NOT NULL, ring INTEGER NOT NULL, idx INTEGER NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, PRIMARY KEY(ceiling, ring, idx));
CREATE TABLE IF NOT EXISTS ceiling_qty(ceiling INTEGER NOT NULL, pos INTEGER NOT NULL, material TEXT NOT NULL,
    area REAL NOT NULL, volume REAL NOT NULL, PRIMARY KEY(ceiling, pos));
-- \u{41b}\u{435}\u{441}\u{442}\u{43d}\u{438}\u{446}\u{430}: kind straight|l|u, turn 1 — \u{432}\u{43b}\u{435}\u{432}\u{43e}; (x, y) — \u{441}\u{435}\u{440}\u{435}\u{434}\u{438}\u{43d}\u{430} \u{43f}\u{435}\u{440}\u{432}\u{43e}\u{433}\u{43e} \u{43f}\u{43e}\u{434}\u{441}\u{442}\u{443}\u{43f}\u{451}\u{43d}\u{43a}\u{430}, angle — \u{440}\u{430}\u{434};
-- tread NULL — \u{43f}\u{43e} \u{43f}\u{440}\u{430}\u{432}\u{438}\u{43b}\u{443} 2R+T; first NULL — \u{43f}\u{43e}\u{43f}\u{43e}\u{43b}\u{430}\u{43c}.
CREATE TABLE IF NOT EXISTS stair(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, top_level INTEGER NOT NULL,
    base_offset REAL NOT NULL, top_offset REAL NOT NULL, kind TEXT NOT NULL, turn INTEGER NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, angle REAL NOT NULL, width REAL NOT NULL, max_riser REAL NOT NULL,
    tread REAL, first INTEGER, gap REAL NOT NULL, landing REAL NOT NULL, thickness REAL NOT NULL,
    stringer_w REAL, stringer_h REAL, material TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS stair_qty(stair INTEGER PRIMARY KEY, risers INTEGER NOT NULL, treads INTEGER NOT NULL,
    riser REAL NOT NULL, tread REAL NOT NULL, run REAL NOT NULL, volume REAL NOT NULL,
    landing_volume REAL NOT NULL, stringer_volume REAL NOT NULL);
-- \u{41e}\u{433}\u{440}\u{430}\u{436}\u{434}\u{435}\u{43d}\u{438}\u{435}: \u{43f}\u{43e} \u{43a}\u{440}\u{430}\u{44e} \u{43b}\u{435}\u{441}\u{442}\u{43d}\u{438}\u{446}\u{44b} (stair, side 0 — \u{43b}\u{435}\u{432}\u{44b}\u{439}, 1 — \u{43f}\u{440}\u{430}\u{432}\u{44b}\u{439}) \u{438}\u{43b}\u{438} \u{43f}\u{43e} \u{44d}\u{441}\u{43a}\u{438}\u{437}\u{443} railing_vertex
-- (z \u{43e}\u{442} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f} level).
CREATE TABLE IF NOT EXISTS railing(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, stair INTEGER, side INTEGER,
    height REAL NOT NULL, post_spacing REAL NOT NULL, baluster_spacing REAL NOT NULL, post REAL NOT NULL,
    baluster REAL NOT NULL, rail_w REAL NOT NULL, rail_h REAL NOT NULL, material TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS railing_vertex(railing INTEGER NOT NULL, idx INTEGER NOT NULL, x REAL NOT NULL,
    y REAL NOT NULL, z REAL NOT NULL, PRIMARY KEY(railing, idx));
CREATE TABLE IF NOT EXISTS railing_qty(railing INTEGER PRIMARY KEY, length REAL NOT NULL, posts INTEGER NOT NULL,
    balusters INTEGER NOT NULL, volume REAL NOT NULL);
-- \u{41a}\u{43e}\u{43c}\u{43f}\u{43e}\u{43d}\u{435}\u{43d}\u{442}: def — \u{442}\u{435}\u{43a}\u{441}\u{442} \u{43e}\u{43f}\u{440}\u{435}\u{434}\u{435}\u{43b}\u{435}\u{43d}\u{438}\u{44f} \u{441}\u{435}\u{43c}\u{435}\u{439}\u{441}\u{442}\u{432}\u{430}, vals — \u{437}\u{43d}\u{430}\u{447}\u{435}\u{43d}\u{438}\u{44f} `Key=value, …`. \u{42d}\u{43a}\u{437}\u{435}\u{43c}\u{43f}\u{43b}\u{44f}\u{440} \u{431}\u{435}\u{437} \u{445}\u{43e}\u{437}\u{44f}\u{438}\u{43d}\u{430}:
-- (x, y) \u{432} \u{43f}\u{43b}\u{430}\u{43d}\u{435}, z \u{43e}\u{442} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f}; \u{441}\u{442}\u{435}\u{43d}\u{430} — x \u{432}\u{434}\u{43e}\u{43b}\u{44c} \u{43e}\u{441}\u{438}, y \u{432}\u{43b}\u{435}\u{432}\u{43e} \u{43e}\u{442} \u{43e}\u{441}\u{438}, z \u{43e}\u{442} \u{43d}\u{438}\u{437}\u{430} \u{441}\u{442}\u{435}\u{43d}\u{44b}, rotation \u{43e}\u{442}
-- \u{43d}\u{430}\u{43f}\u{440}\u{430}\u{432}\u{43b}\u{435}\u{43d}\u{438}\u{44f} \u{441}\u{442}\u{435}\u{43d}\u{44b}; \u{43f}\u{435}\u{440}\u{435}\u{43a}\u{440}\u{44b}\u{442}\u{438}\u{435} — z \u{43e}\u{442} \u{432}\u{435}\u{440}\u{445}\u{430}; \u{43f}\u{43e}\u{442}\u{43e}\u{43b}\u{43e}\u{43a} — z \u{43e}\u{442} \u{43d}\u{438}\u{437}\u{430}. mirror — \u{43e}\u{442}\u{440}\u{430}\u{436}\u{435}\u{43d}\u{438}\u{435} \u{43b}\u{43e}\u{43a}\u{430}\u{43b}\u{44c}\u{43d}\u{43e}\u{439} x.
CREATE TABLE IF NOT EXISTS comp_family(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    category TEXT NOT NULL, host TEXT NOT NULL, def TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS comp_type(id INTEGER PRIMARY KEY, family INTEGER NOT NULL, name TEXT NOT NULL,
    vals TEXT NOT NULL, UNIQUE(family, name));
CREATE TABLE IF NOT EXISTS comp(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, ctype INTEGER NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, z REAL NOT NULL, rotation REAL NOT NULL, mirror INTEGER NOT NULL,
    host_wall INTEGER, host_slab INTEGER, host_ceiling INTEGER, vals TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS comp_level ON comp(level);
CREATE INDEX IF NOT EXISTS comp_ctype ON comp(ctype);
CREATE INDEX IF NOT EXISTS comp_host_wall ON comp(host_wall);
CREATE INDEX IF NOT EXISTS comp_host_slab ON comp(host_slab);
CREATE INDEX IF NOT EXISTS comp_host_ceiling ON comp(host_ceiling);
-- \u{421}\u{435}\u{442}\u{438}: kind — \u{442}\u{438}\u{43f} \u{441}\u{438}\u{441}\u{442}\u{435}\u{43c}\u{44b} (supply_air … power), color — 0xRRGGBB. \u{423}\u{447}\u{430}\u{441}\u{442}\u{43e}\u{43a}: domain duct|pipe|
-- cable_tray|conduit, shape round|rect|oval (w = \u{434}\u{438}\u{430}\u{43c}\u{435}\u{442}\u{440} \u{443} \u{43a}\u{440}\u{443}\u{433}\u{430}), points «x y z;…» \u{432} \u{43c}\u{438}\u{440}\u{435}.
CREATE TABLE IF NOT EXISTS mep_system(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL, color INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS mep_run(id INTEGER PRIMARY KEY, system INTEGER NOT NULL, level INTEGER NOT NULL,
    domain TEXT NOT NULL, shape TEXT NOT NULL, w REAL NOT NULL, h REAL NOT NULL, wall REAL NOT NULL,
    insulation REAL NOT NULL, lining REAL NOT NULL, material TEXT, dn INTEGER, bend REAL,
    slope REAL NOT NULL, points TEXT NOT NULL, cap0 INTEGER NOT NULL, cap1 INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS mep_run_system ON mep_run(system);
CREATE INDEX IF NOT EXISTS mep_run_level ON mep_run(level);
-- \u{41e}\u{433}\u{440}\u{430}\u{43d}\u{438}\u{447}\u{435}\u{43d}\u{438}\u{435}: kind lock_len|lock_dist|eq|align|parallel|perp|align_grid|lock_dist_grid|level_gap|top_level;
-- walls — id \u{443}\u{447}\u{430}\u{441}\u{442}\u{43d}\u{438}\u{43a}\u{43e}\u{432} \u{447}\u{435}\u{440}\u{435}\u{437} \u{43f}\u{440}\u{43e}\u{431}\u{435}\u{43b} (\u{441}\u{442}\u{435}\u{43d}\u{44b}; \u{443} *_grid \u{432}\u{442}\u{43e}\u{440}\u{43e}\u{439} — \u{43e}\u{441}\u{44c}, \u{443} level_gap — \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{438}, \u{443} top_level \u{432}\u{442}\u{43e}\u{440}\u{43e}\u{439} — \u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}).
CREATE TABLE IF NOT EXISTS constraint_rule(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, walls TEXT NOT NULL,
    value REAL);
-- \u{413}\u{440}\u{430}\u{43d}\u{438}\u{446}\u{44b} \u{438} \u{43c}\u{430}\u{440}\u{43a}\u{438} \u{43e}\u{441}\u{438}/\u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f}: view '' — \u{43c}\u{43e}\u{434}\u{435}\u{43b}\u{44c}\u{43d}\u{44b}\u{435} (3D) \u{434}\u{43b}\u{44f} \u{443}\u{440}\u{43e}\u{432}\u{43d}\u{44f} (x \u{432}\u{434}\u{43e}\u{43b}\u{44c} \u{440}\u{430}\u{437}\u{440}\u{435}\u{437}\u{430}), \u{438}\u{43d}\u{430}\u{447}\u{435} — 2D
-- \u{432} \u{432}\u{438}\u{434}\u{435} view (\u{43a}\u{43b}\u{44e}\u{447} «plan:<\u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}>» / «section:<id>»); bubbles: \u{431}\u{438}\u{442} 0 — \u{43d}\u{430}\u{447}\u{430}\u{43b}\u{43e}, \u{431}\u{438}\u{442} 1 — \u{43a}\u{43e}\u{43d}\u{435}\u{446}.
CREATE TABLE IF NOT EXISTS datum_extent(id INTEGER PRIMARY KEY, view TEXT NOT NULL, kind TEXT NOT NULL,
    ref INTEGER NOT NULL, ax REAL NOT NULL, ay REAL NOT NULL, bx REAL NOT NULL, by REAL NOT NULL,
    bubbles INTEGER NOT NULL, UNIQUE(view, kind, ref));
-- \u{41f}\u{440}\u{43e}\u{438}\u{437}\u{432}\u{43e}\u{434}\u{43d}\u{430}\u{44f}: \u{43c}\u{438}\u{440}\u{43e}\u{432}\u{43e}\u{435} \u{43f}\u{43e}\u{43b}\u{43e}\u{436}\u{435}\u{43d}\u{438}\u{435}, \u{43e}\u{431}\u{44a}\u{451}\u{43c}, \u{43f}\u{43b}\u{43e}\u{449}\u{430}\u{434}\u{44c} \u{43f}\u{43e}\u{432}\u{435}\u{440}\u{445}\u{43d}\u{43e}\u{441}\u{442}\u{438}, \u{433}\u{430}\u{431}\u{430}\u{440}\u{438}\u{442}; \u{43d}\u{435}\u{442} \u{441}\u{442}\u{440}\u{43e}\u{43a}\u{438} — \u{43d}\u{435} \u{441}\u{442}\u{440}\u{43e}\u{438}\u{442}\u{441}\u{44f}.
CREATE TABLE IF NOT EXISTS comp_qty(comp INTEGER PRIMARY KEY, wx REAL NOT NULL, wy REAL NOT NULL,
    wz REAL NOT NULL, wrot REAL NOT NULL, wmirror INTEGER NOT NULL, volume REAL NOT NULL, surface REAL NOT NULL,
    x0 REAL NOT NULL, y0 REAL NOT NULL, z0 REAL NOT NULL, x1 REAL NOT NULL, y1 REAL NOT NULL, z1 REAL NOT NULL);
";

fn has_table(db: &Connection, name: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

/// Whether `table` has a column `col` (false for a missing table).
pub(crate) fn has_col(db: &Connection, table: &str, col: &str) -> Result<bool, Error> {
    let n: i64 = db
        .prepare_cached("SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2")?
        .query_row([table, col], |r| r.get(0))?;
    Ok(n > 0)
}

pub(crate) fn stored_version(db: &Connection) -> Result<Option<i64>, Error> {
    if !has_table(db, "meta")? {
        return Ok(None);
    }
    Ok(db
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// File version: from `meta`; a file without `meta` is 1 (journal without `inv`) or 2; an empty file is 0.
pub(crate) fn detect(db: &Connection) -> Result<i64, Error> {
    if let Some(v) = stored_version(db)? {
        return Ok(v);
    }
    if !has_table(db, "op")? {
        return Ok(0);
    }
    let inv: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('op') WHERE name='inv'",
        [],
        |r| r.get(0),
    )?;
    Ok(if inv > 0 { 2 } else { 1 })
}

/// Open: newer than supported is rejected without changing the file; older is migrated in one transaction.
/// `path` is the file on disk: before migrating from an old version, `<path>.bak-v<old>` is written.
pub(crate) fn upgrade(db: &mut Connection, path: Option<&str>) -> Result<(), Error> {
    let found = detect(db)?;
    if found > VERSION {
        return Err(Error::NewerVersion {
            found,
            supported: VERSION,
        });
    }
    if found < 0 {
        return Err(Error::Invalid(format!("bad schema_version {found}")));
    }
    if let Some(p) = path.filter(|_| (1..VERSION).contains(&found)) {
        crate::file::backup_before_migration(db, p, found)?;
    }
    db.execute_batch("PRAGMA journal_mode=WAL;")?;
    // immediate: a deferred read-then-write fails at once when another writer commits in between
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if found == 1 {
        migrate_1_to_2(&tx)?;
    }
    tx.execute_batch(SCHEMA)?;
    add_style_columns(&tx)?;
    add_swing_columns(&tx)?;
    add_arc_columns(&tx)?;
    add_ext_ref_source(&tx)?;
    add_top_profile(&tx)?;
    add_wall_lean(&tx)?;
    add_slab_slope(&tx)?;
    add_mullion_lines(&tx)?;
    add_top_roof(&tx)?;
    add_top_auto(&tx)?;
    add_top_core(&tx)?;
    migrate_19_masses(&tx)?;
    crate::sketch::ensure(&tx)?;
    crate::boundary_link::ensure(&tx)?;
    add_sheets(&tx)?;
    crate::sheets::output::migrate(&tx)?;
    add_v22(&tx)?;
    tx.execute_batch(crate::solids::DDL)?;
    crate::solids::migrate(&tx)?;
    crate::stairs::migrate(&tx)?;
    migrate_54_freeform(&tx)?;
    crate::meshcache::ensure(&tx)?;
    crate::topo::migrate(&tx)?;
    crate::struct_ext::migrate(&tx)?;
    crate::roof_edge::ensure(&tx)?;
    crate::family_edit::migrate(&tx)?;
    crate::groups::migrate(&tx)?;
    add_room_outline(&tx)?;
    crate::source::migrate(&tx)?;
    crate::links::migrate(&tx)?;
    crate::rebar::migrate(&tx)?;
    crate::ramps::migrate(&tx)?;
    crate::zones::migrate(&tx)?;
    crate::mep::migrate_ports(&tx)?;
    crate::annot_text::migrate(&tx)?;
    crate::view_gfx::migrate(&tx)?;
    crate::drawq::migrate_legacy_range(&tx)?;
    // last: scans every table with a `material` column
    crate::materials::migrate(&tx, (1..VERSION).contains(&found))?;
    crate::options::migrate(&tx)?;
    crate::ai_audit::migrate(&tx)?;
    crate::journal::migrate(&tx)?;
    crate::ai_option::migrate(&tx)?;
    crate::mass::bool_link::migrate(&tx)?;
    crate::groups::migrate_locks(&tx)?;
    crate::calc_store::migrate(&tx)?;
    crate::topics::migrate(&tx)?;
    crate::value_src::migrate(&tx)?;
    crate::recipe::migrate(&tx)?;
    crate::mep::migrate_circuits(&tx)?;
    crate::depth::migrate(&tx)?;
    crate::underlay_store::ensure(&tx)?;
    crate::opening_comp::migrate(&tx)?;
    crate::joints::migrate(&tx)?;
    crate::measure::migrate(&tx)?;
    tx.execute(
        // no write when unchanged: a copy opened for a staged Add stays row for row its source
        "INSERT INTO meta(key, value) VALUES ('schema_version', ?1) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE value<>excluded.value",
        [VERSION],
    )?;
    tx.commit()?;
    Ok(())
}

/// Schema 54: freeform patches, curtain systems and shells on them, curved walls, grid shells.
/// `IF NOT EXISTS` throughout, so the step is idempotent and order-free among its neighbours.
fn migrate_54_freeform(db: &Connection) -> Result<(), Error> {
    for ddl in [
        crate::freeform::DDL,
        crate::panel_system::DDL,
        crate::shell_elem::DDL,
        crate::wall_spline::DDL,
    ] {
        db.execute_batch(ddl)?;
    }
    crate::gridshell::migrate(db)
}

/// Sheet tables are created only if missing: the merge order of adjacent-version migrations does not matter.
fn add_sheets(db: &Connection) -> Result<(), Error> {
    for (name, ddl) in crate::sheets::DDL {
        if !has_table(db, name)? {
            db.execute_batch(ddl)?;
        }
    }
    Ok(())
}

/// Style on an entity: NULL = "by layer". Additive, no version change: old files are topped up on open.
const STYLE_TABLES: [&str; 5] = ["d_line", "d_polyline", "d_arc", "d_text", "d_insert"];
const STYLE_COLS: [(&str, &str); 4] = [
    ("color", "INTEGER"),
    ("true_color", "INTEGER"),
    ("linetype", "TEXT"),
    ("lineweight", "INTEGER"),
];
/// MTEXT is a d_text with a non-empty `mtext_width` (frame width, 0 = no wrapping).
const TEXT_COLS: [(&str, &str); 2] = [("mtext_width", "REAL"), ("attach", "INTEGER")];

fn add_style_columns(db: &Connection) -> Result<(), Error> {
    let mut want: Vec<(&str, &str, &str)> = vec![];
    for t in STYLE_TABLES {
        want.extend(STYLE_COLS.iter().map(|c| (t, c.0, c.1)));
    }
    want.extend(TEXT_COLS.iter().map(|c| ("d_text", c.0, c.1)));
    // Hatch boundary: the pattern and the id of the first polyline of the boundary group.
    want.extend([
        ("d_polyline", "hatch", "TEXT"),
        ("d_polyline", "hatch_id", "INTEGER"),
        ("d_layer", "true_color", "INTEGER"),
        // Owner block of an insert nested in a block definition; NULL = model space.
        ("d_insert", "block", "INTEGER"),
    ]);
    for (t, c, ty) in want {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
            [t, c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE {t} ADD COLUMN {c} {ty}"))?;
        }
    }
    Ok(())
}

/// Door swing side/hinges (schema 4): 0 = left of the axis, hinges at the start.
fn add_swing_columns(db: &Connection) -> Result<(), Error> {
    for c in ["swing_side", "swing_hinge"] {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('opening') WHERE name=?1",
            [c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!(
                "ALTER TABLE opening ADD COLUMN {c} INTEGER NOT NULL DEFAULT 0"
            ))?;
        }
    }
    Ok(())
}

/// Arc wall axis (schema 5): axis_kind 0 is straight, 1 is an arc; bulge = tan(angle/4).
fn add_arc_columns(db: &Connection) -> Result<(), Error> {
    for (c, ty) in [("axis_kind", "INTEGER"), ("bulge", "REAL")] {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('wall') WHERE name=?1",
            [c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!(
                "ALTER TABLE wall ADD COLUMN {c} {ty} NOT NULL DEFAULT 0"
            ))?;
        }
    }
    Ok(())
}

/// Schema 52: `wall.lean`, `wall.top_scale` (`ddc_geom::wall_lean::Lean`).
fn add_wall_lean(db: &Connection) -> Result<(), Error> {
    for (c, def) in [("lean", "0"), ("top_scale", "1")] {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('wall') WHERE name=?1",
            [c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE wall ADD COLUMN {c} REAL NOT NULL DEFAULT {def}"))?;
        }
    }
    Ok(())
}

/// Schema 8: `wall.top_profile` is the text `u h;u h;...` (see `walls::top_profile_text`).
fn add_top_profile(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('wall') WHERE name='top_profile'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch("ALTER TABLE wall ADD COLUMN top_profile TEXT")?;
    }
    Ok(())
}

/// Schema 9: `slab.slope` is `x y gx gy` (top = offset at point (x, y), slopes along x and y), NULL is flat.
fn add_slab_slope(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('slab') WHERE name='slope'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch("ALTER TABLE slab ADD COLUMN slope TEXT")?;
    }
    Ok(())
}

/// Schema 10: `curtain_wall.mullion_lines` (see `curtain::LineSections`), `curtain_wall.top_profile`.
fn add_mullion_lines(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('curtain_wall') WHERE name='mullion_lines'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch("ALTER TABLE curtain_wall ADD COLUMN mullion_lines TEXT")?;
    }
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('curtain_wall') WHERE name='top_profile'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch("ALTER TABLE curtain_wall ADD COLUMN top_profile TEXT")?;
    }
    Ok(())
}

/// `room.outline` — imported boundary (JSON points): the area when walls do not close the room.
fn add_room_outline(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('room') WHERE name='outline'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch("ALTER TABLE room ADD COLUMN outline TEXT")?;
    }
    Ok(())
}

/// Schema 13: `wall.top_roof` is the roof whose underside the wall top is attached to (NULL means none).
fn add_top_roof(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('wall') WHERE name='top_roof'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch("ALTER TABLE wall ADD COLUMN top_roof INTEGER")?;
    }
    Ok(())
}

/// `wall.top_auto`: 0 — top set by hand or not attached, 1 — attached by the rule (the roof drives it),
/// 2 — the user released the attachment, the rule no longer touches this wall.
fn add_top_auto(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('wall') WHERE name='top_auto'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch("ALTER TABLE wall ADD COLUMN top_auto INTEGER NOT NULL DEFAULT 0")?;
        // walls of a file made before the rule are left alone by it: trimming is the user's choice
        db.execute_batch("UPDATE wall SET top_auto=2")?;
    }
    Ok(())
}

/// `wall.top_core`: the wall core top at the underside of the roof's bearing layer (NULL means same as `top_profile`).
fn add_top_core(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('wall') WHERE name='top_core'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch("ALTER TABLE wall ADD COLUMN top_core TEXT")?;
    }
    Ok(())
}

/// Schema 19: mass tables (if `SCHEMA` has not created them yet) and binding columns from early
/// files without a role, face label and "orphan" flag. Idempotent.
fn migrate_19_masses(db: &Connection) -> Result<(), Error> {
    if ![
        "mass",
        "mass_paint",
        "mass_host",
        "geom_join",
        "mass_qty",
        "mass_cache",
        "join_cache",
    ]
    .iter()
    .all(|t| has_table(db, t).unwrap_or(false))
    {
        db.execute_batch(MASS_SCHEMA)?;
    }
    for (c, t) in [
        ("role", "TEXT NOT NULL DEFAULT 'face'"),
        ("level", "INTEGER"),
        ("sig", "TEXT"),
        ("orphan", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('mass_host') WHERE name=?1",
            [c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE mass_host ADD COLUMN {c} {t}"))?;
        }
    }
    Ok(())
}

/// Schema 22: phases and project parameters. Tables and columns by presence (the file may have gone
/// through migrations 18-21 in any order); standard phases only into an empty `phase`.
const V22_TABLES: [(&str, &str); 6] = [
    (
        "phase",
        "CREATE TABLE phase(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, seq INTEGER NOT NULL,          key TEXT)",
    ),
    (
        "elem_phase",
        "CREATE TABLE elem_phase(kind TEXT NOT NULL, ref INTEGER NOT NULL, created INTEGER NOT NULL,          demolished INTEGER, PRIMARY KEY(kind, ref))",
    ),
    (
        "view_phase",
        "CREATE TABLE view_phase(vkind TEXT NOT NULL, ref INTEGER NOT NULL, phase INTEGER NOT NULL,          filter TEXT NOT NULL, PRIMARY KEY(vkind, ref))",
    ),
    (
        "pparam",
        "CREATE TABLE pparam(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, ptype TEXT NOT NULL,          per_type INTEGER NOT NULL, unit TEXT NOT NULL, grp TEXT NOT NULL, guid TEXT)",
    ),
    (
        "pparam_cat",
        "CREATE TABLE pparam_cat(param INTEGER NOT NULL, category TEXT NOT NULL,          PRIMARY KEY(param, category))",
    ),
    (
        "pparam_val",
        "CREATE TABLE pparam_val(param INTEGER NOT NULL, kind TEXT NOT NULL, ref INTEGER NOT NULL,          value, PRIMARY KEY(param, kind, ref))",
    ),
];

fn add_v22(db: &Connection) -> Result<(), Error> {
    let new_phases = !has_table(db, "phase")?;
    for (t, ddl) in V22_TABLES {
        if !has_table(db, t)? {
            db.execute_batch(ddl)?;
        }
    }
    // columns added after the first release of the 22 tables are added here too
    for (t, c, ty) in [("pparam", "guid", "TEXT"), ("phase", "key", "TEXT")] {
        let n: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2",
            [t, c],
            |r| r.get(0),
        )?;
        if n == 0 {
            db.execute_batch(&format!("ALTER TABLE {t} ADD COLUMN {c} {ty}"))?;
        }
    }
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS pparam_val_ref ON pparam_val(kind, ref);
         CREATE INDEX IF NOT EXISTS elem_phase_demolished ON elem_phase(demolished);",
    )?;
    // seeded only with the table: an empty phase list the user left is kept on open
    if new_phases {
        crate::phases::seed(db)?;
    }
    Ok(())
}

/// Schema 7: guid namespace per source. rowid is preserved (undo watermarks in the journal).
fn add_ext_ref_source(db: &Connection) -> Result<(), Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('ext_ref') WHERE name='source'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        db.execute_batch(
            "CREATE TABLE ext_ref_v7(source TEXT NOT NULL DEFAULT '', guid TEXT NOT NULL,
                 kind TEXT NOT NULL, ref INTEGER NOT NULL, PRIMARY KEY(source, guid));
             INSERT INTO ext_ref_v7(rowid, source, guid, kind, ref)
                 SELECT rowid, '', guid, kind, ref FROM ext_ref;
             DROP TABLE ext_ref;
             ALTER TABLE ext_ref_v7 RENAME TO ext_ref;",
        )?;
    }
    Ok(())
}

/// v1 stored only `create`/`set` with old/new in the journal; inverse commands are rebuilt from them.
/// An unknown operation kind is an error with seq (a silent no-op on undo would be worse).
fn migrate_1_to_2(db: &Connection) -> Result<(), Error> {
    db.execute_batch("ALTER TABLE op ADD COLUMN inv TEXT NOT NULL DEFAULT '[]'")?;
    type Row = (i64, i64, String, Option<String>, Option<f64>);
    let rows: Vec<Row> = {
        let mut st = db.prepare("SELECT seq, element, kind, name, old FROM op")?;
        let r = st.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        r.collect::<Result<_, _>>()?
    };
    for (seq, element, kind, name, old) in rows {
        let inv = match (kind.as_str(), name, old) {
            ("create", _, _) => serde_json::json!([
                ["DELETE FROM param WHERE element=?1", [element]],
                ["DELETE FROM element WHERE id=?1", [element]]
            ]),
            ("set", Some(n), Some(o)) => serde_json::json!([[
                "UPDATE param SET value=?3 WHERE element=?1 AND name=?2",
                [element, n, o]
            ]]),
            ("set", Some(n), None) => serde_json::json!([[
                "DELETE FROM param WHERE element=?1 AND name=?2",
                [element, n]
            ]]),
            _ => {
                return Err(Error::Invalid(format!(
                    "migration 1->2: op {seq} kind '{kind}' has no known inverse"
                )))
            }
        };
        db.execute(
            "UPDATE op SET inv=?2 WHERE seq=?1",
            params![seq, inv.to_string()],
        )?;
    }
    Ok(())
}
