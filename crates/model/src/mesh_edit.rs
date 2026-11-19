// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Imported meshes to editable elements: recognition (wall, slab, roof, column, beam,
//! extrusion solid) and conversion, editing the mesh itself (transform, plane trim and split,
//! face removal, merge, cleanup), and mesh plan and section views.
//! Each action is one journal operation; mesh row snapshots use `Inv::restore` (the journal
//! stores the BLOB). A converted element is tagged with the `ddc.converted_from` property:
//! re-importing the BIM data updates it instead of inserting the mesh again.

use ddc_geom::member::{Just, Shape};
use ddc_geom::mesh::Mesh;
use ddc_geom::mesh_fit::{self, Candidate, Fit, Kind, Options};
use ddc_geom::mesh_ops::{self, Affine, CleanReport, Keep, Plane, Region, Stats};
use ddc_geom::profile::signed_area;
use ddc_geom::slope::Plane as Slope;
use ddc_geom::{v2, v3, V2, V3};
use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::views::{LineClass, ViewPrim};
use crate::{
    roofs, slabs, structure, BeamJoin, BeamSpec, ColumnSpec, Error, Model, RoofKind, RoofSpec,
    SlabSpec,
};

/// Weld and "on plane" tolerance, m.
const TOL: f64 = 1e-6;
/// Face crease that is drawn as a line (~20°).
const EDGE_COS: f64 = 0.94;
/// Projection edges are computed only for meshes no larger than this.
const EDGE_TRIS: usize = 20_000;
/// Triangle budget per view (plan/section).
const VIEW_TRIS: usize = 3_000_000;
/// Material of layers and sections created from meshes.
const MATERIAL: &str = "Imported";

/// Tables the conversion writes to; undo deletes rows above the watermark.
pub(crate) const CONVERT_TABLES: [&str; 14] = [
    "wall_type",
    "wall_type_layer",
    "wall",
    "opening",
    "slab_type",
    "slab_type_layer",
    "slab",
    "slab_vertex",
    "member_type",
    "col",
    "beam",
    "roof",
    "roof_vertex",
    "prop",
];

/// A mesh from the `mesh` table (world coordinates, m).
pub type MeshInfo = crate::imported::ImportedMesh;

/// Property of a new element: which mesh it was made from (mesh id).
pub const CONVERTED_FROM: &str = "ddc.converted_from";
/// Property of a merge-result mesh: comma-separated ids of the source meshes.
pub const MERGED_FROM: &str = "ddc.merged_from";

/// Result of converting one mesh: `table` is the table of the new element
/// (`wall`, `slab`, `roof`, `col`, `beam`, `solid`; `mesh` means the profile was written to a mesh property).
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    pub mesh: i64,
    pub table: &'static str,
    pub id: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConvertReport {
    pub done: Vec<Converted>,
    /// (mesh, reason): these meshes are untouched.
    pub failed: Vec<(i64, String)>,
    /// (mesh, what was simplified): the element was created but not everything carried over (e.g. an opening did not fit).
    pub warnings: Vec<(i64, String)>,
}

/// A mesh on a level plan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshPlan {
    pub mesh: i64,
    /// Section by the cut plane (section fill), plan coordinates.
    pub cut: Vec<Region>,
    /// Sections of open surfaces (no thickness).
    pub open: Vec<Vec<V2>>,
    /// Visible mesh edges entirely below the cut plane.
    pub below: Vec<(V2, V2)>,
    /// Section fill as triangles (holes are real holes), plan coordinates.
    pub fill: Vec<[V2; 3]>,
    /// Projected outline (convex hull, closed ring) of a mesh that misses the cut plane but
    /// lies within the view range: below the cut, or above it up to the next level.
    pub beyond: Vec<V2>,
    /// `beyond` lies above the cut plane (overhead, drawn dashed).
    pub overhead: bool,
}

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

// ---------- BLOB ----------

/// positions are f64 LE x y z; indices are u32 LE, three per triangle.
pub(crate) fn decode(pos: &[u8], idx: &[u8]) -> Result<Mesh, Error> {
    if pos.len() % 24 != 0 || idx.len() % 12 != 0 {
        return invalid("mesh: bad blob size");
    }
    let f = |c: &[u8]| f64::from_le_bytes(c.try_into().unwrap_or([0; 8]));
    let positions: Vec<V3> = pos
        .chunks_exact(24)
        .map(|c| v3(f(&c[0..8]), f(&c[8..16]), f(&c[16..24])))
        .collect();
    let n = positions.len() as u32;
    let mut triangles = Vec::with_capacity(idx.len() / 12);
    for c in idx.chunks_exact(12) {
        let u = |k: usize| u32::from_le_bytes(c[k..k + 4].try_into().unwrap_or([0; 4]));
        let t = [u(0), u(4), u(8)];
        if t.iter().any(|&i| i >= n) {
            return invalid("mesh: index out of range");
        }
        triangles.push(t);
    }
    let face_of = vec![0; triangles.len()];
    Ok(Mesh {
        positions,
        triangles,
        face_of,
    })
}

pub(crate) use crate::imported::mesh_blobs as encode;

pub(crate) fn info(db: &Connection, id: i64) -> Result<MeshInfo, Error> {
    let r = db
        .query_row(
            "SELECT level, category, name, positions, indices FROM mesh WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((level, category, name, p, i)) = r else {
        return invalid(format!("no mesh {id}"));
    };
    Ok(MeshInfo {
        id,
        level,
        category,
        name,
        mesh: decode(&p, &i)?,
    })
}

fn put(db: &Connection, id: i64, m: &Mesh) -> Result<(), Error> {
    let (p, i) = encode(m);
    db.execute(
        "UPDATE mesh SET positions=?2, indices=?3 WHERE id=?1",
        params![id, p, i],
    )?;
    Ok(())
}

// ---------- journal ----------

/// Snapshot of a mesh row (all columns, rowid): undo inserts it back with the same id.
fn keep_mesh(db: &Connection, id: i64, inv: Inv) -> Result<Inv, Error> {
    let n: i64 = db.query_row("SELECT count(*) FROM mesh WHERE id=?1", [id], |r| r.get(0))?;
    if n == 0 {
        return invalid(format!("no mesh {id}"));
    }
    inv.restore(db, "mesh", "id", id)
}

