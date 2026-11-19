//! Editing system types: slab layers, bar sections, route sizes, type duplication.
//! Each edit is one journal operation (one undo); undo restores the row snapshots.

use ddc_geom::member::Shape;
use rusqlite::{params, Connection};

use crate::ops::Inv;
use crate::{Error, LayerSpec, Model};

/// System type table for [`Model::duplicate_type`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeTable {
    Wall,
    Slab,
    Member,
}

/// A route size change for [`Model::set_mep_sizes`].
#[derive(Clone, Debug, PartialEq)]
pub struct MepResize {
    pub domain: String,
    pub shape: String,
    pub w: f64,
    pub h: f64,
    pub nw: f64,
    pub nh: f64,
}

/// Copies the per-type user parameter values and property rows of type `from` to `to`
/// (`kind` = type table name: wall_type / slab_type / member_type). Runs inside the caller's
/// operation; undo is the caller's (see [`copy_type_data_inv`] or a batch watermark).
pub fn copy_type_data(
    db: &rusqlite::Connection,
    kind: &str,
    from: i64,
    to: i64,
) -> Result<(), Error> {
    db.execute(
        "INSERT OR REPLACE INTO pparam_val(param, kind, ref, value) \
         SELECT param, kind, ?3, value FROM pparam_val WHERE kind=?1 AND ref=?2 ORDER BY rowid",
        params![kind, from, to],
    )?;
    db.execute(
        "INSERT OR REPLACE INTO prop(kind, ref, key, value, unit) \
         SELECT kind, ?3, key, value, unit FROM prop WHERE kind=?1 AND ref=?2 ORDER BY rowid",
        params![kind, from, to],
    )?;
    Ok(())
}

/// [`copy_type_data`] plus the inverse (delete the copied rows) appended to `inv`.
pub(crate) fn copy_type_data_inv(
    db: &rusqlite::Connection,
    kind: &str,
    from: i64,
    to: i64,
    inv: Inv,
) -> Result<Inv, Error> {
    copy_type_data(db, kind, from, to)?;
    let args = || {
        vec![
            rusqlite::types::Value::Text(kind.into()),
            rusqlite::types::Value::Integer(to),
        ]
    };
    Ok(inv
        .sql("DELETE FROM pparam_val WHERE kind=?1 AND ref=?2", args())
        .sql("DELETE FROM prop WHERE kind=?1 AND ref=?2", args()))
}

fn bad<T>(m: &str) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn check_layers(layers: &[LayerSpec]) -> Result<(), Error> {
    if layers.is_empty()
        || layers.iter().any(|l| {
            !(l.thickness.is_finite() && l.thickness > 0.0) || l.material.trim().is_empty()
        })
    {
        return bad("type needs layers with positive thickness and material");
    }
    Ok(())
}

/// Rules of a slab type edit, shared by `set_slab_type` and the workbook batch.
pub(crate) fn check_slab_type(name: &str, layers: &[LayerSpec]) -> Result<(), Error> {
    check_layers(layers)?;
    if name.trim().is_empty() {
        return bad("empty type name");
    }
    Ok(())
}

/// Name and layers of a slab type replaced whole; the caller snapshots the rows for undo.
pub(crate) fn write_slab_type(
    db: &Connection,
    id: i64,
    name: &str,
    layers: &[LayerSpec],
) -> Result<(), Error> {
    let n = db.execute(
        "UPDATE slab_type SET name=?2 WHERE id=?1",
        params![id, name.trim()],
    )?;
    if n == 0 {
        return bad("no such slab type");
    }
    db.execute("DELETE FROM slab_type_layer WHERE stype=?1", [id])?;
    for (pos, l) in layers.iter().enumerate() {
        db.execute(
            "INSERT INTO slab_type_layer VALUES (?1,?2,?3,?4)",
            params![id, pos as i64, l.material.trim(), l.thickness],
        )?;
    }
    Ok(())
}

