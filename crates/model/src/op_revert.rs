// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Selective revert of an older undo step. The undo log is a stack, so an older step can be taken
//! back alone only when nothing newer touched the rows it touched. Both sides are measured, not
//! parsed: the inverse commands of the step and of every newer step are run in a transaction that
//! is rolled back, and the change hook reports the rows each one writes. Disjoint row sets (and no
//! new dangling references) make the revert safe; the revert is one new undo step of its own.

use std::collections::{HashMap, HashSet};

use rusqlite::{types::Value, OptionalExtension};

use crate::ops::{log, run_inv, scope, Inv, Meta};
use crate::{Error, Model};

/// More newer steps than this are not simulated (each one is replayed).
const MAX_NEWER_OPS: i64 = 400;

type Rows = HashMap<String, HashSet<i64>>;

/// Why a step can or cannot be reverted on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevertCheck {
    Ready,
    /// The step is not in the undo log any more (undone, or trimmed).
    Gone,
    /// Taking it back would change nothing.
    Empty,
    /// A newer step changed a row of this table that the step also changed.
    Conflict(String),
    /// Too many newer steps to compare.
    TooMany,
    /// Its inverse cannot be replayed on the current model, or would leave dangling references.
    Unsafe,
}

impl Model {
    /// Rows written by running `invs` in order; nothing is kept.
    fn rows_of_inverse(&mut self, invs: &[String]) -> Result<Rows, Error> {
        let since = self.change_head();
        let tx = scope(&mut self.db)?;
        for inv in invs {
            run_inv(&tx, inv, Some(&*self.tracker))?;
        }
        // `tx` drops at the end of the function: rolled back
        crate::changes::dirty(&self.tracker, since)
            .ok_or_else(|| Error::Invalid("change window lost".into()))
    }