/// Snapshot of an element's properties with their rowid (otherwise restored rows would land above
/// the watermark of an earlier batch operation).
pub(crate) fn keep_props(db: &Connection, kind: &str, id: i64, mut inv: Inv) -> Result<Inv, Error> {
    let mut st =
        db.prepare_cached("SELECT rowid, key, value, unit FROM prop WHERE kind=?1 AND ref=?2")?;
    let rows = st.query_map(params![kind, id], |r| {
        Ok(vec![
            r.get::<_, Value>(0)?,
            Value::Text(kind.to_string()),
            Value::Integer(id),
            r.get::<_, Value>(1)?,
            r.get::<_, Value>(2)?,
            r.get::<_, Value>(3)?,
        ])
    })?;
    for r in rows {
        inv = inv.sql(
            "INSERT OR REPLACE INTO prop(rowid,kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5,?6)",
            r?,
        );
    }
    Ok(inv)
}

/// External references to `from` move to `to`; undo restores them.
pub(crate) fn repoint(
    db: &Connection,
    from: (&str, i64),
    to: (&str, i64),
    mut inv: Inv,
) -> Result<Inv, Error> {
    let refs: Vec<(String, String)> = {
        let mut st =
            db.prepare_cached("SELECT source, guid FROM ext_ref WHERE kind=?1 AND ref=?2")?;
        let rows = st.query_map(params![from.0, from.1], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    if refs.is_empty() {
        return Ok(inv);
    }
    db.execute(
        "UPDATE ext_ref SET kind=?1, ref=?2 WHERE kind=?3 AND ref=?4",
        params![to.0, to.1, from.0, from.1],
    )?;
    for (s, g) in refs {
        inv = inv.sql(
            "UPDATE ext_ref SET kind=?1, ref=?2 WHERE source=?3 AND guid=?4",
            vec![
                Value::Text(from.0.to_string()),
                Value::Integer(from.1),
                Value::Text(s),
                Value::Text(g),
            ],
        );
    }
    Ok(inv)
}

/// Deletes a mesh and its properties; undo restores both.
fn drop_mesh(db: &Connection, id: i64, inv: Inv) -> Result<Inv, Error> {
    let inv = keep_props(db, "mesh", id, keep_mesh(db, id, inv)?)?;
    db.execute("DELETE FROM prop WHERE kind='mesh' AND ref=?1", [id])?;
    db.execute("DELETE FROM mesh WHERE id=?1", [id])?;
    Ok(inv)
}

pub(crate) fn has_table(db: &Connection, t: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [t],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

pub(crate) fn watermarks(db: &Connection, tables: &[&str]) -> Result<Vec<(String, i64)>, Error> {
    tables
        .iter()
        .map(|t| -> Result<(String, i64), Error> {
            let m = db.query_row(
                &format!("SELECT COALESCE(MAX(rowid),0) FROM {t}"),
                [],
                |r| r.get::<_, i64>(0),
            )?;
            Ok((t.to_string(), m))
        })
        .collect()
}

pub(crate) fn unmark(marks: &[(String, i64)]) -> Inv {
    marks.iter().rev().fold(Inv::default(), |inv, (t, m)| {
        inv.sql(
            &format!("DELETE FROM {t} WHERE rowid>?1"),
            vec![Value::Integer(*m)],
        )
    })
}

fn dedup(ids: &[i64]) -> Vec<i64> {
    let mut out: Vec<i64> = vec![];
    for &i in ids {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

// ---------- conversion ----------

/// Level of an element: the mesh's level, else the highest level not above its bottom, else the lowest.
fn host_level(db: &Connection, lv: Option<i64>, zmin: f64) -> Result<(i64, f64), Error> {
    let ls = crate::walls::levels(db)?;
    if let Some(&l) = lv.and_then(|l| ls.iter().find(|x| x.0 == l)) {
        return Ok(l);
    }
    ls.iter()
        .filter(|x| x.1 <= zmin + 1e-6)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .or_else(|| ls.iter().min_by(|a, b| a.1.total_cmp(&b.1)))
        .copied()
        .ok_or_else(|| Error::Invalid("no levels".into()))
}

/// Thickness rounded to 0.1 mm so identical meshes share a type.
fn tenth_mm(t: f64) -> f64 {
    (t * 10_000.0).round() / 10_000.0
}

/// Single-layer type by thickness (`wall_type`/`slab_type`): found by name or created.
fn layered_type(db: &Connection, table: &str, label: &str, t: f64) -> Result<i64, Error> {
    let t = tenth_mm(t);
    if !(t > 0.0 && t.is_finite()) {
        return invalid(format!("{label}: thickness {t}"));
    }
    let name = format!("{label} {} mm", (t * 10_000.0).round() / 10.0);
    let found = db
        .query_row(
            &format!("SELECT id FROM {table} WHERE name=?1"),
            [&name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    if let Some(id) = found {
        return Ok(id);
    }
    db.execute(&format!("INSERT INTO {table}(name) VALUES (?1)"), [&name])?;
    let id = db.last_insert_rowid();
    db.execute(
        &format!("INSERT INTO {table}_layer VALUES (?1,0,?2,?3)"),
        params![id, MATERIAL, t],
    )?;
    Ok(id)
}

fn fnv(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    })
}

/// Member type by section: any existing one with the same section, or a new one.
fn member_type(db: &Connection, shape: &Shape) -> Result<i64, Error> {
    shape.check().map_err(Error::Invalid)?;
    let text = shape.text();
    let found = db
        .query_row(
            "SELECT id FROM member_type WHERE shape=?1 ORDER BY id LIMIT 1",
            [&text],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    if let Some(id) = found {
        return Ok(id);
    }
    let name = match shape {
        Shape::Poly(_) => format!("{MATERIAL} profile {:08x}", fnv(&text)),
        _ => format!("{MATERIAL} {text}"),
    };
    db.execute(
        "INSERT INTO member_type(name, shape, material) VALUES (?1,?2,?3)",
        params![name, text, MATERIAL],
    )?;
    Ok(db.last_insert_rowid())
}

fn tuples(r: &[V2]) -> Vec<(f64, f64)> {
    r.iter().map(|p| (p.x, p.y)).collect()
}

#[allow(clippy::too_many_arguments)]
fn insert_slab(
    db: &Connection,
    level: i64,
    elev: f64,
    outer: &[V2],
    holes: &[Vec<V2>],
    top: f64,
    t: f64,
    slope: Option<Slope>,
) -> Result<i64, Error> {
    let st = layered_type(db, "slab_type", "Imported slab", t)?;
    let spec = SlabSpec {
        level,
        slab_type: st,
        offset: top - elev,
        outer: tuples(outer),
        holes: holes.iter().map(|h| tuples(h)).collect(),
    };
    let prof = slabs::check_slab_in(db, &spec)?;
    db.execute(
        "INSERT INTO slab(level, stype, offset) VALUES (?1,?2,?3)",
        params![level, st, spec.offset],
    )?;
    let id = db.last_insert_rowid();
    slabs::write_rings(db, id, &prof)?;
    if let Some(p) = slope.filter(|p| !p.is_flat()) {
        db.execute(
            "UPDATE slab SET slope=?1 WHERE id=?2",
            params![slabs::slope_text(&p), id],
        )?;
    }
    Ok(id)
}

/// Extrusion solid into the `solid` table (typed-solid format: work plane
/// {"o","x","y","z"} with o.z relative to the level, and shape {"kind":"extrusion",…}). The single
/// write point: once typed solids are merged, replace it with their own validation and write.
fn insert_solid(
    db: &Connection,
    level: i64,
    elev: f64,
    name: &str,
    e: &mesh_fit::Extrusion,
) -> Result<i64, Error> {
    if !has_table(db, "solid")? {
        return invalid("extrusion: parametric solids are not available in this model");
    }
    let unit = |v: V3| (v.len() - 1.0).abs() < 1e-6;
    let ortho =
        e.x.dot(e.y).abs() < 1e-6 && e.x.dot(e.dir).abs() < 1e-6 && e.y.dot(e.dir).abs() < 1e-6;
    if !(unit(e.x) && unit(e.y) && unit(e.dir) && ortho) {
        return invalid("extrusion: work plane axes must be orthonormal");
    }
    if !(e.depth.is_finite() && e.depth > TOL) || e.outer.len() < 3 {
        return invalid("extrusion: degenerate profile");
    }
    let name: String = name.chars().take(200).collect();
    db.execute(
        "INSERT INTO solid(level,name,kind,void,material,plane,def) \
         VALUES (?1,?2,'extrusion',0,?3,?4,?5)",
        params![level, name, MATERIAL, e.plane_json(elev), e.def_json()],
    )?;
    Ok(db.last_insert_rowid())
}

/// Height of the top polyline above the bottom at u (clamped to the ends outside the range).
fn top_at(t: &[(f64, f64)], u: f64) -> f64 {
    let (Some(f), Some(l)) = (t.first(), t.last()) else {
        return 0.0;
    };
    if u <= f.0 {
        return f.1;
    }
    for w in t.windows(2) {
        if u <= w[1].0 {
            let d = w[1].0 - w[0].0;
            return if d > 0.0 {
                w[0].1 + (w[1].1 - w[0].1) * (u - w[0].0) / d
            } else {
                w[0].1.min(w[1].1)
            };
        }
    }
    l.1
}

/// Smallest polyline height on [u0, u1]: the minimum at the ends or at interior vertices.
fn top_min(t: &[(f64, f64)], u0: f64, u1: f64) -> f64 {
    t.iter()
        .filter(|p| p.0 > u0 && p.0 < u1)
        .map(|p| p.1)
        .fold(top_at(t, u0).min(top_at(t, u1)), f64::min)
}

/// Recognition without a database (UI background thread): the category restricts the kinds;
/// extrusion only if the model has typed solids (`solids`).
pub fn recognize_mesh_data(mesh: &Mesh, category: &str, solids: bool) -> Vec<Candidate> {
    recognize_hinted(mesh, category, None, solids)
}

fn recognize_hinted(
    mesh: &Mesh,
    category: &str,
    hint: Option<Kind>,
    solids: bool,
) -> Vec<Candidate> {
    let mut c = mesh_fit::recognize_category(mesh, category, hint, &Options::default());
    c.retain(|c| solids || c.kind() != Kind::Extrusion);
    c
}

/// Trim result: for a closed mesh the cut must close.
fn capped(c: mesh_ops::CutOut) -> Result<Mesh, Error> {
    if c.source_closed && !c.capped {
        return invalid("cut: the section does not close, the mesh would be left open");
    }
    Ok(c.mesh)
}

/// Triangles whose normal is within the angle of `d` (cos·|n| ≤ n·d); zero-area triangles are excluded.
fn facing(m: &Mesh, d: V3, cos: f64) -> Vec<usize> {
    m.triangles
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let [a, b, c] = t.map(|k| m.positions[k as usize]);
            let n = (b - a).cross(c - a);
            let l = n.len();
            (l > 1e-18 && n.dot(d) >= cos * l - 1e-12 * l).then_some(i)
        })
        .collect()
}

/// A mesh without geometry, for UI lists and caches.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshRow {
    pub id: i64,
    pub level: Option<i64>,
    pub category: String,
    pub name: String,
    pub triangles: usize,
    /// (connection, row revision): changes on any mesh edit, including undo.
    pub sig: (u64, i64),
}

static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Mesh revisions of this connection: a temp table and triggers (the file schema is unchanged).
/// Returns the connection number, so signatures never match across different open models.
fn track(db: &Connection) -> Result<u64, Error> {
    let made: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_temp_master WHERE type='table' AND name='mesh_epoch'",
        [],
        |r| r.get(0),
    )?;
    if made > 0 {
        let v: Option<i64> = db
            .query_row("SELECT v FROM temp.mesh_epoch", [], |r| r.get(0))
            .optional()?;
        if let Some(v) = v {
            return Ok(v as u64);
        }
    }
    let v = EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    db.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS mesh_rev(id INTEGER PRIMARY KEY, rev INTEGER NOT NULL);
         CREATE TEMP TABLE IF NOT EXISTS mesh_epoch(v INTEGER NOT NULL);
         CREATE TEMP TRIGGER IF NOT EXISTS mesh_rev_i AFTER INSERT ON main.mesh BEGIN
           INSERT OR REPLACE INTO mesh_rev VALUES (new.id, (SELECT COALESCE(MAX(rev),0)+1 FROM mesh_rev));
         END;
         CREATE TEMP TRIGGER IF NOT EXISTS mesh_rev_u AFTER UPDATE ON main.mesh BEGIN
           INSERT OR REPLACE INTO mesh_rev VALUES (new.id, (SELECT COALESCE(MAX(rev),0)+1 FROM mesh_rev));
         END;
         CREATE TEMP TRIGGER IF NOT EXISTS mesh_rev_d AFTER DELETE ON main.mesh BEGIN
           INSERT OR REPLACE INTO mesh_rev VALUES (old.id, (SELECT COALESCE(MAX(rev),0)+1 FROM mesh_rev));
         END;",
    )?;
    db.execute("INSERT INTO temp.mesh_epoch(v) VALUES (?1)", [v as i64])?;
    Ok(v)
}

fn structural(category: &str) -> bool {
    let c = category.to_lowercase();
    c.contains("struct") || c.contains("\u{43d}\u{435}\u{441}\u{443}\u{449}") || c.contains("\u{43a}\u{430}\u{440}\u{43a}\u{430}\u{441}")
}

/// One mesh to a native element. An error leaves no records (the caller rolls back the
/// savepoint). The third value holds warnings (what was not carried over).
pub(crate) fn convert_one(
    db: &Connection,
    id: i64,
    fit: &Fit,
    keep_source: bool,
) -> Result<(Converted, Inv, Vec<String>), Error> {
    let src = info(db, id)?;
    let zmin = src
        .mesh
        .positions
        .iter()
        .map(|p| p.z)
        .fold(f64::INFINITY, f64::min);
    if !zmin.is_finite() {
        return invalid(format!("mesh {id} is empty"));
    }
    let (level, elev) = host_level(db, src.level, zmin)?;
    let mut inv = Inv::default();
    let mut warn: Vec<String> = vec![];
    let (table, new): (&'static str, i64) = match fit {
        Fit::Wall {
            a,
            b,
            z0,
            height,
            thickness,
            openings,
            top,
        } => {
            crate::walls::check_len("wall", (a.x, a.y), (b.x, b.y))?;
            if !(height.is_finite() && *height > 0.0 && z0.is_finite()) {
                return invalid("wall: bad height");
            }
            let top_text = match top {
                Some(t) => {
                    let s = crate::walls::top_profile_text(t);
                    if crate::walls::parse_top_profile(&s).is_none() {
                        return invalid("wall: bad top profile");
                    }
                    Some(s)
                }
                None => None,
            };
            let wt = layered_type(db, "wall_type", "Imported wall", *thickness)?;
            db.execute(
                "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL)",
                params![level, wt, a.x, a.y, b.x, b.y, z0 - elev, height],
            )?;
            let w = db.last_insert_rowid();
            if let Some(s) = &top_text {
                db.execute("UPDATE wall SET top_profile=?2 WHERE id=?1", params![w, s])?;
            }
            let len = (*b - *a).len();
            let mut placed: Vec<(f64, f64, f64, f64)> = vec![];
            for o in openings {
                let (u0, u1) = (o.along, o.along + o.width);
                let (s0, s1) = (o.sill, o.sill + o.height);
                let roof = match top {
                    Some(t) => top_min(t, u0.max(0.0), u1.min(len)),
                    None => *height,
                };
                let fits = o.width > TOL
                    && o.height > TOL
                    && u0 >= -TOL
                    && u1 <= len + TOL
                    && s0 >= -TOL
                    && s1 <= roof + TOL;
                let overlaps = placed
                    .iter()
                    .any(|p| u0 < p.1 - TOL && p.0 < u1 - TOL && s0 < p.3 - TOL && p.2 < s1 - TOL);
                if !fits || overlaps {
                    let why = if overlaps {
                        "overlaps another opening"
                    } else {
                        "does not fit the wall"
                    };
                    warn.push(format!(
                        "opening {:.3} x {:.3} m at {:.3} m {why}",
                        o.width, o.height, o.along
                    ));
                    continue;
                }
                db.execute(
                    "INSERT INTO opening(wall,along,width,sill,height,kind) \
                     VALUES (?1,?2,?3,?4,?5,'void')",
                    params![w, o.along.max(0.0), o.width, o.sill.max(0.0), o.height],
                )?;
                placed.push((u0, u1, s0, s1));
            }
            ("wall", w)
        }
        Fit::Slab {
            outer,
            holes,
            top,
            thickness,
        } => (
            "slab",
            insert_slab(db, level, elev, outer, holes, *top, *thickness, None)?,
        ),
        Fit::SlopedSlab {
            outer,
            holes,
            p0,
            top_z,
            gx,
            gy,
            thickness,
        } => {
            let s = Slope {
                p0: *p0,
                gx: *gx,
                gy: *gy,
            };
            (
                "slab",
                insert_slab(db, level, elev, outer, holes, *top_z, *thickness, Some(s))?,
            )
        }
        Fit::Roof {
            ring,
            eave,
            slope_deg,
            base_z,
            thickness,
        } => {
            let st = layered_type(db, "slab_type", "Imported roof", *thickness)?;
            let spec = RoofSpec {
                level,
                slab_type: st,
                offset: base_z - elev,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: tuples(ring),
                    slopes: (0..ring.len())
                        .map(|i| (i == *eave).then_some(*slope_deg))
                        .collect(),
                    mansard: None,
                },
            };
            roofs::check(db, &spec)?;
            ("roof", roofs::write(db, &spec, None)?)
        }
        Fit::Column {
            at,
            z0,
            z1,
            shape,
            rotation,
        } => {
            let s = ColumnSpec {
                level,
                top_level: None,
                member_type: member_type(db, shape)?,
                structural: structural(&src.category),
                at: (at.x, at.y),
                base_offset: z0 - elev,
                top_offset: z1 - elev,
                rotation: *rotation,
                top_at: None,
            };
            structure::check_column(db, &s)?;
            ("col", structure::insert_column(db, &s, None)?)
        }
        Fit::Beam {
            a,
            b,
            shape,
            rotation,
        } => {
            // nodes are not trimmed: the imported shape is already final
            let s = BeamSpec {
                level,
                member_type: member_type(db, shape)?,
                a: (a.x, a.y, a.z - elev),
                b: (b.x, b.y, b.z - elev),
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Center,
                rotation: *rotation,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::None,
                join_end: BeamJoin::None,
            };
            structure::check_beam(db, &s)?;
            ("beam", structure::insert_beam(db, &s, None)?)
        }
        // without a typed-solid table this is an error: the mesh stays a mesh and is not counted as converted
        Fit::Extrusion(e) => ("solid", insert_solid(db, level, elev, &src.name, e)?),
    };
    db.execute(
        "INSERT OR IGNORE INTO prop(kind,ref,key,value,unit) \
         SELECT ?1, ?2, key, value, unit FROM prop WHERE kind='mesh' AND ref=?3 \
         AND key NOT IN (?4, ?5)",
        params![table, new, id, CONVERTED_FROM, MERGED_FROM],
    )?;
    db.execute(
        "INSERT OR IGNORE INTO prop(kind,ref,key,value,unit) \
         VALUES (?1,?2,'ddc.mesh_category',?3,'')",
        params![table, new, src.category],
    )?;
    // tag for BIM re-import: the element was made by the user from a mesh
    db.execute(
        "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,'')",
        params![table, new, CONVERTED_FROM, id],
    )?;
    if !keep_source {
        inv = repoint(db, ("mesh", id), (table, new), inv)?;
        inv = drop_mesh(db, id, inv)?;
    }
    Ok((
        Converted {
            mesh: id,
            table,
            id: new,
        },
        inv,
        warn,
    ))
}

