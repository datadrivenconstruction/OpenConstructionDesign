//! Named work planes of the project: one `meta` row (JSON list). A plane is a point and two
//! directions in model coordinates (z is the absolute elevation), so it keeps its place when
//! levels move. Written as a journaled operation: a change is one undo step and travels with
//! the file.

use rusqlite::types::Value;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::ops::Inv;
use crate::{Error, Model};

/// `meta` key of the stored list.
pub const WORK_PLANES_KEY: &str = "work_planes";

/// One named plane: `origin` on it, `u` along it, `n` its normal (both unit length).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamedPlane {
    /// Stable within the project: never reused after a delete.
    pub id: i64,
    pub name: String,
    pub origin: [f64; 3],
    pub u: [f64; 3],
    pub n: [f64; 3],
}

fn bad(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("work planes: {e}"))
}

fn unit(v: [f64; 3]) -> Option<[f64; 3]> {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (l.is_finite() && l > 1e-9).then(|| [v[0] / l, v[1] / l, v[2] / l])
}

impl NamedPlane {
    /// A plane through `origin` with normal `n`; `u` is projected into the plane. `None` for a
    /// zero normal, a `u` along the normal, an empty name or a point that is not finite.
    pub fn new(id: i64, name: &str, origin: [f64; 3], u: [f64; 3], n: [f64; 3]) -> Option<Self> {
        let n = unit(n)?;
        let d = u[0] * n[0] + u[1] * n[1] + u[2] * n[2];
        let u = unit([u[0] - d * n[0], u[1] - d * n[1], u[2] - d * n[2]])?;
        let name = name.trim();
        (!name.is_empty() && origin.iter().all(|c| c.is_finite())).then(|| NamedPlane {
            id,
            name: name.to_string(),
            origin,
            u,
            n,
        })
    }
}

impl Model {
    /// Stored planes in the order they were saved; empty when the project has none.
    pub fn work_planes(&self) -> Result<Vec<NamedPlane>, Error> {
        let t: Option<String> = self
            .db
            .query_row(
                "SELECT CAST(value AS TEXT) FROM meta WHERE key=?1",
                [WORK_PLANES_KEY],
                |r| r.get(0),
            )
            .optional()?;
        t.map_or(Ok(vec![]), |t| serde_json::from_str(&t).map_err(bad))
    }

    /// Stores `p` under a new id, or replaces the plane of the same name (same id). One undo
    /// step; returns the id.
    pub fn save_work_plane(&mut self, p: &NamedPlane) -> Result<i64, Error> {
        let mut list = self.work_planes()?;
        let last = self.wp_last_id()?;
        let key = p.name.trim().to_lowercase();
        let id = match list.iter().position(|x| x.name.to_lowercase() == key) {
            Some(i) => {
                let id = list[i].id;
                list[i] = NamedPlane { id, ..p.clone() };
                id
            }
            None => {
                let id = list.iter().map(|x| x.id).max().unwrap_or(0).max(last) + 1;
                list.push(NamedPlane { id, ..p.clone() });
                id
            }
        };
        self.store_work_planes(&list, last.max(id))?;
        Ok(id)
    }

    /// Removes plane `id`; `false` when there is none (nothing written).
    pub fn delete_work_plane(&mut self, id: i64) -> Result<bool, Error> {
        let mut list = self.work_planes()?;
        let n = list.len();
        list.retain(|x| x.id != id);
        if list.len() == n {
            return Ok(false);
        }
        let last = self.wp_last_id()?.max(id);
        self.store_work_planes(&list, last)?;
        Ok(true)
    }

    /// Highest id ever given, so a deleted plane's id is not reused.
    fn wp_last_id(&self) -> Result<i64, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT value FROM meta WHERE key=?1",
                [WORK_PLANES_LAST],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    fn store_work_planes(&mut self, list: &[NamedPlane], last: i64) -> Result<(), Error> {
        let text = serde_json::to_string(list).map_err(bad)?;
        let keys = [
            Value::Text(WORK_PLANES_KEY.into()),
            Value::Text(WORK_PLANES_LAST.into()),
        ];
        self.op("work_planes", |db| {
            // undo: drop what this step wrote, then put the old rows back
            let inv = Inv::default()
                .sql("DELETE FROM meta WHERE key IN (?1, ?2)", keys.to_vec())
                .restore_where(db, "meta", "key IN (?1, ?2)", &keys)?;
            db.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![WORK_PLANES_KEY, text],
            )?;
            db.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![WORK_PLANES_LAST, last],
            )?;
            Ok(((), 0, inv))
        })
    }
}

/// `meta` key of the highest plane id given so far.
const WORK_PLANES_LAST: &str = "work_planes_last";

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
            .unwrap()
    }

    fn plane(name: &str, z: f64) -> NamedPlane {
        NamedPlane::new(0, name, [1.0, 2.0, z], [1.0, 0.0, 0.0], [0.0, -1.0, 1.0]).unwrap()
    }

    #[test]
    fn new_makes_the_frame_orthonormal_and_refuses_degenerate_planes() {
        let p = NamedPlane::new(0, " Roof ", [0.0; 3], [1.0, 0.0, 1.0], [0.0, 0.0, 2.0]).unwrap();
        assert_eq!(p.name, "Roof");
        assert_eq!(p.n, [0.0, 0.0, 1.0]);
        assert!((p.u[0] - 1.0).abs() < 1e-12 && p.u[2].abs() < 1e-12, "{:?}", p.u);
        assert!(NamedPlane::new(0, "a", [0.0; 3], [1.0, 0.0, 0.0], [0.0; 3]).is_none());
        assert!(NamedPlane::new(0, "a", [0.0; 3], [0.0, 0.0, 3.0], [0.0, 0.0, 1.0]).is_none());
        assert!(NamedPlane::new(0, "  ", [0.0; 3], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]).is_none());
        assert!(NamedPlane::new(0, "a", [f64::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0])
            .is_none());
    }

    #[test]
    fn saves_replaces_by_name_deletes_and_each_change_is_one_undo_step() {
        let mut m = Model::open(":memory:").unwrap();
        assert!(m.work_planes().unwrap().is_empty());
        let n = ops(&m);
        let a = m.save_work_plane(&plane("Roof", 6.0)).unwrap();
        assert_eq!(ops(&m), n + 1, "one journal entry");
        let b = m.save_work_plane(&plane("Ramp", 1.0)).unwrap();
        assert_ne!(a, b);
        // same name, any case: replaced in place, same id
        assert_eq!(m.save_work_plane(&plane("roof", 7.5)).unwrap(), a);
        let l = m.work_planes().unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].id, l[0].origin[2], l[0].name.as_str()), (a, 7.5, "roof"));
        m.undo().unwrap();
        assert_eq!(m.work_planes().unwrap()[0].origin[2], 6.0);
        assert!(m.delete_work_plane(b).unwrap());
        assert!(!m.delete_work_plane(b).unwrap(), "nothing left to delete");
        // a deleted id is never given again
        let c = m.save_work_plane(&plane("Canopy", 3.0)).unwrap();
        assert!(c > b, "{c} after {b}");
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.work_planes().unwrap().len(), 2, "delete undone");
        m.undo().unwrap();
        m.undo().unwrap();
        assert!(m.work_planes().unwrap().is_empty(), "first save undone: no rows left");
    }
}
