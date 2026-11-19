//! Element keys for the revision link: the one string that names an element across exports.
//! Precedence: key written by the generator, then the key of the script that made it
//! (`group/name#n`), then the imported GlobalId, then a key minted once and kept.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::{Error, Model};

/// Property key of the key a generator wrote.
pub const KEY_PROP: &str = "DDC_Identity.ElementKey";
/// Property key of the key minted for an element that had none.
pub const MINTED_PROP: &str = "DDC_Identity.MintedKey";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Generator,
    Script,
    Imported,
    Minted,
}

impl Model {
    /// The key of element `id` of table `kind` and where it came from; `None` when the element
    /// has no key yet (see [`Model::ensure_element_key`]).
    pub fn element_key(&self, kind: &str, id: i64) -> Result<Option<(String, KeySource)>, Error> {
        let prop = |key: &str| -> Result<Option<String>, Error> {
            Ok(self
                .sql()
                .query_row(
                    "SELECT CAST(value AS TEXT) FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                    params![kind, id, key],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .filter(|s| !s.is_empty()))
        };
        if let Some(k) = prop(KEY_PROP)? {
            return Ok(Some((k, KeySource::Generator)));
        }
        if let Some((_, k)) = self.recipe_of(kind, id)? {
            return Ok(Some((k, KeySource::Script)));
        }
        if let Some((_, guid)) = self.ext_guid(kind, id)? {
            return Ok(Some((guid, KeySource::Imported)));
        }
        Ok(prop(MINTED_PROP)?.map(|k| (k, KeySource::Minted)))
    }

    /// Same as [`Model::element_key`], minting and storing a key (undoable property write)
    /// when the element has none, so the next export names it the same.
    pub fn ensure_element_key(&mut self, kind: &str, id: i64) -> Result<(String, KeySource), Error> {
        if let Some(k) = self.element_key(kind, id)? {
            return Ok(k);
        }
        let key = format!("x/{kind}/{}", crate::new_guid(&format!("minted|{kind}|{id}")));
        self.set_prop(kind, id, MINTED_PROP, Some((rusqlite::types::Value::Text(key.clone()), "")))?;
        Ok((key, KeySource::Minted))
    }
}

fn has_table(db: &Connection, schema: &str, name: &str) -> Result<bool, Error> {
    Ok(db
        .query_row(
            &format!("SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name=?1"),
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Keys of every keyed element of `schema`, lowest precedence first so higher ones overwrite
/// (the order of [`Model::element_key`]).
pub(crate) fn load_keys(db: &Connection, schema: &str) -> Result<HashMap<(String, i64), String>, Error> {
    let mut out = HashMap::new();
    let mut put = |sql: String| -> Result<(), Error> {
        let mut st = db.prepare(&sql)?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?))
        })?;
        for row in rows {
            let (t, id, k) = row?;
            if let Some(k) = k.filter(|k| !k.is_empty()) {
                out.insert((t, id), k);
            }
        }
        Ok(())
    };
    let prop = |key: &str| {
        format!(
            "SELECT kind, ref, CAST(value AS TEXT) FROM {schema}.prop WHERE key='{}'",
            key.replace('\'', "''")
        )
    };
    if has_table(db, schema, "prop")? {
        put(prop(MINTED_PROP))?;
    }
    if has_table(db, schema, "ext_ref")? {
        put(format!("SELECT kind, ref, min(guid) FROM {schema}.ext_ref GROUP BY kind, ref"))?;
    }
    if has_table(db, schema, "recipe_part")? {
        put(format!(
            "SELECT p.tbl, p.el, {} FROM {schema}.recipe_part p JOIN {schema}.recipe r ON r.id=p.recipe",
            crate::recipe::PART_KEY_EXPR
        ))?;
    }
    if has_table(db, schema, "prop")? {
        put(prop(KEY_PROP))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};
    use rusqlite::types::Value;

    #[test]
    fn precedence_generator_then_script_then_imported_then_minted() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type("w", &[LayerSpec { material: "brick".into(), thickness: 0.2 }])
            .unwrap();
        let mut walls = vec![];
        for i in 0..3 {
            let w = m
                .add_wall(&WallSpec {
                    a: (0.0, f64::from(i)),
                    b: (5.0, f64::from(i)),
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap();
            walls.push(w);
        }
        assert_eq!(m.element_key("wall", walls[0]).unwrap(), None);
        m.sql()
            .execute("INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('f','2Gx1',?1,?2)", params!["wall", walls[1]])
            .unwrap();
        assert_eq!(
            m.element_key("wall", walls[1]).unwrap(),
            Some(("2Gx1".into(), KeySource::Imported))
        );
        m.set_prop("wall", walls[1], KEY_PROP, Some((Value::Text("b1/s01/wall".into()), ""))).unwrap();
        assert_eq!(
            m.element_key("wall", walls[1]).unwrap(),
            Some(("b1/s01/wall".into(), KeySource::Generator))
        );
        let a = m.ensure_element_key("wall", walls[0]).unwrap();
        assert_eq!(a.1, KeySource::Minted);
        assert_eq!(m.ensure_element_key("wall", walls[0]).unwrap(), a, "a minted key is kept");
        let b = m.ensure_element_key("wall", walls[2]).unwrap();
        assert_ne!(a.0, b.0);
        // a file written before keys existed: no key until the first export asks, then the
        // same key every time, including after undo of nothing else
        assert_eq!(m.element_key("wall", walls[2]).unwrap().unwrap().1, KeySource::Minted);
        assert_eq!(m.ensure_element_key("wall", walls[2]).unwrap(), b);
        // a generator key beats a minted one
        m.set_prop("wall", walls[2], KEY_PROP, Some((Value::Text("b1/s01/wall-x/0".into()), ""))).unwrap();
        assert_eq!(m.element_key("wall", walls[2]).unwrap().unwrap().1, KeySource::Generator);
    }
}
