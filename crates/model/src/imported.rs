// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Imported elements: checked inserts for importers running inside a batch (members, roofs,
//! ceilings), and the read/edit API for generic meshes and properties (`mesh`, `prop`, `ext_ref`).
//! Mesh blobs: positions f64 LE xyz (world metres), indices u32 LE triangles.

use ddc_geom::mesh::Mesh;
use ddc_geom::v3;
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{BeamSpec, CeilingSpec, ColumnSpec, Error, Model, RoofSpec};

/// Checked column insert (`id` = Some: replace that row in place).
pub fn put_column(db: &Connection, s: &ColumnSpec, id: Option<i64>) -> Result<i64, Error> {
    crate::structure::check_column(db, s)?;
    crate::structure::insert_column(db, s, id)
}

pub fn put_beam(db: &Connection, s: &BeamSpec, id: Option<i64>) -> Result<i64, Error> {
    crate::structure::check_beam(db, s)?;
    crate::structure::insert_beam(db, s, id)
}

pub fn put_roof(db: &Connection, s: &RoofSpec, id: Option<i64>) -> Result<i64, Error> {
    crate::roofs::check(db, s)?;
    crate::roofs::write(db, s, id)
}

pub fn put_ceiling(db: &Connection, s: &CeilingSpec, id: Option<i64>) -> Result<i64, Error> {
    crate::shafts::check_ceiling(db, s, id)?;
    crate::shafts::write_ceiling(db, s, id)
}

/// Generic imported mesh (`mesh` row).
#[derive(Clone, Debug)]
pub struct ImportedMesh {
    pub id: i64,
    pub level: Option<i64>,
    pub category: String,
    pub name: String,
    pub mesh: Mesh,
}

pub fn mesh_blobs(m: &Mesh) -> (Vec<u8>, Vec<u8>) {
    let mut pb = Vec::with_capacity(m.positions.len() * 24);
    for p in &m.positions {
        for c in [p.x, p.y, p.z] {
            pb.extend_from_slice(&c.to_le_bytes());
        }
    }
    let ib = m
        .triangles
        .iter()
        .flatten()
        .flat_map(|i| i.to_le_bytes())
        .collect();
    (pb, ib)
}

fn decode(pb: &[u8], ib: &[u8]) -> Mesh {
    let f = |c: &[u8]| f64::from_le_bytes(c.try_into().unwrap_or([0; 8]));
    let positions: Vec<_> = pb
        .chunks_exact(24)
        .map(|c| v3(f(&c[0..8]), f(&c[8..16]), f(&c[16..24])))
        .collect();
    let n = positions.len() as u32;
    let idx: Vec<u32> = ib
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap_or([0; 4])))
        .collect();
    let triangles: Vec<[u32; 3]> = idx
        .chunks_exact(3)
        .map(|t| [t[0], t[1], t[2]])
        .filter(|t| t.iter().all(|&i| i < n))
        .collect();
    let face_of = vec![0; triangles.len()];
    Mesh {
        positions,
        triangles,
        face_of,
    }
}

fn mesh_row(r: &rusqlite::Row) -> rusqlite::Result<ImportedMesh> {
    let pb: Vec<u8> = r.get(4)?;
    let ib: Vec<u8> = r.get(5)?;
    Ok(ImportedMesh {
        id: r.get(0)?,
        level: r.get(1)?,
        category: r.get(2)?,
        name: r.get(3)?,
        mesh: decode(&pb, &ib),
    })
}

const MESH_SEL: &str = "SELECT id, level, category, name, positions, indices FROM mesh";

