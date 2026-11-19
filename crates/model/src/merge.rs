// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Merge with a peer's file: three-way comparison of this model and the peer's copy against a common base.
//! The base is the peer's file rolled back through the journal to the last common entry (or to the
//! previous merge point), so the base file and the peer file always share one id space. Checks: the
//! project tag and a shared journal history; without them the merge is refused. Comparison runs as SQL
//! set operations over attached files (ATTACH); only elements changed on both sides are loaded into
//! Rust; the per-field decision is [`crate::source::decide`], the conflict choice is
//! [`crate::source::Pick`] (same as re-import).
//! Preview writes nothing; apply is a single journal operation (one undo).
//! The peer's new elements get fresh ids in this file; the mapping is stored in `merge_map`, so
//! merging again creates no copies. Not merged: `src_*`, `geom_join`, `clash_item`, view state
//! (`view_phase`, `view_option`).

mod analyze;
mod apply;
mod base;
pub(crate) mod tables;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::Value as J;

use crate::source::Pick;
use crate::{Error, Model, Snapshots};

pub(crate) use apply::{drop_row, HOSTED};
pub use base::{prepare_merge, Lineage, MergeError};

/// Files prepared for a merge (working copies in a temp folder).
#[derive(Clone, Debug)]
pub struct Prepared {
    /// The file the user picked.
    pub source: PathBuf,
    /// Copy of that file in this build's schema.
    pub their: PathBuf,
    /// Copy rolled back to the common point.
    pub base: PathBuf,
    /// The `merge_peer` row, if this file continues an already merged history.
    pub peer: Option<i64>,
    pub base_seq: i64,
    pub tip_seq: i64,
    pub tip_hash: i64,
    /// `merge_peer` rows of the peer's file that this model continues.
    pub reverse: Vec<i64>,
    /// When the file was prepared (unix s): the merge point is stamped with it, so a redo
    /// replays the same value.
    pub at: i64,
}

/// Kind of conflict on an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MergeCase {
    /// The same field was changed differently.
    EditEdit,
    /// Deleted there, changed here (or new work here depends on it).
    DeletedThere,
    /// Deleted here, changed there (or new work there depends on it).
    DeletedHere,
    /// Added on both sides with different values.
    AddAdd,
}

impl MergeCase {
    pub fn key(self) -> &'static str {
        match self {
            MergeCase::EditEdit => "edit_edit",
            MergeCase::DeletedThere => "deleted_there",
            MergeCase::DeletedHere => "deleted_here",
            MergeCase::AddAdd => "add_add",
        }
    }
}

/// Difference in one field (`field` is a column or `#part`/`#part:key`).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldConflict {
    pub field: String,
    pub base: J,
    pub mine: J,
    pub theirs: J,
}

/// A conflict is resolved per whole element: all its fields are "mine" or "theirs".
#[derive(Clone, Debug, PartialEq)]
pub struct MergeConflict {
    pub table: String,
    /// Id in this model.
    pub id: i64,
    pub case: MergeCase,
    pub fields: Vec<FieldConflict>,
}

/// Per-table totals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeCounts {
    pub table: String,
    pub added: usize,
    pub changed: usize,
    pub deleted: usize,
    pub conflicts: usize,
}

/// Preview: what will be taken from the peer's file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergePlan {
    /// Last journal entry of the model at preview time.
    pub head: i64,
    pub added: usize,
    pub changed: usize,
    pub deleted: usize,
    pub conflicts: Vec<MergeConflict>,
    pub by_table: Vec<MergeCounts>,
    /// Everything already matches: nothing to apply.
    pub nothing: bool,
}

/// Result of applying.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub added: usize,
    pub changed: usize,
    pub deleted: usize,
    pub restored: usize,
    pub kept_mine: usize,
    pub took_theirs: usize,
    /// The peer's new elements whose host stayed deleted here.
    pub skipped: usize,
    /// Locked parameter values the colleague's file would change, kept as they are here.
    pub locked: usize,
}

fn attach(db: &Connection, p: &Prepared) -> Result<(), Error> {
    let text = |x: &PathBuf| {
        x.to_str()
            .map(str::to_string)
            .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))
    };
    // an apply run as an editor op cannot DETACH inside its transaction: drop the leftover here
    detach(db);
    db.execute("ATTACH DATABASE ?1 AS their", [text(&p.their)?])?;
    if let Err(e) = db.execute("ATTACH DATABASE ?1 AS base", [text(&p.base)?]) {
        let _ = db.execute("DETACH DATABASE their", []);
        return Err(e.into());
    }
    Ok(())
}

pub(crate) fn detach(db: &Connection) {
    let _ = db.execute("DETACH DATABASE their", []);
    let _ = db.execute("DETACH DATABASE base", []);
}

fn head(db: &Connection) -> Result<i64, Error> {
    Ok(db.query_row("SELECT COALESCE(MAX(seq),0) FROM main.op", [], |r| r.get(0))?)
}

fn preview_on(db: &Connection, p: &Prepared) -> Result<MergePlan, Error> {
    attach(db, p)?;
    let res = (|| {
        // only temp tables are written; dropping the transaction forgets them
        let tx = db.unchecked_transaction()?;
        let a = analyze::analyze(&tx, p)?;
        drop(tx);
        Ok(a.plan)
    })();
    detach(db);
    res
}

/// Preview on a separate connection to the model file (for a background thread).
pub fn preview_file(model_path: &str, p: &Prepared) -> Result<MergePlan, Error> {
    let db = Connection::open(model_path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    preview_on(&db, p)
}

impl Model {
    /// Path of the model file; None for an in-memory model.
    pub fn db_path(&self) -> Option<String> {
        self.db.path().filter(|p| !p.is_empty()).map(str::to_string)
    }

    /// Merge preview (writes nothing).
    pub fn merge_preview(&self, p: &Prepared) -> Result<MergePlan, Error> {
        preview_on(&self.db, p)
    }

    /// Applies the merge as one operation. `picks` are the conflict choices (table, id, pick);
    /// without a pick "mine" is kept. A model changed after the preview is refused.
    pub fn merge_apply(
        &mut self,
        p: &Prepared,
        plan: &MergePlan,
        picks: &[(String, i64, Pick)],
    ) -> Result<MergeReport, Error> {
        if head(&self.db)? != plan.head {
            return Err(Error::Conflict("model changed since the preview".into()));
        }
        let picks: HashMap<(String, i64), Pick> = picks
            .iter()
            .map(|(t, id, k)| ((t.clone(), *id), *k))
            .collect();
        attach(&self.db, p)?;
        let res = self.op("wall_merge", |db| {
            let a = analyze::analyze(db, p)?;
            let mut snaps = Snapshots::new();
            let rep = apply::apply(db, &mut snaps, p, &a, &picks)?;
            analyze::drop_temp(db)?;
            Ok((rep, 0, snaps.into_inv()))
        });
        detach(&self.db);
        res
    }
}
