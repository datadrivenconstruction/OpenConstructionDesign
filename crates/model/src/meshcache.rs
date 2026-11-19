//! Derived cache of imported meshes (schema 24): `mesh_proto` holds one local shape per set
//! of identical `mesh` rows (copies of one type placed by shift and turn about Z) with up to
//! two coarser levels of detail; `mesh_geo` holds, per `mesh` row, its prototype, placement
//! and world box (proto NULL: empty or non-finite mesh). `mesh` stays the source of truth.
//! [`build_mesh_cache`] is resumable: each batch is computed outside a transaction and written
//! in a short one, so a cancel keeps every finished row and the UI connection is not blocked.
//! Triggers drop a row whenever its mesh changes or goes away (edits, undo); undo writes rows
//! back with `INSERT OR REPLACE`, which fires only the INSERT trigger, so that one exists too.
//! A row is written only if its mesh still holds the blobs it was computed from (checked in
//! the write transaction), so an edit between read and write cannot leave a stale row. The cache is not in the op journal, like the other derived tables.

use std::collections::HashMap;

use ddc_geom::instance::{canonical, same_shape, shape_tol, Place};
use ddc_geom::mesh::Mesh;
use ddc_geom::simplify::lods;
use ddc_geom::{v3, V3};
use rusqlite::{params, Connection, OptionalExtension};

use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS mesh_proto(id INTEGER PRIMARY KEY, hash INTEGER NOT NULL,
    verts INTEGER NOT NULL, tris INTEGER NOT NULL, radius REAL NOT NULL,
    positions BLOB NOT NULL, indices BLOB NOT NULL,
    lod1_pos BLOB, lod1_idx BLOB, lod1_err REAL, lod2_pos BLOB, lod2_idx BLOB, lod2_err REAL);
CREATE INDEX IF NOT EXISTS mesh_proto_hash ON mesh_proto(hash);
CREATE TABLE IF NOT EXISTS mesh_geo(mesh INTEGER PRIMARY KEY, proto INTEGER,
    c REAL NOT NULL, s REAL NOT NULL, tx REAL NOT NULL, ty REAL NOT NULL, tz REAL NOT NULL,
    minx REAL NOT NULL, miny REAL NOT NULL, minz REAL NOT NULL,
    maxx REAL NOT NULL, maxy REAL NOT NULL, maxz REAL NOT NULL);
CREATE INDEX IF NOT EXISTS mesh_geo_proto ON mesh_geo(proto);
CREATE TRIGGER IF NOT EXISTS mesh_geo_stale AFTER UPDATE OF positions, indices ON mesh
BEGIN DELETE FROM mesh_geo WHERE mesh=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS mesh_geo_gone AFTER DELETE ON mesh
BEGIN DELETE FROM mesh_geo WHERE mesh=OLD.id; END;
CREATE TRIGGER IF NOT EXISTS mesh_geo_new AFTER INSERT ON mesh
BEGIN DELETE FROM mesh_geo WHERE mesh=NEW.id; END;
";

/// Tables and triggers of step 24 (additive, idempotent; `mesh` exists since step 6).
pub(crate) fn ensure(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// Meshes per written batch.
const BATCH: usize = 256;
/// Verified shapes kept in memory while building.
const SHAPES: usize = 4096;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CacheStats {
    /// `mesh` rows placed in this run, of them placed on an existing shape.
    pub meshes: u64,
    pub reused: u64,
    /// New prototypes and rows without usable geometry.
    pub protos: u64,
    pub skipped: u64,
    pub tris_world: u64,
    pub tris_proto: u64,
}

/// Placement of one `mesh` row.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub mesh: i64,
    pub proto: i64,
    pub level: Option<i64>,
    pub category: String,
    pub place: Place,
    pub lo: [f64; 3],
    pub hi: [f64; 3],
}

/// Prototype geometry at one level of detail (local coordinates, f32).
#[derive(Debug, Clone, PartialEq)]
pub struct Proto {
    pub id: i64,
    pub lod: usize,
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

/// Prototype facts without geometry: error bound and triangles per level.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtoInfo {
    pub id: i64,
    pub radius: f64,
    pub errors: Vec<f64>,
    pub tris: Vec<u64>,
}

