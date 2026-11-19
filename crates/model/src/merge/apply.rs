// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Applies a merge inside one journal operation. Each changed row is snapshotted once before the
//! edit; new rows are covered by the watermark, while part groups and restored rows with the
//! same id get an explicit delete at the start of the inverse commands.

use std::collections::HashMap;

use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};

use super::analyze::{
    canon_text, lookup, map_lookup, text_ref, Analysis, DELETE, DELETE_CASCADE, RESTORE, TAKE,
};
use super::tables::{canon, q, ChildTable, Owner, RefCol, Schema, Table};
use super::{MergeCase, MergeReport, Prepared};
use crate::source::Pick;
use crate::{Error, Snapshots};

/// Components on a host are deleted with it even though the reference is optional.
pub(crate) const HOSTED: [(&str, &str); 3] = [
    ("comp", "host_wall"),
    ("comp", "host_slab"),
    ("comp", "host_ceiling"),
];

fn delete_kids(
    db: &Connection,
    s: &Schema,
    snaps: &mut Snapshots,
    parent: &str,
    id: i64,
) -> Result<(), Error> {
    for (_, c) in s.kids_of(parent) {
        let (cq, o) = (q(c.spec.table), q(c.owner_col()));
        snaps.save(db, c.spec.table, c.owner_col(), id)?;
        let f = c.kind_filter(&cq, parent);
        db.execute(&format!("DELETE FROM {cq} WHERE {cq}.{o}=?1{f}"), [id])?;
    }
    Ok(())
}

/// Deletes an element row with its dependents (snapshots go to `snaps`); elements that require it
/// or are hosted on it go with it, optional references are nulled. Returns 0 if the row is already gone.
pub(crate) fn drop_row(
    db: &Connection,
    s: &Schema,
    snaps: &mut Snapshots,
    table: &str,
    id: i64,
) -> Result<usize, Error> {
    let Some(t) = s.table(table) else {
        return Ok(0);
    };
    let tq = q(&t.name);
    let found = db
        .query_row(&format!("SELECT 1 FROM {tq} WHERE id=?1"), [id], |_| Ok(()))
        .optional()?;
    if found.is_none() {
        return Ok(0);
    }
    let mut n = 1;
    let mut cascade: Vec<(String, i64)> = vec![];
    for d in &s.tables {
        let dq = q(&d.name);
        for r in &d.refs {
            match r {
                RefCol::To { col, parent } if parent == table => {
                    let cq = q(col);
                    let ids: Vec<i64> = db
                        .prepare(&format!("SELECT id FROM {dq} WHERE {cq}=?1"))?
                        .query_map([id], |x| x.get(0))?
                        .collect::<Result<_, _>>()?;
                    let hard = d.notnull.contains(col)
                        || HOSTED.iter().any(|(a, b)| *a == d.name && b == col);
                    for i in ids {
                        if hard {
                            cascade.push((d.name.clone(), i));
                        } else {
                            snaps.save(db, &d.name, "id", i)?;
                            db.execute(&format!("UPDATE {dq} SET {cq}=NULL WHERE id=?1"), [i])?;
                        }
                    }
                }
                RefCol::Poly { kind, col } => {
                    let ids: Vec<i64> = db
                        .prepare(&format!(
                            "SELECT id FROM {dq} WHERE {}=?1 AND {}=?2",
                            q(kind),
                            q(col)
                        ))?
                        .query_map(params![table, id], |x| x.get(0))?
                        .collect::<Result<_, _>>()?;
                    cascade.extend(ids.into_iter().map(|i| (d.name.clone(), i)));
                }
                _ => {}
            }
        }
    }
    for (dt, i) in cascade {
        if !(dt == table && i == id) {
            n += drop_row(db, s, snaps, &dt, i)?;
        }
    }
    delete_kids(db, s, snaps, table, id)?;
    // parts of other elements pointing here (a wall attached to this slab)
    for c in &s.children {
        for r in &c.refs {
            match r {
                RefCol::To { col, parent } if parent == table && col != c.owner_col() => {
                    snaps.save(db, c.spec.table, col, id)?;
                    db.execute(
                        &format!("DELETE FROM {} WHERE {}=?1", q(c.spec.table), q(col)),
                        [id],
                    )?;
                }
                RefCol::Poly { kind, col } if col != c.owner_col() => {
                    snaps.save(db, c.spec.table, col, id)?;
                    db.execute(
                        &format!(
                            "DELETE FROM {} WHERE {}=?1 AND {}=?2",
                            q(c.spec.table),
                            q(col),
                            q(kind)
                        ),
                        params![id, table],
                    )?;
                }
                _ => {}
            }
        }
    }
    if table == "wall" {
        use crate::constraints::Ref;
        for (cid, rule) in crate::constraints::load(db)? {
            if rule
                .refs()
                .iter()
                .any(|r| matches!(r, Ref::Wall(w) if *w == id))
            {
                snaps.save(db, "constraint_rule", "id", cid)?;
                db.execute("DELETE FROM constraint_rule WHERE id=?1", [cid])?;
            }
        }
    }
    snaps.save(db, &t.name, "id", id)?;
    db.execute(&format!("DELETE FROM {tq} WHERE id=?1"), [id])?;
    Ok(n)
}

