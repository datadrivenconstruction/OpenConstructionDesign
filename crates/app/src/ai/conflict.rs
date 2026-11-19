// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A change of the live model during a plan marks only the items it touches: an item is hit when
//! a changed row is one it read or wrote, or lies in a table it read by predicate. Items built on
//! a hit item follow it. The rest replays as it was; a replay whose result moved without a hit is
//! a gap in the sets and turns into a conflict as well.

use super::plan::PlanItem;
use super::readset::{model_table, ReadSet};
use ddc_ai::refusal::{self, RefusalCode};
use ddc_model::Changes;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// A row the item named is gone.
    Deleted,
    /// A row the item read or wrote changed.
    Changed,
    /// A table the item read by predicate changed: a new row, or one that may now match.
    Inserted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub rows: Vec<(String, i64)>,
    pub tables: Vec<String>,
    pub kind: ConflictKind,
    /// The hit item this one depends on; `None` for a direct hit.
    pub via: Option<u32>,
    /// Project settings changed or the journal window is lost: every item.
    pub all: bool,
}

/// Rows named in the text; the rest are counted.
const SHOWN: usize = 8;

impl Conflict {
    /// For the card and the agent: `conflict: plan_conflict …` with what changed.
    pub fn text(&self) -> String {
        if let Some(d) = self.via {
            return via_text(d);
        }
        let what = if self.all {
            "project settings changed".to_string()
        } else {
            let kind = match self.kind {
                ConflictKind::Deleted => "deleted",
                ConflictKind::Changed => "changed",
                ConflictKind::Inserted => "rows added or changed in",
            };
            let mut names: Vec<String> = self
                .rows
                .iter()
                .take(SHOWN)
                .map(|(t, id)| format!("{t}:{id}"))
                .collect();
            if self.rows.len() > SHOWN {
                names.push(format!("+{}", self.rows.len() - SHOWN));
            }
            names.extend(self.tables.iter().map(|t| format!("table {t}")));
            format!("{kind} meanwhile: {}", names.join(", "))
        };
        let rows: Vec<String> = self.rows.iter().map(|(t, id)| format!("{t}:{id}")).collect();
        plan_conflict(&[], &rows, &what)
    }
}

/// The one place the conflict refusal is written; the card reads it back for the person
/// (`ui::logic::conflict_text`).
pub fn plan_conflict(items: &[u32], rows: &[String], what: &str) -> String {
    let mut r = refusal::of(RefusalCode::PlanConflict).detail(what);
    if !items.is_empty() {
        let ids: Vec<String> = items.iter().map(u32::to_string).collect();
        r = r.arg("items", ids.join(","));
    }
    if !rows.is_empty() {
        r = r.arg("rows", rows.join(","));
    }
    refusal::wire(&r)
}

/// The conflict of an item built on a conflicting one.
pub fn via_text(item: u32) -> String {
    plan_conflict(&[item], &[], &format!("depends on item {item}, which conflicts"))
}

/// Hit items with what hit them, then the items depending on them, in plan order. Only ticked
/// items count: an unticked one is not on the copy. `gone(table, id)`: the row is no longer in
/// the model.
pub fn conflicts(
    items: &[PlanItem],
    ch: &Changes,
    gone: &dyn Fn(&str, i64) -> bool,
) -> Vec<(u32, Conflict)> {
    let mut out: Vec<(u32, Conflict)> = vec![];
    if ch.is_empty() {
        return out;
    }
    let mut hit: HashSet<u32> = HashSet::new();
    for it in items.iter().filter(|i| i.checked) {
        let c = if ch.all {
            Some(Conflict {
                rows: vec![],
                tables: vec![],
                kind: ConflictKind::Changed,
                via: None,
                all: true,
            })
        } else {
            direct(&it.sets.reads, &it.sets.writes, ch, gone)
        };
        let c = c.or_else(|| {
            it.depends_on.iter().find(|d| hit.contains(d)).map(|d| Conflict {
                rows: vec![],
                tables: vec![],
                kind: ConflictKind::Changed,
                via: Some(*d),
                all: false,
            })
        });
        if let Some(c) = c {
            hit.insert(it.id);
            out.push((it.id, c));
        }
    }
    out
}

