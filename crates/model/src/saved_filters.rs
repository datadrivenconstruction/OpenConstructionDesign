// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Saved selection filters of the project: one `meta` row (JSON list of query texts, oldest
//! first). Written as a journaled operation: a change is one undo step, counts as an unsaved
//! edit and travels with the file, so a Save As copy edits its own list.

use rusqlite::types::Value;
use rusqlite::{params, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

/// `meta` key of the stored list.
pub const SAVED_FILTERS_KEY: &str = "query_filters";
/// `meta` mark older builds keyed their filters by (in a list outside the file); the first
/// stored list removes it in the same step.
pub const LEGACY_FILTER_MARK: &str = "query_project_uid";

fn keys() -> [Value; 2] {
    [
        Value::Text(SAVED_FILTERS_KEY.into()),
        Value::Text(LEGACY_FILTER_MARK.into()),
    ]
}

impl Model {
    /// Stored filters, oldest first; `None` when the project never stored a list.
    pub fn saved_filters(&self) -> Result<Option<Vec<String>>, Error> {
        let t: Option<String> = self
            .db
            .query_row(
                "SELECT CAST(value AS TEXT) FROM meta WHERE key=?1",
                [SAVED_FILTERS_KEY],
                |r| r.get(0),
            )
            .optional()?;
        t.map(|t| {
            serde_json::from_str::<Vec<String>>(&t)
                .map_err(|e| Error::Invalid(format!("saved filters: {e}")))
        })
        .transpose()
    }

    /// The legacy mark (see [`LEGACY_FILTER_MARK`]); `None` in files that never had one.
    pub fn legacy_filter_mark(&self) -> Result<Option<i64>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT value FROM meta WHERE key=?1",
                [LEGACY_FILTER_MARK],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
    }

    /// Stores `list` (oldest first) in one undo step and drops the legacy mark.
    /// `false`: the same list is stored and no mark is left, nothing is written.
    pub fn set_saved_filters(&mut self, list: &[String]) -> Result<bool, Error> {
        let mark = self.legacy_filter_mark()?;
        if mark.is_none() && self.saved_filters()?.as_deref() == Some(list) {
            return Ok(false);
        }
        let text = serde_json::to_string(list)
            .map_err(|e| Error::Invalid(format!("saved filters: {e}")))?;
        self.op("saved_filters", |db| {
            // undo: drop what this step wrote, then put the old rows back
            let inv = Inv::default()
                .sql("DELETE FROM meta WHERE key IN (?1, ?2)", keys().to_vec())
                .restore_where(db, "meta", "key IN (?1, ?2)", &keys())?;
            db.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![SAVED_FILTERS_KEY, text],
            )?;
            db.execute("DELETE FROM meta WHERE key=?1", [LEGACY_FILTER_MARK])?;
            Ok((true, 0, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
            .unwrap()
    }

    fn list(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn stores_in_one_undo_step_and_skips_equal_lists() {
        let mut m = Model::open(":memory:").unwrap();
        assert_eq!(m.saved_filters().unwrap(), None);
        let n = ops(&m);
        assert!(m.set_saved_filters(&list(&["doors width>900"])).unwrap());
        assert_eq!(ops(&m), n + 1, "one journal entry");
        assert_eq!(m.saved_filters().unwrap(), Some(list(&["doors width>900"])));
        // same list again: nothing written
        assert!(!m.set_saved_filters(&list(&["doors width>900"])).unwrap());
        assert_eq!(ops(&m), n + 1);
        assert!(m.set_saved_filters(&list(&["doors width>900", "walls"])).unwrap());
        m.undo().unwrap();
        assert_eq!(m.saved_filters().unwrap(), Some(list(&["doors width>900"])));
        m.undo().unwrap();
        assert_eq!(m.saved_filters().unwrap(), None, "first store undone: no row left");
    }

    #[test]
    fn first_store_drops_the_legacy_mark_and_undo_brings_it_back() {
        let mut m = Model::open(":memory:").unwrap();
        // fixture: a file an older build marked
        m.sql()
            .execute(
                "INSERT INTO meta(key, value) VALUES (?1, 77)",
                [LEGACY_FILTER_MARK],
            )
            .unwrap();
        // an empty list still migrates: the mark goes
        assert!(m.set_saved_filters(&[]).unwrap());
        assert_eq!(m.legacy_filter_mark().unwrap(), None);
        assert_eq!(m.saved_filters().unwrap(), Some(vec![]));
        m.undo().unwrap();
        assert_eq!(m.legacy_filter_mark().unwrap(), Some(77));
        assert_eq!(m.saved_filters().unwrap(), None);
    }

    /// Saved filters are content: they change the content hash and undo restores it; view
    /// state and import bookkeeping in `meta` do not count.
    #[test]
    fn saved_filters_count_in_the_content_hash() {
        let mut m = Model::open(":memory:").unwrap();
        let h0 = m.content_hash().unwrap();
        m.set_view3d_style_code(2).unwrap();
        let put = |m: &Model, k: &str| {
            m.sql()
                .execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, 5)", [k])
                .unwrap();
        };
        for k in ["view:zoom", "pref:w", "phase_current", "src_shared_hash:ifc"] {
            put(&m, k);
        }
        assert_eq!(m.content_hash().unwrap(), h0, "view state and bookkeeping");
        assert!(m.set_saved_filters(&["walls".to_string()]).unwrap());
        let h1 = m.content_hash().unwrap();
        assert_ne!(h1, h0, "a saved filter is a change");
        // a view-state write moves meta rowids: the content hash stays
        put(&m, "view:zoom");
        assert_eq!(m.content_hash().unwrap(), h1);
        m.undo().unwrap();
        assert_eq!(m.content_hash().unwrap(), h0, "undo restores the hash");
        assert!(crate::compare::content_meta_key("query_filters"));
        assert!(!crate::compare::content_meta_key("view3d.style"));
    }
}