fn set_cols(db: &Connection, t: &Table, cols: &[String], b: i64, mine: i64) -> Result<(), Error> {
    if cols.is_empty() {
        return Ok(());
    }
    let names = cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(",");
    let vals = cols
        .iter()
        .map(|c| canon(&t.refs, "t", c))
        .collect::<Vec<_>>()
        .join(",");
    let nq = q(&t.name);
    db.execute(
        &format!(
            "UPDATE main.{nq} SET ({names}) = (SELECT {vals} FROM their.{nq} t WHERE t.id=?1) WHERE id=?2"
        ),
        params![b, mine],
    )?;
    Ok(())
}

/// Replaces a part group (or one of its rows by key) with rows from the peer's file.
#[allow(clippy::too_many_arguments)]
fn replace_kids(
    db: &Connection,
    snaps: &mut Snapshots,
    c: &ChildTable,
    parent: &str,
    b: i64,
    mine: i64,
    key: Option<&str>,
    rep: &mut MergeReport,
) -> Result<(), Error> {
    let cq = q(c.spec.table);
    let o = q(c.owner_col());
    snaps.save(db, c.spec.table, c.owner_col(), mine)?;
    let mut undo = format!("DELETE FROM {cq} WHERE {o}=?1");
    let mut undo_args = vec![Value::Integer(mine)];
    if let Owner::Poly(k, _) = c.spec.owner {
        undo += &format!(" AND {}=?2", q(k));
        undo_args.push(Value::Text(parent.to_string()));
    }
    snaps.push_front(&undo, undo_args);
    let kind_m = c.kind_filter("main_row", parent);
    let (key_del, key_ins) = match (c.spec.key, key) {
        (Some(k), Some(v)) if !v.is_empty() => (
            // id-valued key (`param`) compares in this file's id space on both sides
            format!(" AND CAST(main_row.{} AS TEXT)=?2", q(k)),
            format!(" AND CAST({} AS TEXT)=?3", canon(&c.refs, "r", k)),
        ),
        _ => (" AND ?2=?2".to_string(), " AND ?3=?3".to_string()),
    };
    let val = key.unwrap_or("");
    // a locked parameter value of this row stays as it is: neither replaced nor removed
    let locked = if c.spec.table == "pparam_val" {
        crate::groups::locked_param_ids(db, parent, mine)?
    } else {
        vec![]
    };
    let (lock_del, lock_ins) = if locked.is_empty() {
        (String::new(), String::new())
    } else {
        let list = locked.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let theirs_param = canon(&c.refs, "r", "param");
        let kind_t = c.kind_filter("r", parent);
        let key_ins = key_ins.as_str();
        let key_del3 = key_del.replace("?2", "?3");
        let changed: i64 = db.query_row(
            &format!(
                "SELECT count(*) FROM their.{cq} r WHERE r.{o}=?1{kind_t}{key_ins} \
                 AND {theirs_param} IN ({list}) AND NOT EXISTS (SELECT 1 FROM main.{cq} main_row \
                 WHERE main_row.{o}=?2{kind_m} AND main_row.param={theirs_param} \
                 AND main_row.value IS r.value)"
            ),
            params![b, mine, val],
            |r| r.get(0),
        )?;
        let gone: i64 = db.query_row(
            &format!(
                "SELECT count(*) FROM main.{cq} main_row WHERE main_row.{o}=?2{kind_m}{key_del3} \
                 AND main_row.param IN ({list}) AND NOT EXISTS (SELECT 1 FROM their.{cq} r \
                 WHERE r.{o}=?1{kind_t} AND {theirs_param}=main_row.param)"
            ),
            params![b, mine, val],
            |r| r.get(0),
        )?;
        rep.locked += (changed + gone) as usize;
        (
            format!(" AND main_row.param NOT IN ({list})"),
            format!(" AND {theirs_param} NOT IN ({list})"),
        )
    };
    db.execute(
        &format!(
            "DELETE FROM {cq} WHERE rowid IN (SELECT main_row.rowid FROM main.{cq} main_row \
             WHERE main_row.{o}=?1{kind_m}{key_del}{lock_del})"
        ),
        params![mine, val],
    )?;
    let names = c.cols.iter().map(|x| q(x)).collect::<Vec<_>>().join(",");
    let vals = c
        .cols
        .iter()
        .map(|x| {
            if x == c.owner_col() {
                "?2".to_string()
            } else {
                canon(&c.refs, "r", x)
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let kind_t = c.kind_filter("r", parent);
    db.execute(
        &format!(
            "INSERT OR IGNORE INTO main.{cq}({names}) SELECT {vals} FROM their.{cq} r \
             WHERE r.{o}=?1{kind_t}{key_ins}{lock_ins}"
        ),
        params![b, mine, val],
    )?;
    Ok(())
}

/// How many declared references point nowhere.
fn dangling(db: &Connection, s: &Schema) -> Result<i64, Error> {
    let mut n = 0;
    for t in &s.tables {
        for r in &t.refs {
            if let RefCol::To { col, parent } = r {
                if s.table(parent).is_some() {
                    let c = q(col);
                    n += db.query_row(
                        &format!(
                            "SELECT count(*) FROM main.{0} WHERE {c} IS NOT NULL AND {c} NOT IN (SELECT id FROM main.{1})",
                            q(&t.name),
                            q(parent)
                        ),
                        [],
                        |x| x.get::<_, i64>(0),
                    )?;
                }
            }
        }
    }
    Ok(n)
}

fn picks_to_acts(
    db: &Connection,
    a: &Analysis,
    picks: &HashMap<(String, i64), Pick>,
    rep: &mut MergeReport,
) -> Result<(), Error> {
    let mut st = db.prepare("UPDATE temp.mg_ch SET act=?1 WHERE tbl=?2 AND mine=?3")?;
    for c in &a.plan.conflicts {
        let pick = picks
            .get(&(c.table.clone(), c.id))
            .copied()
            .unwrap_or(Pick::Mine);
        match pick {
            Pick::Mine => rep.kept_mine += 1,
            Pick::Theirs => rep.took_theirs += 1,
        }
        let act = match (c.case, pick) {
            (MergeCase::DeletedThere, Pick::Theirs) => DELETE_CASCADE,
            (MergeCase::DeletedHere, Pick::Theirs) => RESTORE,
            (MergeCase::DeletedThere | MergeCase::DeletedHere, Pick::Mine) => 0,
            // field conflicts are applied from `Take`
            _ => continue,
        };
        st.execute(params![act, c.table, c.id])?;
    }
    // host kept here although deleted there: what hangs on it stays too
    for t in &a.schema.tables {
        for r in &t.refs {
            let RefCol::To { col, parent } = r else {
                continue;
            };
            if a.schema.table(parent).is_none() {
                continue;
            }
            db.execute(
                &format!(
                    "UPDATE temp.mg_ch SET act=0 WHERE tbl='{c}' AND act={DELETE} AND mine IN \
                     (SELECT r.id FROM main.{cq} r WHERE r.{col} IN \
                      (SELECT z.mine FROM temp.mg_ch z WHERE z.tbl='{parent}' AND z.t=2 AND z.act=0))",
                    c = t.name,
                    cq = q(&t.name),
                    col = q(col),
                ),
                [],
            )?;
        }
    }
    Ok(())
}

fn act_rows(db: &Connection, table: &str, acts: &str) -> Result<Vec<(i64, i64)>, Error> {
    let rows = db
        .prepare(&format!(
            "SELECT b, mine FROM temp.mg_ch WHERE tbl=?1 AND act IN ({acts}) ORDER BY b"
        ))?
        .query_map([table], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

fn fix_text(db: &Connection, t: &Table, ids: &[i64]) -> Result<(), Error> {
    let Some((col, kind_col)) = text_ref(&t.name) else {
        return Ok(());
    };
    if ids.is_empty() || !t.cols.iter().any(|c| c == col) {
        return Ok(());
    }
    let nq = q(&t.name);
    let kind_sel = kind_col.map_or("NULL".to_string(), q);
    let mut read = db.prepare(&format!(
        "SELECT {}, {kind_sel} FROM main.{nq} WHERE id=?1",
        q(col)
    ))?;
    let mut write = db.prepare(&format!("UPDATE main.{nq} SET {}=?2 WHERE id=?1", q(col)))?;
    let mut mapst = map_lookup(db)?;
    for &id in ids {
        let row: Option<(Option<String>, Option<String>)> = read
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((Some(text), kind)) = row else {
            continue;
        };
        let fixed = canon_text(&t.name, col, &text, kind.as_deref(), &mut |tb, i| {
            lookup(&mut mapst, tb, i)
        })?;
        if fixed != text {
            write.execute(params![id, fixed])?;
        }
    }
    Ok(())
}

fn restore_rows(
    db: &Connection,
    s: &Schema,
    snaps: &mut Snapshots,
    rep: &mut MergeReport,
) -> Result<(), Error> {
    for t in &s.tables {
        let rows = act_rows(db, &t.name, &RESTORE.to_string())?;
        if rows.is_empty() {
            continue;
        }
        let nq = q(&t.name);
        let (mut nl, mut vl) = (String::new(), String::new());
        for c in &t.cols {
            nl += &format!(",{}", q(c));
            vl += &format!(",{}", canon(&t.refs, "t", c));
        }
        let sql =
            format!("INSERT INTO main.{nq}(id{nl}) SELECT ?2{vl} FROM their.{nq} t WHERE t.id=?1");
        for (b, mine) in &rows {
            snaps.push_front(
                &format!("DELETE FROM {nq} WHERE id=?1"),
                vec![Value::Integer(*mine)],
            );
            db.execute(&sql, params![b, mine])?;
            for (_, c) in s.kids_of(&t.name) {
                replace_kids(db, snaps, c, &t.name, *b, *mine, None, rep)?;
            }
            rep.restored += 1;
        }
        fix_text(db, t, &rows.iter().map(|x| x.1).collect::<Vec<_>>())?;
    }
    Ok(())
}

fn take_rows(
    db: &Connection,
    s: &Schema,
    snaps: &mut Snapshots,
    rep: &mut MergeReport,
) -> Result<(), Error> {
    for t in &s.tables {
        let rows = act_rows(db, &t.name, &TAKE.to_string())?;
        if rows.is_empty() {
            continue;
        }
        let mut changed_kids =
            db.prepare("SELECT c FROM temp.mg_cd WHERE side='t' AND k=?1 AND o=?2")?;
        for (b, mine) in &rows {
            snaps.save(db, &t.name, "id", *mine)?;
            set_cols(db, t, &t.cols, *b, *mine)?;
            let kids: Vec<i64> = changed_kids
                .query_map(params![t.name, b], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for ci in kids {
                replace_kids(
                    db,
                    snaps,
                    &s.children[ci as usize],
                    &t.name,
                    *b,
                    *mine,
                    None,
                    rep,
                )?;
            }
            rep.changed += 1;
        }
        fix_text(db, t, &rows.iter().map(|x| x.1).collect::<Vec<_>>())?;
    }
    Ok(())
}

fn field_takes(
    db: &Connection,
    a: &Analysis,
    snaps: &mut Snapshots,
    picks: &HashMap<(String, i64), Pick>,
    rep: &mut MergeReport,
) -> Result<(), Error> {
    let s = &a.schema;
    for k in &a.takes {
        let t = &s.tables[k.table];
        let theirs = picks.get(&(t.name.clone(), k.mine)).copied() == Some(Pick::Theirs);
        let mut cols = k.cols.clone();
        let mut kids = k.kids.clone();
        if theirs {
            cols.extend(k.conflict_cols.iter().cloned());
            kids.extend(k.conflict_kids.iter().cloned());
        }
        if cols.is_empty() && kids.is_empty() {
            continue;
        }
        snaps.save(db, &t.name, "id", k.mine)?;
        set_cols(db, t, &cols, k.b, k.mine)?;
        for (ci, key) in &kids {
            replace_kids(
                db,
                snaps,
                &s.children[*ci],
                &t.name,
                k.b,
                k.mine,
                Some(key.as_str()),
                rep,
            )?;
        }
        if text_ref(&t.name).is_some_and(|(c, _)| cols.iter().any(|x| x == c)) {
            fix_text(db, t, &[k.mine])?;
        }
        rep.changed += 1;
    }
    Ok(())
}

fn insert_new(db: &Connection, s: &Schema, rep: &mut MergeReport) -> Result<(), Error> {
    for t in &s.tables {
        let n = &t.name;
        let nq = q(n);
        let total: i64 =
            db.query_row("SELECT count(*) FROM temp.mg_add WHERE tbl=?1", [n], |r| {
                r.get(0)
            })?;
        if total == 0 {
            continue;
        }
        let (mut names, mut vals) = (String::new(), String::new());
        for c in &t.cols {
            names += &format!(",{}", q(c));
            vals += &format!(",{}", canon(&t.refs, "t", c));
        }
        let mut guard = String::new();
        for r in &t.refs {
            if let RefCol::To { col, parent } = r {
                // a self reference points at a row of this same insert; the dangling check covers it
                if s.table(parent).is_some() && parent != n {
                    guard += &format!(
                        " AND (t.{c} IS NULL OR {v} IN (SELECT id FROM main.{p}))",
                        c = q(col),
                        v = canon(&t.refs, "t", col),
                        p = q(parent)
                    );
                }
            }
        }
        let done = db.execute(
            &format!(
                "INSERT INTO main.{nq}(id{names}) SELECT m.mine{vals} FROM temp.mg_add a \
                 JOIN their.{nq} t ON t.id=a.their JOIN temp.mg_map m ON m.tbl=a.tbl AND m.their=a.their \
                 WHERE a.tbl='{n}'{guard}"
            ),
            [],
        )? as i64;
        rep.added += done as usize;
        rep.skipped += (total - done) as usize;
        for (_, c) in s.kids_of(n) {
            let (cq, o) = (q(c.spec.table), q(c.owner_col()));
            let cols = c.cols.iter().map(|x| q(x)).collect::<Vec<_>>().join(",");
            let vs = c
                .cols
                .iter()
                .map(|x| canon(&c.refs, "r", x))
                .collect::<Vec<_>>()
                .join(",");
            let kind = c.kind_filter("r", n);
            db.execute(
                &format!(
                    "INSERT OR IGNORE INTO main.{cq}({cols}) SELECT {vs} FROM their.{cq} r \
                     JOIN temp.mg_map m ON m.tbl='{n}' AND m.their=r.{o} \
                     WHERE r.{o} IN (SELECT a.their FROM temp.mg_add a WHERE a.tbl='{n}'){kind} \
                     AND m.mine IN (SELECT id FROM main.{nq})"
                ),
                [],
            )?;
        }
        let new_ids: Vec<i64> = db
            .prepare(
                "SELECT m.mine FROM temp.mg_add a JOIN temp.mg_map m ON m.tbl=a.tbl AND m.their=a.their WHERE a.tbl=?1",
            )?
            .query_map([n], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        fix_text(db, t, &new_ids)?;
        // skipped rows are not remembered: the next merge offers them again
        db.execute(
            &format!(
                "DELETE FROM temp.mg_map WHERE tbl='{n}' AND their IN (SELECT a.their FROM temp.mg_add a WHERE a.tbl='{n}') \
                 AND mine NOT IN (SELECT id FROM main.{nq})"
            ),
            [],
        )?;
    }
    Ok(())
}

pub(crate) fn apply(
    db: &Connection,
    snaps: &mut Snapshots,
    p: &Prepared,
    a: &Analysis,
    picks: &HashMap<(String, i64), Pick>,
) -> Result<MergeReport, Error> {
    let s = &a.schema;
    let mut rep = MergeReport::default();
    let broken_before = dangling(db, s)?;
    for t in &s.tables {
        snaps.watermark(db, &t.name)?;
    }
    for c in &s.children {
        snaps.watermark(db, c.spec.table)?;
    }
    snaps.watermark(db, "merge_map")?;
    snaps.watermark(db, "merge_peer")?;
    picks_to_acts(db, a, picks, &mut rep)?;

    for t in s.tables.iter().rev() {
        for (_, mine) in act_rows(db, &t.name, &format!("{DELETE},{DELETE_CASCADE}"))? {
            rep.deleted += drop_row(db, s, snaps, &t.name, mine)?;
        }
    }
    restore_rows(db, s, snaps, &mut rep)?;
    take_rows(db, s, snaps, &mut rep)?;
    field_takes(db, a, snaps, picks, &mut rep)?;
    insert_new(db, s, &mut rep)?;

    let broken = dangling(db, s)?;
    if broken > broken_before {
        return Err(Error::Conflict(format!(
            "merge would leave {} broken references",
            broken - broken_before
        )));
    }
    remember(db, snaps, p)?;
    Ok(rep)
}

/// Merge point: the next merge with a continuation of this file starts here.
fn remember(db: &Connection, snaps: &mut Snapshots, p: &Prepared) -> Result<(), Error> {
    let at = p.at;
    let path = p.source.to_string_lossy().into_owned();
    let peer = match p.peer {
        Some(id) => {
            snaps.save(db, "merge_peer", "id", id)?;
            db.execute(
                "UPDATE merge_peer SET path=?2, their_seq=?3, their_hash=?4, at=?5 WHERE id=?1",
                params![id, path, p.tip_seq, p.tip_hash, at],
            )?;
            id
        }
        None => {
            db.execute(
                "INSERT INTO merge_peer(path, their_seq, their_hash, at) VALUES (?1,?2,?3,?4)",
                params![path, p.tip_seq, p.tip_hash, at],
            )?;
            db.last_insert_rowid()
        }
    };
    db.execute(
        "INSERT OR IGNORE INTO merge_map(peer,tbl,their,mine) SELECT ?1, tbl, their, mine FROM temp.mg_map",
        [peer],
    )?;
    Ok(())
}
