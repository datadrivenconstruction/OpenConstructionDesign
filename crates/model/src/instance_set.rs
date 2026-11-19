//! Cached placements and prototype geometry for the 3D scene, without per-row strings or boxes:
//! a million rows stay a few tens of megabytes. Reads through its own connection when the model
//! lives in a file, so a background thread does not touch the UI connection.

use ddc_geom::instance::Place;
use rusqlite::{Connection, Row};

use crate::meshcache::{f32_of, idx_of};
use crate::{Error, Model};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstRow {
    pub mesh: i64,
    pub proto: i64,
    pub level: Option<i64>,
    pub place: Place,
}

/// One level of detail; `err` is the surface deviation from the full mesh, m.
#[derive(Clone, Debug, PartialEq)]
pub struct InstLevel {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub err: f32,
}

/// Levels from fine to coarse, never empty.
#[derive(Clone, Debug, PartialEq)]
pub struct InstProto {
    pub id: i64,
    pub levels: Vec<InstLevel>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstanceSet {
    /// Sorted by (proto, mesh).
    pub rows: Vec<InstRow>,
    pub protos: Vec<InstProto>,
    /// Finer levels left out to stay inside the memory limit.
    pub dropped_levels: usize,
}

fn level_bytes(l: &InstLevel) -> u64 {
    (l.positions.len() as u64) * 12 + (l.triangles.len() as u64) * 12
}

fn level_of(p: &[u8], t: &[u8], err: f64) -> InstLevel {
    let positions = f32_of(p);
    let triangles = idx_of(t, positions.len());
    InstLevel {
        positions,
        triangles,
        err: err as f32,
    }
}

/// Row columns: id, positions, indices, lod1_pos, lod1_idx, lod1_err, lod2_pos, lod2_idx, lod2_err.
fn levels_of(r: &Row) -> rusqlite::Result<Vec<InstLevel>> {
    let (p0, t0): (Vec<u8>, Vec<u8>) = (r.get(1)?, r.get(2)?);
    let mut levels = vec![level_of(&p0, &t0, 0.0)];
    for k in [3usize, 6] {
        let (p, t, e): (Option<Vec<u8>>, Option<Vec<u8>>, Option<f64>) =
            (r.get(k)?, r.get(k + 1)?, r.get(k + 2)?);
        if let (Some(p), Some(t), Some(e)) = (p, t, e) {
            let l = level_of(&p, &t, e);
            if !l.triangles.is_empty() {
                levels.push(l);
            }
        }
    }
    Ok(levels)
}

/// Prototypes go from small to large: over `max_bytes` a prototype keeps only its coarsest level,
/// so the limit hits the few heavy shapes first and every placement still has geometry.
fn read(db: &Connection, max_bytes: u64) -> Result<InstanceSet, Error> {
    let mut st = db.prepare(
        "SELECT g.mesh, g.proto, m.level, g.c, g.s, g.tx, g.ty, g.tz \
         FROM mesh_geo g JOIN mesh m ON m.id=g.mesh \
         WHERE g.proto IS NOT NULL ORDER BY g.proto, g.mesh",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(InstRow {
                mesh: r.get(0)?,
                proto: r.get(1)?,
                level: r.get(2)?,
                place: Place {
                    cos: r.get(3)?,
                    sin: r.get(4)?,
                    t: [r.get(5)?, r.get(6)?, r.get(7)?],
                },
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut st = db.prepare(
        "SELECT id, positions, indices, lod1_pos, lod1_idx, lod1_err, lod2_pos, lod2_idx, lod2_err \
         FROM mesh_proto WHERE id IN (SELECT DISTINCT proto FROM mesh_geo WHERE proto IS NOT NULL) \
         ORDER BY tris, id",
    )?;
    let mut q = st.query([])?;
    let mut used = 0u64;
    let mut dropped_levels = 0;
    let mut protos = vec![];
    while let Some(r) = q.next()? {
        let mut levels = levels_of(r)?;
        let total: u64 = levels.iter().map(level_bytes).sum();
        if used + total > max_bytes && levels.len() > 1 {
            dropped_levels += levels.len() - 1;
            levels.drain(..levels.len() - 1);
        }
        used += levels.iter().map(level_bytes).sum::<u64>();
        protos.push(InstProto {
            id: r.get(0)?,
            levels,
        });
    }
    Ok(InstanceSet {
        rows,
        protos,
        dropped_levels,
    })
}

fn read_protos(db: &Connection, ids: &[i64]) -> Result<Vec<InstProto>, Error> {
    let mut st = db.prepare(
        "SELECT id, positions, indices, lod1_pos, lod1_idx, lod1_err, lod2_pos, lod2_idx, lod2_err \
         FROM mesh_proto WHERE id=?1",
    )?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let mut q = st.query([*id])?;
        if let Some(r) = q.next()? {
            out.push(InstProto {
                id: *id,
                levels: levels_of(r)?,
            });
        }
    }
    Ok(out)
}

/// All levels of the given prototypes: what [`read`] left out once a shape is seen up close.
pub fn read_proto_levels_file(path: &str, ids: &[i64]) -> Result<Vec<InstProto>, Error> {
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(10))?;
    read_protos(&db, ids)
}

/// [`Model::instance_set`] on a new connection to the model file (for a background thread).
pub fn read_instance_set_file(path: &str, max_bytes: u64) -> Result<InstanceSet, Error> {
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(10))?;
    read(&db, max_bytes)
}

impl Model {
    /// Everything the scene needs to draw the cached meshes as instances.
    pub fn instance_set(&self, max_bytes: u64) -> Result<InstanceSet, Error> {
        read(&self.db, max_bytes)
    }

