// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Design options: named sets of alternatives (schema 38). An element belongs to at most one
//! option; elements outside options are the main model. A view shows the selected option of each
//! set, by default the set's primary option. Accepting an option moves its elements into the
//! main model and deletes the elements of the other options, in one journal operation.
//! Limitation: quantities, rooms and schedules are computed over the whole model, ignoring options.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};

use crate::integrity::Issue;
use crate::merge::tables::{self, RefCol, Schema};
use crate::merge::{drop_row, HOSTED};
use crate::phases::ViewKey;
use crate::{Error, Model, Snapshots};

/// Elements hidden by options: table -> ids.
pub type HiddenByOption = HashMap<String, HashSet<i64>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesignOption {
    pub id: i64,
    pub set_id: i64,
    pub name: String,
    pub is_primary: bool,
    pub members: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionSet {
    pub id: i64,
    pub name: String,
    pub options: Vec<DesignOption>,
}

/// Result of accepting an option.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcceptReport {
    pub accepted: usize,
    pub removed: usize,
}

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS design_option_set(
            id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS design_option(
            id INTEGER PRIMARY KEY, set_id INTEGER NOT NULL, name TEXT NOT NULL,
            is_primary INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX IF NOT EXISTS design_option_set_ix ON design_option(set_id);
         CREATE TABLE IF NOT EXISTS option_member(
            kind TEXT NOT NULL, ref INTEGER NOT NULL, option_id INTEGER NOT NULL,
            PRIMARY KEY(kind, ref));
         CREATE INDEX IF NOT EXISTS option_member_opt ON option_member(option_id);
         CREATE TABLE IF NOT EXISTS view_option(
            vkind TEXT NOT NULL, ref INTEGER NOT NULL, set_id INTEGER NOT NULL, option_id INTEGER NOT NULL,
            PRIMARY KEY(vkind, ref, set_id));
         CREATE TABLE IF NOT EXISTS merge_peer(
            id INTEGER PRIMARY KEY, path TEXT NOT NULL, their_seq INTEGER NOT NULL,
            their_hash INTEGER NOT NULL, at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS merge_map(
            peer INTEGER NOT NULL, tbl TEXT NOT NULL, their INTEGER NOT NULL, mine INTEGER NOT NULL,
            PRIMARY KEY(peer, tbl, their));
         INSERT OR IGNORE INTO meta(key, value)
            VALUES('project_uid', (random() & 9223372036854775807) | 1);",
    )?;
    // two forks of one history can log operations with the same inverse: a stamp tells them apart
    let has: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('op') WHERE name='stamp'",
        [],
        |r| r.get(0),
    )?;
    if has == 0 {
        db.execute_batch("ALTER TABLE op ADD COLUMN stamp INTEGER;")?;
    }
    db.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS op_stamp AFTER INSERT ON op WHEN new.stamp IS NULL
         BEGIN UPDATE op SET stamp=random() WHERE seq=new.seq; END;",
    )?;
    Ok(())
}

pub(crate) fn issues(db: &Connection) -> Result<Vec<Issue>, Error> {
    let mut out = vec![];
    for (table, sql, problem) in [
        (
            "design_option",
            "SELECT id FROM design_option WHERE set_id NOT IN (SELECT id FROM design_option_set)",
            "set_id -> missing design_option_set",
        ),
        (
            "option_member",
            "SELECT ref FROM option_member WHERE option_id NOT IN (SELECT id FROM design_option)",
            "option_id -> missing design_option",
        ),
    ] {
        let ids: Vec<i64> = db
            .prepare(sql)?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        out.extend(ids.into_iter().map(|id| Issue {
            table: table.to_string(),
            id,
            problem: problem.to_string(),
        }));
    }
    Ok(out)
}

