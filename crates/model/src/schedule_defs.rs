//! Schedule definitions of the project: one `meta` row holding the app's JSON text (the model
//! keeps it opaque). Written as a journaled operation: a change is one undo step, counts as an
//! unsaved edit and travels with the file.

use rusqlite::{params, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

/// `meta` key of the stored definitions.
pub const SCHEDULE_DEFS_KEY: &str = "schedule_defs";

impl Model {
    /// Stored definitions text; `None` when the project never stored any.
    pub fn schedule_defs(&self) -> Result<Option<String>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT CAST(value AS TEXT) FROM meta WHERE key=?1",
                [SCHEDULE_DEFS_KEY],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Stores `text` in one undo step. `false`: the same text is stored, nothing is written.
    pub fn set_schedule_defs(&mut self, text: &str) -> Result<bool, Error> {
        if self.schedule_defs()?.as_deref() == Some(text) {
            return Ok(false);
        }
        let key = [rusqlite::types::Value::Text(SCHEDULE_DEFS_KEY.into())];
        self.op("schedule_defs", |db| {
            let inv = Inv::default()
                .sql("DELETE FROM meta WHERE key=?1", key.to_vec())
                .restore_where(db, "meta", "key=?1", &key)?;
            db.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![SCHEDULE_DEFS_KEY, text],
            )?;
            Ok((true, 0, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_in_one_undo_step() {
        let mut m = Model::open(":memory:").unwrap();
        assert_eq!(m.schedule_defs().unwrap(), None);
        let top0 = m.op_top().unwrap();
        assert!(m.set_schedule_defs("[1]").unwrap());
        let top1 = m.op_top().unwrap();
        assert_ne!(top1, top0, "a change is a journaled op");
        assert!(!m.set_schedule_defs("[1]").unwrap());
        assert_eq!(m.op_top().unwrap(), top1, "the same text writes nothing");
        assert!(m.set_schedule_defs("[2]").unwrap());
        m.undo().unwrap();
        assert_eq!(m.schedule_defs().unwrap().as_deref(), Some("[1]"));
        m.undo().unwrap();
        assert_eq!(m.schedule_defs().unwrap(), None);
    }
}
