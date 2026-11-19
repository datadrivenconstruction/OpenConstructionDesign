// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Which tables merge and how: main tables (`id`) hold elements; parts without `id` belong to an
//! element and are compared as a group; derived and service tables are not compared. A new table
//! with `id` joins the merge automatically; its references must be declared in `integrity::REFS`.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};

use crate::Error;

/// How a part is attached to an element.
pub(crate) enum Owner {
    /// (column, parent table).
    Col(&'static str, &'static str),
    /// (kind column holding the table name, id column).
    Poly(&'static str, &'static str),
}

/// Part of an element: `key` is the field by which the group merges row by row (otherwise as a whole).
pub(crate) struct Child {
    pub table: &'static str,
    pub owner: Owner,
    pub key: Option<&'static str>,
}

const fn col(table: &'static str, c: &'static str, parent: &'static str) -> Child {
    Child {
        table,
        owner: Owner::Col(c, parent),
        key: None,
    }
}

const fn poly(
    table: &'static str,
    kind: &'static str,
    id: &'static str,
    key: Option<&'static str>,
) -> Child {
    Child {
        table,
        owner: Owner::Poly(kind, id),
        key,
    }
}

pub(crate) static CHILDREN: &[Child] = &[
    col("wall_type_layer", "wtype", "wall_type"),
    col("slab_type_layer", "stype", "slab_type"),
    col("slab_vertex", "slab", "slab"),
    col("roof_vertex", "roof", "roof"),
    col("ceiling_vertex", "ceiling", "ceiling"),
    col("shaft_vertex", "shaft", "shaft"),
    col("railing_vertex", "railing", "railing"),
    col("raft_vertex", "raft", "raft"),
    col("beam_system_vertex", "sys", "beam_system"),
    col("curtain_panel", "cw", "curtain_wall"),
    col("d_vertex", "polyline", "d_polyline"),
    col("d_attdef", "block", "d_block"),
    col("d_attr", "insert_id", "d_insert"),
    col("param", "element", "element"),
    col("mass_paint", "mass", "mass"),
    // boolean link: `result` owns the row, `a` and `b` are mass ids (see EXTRA_REFS)
    col("mass_bool", "result", "mass"),
    col("slab_shape", "slab", "slab"),
    col("wall_profile", "wall", "wall"),
    col("wall_attach", "wall", "wall"),
    col("wall_attach_level", "wall", "wall"),
    col("roof_ext", "roof", "roof"),
    col("topo_point", "topo", "toposolid"),
    col("sheet_rev", "sheet", "sheet"),
    col("sheet_set_item", "sheet_set", "sheet_set"),
    col("pparam_cat", "param", "pparam"),
    col("solid_host", "solid", "solid"),
    col("load_combo_term", "combo", "load_combo"),
    poly("prop", "kind", "ref", Some("key")),
    poly("elem_phase", "kind", "ref", None),
    poly("pparam_val", "kind", "ref", Some("param")),
    poly("sketch", "host_kind", "host", None),
    poly("boundary_link", "host_kind", "host", Some("edge")),
    poly("mass_host", "kind", "ref", None),
    poly("ext_ref", "kind", "ref", Some("guid")),
    poly("option_member", "kind", "ref", None),
];

/// References missing from `integrity::REFS`.
const EXTRA_REFS: &[(&str, &str, &str)] = &[
    ("mass_host", "mass", "mass"),
    ("mass_bool", "a", "mass"),
    ("mass_bool", "b", "mass"),
    ("pparam_val", "param", "pparam"),
    ("elem_phase", "created", "phase"),
    ("elem_phase", "demolished", "phase"),
    ("design_option", "set_id", "design_option_set"),
    ("option_member", "option_id", "design_option"),
    // a linked outline edge may outlive its wall (state «source deleted»): merge-only ref
    ("boundary_link", "wall", "wall"),
];

/// "Kind + id" references in main tables and parts (kind is a table name).
const POLY_REFS: &[(&str, &str, &str)] = &[
    ("host_opening", "host_kind", "host"),
    ("solid_host", "kind", "ref"),
    ("datum_extent", "kind", "ref"),
];