/// An element and everything that cannot exist without it (openings, placed components).
fn with_dependents(
    db: &Connection,
    s: &Schema,
    table: &str,
    id: i64,
    out: &mut HashSet<(String, i64)>,
) -> Result<(), Error> {
    if !out.insert((table.to_string(), id)) {
        return Ok(());
    }
    for d in &s.tables {
        for r in &d.refs {
            let sql = match r {
                RefCol::To { col, parent }
                    if parent == table
                        && (d.notnull.contains(col)
                            || HOSTED.iter().any(|(a, b)| *a == d.name && b == col)) =>
                {
                    format!(
                        "SELECT id FROM {} WHERE {}=?1 AND ?2=?2",
                        tables::q(&d.name),
                        tables::q(col)
                    )
                }
                RefCol::Poly { kind, col } => format!(
                    "SELECT id FROM {} WHERE {}=?1 AND {}=?2",
                    tables::q(&d.name),
                    tables::q(col),
                    tables::q(kind)
                ),
                _ => continue,
            };
            let ids: Vec<i64> = db
                .prepare(&sql)?
                .query_map(params![id, table], |x| x.get(0))?
                .collect::<Result<_, _>>()?;
            for i in ids {
                with_dependents(db, s, &d.name, i, out)?;
            }
        }
    }
    Ok(())
}

fn expand(db: &Connection, items: &[(String, i64)]) -> Result<HashSet<(String, i64)>, Error> {
    let s = tables::load(db, &[])?;
    let mut all = HashSet::new();
    for (k, id) in items {
        if matches!(k.as_str(), "level" | "grid") || s.table(k).is_none() {
            return Err(Error::Invalid(format!("{k} cannot belong to an option")));
        }
        with_dependents(db, &s, k, *id, &mut all)?;
    }
    Ok(all)
}