impl Model {
    /// Assigns an existing slab type (`slab`) or member type (`col`, `beam`) to rows of
    /// `table`; one journal op, undo restores every row snapshot.
    pub fn set_elements_type(&mut self, table: &str, ids: &[i64], ty: i64) -> Result<(), Error> {
        let (col, type_table) = match table {
            "slab" => ("stype", "slab_type"),
            "col" => ("mtype", "member_type"),
            "beam" => ("mtype", "member_type"),
            _ => return bad("type assignment is not defined for this table"),
        };
        let known: i64 = self.db.query_row(
            &format!("SELECT COUNT(*) FROM {type_table} WHERE id=?1"),
            [ty],
            |r| r.get(0),
        )?;
        if known == 0 {
            return bad("no such type");
        }
        // kind must start with the table so `derives` refreshes dims and derived values
        let kind = format!("{table}_type_assign");
        self.op(&kind, |db| {
            let mut inv = Inv::default();
            for &id in ids {
                inv = inv.restore(db, table, "id", id)?;
                let n = db.execute(
                    &format!("UPDATE {table} SET {col}=?2 WHERE id=?1"),
                    params![id, ty],
                )?;
                if n == 0 {
                    return bad("no such element");
                }
            }
            Ok(((), ids.first().copied().unwrap_or(0), inv))
        })
    }

    /// The name and layers of a slab type are replaced entirely.
    pub fn set_slab_type(
        &mut self,
        id: i64,
        name: &str,
        layers: &[LayerSpec],
    ) -> Result<(), Error> {
        check_slab_type(name, layers)?;
        self.op("slab_type_set", |db| {
            let inv = Inv::default()
                .delete("slab_type_layer", "stype", id)
                .restore(db, "slab_type", "id", id)?
                .restore(db, "slab_type_layer", "stype", id)?;
            write_slab_type(db, id, name, layers)?;
            Ok(((), id, inv))
        })
    }

