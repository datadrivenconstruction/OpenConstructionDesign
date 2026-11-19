// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Clipboard and pinning registry for the tables of other modules. Each entry acts only when
//! its table exists in the model; otherwise it is silent. A new element table goes here or into
//! `EXCLUDED`, else the test `every_table_is_registered_or_excluded` fails.

use super::{Dict, Spec, Tx, B, ID, L};

const REBAR_HOSTS: &[(&str, &str)] = &[
    ("column", "col"),
    ("beam", "beam"),
    ("wall", "wall"),
    ("slab", "slab"),
    ("footing", "iso_footing"),
    ("raft", "raft"),
];
const ADJ_KINDS: &[(&str, &str)] = &[
    ("column", "col"),
    ("beam", "beam"),
    ("brace", "brace"),
    ("wall", "wall"),
    ("slab", "slab"),
];

/// Same order as `SPECS`: parent before child. Core elements are already read; attached
/// rows (`prop`, `sketch`, dimensions) come after this list.
pub(super) const LANE_SPECS: &[Spec] = &[
    Spec {
        t: "ramp",
        id: ID,
        levels: L,
        tops: &["top_level"],
        tx: &[("path", Tx::CommaPairs)],
        ..B
    },
    Spec {
        t: "zone",
        id: ID,
        ..B
    },
    Spec {
        t: "zone_room",
        owns: &[("room", "room")],
        refs: &[("zone", "zone")],
        ..B
    },
    Spec {
        t: "anno",
        id: ID,
        dicts: &[
            ("text_style", "anno_text_style"),
            ("line_style", "anno_line_style"),
        ],
        tx: &[("data", Tx::AnnoJson)],
        ..B
    },
    Spec {
        t: "mep_port",
        id: ID,
        owns: &[("comp", "comp")],
        refs: &[("stub", "mep_run")],
        dicts: &[("system", "mep_system")],
        ..B
    },
    // rebar coordinates are local to the host: unchanged when moved
    Spec {
        t: "rebar_set",
        id: ID,
        poly: Some(("host_kind", "host")),
        kinds: REBAR_HOSTS,
        dicts: &[("rtype", "rebar_type"), ("rtype2", "rebar_type")],
        ..B
    },
    Spec {
        t: "rebar_cover",
        id: ID,
        poly: Some(("host_kind", "host")),
        kinds: REBAR_HOSTS,
        ..B
    },
    Spec {
        t: "analytical_adj",
        id: ID,
        poly: Some(("kind", "ref")),
        kinds: ADJ_KINDS,
        ..B
    },
    // a spline wall comes with all its pieces (`wall_spline::group_items`)
    Spec {
        t: "wall_spline",
        id: ID,
        levels: L,
        dicts: &[("wtype", "wall_type")],
        tx: &[("axis", Tx::SplineAxis)],
        ..B
    },
    Spec {
        t: "wall_spline_piece",
        owns: &[("wall", "wall"), ("spline", "wall_spline")],
        ..B
    },
];

pub(super) const LANE_DICTS: &[Dict] = &[
    Dict {
        t: "rebar_type",
        key: &["name"],
        fks: &[],
        children: &[],
        create: true,
    },
    Dict {
        t: "anno_text_style",
        key: &["name"],
        fks: &[],
        children: &[],
        create: true,
    },
    Dict {
        t: "anno_line_style",
        key: &["name"],
        fks: &[],
        children: &[],
        create: true,
    },
];

/// (table, position columns): like `PIN_TABLES` in groups.rs.
pub(crate) const LANE_PINS: &[(&str, &[&str])] = &[
    ("ramp", &["path", "level"]),
    ("anno", &["data", "view_id"]),
    ("zone", &[]),
    ("rebar_set", &[]),
    ("mep_port", &[]),
];

/// Registry-driven deletion: (rows, reference column, table to delete from) when the reference is not the row's host.
pub(super) const DELETE_CASCADE: &[(&str, &str, &str)] = &[("zone_room", "zone", "zone")];

