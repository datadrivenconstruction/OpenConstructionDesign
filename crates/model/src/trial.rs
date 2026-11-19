// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Trial runs: an edit applied inside a transaction that is always rolled back, so a tool can
//! show the result and any refusal before the user commits. The file, the operation journal and
//! the change journal never see a trial; caches built inside it are dropped, the live ones kept.

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Error, Model};

static SERIAL: AtomicU64 = AtomicU64::new(1);

/// A fresh process-wide number for a newly opened model.
pub(crate) fn next_serial() -> u64 {
    SERIAL.fetch_add(1, Ordering::Relaxed)
}

impl Model {
    /// Identity of this open document in the process: two open models never share it.
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Runs `f` and returns its result, then rolls everything back. An error or a panic in `f`
    /// rolls back the same way. Refused inside another open transaction.
    pub fn trial<T>(
        &mut self,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if !self.db.is_autocommit() {
            return Err(Error::Invalid(
                "trial: a transaction is already open".into(),
            ));
        }
        // the regen index and the caches would describe rows that never existed after the
        // rollback (ROLLBACK keeps the counters they are keyed by): the trial gets empty ones
        let regen = std::mem::take(&mut self.regen);
        let structs = std::mem::take(&mut self.struct_cache);
        let memo = std::mem::take(&mut self.memo);
        let rebar = std::mem::take(&mut self.rebar_cache);
        let paint = std::mem::take(&mut self.paint_cache);
        let run = match self.db.execute_batch("BEGIN") {
            Ok(()) => catch_unwind(AssertUnwindSafe(|| f(&mut *self))),
            Err(e) => Ok(Err(e.into())),
        };
        let rolled = if self.db.is_autocommit() {
            Ok(())
        } else {
            self.db.execute_batch("ROLLBACK")
        };
        crate::struct_ext::clear_stash();
        self.regen = regen;
        self.struct_cache = structs;
        self.memo = memo;
        self.rebar_cache = rebar;
        self.paint_cache = paint;
        match run {
            Err(panic) => resume_unwind(panic),
            Ok(r) => {
                if let Err(e) = rolled {
                    return Err(Error::Invalid(format!("trial: rollback failed: {e}")));
                }
                r
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Model;

    fn levels(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn trial_never_mutates_model() {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L0", 0.0).unwrap();
        let (n, top, head) = (levels(&m), m.op_top().unwrap(), m.change_head());
        let inside = m
            .trial(|m| {
                m.add_level("T", 3.0)?;
                Ok(levels(m))
            })
            .unwrap();
        assert_eq!(inside, n + 1, "the trial sees its own edit");
        assert_eq!(levels(&m), n);
        assert_eq!(m.op_top().unwrap(), top, "no journal entry");
        assert_eq!(m.change_head(), head, "no change record");
        assert!(m.sql().is_autocommit());
        // a refusal comes back as is and leaves nothing
        let e = m.trial(|m| {
            m.add_level("U", 6.0)?;
            Err::<(), _>(crate::Error::Invalid("no".into()))
        });
        assert!(matches!(e, Err(crate::Error::Invalid(s)) if s == "no"));
        assert_eq!(levels(&m), n);
        // a panic rolls back too
        let p = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = m.trial(|m| -> Result<(), crate::Error> {
                m.add_level("V", 9.0)?;
                panic!("boom")
            });
        }));
        assert!(p.is_err());
        assert!(m.sql().is_autocommit());
        assert_eq!(levels(&m), n);
        // the journal still undoes the real edit
        m.undo().unwrap();
        assert_eq!(levels(&m), n - 1);
    }

    #[test]
    fn trial_refused_inside_open_transaction() {
        let mut m = Model::open(":memory:").unwrap();
        m.sql().execute_batch("BEGIN").unwrap();
        assert!(m.trial(|_| Ok(())).is_err());
        m.sql().execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn serial_differs_per_model() {
        let a = Model::open(":memory:").unwrap();
        let b = Model::open(":memory:").unwrap();
        assert_ne!(a.serial(), b.serial());
    }
}
