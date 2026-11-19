// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Integrity: dangling-reference checks and deletion of reference rows with orphans forbidden.
//! Child "parts" (`OWNED`) are deleted together with their owner; all other references block deletion.

use rusqlite::Connection;

use crate::ops::Inv;
use crate::{Error, Model};

/// (child table, column, parent table)
const REFS: [(&str, &str, &str); 104] = [
    ("wall", "level", "level"),
    ("curtain_wall", "level", "level"),
    ("curtain_panel", "cw", "curtain_wall"),
    ("room_sep", "level", "level"),
    ("slab", "level", "level"),
    ("room", "level", "level"),
    ("slab", "stype", "slab_type"),
    ("slab_type_layer", "stype", "slab_type"),
    ("slab_vertex", "slab", "slab"),
    ("slab_qty", "slab", "slab"),
    ("room_qty", "room", "room"),
    ("wall", "wtype", "wall_type"),
    ("wall_type_layer", "wtype", "wall_type"),
    ("opening", "wall", "wall"),
    ("wall_qty", "wall", "wall"),
    ("param", "element", "element"),
    ("d_line", "layer", "d_layer"),
    ("d_polyline", "layer", "d_layer"),
    ("d_arc", "layer", "d_layer"),
    ("d_text", "layer", "d_layer"),
    ("d_insert", "layer", "d_layer"),
    ("d_line", "block", "d_block"),
    ("d_polyline", "block", "d_block"),
    ("d_arc", "block", "d_block"),
    ("d_text", "block", "d_block"),
    ("d_insert", "def", "d_block"),
    ("d_insert", "block", "d_block"),
    ("d_vertex", "polyline", "d_polyline"),
    ("d_attr", "insert_id", "d_insert"),
    ("d_attdef", "block", "d_block"),
    ("col", "level", "level"),
    ("col", "top_level", "level"),
    ("col", "mtype", "member_type"),
    ("beam", "level", "level"),
    ("beam", "mtype", "member_type"),
    ("roof", "level", "level"),
    ("roof", "stype", "slab_type"),
    ("roof_vertex", "roof", "roof"),
    ("roof_qty", "roof", "roof"),
    ("wall", "top_roof", "roof"),
    ("grid", "base_level", "level"),
    ("grid", "top_level", "level"),
    ("shaft", "level", "level"),
    ("shaft", "top_level", "level"),
    ("shaft_vertex", "shaft", "shaft"),
    ("shaft_cut", "shaft", "shaft"),
    ("ceiling", "level", "level"),
    ("ceiling", "stype", "slab_type"),
    ("ceiling_vertex", "ceiling", "ceiling"),
    ("ceiling_qty", "ceiling", "ceiling"),
    ("ramp", "level", "level"),
    ("ramp", "top_level", "level"),
    ("zone_room", "zone", "zone"),
    ("zone_room", "room", "room"),
    ("stair", "level", "level"),
    ("mep_run", "level", "level"),
    ("mep_run", "system", "mep_system"),
    ("stair", "top_level", "level"),
    ("stair_qty", "stair", "stair"),
    ("railing", "level", "level"),
    ("railing", "stair", "stair"),
    ("railing_vertex", "railing", "railing"),
    ("railing_qty", "railing", "railing"),
    ("comp_type", "family", "comp_family"),
    ("comp", "level", "level"),
    ("comp", "ctype", "comp_type"),
    ("comp", "host_wall", "wall"),
    ("comp", "host_slab", "slab"),
    ("comp", "host_ceiling", "ceiling"),
    ("comp_qty", "comp", "comp"),
    ("mass", "level", "level"),
    ("slab_shape", "slab", "slab"),
    ("wall_profile", "wall", "wall"),
    ("wall_attach", "wall", "wall"),
    ("wall_attach", "slab", "slab"),
    ("wall_attach_level", "wall", "wall"),
    ("wall_attach_level", "level", "level"),
    ("iso_footing", "level", "level"),
    ("strip_footing", "level", "level"),
    ("raft", "level", "level"),
    ("raft_vertex", "raft", "raft"),
    ("brace", "level", "level"),
    ("brace", "mtype", "member_type"),
    ("truss", "level", "level"),
    ("truss", "chord_type", "member_type"),
    ("truss", "web_type", "member_type"),
    ("beam_system", "level", "level"),
    ("beam_system", "mtype", "member_type"),
    ("beam_system_vertex", "sys", "beam_system"),
    ("skylight", "roof", "roof"),
    ("soffit", "roof", "roof"),
    ("roof_ext", "roof", "roof"),
    ("roof_ext", "join_roof", "roof"),
    ("roof_drain", "taper", "roof_taper"),
    ("roof_taper", "level", "level"),
    ("roof_taper", "slab", "slab"),
    ("sweep", "level", "level"),
    ("solid", "level", "level"),
    ("solid_host", "solid", "solid"),
    ("solid_qty", "solid", "solid"),
    ("solid_cut", "solid", "solid"),
    // terrain: points, subregions and pads are deleted together with it
    ("topo_point", "topo", "toposolid"),
    ("topo_region", "topo", "toposolid"),
    ("topo_pad", "topo", "toposolid"),
];

/// All declared references (child table, column, parent): a merge remaps their ids.
pub(crate) fn all_refs() -> Vec<(&'static str, &'static str, &'static str)> {
    REFS.into_iter()
        .chain(crate::sheets::REFS)
        .chain(crate::calc_store::REFS)
        .collect()
}

/// Children that are deleted together with their owner and therefore do not block deletion.
const OWNED: [&str; 39] = [
    "zone_room",
    "topo_point",
    "topo_region",
    "topo_pad",
    "solid_host",
    "solid_qty",
    "solid_cut",
    "skylight",
    "soffit",
    "roof_ext",
    "roof_drain",
    "slab_shape",
    "wall_attach_level",
    "wall_profile",
    "wall_attach",
    "raft_vertex",
    "beam_system_vertex",
    "comp_qty",
    "shaft_vertex",
    "shaft_cut",
    "ceiling_vertex",
    "ceiling_qty",
    "stair_qty",
    "railing_vertex",
    "railing_qty",
    "roof_vertex",
    "roof_qty",
    "curtain_panel",
    "wall_type_layer",
    "slab_type_layer",
    "slab_vertex",
    "slab_qty",
    "room_qty",
    "opening",
    "wall_qty",
    "param",
    "d_vertex",
    "d_attr",
    "d_attdef",
];