    /// Name and layers of a wall type replaced whole (layers may be added or removed); one undo step.
    pub fn set_wall_type_layers(
        &mut self,
        id: i64,
        name: &str,
        layers: &[LayerSpec],
    ) -> Result<(), Error> {
        check_slab_type(name, layers)?;
        self.op("wall_type_layers_set", |db| {
            let inv = Inv::default()
                .delete("wall_type_layer", "wtype", id)
                .restore(db, "wall_type", "id", id)?
                .restore(db, "wall_type_layer", "wtype", id)?;
            let n = db.execute(
                "UPDATE wall_type SET name=?2 WHERE id=?1",
                params![id, name.trim()],
            )?;
            if n == 0 {
                return bad("no such wall type");
            }
            db.execute("DELETE FROM wall_type_layer WHERE wtype=?1", [id])?;
            for (pos, l) in layers.iter().enumerate() {
                db.execute(
                    "INSERT INTO wall_type_layer VALUES (?1,?2,?3,?4)",
                    params![id, pos as i64, l.material.trim(), l.thickness],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    /// Name, section and material of a bar type.
    pub fn set_member_type(
        &mut self,
        id: i64,
        name: &str,
        shape: &Shape,
        material: &str,
    ) -> Result<(), Error> {
        shape.check().map_err(Error::Invalid)?;
        if name.trim().is_empty() {
            return bad("empty type name");
        }
        let text = shape.text();
        self.op("member_type_set", |db| {
            let inv = Inv::default().restore(db, "member_type", "id", id)?;
            let n = db.execute(
                "UPDATE member_type SET name=?2, shape=?3, material=?4 WHERE id=?1",
                params![id, name.trim(), text, material.trim()],
            )?;
            if n == 0 {
                return bad("no such member type");
            }
            Ok(((), id, inv))
        })
    }

    /// Route sizes of a system in one operation: each pair is (domain, shape, w, h) -> (nw, nh)
    /// for all routes of that size; returns the number of changed routes.
    pub fn set_mep_sizes(&mut self, system: i64, changes: &[MepResize]) -> Result<usize, Error> {
        let mut todo = vec![];
        for c in changes {
            if !(c.nw.is_finite() && c.nw > 0.0 && c.nh.is_finite() && c.nh >= 0.0) {
                return bad("bad duct/pipe size");
            }
            let mut st = self.db.prepare(
                "SELECT id FROM mep_run WHERE system=?1 AND domain=?2 AND shape=?3                  AND abs(w-?4)<1e-9 AND abs(h-?5)<1e-9",
            )?;
            let ids: Vec<i64> = st
                .query_map(params![system, c.domain, c.shape, c.w, c.h], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let nh = if c.shape == "round" { c.nw } else { c.nh };
            todo.extend(ids.into_iter().map(|id| (id, c.nw, nh)));
        }
        if todo.is_empty() {
            return bad("no runs of this size");
        }
        self.op("mep_size_set", |db| {
            let mut inv = Inv::default();
            for &(id, w, h) in &todo {
                inv = inv.restore(db, "mep_run", "id", id)?;
                db.execute(
                    "UPDATE mep_run SET w=?2, h=?3 WHERE id=?1",
                    params![id, w, h],
                )?;
            }
            Ok((todo.len(), todo[0].0, inv))
        })
    }

    /// A copy of a type under a new name (layers are copied); the id of the new type.
    pub fn duplicate_type(&mut self, t: TypeTable, id: i64, name: &str) -> Result<i64, Error> {
        let name = name.trim();
        if name.is_empty() {
            return bad("empty type name");
        }
        let (table, layers, key, kind) = match t {
            TypeTable::Wall => (
                "wall_type",
                Some("wall_type_layer"),
                "wtype",
                "wall_type_dup",
            ),
            TypeTable::Slab => (
                "slab_type",
                Some("slab_type_layer"),
                "stype",
                "slab_type_dup",
            ),
            TypeTable::Member => ("member_type", None, "", "member_type_dup"),
        };
        self.op(kind, |db| {
            let cols: Vec<String> = {
                let st = db.prepare(&format!("SELECT * FROM {table} LIMIT 0"))?;
                st.column_names()
                    .filter(|c| !matches!(*c, "id" | "name"))
                    .map(|c| c.to_string())
                    .collect()
            };
            let rest = cols.iter().map(|c| format!(",{c}")).collect::<String>();
            let n = db.execute(
                &format!(
                    "INSERT INTO {table}(name{rest}) SELECT ?2{rest} FROM {table} WHERE id=?1"
                ),
                params![id, name],
            )?;
            if n == 0 {
                return bad("no such type");
            }
            let new = db.last_insert_rowid();
            let mut inv = Inv::default();
            inv = copy_type_data_inv(db, table, id, new, inv)?;
            if let Some(lt) = layers {
                db.execute(
                    &format!(
                        "INSERT INTO {lt}({key}, pos, material, thickness) \
                         SELECT ?2, pos, material, thickness FROM {lt} WHERE {key}=?1"
                    ),
                    params![id, new],
                )?;
                inv = inv.delete(lt, key, new);
            }
            Ok((new, new, inv.delete(table, "id", new)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(v: &[(&str, f64)]) -> Vec<LayerSpec> {
        v.iter()
            .map(|(m, t)| LayerSpec {
                material: (*m).into(),
                thickness: *t,
            })
            .collect()
    }

    #[test]
    fn slab_type_set_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let id = m.add_slab_type("Slab-test", &ls(&[("Concrete", 0.2)])).unwrap();
        m.set_slab_type(id, "Slab-test 2", &ls(&[("Screed", 0.05), ("Concrete", 0.25)]))
            .unwrap();
        let q = |m: &Model| -> (String, f64) {
            m.sql()
                .query_row(
                    "SELECT name, (SELECT sum(thickness) FROM slab_type_layer WHERE stype=?1) \
                     FROM slab_type WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
        };
        assert_eq!(q(&m).0, "Slab-test 2");
        assert!((q(&m).1 - 0.30).abs() < 1e-9);
        m.undo().unwrap();
        assert_eq!(q(&m).0, "Slab-test");
        assert!((q(&m).1 - 0.2).abs() < 1e-9);
        assert!(m.set_slab_type(id, "x", &ls(&[("Concrete", -1.0)])).is_err());
    }

    fn seed(m: &Model, kind: &str, id: i64) {
        m.sql()
            .execute(
                "INSERT INTO pparam_val(param, kind, ref, value) VALUES (7,?1,?2,'F-90')",
                params![kind, id],
            )
            .unwrap();
        m.sql()
            .execute(
                "INSERT INTO prop(kind, ref, key, value, unit) VALUES (?1,?2,'Mark','S1','')",
                params![kind, id],
            )
            .unwrap();
    }

    fn rows(m: &Model, kind: &str, id: i64) -> (i64, i64) {
        let c = |t: &str| -> i64 {
            m.sql()
                .query_row(
                    &format!("SELECT count(*) FROM {t} WHERE kind=?1 AND ref=?2"),
                    params![kind, id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        (c("pparam_val"), c("prop"))
    }

    #[test]
    fn duplicate_type_keeps_type_param_values() {
        let mut m = Model::open(":memory:").unwrap();
        let w = m.add_wall_type("W-src", &ls(&[("Brick", 0.25)])).unwrap();
        let s = m.add_slab_type("S-src", &ls(&[("Concrete", 0.2)])).unwrap();
        let sh = Shape::parse("rect 0.3 0.4").unwrap();
        let t = m.add_member_type("M-src", &sh, "Concrete").unwrap();
        for (tt, kind, id) in [
            (TypeTable::Wall, "wall_type", w),
            (TypeTable::Slab, "slab_type", s),
            (TypeTable::Member, "member_type", t),
        ] {
            seed(&m, kind, id);
            let c = m.duplicate_type(tt, id, "copy").unwrap();
            assert_eq!(rows(&m, kind, c), (1, 1), "{kind}");
            let v: String = m
                .sql()
                .query_row(
                    "SELECT value FROM pparam_val WHERE kind=?1 AND ref=?2",
                    params![kind, c],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(v, "F-90");
            assert_eq!(rows(&m, kind, id), (1, 1), "source untouched");
        }
    }

    #[test]
    fn undo_duplicate_removes_copied_values() {
        let mut m = Model::open(":memory:").unwrap();
        let w = m.add_wall_type("W-src", &ls(&[("Brick", 0.25)])).unwrap();
        seed(&m, "wall_type", w);
        let top = m.op_top().unwrap();
        let c = m.duplicate_type(TypeTable::Wall, w, "copy").unwrap();
        assert_eq!(m.op_top().unwrap(), top + 1, "one undo step");
        m.undo().unwrap();
        assert_eq!(rows(&m, "wall_type", c), (0, 0));
        assert_eq!(rows(&m, "wall_type", w), (1, 1));
        // redo: the same duplication again copies the values again
        let c2 = m.duplicate_type(TypeTable::Wall, w, "copy").unwrap();
        assert_eq!(rows(&m, "wall_type", c2), (1, 1));
        assert_eq!(m.op_top().unwrap(), top + 1);
    }

    #[test]
    fn duplicate_member_and_wall_type() {
        let mut m = Model::open(":memory:").unwrap();
        let w = m.add_wall_type("Wall-test", &ls(&[("Brick", 0.25)])).unwrap();
        m.add_wall_type("Wall-taken", &ls(&[("Brick", 0.1)]))
            .unwrap();
        assert!(m.duplicate_type(TypeTable::Wall, w, "Wall-taken").is_err());
        let w2 = m.duplicate_type(TypeTable::Wall, w, "Wall-copy").unwrap();
        let n: i64 = m
            .sql()
            .query_row(
                "SELECT count(*) FROM wall_type_layer WHERE wtype=?1",
                [w2],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        m.undo().unwrap();
        let gone = m
            .sql()
            .query_row("SELECT 1 FROM wall_type WHERE id=?1", [w2], |_| Ok(()))
            .is_err();
        assert!(gone);
        let s = Shape::parse("rect 0.3 0.4").unwrap();
        let t = m.add_member_type("Member-test", &s, "Concrete").unwrap();
        let t2 = m.duplicate_type(TypeTable::Member, t, "Member-copy").unwrap();
        let s2 = Shape::parse("rect 0.4 0.4").unwrap();
        m.set_member_type(t2, "Member-copy", &s2, "Concrete").unwrap();
        m.undo().unwrap();
        let sh: String = m
            .sql()
            .query_row("SELECT shape FROM member_type WHERE id=?1", [t2], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(Shape::parse(&sh).unwrap().text(), s.text());
    }

    #[test]
    fn type_assign_kinds_refresh_derived_values() {
        for t in ["slab", "col", "beam"] {
            assert!(crate::derives(&format!("{t}_type_assign")), "{t}");
        }
        assert!(!crate::derives("elements_type_set"));
    }
}