    /// All levels of the given prototypes, see [`read_proto_levels_file`].
    pub fn proto_levels(&self, ids: &[i64]) -> Result<Vec<InstProto>, Error> {
        read_protos(&self.db, ids)
    }

    /// Meshes the cache can draw as instances; cheap enough for the UI thread.
    pub fn instanced_mesh_ids(&self) -> Result<Vec<i64>, Error> {
        let mut st = self
            .db
            .prepare("SELECT mesh FROM mesh_geo WHERE proto IS NOT NULL ORDER BY mesh")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    /// A closed box with a slanted top: no symmetry, so copies share one prototype.
    fn boxy(dx: f64) -> (Vec<u8>, Vec<u8>) {
        let p = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.5, 0.0],
            [0.0, 0.5, 0.0],
            [0.0, 0.0, 2.0],
            [1.0, 0.0, 1.0],
            [1.0, 0.5, 1.0],
            [0.0, 0.5, 2.0],
        ];
        let t: [[u32; 3]; 12] = [
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        (
            p.iter()
                .flat_map(|q: &[f64; 3]| [q[0] + dx, q[1], q[2]])
                .flat_map(|x| x.to_le_bytes())
                .collect(),
            t.iter().flatten().flat_map(|x| x.to_le_bytes()).collect(),
        )
    }

    fn model(n: usize) -> (Model, Vec<i64>, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let ids = (0..n)
            .map(|k| {
                let (p, t) = boxy(k as f64 * 3.0);
                m.db.execute(
                    "INSERT INTO mesh(level, category, name, positions, indices) VALUES (?1,'c','n',?2,?3)",
                    params![lv, p, t],
                )
                .unwrap();
                m.db.last_insert_rowid()
            })
            .collect();
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        (m, ids, lv)
    }

    #[test]
    fn rows_are_sorted_and_rebuild_the_world_mesh() {
        let (m, ids, lv) = model(5);
        let set = m.instance_set(u64::MAX).unwrap();
        assert_eq!(set.rows.len(), 5);
        assert_eq!(set.protos.len(), 1);
        assert_eq!(set.dropped_levels, 0);
        assert!(set
            .rows
            .windows(2)
            .all(|w| (w[0].proto, w[0].mesh) < (w[1].proto, w[1].mesh)));
        assert!(set.rows.iter().all(|r| r.level == Some(lv)));
        assert_eq!(m.instanced_mesh_ids().unwrap(), ids);
        let lod0 = &set.protos[0].levels[0];
        assert_eq!(lod0.triangles.len(), 12);
        assert_eq!(lod0.err, 0.0);
        // the placement carries the shift of the copy
        let at = |r: &InstRow| r.place.apply([0.0, 0.0, 0.0])[0];
        assert!((at(&set.rows[1]) - at(&set.rows[0]) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn a_changed_mesh_leaves_the_set() {
        let (m, ids, _) = model(3);
        let (p, _) = boxy(50.0);
        m.db.execute(
            "UPDATE mesh SET positions=?2 WHERE id=?1",
            params![ids[1], p],
        )
        .unwrap();
        let set = m.instance_set(u64::MAX).unwrap();
        assert_eq!(set.rows.len(), 2);
        assert!(set.rows.iter().all(|r| r.mesh != ids[1]));
        assert!(!m.instanced_mesh_ids().unwrap().contains(&ids[1]));
    }

    #[test]
    fn the_limit_keeps_every_prototype() {
        let (m, ..) = model(2);
        let set = m.instance_set(0).unwrap();
        assert_eq!(set.protos.len(), 1);
        assert_eq!(set.protos[0].levels.len(), 1, "coarsest level only");
        // the exact shape is still there for when the coarse one shows
        let id = set.protos[0].id;
        let exact = m.proto_levels(&[id, id + 1000]).unwrap();
        assert_eq!(exact.len(), 1, "an unknown id is skipped");
        assert_eq!(exact[0].levels[0].err, 0.0);
        assert_eq!(exact[0].levels[0].triangles.len(), 12);
    }

    #[test]
    fn file_reader_sees_the_same_rows() {
        let dir = std::env::temp_dir().join(format!("ddc_inst_set_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.ddc");
        let _ = std::fs::remove_file(&path);
        let s = path.to_string_lossy().to_string();
        {
            let mut m = Model::open(&s).unwrap();
            let lv = m.add_level("L1", 0.0).unwrap();
            let (p, t) = boxy(0.0);
            m.db.execute(
                "INSERT INTO mesh(level, category, name, positions, indices) VALUES (?1,'c','n',?2,?3)",
                params![lv, p, t],
            )
            .unwrap();
            m.mesh_cache_build(&mut |_, _| true).unwrap();
        }
        let set = read_instance_set_file(&s, u64::MAX).unwrap();
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.protos.len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