fn skipped(n: &str) -> bool {
    n.starts_with("sqlite_")
        || n.starts_with("src_")
        || n.ends_with("_qty")
        || matches!(
            n,
            "op" | "meta"
                | "ai_chat"
                | "ai_audit"
                | "ai_policy"
                | "ai_option"
                | "ai_option_merge"
                | "dim_geo"
                | "measure"
                | "measure_geo"
                | "measure_ref"
                | "join_cache"
                | "mass_cache"
                | "mesh_proto"
                | "mesh_geo"
                | "solid_cut"
                | "solid_base"
                | "shaft_cut"
                | "underlay"
                | "struct_issue"
                | "comp_thumb"
                | "fam_doc"
                | "ext_ref_v7"
                | "geom_join"
                | "clash_item"
                | "issue"
                | "issue_elem"
                | "issue_comment"
                | "issue_doc"
                | "merge_peer"
                | "merge_map"
                | "view_phase"
                | "view_option"
                | "mesh_material"
                | "material_image"
                // per-project settings: a merge keeps this side's locks as they are
                | "param_lock"
                // who set a value and its check stay with this side's values
                | "value_src"
                // depth marks and the setting stay with this side's elements
                | "elem_depth"
                | "depth_cfg"
        )
        || crate::calc_store::is_run_table(n)
}

/// A column's reference to an element: its id is translated into this file's id space.
#[derive(Clone, Debug)]
pub(crate) enum RefCol {
    To { col: String, parent: String },
    Poly { kind: String, col: String },
}

pub(crate) struct Table {
    pub name: String,
    /// Columns without `id`.
    pub cols: Vec<String>,
    pub notnull: HashSet<String>,
    pub refs: Vec<RefCol>,
    pub uniq: Vec<Vec<String>>,
}

pub(crate) struct ChildTable {
    pub spec: &'static Child,
    pub cols: Vec<String>,
    pub refs: Vec<RefCol>,
}

impl ChildTable {
    pub fn owner_col(&self) -> &'static str {
        match self.spec.owner {
            Owner::Col(c, _) | Owner::Poly(_, c) => c,
        }
    }

    pub fn owned_by(&self, parent: &str) -> bool {
        match self.spec.owner {
            Owner::Col(_, p) => p == parent,
            Owner::Poly(..) => true,
        }
    }

    /// "Row of this kind" condition for a query with alias `alias`; empty for `Owner::Col`.
    pub fn kind_filter(&self, alias: &str, parent: &str) -> String {
        match self.spec.owner {
            Owner::Col(..) => String::new(),
            Owner::Poly(k, _) => format!(" AND {alias}.{}='{parent}'", q(k)),
        }
    }
}

pub(crate) struct Schema {
    /// Parents before children.
    pub tables: Vec<Table>,
    pub children: Vec<ChildTable>,
}

impl Schema {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.tables.iter().position(|t| t.name == name)
    }

    pub fn kids_of<'s>(
        &'s self,
        parent: &'s str,
    ) -> impl Iterator<Item = (usize, &'s ChildTable)> + 's {
        self.children
            .iter()
            .enumerate()
            .filter(move |(_, c)| c.owned_by(parent))
    }
}

pub(crate) fn q(c: &str) -> String {
    format!("\"{c}\"")
}

/// Expression: peer file id to this file's id in table `tbl`.
pub(crate) fn mapped(expr: &str, tbl: &str) -> String {
    format!(
        "COALESCE((SELECT x.mine FROM temp.mg_map x WHERE x.tbl='{tbl}' AND x.their={expr}),{expr})"
    )
}

/// Column value of a peer-file row in this file's id space.
pub(crate) fn canon(refs: &[RefCol], alias: &str, col: &str) -> String {
    let raw = format!("{alias}.{}", q(col));
    for r in refs {
        match r {
            RefCol::To { col: c, parent } if c == col => return mapped(&raw, parent),
            RefCol::Poly { kind, col: c } if c == col => {
                return format!(
                    "COALESCE((SELECT x.mine FROM temp.mg_map x WHERE x.tbl={alias}.{} AND x.their={raw}),{raw})",
                    q(kind)
                );
            }
            _ => {}
        }
    }
    raw
}

