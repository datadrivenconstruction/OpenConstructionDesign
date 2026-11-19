//! Live geometry recipes (schema 47): the script, its params, the hash of what it built and
//! the stdlib version, with one link per created element under its stable part key. Writes go
//! through the journal, so the recipe lands and leaves with the elements in one undo step.
//! A link whose element is gone (deleted by hand) is not reported: a rerun creates it again.

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS recipe(id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '',
    script TEXT NOT NULL, params_json TEXT NOT NULL DEFAULT '{}', hash TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS recipe_part(recipe INTEGER NOT NULL, key TEXT NOT NULL,
    tbl TEXT NOT NULL, el INTEGER NOT NULL, PRIMARY KEY(recipe, key));
CREATE INDEX IF NOT EXISTS recipe_part_el ON recipe_part(tbl, el);
CREATE TABLE IF NOT EXISTS solid_comp(solid INTEGER PRIMARY KEY, comp INTEGER NOT NULL);
";

/// Tables a recipe may create rows in.
pub const PART_TABLES: [&str; 3] = ["solid", "mass", "gridshell"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub id: i64,
    pub name: String,
    pub script: String,
    pub params_json: String,
    pub hash: String,
    /// Script stdlib version the recipe last ran with.
    pub version: i64,
}

/// One link: part key to an element row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipePart {
    pub key: String,
    pub tbl: String,
    pub el: i64,
}

/// Separates the instance key of a recipe from a part key in an element key.
pub const PART_KEY_SEP: char = '/';

/// Additive: the tables, then the instance key column (`ikey`) minted for recipes without one.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    let has_ikey: bool = db
        .prepare("SELECT 1 FROM pragma_table_info('recipe') WHERE name='ikey'")?
        .exists([])?;
    if !has_ikey {
        db.execute_batch("ALTER TABLE recipe ADD COLUMN ikey TEXT")?;
    }
    let bare: Vec<(i64, String)> = db
        .prepare("SELECT id, hash FROM recipe WHERE ikey IS NULL OR ikey='' ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, hash) in bare {
        let k = mint_ikey(db, id, &hash)?;
        db.execute("UPDATE recipe SET ikey=?2 WHERE id=?1", params![id, k])?;
    }
    Ok(())
}

/// A recipe instance key no recipe of this file has: seeded by the project, the row and the
/// script hash only. No clock: a draft compiled on a copy and its Apply on the model must mint
/// the same key, or the applied elements differ from the reviewed draft and Apply is undone.
fn mint_ikey(db: &Connection, id: i64, hash: &str) -> Result<String, Error> {
    // the uid is an integer in files made by the app and text in older or imported ones
    let uid = match db
        .query_row("SELECT value FROM meta WHERE key='project_uid'", [], |r| r.get::<_, Value>(0))
        .optional()?
    {
        Some(Value::Integer(i)) => i.to_string(),
        Some(Value::Text(t)) => t,
        _ => String::new(),
    };
    for n in 0..1000u32 {
        let k = crate::params::new_guid(&format!("recipe:{uid}:{id}:{hash}:{n}"));
        let taken = db
            .prepare_cached("SELECT 1 FROM recipe WHERE ikey=?1")?
            .exists([&k])?;
        if !taken {
            return Ok(k);
        }
    }
    Err(Error::Invalid("no free recipe instance key".into()))
}

fn check_parts(parts: &[RecipePart]) -> Result<(), Error> {
    for p in parts {
        if !PART_TABLES.contains(&p.tbl.as_str()) {
            return Err(Error::Invalid(format!(
                "recipe part {}: table {} is not allowed",
                p.key, p.tbl
            )));
        }
    }
    Ok(())
}

fn put_parts(db: &Connection, id: i64, parts: &[RecipePart]) -> Result<(), Error> {
    let mut st = db.prepare_cached(
        "INSERT OR REPLACE INTO recipe_part(recipe, key, tbl, el) VALUES (?1,?2,?3,?4)",
    )?;
    for p in parts {
        st.execute(params![id, p.key, p.tbl, p.el])?;
    }
    Ok(())
}

fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Recipe> {
    Ok(Recipe {
        id: r.get(0)?,
        name: r.get(1)?,
        script: r.get(2)?,
        params_json: r.get(3)?,
        hash: r.get(4)?,
        version: r.get(5)?,
    })
}

const COLS: &str = "id, name, script, params_json, hash, version";
/// SQL of a part's element key over `recipe_part p JOIN recipe r`.
pub(crate) const PART_KEY_EXPR: &str = "r.ikey || '/' || p.key";

impl Model {
    /// Stores a recipe and its links; one undo step, or part of the enclosing `atomic` step.
    pub fn recipe_add(&mut self, r: &Recipe, parts: &[RecipePart]) -> Result<i64, Error> {
        check_parts(parts)?;
        self.op("recipe_add", |db| {
            migrate(db)?;
            db.execute(
                "INSERT INTO recipe(name, script, params_json, hash, version) \
                 VALUES (?1,?2,?3,?4,?5)",
                params![r.name, r.script, r.params_json, r.hash, r.version],
            )?;
            let id = db.last_insert_rowid();
            let k = mint_ikey(db, id, &r.hash)?;
            db.execute("UPDATE recipe SET ikey=?2 WHERE id=?1", params![id, k])?;
            put_parts(db, id, parts)?;
            let inv = Inv::default()
                .sql("DELETE FROM recipe_part WHERE recipe=?1", vec![Value::Integer(id)])
                .delete("recipe", "id", id);
            Ok((id, id, inv))
        })
    }

    /// A rerun: new params and hash; links of `gone` keys dropped, `added` ones written.
    pub fn recipe_update(
        &mut self,
        id: i64,
        params_json: &str,
        hash: &str,
        version: i64,
        gone: &[String],
        added: &[RecipePart],
    ) -> Result<(), Error> {
        check_parts(added)?;
        if self.recipe(id)?.is_none() {
            return Err(Error::Invalid(format!("no recipe {id}")));
        }
        self.op("recipe_update", |db| {
            let inv = Inv::default()
                .sql(
                    "DELETE FROM recipe_part WHERE recipe=?1",
                    vec![Value::Integer(id)],
                )
                .restore(db, "recipe_part", "recipe", id)?
                .restore(db, "recipe", "id", id)?;
            db.execute(
                "UPDATE recipe SET params_json=?2, hash=?3, version=?4 WHERE id=?1",
                params![id, params_json, hash, version],
            )?;
            let mut st = db.prepare_cached("DELETE FROM recipe_part WHERE recipe=?1 AND key=?2")?;
            for k in gone {
                st.execute(params![id, k])?;
            }
            put_parts(db, id, added)?;
            Ok(((), id, inv))
        })
    }

    pub fn recipe(&self, id: i64) -> Result<Option<Recipe>, Error> {
        if !has_table(&self.db)? {
            return Ok(None);
        }
        Ok(self
            .db
            .prepare_cached(&format!("SELECT {COLS} FROM recipe WHERE id=?1"))?
            .query_row([id], read)
            .optional()?)
    }