/// A detected violation: table, row rowid, description.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub table: String,
    pub id: i64,
    pub problem: String,
}

/// The table's first reference to a parent: (column, parent, whether the row is a part of the parent).
pub(crate) fn parent_ref(table: &str) -> Option<(&'static str, &'static str, bool)> {
    let owned = OWNED
        .iter()
        .chain(crate::sheets::OWNED.iter())
        .any(|t| *t == table);
    REFS.into_iter()
        .chain(crate::sheets::REFS)
        .find(|r| r.0 == table)
        .map(|r| (r.1, r.2, owned))
}

impl Model {
    /// Finds orphans and dangling references; an empty list means the model is consistent. Changes nothing.
    pub fn check_integrity(&self) -> Result<Vec<Issue>, Error> {
        let db = &self.db;
        let mut out = vec![];
        for (child, col, parent) in REFS
            .into_iter()
            .chain(crate::sheets::REFS)
            .chain(crate::rebar::REFS)
            .chain(crate::calc_store::REFS)
        {
            let mut st = db.prepare(&format!(
                "SELECT rowid, {col} FROM {child} WHERE {col} IS NOT NULL \
                 AND {col} NOT IN (SELECT id FROM {parent}) ORDER BY rowid"
            ))?;
            let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            for r in rows {
                let (id, to) = r?;
                out.push(Issue {
                    table: child.into(),
                    id,
                    problem: format!("{col} -> missing {parent} {to}"),
                });
            }
        }
        let extra = [
            (
                "wall_type",
                "SELECT id FROM wall_type WHERE id NOT IN (SELECT wtype FROM wall_type_layer)",
                "wall type without layers",
            ),
            (
                "wall",
                "SELECT id FROM wall WHERE id NOT IN (SELECT wall FROM wall_qty)",
                "wall without quantities",
            ),
            (
                "slab_type",
                "SELECT id FROM slab_type WHERE id NOT IN (SELECT stype FROM slab_type_layer)",
                "slab type without layers",
            ),
            (
                "slab",
                "SELECT id FROM slab WHERE id NOT IN (SELECT slab FROM slab_qty)",
                "slab without quantities",
            ),
            (
                "slab",
                "SELECT id FROM slab WHERE id NOT IN (SELECT slab FROM slab_vertex WHERE ring=0)",
                "slab without boundary",
            ),
            (
                "comp",
                "SELECT id FROM comp WHERE id NOT IN (SELECT comp FROM comp_qty)",
                "component cannot be built (bad values or host)",
            ),
            (
                "datum_extent",
                "SELECT id FROM datum_extent WHERE (kind='grid' AND ref NOT IN (SELECT id FROM grid))                  OR (kind='level' AND ref NOT IN (SELECT id FROM level))",
                "extents of a missing grid/level",
            ),
        ];
        for (id, problem) in crate::constraints::dangling(db)? {
            out.push(Issue {
                table: "constraint_rule".into(),
                id,
                problem,
            });
        }
        out.extend(crate::structure::issues(db)?);
        out.extend(crate::roof_edge::issues(db)?);
        out.extend(crate::sketch::orphan_sketches(db)?);
        out.extend(crate::annot::tag_issues(db)?);
        out.extend(crate::groups::issues(db)?);
        out.extend(crate::sheets::issues(db)?);
        out.extend(crate::phases::issues(db)?);
        out.extend(crate::params::issues(db)?);
        out.extend(crate::struct_ext::issues(db)?);
        out.extend(crate::rebar::issues(db)?);
        for (table, id, problem) in crate::annot_text::broken_rows(db)? {
            out.push(Issue {
                table: table.into(),
                id,
                problem: problem.into(),
            });
        }
        for (id, why) in crate::slabs::invalid_slabs(db)? {
            out.push(Issue {
                table: "slab".into(),
                id,
                problem: format!("invalid boundary, quantities stale: {why}"),
            });
        }
        for (id, problem) in crate::walls::openings_above_top(db)? {
            out.push(Issue {
                table: "opening".into(),
                id,
                problem,
            });
        }
        for (table, sql, problem) in extra {
            let mut st = db.prepare(sql)?;
            let ids: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            out.extend(ids.into_iter().map(|id| Issue {
                table: table.into(),
                id,
                problem: problem.into(),
            }));
        }
        out.extend(crate::wall_edit::issues(db)?);
        out.extend(crate::options::issues(db)?);
        Ok(out)
    }

    /// Test check: panics with the list of violations.
    #[cfg(test)]
    pub(crate) fn assert_integrity(&self) {
        let v = self.check_integrity().unwrap();
        assert!(v.is_empty(), "integrity: {v:?}");
    }

    /// Deletes a level together with its rules and per-view boundaries (undo restores everything).
    pub fn delete_level(&mut self, id: i64) -> Result<(), Error> {
        // walls whose top hangs on this level lose the attachment in the same step
        self.delete_level_with(id, move |db, inv| {
            let inv = inv.restore(db, "wall_attach_level", "level", id)?;
            db.execute("DELETE FROM wall_attach_level WHERE level=?1", [id])?;
            Ok(inv)
        })
    }