impl Model {
    pub fn mesh_info(&self, id: i64) -> Result<MeshInfo, Error> {
        info(&self.db, id)
    }

    /// Meshes of a level (None for all) and category (None for any), by id.
    pub fn mesh_ids(&self, level: Option<i64>, category: Option<&str>) -> Result<Vec<i64>, Error> {
        let mut st = self.db.prepare(
            "SELECT id FROM mesh WHERE (?1 IS NULL OR level=?1) AND (?2 IS NULL OR category=?2) \
             ORDER BY id",
        )?;
        let rows = st.query_map(params![level, category], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// (category, mesh count), by name.
    pub fn mesh_categories(&self) -> Result<Vec<(String, i64)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT category, count(*) FROM mesh GROUP BY category ORDER BY category")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Conversion candidates, best first. Without a hint, by mesh category; a foreign
    /// category (furniture, doors, ...) restricts the kinds.
    pub fn recognize_mesh(&self, id: i64, hint: Option<Kind>) -> Result<Vec<Candidate>, Error> {
        let m = info(&self.db, id)?;
        Ok(recognize_hinted(
            &m.mesh,
            &m.category,
            hint,
            self.solids_available()?,
        ))
    }

    /// Whether the typed-solid table exists (otherwise extrusion is not offered).
    pub fn solids_available(&self) -> Result<bool, Error> {
        has_table(&self.db, "solid")
    }

    /// Mesh rows of a level (None for all) and category, without geometry, with a revision signature.
    pub fn mesh_rows(
        &self,
        level: Option<i64>,
        category: Option<&str>,
    ) -> Result<Vec<MeshRow>, Error> {
        let epoch = track(&self.db)?;
        let mut st = self.db.prepare(
            "SELECT m.id, m.level, m.category, m.name, length(m.indices)/12, \
             COALESCE(r.rev, 0) FROM mesh m LEFT JOIN temp.mesh_rev r ON r.id = m.id \
             WHERE (?1 IS NULL OR m.level=?1) AND (?2 IS NULL OR m.category=?2) ORDER BY m.id",
        )?;
        let rows = st.query_map(params![level, category], |r| {
            Ok(MeshRow {
                id: r.get(0)?,
                level: r.get(1)?,
                category: r.get(2)?,
                name: r.get(3)?,
                triangles: r.get::<_, i64>(4)? as usize,
                sig: (epoch, r.get(5)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Batch conversion as one operation: each mesh becomes its own element (types by thickness
    /// and section are found or created), properties are copied, external references move to the
    /// new element, and the source mesh is deleted (unless `keep_source`). A mesh that could not
    /// be converted stays as it was and is reported in `failed`.
    pub fn convert_meshes(
        &mut self,
        items: &[(i64, Fit)],
        keep_source: bool,
    ) -> Result<ConvertReport, Error> {
        if items.is_empty() {
            return invalid("no meshes to convert");
        }
        // operation kind set to "wall": recomputes derived tables (walls, slabs, members)
        self.op("wall_from_mesh", |db| {
            let mut tables: Vec<&str> = CONVERT_TABLES.to_vec();
            if has_table(db, "solid")? {
                tables.push("solid");
            }
            let marks = watermarks(db, &tables)?;
            let mut report = ConvertReport::default();
            let mut restores = Inv::default();
            for (id, fit) in items {
                db.execute_batch("SAVEPOINT mesh_convert")?;
                match convert_one(db, *id, fit, keep_source) {
                    Ok((c, inv, warn)) => {
                        db.execute_batch("RELEASE mesh_convert")?;
                        restores = restores.then(inv);
                        report.done.push(c);
                        report.warnings.extend(warn.into_iter().map(|w| (*id, w)));
                    }
                    Err(e) => {
                        db.execute_batch("ROLLBACK TO mesh_convert; RELEASE mesh_convert")?;
                        report.failed.push((*id, e.to_string()));
                    }
                }
            }
            let Some(first) = report.done.first().map(|c| c.id) else {
                let why = report.failed.first().map(|f| f.1.clone());
                return invalid(why.unwrap_or_default());
            };
            Ok((report, first, unmark(&marks).then(restores)))
        })
    }

    /// Edits mesh geometry as one operation; `f` receives the mesh and its id.
    fn edit_meshes(
        &mut self,
        kind: &str,
        ids: &[i64],
        f: impl Fn(&Mesh) -> Result<Mesh, Error>,
    ) -> Result<(), Error> {
        let ids = dedup(ids);
        if ids.is_empty() {
            return invalid("no meshes");
        }
        self.op(kind, |db| {
            let mut inv = Inv::default();
            for &id in &ids {
                let m = f(&info(db, id)?.mesh)?;
                if m.triangles.is_empty() {
                    return invalid(format!("mesh {id}: nothing left"));
                }
                inv = keep_mesh(db, id, inv)?;
                put(db, id, &m)?;
            }
            Ok(((), ids[0], inv))
        })
    }

    /// Translate, rotate, scale, mirror (det < 0 reverses face winding).
    pub fn transform_meshes(&mut self, ids: &[i64], f: &Affine) -> Result<(), Error> {
        if !f.is_valid() {
            return invalid("mesh transform: degenerate");
        }
        self.edit_meshes("mesh_transform", ids, |m| Ok(mesh_ops::transform(m, f)))
    }

    /// Plane trim: the `keep` side stays and the cut is capped with faces. A closed mesh
    /// whose cut does not close is an error (never a silently open mesh).
    pub fn cut_meshes(&mut self, ids: &[i64], pl: &Plane, keep: Keep) -> Result<(), Error> {
        self.edit_meshes("mesh_cut", ids, |m| capped(mesh_ops::cut(m, pl, keep, TOL)))
    }

    /// Plane split: the lower part stays in the original mesh, the upper part becomes a new mesh
    /// with the same level, category, name and properties. Meshes the plane does not
    /// cross are unchanged. Returns the ids of the new meshes.
    pub fn split_meshes(&mut self, ids: &[i64], pl: &Plane) -> Result<Vec<i64>, Error> {
        let ids = dedup(ids);
        self.op("mesh_split", |db| {
            let mut out = vec![];
            let mut added = Inv::default();
            let mut back = Inv::default();
            for &id in &ids {
                let src = info(db, id)?;
                let (lo, hi) = mesh_ops::split(&src.mesh, pl, TOL);
                if lo.mesh.triangles.is_empty() || hi.mesh.triangles.is_empty() {
                    continue;
                }
                let (lo, hi) = (capped(lo)?, capped(hi)?);
                back = keep_mesh(db, id, back)?;
                put(db, id, &lo)?;
                let (p, i) = encode(&hi);
                db.execute(
                    "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,?2,?3,?4,?5)",
                    params![src.level, src.category, src.name, p, i],
                )?;
                let n = db.last_insert_rowid();
                db.execute(
                    "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) \
                     SELECT 'mesh', ?1, key, value, unit FROM prop WHERE kind='mesh' AND ref=?2",
                    params![n, id],
                )?;
                added = added
                    .sql(
                        "DELETE FROM prop WHERE kind='mesh' AND ref=?1",
                        vec![Value::Integer(n)],
                    )
                    .delete("mesh", "id", n);
                out.push(n);
            }
            let Some(&first) = out.first() else {
                return invalid("split: plane misses the meshes");
            };
            Ok((out, first, added.then(back)))
        })
    }

    /// Deletes triangles by index (the mesh becomes open).
    pub fn delete_mesh_faces(&mut self, id: i64, faces: &[usize]) -> Result<(), Error> {
        self.edit_meshes("mesh_faces_delete", &[id], |m| {
            Ok(mesh_ops::delete_faces(m, faces))
        })
    }

    /// Deletes triangles whose normal deviates from `dir` by at most
    /// `max_angle_deg` (e.g. all top faces). Returns the number of deleted triangles.
    pub fn delete_mesh_faces_facing(
        &mut self,
        ids: &[i64],
        dir: V3,
        max_angle_deg: f64,
    ) -> Result<usize, Error> {
        let l = dir.len();
        if !(l > 1e-12 && max_angle_deg.is_finite()) {
            return invalid("delete faces: bad direction");
        }
        let (d, cos) = (
            dir * (1.0 / l),
            max_angle_deg.clamp(0.0, 180.0).to_radians().cos(),
        );
        let removed = std::cell::Cell::new(0usize);
        self.edit_meshes("mesh_faces_delete", ids, |m| {
            let faces = facing(m, d, cos);
            if faces.is_empty() {
                return invalid("delete faces: no faces face that way");
            }
            removed.set(removed.get() + faces.len());
            Ok(mesh_ops::delete_faces(m, &faces))
        })?;
        Ok(removed.get())
    }

    /// Merges meshes into the first; the rest are deleted and their external references move to the first.
    pub fn merge_meshes(&mut self, ids: &[i64]) -> Result<i64, Error> {
        let ids = dedup(ids);
        if ids.len() < 2 {
            return invalid("merge: need two meshes");
        }
        self.op("mesh_merge", |db| {
            let rows = ids
                .iter()
                .map(|&i| info(db, i))
                .collect::<Result<Vec<_>, _>>()?;
            let parts: Vec<&Mesh> = rows.iter().map(|r| &r.mesh).collect();
            let m = mesh_ops::merge(&parts);
            let keep = ids[0];
            let mut inv = keep_props(db, "mesh", keep, keep_mesh(db, keep, Inv::default())?)?;
            put(db, keep, &m)?;
            // tag for BIM re-import: the mesh geometry is user-made
            let from = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            db.execute(
                "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES ('mesh',?1,?2,?3,'')",
                params![keep, MERGED_FROM, from],
            )?;
            inv = Inv::default()
                .sql(
                    "DELETE FROM prop WHERE kind='mesh' AND ref=?1 AND key=?2",
                    vec![Value::Integer(keep), Value::Text(MERGED_FROM.into())],
                )
                .then(inv);
            for &o in &ids[1..] {
                inv = repoint(db, ("mesh", o), ("mesh", keep), inv)?;
                inv = drop_mesh(db, o, inv)?;
            }
            Ok((keep, keep, inv))
        })
    }

    /// Welds vertices closer than `tol` and removes degenerate and duplicate triangles.
    pub fn clean_meshes(&mut self, ids: &[i64], tol: f64) -> Result<CleanReport, Error> {
        let ids = dedup(ids);
        if ids.is_empty() {
            return invalid("no meshes");
        }
        let tol = if tol > 0.0 && tol.is_finite() {
            tol
        } else {
            TOL
        };
        self.op("mesh_clean", |db| {
            let mut sum = CleanReport::default();
            let mut inv = Inv::default();
            for &id in &ids {
                let (m, r) = mesh_ops::clean(&info(db, id)?.mesh, tol);
                if r == CleanReport::default() || m.triangles.is_empty() {
                    continue;
                }
                sum.merged_vertices += r.merged_vertices;
                sum.degenerate += r.degenerate;
                sum.duplicate += r.duplicate;
                sum.unused += r.unused;
                sum.t_junctions += r.t_junctions;
                inv = keep_mesh(db, id, inv)?;
                put(db, id, &m)?;
            }
            Ok((sum, ids[0], inv))
        })
    }

    /// Area, volume, closedness, convexity, bounding box.
    pub fn mesh_stats(&self, id: i64) -> Result<Stats, Error> {
        Ok(mesh_ops::stats(&info(&self.db, id)?.mesh, TOL))
    }

    /// Meshes on a level plan: the section at `cut_height` above the level and mesh edges below.
    /// The plan includes the level's meshes and any others that cross the storey by height.
    pub fn mesh_plan(&self, level: i64, cut_height: f64) -> Result<Vec<MeshPlan>, Error> {
        self.mesh_plan_cached(level, cut_height, &mut MeshPlanCache::default())
    }

    /// Like [`Self::mesh_plan`], but sections are kept in `cache` by (mesh, revision, height):
    /// a frame without edits does not re-cut the meshes.
    pub fn mesh_plan_cached(
        &self,
        level: i64,
        cut_height: f64,
        cache: &mut MeshPlanCache,
    ) -> Result<Vec<MeshPlan>, Error> {
        let ls = crate::walls::levels(&self.db)?;
        let Some(&(_, elev)) = ls.iter().find(|l| l.0 == level) else {
            return invalid(format!("no such level {level}"));
        };
        let next = ls
            .iter()
            .map(|l| l.1)
            .filter(|&e| e > elev + 1e-9)
            .fold(f64::INFINITY, f64::min);
        let zc = elev + cut_height;
        let mut out = vec![];
        let mut budget = VIEW_TRIS;
        let mut seen = std::collections::HashSet::new();
        for row in self.mesh_rows(None, None)? {
            seen.insert(row.id);
            if row.triangles > budget {
                continue;
            }
            let e = cache.entries.entry(row.id).or_default();
            if e.sig != Some(row.sig) {
                *e = PlanEntry {
                    sig: Some(row.sig),
                    ..Default::default()
                };
            }
            let mut mesh: Option<Mesh> = None;
            if e.z.is_none() {
                let Some(m) = self.mesh_geometry(row.id)? else {
                    e.z = Some(None);
                    continue;
                };
                e.z = Some(z_range(&m));
                mesh = Some(m);
            }
            let Some(Some((lo, hi))) = e.z else {
                continue;
            };
            if row.level != Some(level) && (hi <= elev || lo >= next) {
                continue;
            }
            budget -= row.triangles;
            // the overhead outline depends on the view-range top as well as on the cut
            let key = zc.to_bits() ^ next.to_bits().rotate_left(29);
            if !e.plans.iter().any(|p| p.0 == key) {
                let m = match mesh.take() {
                    Some(m) => m,
                    None => match self.mesh_geometry(row.id)? {
                        Some(m) => m,
                        None => continue,
                    },
                };
                cache.computed += 1;
                let p = plan_of(row.id, &m, zc, next, (lo, hi), row.triangles);
                if e.plans.len() >= 4 {
                    e.plans.remove(0);
                }
                e.plans.push((key, p));
            }
            if let Some((_, Some(p))) = e.plans.iter().find(|p| p.0 == key) {
                out.push(p.clone());
            }
        }
        cache.entries.retain(|id, _| seen.contains(id));
        Ok(out)
    }

    /// Mesh geometry (None if the row is missing or the BLOB is corrupt).
    fn mesh_geometry(&self, id: i64) -> Result<Option<Mesh>, Error> {
        let r = self
            .db
            .query_row(
                "SELECT positions, indices FROM mesh WHERE id=?1",
                [id],
                |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        Ok(r.and_then(|(p, i)| decode(&p, &i).ok()))
    }
}

/// Mesh plan cache (held by the UI): per mesh, a revision signature, the z range and up to four
/// sections by height.
#[derive(Debug, Default)]
pub struct MeshPlanCache {
    entries: std::collections::HashMap<i64, PlanEntry>,
    computed: usize,
}

impl MeshPlanCache {
    /// Number of mesh plans computed so far (cache misses).
    pub fn computed(&self) -> usize {
        self.computed
    }
}

#[derive(Debug, Default)]
struct PlanEntry {
    sig: Option<(u64, i64)>,
    /// Some(None): the mesh is empty or corrupt.
    z: Option<Option<(f64, f64)>>,
    plans: Vec<(u64, Option<MeshPlan>)>,
}

fn z_range(m: &Mesh) -> Option<(f64, f64)> {
    let (lo, hi) = m
        .positions
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
            (a.min(p.z), b.max(p.z))
        });
    (lo <= hi).then_some((lo, hi))
}

/// Plan of one mesh at height `zc`; None if there is nothing to show on the plan.
fn plan_of(
    id: i64,
    m: &Mesh,
    zc: f64,
    next: f64,
    (lo, hi): (f64, f64),
    tris: usize,
) -> Option<MeshPlan> {
    let mut e = MeshPlan {
        mesh: id,
        ..Default::default()
    };
    if lo <= zc && zc < hi {
        // horizontal plane: the (x, y) basis gives plan coordinates
        let s = mesh_ops::section(m, &Plane::horizontal(zc), TOL);
        for r in &s.regions {
            let pts: Vec<V2> = r
                .outer
                .iter()
                .chain(r.holes.iter().flatten())
                .copied()
                .collect();
            if let Some(t) = mesh_ops::triangulate_region(&r.outer, &r.holes) {
                e.fill
                    .extend(t.iter().map(|&[a, b, c]| [pts[a], pts[b], pts[c]]));
            }
        }
        e.cut = s.regions;
        e.open = s.open;
    } else if hi <= zc {
        // smooth bodies have almost no feature edges: the outline carries their shape
        e.beyond = plan_hull(m);
        if tris <= EDGE_TRIS {
            e.below = mesh_ops::edges(m, EDGE_COS, TOL)
                .into_iter()
                .map(|(a, b)| (v2(a.x, a.y), v2(b.x, b.y)))
                .filter(|(a, b)| (*b - *a).len() > 1e-9)
                .collect();
        }
    } else if lo < next {
        e.beyond = plan_hull(m);
        e.overhead = true;
    }
    let empty = e.cut.is_empty() && e.open.is_empty() && e.below.is_empty() && e.beyond.is_empty();
    (!empty).then_some(e)
}

/// Convex hull of the mesh projected to the plan (counter-clockwise, monotone chain).
fn plan_hull(m: &Mesh) -> Vec<V2> {
    let mut p: Vec<V2> = m
        .positions
        .iter()
        .filter(|q| q.x.is_finite() && q.y.is_finite())
        .map(|q| v2(q.x, q.y))
        .collect();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup_by(|a, b| (*a - *b).len() <= TOL);
    if p.len() < 3 {
        return p;
    }
    let turn = |a: V2, b: V2, c: V2| (b - a).cross(c - a);
    let mut h: Vec<V2> = Vec::with_capacity(p.len() + 1);
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &V2>> = if pass == 0 {
            Box::new(p.iter())
        } else {
            Box::new(p.iter().rev())
        };
        for &q in it {
            while h.len() >= start + 2 && turn(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    h
}

/// Segment p→q clipped to the band lo ≤ d ≤ hi (d is linear).
fn clip(p: V3, q: V3, d: &dyn Fn(V3) -> f64, lo: f64, hi: f64) -> Option<(V3, V3)> {
    let (dp, dq) = (d(p), d(q));
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (bound, sign) in [(lo, 1.0), (hi, -1.0)] {
        // sign·(d(t) − bound) ≥ 0
        let f0 = sign * (dp - bound);
        let fd = sign * (dq - dp);
        if fd.abs() < 1e-15 {
            if f0 < 0.0 {
                return None;
            }
            continue;
        }
        let t = -f0 / fd;
        if fd > 0.0 {
            t0 = t0.max(t);
        } else {
            t1 = t1.min(t);
        }
    }
    (t1 - t0 > 1e-12).then(|| (p + (q - p) * t0, p + (q - p) * t1))
}

/// Meshes in a section/elevation along a→u: the section by plane d = 0 (fill) and edges in the band
/// d ∈ [0, far] (d is to the left of the view line). Coordinates (s, z), like other view elements.
pub(crate) fn section_prims(
    db: &Connection,
    a: V2,
    u: V2,
    far: Option<f64>,
    is_cut: bool,
    prims: &mut Vec<ViewPrim>,
    grow: &mut dyn FnMut(V2),
) -> Result<(), Error> {
    if !has_table(db, "mesh")? {
        return Ok(());
    }
    let Some(mut c) = Cutter::new(a, u, far, is_cut, prims) else {
        return Ok(());
    };
    let mut st = db.prepare("SELECT id, positions, indices FROM mesh ORDER BY id")?;
    let mut rows = st.query([])?;
    let mut budget = VIEW_TRIS;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let idx: Vec<u8> = r.get(2)?;
        let tris = idx.len() / 12;
        if tris > budget {
            continue;
        }
        let Ok(m) = decode(&r.get::<_, Vec<u8>>(1)?, &idx) else {
            continue;
        };
        if c.add("mesh", id, "", &m, prims, grow) {
            budget -= tris;
        }
    }
    Ok(())
}

/// Other bodies (stairs) cut and seen the same way as stored meshes: (table, id, material, mesh).
pub(crate) fn section_bodies(
    bodies: &[(&'static str, i64, String, Mesh)],
    a: V2,
    u: V2,
    far: Option<f64>,
    is_cut: bool,
    prims: &mut Vec<ViewPrim>,
    grow: &mut dyn FnMut(V2),
) {
    if let Some(mut c) = Cutter::new(a, u, far, is_cut, prims) {
        for (t, id, mat, m) in bodies {
            c.add(t, *id, mat, m, prims, grow);
        }
    }
}

/// Section plane along a→u with the fills drawn so far, which hide edges behind them.
struct Cutter {
    a: V2,
    u: V2,
    n: V2,
    pl: Plane,
    far: f64,
    is_cut: bool,
    occ: Vec<Occluder>,
}

impl Cutter {
    fn new(a: V2, u: V2, far: Option<f64>, is_cut: bool, prims: &[ViewPrim]) -> Option<Self> {
        Some(Self {
            a,
            u,
            n: v2(-u.y, u.x),
            pl: Plane::vertical(a, a + u)?,
            far: far.unwrap_or(f64::INFINITY),
            is_cut,
            // cut fills (of every element) hide the edges behind the plane
            occ: prims.iter().filter_map(Occluder::of).collect(),
        })
    }

    /// Cut fill and seen edges of one body; false when it lies wholly outside the view depth.
    fn add(
        &mut self,
        table: &'static str,
        id: i64,
        material: &str,
        m: &Mesh,
        prims: &mut Vec<ViewPrim>,
        grow: &mut dyn FnMut(V2),
    ) -> bool {
        let (a, u, n, far, is_cut) = (self.a, self.u, self.n, self.far, self.is_cut);
        let pl = &self.pl;
        let occ = &mut self.occ;
        let sz = |p: V3| v2((v2(p.x, p.y) - a).dot(u), p.z);
        let dd = |p: V3| (v2(p.x, p.y) - a).dot(n);
        let tris = m.triangles.len();
        let (lo, hi) = m
            .positions
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(x, y), p| {
                (x.min(dd(*p)), y.max(dd(*p)))
            });
        if hi < 0.0 || lo > far {
            return false;
        }
        let cut = is_cut && lo < 0.0 && hi > 0.0;
        if cut {
            let s = mesh_ops::section(m, pl, TOL);
            let ring = |q: &[V2], ccw: bool| -> Vec<(f64, f64)> {
                let mut v: Vec<V2> = q.iter().map(|&q| sz(pl.to_3d(q))).collect();
                if (signed_area(&v) > 0.0) != ccw {
                    v.reverse();
                }
                v.iter().map(|p| (p.x, p.y)).collect()
            };
            for rg in &s.regions {
                let outer = ring(&rg.outer[..], true);
                outer.iter().for_each(|&(x, y)| grow(v2(x, y)));
                let p = ViewPrim::SectionCut {
                    table,
                    element: id,
                    material: material.to_string(),
                    rings: outer,
                    holes: rg.holes.iter().map(|h| ring(&h[..], false)).collect(),
                };
                occ.extend(Occluder::of(&p));
                prims.push(p);
            }
            for l in &s.open {
                for w in l.windows(2) {
                    let (p, q) = (sz(pl.to_3d(w[0])), sz(pl.to_3d(w[1])));
                    grow(p);
                    grow(q);
                    prims.push(ViewPrim::Edge {
                        table,
                        element: id,
                        class: LineClass::Projection,
                        a: (p.x, p.y),
                        b: (q.x, q.y),
                    });
                }
            }
        }
        if tris > EDGE_TRIS {
            return true;
        }
        let class = if cut || !is_cut {
            LineClass::Projection
        } else {
            LineClass::Beyond
        };
        for (p, q) in mesh_ops::edges(m, EDGE_COS, TOL) {
            let Some((p, q)) = clip(p, q, &dd, 0.0, far) else {
                continue;
            };
            let (p, q) = (sz(p), sz(q));
            if (q - p).len() < 1e-9 {
                continue;
            }
            for (p, q) in visible_parts(p, q, occ) {
                grow(p);
                grow(q);
                prims.push(ViewPrim::Edge {
                    table,
                    element: id,
                    class,
                    a: (p.x, p.y),
                    b: (q.x, q.y),
                });
            }
        }
        true
    }
}

/// Section fill in view coordinates with its bounding box.
struct Occluder {
    outer: Vec<V2>,
    holes: Vec<Vec<V2>>,
    lo: V2,
    hi: V2,
}

impl Occluder {
    fn of(p: &ViewPrim) -> Option<Occluder> {
        let ViewPrim::SectionCut { rings, holes, .. } = p else {
            return None;
        };
        let pts = |r: &[(f64, f64)]| r.iter().map(|&(x, y)| v2(x, y)).collect::<Vec<_>>();
        let outer = pts(rings);
        if outer.len() < 3 {
            return None;
        }
        let (mut lo, mut hi) = (outer[0], outer[0]);
        for q in &outer {
            lo = v2(lo.x.min(q.x), lo.y.min(q.y));
            hi = v2(hi.x.max(q.x), hi.y.max(q.y));
        }
        Some(Occluder {
            outer,
            holes: holes.iter().map(|h| pts(h)).collect(),
            lo,
            hi,
        })
    }

    /// Strictly inside the fill (the boundary and holes are visible).
    fn hides(&self, p: V2) -> bool {
        use ddc_geom::predicates::point_in_ring;
        p.x > self.lo.x
            && p.x < self.hi.x
            && p.y > self.lo.y
            && p.y < self.hi.y
            && point_in_ring(p, &self.outer) == 1
            && self.holes.iter().all(|h| point_in_ring(p, h) == -1)
    }
}

/// Parts of a segment outside the section fills: 8 portions, visibility judged at each portion's
/// midpoint, adjacent visible portions are merged.
fn visible_parts(p: V2, q: V2, occ: &[Occluder]) -> Vec<(V2, V2)> {
    const N: usize = 8;
    if occ.is_empty() {
        return vec![(p, q)];
    }
    let at = |t: f64| p + (q - p) * t;
    let mut out: Vec<(V2, V2)> = vec![];
    let mut open: Option<V2> = None;
    for k in 0..N {
        let (t0, t1) = (k as f64 / N as f64, (k + 1) as f64 / N as f64);
        let m = at((t0 + t1) / 2.0);
        let shown = !occ.iter().any(|o| o.hides(m));
        match (shown, open) {
            (true, None) => open = Some(at(t0)),
            (false, Some(s)) => {
                out.push((s, at(t0)));
                open = None;
            }
            _ => {}
        }
    }
    if let Some(s) = open {
        out.push((s, q));
    }
    out
}

#[cfg(test)]
#[path = "mesh_edit_tests.rs"]
mod tests;
