// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Read-only view of one moment of a working file, for a worker: the save pipeline reads it while
//! the editor keeps writing (WAL). No upgrade and no hooks, so opening it cannot change the file.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::{schema, Error, Model};

impl Model {
    /// Opens `work` read-only inside a read transaction held until the value is dropped: later
    /// commits are invisible here. Refuses any other format version: nothing upgrades it here.
    pub fn open_work_snapshot(work: &Path) -> Result<Model, Error> {
        std::fs::metadata(work)?;
        let db = Connection::open_with_flags(work, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")?;
        match schema::stored_version(&db)? {
            Some(v) if v == schema::VERSION => {}
            Some(found) if found > schema::VERSION => {
                return Err(Error::NewerVersion {
                    found,
                    supported: schema::VERSION,
                });
            }
            v => return Err(Error::Invalid(format!("snapshot of format {v:?}"))),
        }
        Ok(Model {
            db,
            tracker: Default::default(),
            struct_cache: Default::default(),
            memo: Default::default(),
            regen: Default::default(),
            rebar_cache: Default::default(),
            paint_cache: Default::default(),
            plant_rank: Default::default(),
            serial: crate::trial::next_serial(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(tag: &str) -> (std::path::PathBuf, Model) {
        let dir = std::env::temp_dir().join(format!("ddc-o5-snap-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("work.ddc");
        let m = Model::open(p.to_str().unwrap()).unwrap();
        (p, m)
    }

    fn levels(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn later_commits_are_not_seen_and_the_writer_is_not_blocked() {
        let (p, mut m) = work("iso");
        m.add_level("L1", 0.0).unwrap();
        let snap = Model::open_work_snapshot(&p).unwrap();
        m.add_level("L2", 3.0).unwrap();
        assert_eq!(levels(&snap), 1);
        assert_eq!(levels(&m), 2);
        drop(snap);
        assert_eq!(levels(&Model::open_work_snapshot(&p).unwrap()), 2);
    }

    #[test]
    fn a_snapshot_cannot_write_and_has_no_journal() {
        let (p, mut m) = work("ro");
        m.add_level("L1", 0.0).unwrap();
        let snap = Model::open_work_snapshot(&p).unwrap();
        assert!(snap.sql().execute("DELETE FROM level", []).is_err());
        assert_eq!(snap.change_head(), 0);
    }

    #[test]
    fn another_format_is_refused_not_upgraded() {
        let (p, m) = work("ver");
        m.sql()
            .execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [schema::VERSION + 1],
            )
            .unwrap();
        assert!(matches!(
            Model::open_work_snapshot(&p),
            Err(Error::NewerVersion { .. })
        ));
        m.sql()
            .execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [schema::VERSION - 1],
            )
            .unwrap();
        assert!(matches!(
            Model::open_work_snapshot(&p),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn a_missing_file_is_an_error_and_is_not_created() {
        let (p, _m) = work("gone");
        let other = p.with_file_name("none.ddc");
        assert!(Model::open_work_snapshot(&other).is_err());
        assert!(!other.exists());
    }
}