    pub fn recipes(&self) -> Result<Vec<Recipe>, Error> {
        if !has_table(&self.db)? {
            return Ok(vec![]);
        }
        let mut st = self
            .db
            .prepare(&format!("SELECT {COLS} FROM recipe ORDER BY id"))?;
        let rows = st.query_map([], read)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Links whose element still exists, in key order.
    pub fn recipe_parts(&self, id: i64) -> Result<Vec<RecipePart>, Error> {
        if !has_table(&self.db)? {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for t in PART_TABLES {
            let mut st = self.db.prepare_cached(&format!(
                "SELECT p.key, p.el FROM recipe_part p JOIN {t} e ON e.id=p.el \
                 WHERE p.recipe=?1 AND p.tbl='{t}'"
            ))?;
            let rows = st.query_map([id], |r| {
                Ok(RecipePart {
                    key: r.get(0)?,
                    tbl: t.to_string(),
                    el: r.get(1)?,
                })
            })?;
            for r in rows {
                out.push(r?);
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// The element key of a part: instance key of its recipe, [`PART_KEY_SEP`], part key.
    /// Unique across recipes and projects; `None` for an element no recipe made.
    pub fn recipe_part_key(&self, tbl: &str, el: i64) -> Result<Option<String>, Error> {
        if !has_table(&self.db)? {
            return Ok(None);
        }
        Ok(self
            .db
            .prepare_cached(&format!(
                "SELECT {PART_KEY_EXPR} FROM recipe_part p JOIN recipe r ON r.id=p.recipe \
                 WHERE p.tbl=?1 AND p.el=?2"
            ))?
            .query_row(params![tbl, el], |r| r.get(0))
            .optional()?)
    }

    /// The recipe that made element (`tbl`, `el`) and its part key.
    pub fn recipe_of(&self, tbl: &str, el: i64) -> Result<Option<(i64, String)>, Error> {
        if !has_table(&self.db)? {
            return Ok(None);
        }
        Ok(self
            .db
            .prepare_cached("SELECT recipe, key FROM recipe_part WHERE tbl=?1 AND el=?2")?
            .query_row(params![tbl, el], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?)
    }
}

fn has_table(db: &Connection) -> Result<bool, Error> {
    let n: i64 = db
        .prepare_cached("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='recipe'")?
        .query_row([], |r| r.get(0))?;
    Ok(n > 0)
}

/// Drops the recipe link of a deleted element, so a row id the database hands out again is
/// never taken for the old part. The inverse puts the link back with the element.
/// The component row of a role-tagged solid goes with the solid: row and link, undo restores both.
pub(crate) fn drop_role_comp(db: &Connection, solid: i64, inv: Inv) -> Result<Inv, Error> {
    let comp: Option<i64> = db
        .query_row("SELECT comp FROM solid_comp WHERE solid=?1", [solid], |r| r.get(0))
        .optional()?;
    let Some(comp) = comp else { return Ok(inv) };
    let inv = inv
        .restore(db, "comp", "id", comp)?
        .restore(db, "solid_comp", "solid", solid)?
        .then(crate::joints::detach(db, comp)?);
    db.execute("DELETE FROM comp WHERE id=?1", [comp])?;
    db.execute("DELETE FROM solid_comp WHERE solid=?1", [solid])?;
    Ok(inv)
}

impl Model {
    /// Ties a component row to the solid that made it (a role tag), so deleting the solid, as a
    /// rerun does, deletes the row too.
    pub fn link_role_comp(&mut self, solid: i64, comp: i64) -> Result<(), Error> {
        self.op("solid_comp_link", |db| {
            db.execute(
                "INSERT OR REPLACE INTO solid_comp(solid, comp) VALUES (?1,?2)",
                params![solid, comp],
            )?;
            let inv = Inv::default().sql("DELETE FROM solid_comp WHERE solid=?1", vec![Value::Integer(solid)]);
            Ok(((), solid, inv))
        })
    }
}

pub(crate) fn unlink(db: &Connection, tbl: &str, el: i64, inv: Inv) -> Result<Inv, Error> {
    if !has_table(db)? {
        return Ok(inv);
    }
    let rows: Vec<(i64, String)> = db
        .prepare_cached("SELECT recipe, key FROM recipe_part WHERE tbl=?1 AND el=?2")?
        .query_map(params![tbl, el], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut inv = inv;
    for (recipe, key) in rows {
        inv = inv.sql(
            "INSERT OR REPLACE INTO recipe_part(recipe, key, tbl, el) VALUES (?1,?2,?3,?4)",
            vec![
                Value::Integer(recipe),
                Value::Text(key),
                Value::Text(tbl.to_string()),
                Value::Integer(el),
            ],
        );
    }
    db.execute(
        "DELETE FROM recipe_part WHERE tbl=?1 AND el=?2",
        params![tbl, el],
    )?;
    Ok(inv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Form, MassSpec};

    fn cube(m: &mut Model, x: f64) -> i64 {
        let level = m.add_level(&format!("L{x}"), x).unwrap();
        m.add_mass(&MassSpec {
            level,
            name: "box".into(),
            void: false,
            inplace: false,
            form: Form::Cuboid {
                lo: (x, 0.0, 0.0),
                hi: (x + 1.0, 1.0, 1.0),
            },
        })
        .unwrap()
    }

    fn recipe(name: &str) -> Recipe {
        Recipe {
            id: 0,
            name: name.into(),
            script: "box()".into(),
            params_json: "{}".into(),
            hash: "h".into(),
            version: 1,
        }
    }

    fn part(el: i64) -> Vec<RecipePart> {
        vec![RecipePart {
            key: "p0".into(),
            tbl: "mass".into(),
            el,
        }]
    }

    #[test]
    fn the_same_part_key_of_two_scripts_gives_two_element_keys() {
        let mut m = Model::open(":memory:").unwrap();
        let (a, b) = (cube(&mut m, 0.0), cube(&mut m, 5.0));
        let ra = m.recipe_add(&recipe("a"), &part(a)).unwrap();
        let rb = m.recipe_add(&recipe("b"), &part(b)).unwrap();
        let (ka, kb) = (
            m.recipe_part_key("mass", a).unwrap().unwrap(),
            m.recipe_part_key("mass", b).unwrap().unwrap(),
        );
        assert_ne!(ka, kb);
        assert!(ka.ends_with("/p0") && kb.ends_with("/p0"), "{ka} {kb}");
        // the script-local key a rerun works with is unchanged
        assert_eq!(m.recipe_of("mass", a).unwrap(), Some((ra, "p0".into())));
        assert_eq!(m.recipe_parts(rb).unwrap(), part(b));
        assert_eq!(m.recipe_part_key("mass", 999).unwrap(), None);
    }

    #[test]
    fn recipes_without_an_instance_key_get_distinct_ones() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value);
             INSERT INTO meta VALUES ('project_uid', 'p-1');
             CREATE TABLE recipe(id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '',
                 script TEXT NOT NULL, params_json TEXT NOT NULL DEFAULT '{}', hash TEXT NOT NULL,
                 version INTEGER NOT NULL DEFAULT 1);
             INSERT INTO recipe(id, script, hash) VALUES (1, 's', 'h'), (2, 's', 'h');",
        )
        .unwrap();
        migrate(&db).unwrap();
        let keys: Vec<String> = db
            .prepare("SELECT ikey FROM recipe ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().all(|k| k.len() == 36), "{keys:?}");
        assert_ne!(keys[0], keys[1]);
        // a second open keeps them
        migrate(&db).unwrap();
        let again: String = db.query_row("SELECT ikey FROM recipe WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(again, keys[0]);
    }

    #[test]
    fn instance_key_is_the_same_for_a_draft_copy_and_the_model() {
        // a run_script draft on a copy and its Apply on the model add the same recipe row
        let open = || {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(
                "CREATE TABLE meta(key TEXT PRIMARY KEY, value);
                 INSERT INTO meta VALUES ('project_uid', 'p-1');
                 CREATE TABLE recipe(id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '',
                     script TEXT NOT NULL, params_json TEXT NOT NULL DEFAULT '{}', hash TEXT NOT NULL,
                     version INTEGER NOT NULL DEFAULT 1, ikey TEXT);",
            )
            .unwrap();
            db
        };
        let (draft, live) = (open(), open());
        let a = mint_ikey(&draft, 1, "h").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = mint_ikey(&live, 1, "h").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn instance_key_is_minted_when_project_uid_is_an_integer() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value);
             INSERT INTO meta VALUES ('project_uid', 4611686018427387905);
             CREATE TABLE recipe(id INTEGER PRIMARY KEY, hash TEXT NOT NULL, ikey TEXT);",
        )
        .unwrap();
        let k = mint_ikey(&db, 1, "h").unwrap();
        assert_eq!(k.len(), 36, "{k}");
    }
}