/// Tables excluded from the buffer, with the reason. An extra entry is harmless; a missing one is caught by a test.
pub(super) const EXCLUDED: &[(&str, &str)] = &[
    ("underlay", "placed drawings, written by the app before a save"),
    ("elec_circuit", "electrical circuits of a panel, rebuilt by the MEP tools"),
    ("elec_circuit_dev", "devices of a circuit, rebuilt by the MEP tools"),
    ("recipe", "a copied result is plain geometry; the script stays with the original"),
    ("recipe_part", "links of a recipe to what it built; not copied with an element"),
    ("steel_conn", "a connection is regenerated from its members; copies are plain solids"),
    ("steel_conn_part", "parts of a connection, rebuilt by regeneration"),
    ("gridshell", "a grid is one structure on its host surface; copies are plain solids"),
    ("gridshell_node", "nodes of a grid, owned by it and edited in place"),
    ("gridshell_bar", "member topology of a grid, regenerated with it"),
    ("beam_qty", "derived"),
    ("brace_qty", "derived"),
    ("ceiling_qty", "derived"),
    ("comp_qty", "derived"),
    ("mass_qty", "derived"),
    ("member_qty", "derived"),
    ("railing_qty", "derived"),
    ("roof_qty", "derived"),
    ("room_qty", "derived"),
    ("slab_qty", "derived"),
    ("solid_qty", "derived"),
    ("stair_qty", "derived"),
    ("struct_qty", "derived"),
    ("wall_qty", "derived"),
    ("dim_geo", "derived"),
    ("measure_geo", "derived"),
    ("measure_ref", "derived"),
    ("measure", "saved measurements stay with their view; anchors are element ids of this file"),
    ("join_cache", "derived"),
    ("mass_cache", "derived"),
    ("mesh_geo", "derived: mesh body cache"),
    ("mesh_proto", "derived: mesh body cache"),
    ("shaft_cut", "derived"),
    ("solid_cut", "derived"),
    ("solid_base", "derived: quantities before voids"),
    ("comp_thumb", "derived"),
    ("meta", "model service"),
    ("op", "undo log"),
    ("level", "mapped by elevation, never copied as a row"),
    ("el_group", "own paste path through ClipInst"),
    ("group_inst", "own paste path through ClipInst"),
    ("group_member", "own paste path through ClipInst"),
    ("el_pin", "a copy is not pinned"),
    ("param_lock", "a copy is not locked"),
    ("value_src", "a pasted value is a new edit, not the source's"),
    ("elem_depth", "a pasted element is worked out again by the run that deepens it"),
    ("depth_cfg", "project level"),
    ("element", "model service"),
    ("ext_ref", "a copy has no source identity"),
    ("ext_ref_v7", "a copy has no source identity"),
    ("src_base", "source provenance"),
    ("src_base_prev", "source provenance"),
    ("src_batch", "source provenance"),
    ("src_conflict", "source provenance"),
    ("src_file", "source provenance"),
    ("src_target", "source provenance"),
    ("ai_audit", "log of the assistant, not content"),
    ("ai_policy", "project level"),
    ("ai_option", "AI recipes"),
    ("ai_option_merge", "merged marks of AI options, not content"),
    ("ai_chat", "chat of the assistant, not content"),
    ("d_ltype", "linetype definitions, looked up by name; not copied with a layer"),
    ("mass_bool", "a copied result is a plain form; operands are not copied with it"),
    ("design_option", "project level"),
    ("design_option_set", "project level"),
    ("option_member", "project level"),
    ("view_option", "project level"),
    ("merge_map", "merge bookkeeping"),
    ("merge_peer", "merge bookkeeping"),
    ("src_param", "source provenance"),
    ("src_shape", "source provenance"),
    ("src_view", "source provenance"),
    ("model_link", "project level"),
    ("clash_item", "report over the model"),
    ("issue", "review log, not content"),
    ("issue_elem", "review log, not content"),
    ("issue_comment", "review log, not content"),
    ("issue_doc", "review log, not content"),
    ("struct_issue", "report over the model"),
    ("sheet", "project level"),
    ("sheet_rev", "project level"),
    ("sheet_schedule", "project level"),
    ("sheet_set", "project level"),
    ("sheet_set_item", "project level"),
    ("viewport", "project level"),
    ("view_phase", "project level"),
    ("param", "project level"),
    ("site_info", "project level"),
    ("fam_doc", "family editor document"),
    (
        "d_attdef",
        "belongs to a block definition, which moves by name",
    ),
    ("rebar_mark", "mark registry of the model"),
    ("anno_detail_def", "project level"),
    ("anno_view_scale", "project level"),
    ("anno_legend", "project level"),
    ("anno_legend_row", "project level"),
    ("anno_legend_place", "project level"),
    ("anno_drafting", "project level"),
    ("material", "carried by name in Clip.materials"),
    ("material_image", "carried by hash in Clip.materials"),
    ("mesh_material", "carried in Clip.materials, mesh ids remapped on paste"),
    ("boundary_link", "tied to host and wall ids, rebuilt from the sketch"),
    ("view_gfx", "project level"),
    ("view_template", "project level"),
    ("view_filter", "project level"),
    ("object_style", "project level"),
    ("project_field", "project level"),
    ("an_override", "analytical intent keyed by element ids, not copied"),
    ("load_case", "project level"),
    ("load", "belongs to a load case, keyed by element ids"),
    ("load_combo", "project level"),
    ("load_combo_term", "belongs to a load combination"),
    ("calc_settings", "project level"),
    ("calc_run", "analysis results, outside undo"),
    ("calc_blob", "analysis results, outside undo"),
];

#[cfg(test)]
mod tests {
    use super::super::{dict_specs, specs};
    use super::*;
    use crate::Model;

    fn tables(m: &Model) -> Vec<String> {
        let mut st = m
            .sql()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        let v = st.query_map([], |r| r.get::<_, String>(0)).unwrap();
        v.map(Result::unwrap).collect()
    }

    #[test]
    fn every_table_is_registered_or_excluded() {
        let m = Model::open(":memory:").unwrap();
        let missing: Vec<String> = tables(&m)
            .into_iter()
            .filter(|t| {
                !specs().any(|s| s.t == t)
                    && !dict_specs().any(|d| d.t == t || d.children.iter().any(|c| c.0 == t))
                    && !EXCLUDED.iter().any(|e| e.0 == t)
            })
            .collect();
        assert!(
            missing.is_empty(),
            "add to clip/reg.rs, as a spec or with a reason: {missing:?}"
        );
    }

    #[test]
    fn a_table_is_never_both_copied_and_excluded() {
        for (t, why) in EXCLUDED {
            assert!(!why.is_empty());
            assert!(!specs().any(|s| s.t == *t), "{t}");
            assert!(!dict_specs().any(|d| d.t == *t), "{t}");
        }
    }

    #[test]
    fn owners_and_references_are_registered_earlier() {
        let order: Vec<&str> = specs().map(|s| s.t).collect();
        for (i, s) in specs().enumerate() {
            for (_, p) in s.owns {
                let at = order.iter().position(|x| x == p);
                assert!(at.is_some_and(|a| a < i), "{} before its owner {p}", s.t);
            }
        }
    }
}
