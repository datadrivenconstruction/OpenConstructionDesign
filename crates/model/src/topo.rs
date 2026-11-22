// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Terrain (topo solid) and site properties. Source data is stored: survey points (with stable
//! numbers for editing), break lines, boundary, subregions, pads; the surface is built on read
//! (`topo_built`). Elevations are absolute, m. Each edit is one journal operation.

use ddc_geom::mesh::Mesh;
use ddc_geom::predicates::point_in_ring;
use ddc_geom::topo::{grade, smooth, Pad, Tin, TinInput};
use ddc_geom::{v2, v3, V2, V3};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::types::{Value, ValueRef};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS toposolid(id INTEGER PRIMARY KEY, name TEXT NOT NULL,
  thickness REAL NOT NULL, material TEXT NOT NULL, boundary TEXT NOT NULL DEFAULT '',
  breaklines TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS topo_point(topo INTEGER NOT NULL, idx INTEGER NOT NULL,
  x REAL NOT NULL, y REAL NOT NULL, z REAL NOT NULL, PRIMARY KEY(topo, idx));
CREATE TABLE IF NOT EXISTS topo_region(id INTEGER PRIMARY KEY, topo INTEGER NOT NULL,
  name TEXT NOT NULL, material TEXT NOT NULL, ring TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS topo_pad(id INTEGER PRIMARY KEY, topo INTEGER NOT NULL,
  name TEXT NOT NULL, elevation REAL NOT NULL, slope REAL NOT NULL, thickness REAL NOT NULL,
  material TEXT NOT NULL, outline TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS site_info(id INTEGER PRIMARY KEY CHECK(id = 1), name TEXT NOT NULL,
  north_deg REAL NOT NULL, base_x REAL NOT NULL, base_y REAL NOT NULL, base_z REAL NOT NULL,
  latitude REAL NOT NULL, longitude REAL NOT NULL,
  crs_code TEXT NOT NULL DEFAULT '', crs_name TEXT NOT NULL DEFAULT '',
  crs_vertical TEXT NOT NULL DEFAULT '');
";

/// Step 25: terrain and site tables (additive, safe to repeat). Step 49: the site coordinate
/// system `crs_*` (files from steps 25-46 get empty values).
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    for col in ["crs_code", "crs_name", "crs_vertical"] {
        let have: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('site_info') WHERE name=?1",
            [col],
            |r| r.get(0),
        )?;
        if have == 0 {
            db.execute_batch(&format!(
                "ALTER TABLE site_info ADD COLUMN {col} TEXT NOT NULL DEFAULT ''"
            ))?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopoSpec {
    pub name: String,
    /// Thickness of the solid below the surface, m.
    pub thickness: f64,
    pub material: String,
    pub points: Vec<(f64, f64, f64)>,
    pub breaklines: Vec<Vec<(f64, f64, f64)>>,
    /// Boundary in plan; empty means the convex hull of the points.
    pub boundary: Vec<(f64, f64)>,
}

/// Subregion (path, lawn): an outline on the surface with its own material.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopoRegion {
    pub id: i64,
    pub topo: i64,
    pub name: String,
    pub material: String,
    pub ring: Vec<(f64, f64)>,
}

/// Building pad: an outline at elevation `elevation`, embankment slopes with ratio `slope`
/// (m horizontal per 1 m vertical), a slab of thickness `thickness`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PadSpec {
    pub topo: i64,
    pub name: String,
    pub elevation: f64,
    pub slope: f64,
    pub thickness: f64,
    pub material: String,
    pub outline: Vec<(f64, f64)>,
}

/// Pad result: cut/fill, m³, outline area, m², zero-work line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PadReport {
    pub pad: i64,
    pub cut: f64,
    pub fill: f64,
    pub area: f64,
    pub daylight: Vec<Vec<(f64, f64)>>,
}

/// Site: true north is the angle from the project +Y, counter-clockwise, degrees; project base
/// point (east, north, elevation); latitude/longitude, degrees.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteProps {
    pub name: String,
    pub north_deg: f64,
    pub base: (f64, f64, f64),
    pub latitude: f64,
    pub longitude: f64,
    /// Coordinate system of the base point, `AUTHORITY:CODE` (for example `EPSG:25832`);
    /// empty: unknown, and no map conversion is written to IFC.
    pub crs_code: String,
    /// Readable name of the coordinate system (for example `ETRS89 / UTM zone 32N`).
    pub crs_name: String,
    /// Vertical datum (for example `EPSG:5783`); may be empty.
    pub crs_vertical: String,
}

/// `AUTHORITY:CODE`: letters, a colon, a code of letters, digits and `_ . -`; empty is valid.
pub fn crs_code_valid(code: &str) -> bool {
    if code.is_empty() {
        return true;
    }
    let Some((auth, id)) = code.split_once(':') else {
        return false;
    };
    code.len() <= 64
        && !auth.is_empty()
        && auth.chars().all(|c| c.is_ascii_alphabetic())
        && !id.is_empty()
        && id.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// Suggested coordinate system for a latitude/longitude: WGS 84 / UTM of the site zone. A hint
/// for the interface only; it reaches a file only when the user accepts it.
pub fn suggest_utm(latitude: f64, longitude: f64) -> Option<(String, String)> {
    let ok = latitude.is_finite()
        && longitude.is_finite()
        && latitude.abs() <= 84.0
        && longitude.abs() <= 180.0
        && (latitude != 0.0 || longitude != 0.0);
    if !ok {
        return None;
    }
    let zone = (((longitude + 180.0) / 6.0).floor() as i64 + 1).clamp(1, 60);
    let (hemi, base) = if latitude >= 0.0 { ('N', 32600) } else { ('S', 32700) };
    Some((
        format!("EPSG:{}", base + zone),
        format!("WGS 84 / UTM zone {zone}{hemi}"),
    ))
}

/// Built surface: the graded TIN, with the material and pad of each triangle.
#[derive(Clone, Debug)]
pub struct TopoBuilt {
    pub id: i64,
    pub tin: Tin,
    pub material: Vec<String>,
    /// Pad of a triangle (inside its outline), None is ground.
    pub pad_of: Vec<Option<i64>>,
    pub thickness: f64,
    pub pads: Vec<PadReport>,
    /// Slab thickness of each built pad.
    pub pad_thickness: Vec<(i64, f64)>,
    /// Pads and subregions skipped during the build (and why).
    pub warnings: Vec<String>,
}

impl TopoBuilt {
    /// Solid thickness: a pad slab or ground (`None`).
    pub fn piece_thickness(&self, pad: Option<i64>) -> f64 {
        match pad {
            Some(p) => self
                .pad_thickness
                .iter()
                .find(|q| q.0 == p)
                .map_or(0.0, |q| q.1),
            None => self.thickness,
        }
    }
}

fn pieces_of(b: &TopoBuilt) -> Vec<TopoPiece> {
    let mut groups: Vec<((Option<i64>, String), Vec<usize>)> = vec![];
    for (k, m) in b.material.iter().enumerate() {
        let key = (b.pad_of[k], m.clone());
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push(k),
            None => groups.push((key, vec![k])),
        }
    }
    groups
        .into_iter()
        .map(|((pad, material), tris)| {
            let th = b.piece_thickness(pad);
            let mesh = if th > 0.0 {
                b.tin.solid_part(&tris, th)
            } else {
                b.tin.top_mesh(&tris)
            };
            TopoPiece {
                material,
                pad,
                mesh,
            }
        })
        .collect()
}

struct CacheEntry {
    id: i64,
    key: u64,
    built: Result<Arc<TopoBuilt>, String>,
    pieces: Option<Arc<Vec<TopoPiece>>>,
}

/// The most recently built surfaces (shared by all models in the process; the key is the content).
static CACHE: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());
const CACHE_CAP: usize = 8;

fn cache_lock() -> MutexGuard<'static, Vec<CacheEntry>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

fn cache_find<T>(
    id: i64,
    key: u64,
    f: impl FnOnce(&CacheEntry) -> Option<Result<T, String>>,
) -> Option<Result<T, String>> {
    let mut c = cache_lock();
    let i = c.iter().position(|e| e.id == id && e.key == key)?;
    let e = c.remove(i);
    let r = f(&e);
    c.push(e);
    r
}

