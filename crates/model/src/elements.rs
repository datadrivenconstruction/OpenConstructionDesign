// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Elements and numeric parameters (the model's base layer).

use rusqlite::{params, types::Value};

use crate::ops::{log, scope, Inv, Meta};
use crate::{ElementId, Error, Model};

impl Model {
    pub fn create(&mut self, kind: &str) -> Result<ElementId, Error> {
        let tx = scope(&mut self.db)?;
        tx.execute("INSERT INTO element(kind) VALUES (?1)", [kind])?;
        let id = tx.last_insert_rowid();
        let inv = Inv::default()
            .delete("param", "element", id)
            .delete("element", "id", id);
        let meta = Meta {
            name: Some(kind),
            old: None,
            new: None,
        };
        log(&tx, "create", id, meta, &inv)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn set(&mut self, id: ElementId, name: &str, value: f64) -> Result<(), Error> {
        let tx = scope(&mut self.db)?;
        let old: Option<f64> = tx
            .query_row(
                "SELECT value FROM param WHERE element=?1 AND name=?2",
                params![id, name],
                |r| r.get(0),
            )
            .ok();
        tx.execute(
            "INSERT OR REPLACE INTO param VALUES (?1, ?2, ?3)",
            params![id, name, value],
        )?;
        let key = vec![Value::Integer(id), Value::Text(name.into())];
        let inv = match old {
            Some(v) => Inv::default().sql(
                "UPDATE param SET value=?3 WHERE element=?1 AND name=?2",
                vec![key[0].clone(), key[1].clone(), Value::Real(v)],
            ),
            None => Inv::default().sql("DELETE FROM param WHERE element=?1 AND name=?2", key),
        };
        let meta = Meta {
            name: Some(name),
            old,
            new: Some(value),
        };
        log(&tx, "set", id, meta, &inv)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, id: ElementId, name: &str) -> Option<f64> {
        self.db
            .query_row(
                "SELECT value FROM param WHERE element=?1 AND name=?2",
                params![id, name],
                |r| r.get(0),
            )
            .ok()
    }

    pub fn count(&self) -> Result<i64, Error> {
        Ok(self
            .db
            .query_row("SELECT count(*) FROM element WHERE alive=1", [], |r| {
                r.get(0)
            })?)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Error, Model};
    use rusqlite::Connection;

    #[test]
    fn every_change_is_undoable() {
        let mut m = Model::open(":memory:").unwrap();
        let w = m.create("wall").unwrap();
        m.set(w, "height", 3.0).unwrap();
        m.set(w, "height", 4.0).unwrap();
        m.undo().unwrap();
        assert_eq!(m.get(w, "height"), Some(3.0));
        m.undo().unwrap();
        assert_eq!(m.get(w, "height"), None);
        m.undo().unwrap();
        assert_eq!(m.count().unwrap(), 0);
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
    }

    #[test]
    fn model_file_is_plain_sqlite() {
        let dir = std::env::temp_dir().join("ddc_model_test.ddc");
        let _ = std::fs::remove_file(&dir);
        let p = dir.to_str().unwrap();
        {
            let mut m = Model::open(p).unwrap();
            let s = m.create("slab").unwrap();
            m.set(s, "thickness", 0.2).unwrap();
        }
        let db = Connection::open(p).unwrap();
        let v: f64 = db
            .query_row("SELECT value FROM param WHERE name='thickness'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(v, 0.2);
    }
}
