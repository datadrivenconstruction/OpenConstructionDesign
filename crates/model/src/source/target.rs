//! `src_target`: where each export format saves in this project (path, over-source flag, settings,
//! last result). Part of the project file, outside undo, and not an edit (`DERIVED` in the app).

use rusqlite::{params, OptionalExtension};

use crate::{Error, Model};

/// Same table as schema step 39 in `source/file.rs`: whichever runs first creates it.
const DDL: &str = "CREATE TABLE IF NOT EXISTS src_target(id INTEGER PRIMARY KEY,
    format TEXT NOT NULL UNIQUE, source TEXT, path TEXT NOT NULL, rel_path TEXT NOT NULL DEFAULT '',
    over_source INTEGER NOT NULL DEFAULT 0, settings TEXT NOT NULL DEFAULT '{}',
    last_hash TEXT NOT NULL DEFAULT '', last_at INTEGER NOT NULL DEFAULT 0,
    last_journal INTEGER NOT NULL DEFAULT 0, last_report TEXT NOT NULL DEFAULT '{}');";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SaveTarget {
    pub format: String,
    /// Source namespace this target came from, if any.
    pub source: Option<String>,
    pub path: String,
    /// From the project file's folder; empty when outside it.
    pub rel_path: String,
    /// Only an explicit choice sets this: the save then replaces the imported file.
    pub over_source: bool,
    /// JSON.
    pub settings: String,
    /// SHA-256 of what the last save wrote.
    pub last_hash: String,
    pub last_at: i64,
    /// `op.seq` at the snapshot of the last save.
    pub last_journal: i64,
    /// JSON `SaveReport`.
    pub last_report: String,
}

fn json_or_empty(s: &str) -> &str {
    if s.is_empty() {
        "{}"
    } else {
        s
    }
}

impl Model {
    pub fn save_target(&self, format: &str) -> Result<Option<SaveTarget>, Error> {
        let has: i64 = self.db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='src_target'",
            [],
            |r| r.get(0),
        )?;
        if has == 0 {
            return Ok(None);
        }
        Ok(self
            .db
            .query_row(
                "SELECT format,source,path,rel_path,over_source,settings,last_hash,last_at,
                        last_journal,last_report FROM src_target WHERE format=?1",
                [format],
                |r| {
                    Ok(SaveTarget {
                        format: r.get(0)?,
                        source: r.get(1)?,
                        path: r.get(2)?,
                        rel_path: r.get(3)?,
                        over_source: r.get::<_, i64>(4)? != 0,
                        settings: r.get(5)?,
                        last_hash: r.get(6)?,
                        last_at: r.get(7)?,
                        last_journal: r.get(8)?,
                        last_report: r.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    /// Writes the row of `t.format`. No operation: undo never touches it.
    pub fn put_save_target(&self, t: &SaveTarget) -> Result<(), Error> {
        self.db.execute_batch(DDL)?;
        self.db.execute(
            "INSERT INTO src_target(format,source,path,rel_path,over_source,settings,last_hash,
                 last_at,last_journal,last_report) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(format) DO UPDATE SET source=?2,path=?3,rel_path=?4,over_source=?5,
                 settings=?6,last_hash=?7,last_at=?8,last_journal=?9,last_report=?10",
            params![
                t.format,
                t.source,
                t.path,
                t.rel_path,
                t.over_source as i64,
                json_or_empty(&t.settings),
                t.last_hash,
                t.last_at,
                t.last_journal,
                json_or_empty(&t.last_report),
            ],
        )?;
        Ok(())
    }

    /// Newest operation of the undo journal, stored with a save.
    pub fn journal_head(&self) -> Result<i64, Error> {
        Ok(self
            .db
            .query_row("SELECT COALESCE(MAX(seq),0) FROM op", [], |r| r.get(0))?)
    }

    /// User edits since `mark`. An undo below the mark counts too: the file holds those edits and
    /// the model no longer does.
    pub fn edits_since(&self, mark: i64) -> Result<i64, Error> {
        let (later, max): (i64, i64) = self.db.query_row(
            "SELECT COALESCE(SUM(seq>?1),0), COALESCE(MAX(seq),0) FROM op",
            [mark],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(later + (mark - max).max(0))
    }

    /// Our own write over the source: its `written_hash`, so the source check does not call it a
    /// foreign change. `false` until the file has that column (schema step 39).
    pub fn note_source_written(&self, source: &str, hash: &str) -> Result<bool, Error> {
        let has: i64 = self.db.query_row(
            "SELECT count(*) FROM pragma_table_info('src_file') WHERE name='written_hash'",
            [],
            |r| r.get(0),
        )?;
        if has == 0 {
            return Ok(false);
        }
        let n = self.db.execute(
            "UPDATE src_file SET written_hash=?2 WHERE source=?1",
            params![source, hash],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> SaveTarget {
        SaveTarget {
            format: "ifc".into(),
            path: "C:/x/House-edited.ifc".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_row_round_trips_and_a_second_put_updates_it() {
        let m = Model::open(":memory:").unwrap();
        assert_eq!(m.save_target("ifc").unwrap(), None);
        m.put_save_target(&row()).unwrap();
        let got = m.save_target("ifc").unwrap().unwrap();
        assert_eq!(got.settings, "{}");
        assert!(!got.over_source, "a copy unless asked");
        let t = SaveTarget {
            over_source: true,
            last_hash: "abc".into(),
            last_journal: 7,
            ..got
        };
        m.put_save_target(&t).unwrap();
        assert_eq!(m.save_target("ifc").unwrap().unwrap(), t);
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM src_target", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn undo_does_not_touch_the_row_and_the_row_adds_no_operation() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let head = m.journal_head().unwrap();
        m.put_save_target(&row()).unwrap();
        assert_eq!(m.journal_head().unwrap(), head);
        m.undo().unwrap();
        assert!(m.save_target("ifc").unwrap().is_some());
    }

    #[test]
    fn edits_since_counts_new_edits_and_undone_ones() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let mark = m.journal_head().unwrap();
        assert_eq!(m.edits_since(mark).unwrap(), 0);
        m.add_level("L2", 3.0).unwrap();
        assert_eq!(m.edits_since(mark).unwrap(), 1);
        m.undo().unwrap();
        assert_eq!(m.edits_since(mark).unwrap(), 0);
        m.undo().unwrap();
        assert!(
            m.edits_since(mark).unwrap() > 0,
            "the file has an edit the model lost"
        );
    }
}