fn cache_put(id: i64, key: u64, built: Result<Arc<TopoBuilt>, String>) {
    let mut c = cache_lock();
    c.retain(|e| !(e.id == id && e.key == key));
    c.push(CacheEntry {
        id,
        key,
        built,
        pieces: None,
    });
    while c.len() > CACHE_CAP {
        c.remove(0);
    }
}

fn cache_with(id: i64, key: u64, f: impl FnOnce(&mut CacheEntry)) {
    if let Some(e) = cache_lock().iter_mut().find(|e| e.id == id && e.key == key) {
        f(e);
    }
}

/// Hash of the terrain source rows (solid, points, pads, subregions): the cache version.
fn source_key(db: &Connection, id: i64) -> Result<u64, Error> {
    let mut h = DefaultHasher::new();
    for (tag, sql) in [
        "SELECT * FROM toposolid WHERE id=?1",
        "SELECT * FROM topo_point WHERE topo=?1 ORDER BY idx",
        "SELECT * FROM topo_pad WHERE topo=?1 ORDER BY id",
        "SELECT * FROM topo_region WHERE topo=?1 ORDER BY id",
    ]
    .into_iter()
    .enumerate()
    {
        tag.hash(&mut h);
        let mut st = db.prepare_cached(sql)?;
        let n = st.column_count();
        let mut rows = st.query([id])?;
        while let Some(r) = rows.next()? {
            for i in 0..n {
                match r.get_ref(i)? {
                    ValueRef::Null => 0u8.hash(&mut h),
                    ValueRef::Integer(v) => (1u8, v).hash(&mut h),
                    ValueRef::Real(v) => (2u8, v.to_bits()).hash(&mut h),
                    ValueRef::Text(t) | ValueRef::Blob(t) => (3u8, t).hash(&mut h),
                }
            }
        }
    }
    Ok(h.finish())
}

/// A closed part of the solid: ground of one material or a pad slab.
#[derive(Clone, Debug)]
pub struct TopoPiece {
    pub material: String,
    pub pad: Option<i64>,
    pub mesh: Mesh,
}