fn set_of(db: &Connection, option: i64) -> Result<i64, Error> {
    db.query_row(
        "SELECT set_id FROM design_option WHERE id=?1",
        [option],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no option {option}")))
}

fn members(db: &Connection, option: i64) -> Result<Vec<(String, i64)>, Error> {
    let v = db
        .prepare("SELECT kind, ref FROM option_member WHERE option_id=?1 ORDER BY kind, ref")?
        .query_map([option], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// Deletes the option's elements; dependent ids already deleted earlier are skipped.
fn drop_members(
    db: &Connection,
    s: &Schema,
    snaps: &mut Snapshots,
    option: i64,
) -> Result<usize, Error> {
    let mut n = 0;
    for (k, id) in members(db, option)? {
        n += drop_row(db, s, snaps, &k, id)?;
    }
    snaps.save(db, "option_member", "option_id", option)?;
    db.execute("DELETE FROM option_member WHERE option_id=?1", [option])?;
    Ok(n)
}

fn forget_option(db: &Connection, snaps: &mut Snapshots, option: i64) -> Result<(), Error> {
    snaps.save(db, "design_option", "id", option)?;
    snaps.save(db, "view_option", "option_id", option)?;
    db.execute("DELETE FROM view_option WHERE option_id=?1", [option])?;
    db.execute("DELETE FROM design_option WHERE id=?1", [option])?;
    Ok(())
}

impl Model {
    /// Option sets with the element count of each option.
    pub fn option_sets(&self) -> Result<Vec<OptionSet>, Error> {
        let mut sets: Vec<OptionSet> = self
            .db
            .prepare("SELECT id, name FROM design_option_set ORDER BY id")?
            .query_map([], |r| {
                Ok(OptionSet {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    options: vec![],
                })
            })?
            .collect::<Result<_, _>>()?;
        let opts: Vec<DesignOption> = self
            .db
            .prepare(
                "SELECT o.id, o.set_id, o.name, o.is_primary, \
                 (SELECT count(*) FROM option_member m WHERE m.option_id=o.id) \
                 FROM design_option o ORDER BY o.id",
            )?
            .query_map([], |r| {
                Ok(DesignOption {
                    id: r.get(0)?,
                    set_id: r.get(1)?,
                    name: r.get(2)?,
                    is_primary: r.get::<_, i64>(3)? != 0,
                    members: r.get::<_, i64>(4)? as usize,
                })
            })?
            .collect::<Result<_, _>>()?;
        for o in opts {
            if let Some(s) = sets.iter_mut().find(|s| s.id == o.set_id) {
                s.options.push(o);
            }
        }
        Ok(sets)
    }

    /// A new set with its first (primary) option: (set, option).
    pub fn add_option_set(&mut self, name: &str, first: &str) -> Result<(i64, i64), Error> {
        self.op("option_set_add", |db| {
            let mut snaps = Snapshots::new();
            snaps.watermark(db, "design_option_set")?;
            snaps.watermark(db, "design_option")?;
            db.execute("INSERT INTO design_option_set(name) VALUES (?1)", [name])?;
            let set = db.last_insert_rowid();
            db.execute(
                "INSERT INTO design_option(set_id, name, is_primary) VALUES (?1, ?2, 1)",
                params![set, first],
            )?;
            let opt = db.last_insert_rowid();
            Ok(((set, opt), 0, snaps.into_inv()))
        })
    }

    pub fn add_option(&mut self, set: i64, name: &str) -> Result<i64, Error> {
        self.op("option_add", |db| {
            let mut snaps = Snapshots::new();
            snaps.watermark(db, "design_option")?;
            db.execute(
                "INSERT INTO design_option(set_id, name) VALUES (?1, ?2)",
                params![set, name],
            )?;
            Ok((db.last_insert_rowid(), 0, snaps.into_inv()))
        })
    }

    pub fn rename_option(&mut self, id: i64, name: &str) -> Result<(), Error> {
        self.op("option_rename", |db| {
            let mut snaps = Snapshots::new();
            snaps.save(db, "design_option", "id", id)?;
            db.execute(
                "UPDATE design_option SET name=?2 WHERE id=?1",
                params![id, name],
            )?;
            Ok(((), 0, snaps.into_inv()))
        })
    }

    pub fn rename_option_set(&mut self, id: i64, name: &str) -> Result<(), Error> {
        self.op("option_set_rename", |db| {
            let mut snaps = Snapshots::new();
            snaps.save(db, "design_option_set", "id", id)?;
            db.execute(
                "UPDATE design_option_set SET name=?2 WHERE id=?1",
                params![id, name],
            )?;
            Ok(((), 0, snaps.into_inv()))
        })
    }

    /// Makes an option primary: views without their own selection show it.
    pub fn set_primary_option(&mut self, id: i64) -> Result<(), Error> {
        self.op("option_primary", |db| {
            let set = set_of(db, id)?;
            let mut snaps = Snapshots::new();
            snaps.save(db, "design_option", "set_id", set)?;
            db.execute(
                "UPDATE design_option SET is_primary=(id=?2) WHERE set_id=?1",
                params![set, id],
            )?;
            Ok(((), 0, snaps.into_inv()))
        })
    }

    /// Deletes an option together with its elements. The next option becomes primary; deleting the
    /// last option removes the set.
    pub fn delete_option(&mut self, id: i64) -> Result<usize, Error> {
        self.op("wall_option_delete", |db| {
            let set = set_of(db, id)?;
            let s = tables::load(db, &[])?;
            let mut snaps = Snapshots::new();
            snaps.save(db, "design_option", "set_id", set)?;
            let n = drop_members(db, &s, &mut snaps, id)?;
            forget_option(db, &mut snaps, id)?;
            let next: Option<i64> = db
                .query_row(
                    "SELECT id FROM design_option WHERE set_id=?1 ORDER BY is_primary DESC, id LIMIT 1",
                    [set],
                    |r| r.get(0),
                )
                .optional()?;
            match next {
                Some(o) => {
                    db.execute("UPDATE design_option SET is_primary=1 WHERE id=?1", [o])?;
                }
                None => {
                    snaps.save(db, "design_option_set", "id", set)?;
                    db.execute("DELETE FROM design_option_set WHERE id=?1", [set])?;
                }
            }
            Ok((n, 0, snaps.into_inv()))
        })
    }

    /// Adds elements, with their dependents, to an option; they leave any other option.
    pub fn add_to_option(&mut self, option: i64, items: &[(String, i64)]) -> Result<usize, Error> {
        self.op("option_members", |db| {
            set_of(db, option)?;
            let all = expand(db, items)?;
            let mut snaps = Snapshots::new();
            snaps.watermark(db, "option_member")?;
            let mut st = db.prepare_cached(
                "INSERT OR REPLACE INTO option_member(kind, ref, option_id) VALUES (?1, ?2, ?3)",
            )?;
            for (k, id) in &all {
                snaps.save(db, "option_member", "ref", *id)?;
                st.execute(params![k, id, option])?;
            }
            Ok((all.len(), 0, snaps.into_inv()))
        })
    }

    /// Returns elements (with dependents) to the main model.
    pub fn remove_from_option(&mut self, items: &[(String, i64)]) -> Result<usize, Error> {
        self.op("option_members", |db| {
            let all = expand(db, items)?;
            let mut snaps = Snapshots::new();
            let mut n = 0;
            for (k, id) in &all {
                snaps.save(db, "option_member", "ref", *id)?;
                n += db.execute(
                    "DELETE FROM option_member WHERE kind=?1 AND ref=?2",
                    params![k, id],
                )?;
            }
            Ok((n, 0, snaps.into_inv()))
        })
    }

    /// Which option of a set a view shows.
    pub fn set_view_option(&mut self, view: ViewKey, set: i64, option: i64) -> Result<(), Error> {
        let (vkind, vref) = view.parts();
        self.op("view_option", |db| {
            if set_of(db, option)? != set {
                return Err(Error::Invalid("option of another set".into()));
            }
            let mut snaps = Snapshots::new();
            snaps.watermark(db, "view_option")?;
            snaps.save(db, "view_option", "set_id", set)?;
            db.execute(
                "INSERT OR REPLACE INTO view_option(vkind, ref, set_id, option_id) VALUES (?1,?2,?3,?4)",
                params![vkind, vref, set, option],
            )?;
            Ok(((), 0, snaps.into_inv()))
        })
    }

    /// The option of a set that the view shows.
    pub fn view_option(&self, view: ViewKey, set: i64) -> Result<Option<i64>, Error> {
        let (vkind, vref) = view.parts();
        Ok(self
            .db
            .query_row(
                "SELECT COALESCE(\
                   (SELECT option_id FROM view_option WHERE vkind=?1 AND ref=?2 AND set_id=?3),\
                   (SELECT id FROM design_option WHERE set_id=?3 AND is_primary=1 LIMIT 1))",
                params![vkind, vref, set],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Elements the view does not show: members of options other than the one selected in their set.
    pub fn option_hidden(&self, view: ViewKey) -> Result<HiddenByOption, Error> {
        let (vkind, vref) = view.parts();
        let mut st = self.db.prepare_cached(
            "SELECT m.kind, m.ref FROM option_member m JOIN design_option o ON o.id=m.option_id \
             WHERE m.option_id IS NOT COALESCE(\
               (SELECT v.option_id FROM view_option v WHERE v.vkind=?1 AND v.ref=?2 AND v.set_id=o.set_id),\
               (SELECT p.id FROM design_option p WHERE p.set_id=o.set_id AND p.is_primary=1 LIMIT 1))",
        )?;
        let mut out = HiddenByOption::new();
        let mut rows = st.query(params![vkind, vref])?;
        while let Some(r) = rows.next()? {
            out.entry(r.get::<_, String>(0)?)
                .or_default()
                .insert(r.get(1)?);
        }
        Ok(out)
    }

    /// Accepts an option: its elements go into the main model, the other options of the set are
    /// deleted with their elements, and the set disappears. One operation.
    pub fn accept_option(&mut self, id: i64) -> Result<AcceptReport, Error> {
        self.op("wall_option_accept", |db| {
            let set = set_of(db, id)?;
            let s = tables::load(db, &[])?;
            let mut snaps = Snapshots::new();
            let options: Vec<i64> = db
                .prepare("SELECT id FROM design_option WHERE set_id=?1 ORDER BY id")?
                .query_map([set], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let mut rep = AcceptReport::default();
            for o in &options {
                if *o == id {
                    continue;
                }
                rep.removed += drop_members(db, &s, &mut snaps, *o)?;
            }
            // an element of the accepted option may hang on a removed one; removed together
            rep.accepted = members(db, id)?.len();
            snaps.save(db, "option_member", "option_id", id)?;
            db.execute("DELETE FROM option_member WHERE option_id=?1", [id])?;
            for o in &options {
                forget_option(db, &mut snaps, *o)?;
            }
            snaps.save(db, "view_option", "set_id", set)?;
            db.execute("DELETE FROM view_option WHERE set_id=?1", [set])?;
            snaps.save(db, "design_option_set", "id", set)?;
            db.execute("DELETE FROM design_option_set WHERE id=?1", [set])?;
            Ok((rep, 0, snaps.into_inv()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};

    fn wall(m: &mut Model, lv: i64, t: i64, y: f64) -> i64 {
        m.add_wall(&WallSpec {
            a: (0.0, y),
            b: (5.0, y),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap()
    }

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        (m, lv, t)
    }

    fn item(id: i64) -> Vec<(String, i64)> {
        vec![("wall".to_string(), id)]
    }

    fn walls(m: &Model) -> i64 {
        m.db.query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn view_hides_other_option_and_switch_shows_it() {
        let (mut m, lv, t) = model();
        let main = wall(&mut m, lv, t, 0.0);
        let (a, b) = (wall(&mut m, lv, t, 10.0), wall(&mut m, lv, t, 20.0));
        let (set, oa) = m.add_option_set("Entrance", "A").unwrap();
        let ob = m.add_option(set, "B").unwrap();
        m.add_to_option(oa, &item(a)).unwrap();
        m.add_to_option(ob, &item(b)).unwrap();
        let v = ViewKey::ThreeD;
        let hide = m.option_hidden(v).unwrap();
        assert!(hide["wall"].contains(&b) && !hide["wall"].contains(&a));
        assert!(!hide["wall"].contains(&main));
        m.set_view_option(v, set, ob).unwrap();
        let hide = m.option_hidden(v).unwrap();
        assert!(hide["wall"].contains(&a) && !hide["wall"].contains(&b));
        assert_eq!(m.view_option(v, set).unwrap(), Some(ob));
        // another view is untouched
        let hide = m.option_hidden(ViewKey::Plan(lv)).unwrap();
        assert!(hide["wall"].contains(&b));
        m.assert_integrity();
    }

    #[test]
    fn accept_keeps_members_removes_rest_and_undo_is_exact() {
        let (mut m, lv, t) = model();
        wall(&mut m, lv, t, 0.0);
        let (a, b) = (wall(&mut m, lv, t, 10.0), wall(&mut m, lv, t, 20.0));
        let (set, oa) = m.add_option_set("Entrance", "A").unwrap();
        let ob = m.add_option(set, "B").unwrap();
        m.add_to_option(oa, &item(a)).unwrap();
        m.add_to_option(ob, &item(b)).unwrap();
        m.set_view_option(ViewKey::ThreeD, set, ob).unwrap();
        let before = m.content_hash().unwrap();
        let rep = m.accept_option(oa).unwrap();
        assert_eq!((rep.accepted, rep.removed), (1, 1));
        assert_eq!(walls(&m), 2);
        assert!(m.option_sets().unwrap().is_empty());
        assert!(m.option_hidden(ViewKey::ThreeD).unwrap().is_empty());
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.content_hash().unwrap(), before);
        assert_eq!(walls(&m), 3);
        m.assert_integrity();
    }

    #[test]
    fn delete_option_promotes_next_and_undo_is_exact() {
        let (mut m, lv, t) = model();
        let a = wall(&mut m, lv, t, 10.0);
        let (set, oa) = m.add_option_set("Entrance", "A").unwrap();
        let ob = m.add_option(set, "B").unwrap();
        m.add_to_option(oa, &item(a)).unwrap();
        let before = m.content_hash().unwrap();
        assert_eq!(m.delete_option(oa).unwrap(), 1);
        assert_eq!(walls(&m), 0);
        let sets = m.option_sets().unwrap();
        assert!(sets[0].options[0].id == ob && sets[0].options[0].is_primary);
        m.undo().unwrap();
        assert_eq!(m.content_hash().unwrap(), before);
    }

    #[test]
    fn deleting_a_member_forgets_membership_and_undo_returns_it() {
        let (mut m, lv, t) = model();
        let a = wall(&mut m, lv, t, 10.0);
        let (_, oa) = m.add_option_set("Entrance", "A").unwrap();
        m.add_to_option(oa, &item(a)).unwrap();
        let before = m.content_hash().unwrap();
        m.delete_wall(a).unwrap();
        let n: i64 =
            m.db.query_row("SELECT count(*) FROM option_member", [], |r| r.get(0))
                .unwrap();
        assert_eq!(n, 0);
        m.undo().unwrap();
        assert_eq!(m.content_hash().unwrap(), before);
    }

    #[test]
    fn levels_cannot_join_an_option() {
        let (mut m, lv, _) = model();
        let (_, oa) = m.add_option_set("Entrance", "A").unwrap();
        assert!(matches!(
            m.add_to_option(oa, &[("level".to_string(), lv)]),
            Err(Error::Invalid(_))
        ));
    }
}
