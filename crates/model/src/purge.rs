// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Purge unused: wall, slab and member types, component types and families nothing refers to.
//! `purge_candidates` is the preview; `purge_unused` removes the chosen items in one journal
//! entry, so one undo brings every row back byte for byte. The last type of a kind is kept
//! (tools need a current type), and so is a family with a type left out of the choice.
//! Unused material rows go too (layer text keeps its name and falls back to the defaults).

use std::collections::HashSet;

use rusqlite::types::Value;
use rusqlite::{params, Connection};

use crate::ops::Inv;
use crate::{Error, Model};

/// Tables a purge touches, in removal order (types before the families that hold them).
pub const PURGE_TABLES: [&str; 6] = [
    "wall_type",
    "slab_type",
    "member_type",
    "comp_type",
    "comp_family",
    "material",
];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PurgeItem {
    pub table: &'static str,
    pub id: i64,
    pub name: String,
    /// Family of a `comp_type`.
    pub family: Option<i64>,
}

/// Rows of an unused type that go with it.
fn owned(table: &str) -> &'static [(&'static str, &'static str)] {
    match table {
        "wall_type" => &[
            ("wall_type_layer", "wtype"),
            ("wall_region", "wtype"),
            ("wall_wrap", "wtype"),
            ("wall_layer_flag", "wtype"),
        ],
        "slab_type" => &[("slab_type_layer", "stype")],
        _ => &[],
    }
}

/// Removes the `ext_ref` and `prop` rows of a removed type. Unlike element meshes, a type keeps
/// no link: ids are reused, and a stale link would make re-import land on the new type with this id.
pub(crate) fn drop_links(
    db: &Connection,
    mut inv: Inv,
    table: &str,
    id: i64,
) -> Result<Inv, Error> {
    for t in ["ext_ref", "prop"] {
        let mut st = db.prepare(&format!(
            "SELECT rowid AS rowid_, * FROM {t} WHERE kind=?1 AND ref=?2 ORDER BY rowid"
        ))?;
        let mut cols: Vec<String> = st.column_names().map(|c| c.to_string()).collect();
        cols[0] = "rowid".into();
        let n = cols.len();
        let ph = (1..=n)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT OR REPLACE INTO {t}({}) VALUES ({ph})",
            cols.join(",")
        );
        let rows: Vec<Vec<Value>> = st
            .query_map(params![table, id], |r| {
                (0..n)
                    .map(|i| r.get::<_, Value>(i))
                    .collect::<Result<Vec<_>, _>>()
            })?
            .collect::<Result<_, _>>()?;
        for r in rows {
            inv = inv.sql(&sql, r);
        }
        db.execute(
            &format!("DELETE FROM {t} WHERE kind=?1 AND ref=?2"),
            params![table, id],
        )?;
    }
    Ok(inv)
}