    /// Walls whose top is attached to level `id`: they lose it when the level is deleted.
    pub fn level_attach_walls(&self, id: i64) -> Result<usize, Error> {
        let n: i64 = self.db.query_row(
            "SELECT count(*) FROM wall_attach_level WHERE level=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Rows that block deleting level `id`: (table, id) pairs, empty when the level is free.
    pub fn level_contents(&self, id: i64) -> Result<Vec<(&'static str, i64)>, Error> {
        referrers(&self.db, "level", id)
    }

    /// Pinned elements among the contents of level `id`: moving them to another level is refused.
    pub fn level_pinned(&self, id: i64) -> Result<Vec<(&'static str, i64)>, Error> {
        pinned_on(&self.db, id)
    }

    /// Moves everything on level `id` to level `to` and deletes `id`: one journal entry, one undo.
    /// Refused while any of the contents is pinned.
    pub fn delete_level_into(&mut self, id: i64, to: i64) -> Result<(), Error> {
        if id == to {
            return Err(Error::Invalid("the target level is the deleted one".into()));
        }
        self.delete_level_with(id, move |db, mut inv| {
            let pinned = pinned_on(db, id)?;
            if !pinned.is_empty() {
                let names: Vec<String> = pinned.iter().map(|(t, i)| format!("{t} {i}")).collect();
                return Err(Error::Invalid(format!(
                    "pinned elements stay on the level: {}",
                    names.join(", ")
                )));
            }
            let n: i64 = db.query_row("SELECT count(*) FROM level WHERE id=?1", [to], |r| r.get(0))?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such level {to}")));
            }
            let z = |l: i64| -> Result<f64, Error> {
                Ok(db.query_row("SELECT elevation FROM level WHERE id=?1", [l], |r| r.get(0))?)
            };
            let delta = z(id)? - z(to)?;
            let refs: Vec<(&str, &str)> = REFS
                .into_iter()
                .chain(crate::sheets::REFS)
                .chain(crate::rebar::REFS)
                .filter(|(c, _, p)| {
                    *p == "level" && !OWNED.contains(c) && !crate::sheets::OWNED.contains(c)
                })
                .map(|(c, col, _)| (c, col))
                .collect();
            // every snapshot before any update: a row on both base and top level is taken unchanged
            for (child, col) in &refs {
                inv = inv.restore(db, child, col, id)?;
            }
            inv = inv.restore(db, "wall_attach_level", "level", id)?;
            let free_rails: Vec<i64> = db
                .prepare("SELECT id FROM railing WHERE level=?1 AND stair IS NULL")?
                .query_map([id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for r in &free_rails {
                inv = inv.restore(db, "railing_vertex", "railing", *r)?;
            }
            // heights stored relative to the level: shift them so absolute heights stay put
            if delta.abs() > 1e-12 {
                for r in &free_rails {
                    db.execute(
                        "UPDATE railing_vertex SET z=z+?1 WHERE railing=?2",
                        rusqlite::params![delta, r],
                    )?;
                }
                let masses: Vec<(i64, String)> = db
                    .prepare("SELECT id, form FROM mass WHERE level=?1")?
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                for (m, form) in masses {
                    let base = crate::Form::from_json(&serde_json::from_str(&form)?)?;
                    let moved = crate::Form::Move {
                        base: Box::new(base),
                        d: (0.0, 0.0, delta),
                    };
                    db.execute(
                        "UPDATE mass SET form=?1 WHERE id=?2",
                        rusqlite::params![moved.to_json().to_string(), m],
                    )?;
                }
            }
            db.execute(
                "UPDATE wall_attach_level SET level=?1, offset=offset+?3 WHERE level=?2",
                rusqlite::params![to, id, delta],
            )?;
            for (child, col) in refs {
                let cols: Vec<String> = db
                    .prepare(&format!("SELECT name FROM pragma_table_info('{child}')"))?
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let has = |c: &str| cols.iter().any(|x| x == c);
                let mut shifted: Vec<&str> = vec![];
                if col == "level" {
                    for c in ["base_offset", "offset", "az", "bz"] {
                        if has(c) {
                            shifted.push(c);
                        }
                    }
                    // tables without a top level keep top_offset over the level (footings);
                    // with one, top_offset is over the top level or over the base and stays
                    if has("top_offset") && !has("top_level") {
                        shifted.push("top_offset");
                    }
                    match child {
                        "comp" | "beam_system" => shifted.push("z"),
                        "ceiling" => shifted.push("height"),
                        _ => {}
                    }
                } else if col == "top_level" && has("top_offset") {
                    shifted.push("top_offset");
                }
                let sql = |set: String| format!("UPDATE {child} SET {col}=?1{set} WHERE {col}=?2");
                if shifted.is_empty() {
                    db.execute(&sql(String::new()), rusqlite::params![to, id])?;
                } else {
                    let set: String = shifted.iter().map(|c| format!(", {c}={c}+?3")).collect();
                    db.execute(&sql(set), rusqlite::params![to, id, delta])?;
                }
            }
            Ok(inv)
        })
    }

    fn delete_level_with(
        &mut self,
        id: i64,
        pre: impl Fn(&Connection, Inv) -> Result<Inv, Error>,
    ) -> Result<(), Error> {
        // the level's plan on sheets goes away together with the level
        self.delete_ref_pre(
            "level",
            "level_delete",
            id,
            &[("viewport", "level")],
            pre,
            |db, inv| {
                let inv =
                    crate::constraints::drop_for_ref(db, inv, crate::constraints::Ref::Level(id))?;
                let inv = crate::datum_view::drop_for_datum(db, inv, crate::DatumKind::Level, id)?;
                crate::annot_text::drop_view(db, inv, crate::AnnoView::Plan(id))
            },
        )
    }

    pub fn delete_wall_type(&mut self, id: i64) -> Result<(), Error> {
        // lazy wall-edit tables are read only when present: creating them here is not
        // journalled, so undo would leave a model that differs from the one before
        let has = |t: &str| crate::rebar::has_table(&self.db, t);
        let used: i64 = if has("stack_part")? {
            self.db.query_row(
                "SELECT count(*) FROM stack_part WHERE wtype=?1",
                [id],
                |r| r.get(0),
            )?
        } else {
            0
        };
        if used > 0 {
            return Err(Error::Invalid(format!(
                "wall type {id} is a part of a stacked wall type"
            )));
        }
        let mut owned: Vec<(&str, &str)> = vec![("wall_type_layer", "wtype")];
        for t in ["wall_region", "wall_wrap", "wall_layer_flag"] {
            if has(t)? {
                owned.push((t, "wtype"));
            }
        }
        self.delete_ref_with(
            "wall_type",
            "wall_type_delete",
            id,
            &owned,
            |db, inv| crate::purge::drop_links(db, inv, "wall_type", id),
        )
    }

    pub fn delete_layer(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("d_layer", "layer_delete", id, &[])
    }

    pub fn delete_block(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("d_block", "block_delete", id, &[("d_attdef", "block")])
    }

    /// Deletes a reference row and its `owned` parts; while it is referenced, returns `Referenced`.
    pub(crate) fn delete_ref(
        &mut self,
        table: &'static str,
        kind: &str,
        id: i64,
        owned: &[(&str, &str)],
    ) -> Result<(), Error> {
        self.delete_ref_with(table, kind, id, owned, |_, inv| Ok(inv))
    }

    /// Same, but `cascade` removes in the same operation rows that have no foreign key (rules, boundaries).
    pub(crate) fn delete_ref_with(
        &mut self,
        table: &'static str,
        kind: &str,
        id: i64,
        owned: &[(&str, &str)],
        cascade: impl Fn(&Connection, Inv) -> Result<Inv, Error>,
    ) -> Result<(), Error> {
        self.delete_ref_pre(table, kind, id, owned, |_, inv| Ok(inv), cascade)
    }

    /// Like `delete_ref_with`, with `pre` run (and its snapshots taken) before referrers are checked.
    fn delete_ref_pre(
        &mut self,
        table: &'static str,
        kind: &str,
        id: i64,
        owned: &[(&str, &str)],
        pre: impl Fn(&Connection, Inv) -> Result<Inv, Error>,
        cascade: impl Fn(&Connection, Inv) -> Result<Inv, Error>,
    ) -> Result<(), Error> {
        self.op(kind, |db| {
            let n: i64 = db.query_row(
                &format!("SELECT count(*) FROM {table} WHERE id=?1"),
                [id],
                |r| r.get(0),
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such {table} {id}")));
            }
            let inv = pre(db, Inv::default().restore(db, table, "id", id)?)?;
            let by = referrers(db, table, id)?;
            if !by.is_empty() {
                return Err(Error::Referenced {
                    kind: table,
                    id,
                    by,
                });
            }
            let mut inv = inv;
            for (ct, col) in owned {
                inv = inv.restore(db, ct, col, id)?;
                db.execute(&format!("DELETE FROM {ct} WHERE {col}=?1"), [id])?;
            }
            inv = crate::sketch::drop_owned(db, inv, table, id)?;
            let inv = cascade(db, inv)?;
            db.execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])?;
            Ok(((), id, inv))
        })
    }
}

/// Rows blocking deletion of `parent id`: (table, id), in REFS order, then by id.
fn referrers(db: &Connection, parent: &str, id: i64) -> Result<Vec<(&'static str, i64)>, Error> {
    let mut out = vec![];
    for (child, col, p) in REFS
        .into_iter()
        .chain(crate::sheets::REFS)
        .chain(crate::rebar::REFS)
    {
        if p != parent || OWNED.contains(&child) || crate::sheets::OWNED.contains(&child) {
            continue;
        }
        let mut st = db.prepare(&format!(
            "SELECT id FROM {child} WHERE {col}=?1 ORDER BY id"
        ))?;
        let ids: Vec<i64> = st
            .query_map([id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        out.extend(ids.into_iter().map(|i| (child, i)));
    }
    Ok(out)
}

fn pinned_on(db: &Connection, level: i64) -> Result<Vec<(&'static str, i64)>, Error> {
    if !crate::rebar::has_table(db, "el_pin")? {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for (t, i) in referrers(db, "level", level)? {
        if !out.contains(&(t, i)) && crate::groups::is_pinned_in(db, t, i)? {
            out.push((t, i));
        }
    }
    Ok(out)
}

/// Ids of `parent` rows that some non-owned child row points at (purge: what is in use).
pub(crate) fn used_ids(
    db: &Connection,
    parent: &str,
) -> Result<std::collections::HashSet<i64>, Error> {
    let mut out = std::collections::HashSet::new();
    for (child, col, p) in REFS.into_iter().chain(crate::sheets::REFS) {
        if p != parent || OWNED.contains(&child) || crate::sheets::OWNED.contains(&child) {
            continue;
        }
        let mut st = db.prepare(&format!(
            "SELECT DISTINCT {col} FROM {child} WHERE {col} IS NOT NULL"
        ))?;
        for id in st.query_map([], |r| r.get::<_, i64>(0))? {
            out.insert(id?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Entity, LayerSpec, OpeningSpec, WallSpec};
    use rusqlite::types::Value as V;

    fn layers() -> Vec<LayerSpec> {
        [("brick", 0.2), ("insulation", 0.1)]
            .iter()
            .map(|(m, t)| LayerSpec {
                material: m.to_string(),
                thickness: *t,
            })
            .collect()
    }

    /// All table rows as text: a before/after comparison without tolerances.
    fn dump(m: &Model, table: &str) -> Vec<String> {
        let mut st = m
            .sql()
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let n = st.column_count();
        st.query_map([], |r| {
            Ok((0..n)
                .map(|i| format!("{:?}", r.get::<_, V>(i).unwrap()))
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
    }

    fn qty(m: &Model, wall: i64) -> (f64, f64) {
        m.sql()
            .query_row(
                "SELECT SUM(plan_area), SUM(volume) FROM wall_qty WHERE wall=?1",
                [wall],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    fn spec(a: (f64, f64), b: (f64, f64), level: i64, t: i64) -> WallSpec {
        WallSpec {
            a,
            b,
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        }
    }

    #[test]
    fn level_moves_contents_then_deletes_in_one_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, _, _) = l_plus_t(&mut m);
        let lv: i64 = m
            .sql()
            .query_row("SELECT level FROM wall WHERE id=?1", [a], |r| r.get(0))
            .unwrap();
        let to = m.add_level("L2", 0.0).unwrap();
        assert_eq!(m.level_contents(lv).unwrap().len(), 3);
        let top = |m: &Model, w: i64| -> (f64, f64) {
            m.sql()
                .query_row(
                    "SELECT l.elevation+w.base_offset, l.elevation+w.base_offset+w.height                      FROM wall w JOIN level l ON l.id=w.level WHERE w.id=?1",
                    [w],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
        };
        m.update_level(lv, "L1", 3.0).unwrap();
        let abs_before = top(&m, a);
        assert!(m.delete_level_into(lv, lv).is_err());
        let names = ["level", "wall"];
        let before = tables(&m, &names);
        m.delete_level_into(lv, to).unwrap();
        assert!(m.level_contents(lv).unwrap().is_empty());
        assert_eq!(count(&m, "wall"), 3);
        assert_eq!(top(&m, a), abs_before, "absolute base and top are kept");
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(tables(&m, &names), before);
    }

    /// L-shaped corner A(0,0)-(6,0) + B(6,0)-(6,4) and a T-junction C(3,0)-(3,3) at the middle of A.
    fn l_plus_t(m: &mut Model) -> (i64, i64, i64) {
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("ext", &layers()).unwrap();
        let a = m.add_wall(&spec((0., 0.), (6., 0.), lv, t)).unwrap();
        let b = m.add_wall(&spec((6., 0.), (6., 4.), lv, t)).unwrap();
        let c = m.add_wall(&spec((3., 0.), (3., 3.), lv, t)).unwrap();
        m.assert_integrity();
        (a, b, c)
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    fn count(m: &Model, table: &str) -> i64 {
        m.sql()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn tables(m: &Model, names: &[&str]) -> Vec<Vec<String>> {
        // row order of the derived table depends on the history of partial recomputes
        names
            .iter()
            .map(|t| {
                let mut rows = dump(m, t);
                rows.sort();
                rows
            })
            .collect()
    }

    fn op_spec(kind: &str, along: f64, width: f64, sill: f64, height: f64) -> OpeningSpec {
        OpeningSpec {
            along,
            width,
            sill,
            height,
            kind: kind.into(),
        }
    }

    #[test]
    fn delete_wall_takes_openings_and_undo_restores_exactly() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, _, _) = l_plus_t(&mut m);
        m.add_opening(a, &op_spec("door", 1.0, 0.9, 0.0, 2.1))
            .unwrap();
        m.add_opening(a, &op_spec("window", 4.0, 1.2, 0.9, 1.5))
            .unwrap();
        let names = ["wall", "opening", "wall_qty"];
        let before = tables(&m, &names);
        assert_eq!((count(&m, "wall"), count(&m, "opening")), (3, 2));
        // Wall A: 6x0.3x3 = 5.4 m3 minus a 3.69 m2 deduction over layers 0.2/0.1 -> 5.4 - 1.107 = 4.293.
        near(qty(&m, a).1, 4.293);

        m.delete_wall(a).unwrap();
        assert_eq!((count(&m, "wall"), count(&m, "opening")), (2, 0));
        assert_eq!(count(&m, "wall_qty"), 4);
        m.assert_integrity();

        m.undo().unwrap();
        assert_eq!(tables(&m, &names), before);
        near(qty(&m, a).1, 4.293);
        m.assert_integrity();
    }

    #[test]
    fn referenced_directories_refuse_deletion_naming_ids() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, b, c) = l_plus_t(&mut m);
        let e = m.delete_wall_type(1).unwrap_err();
        assert!(
            matches!(&e, Error::Referenced { kind: "wall_type", id: 1, by }
            if by == &vec![("wall", a), ("wall", b), ("wall", c)])
        );
        assert_eq!(
            e.to_string(),
            format!("wall_type 1 is still referenced by wall {a}, wall {b}, wall {c}")
        );
        let e = m.delete_level(1).unwrap_err().to_string();
        assert_eq!(
            e,
            format!("level 1 is still referenced by wall {a}, wall {b}, wall {c}")
        );
        assert_eq!((count(&m, "level"), count(&m, "wall_type")), (1, 1));
        assert_eq!(count(&m, "op"), 5);

        // Without references: a two-layer type and a level are deleted; undo restores the rows exactly.
        let t2 = m.add_wall_type("tmp", &layers()).unwrap();
        let lv2 = m.add_level("L2", 3.0).unwrap();
        let names = ["wall_type", "wall_type_layer", "level"];
        let before = tables(&m, &names);
        m.delete_wall_type(t2).unwrap();
        m.delete_level(lv2).unwrap();
        assert_eq!(
            (count(&m, "wall_type"), count(&m, "wall_type_layer")),
            (1, 2)
        );
        m.assert_integrity();
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(tables(&m, &names), before);
        // The type is in use by a wall again: set_wall_type to t2, then deletion is forbidden.
        m.set_wall_type(c, t2).unwrap();
        let e = m.delete_wall_type(t2).unwrap_err().to_string();
        assert_eq!(e, format!("wall_type {t2} is still referenced by wall {c}"));
        assert!(matches!(m.delete_level(999), Err(Error::Invalid(_))));
    }

    #[test]
    fn referenced_layer_and_block_refuse_deletion_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_layer("A", 7, "CONTINUOUS", 0.25).unwrap();
        let l2 = m.add_layer("B", 1, "CONTINUOUS", 0.25).unwrap();
        let blk = m
            .add_block("DOOR", (0., 0.), &[("MARK", "D", (0., 0.), 0.2)])
            .unwrap();
        let line = m.add_line(l1, None, (0., 0.), (1., 0.)).unwrap();
        let inner = m.add_line(l2, Some(blk), (0., 0.), (0.9, 0.)).unwrap();
        let ins = m.add_insert(l1, blk, (3., 4.), (1., 1.), 0.0, &[]).unwrap();
        m.assert_integrity();

        let e = m.delete_layer(l1).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("d_layer {l1} is still referenced by d_line {line}, d_insert {ins}")
        );
        let e = m.delete_layer(l2).unwrap_err().to_string();
        assert_eq!(
            e,
            format!("d_layer {l2} is still referenced by d_line {inner}")
        );
        let e = m.delete_block(blk).unwrap_err().to_string();
        assert_eq!(
            e,
            format!("d_block {blk} is still referenced by d_line {inner}, d_insert {ins}")
        );
        assert_eq!((count(&m, "d_layer"), count(&m, "d_block")), (2, 1));

        m.delete_entity(Entity::Insert, ins).unwrap();
        m.delete_entity(Entity::Line, inner).unwrap();
        let names = ["d_block", "d_attdef"];
        let before = tables(&m, &names);
        m.delete_block(blk).unwrap();
        assert_eq!((count(&m, "d_block"), count(&m, "d_attdef")), (0, 0));
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(tables(&m, &names), before);
        m.delete_block(blk).unwrap();
        m.delete_layer(l2).unwrap();
        assert_eq!(count(&m, "d_layer"), 1);
        m.assert_integrity();
    }

    #[test]
    fn integrity_finds_orphans() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, _, _) = l_plus_t(&mut m);
        let db = m.sql();
        db.execute(
            "INSERT INTO opening(id,wall,along,width,sill,height,kind) VALUES (50, 77, 1, 1, 0, 1, 'door')",
            [],
        )
        .unwrap();
        db.execute("UPDATE wall SET level=9 WHERE id=?1", [a])
            .unwrap();
        db.execute(
            "INSERT INTO d_line(layer,block,x1,y1,x2,y2) VALUES (4,NULL,0,0,1,1)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO wall_type(id,name) VALUES (8,'empty')", [])
            .unwrap();
        let got: Vec<(String, i64, String)> = m
            .check_integrity()
            .unwrap()
            .into_iter()
            .map(|i| (i.table, i.id, i.problem))
            .collect();
        let s = |t: &str, id, p: &str| (t.to_string(), id, p.to_string());
        assert_eq!(
            got,
            vec![
                s("wall", a, "level -> missing level 9"),
                s("opening", 50, "wall -> missing wall 77"),
                s("d_line", 1, "layer -> missing d_layer 4"),
                s("wall_type", 8, "wall type without layers"),
            ]
        );
    }

    #[test]
    fn split_keeps_neighbour_joins_in_l_plus_t_plan() {
        // Initially: A (0,0)-(6,0) 1.8 m2/5.4 m3; B (corner) 1.2/3.6; C (T-junction) 0.855/2.565.
        let cases = [
            // (t, left part of A, right part of A)
            (1.5, (0.45, 1.35), (1.35, 4.05)),
            (4.5, (1.35, 4.05), (0.45, 1.35)),
        ];
        for (t, left, right) in cases {
            let mut m = Model::open(":memory:").unwrap();
            let (a, b, c) = l_plus_t(&mut m);
            let (qa, qb, qc) = (qty(&m, a), qty(&m, b), qty(&m, c));
            near(qa.0, 1.8);
            near(qa.1, 5.4);
            near(qb.0, 1.2);
            near(qb.1, 3.6);
            near(qc.0, 0.855);
            near(qc.1, 2.565);
            let names = ["wall", "opening", "wall_qty"];
            let before = tables(&m, &names);

            let n = m.split_wall(a, t).unwrap();
            m.assert_integrity();
            // Neighbours are bit-for-bit unchanged; the parts of A share the 5.4 volume in proportion to length.
            assert_eq!((qty(&m, b), qty(&m, c)), (qb, qc));
            near(qty(&m, a).0, left.0);
            near(qty(&m, a).1, left.1);
            near(qty(&m, n).0, right.0);
            near(qty(&m, n).1, right.1);
            // B ended at the end of A (6,0): it now touches only the second part.
            let ends: (f64, f64, f64, f64) = m
                .sql()
                .query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [n], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .unwrap();
            assert_eq!(ends, (t, 0.0, 6.0, 0.0));

            m.undo().unwrap();
            assert_eq!(tables(&m, &names), before);
            m.assert_integrity();
        }
    }

    #[test]
    fn split_at_t_junction_keeps_neighbours() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, b, c) = l_plus_t(&mut m);
        let (qb, qc) = (qty(&m, b), qty(&m, c));
        let n = m.split_wall(a, 3.0).unwrap();
        m.assert_integrity();
        assert_eq!((qty(&m, b), qty(&m, c)), (qb, qc));
        // A cut at the junction point is a straight continuation + T: the sum of the halves equals the original wall.
        near(qty(&m, a).0 + qty(&m, n).0, 1.8);
        near(qty(&m, a).1 + qty(&m, n).1, 5.4);
    }

    #[test]
    fn batch_undo_removes_children_of_existing_parents() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_layer("A", 7, "CONTINUOUS", 0.25).unwrap();
        let blk = m.add_block("DOOR", (0., 0.), &[]).unwrap();
        let ins = m.add_insert(l, blk, (0., 0.), (1., 1.), 0.0, &[]).unwrap();
        let pl = m
            .add_polyline(
                l,
                None,
                false,
                &[
                    crate::Vertex {
                        x: 0.,
                        y: 0.,
                        bulge: 0.,
                    },
                    crate::Vertex {
                        x: 1.,
                        y: 0.,
                        bulge: 0.,
                    },
                ],
            )
            .unwrap();
        m.batch("import", |db| {
            db.execute("INSERT INTO d_attdef VALUES (?1,'X','v',0,0,0.1)", [blk])?;
            db.execute("INSERT INTO d_attr VALUES (?1,'X','v')", [ins])?;
            db.execute("INSERT INTO d_vertex VALUES (?1,2,5,5,0)", [pl])?;
            Ok(())
        })
        .unwrap();
        assert_eq!(
            (
                count(&m, "d_attdef"),
                count(&m, "d_attr"),
                count(&m, "d_vertex")
            ),
            (1, 1, 3)
        );
        m.undo().unwrap();
        assert_eq!(
            (
                count(&m, "d_attdef"),
                count(&m, "d_attr"),
                count(&m, "d_vertex")
            ),
            (0, 0, 2)
        );
        assert_eq!((count(&m, "d_block"), count(&m, "d_insert")), (1, 1));
        m.assert_integrity();
    }

    fn temp(name: &str) -> String {
        let p = std::env::temp_dir().join(format!("ddc_t14_{name}_{}.ddc", std::process::id()));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
        }
        p.to_str().unwrap().to_string()
    }

