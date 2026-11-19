// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bulk load of a big import: one transaction (the usual batch, one undo step), cached
//! prepared statements, and for its duration `synchronous=OFF`, a large page cache, temp in
//! memory and no WAL auto-checkpoint; every pragma is restored afterwards, also on error.
//! The journal stays as it is (WAL on disk): a crash mid-import must not damage the file,
//! and a cancelled or failed import must roll back.

use std::ops::Deref;

use rusqlite::{CachedStatement, Connection, Params, Row};

use crate::{Error, Model, Snapshots};

/// Statement cache entries during a bulk load (the default 16 thrashes on importers).
const STMT_CACHE: usize = 256;
const STMT_CACHE_DEFAULT: usize = 16;

/// Connection view whose `execute`/`query_row` reuse prepared statements; derefs to the
/// connection for everything else (`let c = Cached(db); let db = &c;` in a bulk writer).
pub struct Cached<'a>(pub &'a Connection);

impl Deref for Cached<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.0
    }
}

impl Cached<'_> {
    pub fn execute<P: Params>(&self, sql: &str, params: P) -> rusqlite::Result<usize> {
        self.0.prepare_cached(sql)?.execute(params)
    }

    pub fn query_row<T, P, F>(&self, sql: &str, params: P, f: F) -> rusqlite::Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    {
        self.0.prepare_cached(sql)?.query_row(params, f)
    }

    pub fn prepare_cached(&self, sql: &str) -> rusqlite::Result<CachedStatement<'_>> {
        self.0.prepare_cached(sql)
    }
}

/// Pragmas in force before [`tune`].
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    /// None: tuned inside an open transaction (an undo step), where SQLite keeps it fixed.
    synchronous: Option<i64>,
    cache_size: i64,
    autocheckpoint: Option<i64>,
}

fn pragma(db: &Connection, name: &str) -> Result<i64, Error> {
    Ok(db.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0))?)
}

/// Sets the bulk pragmas; inside an open transaction `synchronous` stays (SQLite refuses it).
pub fn tune(db: &Connection) -> Result<Saved, Error> {
    let wal = db
        .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?
        .eq_ignore_ascii_case("wal");
    let in_tx = !db.is_autocommit();
    let saved = Saved {
        synchronous: if in_tx {
            None
        } else {
            Some(pragma(db, "synchronous")?)
        },
        cache_size: pragma(db, "cache_size")?,
        autocheckpoint: if wal {
            Some(pragma(db, "wal_autocheckpoint")?)
        } else {
            None
        },
    };
    // not temp_store: changing it drops every TEMP table and trigger (the phase tracking)
    if !in_tx {
        db.execute_batch("PRAGMA synchronous=OFF;")?;
    }
    db.execute_batch("PRAGMA cache_size=-262144;")?;
    if wal {
        db.execute_batch("PRAGMA wal_autocheckpoint=0;")?;
    }
    db.set_prepared_statement_cache_capacity(STMT_CACHE);
    Ok(saved)
}

/// Restores [`tune`]'s pragmas and checkpoints the WAL the import grew (TRUNCATE: a
/// passive checkpoint would leave a WAL as big as the import on disk).
pub fn restore(db: &Connection, s: &Saved) -> Result<(), Error> {
    if let Some(sync) = s.synchronous {
        db.execute_batch(&format!("PRAGMA synchronous={sync};"))?;
    }
    db.execute_batch(&format!("PRAGMA cache_size={};", s.cache_size))?;
    db.set_prepared_statement_cache_capacity(STMT_CACHE_DEFAULT);
    if let Some(n) = s.autocheckpoint {
        db.execute_batch(&format!("PRAGMA wal_autocheckpoint={n};"))?;
        // a reader (the mesh cache worker) may hold the WAL: the next checkpoint finishes it
        let _ = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    Ok(())
}

/// Bulk pragmas for its lifetime: restored on drop, also when the bulk writer panics. A
/// failed restore is logged, not returned: the batch it follows is already committed and
/// journaled, and reporting it as failed would desync the caller's undo stack.
struct Guard<'a> {
    db: &'a Connection,
    saved: Saved,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if let Err(e) = restore(self.db, &self.saved) {
            eprintln!("bulk load: pragmas not restored: {e}");
        }
    }
}

fn guard(db: &Connection) -> Result<Guard<'_>, Error> {
    Ok(Guard {
        db,
        saved: tune(db)?,
    })
}

/// `run` under bulk pragmas, restored after it also on panic (the batch needs `&mut Model`,
/// so the guard cannot borrow the connection across it).
fn bulk<T>(m: &mut Model, run: impl FnOnce(&mut Model) -> Result<T, Error>) -> Result<T, Error> {
    let saved = tune(&m.db)?;
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&mut *m)));
    drop(Guard { db: &m.db, saved });
    out.unwrap_or_else(|p| std::panic::resume_unwind(p))
}

/// Runs `f` with bulk pragmas set; they are restored whatever `f` returns.
pub fn with_bulk<T>(db: &Connection, f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let _g = guard(db)?;
    f()
}