fn has_table(db: &Connection, name: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

fn rows(db: &Connection, sql: &str) -> Result<Vec<(i64, String)>, Error> {
    let mut st = db.prepare(sql)?;
    let v = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// Unused rows of one reference table, lowest id first.
fn unused_in(
    db: &Connection,
    table: &'static str,
    keep_last: bool,
) -> Result<Vec<PurgeItem>, Error> {
    let all = rows(db, &format!("SELECT id, name FROM {table} ORDER BY id"))?;
    let mut used = crate::integrity::used_ids(db, table)?;
    if table == "wall_type" && has_table(db, "stack_part")? {
        let mut st = db.prepare("SELECT DISTINCT wtype FROM stack_part")?;
        for id in st.query_map([], |r| r.get::<_, i64>(0))? {
            used.insert(id?);
        }
    }
    let mut out: Vec<PurgeItem> = all
        .iter()
        .filter(|(id, _)| !used.contains(id))
        .map(|(id, name)| PurgeItem {
            table,
            id: *id,
            name: name.clone(),
            family: None,
        })
        .collect();
    // every type unused: keep the first so the next wall/slab/member can still be drawn
    if keep_last && out.len() == all.len() && !out.is_empty() {
        out.remove(0);
    }
    Ok(out)
}

pub(crate) fn candidates(db: &Connection) -> Result<Vec<PurgeItem>, Error> {
    let mut out = vec![];
    for t in ["wall_type", "slab_type", "member_type"] {
        out.extend(unused_in(db, t, true)?);
    }
    let mut st = db.prepare(
        "SELECT t.id, f.name || ': ' || t.name, t.family FROM comp_type t JOIN comp_family f ON f.id=t.family ORDER BY t.id",
    )?;
    let types: Vec<(i64, String, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let used = crate::integrity::used_ids(db, "comp_type")?;
    // a family goes when none of its types stays
    let alive: HashSet<i64> = types
        .iter()
        .filter(|t| used.contains(&t.0))
        .map(|t| t.2)
        .collect();
    out.extend(
        types
            .into_iter()
            .filter(|t| !used.contains(&t.0))
            .map(|(id, name, family)| PurgeItem {
                table: "comp_type",
                id,
                name,
                family: Some(family),
            }),
    );
    for (id, name) in rows(db, "SELECT id, name FROM comp_family ORDER BY id")? {
        if !alive.contains(&id) {
            out.push(PurgeItem {
                table: "comp_family",
                id,
                name,
                family: None,
            });
        }
    }
    if has_table(db, "material")? {
        for (id, name) in rows(db, "SELECT id, name FROM material ORDER BY id")? {
            if crate::materials::usage(db, &name)?.is_empty() {
                out.push(PurgeItem {
                    table: "material",
                    id,
                    name,
                    family: None,
                });
            }
        }
    }
    Ok(out)
}

impl Model {
    /// Preview: what a purge would remove now. Changes nothing.
    pub fn purge_candidates(&self) -> Result<Vec<PurgeItem>, Error> {
        crate::wall_edit::ensure(&self.db)?;
        candidates(&self.db)
    }

    /// What `purge_unused(items)` would remove: `items` without the families that keep a type
    /// left out of `items`. Empty means the purge would write nothing.
    pub fn purge_plan(&self, items: &[PurgeItem]) -> Result<Vec<PurgeItem>, Error> {
        let chosen: HashSet<i64> = items
            .iter()
            .filter(|i| i.table == "comp_type")
            .map(|i| i.id)
            .collect();
        let mut st = self
            .db
            .prepare("SELECT id FROM comp_type WHERE family=?1")?;
        let mut out = Vec::with_capacity(items.len());
        for i in items {
            if i.table == "comp_family" {
                let types: Vec<i64> = st
                    .query_map([i.id], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                if !types.iter().all(|t| chosen.contains(t)) {
                    continue;
                }
            }
            out.push(i.clone());
        }
        Ok(out)
    }

    /// Removes [`Self::purge_plan`] of `items` (each must still be unused and come from
    /// [`Self::purge_candidates`]) in one operation; returns how many rows were removed.
    /// Nothing to do: no journal entry.
    pub fn purge_unused(&mut self, items: &[PurgeItem]) -> Result<usize, Error> {
        let plan = self.purge_plan(items)?;
        if plan.is_empty() {
            return Ok(0);
        }
        crate::wall_edit::ensure(&self.db)?;
        let want: HashSet<(&str, i64)> = plan.iter().map(|i| (i.table, i.id)).collect();
        self.op("purge_unused", |db| {
            let now: HashSet<(&str, i64)> =
                candidates(db)?.iter().map(|i| (i.table, i.id)).collect();
            if let Some((t, id)) = want.iter().find(|w| !now.contains(*w)) {
                return Err(Error::Invalid(format!("{t} {id} is in use or missing")));
            }
            let mut inv = Inv::default();
            let mut n = 0;
            for table in PURGE_TABLES {
                let mut ids: Vec<i64> = want.iter().filter(|w| w.0 == table).map(|w| w.1).collect();
                ids.sort_unstable();
                for id in ids {
                    inv = inv.restore(db, table, "id", id)?;
                    for (ct, col) in owned(table) {
                        inv = inv.restore(db, ct, col, id)?;
                        db.execute(&format!("DELETE FROM {ct} WHERE {col}=?1"), [id])?;
                    }
                    inv = drop_links(db, inv, table, id)?;
                    db.execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])?;
                    n += 1;
                }
            }
            Ok((n, 0, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompHost, CompSpec, LayerSpec, MemberShape, WallSpec};
    use rusqlite::types::Value as V;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    /// Hash of every table row with its rowid, except the journal itself: equal before and after
    /// undo. `*_qty` are rebuilt by regeneration, so only their contents count.
    pub(crate) fn fingerprint(m: &Model) -> u64 {
        let db = m.sql();
        let names: Vec<String> = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name<>'op' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let mut h = DefaultHasher::new();
        for t in names {
            t.hash(&mut h);
            // a WITHOUT ROWID table has no rowid: its rows count by content, in sorted order
            let sql: String = db
                .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name=?1", [&t], |r| r.get(0))
                .unwrap();
            let rowless = sql.to_ascii_uppercase().contains("WITHOUT ROWID");
            let q = match (t.ends_with("_qty"), rowless) {
                (_, true) => format!("SELECT * FROM {t}"),
                (true, false) => format!("SELECT * FROM {t} ORDER BY rowid"),
                (false, false) => format!("SELECT rowid, * FROM {t} ORDER BY rowid"),
            };
            let mut st = db.prepare(&q).unwrap();
            let n = st.column_count();
            let mut rows: Vec<String> = st
                .query_map([], |r| {
                    Ok((0..n)
                        .map(|i| format!("{:?}", r.get::<_, V>(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            if rowless {
                rows.sort();
            }
            for r in rows {
                r.hash(&mut h);
            }
        }
        h.finish()
    }

    fn layers() -> Vec<LayerSpec> {
        vec![LayerSpec {
            material: "brick".into(),
            thickness: 0.2,
        }]
    }

    /// Wall types: used, spare (with every owned row, a link and a property), part of a stack.
    /// Two slab types, three member types, two library families.
    fn project() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let used = m.add_wall_type("used", &layers()).unwrap();
        let spare = m.add_wall_type("spare", &layers()).unwrap();
        let in_stack = m.add_wall_type("in stack", &layers()).unwrap();
        m.add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: used,
            offset0: None,
        })
        .unwrap();
        m.add_slab_type("floor A", &layers()).unwrap();
        m.add_slab_type("floor B", &layers()).unwrap();
        for (i, n) in ["col A", "col B", "col C"].into_iter().enumerate() {
            m.add_member_type(
                n,
                &MemberShape::Rect {
                    b: 0.3 + i as f64 * 0.1,
                    h: 0.3,
                },
                "Concrete",
            )
            .unwrap();
        }
        let srcs = crate::library_sources();
        let (f1, f2) = (
            m.load_family(srcs[0]).unwrap(),
            m.load_family(srcs[1]).unwrap(),
        );
        let t1 = m.comp_types(f1).unwrap()[0].0;
        m.add_component(&CompSpec {
            level: lv,
            ctype: t1,
            at: (1.0, 1.0, 0.0),
            rotation: 0.0,
            mirror: false,
            tilt: None,
            host: CompHost::None,
            values: vec![],
        })
        .unwrap();
        // lazily created tables exist before the first fingerprint
        m.purge_candidates().unwrap();
        for (sql, id) in [
            ("INSERT INTO wall_region(wtype,idx,layer,z0,z1,material) VALUES (?1,0,0,0.0,1.0,0)", spare),
            ("INSERT INTO wall_wrap(wtype,at_inserts,at_ends,wraps,core0,core1) VALUES (?1,'none','none','',0,0)", spare),
            ("INSERT INTO wall_layer_flag(wtype,pos,core,priority) VALUES (?1,0,1,NULL)", spare),
            ("INSERT INTO pparam(id,name,ptype,per_type,unit,grp,guid) VALUES (?1,'p','text',1,'','',NULL)", 1),
            ("INSERT INTO pparam_val(param,kind,ref,value) VALUES (1,'wall_type',?1,'v')", spare),
            ("INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('f','g1','wall_type',?1)", spare),
            ("INSERT INTO prop(kind,ref,key,value,unit) VALUES ('wall_type',?1,'k','v','')", spare),
            ("INSERT INTO stack_type(id,name) VALUES (?1,'s')", 1),
            ("INSERT INTO stack_part(stack,pos,wtype,height,offset) VALUES (1,0,?1,NULL,0.0)", in_stack),
        ] {
            m.sql().execute(sql, [id]).unwrap();
        }
        m.add_level("tmp", 9.0).unwrap();
        m.undo().unwrap();
        (m, f1, f2)
    }

    fn names(v: &[PurgeItem], table: &str) -> Vec<String> {
        v.iter()
            .filter(|i| i.table == table)
            .map(|i| i.name.clone())
            .collect()
    }

    #[test]
    fn preview_lists_only_unused_and_keeps_the_last_type() {
        let (m, f1, f2) = project();
        let c = m.purge_candidates().unwrap();
        assert_eq!(names(&c, "wall_type"), ["spare"]);
        // both slab types unused: the first one stays
        assert_eq!(names(&c, "slab_type"), ["floor B"]);
        assert_eq!(names(&c, "member_type").len(), 2);
        assert!(c.iter().any(|i| i.table == "comp_family" && i.id == f2));
        assert!(!c.iter().any(|i| i.table == "comp_family" && i.id == f1));
        // the type under the placed instance is never listed
        let t1 = m.comp_types(f1).unwrap()[0].0;
        assert!(!c.iter().any(|i| i.table == "comp_type" && i.id == t1));
        m.assert_integrity();
    }

    #[test]
    fn purge_is_one_step_and_undo_restores_the_exact_state() {
        let (mut m, ..) = project();
        let before = fingerprint(&m);
        let ops = m
            .sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
            .unwrap();
        let c = m.purge_candidates().unwrap();
        assert!(c.len() >= 5);
        assert_eq!(m.purge_unused(&c).unwrap(), c.len());
        let after = m
            .sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(after, ops + 1, "one journal entry for the whole purge");
        assert!(m.purge_candidates().unwrap().is_empty());
        assert_ne!(fingerprint(&m), before);
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(fingerprint(&m), before);
        m.assert_integrity();
        assert_eq!(m.purge_candidates().unwrap(), c);
    }

    #[test]
    fn stale_or_used_items_are_refused_and_nothing_changes() {
        let (mut m, ..) = project();
        let before = fingerprint(&m);
        let used = m.purge_candidates().unwrap();
        let mut bad = used[..1].to_vec();
        bad.push(PurgeItem {
            table: "wall_type",
            id: m
                .sql()
                .query_row("SELECT id FROM wall_type WHERE name='used'", [], |r| {
                    r.get(0)
                })
                .unwrap(),
            name: "used".into(),
            family: None,
        });
        assert!(m.purge_unused(&bad).is_err());
        assert_eq!(fingerprint(&m), before);
        assert_eq!(m.purge_unused(&[]).unwrap(), 0);
    }

    #[test]
    fn a_type_in_a_stack_is_not_listed() {
        let (m, ..) = project();
        let c = m.purge_candidates().unwrap();
        assert!(!names(&c, "wall_type").iter().any(|n| n == "in stack"));
    }

    #[test]
    fn an_unchecked_type_keeps_its_family() {
        let (mut m, _, f2) = project();
        let t2 = m.comp_types(f2).unwrap()[0].0;
        let chosen: Vec<PurgeItem> = m
            .purge_candidates()
            .unwrap()
            .into_iter()
            .filter(|i| !(i.table == "comp_type" && i.id == t2))
            .collect();
        assert!(chosen
            .iter()
            .any(|i| i.table == "comp_family" && i.id == f2));
        assert_eq!(m.purge_plan(&chosen).unwrap().len(), chosen.len() - 1);
        assert_eq!(m.purge_unused(&chosen).unwrap(), chosen.len() - 1);
        assert!(m.comp_types(f2).unwrap().iter().any(|t| t.0 == t2));
        m.assert_integrity();
    }

    #[test]
    fn a_family_alone_writes_nothing() {
        let (mut m, _, f2) = project();
        let family: Vec<PurgeItem> = m
            .purge_candidates()
            .unwrap()
            .into_iter()
            .filter(|i| i.table == "comp_family" && i.id == f2)
            .collect();
        let ops = m.journal_len().unwrap();
        assert!(m.purge_plan(&family).unwrap().is_empty());
        assert_eq!(m.purge_unused(&family).unwrap(), 0);
        assert_eq!(m.journal_len().unwrap(), ops);
    }

    fn links(m: &Model, id: i64) -> i64 {
        m.sql()
            .query_row(
                "SELECT (SELECT count(*) FROM ext_ref WHERE kind='wall_type' AND ref=?1) \
                 + (SELECT count(*) FROM prop WHERE kind='wall_type' AND ref=?1)",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn spare_id(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT id FROM wall_type WHERE name='spare'", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn a_removed_type_takes_its_link_and_properties_along() {
        let (mut m, ..) = project();
        let spare = spare_id(&m);
        assert_eq!(links(&m, spare), 2);
        let before = fingerprint(&m);
        let c: Vec<PurgeItem> = m
            .purge_candidates()
            .unwrap()
            .into_iter()
            .filter(|i| i.table == "wall_type" && i.id == spare)
            .collect();
        assert_eq!(m.purge_unused(&c).unwrap(), 1);
        assert_eq!(links(&m, spare), 0);
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(links(&m, spare), 2);
        assert_eq!(fingerprint(&m), before);
    }

    #[test]
    fn delete_wall_type_also_drops_the_link_and_undoes_exactly() {
        let (mut m, ..) = project();
        let spare = spare_id(&m);
        let before = fingerprint(&m);
        m.delete_wall_type(spare).unwrap();
        assert_eq!(links(&m, spare), 0);
        m.undo().unwrap();
        assert_eq!(fingerprint(&m), before);
    }
}