    #[test]
    fn schema_version_is_stored_and_newer_files_are_rejected_untouched() {
        const V: i64 = crate::schema::VERSION;
        let p = temp("newer");
        {
            let m = Model::open(&p).unwrap();
            assert_eq!(m.schema_version().unwrap(), V);
        }
        let raw = rusqlite::Connection::open(&p).unwrap();
        let newer = crate::schema::VERSION + 1;
        raw.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [newer],
        )
        .unwrap();
        drop(raw);
        let e = Model::open(&p).err().unwrap();
        assert!(
            matches!(
                e,
                Error::NewerVersion { found, supported }
                    if found == newer && supported == crate::schema::VERSION
            ),
            "{e}"
        );
        assert_eq!(
            e.to_string(),
            format!(
                "file schema version {newer} is newer than supported {}",
                crate::schema::VERSION
            )
        );
        let raw = rusqlite::Connection::open(&p).unwrap();
        let stored: i64 = raw
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, newer);
    }

    #[test]
    fn version_2_file_gains_slabs_and_rooms() {
        let p = temp("v2");
        {
            let mut m = Model::open(&p).unwrap();
            let (a, _, _) = l_plus_t(&mut m);
            assert!(qty(&m, a).1 > 0.0);
        }
        let raw = rusqlite::Connection::open(&p).unwrap();
        raw.execute_batch(
            "DROP TABLE slab_type; DROP TABLE slab_type_layer; DROP TABLE slab; DROP TABLE slab_vertex;
             DROP TABLE slab_qty; DROP TABLE room; DROP TABLE room_qty;
             UPDATE meta SET value=2 WHERE key='schema_version';",
        )
        .unwrap();
        drop(raw);
        let mut m = Model::open(&p).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        assert_eq!(count(&m, "wall"), 3);
        m.add_room(&crate::RoomSpec {
            level: 1,
            name: "R".into(),
            number: "1".into(),
            point: (1., 1.),
            height: 3.0,
        })
        .unwrap();
        assert_eq!(count(&m, "room_qty"), 0);
        m.assert_integrity();
    }

    #[test]
    fn version_3_file_gains_swing_and_separators_additively() {
        let p = temp("v3");
        let door;
        {
            let mut m = Model::open(&p).unwrap();
            let (a, _, _) = l_plus_t(&mut m);
            door = m
                .add_opening(a, &op_spec("door", 1.0, 0.9, 0.0, 2.1))
                .unwrap();
        }
        let raw = rusqlite::Connection::open(&p).unwrap();
        raw.execute_batch(
            "ALTER TABLE opening DROP COLUMN swing_side; ALTER TABLE opening DROP COLUMN swing_hinge;
             DROP TABLE room_sep; UPDATE meta SET value=3 WHERE key='schema_version';",
        )
        .unwrap();
        drop(raw);
        let mut m = Model::open(&p).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        assert_eq!(m.opening_swing(door).unwrap(), crate::Swing::default());
        assert_eq!(count(&m, "wall"), 3);
        assert_eq!(count(&m, "opening"), 1);
        m.set_opening_swing(
            door,
            crate::Swing {
                side: true,
                hinge: false,
            },
        )
        .unwrap();
        m.undo().unwrap();
        assert_eq!(m.opening_swing(door).unwrap(), crate::Swing::default());
        m.assert_integrity();
    }

    #[test]
    fn version_1_file_migrates_journal_and_undo_works() {
        let p = temp("v1");
        {
            let raw = rusqlite::Connection::open(&p).unwrap();
            raw.execute_batch(
                "CREATE TABLE element(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, alive INTEGER NOT NULL DEFAULT 1);
                 CREATE TABLE param(element INTEGER NOT NULL, name TEXT NOT NULL, value REAL, PRIMARY KEY(element, name));
                 CREATE TABLE op(seq INTEGER PRIMARY KEY, element INTEGER NOT NULL, kind TEXT NOT NULL, name TEXT, old REAL, new REAL);
                 INSERT INTO element(id, kind) VALUES (1,'slab');
                 INSERT INTO param VALUES (1,'thickness',0.3);
                 INSERT INTO op(element,kind,name,old,new) VALUES (1,'create','slab',NULL,NULL);
                 INSERT INTO op(element,kind,name,old,new) VALUES (1,'set','thickness',NULL,0.2);
                 INSERT INTO op(element,kind,name,old,new) VALUES (1,'set','thickness',0.2,0.3);",
            )
            .unwrap();
        }
        let mut m = Model::open(&p).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        assert_eq!(m.get(1, "thickness"), Some(0.3));
        m.undo().unwrap();
        assert_eq!(m.get(1, "thickness"), Some(0.2));
        m.undo().unwrap();
        assert_eq!(m.get(1, "thickness"), None);
        m.undo().unwrap();
        assert_eq!(m.count().unwrap(), 0);
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
        // New operations are written and undone as usual; reopening migrates nothing.
        let w = m.create("wall").unwrap();
        m.set(w, "h", 3.0).unwrap();
        drop(m);
        let mut m = Model::open(&p).unwrap();
        m.undo().unwrap();
        assert_eq!(m.get(w, "h"), None);
        m.assert_integrity();
    }

    #[test]
    fn version_1_file_with_unknown_op_kind_is_rejected_by_seq() {
        let p = temp("v1bad");
        {
            let raw = rusqlite::Connection::open(&p).unwrap();
            raw.execute_batch(
                "CREATE TABLE op(seq INTEGER PRIMARY KEY, element INTEGER NOT NULL, kind TEXT NOT NULL, name TEXT, old REAL, new REAL);
                 INSERT INTO op(element,kind) VALUES (1,'mystery');",
            )
            .unwrap();
        }
        let e = Model::open(&p).err().unwrap().to_string();
        assert!(e.contains("op 1 kind 'mystery'"), "{e}");
        // The migration rolled back entirely: the file stayed at version 1.
        let raw = rusqlite::Connection::open(&p).unwrap();
        let n: i64 = raw
            .query_row(
                "SELECT count(*) FROM pragma_table_info('op') WHERE name='inv'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn delete_level_drops_attach_rows_undo_restores() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, b, _) = l_plus_t(&mut m);
        let top = m.add_level("Top", 3.0).unwrap();
        m.attach_wall_to_level(a, top, 0.0).unwrap();
        m.attach_wall_to_level(b, top, 0.1).unwrap();
        assert_eq!(m.level_attach_walls(top).unwrap(), 2);
        let names = ["level", "wall", "wall_attach_level"];
        let before = tables(&m, &names);
        m.delete_level(top).unwrap();
        assert_eq!(count(&m, "wall_attach_level"), 0, "no orphan rows");
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(tables(&m, &names), before);
        assert_eq!(m.level_attach_walls(top).unwrap(), 2);
    }

    fn run_on(m: &mut Model, lv: i64, z: f64) -> i64 {
        let sys = m
            .add_mep_system("CW", crate::SystemKind::ColdWater)
            .unwrap();
        let spec = crate::RunSpec::pipe(
            sys,
            lv,
            crate::PipeMaterial::Copper,
            15,
            vec![(0.0, 0.0, z), (2.0, 0.0, z)],
        )
        .unwrap();
        m.add_mep_run(&spec).unwrap()
    }

    #[test]
    fn delete_into_mep_run_keeps_absolute_z() {
        // mep_run points are world coordinates: only the level link moves, z is not shifted
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let to = m.add_level("L2", 0.0).unwrap();
        m.update_level(lv, "L1", 3.0).unwrap();
        let run = run_on(&mut m, lv, 3.5);
        let pts = m.mep_run(run).unwrap().points;
        m.delete_level_into(lv, to).unwrap();
        let r = m.mep_run(run).unwrap();
        assert_eq!(r.level, to);
        assert_eq!(r.points, pts);
    }

    #[test]
    fn delete_into_undo_restores_table_hash() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, _, _) = l_plus_t(&mut m);
        let lv: i64 = m
            .sql()
            .query_row("SELECT level FROM wall WHERE id=?1", [a], |r| r.get(0))
            .unwrap();
        let to = m.add_level("L2", 0.0).unwrap();
        m.update_level(lv, "L1", 3.0).unwrap();
        let top = m.add_level("Top", 6.0).unwrap();
        m.attach_wall_to_level(a, top, 0.0).unwrap();
        m.attach_wall_to_level(a, lv, 0.2).unwrap();
        run_on(&mut m, lv, 3.5);
        let names = ["level", "wall", "wall_attach_level", "mep_run", "mep_system"];
        let before = tables(&m, &names);
        m.delete_level_into(lv, to).unwrap();
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(tables(&m, &names), before);
    }

    #[test]
    fn delete_into_keeps_every_element_and_its_data() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, b, c) = l_plus_t(&mut m);
        let lv: i64 = m
            .sql()
            .query_row("SELECT level FROM wall WHERE id=?1", [a], |r| r.get(0))
            .unwrap();
        let to = m.add_level("L2", 4.0).unwrap();
        let geom = |m: &Model| -> Vec<String> {
            let mut st = m
                .sql()
                .prepare(
                    "SELECT w.id, w.ax, w.ay, w.bx, w.by, w.wtype, \
                     round(l.elevation + w.base_offset, 9), round(w.height, 9) \
                     FROM wall w JOIN level l ON l.id=w.level ORDER BY w.id",
                )
                .unwrap();
            let n = st.column_count();
            st.query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get::<_, V>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
        };
        let keep = ["opening", "wall_qty", "param", "wall_type"];
        let (g0, k0) = (geom(&m), tables(&m, &keep));
        m.delete_level_into(lv, to).unwrap();
        assert!(m.level_list().unwrap().iter().all(|l| l.0 != lv));
        assert_eq!(geom(&m), g0, "same walls, same absolute place and height");
        assert_eq!(tables(&m, &keep), k0, "owned rows and values are untouched");
        for w in [a, b, c] {
            let l: i64 = m
                .sql()
                .query_row("SELECT level FROM wall WHERE id=?1", [w], |r| r.get(0))
                .unwrap();
            assert_eq!(l, to);
        }
        m.assert_integrity();
    }

    #[test]
    fn delete_into_refuses_pinned_contents_and_writes_nothing() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, _, _) = l_plus_t(&mut m);
        let lv: i64 = m
            .sql()
            .query_row("SELECT level FROM wall WHERE id=?1", [a], |r| r.get(0))
            .unwrap();
        let to = m.add_level("L2", 3.0).unwrap();
        m.pin(&[("wall", a)]).unwrap();
        assert_eq!(m.level_pinned(lv).unwrap(), vec![("wall", a)]);
        let names = ["level", "wall", "el_pin"];
        let before = tables(&m, &names);
        let head = m.change_head();
        let e = m.delete_level_into(lv, to).unwrap_err().to_string();
        assert!(e.contains("pinned") && e.contains(&format!("wall {a}")), "{e}");
        assert_eq!(tables(&m, &names), before, "refused move writes nothing");
        assert_eq!(m.change_head(), head, "no undo step for a refusal");
    }
}
