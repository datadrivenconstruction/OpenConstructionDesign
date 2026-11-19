//! Imported solids without a parametric model (`mesh`) and native imported rows:
//! deletion is a single undo step; move/rotate/mirror of meshes are journaled as a transform
//! (`MESH_XF`), not as a copy of the body. Reading solids and properties lives in `imported.rs`.
//!
//! `ext_ref` policy on delete: the link row stays. A link to a missing row means
//! "deleted here": re-import does not resurrect it (the merge decides by the import base), and undo
//! of the delete returns the element to the same link.

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

/// Journal inverse command: apply a plan transform to a mesh (args: id, m00, m01,
/// m10, m11, tx, ty, dz). Executed by `undo` in Rust, not SQL.
pub(crate) const MESH_XF: &str = "@mesh_xf";

type M2 = [[f64; 2]; 2];

/// `p' = m·p + t`, `z' = z + dz` for all vertices of mesh `id`; a mirroring `m` reverses face winding.
pub(crate) fn xf_mesh(
    db: &Connection,
    id: i64,
    m: M2,
    t: (f64, f64),
    dz: f64,
) -> Result<(), Error> {
    let row: Option<(Vec<u8>, Vec<u8>)> = db
        .prepare_cached("SELECT positions, indices FROM mesh WHERE id=?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((pb, ib)) = row else {
        return Err(Error::Invalid(format!("no such mesh {id}")));
    };
    let mut out = Vec::with_capacity(pb.len());
    for c in pb.chunks_exact(24) {
        let g = |k: usize| f64::from_le_bytes(c[k * 8..k * 8 + 8].try_into().unwrap_or([0; 8]));
        let (x, y) = (g(0), g(1));
        for v in [
            m[0][0] * x + m[0][1] * y + t.0,
            m[1][0] * x + m[1][1] * y + t.1,
            g(2) + dz,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    if m[0][0] * m[1][1] - m[0][1] * m[1][0] < 0.0 {
        let mut v = ib;
        for t in v.chunks_exact_mut(12) {
            let (a, b) = t.split_at_mut(8);
            a[4..8].swap_with_slice(&mut b[0..4]);
        }
        db.execute(
            "UPDATE mesh SET positions=?2, indices=?3 WHERE id=?1",
            params![id, out, v],
        )?;
    } else {
        db.execute("UPDATE mesh SET positions=?2 WHERE id=?1", params![id, out])?;
    }
    Ok(())
}

/// Journal entry that undoes `xf_mesh(id, m, t, dz)`: the inverse transform, a few numbers
/// instead of the body (a move of thousands of meshes stays a small log row).
pub(crate) fn xf_inverse(
    id: i64,
    m: M2,
    t: (f64, f64),
    dz: f64,
) -> Result<(String, Vec<Value>), Error> {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    if !det.is_finite() || det.abs() < 1e-12 {
        return Err(Error::Invalid("mesh transform is not invertible".into()));
    }
    let mi = [
        [m[1][1] / det, -m[0][1] / det],
        [-m[1][0] / det, m[0][0] / det],
    ];
    let ti = (
        -(mi[0][0] * t.0 + mi[0][1] * t.1),
        -(mi[1][0] * t.0 + mi[1][1] * t.1),
    );
    let args = [
        id as f64, mi[0][0], mi[0][1], mi[1][0], mi[1][1], ti.0, ti.1, -dz,
    ];
    let mut v = vec![Value::Integer(id)];
    v.extend(args[1..].iter().map(|&x| Value::Real(x)));
    Ok((MESH_XF.to_string(), v))
}

/// Applies a `MESH_XF` journal entry.
pub(crate) fn undo_xf(db: &Connection, args: &[Value]) -> Result<(), Error> {
    let bad = || Error::Invalid("bad mesh transform in op log".into());
    let id = match args.first() {
        Some(Value::Integer(i)) => *i,
        _ => return Err(bad()),
    };
    let mut f = [0.0; 7];
    for (k, slot) in f.iter_mut().enumerate() {
        *slot = match args.get(k + 1) {
            Some(Value::Real(x)) => *x,
            Some(Value::Integer(i)) => *i as f64,
            _ => return Err(bad()),
        };
    }
    xf_mesh(db, id, [[f[0], f[1]], [f[2], f[3]]], (f[4], f[5]), f[6])
}

impl Model {
    /// Solid ids, by level (None for all).
    pub fn meshes(&self, level: Option<i64>) -> Result<Vec<i64>, Error> {
        let mut st = self
            .sql()
            .prepare("SELECT id FROM mesh WHERE ?1 IS NULL OR level=?1 ORDER BY id")?;
        let v = st
            .query_map([level], |r| r.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        Ok(v)
    }

    /// Deletes a solid with its properties; the `ext_ref` link stays (see the module docs).
    pub fn delete_mesh(&mut self, id: i64) -> Result<(), Error> {
        self.delete_meshes(&[id])
    }

    /// Deletes solids with their properties in one operation (one undo).
    pub fn delete_meshes(&mut self, ids: &[i64]) -> Result<(), Error> {
        let v: Vec<(&str, i64)> = ids.iter().map(|&i| ("mesh", i)).collect();
        self.delete_imported(&v)
    }

    /// Deletes elements (`mesh`, `col`, `beam`, `mep_run`, `ceiling`: table and id) together with
    /// properties and outlines in one operation (one undo). `ext_ref` links stay.
    pub fn delete_imported(&mut self, items: &[(&str, i64)]) -> Result<(), Error> {
        let Some(&(_, first)) = items.first() else {
            return Ok(());
        };
        if let Some((t, _)) = items
            .iter()
            .find(|(t, _)| !matches!(*t, "mesh" | "col" | "beam" | "mep_run" | "ceiling"))
        {
            return Err(Error::Invalid(format!("not an imported kind: {t}")));
        }
        // members and ceilings have derived quantities (`member_qty`, `ceiling_qty`, column cuts
        // in walls): the "member" kind regenerates them; plain meshes skip the regeneration
        let kind = if items.iter().any(|(t, _)| *t != "mesh") {
            "member_imported_delete"
        } else {
            "imported_delete"
        };
        self.op(kind, |db| {
            let mut inv = Inv::default();
            for &(table, id) in items {
                inv = inv.restore(db, table, "id", id)?;
                if table == "ceiling" {
                    inv = inv.restore(db, "ceiling_vertex", "ceiling", id)?;
                }
                let rows: Vec<Vec<Value>> = {
                    let mut st = db.prepare_cached(
                        "SELECT rowid, kind, ref, key, value, unit FROM prop WHERE kind=?1 AND ref=?2",
                    )?;
                    let v = st
                        .query_map(params![table, id], |r| {
                            (0..6).map(|i| r.get::<_, Value>(i)).collect()
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    v
                };
                // rowid is preserved: otherwise undoing an earlier batch operation would delete the row
                for r in rows {
                    inv = inv.sql(
                        "INSERT INTO prop(rowid,kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5,?6)",
                        r,
                    );
                }
                if db.execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])? == 0 {
                    return Err(Error::Invalid(format!("no such {table} {id}")));
                }
                db.execute("DELETE FROM prop WHERE kind=?1 AND ref=?2", params![table, id])?;
                if table == "ceiling" {
                    db.execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [id])?;
                    inv = crate::components::drop_hosted(db, inv, "host_ceiling", id)?;
                }
            }
            Ok(((), first, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_transform_restores_points_and_winding() {
        let m = Model::open(":memory:").unwrap();
        let pb: Vec<u8> = [[0.0f64, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 2.0]]
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let ib: Vec<u8> = [0u32, 1, 2].iter().flat_map(|i| i.to_le_bytes()).collect();
        let db = m.sql();
        db.execute(
            "INSERT INTO mesh(level,category,name,positions,indices) VALUES (NULL,'x','x',?1,?2)",
            params![pb, ib],
        )
        .unwrap();
        // mirror about x = 1, then shift
        let mx = [[-1.0, 0.0], [0.0, 1.0]];
        xf_mesh(db, 1, mx, (2.0, 0.5), 0.25).unwrap();
        let (sql, args) = xf_inverse(1, mx, (2.0, 0.5), 0.25).unwrap();
        assert_eq!(sql, MESH_XF);
        undo_xf(db, &args).unwrap();
        let (p2, i2): (Vec<u8>, Vec<u8>) = db
            .query_row("SELECT positions, indices FROM mesh WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(i2, ib, "winding back");
        for (a, b) in p2.chunks_exact(8).zip(pb.chunks_exact(8)) {
            let f = |c: &[u8]| f64::from_le_bytes(c.try_into().unwrap());
            assert!((f(a) - f(b)).abs() < 1e-12);
        }
        assert!(xf_inverse(1, [[0.0, 0.0], [0.0, 0.0]], (0.0, 0.0), 0.0).is_err());
    }
}