fn decode_world(pb: &[u8], ib: &[u8]) -> (Vec<V3>, Vec<[u32; 3]>) {
    let f = |c: &[u8], k: usize| {
        let mut b = [0u8; 8];
        b.copy_from_slice(&c[k * 8..k * 8 + 8]);
        f64::from_le_bytes(b)
    };
    let pos: Vec<V3> = pb
        .chunks_exact(24)
        .map(|c| v3(f(c, 0), f(c, 1), f(c, 2)))
        .collect();
    let n = pos.len() as u32;
    let tris = ib
        .chunks_exact(12)
        .map(|c| {
            let g = |k: usize| u32::from_le_bytes([c[k], c[k + 1], c[k + 2], c[k + 3]]);
            [g(0), g(4), g(8)]
        })
        .filter(|t| t.iter().all(|i| *i < n))
        .collect();
    (pos, tris)
}

fn f32_blob(p: &[[f32; 3]]) -> Vec<u8> {
    p.iter().flatten().flat_map(|x| x.to_le_bytes()).collect()
}

fn idx_blob(t: &[[u32; 3]]) -> Vec<u8> {
    t.iter().flatten().flat_map(|x| x.to_le_bytes()).collect()
}

pub(crate) fn f32_of(b: &[u8]) -> Vec<[f32; 3]> {
    b.chunks_exact(12)
        .map(|c| {
            let g = |k: usize| f32::from_le_bytes([c[k], c[k + 1], c[k + 2], c[k + 3]]);
            [g(0), g(4), g(8)]
        })
        .collect()
}

pub(crate) fn idx_of(b: &[u8], n: usize) -> Vec<[u32; 3]> {
    b.chunks_exact(12)
        .map(|c| {
            let g = |k: usize| u32::from_le_bytes([c[k], c[k + 1], c[k + 2], c[k + 3]]);
            [g(0), g(4), g(8)]
        })
        .filter(|t| t.iter().all(|i| (*i as usize) < n))
        .collect()
}

fn mesh_f32(m: &Mesh) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    (
        m.positions
            .iter()
            .map(|p| [p.x as f32, p.y as f32, p.z as f32])
            .collect(),
        m.triangles.clone(),
    )
}

/// Prototype to be inserted: `key` is its index in the batch until it has a row id.
struct NewProto {
    hash: u64,
    radius: f64,
    levels: Vec<(Vec<[f32; 3]>, Vec<[u32; 3]>)>,
    errors: Vec<f64>,
}

enum Ref {
    Old(i64),
    New(usize),
    None,
}

struct GeoRow {
    mesh: i64,
    proto: Ref,
    place: Place,
    lo: [f64; 3],
    hi: [f64; 3],
    /// Blobs the row was computed from.
    src: (Vec<u8>, Vec<u8>),
}

struct Builder<'a> {
    db: &'a Connection,
    by_hash: HashMap<u64, Vec<i64>>,
    shapes: HashMap<i64, (Vec<[f32; 3]>, Vec<[u32; 3]>)>,
}