fn columns(db: &Connection, table: &str) -> Result<Vec<(String, i64, bool)>, Error> {
    let mut st = db.prepare("SELECT name, pk, \"notnull\" FROM pragma_table_info(?1, 'main')")?;
    let v = st
        .query_map([table], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)? != 0,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

fn column_names(db: &Connection, schema: &str, table: &str) -> Result<Vec<String>, Error> {
    let mut st = db.prepare("SELECT name FROM pragma_table_info(?1, ?2)")?;
    let v = st
        .query_map([table, schema], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

fn uniques(db: &Connection, table: &str) -> Result<Vec<Vec<String>>, Error> {
    let mut st = db.prepare(
        "SELECT name FROM pragma_index_list(?1, 'main') WHERE \"unique\"=1 AND origin<>'pk' AND partial=0",
    )?;
    let names: Vec<String> = st
        .query_map([table], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut out = vec![];
    for n in names {
        let mut c = db.prepare("SELECT name FROM pragma_index_info(?1, 'main') ORDER BY seqno")?;
        let cols: Vec<Option<String>> =
            c.query_map([&n], |r| r.get(0))?.collect::<Result<_, _>>()?;
        // expression indexes have no column names and cannot be matched here
        if let Some(cols) = cols.into_iter().collect::<Option<Vec<_>>>() {
            out.push(cols);
        }
    }
    Ok(out)
}

fn in_schema(db: &Connection, schema: &str, table: &str) -> Result<bool, Error> {
    Ok(db
        .query_row(
            &format!("SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name=?1"),
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Tables of this file that also exist in all `also` (attached files).
pub(crate) fn load(db: &Connection, also: &[&str]) -> Result<Schema, Error> {
    let names: Vec<String> = db
        .prepare("SELECT name FROM main.sqlite_master WHERE type='table' ORDER BY name")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut declared = crate::integrity::all_refs();
    declared.extend_from_slice(EXTRA_REFS);
    let present: HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut tables = vec![];
    let mut children = vec![];
    for n in &names {
        if skipped(n) {
            continue;
        }
        let mut everywhere = true;
        for s in also {
            everywhere &= in_schema(db, s, n)?;
        }
        if !everywhere {
            continue;
        }
        // a colleague's older file may lack columns added in place (material textures)
        let mut info = columns(db, n)?;
        for s in also {
            let theirs = column_names(db, s, n)?;
            info.retain(|c| theirs.contains(&c.0));
        }
        let has = |c: &str| info.iter().any(|x| x.0 == c);
        let mut refs: Vec<RefCol> = declared
            .iter()
            .filter(|(child, c, parent)| *child == n.as_str() && has(c) && present.contains(parent))
            .map(|(_, c, parent)| RefCol::To {
                col: (*c).to_string(),
                parent: (*parent).to_string(),
            })
            .collect();
        refs.extend(
            POLY_REFS
                .iter()
                .filter(|(child, k, c)| *child == n.as_str() && has(k) && has(c))
                .map(|(_, k, c)| RefCol::Poly {
                    kind: (*k).to_string(),
                    col: (*c).to_string(),
                }),
        );
        if let Some(spec) = CHILDREN.iter().find(|c| c.table == n.as_str()) {
            let known = |c: &str| {
                refs.iter().any(|r| match r {
                    RefCol::To { col, .. } | RefCol::Poly { col, .. } => col == c,
                })
            };
            let extra = match spec.owner {
                Owner::Col(c, parent) if !known(c) => Some(RefCol::To {
                    col: c.to_string(),
                    parent: parent.to_string(),
                }),
                Owner::Poly(k, c) if !known(c) => Some(RefCol::Poly {
                    kind: k.to_string(),
                    col: c.to_string(),
                }),
                _ => None,
            };
            refs.extend(extra);
            children.push(ChildTable {
                spec,
                cols: info.iter().map(|x| x.0.clone()).collect(),
                refs,
            });
        } else if info.iter().any(|x| x.0 == "id" && x.1 == 1) {
            tables.push(Table {
                name: n.clone(),
                cols: info
                    .iter()
                    .filter(|x| x.0 != "id")
                    .map(|x| x.0.clone())
                    .collect(),
                notnull: info.iter().filter(|x| x.2).map(|x| x.0.clone()).collect(),
                refs,
                uniq: uniques(db, n)?,
            });
        }
    }
    Ok(Schema {
        tables: parents_first(tables),
        children,
    })
}

/// "Parents before children" order; a reference cycle is appended as is.
fn parents_first(mut rest: Vec<Table>) -> Vec<Table> {
    let mut out: Vec<Table> = Vec::with_capacity(rest.len());
    while !rest.is_empty() {
        let names: HashSet<String> = rest.iter().map(|t| t.name.clone()).collect();
        let ready = rest.iter().position(|t| {
            !t.refs.iter().any(|r| match r {
                RefCol::To { parent, .. } => *parent != t.name && names.contains(parent),
                RefCol::Poly { .. } => false,
            })
        });
        out.push(rest.remove(ready.unwrap_or(0)));
    }
    out
}