fn direct(
    reads: &ReadSet,
    writes: &ReadSet,
    ch: &Changes,
    gone: &dyn Fn(&str, i64) -> bool,
) -> Option<Conflict> {
    let mut rows: BTreeSet<(String, i64)> = BTreeSet::new();
    let mut tables: Vec<String> = vec![];
    // a table written whole: its rows changed, whether or not any were added
    let mut whole = false;
    for (t, ids) in &ch.rows {
        let t = t.as_str();
        if reads.tables.contains(t) {
            whole |= ch.whole(t);
            tables.push(t.to_string());
            for id in ch.ids(t).filter(|id| gone(t, *id)) {
                rows.insert((t.to_string(), id));
            }
            continue;
        }
        let w = model_table(t);
        if ch.whole(t) {
            let any = reads.rows.get(t).is_some_and(|r| !r.is_empty())
                || (w && (writes.tables.contains(t) || writes.rows.contains_key(t)));
            if any {
                whole = true;
                tables.push(t.to_string());
            }
            continue;
        }
        // the smaller side is walked: a plan of 10 000 items against 1 000 changed rows
        for set in [Some(reads), w.then_some(writes)].into_iter().flatten() {
            if set.tables.contains(t) {
                tables.push(t.to_string());
                continue;
            }
            let Some(mine) = set.rows.get(t) else {
                continue;
            };
            if mine.len() < ids.len() {
                rows.extend(mine.iter().filter(|id| ids.contains(id)).map(|id| (t.to_string(), *id)));
            } else {
                rows.extend(ids.iter().filter(|id| mine.contains(id)).map(|id| (t.to_string(), *id)));
            }
        }
    }
    if rows.is_empty() && tables.is_empty() {
        return None;
    }
    tables.sort();
    tables.dedup();
    let rows: Vec<(String, i64)> = rows.into_iter().collect();
    let kind = if rows.iter().any(|(t, id)| gone(t, *id)) {
        ConflictKind::Deleted
    } else if rows.is_empty() && !whole {
        ConflictKind::Inserted
    } else {
        ConflictKind::Changed
    };
    Some(Conflict {
        rows,
        tables,
        kind,
        via: None,
        all: false,
    })
}

/// The row is not in `conn`'s model.
pub fn row_gone(conn: &Connection, table: &str, id: i64) -> bool {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM \"{table}\" WHERE rowid=?1)");
    conn.prepare_cached(&sql)
        .and_then(|mut st| st.query_row([id], |r| r.get::<_, bool>(0)))
        .map_or(true, |e| !e)
}

/// The content of the rows an item writes, as it left them: the same after a replay unless
/// something the item depends on changed.
pub fn written_fp(conn: &Connection, writes: &ReadSet) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut tables: Vec<&&str> = writes.rows.keys().filter(|t| model_table(t)).collect();
    tables.sort();
    for t in tables {
        let sql = format!("SELECT * FROM \"{t}\" WHERE rowid=?1");
        let Ok(mut st) = conn.prepare_cached(&sql) else {
            continue;
        };
        let n = st.column_count();
        for id in &writes.rows[*t] {
            (t, id).hash(&mut h);
            let Ok(mut rows) = st.query([id]) else {
                continue;
            };
            if let Ok(Some(r)) = rows.next() {
                for i in 0..n {
                    match r.get_ref(i) {
                        Ok(rusqlite::types::ValueRef::Real(f)) => f.to_bits().hash(&mut h),
                        Ok(v) => format!("{v:?}").hash(&mut h),
                        Err(_) => 0u8.hash(&mut h),
                    }
                }
            }
        }
    }
    h.finish() | 1
}

#[cfg(test)]
#[path = "conflict_tests.rs"]
mod tests;
