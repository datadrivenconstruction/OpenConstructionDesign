//! Groups and pinning. A group definition is a clipboard snapshot (`Clip`) in `el_group.def`;
//! instance rows are ordinary elements, and `group_member.key` ties them to the definition row.
//! Editing a definition updates the live rows in place: ids do not change, so dimensions and tags
//! attached to an instance survive. Pinning uses temporary triggers: they catch every edit path
//! (grips, 3D, tools) and never end up in the model file.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, types::Value, Connection};

use crate::clip::{self, Clip, ClipInst, IdSet, PasteOpts, Paster, PlaceLevel, SnapOpts};
use crate::integrity::Issue;
use crate::modify::Target;
use crate::ops::{Inv, Snapshots};
use crate::{Error, Model};

const PINNED_MSG: &str = "element is pinned";

/// Element position: when these columns change, a pinned element rejects the edit.
/// Heights and vertical constraints are excluded: regeneration may adjust them.
const PIN_TABLES: &[(&str, &[&str])] = &[
    ("wall", &["ax", "ay", "bx", "by", "level"]),
    ("curtain_wall", &["ax", "ay", "bx", "by", "level"]),
    ("room_sep", &["ax", "ay", "bx", "by", "level"]),
    ("col", &["x", "y", "rotation", "level"]),
    ("beam", &["ax", "ay", "bx", "by", "level"]),
    ("comp", &["x", "y", "rotation", "mirror", "level"]),
    ("room", &["px", "py", "level"]),
    ("mep_run", &["points", "level"]),
    ("roof", &["ax", "ay", "bx", "by", "level"]),
    ("stair", &["x", "y", "angle", "level"]),
    ("opening", &["along", "wall"]),
    ("solid", &["plane", "level"]),
    ("mesh", &["positions", "level"]),
    ("mass", &["form", "level"]),
    ("grid", &["ax", "ay", "bx", "by"]),
    ("ref_plane", &["ax", "ay", "bx", "by"]),
    ("iso_footing", &["x", "y", "level"]),
    ("strip_footing", &["ax", "ay", "bx", "by", "level"]),
    ("brace", &["ax", "ay", "bx", "by", "level"]),
    ("truss", &["ax", "ay", "bx", "by", "level"]),
    ("slab", &["level"]),
    ("ceiling", &["level"]),
    ("shaft", &["level"]),
    ("raft", &["level"]),
    ("railing", &["level"]),
    ("beam_system", &["level"]),
    ("toposolid", &["boundary"]),
    ("topo_region", &["ring"]),
    ("topo_pad", &["outline"]),
    ("skylight", &["x", "y"]),
    ("d_line", &["x1", "y1", "x2", "y2"]),
    ("d_polyline", &["block"]),
    ("d_arc", &["cx", "cy"]),
    ("d_text", &["x", "y"]),
    ("d_insert", &["x", "y"]),
];
/// Contours: (vertices, parent column, parent table).
const PIN_VERTICES: &[(&str, &str, &str)] = &[
    ("slab_vertex", "slab", "slab"),
    ("ceiling_vertex", "ceiling", "ceiling"),
    ("roof_vertex", "roof", "roof"),
    ("shaft_vertex", "shaft", "shaft"),
    ("railing_vertex", "railing", "railing"),
    ("raft_vertex", "raft", "raft"),
    ("beam_system_vertex", "sys", "beam_system"),
    ("topo_point", "topo", "toposolid"),
    ("d_vertex", "polyline", "d_polyline"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct GroupInfo {
    pub id: i64,
    pub name: String,
    pub rev: i64,
    pub instances: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstInfo {
    pub id: i64,
    pub group: i64,
    pub level: i64,
    pub x: f64,
    pub y: f64,
}

fn invalid<T>(m: &str) -> Result<T, Error> {
    Err(Error::Invalid(m.to_string()))
}

/// Schema step 33; additive.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS el_group(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
            def TEXT NOT NULL, rev INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE IF NOT EXISTS group_inst(id INTEGER PRIMARY KEY, grp INTEGER NOT NULL,
            level INTEGER NOT NULL, x REAL NOT NULL, y REAL NOT NULL);
         CREATE TABLE IF NOT EXISTS group_member(inst INTEGER NOT NULL, kind TEXT NOT NULL,
            ref INTEGER NOT NULL, key INTEGER NOT NULL, PRIMARY KEY(kind, ref));
         CREATE INDEX IF NOT EXISTS group_member_inst ON group_member(inst);
         CREATE TABLE IF NOT EXISTS el_pin(kind TEXT NOT NULL, ref INTEGER NOT NULL,
            PRIMARY KEY(kind, ref));",
    )?;
    Ok(())
}

/// Schema step 43: parameter locks `param_lock` (additive). Project settings, not content:
/// compare, merge and the content hash skip the table; a pasted copy is never locked.
pub(crate) fn migrate_locks(db: &Connection) -> Result<(), Error> {
    db.execute_batch(LOCK_SQL)?;
    Ok(())
}

fn has_table(db: &Connection, t: &str) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn table_cols(db: &Connection, t: &str) -> Result<Vec<String>, Error> {
    let mut st = db.prepare(&format!("PRAGMA table_info({t})"))?;
    let v = st
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// Per-connection triggers: without them pinning has no effect; with them the file hash is unchanged.
pub(crate) fn pin_triggers(db: &Connection) -> Result<(), Error> {
    let mut sql = String::new();
    for (t, cols) in PIN_TABLES.iter().chain(clip::reg::LANE_PINS) {
        if !has_table(db, t)? {
            continue;
        }
        let raise = format!("BEGIN SELECT RAISE(ABORT,'{PINNED_MSG}: {t}'); END;");
        let have = table_cols(db, t)?;
        let cols: Vec<&str> = cols
            .iter()
            .copied()
            .filter(|c| have.iter().any(|h| h == c))
            .collect();
        let pinned = format!("EXISTS(SELECT 1 FROM el_pin WHERE kind='{t}' AND ref=OLD.id)");
        if !cols.is_empty() {
            let changed = cols
                .iter()
                .map(|c| format!("NEW.{c} IS NOT OLD.{c}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            sql += &format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS ddc_pin_{t}_u BEFORE UPDATE OF {} ON main.{t} \
                 WHEN ({changed}) AND {pinned} {raise}",
                cols.join(",")
            );
        }
        sql += &format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ddc_pin_{t}_d BEFORE DELETE ON main.{t} \
             WHEN {pinned} {raise}"
        );
    }
    for (v, owner, parent) in PIN_VERTICES {
        if !has_table(db, v)? {
            continue;
        }
        let raise = format!("BEGIN SELECT RAISE(ABORT,'{PINNED_MSG}: {parent}'); END;");
        let pinned =
            format!("EXISTS(SELECT 1 FROM el_pin WHERE kind='{parent}' AND ref=OLD.{owner})");
        sql += &format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ddc_pin_{v}_u BEFORE UPDATE ON main.{v} WHEN {pinned} {raise}\
             CREATE TEMP TRIGGER IF NOT EXISTS ddc_pin_{v}_d BEFORE DELETE ON main.{v} WHEN {pinned} {raise}"
        );
    }
    db.execute_batch(&sql)?;
    Ok(())
}

/// Whether the element is pinned: batch paths skip such rows instead of failing the whole operation.
pub(crate) fn is_pinned_in(db: &Connection, kind: &str, id: i64) -> Result<bool, Error> {
    let n: i64 = db
        .prepare_cached("SELECT count(*) FROM el_pin WHERE kind=?1 AND ref=?2")?
        .query_row(params![kind, id], |r| r.get(0))?;
    Ok(n > 0)
}

/// Deleting the element would hit a pin: on the element itself or on something hosted on it.
pub(crate) fn pin_blocks_delete(db: &Connection, kind: &str, id: i64) -> Result<bool, Error> {
    if is_pinned_in(db, kind, id)? {
        return Ok(true);
    }
    let hosted: &[(&str, &str)] = match kind {
        "wall" => &[("opening", "wall"), ("comp", "host_wall")],
        "slab" => &[("comp", "host_slab")],
        "ceiling" => &[("comp", "host_ceiling")],
        _ => &[],
    };
    for (t, col) in hosted {
        let n: i64 = db
            .prepare_cached(&format!(
                "SELECT count(*) FROM el_pin p JOIN {t} c ON c.id=p.ref WHERE p.kind='{t}' AND c.{col}=?1"
            ))?
            .query_row([id], |r| r.get(0))?;
        if n > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Member rows of deleted elements and emptied instances are removed in the same journal entry.
pub(crate) fn after_op(db: &Connection, mut inv: Inv) -> Result<Inv, Error> {
    let any: i64 = db.query_row("SELECT EXISTS(SELECT 1 FROM group_inst)", [], |r| r.get(0))?;
    if any == 0 {
        return Ok(inv);
    }
    let kinds: Vec<String> = {
        let mut st = db.prepare("SELECT DISTINCT kind FROM group_member")?;
        let v = st
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    for k in kinds {
        if !has_table(db, &k)? {
            continue;
        }
        let gone: Vec<(i64, i64, i64, i64)> = {
            let mut st = db.prepare(&format!(
                "SELECT rowid,inst,ref,key FROM group_member WHERE kind=?1 AND ref NOT IN (SELECT id FROM {k})"
            ))?;
            let v = st
                .query_map([&k], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        for (rowid, inst, rf, key) in gone {
            inv = inv.sql(
                "INSERT OR REPLACE INTO group_member(rowid,inst,kind,ref,key) VALUES (?1,?2,?3,?4,?5)",
                vec![
                    Value::Integer(rowid),
                    Value::Integer(inst),
                    Value::Text(k.clone()),
                    Value::Integer(rf),
                    Value::Integer(key),
                ],
            );
            db.execute("DELETE FROM group_member WHERE rowid=?1", [rowid])?;
        }
    }
    let empty: Vec<i64> = {
        let mut st = db.prepare(
            "SELECT id FROM group_inst WHERE id NOT IN (SELECT inst FROM group_member) \
             OR level NOT IN (SELECT id FROM level)",
        )?;
        let v = st
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    for id in empty {
        inv = inv.restore(db, "group_member", "inst", id)?;
        inv = inv.restore(db, "group_inst", "id", id)?;
        db.execute("DELETE FROM group_member WHERE inst=?1", [id])?;
        db.execute("DELETE FROM group_inst WHERE id=?1", [id])?;
    }
    Ok(inv)
}

pub(crate) fn issues(db: &Connection) -> Result<Vec<Issue>, Error> {
    let mut out = vec![];
    let mut add = |table: &str, id: i64, problem: &str| {
        out.push(Issue {
            table: table.into(),
            id,
            problem: problem.into(),
        });
    };
    for (sql, table, problem) in [
        (
            "SELECT rowid FROM group_inst WHERE grp NOT IN (SELECT id FROM el_group)",
            "group_inst",
            "instance of a missing group",
        ),
        (
            "SELECT rowid FROM group_inst WHERE level NOT IN (SELECT id FROM level)",
            "group_inst",
            "instance on a missing level",
        ),
        (
            "SELECT rowid FROM group_member WHERE inst NOT IN (SELECT id FROM group_inst)",
            "group_member",
            "member of a missing instance",
        ),
    ] {
        let mut st = db.prepare(sql)?;
        for id in st.query_map([], |r| r.get::<_, i64>(0))? {
            add(table, id?, problem);
        }
    }
    for (table, sql) in [
        ("group_member", "SELECT DISTINCT kind FROM group_member"),
        ("el_pin", "SELECT DISTINCT kind FROM el_pin"),
    ] {
        let kinds: Vec<String> = {
            let mut st = db.prepare(sql)?;
            let v = st
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        for k in kinds {
            let exists = has_table(db, &k)?;
            let query = if exists {
                format!(
                    "SELECT rowid FROM {table} WHERE kind=?1 AND ref NOT IN (SELECT id FROM {k})"
                )
            } else {
                format!("SELECT rowid FROM {table} WHERE kind=?1")
            };
            let mut st = db.prepare(&query)?;
            for id in st.query_map([&k], |r| r.get::<_, i64>(0))? {
                add(table, id?, "reference to a missing element");
            }
        }
    }
    Ok(out)
}

/// An instance is held by its definition: rotating or mirroring it would break definition editing.
pub(crate) fn reject_turn(db: &Connection, ts: &[Target], copy: bool) -> Result<(), Error> {
    if copy {
        return Ok(());
    }
    let mut st = db.prepare_cached("SELECT count(*) FROM group_member WHERE kind=?1 AND ref=?2")?;
    for t in ts {
        let (k, id) = crate::modify::table_of(*t);
        let n: i64 = st.query_row(params![k, id], |r| r.get(0))?;
        if n > 0 {
            return invalid("group members cannot be rotated or mirrored: ungroup first");
        }
    }
    Ok(())
}

fn members_of(db: &Connection, inst: i64) -> Result<Vec<(String, i64, i64)>, Error> {
    let mut st =
        db.prepare_cached("SELECT kind,ref,key FROM group_member WHERE inst=?1 ORDER BY rowid")?;
    let v = st
        .query_map([inst], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// The snapshot includes instances selected in full: pasting creates new instances.
pub(crate) fn snapshot_instances(
    db: &Connection,
    clip: &mut Clip,
    set: &IdSet,
) -> Result<(), Error> {
    let rows: Vec<(i64, String, f64, f64, i64, String)> = {
        let mut st = db.prepare(
            "SELECT i.id,g.name,i.x,i.y,i.level,g.def FROM group_inst i JOIN el_group g ON g.id=i.grp ORDER BY i.id",
        )?;
        let v = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    for (id, group, x, y, level, def) in rows {
        let members = members_of(db, id)?;
        if !members.is_empty() && members.iter().all(|m| clip::contains(set, &m.0, m.1)) {
            clip.instances.push(ClipInst {
                group,
                def,
                level,
                at: (x, y),
                members,
            });
        }
    }
    Ok(())
}

/// New instances for the snapshot's instances; another model also receives the definition.
pub(crate) fn paste_instances(
    db: &Connection,
    clip: &Clip,
    p: &Paster,
    o: &PasteOpts,
) -> Result<(), Error> {
    for ci in &clip.instances {
        let new: Vec<(&str, i64, i64)> = ci
            .members
            .iter()
            .filter_map(|m| {
                p.map
                    .get(&m.0)
                    .and_then(|x| x.get(&m.1))
                    .map(|n| (m.0.as_str(), *n, m.2))
            })
            .collect();
        let Some(level) = p.level_of(ci.level) else {
            continue;
        };
        if new.is_empty() {
            continue;
        }
        let grp = ensure_group(db, &ci.group, &ci.def)?;
        db.execute(
            "INSERT INTO group_inst(grp,level,x,y) VALUES (?1,?2,?3,?4)",
            params![grp, level, ci.at.0 + o.dx, ci.at.1 + o.dy],
        )?;
        let inst = db.last_insert_rowid();
        let mut member = db.prepare_cached(
            "INSERT OR IGNORE INTO group_member(inst,kind,ref,key) VALUES (?1,?2,?3,?4)",
        )?;
        for (k, r, key) in new {
            member.execute(params![inst, k, r, key])?;
        }
    }
    Ok(())
}

/// A group with the same definition, or a new one; a foreign definition with the same name gets a suffix.
fn ensure_group(db: &Connection, name: &str, def: &str) -> Result<i64, Error> {
    let mut cand = name.to_string();
    let mut n = 1;
    loop {
        let got: Option<(i64, String)> = db
            .query_row("SELECT id,def FROM el_group WHERE name=?1", [&cand], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok();
        match got {
            Some((id, d)) if d == def => return Ok(id),
            Some(_) => {
                n += 1;
                cand = format!("{name}-{n}");
            }
            None => {
                db.execute(
                    "INSERT INTO el_group(name,def) VALUES (?1,?2)",
                    params![cand, def],
                )?;
                return Ok(db.last_insert_rowid());
            }
        }
    }
}

/// Offset of a live instance from the definition: by majority over member points, so a
/// one-off edit of a single member does not shift the whole instance.
fn offset_from_def(db: &Connection, inst: i64, def: &Clip) -> Result<(f64, f64), Error> {
    let members = members_of(db, inst)?;
    let items: Vec<(&str, i64)> = members.iter().map(|m| (m.0.as_str(), m.1)).collect();
    let live = clip::snapshot(db, &items, SnapOpts { relations: false })?;
    let (now, was) = (clip::points(&live), clip::points(def));
    let mut votes: HashMap<(i64, i64), (usize, (f64, f64))> = HashMap::new();
    for (k, r, key) in &members {
        if let (Some(a), Some(b)) = (now.get(&(k.clone(), *r)), was.get(&(k.clone(), *key))) {
            let d = (a.0 - b.0, a.1 - b.1);
            let e = votes
                .entry(((d.0 * 1e6).round() as i64, (d.1 * 1e6).round() as i64))
                .or_insert((0, d));
            e.0 += 1;
        }
    }
    if let Some(best) = votes
        .iter()
        .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(b.0.cmp(a.0)))
    {
        return Ok(best.1 .1);
    }
    let (x, y): (f64, f64) =
        db.query_row("SELECT x,y FROM group_inst WHERE id=?1", [inst], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let c = def.center();
    Ok((x - c.0, y - c.1))
}

fn save_members(db: &Connection, s: &mut Snapshots, inst: i64) -> Result<(), Error> {
    s.save(db, "group_member", "inst", inst)?;
    s.push_front(
        "DELETE FROM group_member WHERE inst=?1",
        vec![Value::Integer(inst)],
    );
    db.execute("DELETE FROM group_member WHERE inst=?1", [inst])?;
    Ok(())
}

impl Model {
    /// New group from the selection; the selected elements become its first instance.
    pub fn group_create(&mut self, name: &str, items: &[(&str, i64)]) -> Result<(i64, i64), Error> {
        let name = name.trim();
        if name.is_empty() {
            return invalid("group: name is empty");
        }
        let taken: i64 =
            self.db
                .query_row("SELECT count(*) FROM el_group WHERE name=?1", [name], |r| {
                    r.get(0)
                })?;
        if taken > 0 {
            return invalid("group: this name is taken");
        }
        let def = clip::snapshot(&self.db, items, SnapOpts { relations: false })?;
        if !def.unsupported.is_empty() {
            return invalid("group: some of these elements cannot be grouped");
        }
        let Some(level) = def.base_level else {
            return invalid("group: elements have no level");
        };
        let members = clip::members(&def);
        for (k, id) in &members {
            if self.group_of(k, *id)?.is_some() {
                return invalid("group: an element already belongs to a group");
            }
        }
        let at = def.center();
        let text = def.to_text();
        self.op("group_add", |db| {
            db.execute(
                "INSERT INTO el_group(name,def) VALUES (?1,?2)",
                params![name, text],
            )?;
            let grp = db.last_insert_rowid();
            db.execute(
                "INSERT INTO group_inst(grp,level,x,y) VALUES (?1,?2,?3,?4)",
                params![grp, level, at.0, at.1],
            )?;
            let inst = db.last_insert_rowid();
            for (k, id) in &members {
                db.execute(
                    "INSERT INTO group_member(inst,kind,ref,key) VALUES (?1,?2,?3,?3)",
                    params![inst, k, id],
                )?;
            }
            let inv = Inv::default()
                .sql(
                    "DELETE FROM group_member WHERE inst=?1",
                    vec![Value::Integer(inst)],
                )
                .delete("group_inst", "id", inst)
                .delete("el_group", "id", grp);
            Ok(((grp, inst), grp, inv))
        })
    }

    /// New group instance: the definition center is placed at (x, y) on the given level.
    pub fn group_place(&mut self, group: i64, level: i64, x: f64, y: f64) -> Result<i64, Error> {
        if !x.is_finite() || !y.is_finite() {
            return invalid("group: bad position");
        }
        let text: String =
            self.db
                .query_row("SELECT def FROM el_group WHERE id=?1", [group], |r| {
                    r.get(0)
                })?;
        let Some(def) = Clip::from_text(&text) else {
            return invalid("group: definition is unreadable");
        };
        let c = def.center();
        let o = PasteOpts {
            dx: x - c.0,
            dy: y - c.1,
            level: PlaceLevel::Base(level),
        };
        let tables = clip::write_tables(&self.db)?;
        self.batch_in("wall_group_place", &tables, |db, s| {
            let mut p = Paster::new(db, &def, &o)?;
            p.run(s)?;
            db.execute(
                "INSERT INTO group_inst(grp,level,x,y) VALUES (?1,?2,?3,?4)",
                params![group, level, x, y],
            )?;
            let inst = db.last_insert_rowid();
            let n = add_members(db, inst, &def, &p)?;
            if n == 0 {
                return invalid("group: no member could be placed");
            }
            crate::constraints::verify(db)?;
            Ok(inst)
        })
    }

    /// Ungroups: the elements stay ordinary; the definition remains for new instances.
    pub fn group_ungroup(&mut self, inst: i64) -> Result<(), Error> {
        self.op("group_ungroup", |db| {
            let inv = Inv::default()
                .restore(db, "group_inst", "id", inst)?
                .restore(db, "group_member", "inst", inst)?;
            db.execute("DELETE FROM group_member WHERE inst=?1", [inst])?;
            db.execute("DELETE FROM group_inst WHERE id=?1", [inst])?;
            Ok(((), inst, inv))
        })
    }

    /// Ungroups several instances in one undo step.
    pub fn group_ungroup_many(&mut self, insts: &[i64]) -> Result<(), Error> {
        self.compound("group_ungroup", |m| {
            for i in insts {
                m.group_ungroup(*i)?;
            }
            Ok(())
        })
    }

    /// Deletes a definition that has no instances.
    pub fn group_delete(&mut self, group: i64) -> Result<(), Error> {
        let n: i64 = self.db.query_row(
            "SELECT count(*) FROM group_inst WHERE grp=?1",
            [group],
            |r| r.get(0),
        )?;
        if n > 0 {
            return invalid("group: instances still exist");
        }
        self.op("group_delete", |db| {
            let inv = Inv::default().restore(db, "el_group", "id", group)?;
            db.execute("DELETE FROM el_group WHERE id=?1", [group])?;
            Ok(((), group, inv))
        })
    }

    /// Finishes editing: instance `inst` with the `add`/`remove` amendments becomes the definition,
    /// and the other instances are updated in place. One undo step.
    pub fn group_finish_edit(
        &mut self,
        inst: i64,
        add: &[(&str, i64)],
        remove: &[(&str, i64)],
    ) -> Result<(), Error> {
        let (group, old_text): (i64, String) = self.db.query_row(
            "SELECT g.id,g.def FROM group_inst i JOIN el_group g ON g.id=i.grp WHERE i.id=?1",
            [inst],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let Some(old_def) = Clip::from_text(&old_text) else {
            return invalid("group: definition is unreadable");
        };
        let own = members_of(&self.db, inst)?;
        let mut items: Vec<(&str, i64)> = own
            .iter()
            .map(|m| (m.0.as_str(), m.1))
            .filter(|m| !remove.contains(m))
            .collect();
        for a in add {
            match self.group_of(a.0, a.1)? {
                Some(i) if i != inst => {
                    return invalid("group: an element already belongs to a group")
                }
                _ if !items.contains(a) => items.push(*a),
                _ => {}
            }
        }
        if items.is_empty() {
            return invalid("group: an instance needs a member");
        }
        // removed items stay in the definition: otherwise the next edit would bring them back along with the host
        let mut except: Vec<(String, i64)> = old_def
            .except
            .iter()
            .filter(|e| !add.contains(&(e.0.as_str(), e.1)))
            .cloned()
            .collect();
        for r in remove {
            if !except.iter().any(|e| e.0 == r.0 && e.1 == r.1) {
                except.push((r.0.to_string(), r.1));
            }
        }
        let mut def = {
            let out: Vec<(&str, i64)> = except.iter().map(|e| (e.0.as_str(), e.1)).collect();
            clip::snapshot_except(&self.db, &items, SnapOpts { relations: false }, &out)?
        };
        def.except = except;
        let new_members = clip::members(&def);
        for (k, id) in &new_members {
            if matches!(self.group_of(k, *id)?, Some(i) if i != inst) {
                return invalid("group: an element already belongs to a group");
            }
        }
        // old definition key -> new key (id of the edited instance's member)
        let rekey: HashMap<(String, i64), i64> =
            own.iter().map(|m| ((m.0.clone(), m.2), m.1)).collect();
        let c = def.center();
        let base = def.base_level;
        let text = def.to_text();
        // the new definition is captured at the edited instance's live position, not the old one
        let e = offset_from_def(&self.db, inst, &old_def)?;
        let others: Vec<(i64, i64)> = {
            let mut st = self
                .db
                .prepare("SELECT id,level FROM group_inst WHERE grp=?1 AND id<>?2 ORDER BY id")?;
            let v = st
                .query_map([group, inst], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        let mut plans = vec![];
        let mut removed: Vec<(String, i64)> = vec![];
        for (other, level) in others {
            let d = offset_from_def(&self.db, other, &old_def)?;
            let d = (d.0 - e.0, d.1 - e.1);
            let live = members_of(&self.db, other)?;
            // a definition member deleted in this instance only is not recreated
            let have: HashSet<(&str, i64)> = live.iter().map(|m| (m.0.as_str(), m.2)).collect();
            let omit: Vec<(String, i64)> = rekey
                .iter()
                .filter(|((k, old), _)| !have.contains(&(k.as_str(), *old)))
                .map(|((k, _), nk)| (k.clone(), *nk))
                .collect();
            let mut keep = vec![];
            for (k, r, key) in live.iter().cloned() {
                match rekey.get(&(k.clone(), key)) {
                    Some(nk) if new_members.iter().any(|m| m.0 == k && m.1 == *nk) => {
                        keep.push((k, *nk, r))
                    }
                    _ => removed.push((k, r)),
                }
            }
            plans.push((other, level, d, keep, omit));
        }
        let tables = clip::write_tables(&self.db)?;
        self.compound("wall_group_edit", |m| {
            m.batch_in("wall_group_edit", &tables, |db, s| {
                s.save(db, "el_group", "id", group)?;
                db.execute(
                    "UPDATE el_group SET def=?2, rev=rev+1 WHERE id=?1",
                    params![group, text],
                )?;
                s.save(db, "group_inst", "id", inst)?;
                db.execute(
                    "UPDATE group_inst SET x=?2,y=?3,level=COALESCE(?4,level) WHERE id=?1",
                    params![inst, c.0, c.1, base],
                )?;
                save_members(db, s, inst)?;
                for (k, id) in &new_members {
                    db.execute(
                        "INSERT INTO group_member(inst,kind,ref,key) VALUES (?1,?2,?3,?3)",
                        params![inst, k, id],
                    )?;
                }
                for (other, level, d, keep, omit) in &plans {
                    let o = PasteOpts {
                        dx: d.0,
                        dy: d.1,
                        level: PlaceLevel::Base(*level),
                    };
                    let mut p = Paster::new(db, &def, &o)?;
                    for (k, key, r) in keep {
                        p.prefill(k, *key, *r);
                    }
                    for (k, key) in omit {
                        p.omit(k, *key);
                    }
                    p.run(s)?;
                    save_members(db, s, *other)?;
                    add_members(db, *other, &def, &p)?;
                    s.save(db, "group_inst", "id", *other)?;
                    db.execute(
                        "UPDATE group_inst SET x=?2,y=?3 WHERE id=?1",
                        params![other, c.0 + d.0, c.1 + d.1],
                    )?;
                }
                crate::constraints::verify(db)?;
                Ok(())
            })?;
            if !removed.is_empty() {
                let list: Vec<(&str, i64)> = removed.iter().map(|r| (r.0.as_str(), r.1)).collect();
                m.delete_items("wall_group_edit", &list)?;
            }
            Ok(())
        })
    }

    pub fn groups(&self) -> Result<Vec<GroupInfo>, Error> {
        let mut st = self.db.prepare(
            "SELECT g.id,g.name,g.rev,(SELECT count(*) FROM group_inst i WHERE i.grp=g.id) \
             FROM el_group g ORDER BY g.name",
        )?;
        let v = st
            .query_map([], |r| {
                Ok(GroupInfo {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    rev: r.get(2)?,
                    instances: r.get::<_, i64>(3)? as usize,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn group_instances(&self, group: i64) -> Result<Vec<InstInfo>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id,grp,level,x,y FROM group_inst WHERE grp=?1 ORDER BY id")?;
        let v = st
            .query_map([group], |r| {
                Ok(InstInfo {
                    id: r.get(0)?,
                    group: r.get(1)?,
                    level: r.get(2)?,
                    x: r.get(3)?,
                    y: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// The instance the element belongs to.
    pub fn group_of(&self, kind: &str, id: i64) -> Result<Option<i64>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT inst FROM group_member WHERE kind=?1 AND ref=?2")?;
        let r = st.query_row(params![kind, id], |r| r.get(0));
        match r {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Instance members: (table, id).
    pub fn group_members(&self, inst: i64) -> Result<Vec<(String, i64)>, Error> {
        Ok(members_of(&self.db, inst)?
            .into_iter()
            .map(|m| (m.0, m.1))
            .collect())
    }

    /// Selection expanded to whole instances: a group is selected and copied as a unit.
    pub fn group_expand(&self, items: &[(&str, i64)]) -> Result<Vec<(String, i64)>, Error> {
        let mut out: Vec<(String, i64)> = vec![];
        let mut seen: HashSet<(String, i64)> = HashSet::new();
        let mut insts: HashSet<i64> = HashSet::new();
        for (k, id) in items {
            let more = match self.group_of(k, *id)? {
                Some(inst) if insts.insert(inst) => self.group_members(inst)?,
                Some(_) => vec![],
                None => vec![((*k).to_string(), *id)],
            };
            for m in more {
                if seen.insert(m.clone()) {
                    out.push(m);
                }
            }
        }
        Ok(out)
    }

    /// Rotating or mirroring a group member by other paths (3D manipulator): the same prohibition.
    pub fn reject_group_turn(&self, items: &[(&str, i64)]) -> Result<(), Error> {
        for (k, id) in items {
            if self.group_of(k, *id)?.is_some() {
                return invalid("group members cannot be rotated or mirrored: ungroup first");
            }
        }
        Ok(())
    }

    /// Whether the category can be pinned: otherwise the command reports a skip.
    pub fn can_pin(&self, kind: &str) -> bool {
        pinnable(kind)
    }

    /// An edit batch without the edits that would hit a pin: (the rest, what was skipped).
    pub fn split_pinned(
        &self,
        edits: &[crate::Edit],
    ) -> Result<(Vec<crate::Edit>, Vec<(String, i64)>), Error> {
        let (mut keep, mut skipped) = (vec![], vec![]);
        for e in edits {
            let blocked = match e.target() {
                Some((k, id, true)) => pin_blocks_delete(&self.db, k, id)?.then_some((k, id)),
                Some((k, id, false)) => is_pinned_in(&self.db, k, id)?.then_some((k, id)),
                None => None,
            };
            match blocked {
                Some((k, id)) => {
                    let it = (k.to_string(), id);
                    if !skipped.contains(&it) {
                        skipped.push(it);
                    }
                }
                None => keep.push(e.clone()),
            }
        }
        Ok((keep, skipped))
    }

    /// Pins elements: position changes and deletion are rejected until unpinned. One undo step.
    pub fn pin(&mut self, items: &[(&str, i64)]) -> Result<usize, Error> {
        self.op("el_pin", |db| {
            let mut inv = Inv::default();
            let mut n = 0;
            for (k, id) in items {
                if !pinnable(k) {
                    continue;
                }
                if db.execute(
                    "INSERT OR IGNORE INTO el_pin(kind,ref) VALUES (?1,?2)",
                    params![k, id],
                )? > 0
                {
                    n += 1;
                    inv = inv.sql(
                        "DELETE FROM el_pin WHERE kind=?1 AND ref=?2",
                        vec![Value::Text((*k).to_string()), Value::Integer(*id)],
                    );
                }
            }
            if n == 0 {
                return invalid("pin: nothing to pin");
            }
            Ok((n, 0, inv))
        })
    }

    pub fn unpin(&mut self, items: &[(&str, i64)]) -> Result<usize, Error> {
        self.op("el_unpin", |db| {
            let mut inv = Inv::default();
            let mut n = 0;
            for (k, id) in items {
                let rowid: Option<i64> = db
                    .query_row(
                        "SELECT rowid FROM el_pin WHERE kind=?1 AND ref=?2",
                        params![k, id],
                        |r| r.get(0),
                    )
                    .ok();
                let Some(rowid) = rowid else { continue };
                n += db.execute("DELETE FROM el_pin WHERE rowid=?1", [rowid])?;
                inv = inv.sql(
                    "INSERT INTO el_pin(rowid,kind,ref) VALUES (?1,?2,?3)",
                    vec![
                        Value::Integer(rowid),
                        Value::Text((*k).to_string()),
                        Value::Integer(*id),
                    ],
                );
            }
            if n == 0 {
                return invalid("unpin: nothing is pinned here");
            }
            Ok((n, 0, inv))
        })
    }

    pub fn is_pinned(&self, kind: &str, id: i64) -> Result<bool, Error> {
        let n: i64 = self
            .db
            .prepare_cached("SELECT count(*) FROM el_pin WHERE kind=?1 AND ref=?2")?
            .query_row(params![kind, id], |r| r.get(0))?;
        Ok(n > 0)
    }

    pub fn pinned(&self) -> Result<Vec<(String, i64)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT kind,ref FROM el_pin ORDER BY rowid")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    /// The batch without the edits that would change a locked parameter value: (the rest,
    /// skipped (table, id, parameter)). Workbook import, bulk edit and AI apply call it.
    pub fn split_locked(
        &self,
        edits: &[crate::Edit],
    ) -> Result<(Vec<crate::Edit>, Vec<LockKey>), Error> {
        if !has_table(&self.db, LOCK_TABLE)? {
            return Ok((edits.to_vec(), vec![]));
        }
        let (mut keep, mut skipped) = (vec![], vec![]);
        for e in edits {
            if let crate::Edit::ParamValue {
                param, kind, id, ..
            } = e
            {
                if is_locked_in(&self.db, kind, *id, param)? {
                    let it = (kind.clone(), *id, param.trim().to_string());
                    if !skipped.contains(&it) {
                        skipped.push(it);
                    }
                    continue;
                }
            }
            keep.push(e.clone());
        }
        Ok((keep, skipped))
    }

    /// [`Model::split_locked`] for value sets by parameter id (schedule cells and other
    /// per-cell writers): the sets to write and the locked ones left alone.
    pub fn split_locked_sets(
        &self,
        sets: &[crate::ParamSet],
    ) -> Result<(Vec<crate::ParamSet>, Vec<crate::ParamSet>), Error> {
        if !has_table(&self.db, LOCK_TABLE)? {
            return Ok((sets.to_vec(), vec![]));
        }
        let (mut keep, mut skipped) = (vec![], vec![]);
        for s in sets {
            if is_locked_param(&self.db, &s.kind, s.id, s.param)? {
                skipped.push(s.clone());
            } else {
                keep.push(s.clone());
            }
        }
        Ok((keep, skipped))
    }

    /// Locks parameter values (table, id, parameter): no import, bulk edit or AI apply writes
    /// them until unlocked. One undo step; the table appears with the first lock.
    pub fn set_param_lock(&mut self, items: &[(&str, i64, &str)]) -> Result<usize, Error> {
        self.op("param_lock", |db| {
            db.execute_batch(LOCK_SQL)?;
            let mut inv = Inv::default();
            let mut n = 0;
            for (t, id, name) in items {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                if db.execute(
                    "INSERT OR IGNORE INTO param_lock(tbl,id,name) VALUES (?1,?2,?3)",
                    params![t, id, name],
                )? > 0
                {
                    n += 1;
                    inv = inv.sql(
                        "DELETE FROM param_lock WHERE tbl=?1 AND id=?2 AND name=?3",
                        lock_row(t, *id, name),
                    );
                }
            }
            if n == 0 {
                return invalid("lock: the value is already locked");
            }
            Ok((n, 0, inv))
        })
    }

    /// Unlocks parameter values; one undo step.
    pub fn clear_param_lock(&mut self, items: &[(&str, i64, &str)]) -> Result<usize, Error> {
        if !has_table(&self.db, LOCK_TABLE)? {
            return invalid("unlock: the value is not locked");
        }
        self.op("param_unlock", |db| {
            let mut inv = Inv::default();
            let mut n = 0;
            for (t, id, name) in items {
                let name = name.trim();
                let k = db.execute(
                    "DELETE FROM param_lock WHERE tbl=?1 AND id=?2 AND name=?3",
                    params![t, id, name],
                )?;
                if k > 0 {
                    n += k;
                    inv = inv.sql(
                        "INSERT OR IGNORE INTO param_lock(tbl,id,name) VALUES (?1,?2,?3)",
                        lock_row(t, *id, name),
                    );
                }
            }
            if n == 0 {
                return invalid("unlock: the value is not locked");
            }
            Ok((n, 0, inv))
        })
    }

    pub fn param_locked(&self, tbl: &str, id: i64, name: &str) -> Result<bool, Error> {
        is_locked_in(&self.db, tbl, id, name)
    }

    pub fn param_locks(&self) -> Result<Vec<LockKey>, Error> {
        if !has_table(&self.db, LOCK_TABLE)? {
            return Ok(vec![]);
        }
        let mut st = self
            .db
            .prepare("SELECT tbl,id,name FROM param_lock ORDER BY rowid")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }
}

/// A locked parameter value: (table of the row, row id, parameter name).
pub type LockKey = (String, i64, String);

const LOCK_TABLE: &str = "param_lock";
const LOCK_SQL: &str = "CREATE TABLE IF NOT EXISTS param_lock(tbl TEXT NOT NULL, \
     id INTEGER NOT NULL, name TEXT NOT NULL, PRIMARY KEY(tbl, id, name))";

fn lock_row(t: &str, id: i64, name: &str) -> Vec<Value> {
    vec![
        Value::Text(t.to_string()),
        Value::Integer(id),
        Value::Text(name.to_string()),
    ]
}

/// Is the value of parameter `name` of row (`tbl`, `id`) locked. The value writer refuses a
/// locked value, so a path that forgot `split_locked` fails instead of overwriting it.
pub(crate) fn is_locked_in(db: &Connection, tbl: &str, id: i64, name: &str) -> Result<bool, Error> {
    if !has_table(db, LOCK_TABLE)? {
        return Ok(false);
    }
    let n: i64 = db
        .prepare_cached("SELECT count(*) FROM param_lock WHERE tbl=?1 AND id=?2 AND name=?3")?
        .query_row(params![tbl, id, name.trim()], |r| r.get(0))?;
    Ok(n > 0)
}

/// [`is_locked_in`] by parameter id (paths that copy values by id).
pub(crate) fn is_locked_param(db: &Connection, tbl: &str, id: i64, param: i64) -> Result<bool, Error> {
    if !has_table(db, LOCK_TABLE)? {
        return Ok(false);
    }
    let n: i64 = db
        .prepare_cached(
            "SELECT count(*) FROM param_lock l JOIN pparam p ON p.name=l.name WHERE l.tbl=?1 AND l.id=?2 AND p.id=?3",
        )?
        .query_row(params![tbl, id, param], |r| r.get(0))?;
    Ok(n > 0)
}

/// Built-in column names per table, read once per call. A lock on such a name belongs to the
/// built-in field, not to a user parameter that happens to share the name.
#[derive(Default)]
struct Builtin(HashMap<String, HashSet<String>>);

impl Builtin {
    fn has(&mut self, db: &Connection, tbl: &str, name: &str) -> Result<bool, Error> {
        if !self.0.contains_key(tbl) {
            let mut st = db.prepare(&format!("PRAGMA table_info(\"{}\")", tbl.replace('"', "\"\"")))?;
            let v = st
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            self.0.insert(tbl.to_string(), v.iter().map(|c| c.to_lowercase()).collect());
        }
        Ok(self.0[tbl].contains(&name.trim().to_lowercase()))
    }
}

/// Ids of the parameters whose value on row (`tbl`, `id`) is locked (merge keeps them).
pub(crate) fn locked_param_ids(db: &Connection, tbl: &str, id: i64) -> Result<Vec<i64>, Error> {
    if !has_table(db, LOCK_TABLE)? {
        return Ok(vec![]);
    }
    let mut st = db.prepare_cached(
        "SELECT p.id FROM param_lock l JOIN pparam p ON p.name=l.name WHERE l.tbl=?1 AND l.id=?2",
    )?;
    let v = st
        .query_map(params![tbl, id], |r| r.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(v)
}

/// A parameter was renamed: its locks follow the new name (undo moves them back).
pub(crate) fn rename_locks(db: &Connection, old: &str, new: &str) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    if old == new || !has_table(db, LOCK_TABLE)? {
        return Ok(inv);
    }
    let mut cols = Builtin::default();
    for (t, id) in locks_named(db, old)? {
        if cols.has(db, &t, old)? {
            continue;
        }
        db.execute(
            "DELETE FROM param_lock WHERE tbl=?1 AND id=?2 AND name=?3",
            params![t, id, old],
        )?;
        if db.execute(
            "INSERT OR IGNORE INTO param_lock(tbl,id,name) VALUES (?1,?2,?3)",
            params![t, id, new],
        )? > 0
        {
            inv = inv.sql(
                "DELETE FROM param_lock WHERE tbl=?1 AND id=?2 AND name=?3",
                lock_row(&t, id, new),
            );
        }
        inv = inv.sql(
            "INSERT OR IGNORE INTO param_lock(tbl,id,name) VALUES (?1,?2,?3)",
            lock_row(&t, id, old),
        );
    }
    Ok(inv)
}

/// A parameter was deleted: its locks go with it (undo brings them back).
pub(crate) fn drop_locks(db: &Connection, name: &str) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    if !has_table(db, LOCK_TABLE)? {
        return Ok(inv);
    }
    let mut cols = Builtin::default();
    for (t, id) in locks_named(db, name)? {
        if cols.has(db, &t, name)? {
            continue;
        }
        db.execute(
            "DELETE FROM param_lock WHERE tbl=?1 AND id=?2 AND name=?3",
            params![t, id, name],
        )?;
        inv = inv.sql(
            "INSERT OR IGNORE INTO param_lock(tbl,id,name) VALUES (?1,?2,?3)",
            lock_row(&t, id, name),
        );
    }
    Ok(inv)
}

fn locks_named(db: &Connection, name: &str) -> Result<Vec<(String, i64)>, Error> {
    let mut st = db.prepare("SELECT tbl,id FROM param_lock WHERE name=?1 ORDER BY rowid")?;
    let v = st
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// Removes locks whose row is gone, so a reused rowid never inherits one: one set-based DELETE
/// per lock table. Locks on a table that does not exist or has no rowid are left alone. Undo
/// restores the rows.
pub(crate) fn prune_locks(db: &Connection) -> Result<Inv, Error> {
    Ok(prune_tables(db)?.0)
}

/// [`prune_locks`] plus the number of DELETE statements it ran.
fn prune_tables(db: &Connection) -> Result<(Inv, usize), Error> {
    let mut inv = Inv::default();
    let mut stmts = 0;
    if !has_table(db, LOCK_TABLE)? {
        return Ok((inv, stmts));
    }
    let tables: Vec<String> = {
        let mut st = db.prepare_cached("SELECT DISTINCT tbl FROM param_lock")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
        v
    };
    for t in tables {
        if !has_table(db, &t)? {
            continue;
        }
        // a table without a rowid cannot be checked: its locks stay
        let sql = format!(
            "DELETE FROM param_lock WHERE tbl=?1 AND NOT EXISTS (SELECT 1 FROM \"{}\" WHERE rowid=param_lock.id) RETURNING id,name",
            t.replace('"', "\"\"")
        );
        let Ok(mut st) = db.prepare(&sql) else {
            continue;
        };
        let gone = st
            .query_map([&t], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        stmts += 1;
        for (id, name) in gone {
            inv = inv.sql(
                "INSERT OR IGNORE INTO param_lock(tbl,id,name) VALUES (?1,?2,?3)",
                lock_row(&t, id, &name),
            );
        }
    }
    Ok((inv, stmts))
}

fn pinnable(kind: &str) -> bool {
    PIN_TABLES
        .iter()
        .chain(clip::reg::LANE_PINS)
        .any(|t| t.0 == kind)
        || PIN_VERTICES.iter().any(|t| t.2 == kind)
}

/// Instance member rows from the snapshot and its map of new ids.
fn add_members(db: &Connection, inst: i64, def: &Clip, p: &Paster) -> Result<usize, Error> {
    let mut n = 0;
    for (k, key) in clip::members(def) {
        if let Some(r) = p.map.get(&k).and_then(|m| m.get(&key)) {
            n += db.execute(
                "INSERT OR IGNORE INTO group_member(inst,kind,ref,key) VALUES (?1,?2,?3,?4)",
                params![inst, k, r, key],
            )?;
        }
    }
    Ok(n)
}

#[cfg(test)]
#[path = "param_lock_tests.rs"]
mod lock_tests;

#[cfg(test)]
#[path = "groups_lock_tests.rs"]
mod groups_lock_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_tables_have_unique_names() {
        let mut t: Vec<&str> = PIN_TABLES.iter().map(|x| x.0).collect();
        t.sort_unstable();
        let n = t.len();
        t.dedup();
        assert_eq!(n, t.len());
    }

    #[test]
    fn triggers_install_on_a_fresh_model() {
        let m = Model::open(":memory:").unwrap();
        let n: i64 = m
            .sql()
            .query_row(
                "SELECT count(*) FROM sqlite_temp_master WHERE type='trigger' AND name LIKE 'ddc_pin_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(n > 0);
    }
}