    fn revert_plan(&mut self, seq: i64) -> Result<(RevertCheck, String, Rows), Error> {
        let none = |c| Ok((c, String::new(), Rows::new()));
        let inv: Option<String> = self
            .db
            .query_row("SELECT inv FROM op WHERE seq=?1", [seq], |r| r.get(0))
            .optional()?;
        let Some(inv) = inv else {
            return none(RevertCheck::Gone);
        };
        let newer: i64 = self
            .db
            .query_row("SELECT count(*) FROM op WHERE seq>?1", [seq], |r| r.get(0))?;
        if newer > MAX_NEWER_OPS {
            return none(RevertCheck::TooMany);
        }
        let later: Vec<String> = self
            .db
            .prepare("SELECT inv FROM op WHERE seq>?1 ORDER BY seq DESC")?
            .query_map([seq], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let Ok(mine) = self.rows_of_inverse(std::slice::from_ref(&inv)) else {
            return none(RevertCheck::Unsafe);
        };
        if mine.values().all(HashSet::is_empty) {
            return none(RevertCheck::Empty);
        }
        let Ok(theirs) = self.rows_of_inverse(&later) else {
            return none(RevertCheck::Unsafe);
        };
        let mut tables: Vec<&String> = mine.keys().collect();
        tables.sort();
        for t in tables {
            if let Some(o) = theirs.get(t) {
                if mine[t].iter().any(|id| o.contains(id)) {
                    return none(RevertCheck::Conflict(t.clone()));
                }
            }
        }
        Ok((RevertCheck::Ready, inv, mine))
    }

    /// Can undo step `seq` be taken back on its own? Changes nothing.
    pub fn revert_check(&mut self, seq: i64) -> Result<RevertCheck, Error> {
        Ok(self.revert_plan(seq)?.0)
    }

    /// Takes back undo step `seq` alone, as one new undo step. The old step stays in the log as
    /// an empty one so the steps above and below keep their order; undoing the revert restores
    /// it. Fails without any change unless [`Model::revert_check`] says `Ready`.
    pub fn revert_op(&mut self, seq: i64) -> Result<(), Error> {
        let (check, inv, mine) = self.revert_plan(seq)?;
        if check != RevertCheck::Ready {
            return Err(Error::Invalid(format!("revert refused: {check:?}")));
        }
        crate::struct_ext::clear_stash();
        let tx = scope(&mut self.db)?;
        let flagged: Vec<i64> = tx
            .prepare("SELECT seq FROM ai_audit WHERE op_seq=?1 AND op_undone=0")?
            .query_map([seq], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        // the revert's own inverse, run top to bottom by undo: the old step's commands and
        // journal flags come back, then the rows it touched are put as they are now
        let mut back = Inv::default().sql(
            "UPDATE op SET inv=?1 WHERE seq=?2",
            vec![Value::Text(inv.clone()), Value::Integer(seq)],
        );
        for s in flagged {
            back = back.sql(
                "UPDATE ai_audit SET op_undone=0 WHERE seq=?1",
                vec![Value::Integer(s)],
            );
        }
        let mut tables: Vec<(&String, Vec<i64>)> = mine
            .iter()
            .map(|(t, ids)| {
                let mut v: Vec<i64> = ids.iter().copied().collect();
                v.sort_unstable();
                (t, v)
            })
            .collect();
        tables.sort();
        for (t, ids) in &tables {
            for id in ids {
                back = back.delete(t, "rowid", *id);
            }
        }
        for (t, ids) in &tables {
            for id in ids {
                back = back.restore(&tx, t, "rowid", *id)?;
            }
        }
        let dangling = |db: &rusqlite::Connection| -> Result<usize, Error> {
            Ok(db
                .prepare("PRAGMA foreign_key_check")?
                .query_map([], |_| Ok(()))?
                .count())
        };
        let before = dangling(&tx)?;
        run_inv(&tx, &inv, Some(&*self.tracker))?;
        if dangling(&tx)? > before {
            return Err(Error::Invalid("revert refused: dangling references".into()));
        }
        tx.execute("UPDATE op SET inv='[]' WHERE seq=?1", [seq])?;
        crate::ai_audit::note_undo(&tx, seq)?;
        crate::walls::LENIENT.with(|l| l.set(true));
        let regen = crate::regen::logged(&tx, &self.tracker, &mut self.regen);
        crate::walls::LENIENT.with(|l| l.set(false));
        let back = regen?.then(back);
        let meta = Meta {
            name: None,
            old: None,
            new: None,
        };
        log(&tx, "revert", 0, meta, &back)?;
        tx.commit()?;
        self.regen.promote(self.change_head());
        self.adopt_struct();
        Ok(())
    }

    /// Rows undo step `seq` touched, as `(table, id)`, read from its inverse commands only:
    /// nothing is run or written. Rows the step inserted under a batch watermark are the ones
    /// above that mark and below the next newer mark of the same table. Empty when the step is gone.
    pub fn op_element_ids(&self, seq: i64) -> Result<Vec<(String, i64)>, Error> {
        let inv: Option<String> = self
            .db
            .query_row("SELECT inv FROM op WHERE seq=?1", [seq], |r| r.get(0))
            .optional()?;
        let Some(inv) = inv else {
            return Ok(vec![]);
        };
        let mut out: Vec<(String, i64)> = vec![];
        let mut marks: Vec<(String, i64)> = vec![];
        for (sql, args) in decode_inv(&inv) {
            if let Some(m) = watermark_of(&sql, &args) {
                marks.push(m);
            } else if let Some(r) = row_of(&sql, &args) {
                out.push(r);
            }
        }
        if !marks.is_empty() {
            let newer: Vec<String> = self
                .db
                .prepare("SELECT inv FROM op WHERE seq>?1 ORDER BY seq LIMIT ?2")?
                .query_map(rusqlite::params![seq, MAX_NEWER_OPS], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let mut upper: HashMap<String, i64> = HashMap::new();
            // rows a newer step inserted (its undo deletes them by id): not this step's
            let mut theirs: HashSet<(String, i64)> = HashSet::new();
            for inv in &newer {
                for (sql, args) in decode_inv(inv) {
                    if let Some((t, m)) = watermark_of(&sql, &args) {
                        let e = upper.entry(t).or_insert(m);
                        *e = (*e).min(m);
                    } else if sql.starts_with("DELETE FROM ") {
                        if let Some(r) = row_of(&sql, &args) {
                            theirs.insert(r);
                        }
                    }
                }
            }
            for (t, lo) in marks {
                let there: bool = self.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1)",
                    [&t],
                    |r| r.get(0),
                )?;
                if !there {
                    continue;
                }
                let hi = upper.get(&t).copied().unwrap_or(i64::MAX);
                let tq = format!("\"{}\"", t.replace('"', "\"\""));
                let ids: Vec<i64> = self
                    .db
                    .prepare(&format!(
                        "SELECT rowid FROM main.{tq} WHERE rowid>?1 AND rowid<=?2 ORDER BY rowid LIMIT ?3"
                    ))?
                    .query_map(rusqlite::params![lo, hi, MAX_IDS as i64], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                out.extend(
                    ids.into_iter()
                        .map(|id| (t.clone(), id))
                        .filter(|r| !theirs.contains(r)),
                );
            }
        }
        out.retain(|(t, _)| !bookkeeping(t));
        let mut seen = HashSet::new();
        out.retain(|r| seen.insert(r.clone()));
        out.truncate(MAX_IDS);
        Ok(out)
    }
}

/// A step's row list is cut here (a large import is one step).
const MAX_IDS: usize = 100_000;

/// Tables of the file's own bookkeeping, never a model element.
fn bookkeeping(t: &str) -> bool {
    t == "op" || t.starts_with("ai_") || t.starts_with("sqlite_")
}

/// The commands of one undo-log row as `(sql, args)`; a damaged row reads as none.
fn decode_inv(inv: &str) -> Vec<(String, Vec<Value>)> {
    let Ok(serde_json::Value::Array(stmts)) = serde_json::from_str::<serde_json::Value>(inv) else {
        return vec![];
    };
    stmts
        .iter()
        .filter_map(|s| {
            let sql = s.get(0)?.as_str()?.to_string();
            let args = s
                .get(1)
                .and_then(|a| a.as_array())
                .map_or(vec![], |a| a.iter().map(crate::ops::from_json).collect());
            Some((sql, args))
        })
        .collect()
}

fn table_word(s: &str) -> Option<String> {
    let w = s
        .trim_start()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()?;
    (!w.is_empty()).then(|| w.to_string())
}

/// `DELETE FROM t WHERE rowid>?1`: the batch watermark of table `t`.
fn watermark_of(sql: &str, args: &[Value]) -> Option<(String, i64)> {
    let t = sql.strip_prefix("DELETE FROM ")?.strip_suffix(" WHERE rowid>?1")?;
    match args.first() {
        Some(Value::Integer(m)) => Some((t.to_string(), *m)),
        _ => None,
    }
}

/// The row one inverse command names by id: `DELETE FROM t WHERE id=?k`, `UPDATE t .. WHERE
/// id=?k` and `INSERT [OR REPLACE] INTO t(rowid|id, ..)`. Anything else names no row.
fn row_of(sql: &str, args: &[Value]) -> Option<(String, i64)> {
    let int = |k: usize| match args.get(k) {
        Some(Value::Integer(i)) => Some(*i),
        _ => None,
    };
    let insert = sql
        .strip_prefix("INSERT OR REPLACE INTO ")
        .or_else(|| sql.strip_prefix("INSERT INTO "));
    if let Some(rest) = insert {
        let table = table_word(rest)?;
        let open = rest.find('(')?;
        let close = open + rest[open..].find(')')?;
        let pos = rest[open + 1..close]
            .split(',')
            .position(|c| matches!(c.trim(), "rowid" | "id"))?;
        return Some((table, int(pos)?));
    }
    let rest = sql
        .strip_prefix("DELETE FROM ")
        .or_else(|| sql.strip_prefix("UPDATE "))?;
    let table = table_word(rest)?;
    let cond = rest.rsplit_once(" WHERE ")?.1.trim();
    let (col, ph) = cond.split_once('=')?;
    if !matches!(col.trim(), "id" | "rowid") {
        return None;
    }
    let k: usize = ph.trim().strip_prefix('?')?.parse().ok()?;
    Some((table, int(k.checked_sub(1)?)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_level(m: &Model, id: i64) -> bool {
        m.sql()
            .query_row("SELECT count(*) FROM level WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
            == 1
    }

    #[test]
    fn independent_older_step_reverts_alone_and_the_revert_is_one_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("A", 0.0).unwrap();
        let seq_a = m.last_op_seq().unwrap().unwrap();
        let b = m.add_level("B", 3.0).unwrap();
        assert_eq!(m.revert_check(seq_a).unwrap(), RevertCheck::Ready);
        assert!(has_level(&m, a), "a check changes nothing");
        m.revert_op(seq_a).unwrap();
        assert!(!has_level(&m, a) && has_level(&m, b));
        m.undo().unwrap();
        assert!(has_level(&m, a) && has_level(&m, b), "one undo brings it back");
        m.undo().unwrap();
        m.undo().unwrap();
        assert!(
            !has_level(&m, a) && !has_level(&m, b),
            "older steps still undo in order"
        );
    }

    #[test]
    fn newer_edit_of_the_same_row_blocks_the_revert() {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("A", 0.0).unwrap();
        let seq_a = m.last_op_seq().unwrap().unwrap();
        m.update_level(a, "A2", 1.0).unwrap();
        assert_eq!(
            m.revert_check(seq_a).unwrap(),
            RevertCheck::Conflict("level".into())
        );
        assert!(m.revert_op(seq_a).is_err());
        assert!(has_level(&m, a), "a refused revert changes nothing");
        assert_eq!(m.revert_check(9_999).unwrap(), RevertCheck::Gone);
    }

    #[test]
    fn a_reverted_step_is_flagged_in_the_journal_and_unflagged_by_undo() {
        use crate::AuditEntry;
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("A", 0.0).unwrap();
        let seq_a = m.last_op_seq().unwrap().unwrap();
        let row = m
            .audit_append(&AuditEntry {
                event: "apply".into(),
                op_seq: Some(seq_a),
                status: "ok".into(),
                ..Default::default()
            })
            .unwrap();
        m.add_level("B", 3.0).unwrap();
        assert!(m.audit_op_live(row).unwrap());
        m.revert_op(seq_a).unwrap();
        assert!(!m.audit_op_live(row).unwrap());
        m.undo().unwrap();
        assert!(m.audit_op_live(row).unwrap());
    }

    fn op_rows(m: &Model) -> Vec<(i64, String)> {
        m.sql()
            .prepare("SELECT seq, inv FROM op ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn op_element_ids_of_a_two_element_step_lists_both_and_writes_nothing() {
        let mut m = Model::open(":memory:").unwrap();
        let ((a, b), wrote) = m
            .step(|m| Ok((m.add_level("A", 0.0)?, m.add_level("B", 3.0)?)))
            .unwrap();
        assert!(wrote);
        let seq = m.last_op_seq().unwrap().unwrap();
        let before = op_rows(&m);
        let mut got = m.op_element_ids(seq).unwrap();
        got.retain(|(t, _)| t == "level");
        got.sort();
        assert_eq!(got, vec![("level".to_string(), a), ("level".to_string(), b)]);
        assert_eq!(op_rows(&m), before, "the undo log is untouched");
        assert!(has_level(&m, a) && has_level(&m, b), "the model is untouched");
        assert!(m.op_element_ids(9_999).unwrap().is_empty(), "a gone step names nothing");
    }

    #[test]
    fn op_element_ids_reads_an_edit_and_skips_bookkeeping() {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("A", 0.0).unwrap();
        m.update_level(a, "A2", 1.0).unwrap();
        let seq = m.last_op_seq().unwrap().unwrap();
        let got = m.op_element_ids(seq).unwrap();
        assert!(got.contains(&("level".to_string(), a)), "{got:?}");
        assert!(got.iter().all(|(t, _)| !bookkeeping(t)), "{got:?}");
        assert_eq!(
            row_of("UPDATE ai_audit SET op_undone=0 WHERE seq=?1", &[Value::Integer(1)]),
            None
        );
        assert!(bookkeeping("ai_audit") && bookkeeping("op") && !bookkeeping("level"));
        assert_eq!(
            row_of("INSERT OR REPLACE INTO wall(rowid,id,a) VALUES (?1,?2,?3)", &[
                Value::Integer(7),
                Value::Integer(7),
                Value::Null
            ]),
            Some(("wall".to_string(), 7))
        );
        assert_eq!(
            watermark_of("DELETE FROM wall WHERE rowid>?1", &[Value::Integer(4)]),
            Some(("wall".to_string(), 4))
        );
        assert_eq!(row_of("DELETE FROM wall WHERE rowid>?1", &[Value::Integer(4)]), None);
    }
}