fn geom(e: impl std::fmt::Display) -> Error {
    Error::Geom(e.to_string())
}

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn ring_text(r: &[(f64, f64)]) -> String {
    r.iter()
        .map(|(x, y)| format!("{x} {y}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn parse_ring(s: &str) -> Vec<(f64, f64)> {
    s.split(';')
        .filter_map(|p| {
            let mut it = p.split_whitespace().map(|v| v.parse::<f64>().ok());
            Some((it.next()??, it.next()??))
        })
        .collect()
}

fn lines_text(ls: &[Vec<(f64, f64, f64)>]) -> String {
    ls.iter()
        .filter(|l| l.len() >= 2)
        .map(|l| {
            l.iter()
                .map(|(x, y, z)| format!("{x} {y} {z}"))
                .collect::<Vec<_>>()
                .join(";")
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn parse_lines(s: &str) -> Vec<Vec<(f64, f64, f64)>> {
    s.split('|')
        .map(|l| {
            l.split(';')
                .filter_map(|p| {
                    let mut it = p.split_whitespace().map(|v| v.parse::<f64>().ok());
                    Some((it.next()??, it.next()??, it.next()??))
                })
                .collect::<Vec<_>>()
        })
        .filter(|l| l.len() >= 2)
        .collect()
}

fn ring2(r: &[(f64, f64)]) -> Vec<V2> {
    r.iter().map(|&(x, y)| v2(x, y)).collect()
}

fn area(r: &[(f64, f64)]) -> f64 {
    let n = r.len();
    ((0..n)
        .map(|i| {
            let (p, q) = (r[i], r[(i + 1) % n]);
            p.0 * q.1 - q.0 * p.1
        })
        .sum::<f64>()
        * 0.5)
        .abs()
}

fn fin(v: f64) -> bool {
    v.is_finite() && v.abs() < 1e9
}

fn check_ring(r: &[(f64, f64)], what: &str) -> Result<(), Error> {
    if r.len() < 3 || r.iter().any(|p| !fin(p.0) || !fin(p.1)) || area(r) < 1e-9 {
        return invalid(format!(
            "{what}: need a closed outline of at least 3 points"
        ));
    }
    Ok(())
}

fn check_spec(s: &TopoSpec) -> Result<(), Error> {
    if !(s.thickness > 0.0) || !fin(s.thickness) {
        return invalid("terrain thickness must be positive");
    }
    let bad = |p: &(f64, f64, f64)| !fin(p.0) || !fin(p.1) || !fin(p.2);
    if s.points.iter().any(bad) || s.breaklines.iter().flatten().any(bad) {
        return invalid("terrain point is not finite");
    }
    if !s.boundary.is_empty() {
        check_ring(&s.boundary, "terrain boundary")?;
    }
    surface(s).map(|_| ())
}

fn check_pad(p: &PadSpec) -> Result<(), Error> {
    check_ring(&p.outline, "pad")?;
    if !fin(p.elevation) || !(p.slope >= 0.0) || !fin(p.slope) {
        return invalid("pad elevation/slope out of range");
    }
    if !(p.thickness >= 0.0) || !fin(p.thickness) {
        return invalid("pad thickness must not be negative");
    }
    Ok(())
}

/// The source surface (without pads and subregions).
fn surface(s: &TopoSpec) -> Result<Tin, Error> {
    let pts: Vec<V3> = s.points.iter().map(|&(x, y, z)| v3(x, y, z)).collect();
    let bl: Vec<Vec<V3>> = s
        .breaklines
        .iter()
        .map(|l| l.iter().map(|&(x, y, z)| v3(x, y, z)).collect())
        .collect();
    let b = ring2(&s.boundary);
    Tin::build(&TinInput {
        points: &pts,
        breaklines: &bl,
        boundary: (b.len() >= 3).then_some(b.as_slice()),
        regions: &[],
    })
    .map_err(geom)
}

fn exists(db: &Connection, id: i64) -> Result<(), Error> {
    let n: Option<i64> = db
        .query_row("SELECT id FROM toposolid WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    match n {
        Some(_) => Ok(()),
        None => invalid(format!("no such terrain {id}")),
    }
}

fn load_points(db: &Connection, id: i64) -> Result<Vec<(i64, (f64, f64, f64))>, Error> {
    let mut st =
        db.prepare_cached("SELECT idx, x, y, z FROM topo_point WHERE topo=?1 ORDER BY idx")?;
    let rows = st.query_map([id], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn write_points(db: &Connection, id: i64, from: i64, pts: &[(f64, f64, f64)]) -> Result<(), Error> {
    let mut st =
        db.prepare_cached("INSERT INTO topo_point(topo, idx, x, y, z) VALUES (?1,?2,?3,?4,?5)")?;
    for (k, p) in pts.iter().enumerate() {
        st.execute(params![id, from + k as i64, p.0, p.1, p.2])?;
    }
    Ok(())
}

/// Inverse command: restore a point's elevation/position.
fn point_back(id: i64, idx: i64, p: (f64, f64, f64)) -> (String, Vec<Value>) {
    (
        "UPDATE topo_point SET x=?1, y=?2, z=?3 WHERE topo=?4 AND idx=?5".into(),
        vec![
            Value::Real(p.0),
            Value::Real(p.1),
            Value::Real(p.2),
            Value::Integer(id),
            Value::Integer(idx),
        ],
    )
}

/// Vertical shift of a whole terrain: points, break lines and pad levels move by `dz`. The caller
/// snapshots the rows first, so one undo restores them.
pub(crate) fn lift_in(db: &Connection, id: i64, dz: f64) -> Result<(), Error> {
    exists(db, id)?;
    db.execute(
        "UPDATE topo_point SET z=z+?1 WHERE topo=?2",
        params![dz, id],
    )?;
    let lines: String = db.query_row("SELECT breaklines FROM toposolid WHERE id=?1", [id], |r| {
        r.get(0)
    })?;
    let lines: Vec<Vec<(f64, f64, f64)>> = parse_lines(&lines)
        .into_iter()
        .map(|l| l.into_iter().map(|(x, y, z)| (x, y, z + dz)).collect())
        .collect();
    db.execute(
        "UPDATE toposolid SET breaklines=?1 WHERE id=?2",
        params![lines_text(&lines), id],
    )?;
    db.execute(
        "UPDATE topo_pad SET elevation=elevation+?1 WHERE topo=?2",
        params![dz, id],
    )?;
    Ok(())
}

/// Plan transform of a whole terrain (move, rotate, mirror): every stored plan coordinate goes
/// through `f`, elevations stay, rings keep their orientation under a mirror. The caller
/// snapshots the rows first, so one undo restores them.
pub(crate) fn transform_in(
    db: &Connection,
    id: i64,
    f: &dyn Fn((f64, f64)) -> (f64, f64),
    mirrored: bool,
) -> Result<(), Error> {
    exists(db, id)?;
    let ring = |r: &str| -> String {
        let mut v: Vec<(f64, f64)> = parse_ring(r).into_iter().map(f).collect();
        if mirrored {
            v.reverse();
        }
        ring_text(&v)
    };
    for (idx, (x, y, _)) in load_points(db, id)? {
        let q = f((x, y));
        db.execute(
            "UPDATE topo_point SET x=?1, y=?2 WHERE topo=?3 AND idx=?4",
            params![q.0, q.1, id, idx],
        )?;
    }
    let (boundary, lines): (String, String) = db.query_row(
        "SELECT boundary, breaklines FROM toposolid WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let lines: Vec<Vec<(f64, f64, f64)>> = parse_lines(&lines)
        .into_iter()
        .map(|l| {
            l.into_iter()
                .map(|(x, y, z)| {
                    let q = f((x, y));
                    (q.0, q.1, z)
                })
                .collect()
        })
        .collect();
    db.execute(
        "UPDATE toposolid SET boundary=?1, breaklines=?2 WHERE id=?3",
        params![ring(&boundary), lines_text(&lines), id],
    )?;
    for (table, col) in [("topo_region", "ring"), ("topo_pad", "outline")] {
        let rows: Vec<(i64, String)> = db
            .prepare(&format!("SELECT id, {col} FROM {table} WHERE topo=?1"))?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (k, text) in rows {
            db.execute(
                &format!("UPDATE {table} SET {col}=?1 WHERE id=?2"),
                params![ring(&text), k],
            )?;
        }
    }
    Ok(())
}

/// All terrain rows (for deletion and full replacement).
fn snapshot(db: &Connection, id: i64) -> Result<Inv, Error> {
    let mut inv = Inv::default().restore(db, "toposolid", "id", id)?;
    for t in ["topo_point", "topo_region", "topo_pad"] {
        inv = inv.restore(db, t, "topo", id)?;
    }
    Ok(inv)
}

/// New elevations of the points in an outline: (idx, old, new) for the changed ones.
fn changes(
    pts: &[(i64, (f64, f64, f64))],
    znew: impl Fn(usize) -> Option<f64>,
) -> Vec<(i64, (f64, f64, f64), f64)> {
    pts.iter()
        .enumerate()
        .filter_map(|(k, &(idx, p))| {
            let z = znew(k)?;
            ((z - p.2).abs() > 1e-12).then_some((idx, p, z))
        })
        .collect()
}

impl Model {
    pub fn add_toposolid(&mut self, s: &TopoSpec) -> Result<i64, Error> {
        check_spec(s)?;
        self.op("topo_add", |db| {
            db.execute(
                "INSERT INTO toposolid(name, thickness, material, boundary, breaklines)
                 VALUES (?1,?2,?3,?4,?5)",
                params![
                    s.name,
                    s.thickness,
                    crate::material_look::assign("toposolid", &s.material, &s.name),
                    ring_text(&s.boundary),
                    lines_text(&s.breaklines)
                ],
            )?;
            let id = db.last_insert_rowid();
            write_points(db, id, 0, &s.points)?;
            let inv = Inv::default()
                .delete("toposolid", "id", id)
                .delete("topo_point", "topo", id);
            Ok((id, id, inv))
        })
    }

    /// Full replacement of the source data (points are renumbered); subregions and pads
    /// remain.
    pub fn update_toposolid(&mut self, id: i64, s: &TopoSpec) -> Result<(), Error> {
        check_spec(s)?;
        self.op("topo_edit", |db| {
            exists(db, id)?;
            let inv = Inv::default()
                .delete("topo_point", "topo", id)
                .then(Inv::default().restore(db, "toposolid", "id", id)?)
                .then(Inv::default().restore(db, "topo_point", "topo", id)?);
            db.execute(
                "UPDATE toposolid SET name=?1, thickness=?2, material=?3, boundary=?4, breaklines=?5
                 WHERE id=?6",
                params![
                    s.name,
                    s.thickness,
                    crate::material_look::assign("toposolid", &s.material, &s.name),
                    ring_text(&s.boundary),
                    lines_text(&s.breaklines),
                    id
                ],
            )?;
            db.execute("DELETE FROM topo_point WHERE topo=?1", [id])?;
            write_points(db, id, 0, &s.points)?;
            Ok(((), id, inv))
        })
    }

    pub fn set_topo_props(
        &mut self,
        id: i64,
        name: &str,
        thickness: f64,
        material: &str,
    ) -> Result<(), Error> {
        if !(thickness > 0.0) || !fin(thickness) {
            return invalid("terrain thickness must be positive");
        }
        self.op("topo_props", |db| {
            exists(db, id)?;
            let inv = Inv::default().restore(db, "toposolid", "id", id)?;
            db.execute(
                "UPDATE toposolid SET name=?1, thickness=?2, material=?3 WHERE id=?4",
                params![name, thickness, material, id],
            )?;
            Ok(((), id, inv))
        })
    }

    /// Deletes the terrain together with its points, subregions and pads.
    pub fn delete_toposolid(&mut self, id: i64) -> Result<(), Error> {
        self.op("topo_delete", |db| {
            exists(db, id)?;
            let inv = snapshot(db, id)?;
            db.execute("DELETE FROM toposolid WHERE id=?1", [id])?;
            for t in ["topo_point", "topo_region", "topo_pad"] {
                db.execute(&format!("DELETE FROM {t} WHERE topo=?1"), [id])?;
            }
            Ok(((), id, inv))
        })
    }

    /// Terrains: (id, name).
    pub fn toposolids(&self) -> Result<Vec<(i64, String)>, Error> {
        let mut st = self
            .db
            .prepare_cached("SELECT id, name FROM toposolid ORDER BY id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn toposolid(&self, id: i64) -> Result<TopoSpec, Error> {
        let row: Option<(String, f64, String, String, String)> = self
            .db
            .query_row(
                "SELECT name, thickness, material, boundary, breaklines FROM toposolid WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((name, thickness, material, boundary, lines)) = row else {
            return invalid(format!("no such terrain {id}"));
        };
        Ok(TopoSpec {
            name,
            thickness,
            material,
            points: load_points(&self.db, id)?
                .into_iter()
                .map(|p| p.1)
                .collect(),
            breaklines: parse_lines(&lines),
            boundary: parse_ring(&boundary),
        })
    }

    /// Survey points with stable numbers (deleting one does not renumber the others).
    pub fn topo_points(&self, id: i64) -> Result<Vec<(i64, (f64, f64, f64))>, Error> {
        load_points(&self.db, id)
    }

    pub fn add_topo_points(&mut self, id: i64, pts: &[(f64, f64, f64)]) -> Result<(), Error> {
        if pts.is_empty() {
            return Ok(());
        }
        if pts.iter().any(|p| !fin(p.0) || !fin(p.1) || !fin(p.2)) {
            return invalid("terrain point is not finite");
        }
        self.op("topo_points_add", |db| {
            exists(db, id)?;
            let from: i64 = db.query_row(
                "SELECT COALESCE(MAX(idx), -1) + 1 FROM topo_point WHERE topo=?1",
                [id],
                |r| r.get(0),
            )?;
            write_points(db, id, from, pts)?;
            let inv = Inv::default().sql(
                "DELETE FROM topo_point WHERE topo=?1 AND idx>=?2",
                vec![Value::Integer(id), Value::Integer(from)],
            );
            Ok(((), id, inv))
        })
    }

    /// Moves a point (in plan and in height).
    pub fn move_topo_point(&mut self, id: i64, idx: i64, p: (f64, f64, f64)) -> Result<(), Error> {
        if !fin(p.0) || !fin(p.1) || !fin(p.2) {
            return invalid("terrain point is not finite");
        }
        self.op("topo_point_move", |db| {
            let old: Option<(f64, f64, f64)> = db
                .query_row(
                    "SELECT x, y, z FROM topo_point WHERE topo=?1 AND idx=?2",
                    [id, idx],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some(old) = old else {
                return invalid(format!("no point {idx} in terrain {id}"));
            };
            db.execute(
                "UPDATE topo_point SET x=?1, y=?2, z=?3 WHERE topo=?4 AND idx=?5",
                params![p.0, p.1, p.2, id, idx],
            )?;
            let (sql, args) = point_back(id, idx, old);
            Ok(((), id, Inv::default().sql(&sql, args)))
        })
    }

    /// Moves several points in one operation (entering an elevation for the selected ones).
    pub fn move_topo_points(
        &mut self,
        id: i64,
        moves: &[(i64, (f64, f64, f64))],
    ) -> Result<(), Error> {
        if moves
            .iter()
            .any(|(_, p)| !fin(p.0) || !fin(p.1) || !fin(p.2))
        {
            return invalid("terrain point is not finite");
        }
        if moves.is_empty() {
            return Ok(());
        }
        self.op("topo_points_move", |db| {
            let old: std::collections::HashMap<i64, (f64, f64, f64)> =
                load_points(db, id)?.into_iter().collect();
            let mut inv = Inv::default();
            for &(idx, p) in moves {
                let Some(&o) = old.get(&idx) else {
                    return invalid(format!("no point {idx} in terrain {id}"));
                };
                db.execute(
                    "UPDATE topo_point SET x=?1, y=?2, z=?3 WHERE topo=?4 AND idx=?5",
                    params![p.0, p.1, p.2, id, idx],
                )?;
                let (sql, args) = point_back(id, idx, o);
                inv = inv.sql(&sql, args);
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_topo_points(&mut self, id: i64, idx: &[i64]) -> Result<(), Error> {
        if idx.is_empty() {
            return Ok(());
        }
        self.op("topo_points_delete", |db| {
            let pts = load_points(db, id)?;
            let gone: Vec<&(i64, (f64, f64, f64))> =
                pts.iter().filter(|p| idx.contains(&p.0)).collect();
            if pts.len() - gone.len() < 3 {
                return invalid("terrain needs at least 3 points");
            }
            let mut inv = Inv::default();
            for &&(i, p) in &gone {
                db.execute("DELETE FROM topo_point WHERE topo=?1 AND idx=?2", [id, i])?;
                inv = inv.sql(
                    "INSERT INTO topo_point(topo, idx, x, y, z) VALUES (?1,?2,?3,?4,?5)",
                    vec![
                        Value::Integer(id),
                        Value::Integer(i),
                        Value::Real(p.0),
                        Value::Real(p.1),
                        Value::Real(p.2),
                    ],
                );
            }
            Ok(((), id, inv))
        })
    }

    /// Terrain boundary; an empty outline means the convex hull.
    pub fn set_topo_boundary(&mut self, id: i64, ring: &[(f64, f64)]) -> Result<(), Error> {
        if !ring.is_empty() {
            check_ring(ring, "terrain boundary")?;
        }
        self.op("topo_boundary", |db| {
            exists(db, id)?;
            let inv = Inv::default().restore(db, "toposolid", "id", id)?;
            db.execute(
                "UPDATE toposolid SET boundary=?1 WHERE id=?2",
                params![ring_text(ring), id],
            )?;
            Ok(((), id, inv))
        })
    }

    /// Smooths the elevations of points inside an outline; returns the number of changed points.
    pub fn smooth_topo(
        &mut self,
        id: i64,
        ring: &[(f64, f64)],
        passes: u32,
    ) -> Result<usize, Error> {
        check_ring(ring, "smooth region")?;
        let spec = self.toposolid(id)?;
        let tin = surface(&spec)?;
        let z = smooth(&tin, &ring2(ring), passes.clamp(1, 100));
        let mut per_point: Vec<Option<f64>> = vec![None; spec.points.len()];
        for (v, &s) in tin.src.iter().enumerate() {
            if let Some(slot) = per_point.get_mut(s as usize) {
                *slot = Some(z[v]);
            }
        }
        self.set_z("topo_smooth", id, |k| per_point[k])
    }

    /// Levelling: points inside the outline (and on it) get elevation `z`.
    pub fn flatten_topo(&mut self, id: i64, ring: &[(f64, f64)], z: f64) -> Result<usize, Error> {
        check_ring(ring, "flatten region")?;
        if !fin(z) {
            return invalid("elevation is not finite");
        }
        let r = ring2(ring);
        let pts = self.topo_points(id)?;
        self.set_z("topo_flatten", id, |k| {
            let p = pts[k].1;
            (point_in_ring(v2(p.0, p.1), &r) >= 0).then_some(z)
        })
    }

    fn set_z(
        &mut self,
        kind: &str,
        id: i64,
        znew: impl Fn(usize) -> Option<f64>,
    ) -> Result<usize, Error> {
        let pts = self.topo_points(id)?;
        let ch = changes(&pts, znew);
        if ch.is_empty() {
            return Ok(0);
        }
        self.op(kind, |db| {
            let mut inv = Inv::default();
            let mut st =
                db.prepare_cached("UPDATE topo_point SET z=?1 WHERE topo=?2 AND idx=?3")?;
            for &(idx, old, z) in &ch {
                st.execute(params![z, id, idx])?;
                let (sql, args) = point_back(id, idx, old);
                inv = inv.sql(&sql, args);
            }
            Ok((ch.len(), id, inv))
        })
    }

    pub fn add_topo_region(
        &mut self,
        topo: i64,
        name: &str,
        material: &str,
        ring: &[(f64, f64)],
    ) -> Result<i64, Error> {
        check_ring(ring, "terrain region")?;
        self.op("topo_region_add", |db| {
            exists(db, topo)?;
            db.execute(
                "INSERT INTO topo_region(topo, name, material, ring) VALUES (?1,?2,?3,?4)",
                params![topo, name, material, ring_text(ring)],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, topo, Inv::default().delete("topo_region", "id", id)))
        })
    }

    pub fn delete_topo_region(&mut self, id: i64) -> Result<(), Error> {
        self.op("topo_region_delete", |db| {
            let topo: Option<i64> = db
                .query_row("SELECT topo FROM topo_region WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(topo) = topo else {
                return invalid(format!("no such terrain region {id}"));
            };
            let inv = Inv::default().restore(db, "topo_region", "id", id)?;
            db.execute("DELETE FROM topo_region WHERE id=?1", [id])?;
            Ok(((), topo, inv))
        })
    }

    pub fn topo_regions(&self, topo: i64) -> Result<Vec<TopoRegion>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT id, name, material, ring FROM topo_region WHERE topo=?1 ORDER BY id",
        )?;
        let rows = st.query_map([topo], |r| {
            Ok(TopoRegion {
                id: r.get(0)?,
                topo,
                name: r.get(1)?,
                material: r.get(2)?,
                ring: parse_ring(&r.get::<_, String>(3)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn add_pad(&mut self, p: &PadSpec) -> Result<i64, Error> {
        check_pad(p)?;
        self.op("topo_pad_add", |db| {
            exists(db, p.topo)?;
            db.execute(
                "INSERT INTO topo_pad(topo, name, elevation, slope, thickness, material, outline)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    p.topo,
                    p.name,
                    p.elevation,
                    p.slope,
                    p.thickness,
                    crate::material_look::assign("topo_pad", &p.material, &p.name),
                    ring_text(&p.outline)
                ],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, p.topo, Inv::default().delete("topo_pad", "id", id)))
        })
    }

    pub fn update_pad(&mut self, id: i64, p: &PadSpec) -> Result<(), Error> {
        check_pad(p)?;
        self.op("topo_pad_edit", |db| {
            exists(db, p.topo)?;
            let inv = Inv::default().restore(db, "topo_pad", "id", id)?;
            let n = db.execute(
                "UPDATE topo_pad SET topo=?1, name=?2, elevation=?3, slope=?4, thickness=?5,
                 material=?6, outline=?7 WHERE id=?8",
                params![
                    p.topo,
                    p.name,
                    p.elevation,
                    p.slope,
                    p.thickness,
                    crate::material_look::assign("topo_pad", &p.material, &p.name),
                    ring_text(&p.outline),
                    id
                ],
            )?;
            if n == 0 {
                return invalid(format!("no such pad {id}"));
            }
            Ok(((), p.topo, inv))
        })
    }

    pub fn delete_pad(&mut self, id: i64) -> Result<(), Error> {
        self.op("topo_pad_delete", |db| {
            let topo: Option<i64> = db
                .query_row("SELECT topo FROM topo_pad WHERE id=?1", [id], |r| r.get(0))
                .optional()?;
            let Some(topo) = topo else {
                return invalid(format!("no such pad {id}"));
            };
            let inv = Inv::default().restore(db, "topo_pad", "id", id)?;
            db.execute("DELETE FROM topo_pad WHERE id=?1", [id])?;
            Ok(((), topo, inv))
        })
    }

    pub fn pads(&self, topo: i64) -> Result<Vec<(i64, PadSpec)>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT id, name, elevation, slope, thickness, material, outline FROM topo_pad
             WHERE topo=?1 ORDER BY id",
        )?;
        let rows = st.query_map([topo], |r| {
            Ok((
                r.get(0)?,
                PadSpec {
                    topo,
                    name: r.get(1)?,
                    elevation: r.get(2)?,
                    slope: r.get(3)?,
                    thickness: r.get(4)?,
                    material: r.get(5)?,
                    outline: parse_ring(&r.get::<_, String>(6)?),
                },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Surface with pads (graded) and subregions; a pad report.
    /// A pad or subregion on which the build fails is skipped with a warning
    /// (`TopoBuilt::warnings`), and the rest is built.
    pub fn topo_built(&self, id: i64) -> Result<TopoBuilt, Error> {
        Ok((*self.topo_built_shared(id)?).clone())
    }

    /// Like `topo_built`, without a copy: a shared cache for plan, 3D, sections and IFC. The key is
    /// the hash of the terrain source rows, so any edit or undo triggers a new build.
    pub fn topo_built_shared(&self, id: i64) -> Result<Arc<TopoBuilt>, Error> {
        let key = source_key(&self.db, id)?;
        self.built_by_key(id, key)
    }

    fn built_by_key(&self, id: i64, key: u64) -> Result<Arc<TopoBuilt>, Error> {
        if let Some(hit) = cache_find(id, key, |e| Some(e.built.clone())) {
            return hit.map_err(Error::Geom);
        }
        let stored = match self.build_topo(id) {
            Ok(b) => Ok(Arc::new(b)),
            // a build error would repeat on every frame, so it is cached the same way
            Err(Error::Geom(m)) => Err(m),
            Err(e) => return Err(e),
        };
        cache_put(id, key, stored.clone());
        stored.map_err(Error::Geom)
    }

    fn build_topo(&self, id: i64) -> Result<TopoBuilt, Error> {
        let spec = self.toposolid(id)?;
        let base = surface(&spec)?;
        let mut warnings = vec![];
        let all_pads = self.pads(id)?;
        let all_gp: Vec<Pad> = all_pads
            .iter()
            .map(|(_, p)| Pad {
                outline: ring2(&p.outline),
                z: p.elevation,
                slope: p.slope,
            })
            .collect();
        let (g, pads, gp) = match grade(&base, &all_gp) {
            Ok(g) => (g, all_pads, all_gp),
            Err(_) => {
                // one at a time: an incompatible pad does not suppress the others
                let (mut pads, mut gp) = (vec![], vec![]);
                let mut last = None;
                for (k, (pid, p)) in all_pads.into_iter().enumerate() {
                    gp.push(all_gp[k].clone());
                    match grade(&base, &gp) {
                        Ok(g) => {
                            last = Some(g);
                            pads.push((pid, p));
                        }
                        Err(e) => {
                            gp.pop();
                            warnings.push(format!("pad {pid} skipped: {e}"));
                        }
                    }
                }
                let g = match last {
                    Some(g) => g,
                    None => grade(&base, &[]).map_err(geom)?,
                };
                (g, pads, gp)
            }
        };
        let all_regions = self.topo_regions(id)?;
        let all_rings: Vec<Vec<V2>> = all_regions.iter().map(|r| ring2(&r.ring)).collect();
        let (tin, regions) = if all_rings.is_empty() {
            (g.tin, all_regions)
        } else {
            match g.tin.with_regions(&all_rings) {
                Ok(t) => (t, all_regions),
                Err(_) => {
                    let (mut regions, mut rings) = (vec![], vec![]);
                    let mut last = None;
                    for (k, r) in all_regions.into_iter().enumerate() {
                        rings.push(all_rings[k].clone());
                        match g.tin.with_regions(&rings) {
                            Ok(t) => {
                                last = Some(t);
                                regions.push(r);
                            }
                            Err(e) => {
                                rings.pop();
                                warnings.push(format!("terrain region {} skipped: {e}", r.id));
                            }
                        }
                    }
                    (last.unwrap_or(g.tin), regions)
                }
            }
        };
        let mut material = Vec::with_capacity(tin.tris.len());
        let mut pad_of = Vec::with_capacity(tin.tris.len());
        for (k, t) in tin.tris.iter().enumerate() {
            let [a, b, c] = t.map(|i| tin.pts[i as usize]);
            let m = v2((a.x + b.x + c.x) / 3.0, (a.y + b.y + c.y) / 3.0);
            let pad = gp
                .iter()
                .position(|p| p.outline.len() >= 3 && point_in_ring(m, &p.outline) > 0);
            if let Some(j) = pad {
                pad_of.push(Some(pads[j].0));
                material.push(pads[j].1.material.clone());
                continue;
            }
            pad_of.push(None);
            let r = tin.region_of.get(k).copied().unwrap_or(-1);
            material.push(match usize::try_from(r).ok().and_then(|r| regions.get(r)) {
                Some(reg) => reg.material.clone(),
                None => spec.material.clone(),
            });
        }
        let reports = pads
            .iter()
            .enumerate()
            .map(|(j, (pid, p))| PadReport {
                pad: *pid,
                cut: g.cut.get(j).copied().unwrap_or(0.0),
                fill: g.fill.get(j).copied().unwrap_or(0.0),
                area: area(&p.outline),
                daylight: g
                    .daylight
                    .get(j)
                    .map(|d| vec![d.iter().map(|q| (q.x, q.y)).collect()])
                    .unwrap_or_default(),
            })
            .collect();
        Ok(TopoBuilt {
            id,
            tin,
            material,
            pad_of,
            thickness: spec.thickness,
            pads: reports,
            pad_thickness: pads.iter().map(|(pid, p)| (*pid, p.thickness)).collect(),
            warnings,
        })
    }

    /// Closed solids: ground by material (terrain thickness), pad slabs (their thickness;
    /// a zero-thickness pad is a surface only).
    pub fn topo_pieces(&self, id: i64) -> Result<Vec<TopoPiece>, Error> {
        Ok((*self.topo_pieces_shared(id)?).clone())
    }

    /// Like `topo_pieces`, without a copy (the same cache as `topo_built_shared`).
    pub fn topo_pieces_shared(&self, id: i64) -> Result<Arc<Vec<TopoPiece>>, Error> {
        let key = source_key(&self.db, id)?;
        let b = self.built_by_key(id, key)?;
        if let Some(Ok(p)) = cache_find(id, key, |e| e.pieces.clone().map(Ok)) {
            return Ok(p);
        }
        let pieces = Arc::new(pieces_of(&b));
        cache_with(id, key, |e| e.pieces = Some(pieces.clone()));
        Ok(pieces)
    }

    /// Sections of the terrains by a vertical plane through `o` along `u` (unit) over the segment
    /// s ∈ [0, len]: (id, material, outline (s, z)), with s measured from `o` along `u`. As in 3D:
    /// ground has the terrain thickness by material, under a pad is its slab (thickness 0 means no fill).
    pub(crate) fn topo_section_cuts(
        &self,
        o: V2,
        u: V2,
        len: f64,
    ) -> Result<Vec<(i64, String, Vec<V2>)>, Error> {
        let mut out = vec![];
        if !(len > 0.0) {
            return Ok(out);
        }
        for (id, _) in self.toposolids()? {
            let Ok(b) = self.topo_built_shared(id) else {
                continue;
            };
            for top in b.tin.profile(o, o + u * len) {
                // pieces of one solid; the solid is determined by the triangle under the midpoint of the profile segment
                let mut run: Vec<V2> = vec![];
                let mut cur: Option<(Option<i64>, &str)> = None;
                let mut flush = |run: &mut Vec<V2>, key: Option<(Option<i64>, &str)>| {
                    if let (Some((pad, mat)), true) = (key, run.len() >= 2) {
                        let th = b.piece_thickness(pad);
                        if th > 0.0 {
                            let mut r: Vec<V2> = run.iter().map(|p| v2(p.x, p.y - th)).collect();
                            r.extend(run.iter().rev().copied());
                            out.push((id, mat.to_string(), r));
                        }
                    }
                    run.clear();
                };
                for w in top.windows(2) {
                    let mid = o + u * (0.5 * (w[0].x + w[1].x));
                    let key = b
                        .tin
                        .tri_at(mid)
                        .map(|t| (b.pad_of[t], b.material[t].as_str()));
                    if key != cur {
                        flush(&mut run, cur);
                        cur = key;
                    }
                    if run.is_empty() {
                        run.push(w[0]);
                    }
                    run.push(w[1]);
                }
                flush(&mut run, cur);
            }
        }
        Ok(out)
    }

    /// Site properties; defaults if not set.
    pub fn site(&self) -> Result<SiteProps, Error> {
        read_site(&self.db)
    }

    pub fn set_site(&mut self, s: &SiteProps) -> Result<(), Error> {
        check_site(s)?;
        self.op("topo_site", |db| Ok(((), 0, write_site(db, s)?)))
    }
}

/// Site row inside a running operation; defaults when none is stored.
pub(crate) fn read_site(db: &Connection) -> Result<SiteProps, Error> {
    {
        let row = db
            .query_row(
                "SELECT name, north_deg, base_x, base_y, base_z, latitude, longitude,
                 crs_code, crs_name, crs_vertical FROM site_info WHERE id=1",
                [],
                |r| {
                    Ok(SiteProps {
                        name: r.get(0)?,
                        north_deg: r.get(1)?,
                        base: (r.get(2)?, r.get(3)?, r.get(4)?),
                        latitude: r.get(5)?,
                        longitude: r.get(6)?,
                        crs_code: r.get(7)?,
                        crs_name: r.get(8)?,
                        crs_vertical: r.get(9)?,
                    })
                },
            )
            .optional()?;
        Ok(row.unwrap_or_default())
    }
}

/// Fills the empty coordinate system of the site (and, for a site still at the origin, its
/// base point and north) from an imported file, inside a running batch. `false`: no change.
pub fn fill_site_in_batch(
    db: &Connection,
    snaps: &mut crate::Snapshots,
    found: &SiteProps,
) -> Result<bool, Error> {
    let cur = read_site(db)?;
    let mut new = cur.clone();
    if cur.crs_code.is_empty() && !found.crs_code.is_empty() {
        new.crs_code = found.crs_code.clone();
        new.crs_name = found.crs_name.clone();
        new.crs_vertical = found.crs_vertical.clone();
        // the base point is meaningful only together with its coordinate system
        if cur.base == (0.0, 0.0, 0.0) && cur.north_deg == 0.0 {
            new.base = found.base;
            new.north_deg = found.north_deg;
        }
    }
    if new == cur {
        return Ok(false);
    }
    check_site(&new)?;
    snaps.push(write_site(db, &new)?);
    Ok(true)
}

pub(crate) fn check_site(s: &SiteProps) -> Result<(), Error> {
    let vals = [
        s.north_deg,
        s.base.0,
        s.base.1,
        s.base.2,
        s.latitude,
        s.longitude,
    ];
    if vals.iter().any(|&v| !fin(v)) || s.latitude.abs() > 90.0 || s.longitude.abs() > 180.0 {
        return invalid("site value out of range");
    }
    if !crs_code_valid(&s.crs_code)
        || !(s.crs_vertical.is_empty() || crs_code_valid(&s.crs_vertical))
        || s.crs_name.len() > 200
    {
        return invalid("site coordinate system is not AUTHORITY:CODE");
    }
    Ok(())
}

/// Writes the site inside an operation; the result is the inverse command.
pub(crate) fn write_site(db: &Connection, s: &SiteProps) -> Result<Inv, Error> {
    let had: Option<i64> = db
        .query_row("SELECT id FROM site_info WHERE id=1", [], |r| r.get(0))
        .optional()?;
    let inv = match had {
        Some(_) => Inv::default().restore(db, "site_info", "id", 1)?,
        None => Inv::default().delete("site_info", "id", 1),
    };
    db.execute(
        "INSERT OR REPLACE INTO site_info(id, name, north_deg, base_x, base_y, base_z,
         latitude, longitude, crs_code, crs_name, crs_vertical)
         VALUES (1,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            s.name,
            s.north_deg,
            s.base.0,
            s.base.1,
            s.base.2,
            s.latitude,
            s.longitude,
            s.crs_code,
            s.crs_name,
            s.crs_vertical
        ],
    )?;
    Ok(inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        Model::open(":memory:").unwrap()
    }

    fn grid(n: usize, step: f64, f: impl Fn(f64, f64) -> f64) -> Vec<(f64, f64, f64)> {
        let mut v = vec![];
        for j in 0..n {
            for i in 0..n {
                let (x, y) = (i as f64 * step, j as f64 * step);
                v.push((x, y, f(x, y)));
            }
        }
        v
    }

    fn spec(points: Vec<(f64, f64, f64)>) -> TopoSpec {
        TopoSpec {
            name: "Ground".into(),
            thickness: 2.0,
            material: "earth".into(),
            points,
            ..Default::default()
        }
    }

    fn square(x0: f64, y0: f64, w: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x0 + w, y0), (x0 + w, y0 + w), (x0, y0 + w)]
    }

    #[test]
    fn add_build_undo() {
        let mut m = model();
        let id = m
            .add_toposolid(&spec(grid(5, 2.0, |x, y| x + 0.5 * y)))
            .unwrap();
        let b = m.topo_built(id).unwrap();
        b.tin.validate().unwrap();
        assert!((b.tin.plan_area() - 64.0).abs() < 1e-9);
        assert!(b.material.iter().all(|s| s == "earth"));
        let pieces = m.topo_pieces(id).unwrap();
        assert_eq!(pieces.len(), 1);
        assert!(pieces[0].mesh.is_watertight());
        assert!((pieces[0].mesh.volume() - 128.0).abs() < 1e-6);
        m.undo().unwrap();
        assert!(m.toposolids().unwrap().is_empty());
        assert!(m.topo_points(id).unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        let mut m = model();
        assert!(m
            .add_toposolid(&spec(vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)]))
            .is_err());
        let mut s = spec(grid(3, 1.0, |_, _| 0.0));
        s.thickness = 0.0;
        assert!(m.add_toposolid(&s).is_err());
        assert!(m.toposolids().unwrap().is_empty());
    }

    #[test]
    fn point_edits_keep_numbers_and_undo() {
        let mut m = model();
        let id = m.add_toposolid(&spec(grid(4, 1.0, |_, _| 0.0))).unwrap();
        m.move_topo_point(id, 5, (1.0, 1.0, 3.0)).unwrap();
        let p = m.topo_points(id).unwrap();
        assert_eq!(p[5], (5, (1.0, 1.0, 3.0)));
        m.delete_topo_points(id, &[0, 1]).unwrap();
        let p = m.topo_points(id).unwrap();
        assert_eq!(p.len(), 14);
        assert_eq!(p[0].0, 2);
        m.add_topo_points(id, &[(5.0, 5.0, 1.0)]).unwrap();
        assert_eq!(m.topo_points(id).unwrap().last().unwrap().0, 16);
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.topo_points(id).unwrap().len(), 16);
        m.undo().unwrap();
        assert_eq!(m.topo_points(id).unwrap()[5].1 .2, 0.0);
    }

    #[test]
    fn flatten_and_smooth_one_step_each() {
        let mut m = model();
        let id = m.add_toposolid(&spec(grid(9, 1.0, |x, _| x))).unwrap();
        let n = m.flatten_topo(id, &square(2.5, 2.5, 3.0), 4.0).unwrap();
        assert_eq!(n, 9 - 3); // 9 points in the outline, three of them (x = 4) already have elevation 4
        let z: Vec<f64> = m.topo_points(id).unwrap().iter().map(|p| p.1 .2).collect();
        assert_eq!(z[3 * 9 + 3], 4.0);
        m.undo().unwrap();
        let mut pts = grid(9, 1.0, |_, _| 0.0);
        pts[40].2 = 5.0;
        let id2 = m.add_toposolid(&spec(pts)).unwrap();
        assert!(m.smooth_topo(id2, &square(2.5, 2.5, 3.0), 2).unwrap() > 0);
        let z = m.topo_points(id2).unwrap()[40].1 .2;
        assert!(z > 0.0 && z < 5.0);
        m.undo().unwrap();
        assert_eq!(m.topo_points(id2).unwrap()[40].1 .2, 5.0);
    }

    #[test]
    fn regions_and_pad_report() {
        let mut m = model();
        let id = m.add_toposolid(&spec(grid(61, 1.0, |_, _| 0.0))).unwrap();
        m.add_topo_region(id, "Path", "gravel", &square(2.0, 2.0, 5.0))
            .unwrap();
        let pad = m
            .add_pad(&PadSpec {
                topo: id,
                name: "Pad 1".into(),
                elevation: -1.0,
                slope: 1.0,
                thickness: 0.3,
                material: "concrete".into(),
                outline: square(25.0, 25.0, 10.0),
            })
            .unwrap();
        let b = m.topo_built(id).unwrap();
        b.tin.validate().unwrap();
        let r = &b.pads[0];
        assert_eq!(r.pad, pad);
        let want = 100.0 + 20.0 + std::f64::consts::PI / 3.0;
        assert!((r.cut - want).abs() < 1e-3 * want, "{}", r.cut);
        assert!(r.fill < 1e-9);
        assert!((r.area - 100.0).abs() < 1e-9);
        let gravel: f64 = b
            .tin
            .tris
            .iter()
            .zip(&b.material)
            .filter(|(_, s)| s.as_str() == "gravel")
            .map(|(t, _)| {
                let [a, b2, c] = t.map(|i| b.tin.pts[i as usize]);
                0.5 * ((b2.x - a.x) * (c.y - a.y) - (b2.y - a.y) * (c.x - a.x))
            })
            .sum();
        assert!((gravel - 25.0).abs() < 1e-9);
        let pieces = m.topo_pieces(id).unwrap();
        assert!(pieces.iter().all(|p| p.mesh.is_watertight()));
        let slab = pieces.iter().find(|p| p.pad == Some(pad)).unwrap();
        assert!((slab.mesh.volume() - 30.0).abs() < 1e-6);
        m.undo().unwrap();
        assert!(m.topo_built(id).unwrap().pads.is_empty());
    }

    #[test]
    fn section_poche_on_flat_ground() {
        let mut m = model();
        m.add_toposolid(&spec(grid(11, 1.0, |_, _| 3.0))).unwrap();
        let cuts = m
            .topo_section_cuts(v2(-5.0, 5.0), v2(1.0, 0.0), 30.0)
            .unwrap();
        assert_eq!(cuts.len(), 1);
        let r = &cuts[0].2;
        let a: f64 = (0..r.len())
            .map(|i| r[i].cross(r[(i + 1) % r.len()]))
            .sum::<f64>()
            * 0.5;
        assert!((a - 20.0).abs() < 1e-9, "{a}");
        let s: Vec<f64> = r.iter().map(|p| p.x).collect();
        assert!(s.iter().any(|&x| (x - 5.0).abs() < 1e-9));
        assert!(s.iter().any(|&x| (x - 15.0).abs() < 1e-9));
    }

    #[test]
    fn section_clipped_to_view_line() {
        let mut m = model();
        m.add_toposolid(&spec(grid(11, 1.0, |_, _| 3.0))).unwrap();
        let cuts = m
            .topo_section_cuts(v2(-5.0, 5.0), v2(1.0, 0.0), 10.0)
            .unwrap();
        let a: f64 = cuts.iter().map(|c| ring_area(&c.2)).sum();
        assert!((a - 10.0).abs() < 1e-9, "{a}");
        assert!(cuts.iter().flat_map(|c| &c.2).all(|p| p.x <= 10.0 + 1e-9));
    }

    #[test]
    fn section_under_pad_is_slab_like_3d() {
        let mut m = model();
        let id = m.add_toposolid(&spec(grid(21, 1.0, |_, _| 0.0))).unwrap();
        m.add_pad(&PadSpec {
            topo: id,
            name: "Pad".into(),
            elevation: -1.0,
            slope: 1.0,
            thickness: 0.3,
            material: "concrete".into(),
            outline: square(5.0, 5.0, 10.0),
        })
        .unwrap();
        let cuts = m
            .topo_section_cuts(v2(-5.0, 10.0), v2(1.0, 0.0), 30.0)
            .unwrap();
        let by = |mat: &str| -> f64 {
            cuts.iter()
                .filter(|c| c.1 == mat)
                .map(|c| ring_area(&c.2))
                .sum()
        };
        // ground 2 m over 10 m of length outside the slab, under the slab only the 0.3 × 10 slab
        assert!((by("earth") - 20.0).abs() < 1e-6, "{}", by("earth"));
        assert!((by("concrete") - 3.0).abs() < 1e-6, "{}", by("concrete"));
        assert!(cuts.iter().all(|c| c.1 == "earth" || c.1 == "concrete"));
        let vol: f64 = m
            .topo_pieces(id)
            .unwrap()
            .iter()
            .filter(|p| p.pad.is_some())
            .map(|p| p.mesh.volume())
            .sum();
        assert!((vol - 30.0).abs() < 1e-6);
    }

    #[test]
    fn cache_follows_edits_and_undo() {
        let mut m = model();
        let id = m.add_toposolid(&spec(grid(5, 1.0, |_, _| 1.0))).unwrap();
        let z0 = m.topo_built(id).unwrap().tin.z_at(v2(2.0, 2.0)).unwrap();
        let idx = m.topo_points(id).unwrap()[12].0;
        m.move_topo_point(id, idx, (2.0, 2.0, 5.0)).unwrap();
        assert!((m.topo_built(id).unwrap().tin.z_at(v2(2.0, 2.0)).unwrap() - 5.0).abs() < 1e-9);
        assert!(Arc::ptr_eq(
            &m.topo_built_shared(id).unwrap(),
            &m.topo_built_shared(id).unwrap()
        ));
        m.undo().unwrap();
        assert!((m.topo_built(id).unwrap().tin.z_at(v2(2.0, 2.0)).unwrap() - z0).abs() < 1e-9);
        assert!(m.topo_built(id).unwrap().warnings.is_empty());
    }

    #[test]
    fn older_file_gains_topo_tables() {
        let p = std::env::temp_dir().join(format!("ddc_topo_mig_{}.ddc", std::process::id()));
        let ps = p.to_str().unwrap();
        let _ = std::fs::remove_file(&p);
        {
            let m = Model::open(ps).unwrap();
            m.db.execute_batch(
                "DROP TABLE toposolid; DROP TABLE topo_point; DROP TABLE topo_region;                  DROP TABLE topo_pad; DROP TABLE site_info;                  UPDATE meta SET value=17 WHERE key='schema_version';",
            )
            .unwrap();
        }
        let mut m = Model::open(ps).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        let id = m.add_toposolid(&spec(grid(3, 1.0, |_, _| 0.0))).unwrap();
        assert_eq!(m.toposolids().unwrap().len(), 1);
        assert!(m.topo_built(id).is_ok());
        drop(m);
        for f in [
            String::new(),
            "-wal".into(),
            "-shm".into(),
            ".bak-v17".into(),
        ] {
            let _ = std::fs::remove_file(format!("{ps}{f}"));
        }
    }

    #[test]
    fn site_from_step_46_gains_an_empty_coordinate_system() {
        let p = std::env::temp_dir().join(format!("ddc_site_crs_{}.ddc", std::process::id()));
        let ps = p.to_str().unwrap();
        let _ = std::fs::remove_file(&p);
        {
            let m = Model::open(ps).unwrap();
            m.db.execute_batch(
                "DROP TABLE site_info;
                 CREATE TABLE site_info(id INTEGER PRIMARY KEY CHECK(id = 1), name TEXT NOT NULL,
                   north_deg REAL NOT NULL, base_x REAL NOT NULL, base_y REAL NOT NULL,
                   base_z REAL NOT NULL, latitude REAL NOT NULL, longitude REAL NOT NULL);
                 INSERT INTO site_info VALUES (1,'Old plot',15.0,100.0,200.0,5.0,48.0,11.0);
                 UPDATE meta SET value=46 WHERE key='schema_version';",
            )
            .unwrap();
        }
        let mut m = Model::open(ps).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        let s = m.site().unwrap();
        assert_eq!((s.name.as_str(), s.north_deg, s.base.0), ("Old plot", 15.0, 100.0));
        assert!(s.crs_code.is_empty() && s.crs_name.is_empty() && s.crs_vertical.is_empty());
        // the new fields write, read back and undo
        let set = SiteProps {
            crs_code: "EPSG:25832".into(),
            crs_name: "ETRS89 / UTM zone 32N".into(),
            crs_vertical: "EPSG:5783".into(),
            ..s.clone()
        };
        m.set_site(&set).unwrap();
        assert_eq!(m.site().unwrap(), set);
        m.undo().unwrap();
        assert_eq!(m.site().unwrap(), s);
        drop(m);
        for f in [String::new(), "-wal".into(), "-shm".into(), ".bak-v46".into()] {
            let _ = std::fs::remove_file(format!("{ps}{f}"));
        }
    }

    #[test]
    fn coordinate_system_codes_are_checked() {
        for ok in ["", "EPSG:25832", "EPSG:5783", "ESRI:102100", "IGNF:LAMB93"] {
            assert!(crs_code_valid(ok), "{ok}");
        }
        for bad in ["25832", "EPSG:", ":25832", "EP SG:1", "EPSG:12 34", "EPSG:1\u{43f}"] {
            assert!(!crs_code_valid(bad), "{bad}");
        }
        let mut m = model();
        let bad = SiteProps {
            crs_code: "nonsense".into(),
            ..Default::default()
        };
        assert!(m.set_site(&bad).is_err());
        let bad = SiteProps {
            crs_vertical: "5783".into(),
            ..Default::default()
        };
        assert!(m.set_site(&bad).is_err());
    }

    #[test]
    fn utm_is_only_a_suggestion() {
        assert_eq!(
            suggest_utm(48.1, 11.6),
            Some(("EPSG:32632".into(), "WGS 84 / UTM zone 32N".into()))
        );
        assert_eq!(suggest_utm(-33.9, 151.2).unwrap().0, "EPSG:32756");
        assert_eq!(suggest_utm(0.0, 0.0), None);
        assert_eq!(suggest_utm(88.0, 10.0), None);
        // suggesting does not write anything
        let m = model();
        assert_eq!(m.site().unwrap().crs_code, "");
    }

    fn ring_area(r: &[V2]) -> f64 {
        (0..r.len())
            .map(|i| r[i].cross(r[(i + 1) % r.len()]))
            .sum::<f64>()
            * 0.5
    }

    #[test]
    fn site_default_set_undo() {
        let mut m = model();
        assert_eq!(m.site().unwrap(), SiteProps::default());
        let s = SiteProps {
            name: "Plot".into(),
            north_deg: 12.5,
            base: (100.0, 200.0, 50.0),
            latitude: 55.0,
            longitude: 37.0,
            crs_code: "EPSG:32637".into(),
            crs_name: "WGS 84 / UTM zone 37N".into(),
            crs_vertical: String::new(),
        };
        m.set_site(&s).unwrap();
        assert_eq!(m.site().unwrap(), s);
        m.undo().unwrap();
        assert_eq!(m.site().unwrap(), SiteProps::default());
        assert!(m
            .set_site(&SiteProps {
                latitude: 95.0,
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn migrate_twice_and_version() {
        let m = model();
        migrate(&m.db).unwrap();
        migrate(&m.db).unwrap();
        let v: i64 =
            m.db.query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, crate::schema::VERSION);
    }

    #[test]
    fn delete_removes_everything_and_undo_restores() {
        let mut m = model();
        let id = m
            .add_toposolid(&spec(grid(6, 1.0, |x, y| x * y * 0.1)))
            .unwrap();
        m.add_topo_region(id, "Lawn", "grass", &square(1.0, 1.0, 2.0))
            .unwrap();
        m.delete_toposolid(id).unwrap();
        assert!(m.toposolid(id).is_err());
        assert!(m.topo_regions(id).unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.topo_points(id).unwrap().len(), 36);
        assert_eq!(m.topo_regions(id).unwrap().len(), 1);
    }
    #[test]
    fn plan_transforms_keep_elevations_and_undo_in_one_step() {
        use crate::{Spacing, Target};
        let mut m = model();
        let id = m
            .add_toposolid(&spec(grid(5, 2.0, |x, y| x + 0.5 * y)))
            .unwrap();
        m.add_topo_region(id, "Lawn", "grass", &square(1.0, 1.0, 2.0))
            .unwrap();
        let before = (m.toposolid(id).unwrap(), m.topo_regions(id).unwrap());
        m.move_elements(&[Target::Topo(id)], (10.0, -4.0)).unwrap();
        let moved = m.toposolid(id).unwrap();
        for (a, b) in before.0.points.iter().zip(&moved.points) {
            assert!((b.0 - a.0 - 10.0).abs() < 1e-9 && (b.1 - a.1 + 4.0).abs() < 1e-9);
            assert_eq!(a.2, b.2);
        }
        assert_eq!(m.topo_regions(id).unwrap()[0].ring[0], (11.0, -3.0));
        m.undo().unwrap();
        assert_eq!((m.toposolid(id).unwrap(), m.topo_regions(id).unwrap()), before);

        let copies = m
            .array_linear(&[Target::Topo(id)], 3, Spacing::Step((20.0, 0.0)))
            .unwrap();
        assert_eq!(copies.len(), 2);
        let Target::Topo(c) = copies[1] else { panic!() };
        assert_eq!(m.topo_points(c).unwrap().len(), 25);
        assert_eq!(m.topo_regions(c).unwrap().len(), 1);
        m.undo().unwrap();
        assert_eq!(m.toposolids().unwrap().len(), 1);

        m.mirror_elements(&[Target::Topo(id)], (0.0, 0.0), (0.0, 1.0), false)
            .unwrap();
        let mirrored = m.toposolid(id).unwrap();
        assert!(mirrored.points.iter().all(|p| p.0 <= 1e-9));
        m.rotate_elements(&[Target::Topo(id)], (0.0, 0.0), 0.5, false)
            .unwrap();
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.toposolid(id).unwrap(), before.0);
    }

    #[test]
    fn terrain_lifts_with_break_lines_in_one_undo_step() {
        use crate::Target;
        let mut m = model();
        let mut sp = spec(grid(4, 2.0, |x, y| x + 0.5 * y));
        sp.breaklines = vec![vec![(0.0, 0.0, 1.0), (2.0, 0.0, 2.0)]];
        let id = m.add_toposolid(&sp).unwrap();
        let before = m.toposolid(id).unwrap();
        m.lift_elements(&[Target::Topo(id)], 1.5).unwrap();
        let up = m.toposolid(id).unwrap();
        for (a, b) in before.points.iter().zip(&up.points) {
            assert_eq!((a.0, a.1), (b.0, b.1));
            assert!((b.2 - a.2 - 1.5).abs() < 1e-9);
        }
        assert!((up.breaklines[0][1].2 - 3.5).abs() < 1e-9);
        m.undo().unwrap();
        assert_eq!(m.toposolid(id).unwrap(), before);
    }
}