impl Builder<'_> {
    fn shape(&mut self, id: i64) -> Result<Option<&(Vec<[f32; 3]>, Vec<[u32; 3]>)>, Error> {
        if !self.shapes.contains_key(&id) {
            if self.shapes.len() >= SHAPES {
                self.shapes.clear();
            }
            let row: Option<(Vec<u8>, Vec<u8>)> = self
                .db
                .prepare_cached("SELECT positions, indices FROM mesh_proto WHERE id=?1")?
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            let Some((pb, ib)) = row else {
                return Ok(None);
            };
            let p = f32_of(&pb);
            let t = idx_of(&ib, p.len());
            self.shapes.insert(id, (p, t));
        }
        Ok(self.shapes.get(&id))
    }

    fn batch(
        &mut self,
        ids: &[i64],
        st: &mut CacheStats,
    ) -> Result<(Vec<NewProto>, Vec<GeoRow>), Error> {
        let mut news: Vec<NewProto> = vec![];
        let mut new_shapes: Vec<(Vec<[f32; 3]>, Vec<[u32; 3]>)> = vec![];
        let mut rows = vec![];
        for &id in ids {
            let blobs: Option<(Vec<u8>, Vec<u8>)> = self
                .db
                .prepare_cached("SELECT positions, indices FROM mesh WHERE id=?1")?
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            let Some((pb, ib)) = blobs else { continue };
            let (pos, tris) = decode_world(&pb, &ib);
            let src = (pb, ib);
            let mut lo = [f64::MAX; 3];
            let mut hi = [f64::MIN; 3];
            for p in &pos {
                for (k, x) in [p.x, p.y, p.z].into_iter().enumerate() {
                    if x.is_finite() {
                        lo[k] = lo[k].min(x);
                        hi[k] = hi[k].max(x);
                    }
                }
            }
            let canon = if tris.is_empty() {
                None
            } else {
                canonical(&pos, &tris)
            };
            let Some(cn) = canon else {
                st.skipped += 1;
                let (lo, hi) = if lo[0] <= hi[0] {
                    (lo, hi)
                } else {
                    ([0.0; 3], [0.0; 3])
                };
                rows.push(GeoRow {
                    mesh: id,
                    proto: Ref::None,
                    place: Place::IDENTITY,
                    lo,
                    hi,
                    src,
                });
                continue;
            };
            let tol = shape_tol(cn.radius);
            let mut found = Ref::None;
            let olds = self.by_hash.get(&cn.hash).cloned().unwrap_or_default();
            for pid in olds {
                if let Some(s) = self.shape(pid)? {
                    if same_shape(&cn.local, &tris, &s.0, &s.1, tol) {
                        found = Ref::Old(pid);
                        break;
                    }
                }
            }
            if matches!(found, Ref::None) {
                if let Some(k) = news.iter().enumerate().position(|(k, n)| {
                    n.hash == cn.hash
                        && same_shape(&cn.local, &tris, &new_shapes[k].0, &new_shapes[k].1, tol)
                }) {
                    found = Ref::New(k);
                }
            }
            if matches!(found, Ref::None) {
                let local = Mesh {
                    positions: cn.local.iter().map(|p| v3(p[0], p[1], p[2])).collect(),
                    triangles: tris.clone(),
                    face_of: vec![],
                };
                let l = lods(&local);
                let levels: Vec<_> = l.levels.iter().map(mesh_f32).collect();
                new_shapes.push(levels[0].clone());
                st.protos += 1;
                st.tris_proto += tris.len() as u64;
                news.push(NewProto {
                    hash: cn.hash,
                    radius: cn.radius,
                    levels,
                    errors: l.errors,
                });
                found = Ref::New(news.len() - 1);
            } else {
                st.reused += 1;
            }
            st.meshes += 1;
            st.tris_world += tris.len() as u64;
            rows.push(GeoRow {
                mesh: id,
                proto: found,
                place: cn.place,
                lo,
                hi,
                src,
            });
        }
        Ok((news, rows))
    }

    fn write(&mut self, news: Vec<NewProto>, rows: Vec<GeoRow>) -> Result<(), Error> {
        let tx = self.db.unchecked_transaction()?;
        let mut ids = Vec::with_capacity(news.len());
        for n in news {
            let lv = |k: usize| n.levels.get(k).map(|l| (f32_blob(&l.0), idx_blob(&l.1)));
            let err = |k: usize| n.errors.get(k).copied();
            let (l1, l2) = (lv(1), lv(2));
            tx.prepare_cached(
                "INSERT INTO mesh_proto(hash, verts, tris, radius, positions, indices, \
                 lod1_pos, lod1_idx, lod1_err, lod2_pos, lod2_idx, lod2_err) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            )?
            .execute(params![
                n.hash as i64,
                n.levels[0].0.len() as i64,
                n.levels[0].1.len() as i64,
                n.radius,
                f32_blob(&n.levels[0].0),
                idx_blob(&n.levels[0].1),
                l1.as_ref().map(|l| &l.0),
                l1.as_ref().map(|l| &l.1),
                err(1),
                l2.as_ref().map(|l| &l.0),
                l2.as_ref().map(|l| &l.1),
                err(2),
            ])?;
            let id = tx.last_insert_rowid();
            self.by_hash.entry(n.hash).or_default().push(id);
            ids.push(id);
        }
        for r in rows {
            let proto = match r.proto {
                Ref::Old(id) => Some(id),
                Ref::New(k) => Some(ids[k]),
                Ref::None => None,
            };
            tx.prepare_cached(
                "INSERT OR REPLACE INTO mesh_geo(mesh, proto, c, s, tx, ty, tz, \
                 minx, miny, minz, maxx, maxy, maxz) \
                 SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13 WHERE EXISTS \
                 (SELECT 1 FROM mesh WHERE id=?1 AND positions=?14 AND indices=?15)",
            )?
            .execute(params![
                r.mesh,
                proto,
                r.place.cos,
                r.place.sin,
                r.place.t[0],
                r.place.t[1],
                r.place.t[2],
                r.lo[0],
                r.lo[1],
                r.lo[2],
                r.hi[0],
                r.hi[1],
                r.hi[2],
                r.src.0,
                r.src.1,
            ])?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// Places every `mesh` row without a cache row, lowest level first. `progress(done, total)`
/// returning false cancels after the current batch (finished batches stay).
pub fn build_mesh_cache(
    db: &Connection,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<CacheStats, Error> {
    ensure(db)?;
    let todo: Vec<i64> = {
        let mut st = db.prepare(
            "SELECT m.id FROM mesh m LEFT JOIN mesh_geo g ON g.mesh=m.id \
             LEFT JOIN level l ON l.id=m.level WHERE g.mesh IS NULL \
             ORDER BY l.elevation IS NULL, l.elevation, m.id",
        )?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        v
    };
    let mut by_hash: HashMap<u64, Vec<i64>> = HashMap::new();
    {
        let mut st = db.prepare("SELECT id, hash FROM mesh_proto ORDER BY id")?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (id, h) = r?;
            by_hash.entry(h as u64).or_default().push(id);
        }
    }
    let mut b = Builder {
        db,
        by_hash,
        shapes: HashMap::new(),
    };
    let mut st = CacheStats::default();
    let total = todo.len() as u64;
    let mut done = 0u64;
    for chunk in todo.chunks(BATCH) {
        if !progress(done, total) {
            return Err(Error::Invalid("cancelled".into()));
        }
        let (news, rows) = b.batch(chunk, &mut st)?;
        b.write(news, rows)?;
        done += chunk.len() as u64;
    }
    progress(done, total);
    // orphans in short statements: one big DELETE would hold the write lock for seconds
    for sql in [
        "DELETE FROM mesh_geo WHERE mesh IN (SELECT g.mesh FROM mesh_geo g \
         LEFT JOIN mesh m ON m.id=g.mesh WHERE m.id IS NULL LIMIT 4096)",
        "DELETE FROM mesh_proto WHERE id IN (SELECT p.id FROM mesh_proto p WHERE NOT EXISTS \
         (SELECT 1 FROM mesh_geo g WHERE g.proto=p.id) LIMIT 4096)",
    ] {
        while db.prepare_cached(sql)?.execute([])? > 0 {}
    }
    Ok(st)
}

/// [`build_mesh_cache`] on its own connection to a model file (a worker thread next to the UI
/// connection; WAL lets both work, writers wait for each other).
pub fn build_mesh_cache_file(
    path: &str,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<CacheStats, Error> {
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(10))?;
    build_mesh_cache(&db, progress)
}

fn placed(r: &rusqlite::Row) -> rusqlite::Result<Placed> {
    Ok(Placed {
        mesh: r.get(0)?,
        proto: r.get(1)?,
        level: r.get(2)?,
        category: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        place: Place {
            cos: r.get(4)?,
            sin: r.get(5)?,
            t: [r.get(6)?, r.get(7)?, r.get(8)?],
        },
        lo: [r.get(9)?, r.get(10)?, r.get(11)?],
        hi: [r.get(12)?, r.get(13)?, r.get(14)?],
    })
}

const PLACED: &str = "SELECT g.mesh, g.proto, m.level, m.category, g.c, g.s, g.tx, g.ty, g.tz, \
     g.minx, g.miny, g.minz, g.maxx, g.maxy, g.maxz FROM mesh_geo g JOIN mesh m ON m.id=g.mesh \
     LEFT JOIN level l ON l.id=m.level WHERE g.proto IS NOT NULL";

impl Model {
    /// [`build_mesh_cache`] on the model's own connection.
    pub fn mesh_cache_build(
        &self,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<CacheStats, Error> {
        build_mesh_cache(&self.db, progress)
    }

    /// `mesh` rows not yet in the cache.
    pub fn mesh_cache_missing(&self) -> Result<u64, Error> {
        Ok(self.db.query_row(
            "SELECT count(*) FROM mesh m WHERE NOT EXISTS (SELECT 1 FROM mesh_geo g WHERE g.mesh=m.id)",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Cached placements, `first` level first, then the others by distance in elevation
    /// (rows without a level last): the visible level can be drawn before the rest loads.
    pub fn mesh_placements(&self, first: Option<i64>) -> Result<Vec<Placed>, Error> {
        let sql = format!(
            "{PLACED} ORDER BY CASE WHEN m.level = ?1 THEN 0 ELSE 1 END, \
             m.level IS NULL, abs(coalesce(l.elevation, 0) - coalesce((SELECT elevation FROM level WHERE id=?1), 0)), g.mesh"
        );
        let mut st = self.db.prepare(&sql)?;
        let v = st.query_map([first], placed)?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Prototype at `lod` (0 full); a missing coarse level falls back to the next finer one.
    pub fn mesh_proto(&self, id: i64, lod: usize) -> Result<Proto, Error> {
        let row: (
            Vec<u8>,
            Vec<u8>,
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            Option<Vec<u8>>,
        ) = self.db.query_row(
            "SELECT positions, indices, \
                 CASE WHEN ?2 >= 1 THEN lod1_pos END, CASE WHEN ?2 >= 1 THEN lod1_idx END, \
                 CASE WHEN ?2 >= 2 THEN lod2_pos END, CASE WHEN ?2 >= 2 THEN lod2_idx END \
                 FROM mesh_proto WHERE id=?1",
            params![id, lod as i64],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )?;
        let (p, t, k) = match row {
            (_, _, _, _, Some(p), Some(t)) if lod >= 2 => (p, t, 2),
            (_, _, Some(p), Some(t), _, _) if lod >= 1 => (p, t, 1),
            (p, t, ..) => (p, t, 0),
        };
        let positions = f32_of(&p);
        let triangles = idx_of(&t, positions.len());
        Ok(Proto {
            id,
            lod: k,
            positions,
            triangles,
        })
    }

    pub fn mesh_proto_info(&self, id: i64) -> Result<ProtoInfo, Error> {
        let (radius, t0, e1, t1, e2, t2): (f64, i64, Option<f64>, Option<i64>, Option<f64>, Option<i64>) =
            self.db.query_row(
                "SELECT radius, tris, lod1_err, length(lod1_idx) / 12, lod2_err, length(lod2_idx) / 12 \
                 FROM mesh_proto WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )?;
        let mut errors = vec![0.0];
        let mut tris = vec![t0 as u64];
        if let (Some(e), Some(t)) = (e1, t1) {
            errors.push(e);
            tris.push(t as u64);
            if let (Some(e), Some(t)) = (e2, t2) {
                errors.push(e);
                tris.push(t as u64);
            }
        }
        Ok(ProtoInfo {
            id,
            radius,
            errors,
            tris,
        })
    }

    /// (cached meshes, prototypes, world triangles, prototype triangles).
    pub fn mesh_cache_totals(&self) -> Result<(u64, u64, u64, u64), Error> {
        Ok(self.db.query_row(
            "SELECT (SELECT count(*) FROM mesh_geo WHERE proto IS NOT NULL), \
             (SELECT count(*) FROM mesh_proto), \
             (SELECT coalesce(sum(p.tris), 0) FROM mesh_geo g JOIN mesh_proto p ON p.id=g.proto), \
             (SELECT coalesce(sum(tris), 0) FROM mesh_proto)",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                    r.get::<_, i64>(3)? as u64,
                ))
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blobs(p: &[[f64; 3]], t: &[[u32; 3]]) -> (Vec<u8>, Vec<u8>) {
        (
            p.iter().flatten().flat_map(|x| x.to_le_bytes()).collect(),
            t.iter().flatten().flat_map(|x| x.to_le_bytes()).collect(),
        )
    }

    /// Box 0.5×0.4×0.9 with a slanted top vertex (no symmetry), turned by `th`, shifted by `d`.
    fn chair(th: f64, d: [f64; 3]) -> (Vec<u8>, Vec<u8>) {
        let base = [
            [0.0, 0.0, 0.0],
            [0.5, 0.0, 0.0],
            [0.5, 0.4, 0.0],
            [0.0, 0.4, 0.0],
            [0.0, 0.0, 0.9],
            [0.5, 0.0, 0.45],
            [0.5, 0.4, 0.45],
            [0.0, 0.4, 0.9],
        ];
        let (c, s) = (th.cos(), th.sin());
        let p: Vec<[f64; 3]> = base
            .iter()
            .map(|q| {
                [
                    c * q[0] - s * q[1] + d[0],
                    s * q[0] + c * q[1] + d[1],
                    q[2] + d[2],
                ]
            })
            .collect();
        let t = [
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
        blobs(&p, &t)
    }

    fn add(m: &Model, level: Option<i64>, b: (Vec<u8>, Vec<u8>)) -> i64 {
        m.db.execute(
            "INSERT INTO mesh(level, category, name, positions, indices) VALUES (?1,'furniture','Chair',?2,?3)",
            params![level, b.0, b.1],
        )
        .unwrap();
        m.db.last_insert_rowid()
    }

    #[test]
    fn identical_copies_share_one_prototype() {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_level("L1", 0.0).unwrap();
        let l2 = m.add_level("L2", 3.0).unwrap();
        let mut ids = vec![];
        for k in 0..20 {
            let lv = if k % 2 == 0 { l1 } else { l2 };
            ids.push(add(
                &m,
                Some(lv),
                chair(
                    k as f64 * 0.37,
                    [k as f64 * 2.0, 5.0, if k % 2 == 0 { 0.0 } else { 3.0 }],
                ),
            ));
        }
        let other = add(&m, Some(l1), chair(0.0, [0.0, 0.0, 0.0]));
        // a different shape: same topology, one vertex moved
        m.db.execute(
            "UPDATE mesh SET positions=?2 WHERE id=?1",
            params![other, {
                let mut p = chair(0.0, [0.0, 0.0, 0.0]).0;
                p[4 * 24 + 16..4 * 24 + 24].copy_from_slice(&1.2f64.to_le_bytes());
                p
            }],
        )
        .unwrap();
        add(&m, None, (vec![], vec![]));
        let st = m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert_eq!(st.meshes, 21);
        assert_eq!(st.protos, 2);
        assert_eq!(st.reused, 19);
        assert_eq!(st.skipped, 1);
        assert_eq!(m.mesh_cache_missing().unwrap(), 0);
        let (n, protos, tw, tp) = m.mesh_cache_totals().unwrap();
        assert_eq!((n, protos, tw, tp), (21, 2, 21 * 12, 24));
        // placement rebuilds the world mesh
        let pl = m.mesh_placements(Some(l2)).unwrap();
        assert_eq!(pl.len(), 21);
        assert!(
            pl.iter().take(10).all(|p| p.level == Some(l2)),
            "visible level first"
        );
        let p = pl.iter().find(|p| p.mesh == ids[3]).unwrap();
        let proto = m.mesh_proto(p.proto, 0).unwrap();
        let (wb, _) = chair(3.0 * 0.37, [6.0, 5.0, 3.0]);
        for (k, q) in proto.positions.iter().enumerate() {
            let w = p.place.apply([q[0] as f64, q[1] as f64, q[2] as f64]);
            for a in 0..3 {
                let o = k * 24 + a * 8;
                let e = f64::from_le_bytes(wb[o..o + 8].try_into().unwrap());
                assert!((w[a] - e).abs() < 1e-5, "{k} {a}: {} vs {e}", w[a]);
            }
        }
        assert!(p.lo[0] <= p.hi[0] && p.lo[2] >= 3.0 - 1e-9);
        // nothing left to do; a second build is a no-op
        let again = m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert_eq!(again, CacheStats::default());
    }

    #[test]
    fn edits_and_undo_invalidate_rows_and_drop_orphans() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let a = add(&m, Some(l), chair(0.0, [0.0; 3]));
        let b = add(&m, Some(l), chair(1.0, [4.0, 0.0, 0.0]));
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert_eq!(m.mesh_cache_totals().unwrap().1, 1);
        m.db.execute(
            "UPDATE mesh SET positions=?2 WHERE id=?1",
            params![b, chair(1.0, [9.0, 0.0, 0.0]).0],
        )
        .unwrap();
        assert_eq!(m.mesh_cache_missing().unwrap(), 1, "moved mesh is stale");
        let st = m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert_eq!((st.meshes, st.reused, st.protos), (1, 1, 0));
        m.db.execute("DELETE FROM mesh WHERE id=?1", [a]).unwrap();
        m.db.execute("DELETE FROM mesh WHERE id=?1", [b]).unwrap();
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert_eq!(
            m.mesh_cache_totals().unwrap(),
            (0, 0, 0, 0),
            "orphan prototype removed"
        );
    }

    fn placed_x(m: &Model, id: i64) -> f64 {
        let p = m.mesh_placements(None).unwrap();
        p.iter().find(|p| p.mesh == id).map(|p| p.lo[0]).unwrap()
    }

    #[test]
    fn undo_of_a_mesh_move_invalidates_the_moved_row() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let a = add(&m, Some(l), chair(0.0, [0.0; 3]));
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert!(placed_x(&m, a).abs() < 1e-6);
        m.move_mesh(a, (5.0, 0.0, 0.0)).unwrap();
        assert_eq!(m.mesh_cache_missing().unwrap(), 1, "move is stale");
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert!((placed_x(&m, a) - 5.0).abs() < 1e-6);
        // undo restores the row by INSERT OR REPLACE: no UPDATE trigger fires
        m.undo().unwrap();
        assert_eq!(m.mesh_cache_missing().unwrap(), 1, "undo is stale too");
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert!(placed_x(&m, a).abs() < 1e-6, "back at the original place");
    }

    #[test]
    fn row_computed_before_an_edit_is_not_written() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let a = add(&m, Some(l), chair(0.0, [0.0; 3]));
        let mut b = Builder {
            db: &m.db,
            by_hash: HashMap::new(),
            shapes: HashMap::new(),
        };
        let mut st = CacheStats::default();
        let (news, rows) = b.batch(&[a], &mut st).unwrap();
        // the UI edits the mesh between the worker's read and its write
        m.db.execute(
            "UPDATE mesh SET positions=?2 WHERE id=?1",
            params![a, chair(0.0, [7.0, 0.0, 0.0]).0],
        )
        .unwrap();
        b.write(news, rows).unwrap();
        assert_eq!(m.mesh_cache_missing().unwrap(), 1, "stale row refused");
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert!((placed_x(&m, a) - 7.0).abs() < 1e-6);
    }

    #[test]
    fn cancel_keeps_finished_batches_only() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        for k in 0..(BATCH + 10) {
            add(&m, Some(l), chair(k as f64, [k as f64, 0.0, 0.0]));
        }
        let mut calls = 0;
        let e = m.mesh_cache_build(&mut |_, _| {
            calls += 1;
            calls < 2
        });
        assert!(e.is_err());
        assert_eq!(m.mesh_cache_missing().unwrap(), 10, "first batch kept");
        let st = m.mesh_cache_build(&mut |_, _| true).unwrap();
        assert_eq!(
            (st.meshes, st.protos),
            (10, 0),
            "resumed on the kept prototype"
        );
    }

    #[test]
    fn levels_of_detail_are_stored_and_fall_back() {
        let m = Model::open(":memory:").unwrap();
        // fine sphere-like cylinder: 64 segments × 16 rings, enough triangles for coarse levels
        let (nu, nv) = (64usize, 16usize);
        let mut p = vec![];
        for j in 0..=nv {
            for i in 0..nu {
                let a = std::f64::consts::TAU * i as f64 / nu as f64;
                let z = j as f64 / nv as f64;
                p.push([a.cos(), a.sin(), z]);
            }
        }
        let mut t = vec![];
        for j in 0..nv {
            for i in 0..nu {
                let q = |i: usize, j: usize| (j * nu + i % nu) as u32;
                t.push([q(i, j), q(i + 1, j), q(i + 1, j + 1)]);
                t.push([q(i, j), q(i + 1, j + 1), q(i, j + 1)]);
            }
        }
        add(&m, None, blobs(&p, &t));
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        let pl = m.mesh_placements(None).unwrap();
        let info = m.mesh_proto_info(pl[0].proto).unwrap();
        assert!(info.errors.len() >= 2, "{info:?}");
        assert!(info.tris.windows(2).all(|w| w[1] < w[0]));
        let full = m.mesh_proto(pl[0].proto, 0).unwrap();
        assert_eq!(full.triangles.len(), t.len());
        let coarse = m.mesh_proto(pl[0].proto, 9).unwrap();
        assert_eq!(coarse.lod, info.errors.len() - 1);
        assert_eq!(coarse.triangles.len() as u64, *info.tris.last().unwrap());
    }
}
