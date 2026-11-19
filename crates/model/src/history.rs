// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Read access to the operation journal (`op`): the model-level view under the editor's undo
//! stack. Rolling back is `Editor::undo` / `jump_history`: the stack must move with the journal.

use ddc_ops::Actor;

use crate::{Error, Model};

#[derive(Clone, Debug, PartialEq)]
pub struct JournalEntry {
    /// Reused after an undo: not a lasting name for an operation.
    pub seq: i64,
    /// Operation kind (`wall_add`, `purge_unused`, ...).
    pub kind: String,
    pub element: i64,
    pub name: Option<String>,
    /// Who made it; `None` for entries written before schema 40 (or an actor kind this build
    /// cannot read).
    pub actor: Option<Actor>,
    pub title: Option<String>,
    pub why: Option<String>,
    /// The forward envelope is stored (`Model::journal_src`).
    pub has_src: bool,
    /// Newest live `ai_audit` row for this entry: an undone entry whose number was reused
    /// by a later edit links none.
    pub audit: Option<i64>,
}

impl Model {
    /// The latest `limit` entries, newest first.
    pub fn journal(&self, limit: usize) -> Result<Vec<JournalEntry>, Error> {
        let mut st = self.db.prepare(
            "SELECT seq, kind, element, name, actor, title, why, src IS NOT NULL,              (SELECT max(a.seq) FROM ai_audit a WHERE a.op_seq=op.seq AND a.op_undone=0)              FROM op ORDER BY seq DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(JournalEntry {
                seq: r.get(0)?,
                kind: r.get(1)?,
                element: r.get(2)?,
                name: r.get(3)?,
                actor: crate::journal::parse_actor(r.get(4)?),
                title: r.get(5)?,
                why: r.get(6)?,
                has_src: r.get(7)?,
                audit: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Number of journal entries. Not undo steps: one tool or import step can span several.
    pub fn journal_len(&self) -> Result<usize, Error> {
        let n: i64 = self
            .db
            .query_row("SELECT count(*) FROM op", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM level", [], |r| r.get::<_, i64>(0))
            .unwrap()
    }

    #[test]
    fn lists_newest_first() {
        let mut m = Model::open(":memory:").unwrap();
        let before = m.journal_len().unwrap();
        m.add_level("L1", 0.0).unwrap();
        m.add_level("L2", 3.0).unwrap();
        m.add_level("L3", 6.0).unwrap();
        let j = m.journal(2).unwrap();
        assert_eq!(j.len(), 2);
        assert!(j[0].seq > j[1].seq);
        assert_eq!(j[0].kind, "level_add");
        assert_eq!(m.journal_len().unwrap(), before + 3);
        m.undo().unwrap();
        assert_eq!(levels(&m), 2);
        assert_eq!(m.journal_len().unwrap(), before + 2);
        assert_eq!(m.journal(1).unwrap()[0].seq, j[1].seq);
    }
}