impl Model {
    /// [`Model::batch_walls`] under bulk-load pragmas: one transaction, one undo step.
    pub fn batch_walls_bulk<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.batch_walls_edit_bulk(kind, |db, _| f(db))
    }

    /// [`Model::batch_walls_edit`] under bulk-load pragmas: re-import edits stay undoable.
    pub fn batch_walls_edit_bulk<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&Connection, &mut Snapshots) -> Result<T, Error>,
    ) -> Result<T, Error> {
        bulk(self, |m| m.batch_walls_edit(kind, f))
    }

    /// [`Model::batch`] (2D drawing tables) under bulk-load pragmas.
    pub fn batch_bulk<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        bulk(self, |m| m.batch(kind, f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> String {
        let p = std::env::temp_dir().join(format!("ddc-g3-{}-{name}.ddc", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p.to_string_lossy().into_owned()
    }

    /// An import run as an editor op (`Model::step`) must not fail on `synchronous`.
    #[test]
    fn bulk_inside_step_writes_and_keeps_synchronous() {
        let path = file("bulk-step");
        let mut m = Model::open(&path).unwrap();
        let sync = pragma(&m.db, "synchronous").unwrap();
        let top = m.op_top().unwrap();
        let ((), wrote) = m
            .step(|m| {
                m.batch_walls_bulk("wall_test", |db| {
                    db.execute("INSERT INTO level(name, elevation) VALUES ('S', 1.0)", [])?;
                    Ok(())
                })
            })
            .unwrap();
        assert!(wrote);
        assert_eq!(m.op_top().unwrap(), top + 1);
        assert_eq!(pragma(&m.db, "synchronous").unwrap(), sync);
        assert!(m.db.is_autocommit(), "the step committed");
        // inside an open transaction (an atomic plan) SQLite refuses `synchronous`
        m.atomic("wall_test", |m| {
            m.batch_walls_bulk("wall_test", |db| {
                db.execute("INSERT INTO level(name, elevation) VALUES ('T', 2.0)", [])?;
                Ok(())
            })
        })
        .unwrap();
        assert_eq!(m.op_top().unwrap(), top + 2);
        assert_eq!(pragma(&m.db, "synchronous").unwrap(), sync);
        assert!(m.db.is_autocommit(), "the atomic plan committed");
    }

    #[test]
    fn pragmas_are_restored_after_success_and_error() {
        let path = file("bulk");
        let mut m = Model::open(&path).unwrap();
        let before = (
            pragma(&m.db, "synchronous").unwrap(),
            pragma(&m.db, "wal_autocheckpoint").unwrap(),
        );
        let l = m
            .batch_walls_bulk("wall_test", |db| {
                assert_eq!(pragma(db, "synchronous").unwrap(), 0);
                assert_eq!(pragma(db, "wal_autocheckpoint").unwrap(), 0);
                let c = Cached(db);
                for k in 0..500 {
                    c.execute(
                        "INSERT INTO level(name, elevation) VALUES (?1, ?2)",
                        rusqlite::params![format!("L{k}"), k as f64],
                    )?;
                }
                Ok(c.last_insert_rowid())
            })
            .unwrap();
        assert!(l > 0);
        let after = (
            pragma(&m.db, "synchronous").unwrap(),
            pragma(&m.db, "wal_autocheckpoint").unwrap(),
        );
        assert_eq!(before, after);
        let n: i64 =
            m.db.query_row("SELECT count(*) FROM level", [], |r| r.get(0))
                .unwrap();
        assert_eq!(n, 500);
        let e = m.batch_walls_bulk("wall_test", |db| -> Result<(), Error> {
            db.execute("INSERT INTO level(name, elevation) VALUES ('X', 0)", [])?;
            Err(Error::Invalid("stop".into()))
        });
        assert!(e.is_err());
        let n2: i64 =
            m.db.query_row("SELECT count(*) FROM level", [], |r| r.get(0))
                .unwrap();
        assert_eq!(n2, 500, "failed batch rolled back");
        assert_eq!(pragma(&m.db, "synchronous").unwrap(), before.0);
        m.undo().unwrap();
        let n3: i64 =
            m.db.query_row("SELECT count(*) FROM level", [], |r| r.get(0))
                .unwrap();
        assert_eq!(n3, 0, "one undo step");
        // the operation journal survives the bulk session
        m.add_level("after bulk", 0.0).unwrap();
        drop(m);
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{s}"));
        }
    }

    #[test]
    fn edit_bulk_snapshots_edits_and_restores_after_panic() {
        let path = file("bulk-edit");
        let mut m = Model::open(&path).unwrap();
        let sync = pragma(&m.db, "synchronous").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        m.batch_walls_edit_bulk("wall_test", |db, snaps| {
            snaps.save(db, "level", "id", l)?;
            db.execute("UPDATE level SET elevation=5 WHERE id=?1", [l])?;
            Ok(())
        })
        .unwrap();
        let elev = |m: &Model| -> f64 {
            m.db.query_row("SELECT elevation FROM level WHERE id=?1", [l], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(elev(&m), 5.0);
        m.undo().unwrap();
        assert_eq!(elev(&m), 0.0, "bulk edit undone from its snapshot");
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = m.batch_walls_edit_bulk("wall_test", |_, _| -> Result<(), Error> {
                panic!("writer bug")
            });
        }));
        assert!(r.is_err());
        assert_eq!(
            pragma(&m.db, "synchronous").unwrap(),
            sync,
            "restored after panic"
        );
        drop(m);
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{s}"));
        }
    }
}