impl Model {
    /// All imported meshes (optionally of one level), by id.
    pub fn imported_meshes(&self, level: Option<i64>) -> Result<Vec<ImportedMesh>, Error> {
        let db = self.sql();
        let mut st = db.prepare(&format!(
            "{MESH_SEL} WHERE ?1 IS NULL OR level=?1 ORDER BY id"
        ))?;
        let r = st.query_map([level], mesh_row)?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    pub fn imported_mesh(&self, id: i64) -> Result<ImportedMesh, Error> {
        self.sql()
            .query_row(&format!("{MESH_SEL} WHERE id=?1"), [id], mesh_row)
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such mesh {id}")))
    }

    /// Properties of a row (`kind` = table name): (key, value, unit) by key.
    pub fn props(&self, kind: &str, id: i64) -> Result<Vec<(String, Value, String)>, Error> {
        let mut st = self
            .sql()
            .prepare("SELECT key, value, unit FROM prop WHERE kind=?1 AND ref=?2 ORDER BY key")?;
        let r = st.query_map(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// External (source, guid) of a row, if it was imported.
    pub fn ext_guid(&self, kind: &str, id: i64) -> Result<Option<(String, String)>, Error> {
        Ok(self
            .sql()
            .query_row(
                "SELECT source, guid FROM ext_ref WHERE kind=?1 AND ref=?2 ORDER BY guid LIMIT 1",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// Sets (or with `None` removes) one property; undoable. Editing a wall's external flag
    /// also drops the marker that says the exporter computed it: the value is now the user's.
    pub fn set_prop(
        &mut self,
        kind: &str,
        id: i64,
        key: &str,
        value: Option<(Value, &str)>,
    ) -> Result<(), Error> {
        if key.is_empty() {
            return Err(Error::Invalid("property key is empty".into()));
        }
        let (kind, key) = (kind.to_string(), key.to_string());
        self.op("prop_set", |db| {
            let mut inv = prop_restore(db, &kind, id, &key)?;
            db.execute(
                "DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                params![kind, id, key],
            )?;
            if kind == "wall" && key == WALL_IS_EXTERNAL_KEY {
                inv = inv.then(prop_restore(db, &kind, id, WALL_COMPUTED_MARKER)?);
                db.execute(
                    "DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                    params![kind, id, WALL_COMPUTED_MARKER],
                )?;
            }
            if let Some((v, unit)) = value {
                db.execute(
                    "INSERT INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
                    params![kind, id, key, v, unit],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    /// Several [`Self::set_prop`] as one operation, in order: one journal record, one undo,
    /// the same rows as one by one. Empty input: no-op.
    pub fn set_props(&mut self, sets: &[PropWrite<'_>]) -> Result<(), Error> {
        if sets.is_empty() {
            return Ok(());
        }
        if sets.iter().any(|s| s.key.is_empty()) {
            return Err(Error::Invalid("property key is empty".into()));
        }
        self.op("prop_set", |db| {
            let mut del =
                db.prepare_cached("DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3")?;
            let mut put = db.prepare_cached(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
            )?;
            let mut inv = Inv::default();
            for s in sets {
                // undo runs newest first, as the records of single sets would
                let mut one = prop_restore(db, s.kind, s.id, s.key)?;
                del.execute(params![s.kind, s.id, s.key])?;
                if s.kind == "wall" && s.key == WALL_IS_EXTERNAL_KEY {
                    one = one.then(prop_restore(db, s.kind, s.id, WALL_COMPUTED_MARKER)?);
                    del.execute(params![s.kind, s.id, WALL_COMPUTED_MARKER])?;
                }
                if let Some((v, unit)) = &s.value {
                    put.execute(params![s.kind, s.id, s.key, v, unit])?;
                }
                inv = one.then(inv);
            }
            Ok(((), sets[0].id, inv))
        })
    }

    /// Moves an imported mesh by `d`; undoable (journals the inverse shift, not the body).
    pub fn move_mesh(&mut self, id: i64, d: (f64, f64, f64)) -> Result<(), Error> {
        if ![d.0, d.1, d.2].iter().all(|v| v.is_finite()) {
            return Err(Error::Invalid("mesh move: not finite".into()));
        }
        const I: [[f64; 2]; 2] = [[1.0, 0.0], [0.0, 1.0]];
        self.op("mesh_move", |db| {
            crate::meshes::xf_mesh(db, id, I, (d.0, d.1), d.2)?;
            let (sql, args) = crate::meshes::xf_inverse(id, I, (d.0, d.1), d.2)?;
            Ok(((), id, Inv::default().sql(&sql, args)))
        })
    }
}

/// One property write of [`Model::set_props`]: `value` None removes it.
#[derive(Clone, Debug)]
pub struct PropWrite<'a> {
    pub kind: &'a str,
    pub id: i64,
    pub key: &'a str,
    pub value: Option<(Value, &'a str)>,
}

/// Property key of a wall's external flag and the marker written beside a flag the exporter computed.
pub const WALL_IS_EXTERNAL_KEY: &str = "Pset_WallCommon.IsExternal";
pub const WALL_COMPUTED_MARKER: &str = "DDC_Computed.WallIsExternal";

pub(crate) fn prop_restore(db: &Connection, kind: &str, id: i64, key: &str) -> Result<Inv, Error> {
    let mut inv = Inv::default().sql(
        "DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
        vec![
            Value::Text(kind.into()),
            Value::Integer(id),
            Value::Text(key.into()),
        ],
    );
    let old: Option<(i64, Value, String)> = db
        .prepare_cached(
            "SELECT rowid, value, unit FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
        )?
        .query_row(params![kind, id, key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    // rowid kept: a restored row above an earlier batch's watermark would be dropped by its undo
    if let Some((row, v, u)) = old {
        inv = inv.sql(
            "INSERT INTO prop(rowid,kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5,?6)",
            vec![
                Value::Integer(row),
                Value::Text(kind.into()),
                Value::Integer(id),
                Value::Text(key.into()),
                v,
                Value::Text(u),
            ],
        );
    }
    Ok(inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_wall_is_external_drops_computed_marker_and_undo_restores_it() {
        let mut m = Model::open(":memory:").unwrap();
        m.batch_walls("wall_seed", |db| {
            db.execute("INSERT INTO prop VALUES ('wall',1,?1,0,'')", [WALL_IS_EXTERNAL_KEY])?;
            db.execute("INSERT INTO prop VALUES ('wall',1,?1,0,'')", [WALL_COMPUTED_MARKER])?;
            Ok(())
        })
        .unwrap();
        m.set_prop("wall", 1, WALL_IS_EXTERNAL_KEY, Some((Value::Integer(0), "")))
            .unwrap();
        let has = |m: &Model| {
            m.props("wall", 1)
                .unwrap()
                .iter()
                .any(|p| p.0 == WALL_COMPUTED_MARKER)
        };
        assert!(!has(&m));
        m.undo().unwrap();
        assert!(has(&m));
    }

    #[test]
    fn mesh_props_edit_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let mesh = Mesh {
            positions: vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0)],
            triangles: vec![[0, 1, 2]],
            face_of: vec![0],
        };
        let (pb, ib) = mesh_blobs(&mesh);
        m.batch_walls("wall_import_test", |db| {
            db.execute(
                "INSERT INTO mesh(level,category,name,positions,indices) VALUES (NULL,'Furniture','Chair',?1,?2)",
                params![pb, ib],
            )?;
            db.execute("INSERT INTO prop VALUES ('mesh',1,'Pset.A',2.5,'m')", [])?;
            db.execute("INSERT INTO ext_ref VALUES ('ifc','g1','mesh',1)", [])?;
            Ok(())
        })
        .unwrap();
        let all = m.imported_meshes(None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].mesh.positions, mesh.positions);
        assert_eq!(all[0].mesh.triangles, mesh.triangles);
        assert_eq!(m.props("mesh", 1).unwrap()[0].1, Value::Real(2.5));
        assert_eq!(m.ext_guid("mesh", 1).unwrap().unwrap().1, "g1");
        m.set_prop("mesh", 1, "Pset.A", Some((Value::Text("x".into()), "")))
            .unwrap();
        assert_eq!(m.props("mesh", 1).unwrap()[0].1, Value::Text("x".into()));
        m.undo().unwrap();
        assert_eq!(m.props("mesh", 1).unwrap()[0].1, Value::Real(2.5));
        m.move_mesh(1, (1.0, 2.0, 3.0)).unwrap();
        assert_eq!(
            m.imported_mesh(1).unwrap().mesh.positions[0],
            v3(1.0, 2.0, 3.0)
        );
        m.delete_mesh(1).unwrap();
        assert!(m.imported_meshes(None).unwrap().is_empty());
        assert!(m.props("mesh", 1).unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.imported_meshes(None).unwrap().len(), 1);
        assert_eq!(m.props("mesh", 1).unwrap().len(), 1);
        assert!(m.ext_guid("mesh", 1).unwrap().is_some());
    }
}
