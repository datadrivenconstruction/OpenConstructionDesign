// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Components (schema 15): families (`comp_family`, definition text of ddc_geom::component),
//! types (`comp_type`, values `Key=value, …`), instances (`comp`) with a host: wall (x along the
//! axis, y to the left of the axis, z from the wall base, rotation from the wall direction), slab (z from the top),
//! ceiling (z from the bottom). `comp_qty` is derived: world position, volume, area, bounding box.
//! Identical (type, values) share one mesh: the scene is shared meshes + instance matrices.
//!
//! Doors and windows (migration plan, not done): `opening` stays a cut in the wall, while the infill
//! becomes a component of category door/window with a wall host and Width/Height/Sill parameters
//! tied to the opening; the cut is the family's `void` body. Schema migration: one type per
//! (kind, width, height), one instance per opening, `opening` stores a reference to `comp`; IFC uses the same
//! IfcDoor/IfcWindow with `DDC_Component`. Old files open unchanged until migrated.

use std::sync::atomic::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use ddc_geom::arc::ArcAxis;
use ddc_geom::component::{ddcf, Built, Family, Hosting, Kind, Lod, Resolved, Value};
use ddc_geom::mesh::Mesh;
use ddc_geom::{v2, Tol};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::views::{LineClass, ViewPrim};
use crate::{Error, Model};

/// Built-in library: families separated by a `---` line.
pub const LIBRARY: &str = include_str!("../data/library.ddcf");

/// Texts of the built-in library families.
pub fn library_sources() -> Vec<&'static str> {
    LIBRARY
        .split("\n---")
        .map(|s| s.trim_start_matches('\n'))
        .filter(|s| s.lines().any(|l| l.trim_start().starts_with("family ")))
        .collect()
}

/// Built-in library families.
pub fn library() -> Result<Vec<Family>, Error> {
    library_sources()
        .into_iter()
        .map(|s| Family::parse(s).map_err(|e| Error::Invalid(e.to_string())))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompHost {
    None,
    Wall(i64),
    Floor(i64),
    Ceiling(i64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompSpec {
    /// For an instance with a host, the host's level (filled in).
    pub level: i64,
    pub ctype: i64,
    /// Without a host: x, y in plan; with a wall host: along the axis from the start, to the left of the axis.
    pub at: (f64, f64, f64),
    /// Radians, counter-clockwise; for a wall, from its direction.
    pub rotation: f64,
    /// Mirror about local x before rotation.
    pub mirror: bool,
    /// Tilt in the instance's own frame (`Placement::tilt`); `None` keeps it upright.
    pub tilt: Option<[f64; 4]>,
    pub host: CompHost,
    /// Instance parameter values.
    pub values: Vec<(String, Value)>,
}

impl CompSpec {
    pub fn at(level: i64, ctype: i64, x: f64, y: f64) -> Self {
        CompSpec {
            level,
            ctype,
            at: (x, y, 0.0),
            rotation: 0.0,
            mirror: false,
            tilt: None,
            host: CompHost::None,
            values: vec![],
        }
    }
}

/// World placement: insertion point, rotation, mirror.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub origin: (f64, f64, f64),
    pub rotation: f64,
    pub mirror: bool,
    /// Tilt in the instance's own frame (unit quaternion x, y, z, w), applied before the plan
    /// turn: world = Rz(rotation) * tilt * mirror. `None`: upright (every older instance).
    pub tilt: Option<[f64; 4]>,
}

/// Rotation matrix of a quaternion (x, y, z, w), normalized first; rows.
pub fn quat_matrix(q: [f64; 4]) -> [[f64; 3]; 3] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    let [x, y, z, w] = if n > 1e-300 { q.map(|v| v / n) } else { [0.0, 0.0, 0.0, 1.0] };
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

/// Unit quaternion (x, y, z, w) of a rotation matrix (rows), w >= 0.
pub fn matrix_quat(m: [[f64; 3]; 3]) -> [f64; 4] {
    let t = m[0][0] + m[1][1] + m[2][2];
    let q = if t > 0.0 {
        let s = (t + 1.0).sqrt() * 2.0;
        [(m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s, 0.25 * s]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [0.25 * s, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s, (m[2][1] - m[1][2]) / s]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [(m[0][1] + m[1][0]) / s, 0.25 * s, (m[1][2] + m[2][1]) / s, (m[0][2] - m[2][0]) / s]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [(m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, 0.25 * s, (m[1][0] - m[0][1]) / s]
    };
    let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    let k = if q[3] < 0.0 { -1.0 / n } else { 1.0 / n };
    q.map(|v| v * k)
}

/// Stored text of a tilt: `x y z w`.
pub(crate) fn tilt_text(t: Option<[f64; 4]>) -> Option<String> {
    t.map(|q| q.map(|v| v.to_string()).join(" "))
}

pub(crate) fn tilt_parse(s: Option<String>) -> Option<[f64; 4]> {
    let v: Vec<f64> = s?.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    (v.len() == 4 && v.iter().all(|x| x.is_finite()) && v.iter().any(|x| *x != 0.0))
        .then(|| [v[0], v[1], v[2], v[3]])
}

impl Placement {
    /// 3×4 matrix, row-major: world = M·(x, y, z, 1).
    pub fn matrix(&self) -> [f64; 12] {
        let (s, c) = self.rotation.sin_cos();
        let k = if self.mirror { -1.0 } else { 1.0 };
        let o = self.origin;
        let rz = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
        let t = self.tilt.map_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], quat_matrix);
        let mut m = [0.0; 12];
        for i in 0..3 {
            for j in 0..3 {
                let v: f64 = (0..3).map(|l| rz[i][l] * t[l][j]).sum();
                m[i * 4 + j] = if j == 0 { k * v } else { v };
            }
        }
        m[3] = o.0;
        m[7] = o.1;
        m[11] = o.2;
        m
    }
    pub fn apply(&self, p: (f64, f64, f64)) -> (f64, f64, f64) {
        let m = self.matrix();
        (
            m[0] * p.0 + m[1] * p.1 + m[2] * p.2 + m[3],
            m[4] * p.0 + m[5] * p.1 + m[6] * p.2 + m[7],
            m[8] * p.0 + m[9] * p.1 + m[10] * p.2 + m[11],
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompQty {
    pub placement: Placement,
    /// Exact volume of bodies minus cuts, m³.
    pub volume: f64,
    /// Surface area, m².
    pub surface: f64,
    /// World bounding box (min, max).
    pub bbox: [(f64, f64, f64); 2],
}

/// Component schedule row.
#[derive(Clone, Debug, PartialEq)]
pub struct ScheduleRow {
    pub id: i64,
    pub family: String,
    pub type_name: String,
    pub category: &'static str,
    pub level: i64,
    pub host: CompHost,
    pub volume: f64,
    pub surface: f64,
    /// All parameters in definition order: (name, kind, value in SI or text).
    pub params: Vec<(String, Kind, Value)>,
}

/// Scene instance: shared mesh index and placement.
#[derive(Clone, Debug, PartialEq)]
pub struct CompInstance {
    pub id: i64,
    pub family: i64,
    pub ctype: i64,
    pub mesh: usize,
    pub placement: Placement,
    /// Uniform size of the shared mesh at this placement: a plant's `Scale`, 1 for the rest.
    pub scale: f64,
}

impl CompInstance {
    /// 3x4 matrix, row-major, with the instance scale: world = M·(x, y, z, 1).
    pub fn matrix(&self) -> [f64; 12] {
        let mut m = self.placement.matrix();
        for r in 0..3 {
            for c in 0..3 {
                m[r * 4 + c] *= self.scale;
            }
        }
        m
    }

    pub fn apply(&self, p: (f64, f64, f64)) -> (f64, f64, f64) {
        let s = self.scale;
        self.placement.apply((p.0 * s, p.1 * s, p.2 * s))
    }
}

const PLANT_LOD: &str = include_str!("../data/plant_lod.json");

/// Crowd thresholds of `data/plant_lod.json`: plants past `medium_over` are built no finer than
/// medium and stay so down to `medium_under`; the same pair for coarse.
struct Crowd2 {
    medium: (usize, usize),
    coarse: (usize, usize),
}

struct PlantLod {
    scene: Crowd2,
    plan: Crowd2,
}

/// Where the plants are drawn: the 3D scene, whose limit is the triangle budget (near trees stay
/// fine up to it, the renderer drops far ones to coarser levels by distance), or the plan, whose
/// limit is the symbol segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Crowd {
    Scene,
    Plan,
}

fn plant_lod() -> &'static PlantLod {
    static T: std::sync::OnceLock<PlantLod> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(PLANT_LOD).expect("data/plant_lod.json is not JSON");
        let one = |k: &str| {
            let n = |f: &str| v[k][f].as_u64().unwrap_or_else(|| panic!("data/plant_lod.json: `{k}.{f}` is not a count")) as usize;
            let t = Crowd2 { medium: (n("medium_over"), n("medium_under")), coarse: (n("coarse_over"), n("coarse_under")) };
            assert!(
                t.medium.1 <= t.medium.0 && t.medium.0 < t.coarse.1 && t.coarse.1 <= t.coarse.0,
                "data/plant_lod.json: `{k}` thresholds out of order"
            );
            t
        };
        PlantLod { scene: one("scene"), plan: one("plan") }
    })
}

/// Whether the family grows a plant: one with leaf lumps, a tree to be drawn in a crowd.
fn is_plant(f: &Family) -> bool {
    f.solids.iter().any(|s| matches!(s.shape, ddc_geom::component::Shape::Plant { .. }))
}

/// The level a plant is built at among `plants` of them, and the rank (0 as asked, 1 medium,
/// 2 coarse) the caller keeps for the next call: a rank is entered past its `over` count and left
/// below its `under` count, so a count hovering at a limit does not flip the drawing.
pub fn plant_level(lod: Lod, plants: usize, last: u8, kind: Crowd) -> (Lod, u8) {
    let t = plant_lod();
    let t = if kind == Crowd::Scene { &t.scene } else { &t.plan };
    let enter = |r: u8, (over, under): (usize, usize)| if last >= r { plants >= under } else { plants > over };
    let rank = if enter(2, t.coarse) {
        2
    } else if enter(1, t.medium) {
        1
    } else {
        0
    };
    let out = match rank {
        2 => Lod::Coarse,
        1 if lod == Lod::Fine => Lod::Medium,
        _ => lod,
    };
    (out, rank)
}

/// Name of the instance parameter that sizes a whole plant.
const PLANT_SCALE: &str = "Scale";

/// Instance values of a plant built at unit size, and the size it is placed at: `Scale` goes to
/// the placement and every instance length is divided by it, so trees of one type share a mesh.
/// Plant bodies are drawn in lengths times `Scale`, which makes the two the same tree.
pub fn plant_unit_values(
    f: &Family,
    iv: &[(String, ddc_geom::component::Value)],
) -> (Vec<(String, ddc_geom::component::Value)>, f64) {
    use ddc_geom::component::{Kind, Value};
    let s = match iv.iter().find(|(k, _)| k == PLANT_SCALE) {
        Some((_, Value::Num(s))) if s.is_finite() && *s > 1e-6 => *s,
        _ => return (iv.to_vec(), 1.0),
    };
    if !is_plant(f) || !f.param(PLANT_SCALE).is_some_and(|p| p.instance) {
        return (iv.to_vec(), 1.0);
    }
    let out = iv
        .iter()
        .map(|(k, v)| match v {
            _ if k == PLANT_SCALE => (k.clone(), Value::Num(1.0)),
            Value::Num(x) if f.param(k).is_some_and(|p| p.kind == Kind::Length) => {
                (k.clone(), Value::Num(x / s))
            }
            _ => (k.clone(), v.clone()),
        })
        .collect();
    (out, s)
}

/// Plants among the placed instances: types whose family grows a plant, by instance.
fn plant_count(cache: &mut Cache, db: &Connection, ctypes: impl Iterator<Item = i64>) -> usize {
    let mut by: HashMap<i64, usize> = HashMap::new();
    for c in ctypes {
        *by.entry(c).or_default() += 1;
    }
    by.into_iter().filter(|(c, _)| cache.ctype(db, *c).is_ok_and(|(f, _)| is_plant(&f))).map(|(_, n)| n).sum()
}

/// Coarser levels of a plant: the same tree with fewer, larger pieces. The error is a share of
/// the crown width, what the lathed envelope and the clusters can differ by.
fn coarser(
    f: &Family,
    r: &ddc_geom::component::Resolved,
    lod: Lod,
    full: &ddc_geom::component::Built,
) -> Vec<(Arc<Mesh>, Vec<String>, f64)> {
    let crown = full.bbox[1].x - full.bbox[0].x;
    if !is_plant(f) {
        return vec![];
    }
    let levels: &[(Lod, f64)] = match lod {
        Lod::Fine => &[(Lod::Medium, 0.03), (Lod::Coarse, 0.08)],
        Lod::Medium => &[(Lod::Coarse, 0.08)],
        Lod::Coarse => &[],
    };
    levels
        .iter()
        .filter_map(|(l, e)| {
            let b = f.build(r, *l).ok()?;
            let mats = b.parts.iter().map(|p| p.material.clone()).collect();
            Some((Arc::new(b.mesh), mats, e * crown.max(1.0)))
        })
        .collect()
}

/// Component scene: each mesh is built once per (family, values).
#[derive(Clone, Debug, Default)]
pub struct CompScene {
    pub meshes: Vec<Arc<Mesh>>,
    /// Material of every part of `meshes[k]`, indexed like `Mesh::face_of`.
    pub materials: Vec<Vec<String>>,
    /// Coarser pictures of `meshes[k]` (plants only): mesh, its materials per part and the
    /// error against the full body in metres, finest first.
    pub lods: Vec<Vec<(Arc<Mesh>, Vec<String>, f64)>>,
    pub instances: Vec<CompInstance>,
}

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn fam_err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("component: {e}"))
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    Ok(db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

/// Parsed families and types for the duration of one pass.
#[derive(Default)]
struct Cache {
    fams: HashMap<i64, Arc<Family>>,
    types: HashMap<i64, (i64, Arc<Vec<(String, Value)>>)>,
    resolved: HashMap<(i64, String), Option<Arc<Resolved>>>,
}

impl Cache {
    fn family(&mut self, db: &Connection, id: i64) -> Result<Arc<Family>, Error> {
        if let Some(f) = self.fams.get(&id) {
            return Ok(f.clone());
        }
        let def: String = db
            .query_row("SELECT def FROM comp_family WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no component family {id}")))?;
        let f = Arc::new(Family::parse(&def).map_err(fam_err)?);
        self.fams.insert(id, f.clone());
        Ok(f)
    }

    fn ctype(
        &mut self,
        db: &Connection,
        id: i64,
    ) -> Result<(Arc<Family>, Arc<Vec<(String, Value)>>), Error> {
        if let Some((fid, v)) = self.types.get(&id).cloned() {
            return Ok((self.family(db, fid)?, v));
        }
        let (fid, vals): (i64, String) = db
            .query_row(
                "SELECT family, vals FROM comp_type WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no component type {id}")))?;
        let f = self.family(db, fid)?;
        let v = Arc::new(f.assignments(&vals).map_err(fam_err)?);
        self.types.insert(id, (fid, v.clone()));
        Ok((f, v))
    }

    /// Instance values of type `ctype` with value text `vals`; None if it cannot be built.
    fn resolve(
        &mut self,
        db: &Connection,
        ctype: i64,
        vals: &str,
    ) -> Result<Option<Arc<Resolved>>, Error> {
        let key = (ctype, vals.to_string());
        if let Some(r) = self.resolved.get(&key) {
            return Ok(r.clone());
        }
        let (f, tv) = self.ctype(db, ctype)?;
        let r = f
            .assignments(vals)
            .ok()
            .and_then(|iv| f.resolve(&tv, &iv).ok())
            .map(Arc::new);
        self.resolved.insert(key, r.clone());
        Ok(r)
    }
}

/// Instance row with host data.
struct Row {
    id: i64,
    #[allow(dead_code)]
    level: i64,
    ctype: i64,
    vals: String,
    x: f64,
    y: f64,
    z: f64,
    rot: f64,
    mirror: bool,
    host: CompHost,
    level_z: f64,
    /// Wall: (a, b, bulge, base z).
    wall: Option<((f64, f64), (f64, f64), f64, f64)>,
    /// z of the slab top or ceiling bottom.
    face_z: Option<f64>,
    tilt: Option<[f64; 4]>,
}

const ROW_SQL: &str =
    "SELECT c.id, c.level, c.ctype, c.vals, c.x, c.y, c.z, c.rotation, c.mirror, \
     c.host_wall, c.host_slab, c.host_ceiling, l.elevation, \
     w.ax, w.ay, w.bx, w.by, w.bulge, wl.elevation + w.base_offset, \
     sl.elevation + s.offset, cl.elevation + ce.height, c.tilt \
     FROM comp c JOIN level l ON l.id=c.level \
     LEFT JOIN wall w ON w.id=c.host_wall LEFT JOIN level wl ON wl.id=w.level \
     LEFT JOIN slab s ON s.id=c.host_slab LEFT JOIN level sl ON sl.id=s.level \
     LEFT JOIN ceiling ce ON ce.id=c.host_ceiling LEFT JOIN level cl ON cl.id=ce.level";

fn rows(db: &Connection, filter: &str, arg: Option<i64>) -> Result<Vec<Row>, Error> {
    let sql = format!("{ROW_SQL} {filter} ORDER BY c.id");
    let mut st = db.prepare_cached(&sql)?;
    let map = |r: &rusqlite::Row| -> rusqlite::Result<Option<Row>> {
        let (hw, hs, hc): (Option<i64>, Option<i64>, Option<i64>) =
            (r.get(9)?, r.get(10)?, r.get(11)?);
        let host = match (hw, hs, hc) {
            (Some(w), _, _) => CompHost::Wall(w),
            (_, Some(s), _) => CompHost::Floor(s),
            (_, _, Some(c)) => CompHost::Ceiling(c),
            _ => CompHost::None,
        };
        let wall = match r.get::<_, Option<f64>>(13)? {
            Some(ax) => Some((
                (ax, r.get(14)?),
                (r.get(15)?, r.get(16)?),
                r.get(17)?,
                r.get::<_, Option<f64>>(18)?.unwrap_or(f64::NAN),
            )),
            None => None,
        };
        let face_z: Option<f64> = match host {
            CompHost::Floor(_) => r.get(19)?,
            CompHost::Ceiling(_) => r.get(20)?,
            _ => None,
        };
        // dangling host reference: the instance is not built (the integrity check will report it)
        let dangling = match host {
            CompHost::Wall(_) => wall.is_none(),
            CompHost::Floor(_) | CompHost::Ceiling(_) => face_z.is_none(),
            CompHost::None => false,
        };
        if dangling {
            return Ok(None);
        }
        Ok(Some(Row {
            id: r.get(0)?,
            level: r.get(1)?,
            ctype: r.get(2)?,
            vals: r.get(3)?,
            x: r.get(4)?,
            y: r.get(5)?,
            z: r.get(6)?,
            rot: r.get(7)?,
            mirror: r.get::<_, i64>(8)? != 0,
            host,
            level_z: r.get(12)?,
            wall,
            face_z,
            tilt: tilt_parse(r.get(21)?),
        }))
    };
    let it = match arg {
        Some(a) => st.query_map([a], map)?.collect::<Result<Vec<_>, _>>()?,
        None => st.query_map([], map)?.collect::<Result<Vec<_>, _>>()?,
    };
    Ok(it.into_iter().flatten().collect())
}

fn placement(r: &Row) -> Option<Placement> {
    let p = match (r.host, r.wall) {
        (CompHost::Wall(_), Some((a, b, bulge, zb))) => {
            let (o, t) = if bulge == 0.0 {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let l = dx.hypot(dy);
                if l < 1e-12 {
                    return None;
                }
                let t = (dx / l, dy / l);
                ((a.0 + t.0 * r.x, a.1 + t.1 * r.x), t)
            } else {
                let ax = ArcAxis::new(v2(a.0, a.1), v2(b.0, b.1), bulge, Tol::default()).ok()?;
                let (p, t) = (ax.point_at(r.x), ax.tangent_at(r.x));
                ((p.x, p.y), (t.x, t.y))
            };
            let n = (-t.1, t.0);
            Placement {
                origin: (o.0 + n.0 * r.y, o.1 + n.1 * r.y, zb + r.z),
                rotation: t.1.atan2(t.0) + r.rot,
                mirror: r.mirror,
                tilt: r.tilt,
            }
        }
        _ => Placement {
            origin: (r.x, r.y, r.face_z.unwrap_or(r.level_z) + r.z),
            rotation: r.rot,
            mirror: r.mirror,
            tilt: r.tilt,
        },
    };
    [p.origin.0, p.origin.1, p.origin.2, p.rotation]
        .iter()
        .all(|v| v.is_finite())
        .then_some(p)
}

fn world_bbox(p: &Placement, b: &Built) -> [(f64, f64, f64); 2] {
    let (lo, hi) = (b.bbox[0], b.bbox[1]);
    let mut out = [
        (f64::MAX, f64::MAX, f64::MAX),
        (f64::MIN, f64::MIN, f64::MIN),
    ];
    for k in 0..8 {
        let q = (
            if k & 1 == 0 { lo.x } else { hi.x },
            if k & 2 == 0 { lo.y } else { hi.y },
            if k & 4 == 0 { lo.z } else { hi.z },
        );
        let w = p.apply(q);
        out[0] = (out[0].0.min(w.0), out[0].1.min(w.1), out[0].2.min(w.2));
        out[1] = (out[1].0.max(w.0), out[1].1.max(w.1), out[1].2.max(w.2));
    }
    out
}

/// Recomputes `comp_qty`: the body is built once per (type, instance values).
pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    db.execute("DELETE FROM comp_qty", [])?;
    if db.query_row("SELECT count(*) FROM comp", [], |r| r.get::<_, i64>(0))? == 0 {
        return Ok(());
    }
    write_qty(db, rows(db, "", None)?)
}

/// Recomputes `comp_qty` for these instances only (partial recompute, `regen`).
pub(crate) fn regenerate_ids(
    db: &Connection,
    ids: &std::collections::BTreeSet<i64>,
) -> Result<(), Error> {
    let mut rs = vec![];
    for &id in ids {
        db.execute("DELETE FROM comp_qty WHERE comp=?1", [id])?;
        rs.extend(rows(db, "WHERE c.id=?1", Some(id))?);
    }
    write_qty(db, rs)
}

fn write_qty(db: &Connection, rs: Vec<Row>) -> Result<(), Error> {
    let mut cache = Cache::default();
    let mut built: HashMap<(i64, String), Option<Arc<Built>>> = HashMap::new();
    let mut ins = db.prepare_cached(
        "INSERT INTO comp_qty VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
    )?;
    for r in rs {
        let Some(p) = placement(&r) else { continue };
        let key = (r.ctype, r.vals.clone());
        let b = match built.get(&key) {
            Some(b) => b.clone(),
            None => {
                let b = match cache.resolve(db, r.ctype, &r.vals) {
                    Ok(Some(res)) => {
                        let (f, _) = cache.ctype(db, r.ctype)?;
                        f.build(&res, Lod::Medium).ok().map(Arc::new)
                    }
                    _ => None,
                };
                built.insert(key, b.clone());
                b
            }
        };
        let Some(b) = b else { continue };
        let bb = world_bbox(&p, &b);
        ins.execute(params![
            r.id,
            p.origin.0,
            p.origin.1,
            p.origin.2,
            p.rotation,
            p.mirror as i64,
            b.volume,
            b.surface,
            bb[0].0,
            bb[0].1,
            bb[0].2,
            bb[1].0,
            bb[1].1,
            bb[1].2
        ])?;
    }
    Ok(())
}

/// Snapshots and deletes the instances held by a host (`col` = host_wall | host_slab | host_ceiling).
pub(crate) fn drop_hosted(db: &Connection, inv: Inv, col: &str, id: i64) -> Result<Inv, Error> {
    let inv = inv.restore(db, "comp", col, id)?;
    db.execute(&format!("DELETE FROM comp WHERE {col}=?1"), [id])?;
    Ok(inv)
}

/// Family by text within an open transaction (import): same name and text yields the existing one,
/// same name with different text yields a new name "Name (2)".
pub fn family_into(db: &Connection, def: &str) -> Result<i64, Error> {
    let f = Family::parse(def).map_err(fam_err)?;
    let mut name = f.name.clone();
    for k in 2.. {
        let same: Option<(i64, String)> = db
            .query_row(
                "SELECT id, def FROM comp_family WHERE name=?1",
                [&name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match same {
            Some((id, d)) if d == def => return Ok(id),
            Some(_) => name = format!("{} ({k})", f.name),
            None => break,
        }
    }
    let def = if name == f.name {
        def.to_string()
    } else {
        rename(def, &name)
    };
    let def = unique_uid(db, &def)?;
    let f = Family::parse(&def).map_err(fam_err)?;
    db.execute(
        "INSERT INTO comp_family(name, category, host, def) VALUES (?1,?2,?3,?4)",
        params![f.name, f.category.name(), f.hosting.name(), def],
    )?;
    let id = db.last_insert_rowid();
    for t in &f.types {
        db.execute(
            "INSERT INTO comp_type(family, name, vals) VALUES (?1,?2,?3)",
            params![id, t.name, f.assignments_text(&t.values)],
        )?;
    }
    Ok(id)
}

/// One family per `uid` in a project: a text whose uid another family already holds (a copy
/// saved under a new name) gets a fresh uid derived from its own text.
fn unique_uid(db: &Connection, def: &str) -> Result<String, Error> {
    let Some(uid) = Family::parse(def).ok().and_then(|f| f.meta.uid) else {
        return Ok(def.to_string());
    };
    let taken = |u: &str| -> Result<bool, Error> {
        let mut st = db.prepare("SELECT def FROM comp_family WHERE instr(def, ?1) > 0")?;
        let defs = st.query_map([format!("uid {u}")], |r| r.get::<_, String>(0))?;
        for d in defs {
            if Family::parse(&d?).is_ok_and(|f| f.meta.uid.as_deref() == Some(u)) {
                return Ok(true);
            }
        }
        Ok(false)
    };
    if !taken(&uid)? {
        return Ok(def.to_string());
    }
    let mut seed = def.to_string();
    loop {
        let fresh = ddcf::uid_for(&seed);
        if !taken(&fresh)? {
            return set_uid(def, &fresh);
        }
        seed.push('\n');
    }
}

/// `def` with its own `uid` statement replaced; an error when it has none, so a duplicate uid
/// is never saved silently.
pub(crate) fn set_uid(def: &str, uid: &str) -> Result<String, Error> {
    ddcf::replace_uid(def, uid)
        .ok_or_else(|| Error::Invalid("component: cannot replace the uid of a family text".into()))
}

fn rename(def: &str, name: &str) -> String {
    let mut out = vec![];
    let mut done = false;
    for l in def.lines() {
        if !done && l.trim_start().starts_with("family ") {
            out.push(format!("family {name}"));
            done = true;
        } else {
            out.push(l.to_string());
        }
    }
    out.join("\n") + "\n"
}

/// Family type by name and values (import): a match yields the existing one, otherwise a new one.
pub fn type_into(db: &Connection, family: i64, name: &str, vals: &str) -> Result<i64, Error> {
    let mut n = name.to_string();
    for k in 2.. {
        let same: Option<(i64, String)> = db
            .query_row(
                "SELECT id, vals FROM comp_type WHERE family=?1 AND name=?2",
                params![family, n],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match same {
            Some((id, v)) if v == vals => return Ok(id),
            Some(_) => n = format!("{name} ({k})"),
            None => break,
        }
    }
    db.execute(
        "INSERT INTO comp_type(family, name, vals) VALUES (?1,?2,?3)",
        params![family, n, vals],
    )?;
    Ok(db.last_insert_rowid())
}

fn host_cols(h: CompHost) -> (Option<i64>, Option<i64>, Option<i64>) {
    match h {
        CompHost::None => (None, None, None),
        CompHost::Wall(w) => (Some(w), None, None),
        CompHost::Floor(s) => (None, Some(s), None),
        CompHost::Ceiling(c) => (None, None, Some(c)),
    }
}

/// Instance check: the host suits the family and exists, the body can be built.
/// Returns the level (the host's, if any) and the value text.
fn check(db: &Connection, cache: &mut Cache, s: &CompSpec) -> Result<(i64, String), Error> {
    let (f, tv) = cache.ctype(db, s.ctype)?;
    let ok = match (f.hosting, s.host) {
        (Hosting::Wall, CompHost::Wall(_)) => true,
        (Hosting::Wall, _) => false,
        (Hosting::Floor, CompHost::Floor(_) | CompHost::None) => true,
        (Hosting::Ceiling, CompHost::Ceiling(_) | CompHost::None) => true,
        (Hosting::Face, _) => true,
        (Hosting::None, CompHost::None) => true,
        _ => false,
    };
    if !ok {
        return invalid(format!(
            "component: family {} is {}-hosted, got {:?}",
            f.name,
            f.hosting.name(),
            s.host
        ));
    }
    let level = match s.host {
        CompHost::None => s.level,
        CompHost::Wall(w) => host_level(db, "wall", w)?,
        CompHost::Floor(x) => host_level(db, "slab", x)?,
        CompHost::Ceiling(x) => host_level(db, "ceiling", x)?,
    };
    if !exists(db, "level", level)? {
        return invalid(format!("component: no level {level}"));
    }
    let (x, y, z) = s.at;
    if ![x, y, z, s.rotation].iter().all(|v| v.is_finite()) {
        return invalid("component: position must be finite");
    }
    if let CompHost::Wall(w) = s.host {
        let (a, b, bulge): ((f64, f64), (f64, f64), f64) = db.query_row(
            "SELECT ax, ay, bx, by, bulge FROM wall WHERE id=?1",
            [w],
            |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?), r.get(4)?)),
        )?;
        let len = ddc_geom::arc::axis_length(v2(a.0, a.1), v2(b.0, b.1), bulge);
        if x < -1e-9 || x > len + 1e-9 {
            return invalid(format!("component: {x} is outside wall {w} (length {len})"));
        }
    }
    let iv = f.assignments_text(&s.values);
    let r = f.resolve(&tv, &s.values).map_err(fam_err)?;
    f.build(&r, Lod::Medium).map_err(fam_err)?;
    Ok((level, iv))
}

/// [`check`] for one instance: (level, values text).
pub(crate) fn checked(db: &Connection, s: &CompSpec) -> Result<(i64, String), Error> {
    check(db, &mut Cache::default(), s)
}

/// New `comp` row of an already [`checked`] instance.
pub(crate) fn insert(db: &Connection, s: &CompSpec, level: i64, vals: &str) -> Result<i64, Error> {
    write(db, s, level, vals, None)
}

fn host_level(db: &Connection, table: &str, id: i64) -> Result<i64, Error> {
    db.query_row(
        &format!("SELECT level FROM {table} WHERE id=?1"),
        [id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("component: no host {table} {id}")))
}

fn write(
    db: &Connection,
    s: &CompSpec,
    level: i64,
    vals: &str,
    id: Option<i64>,
) -> Result<i64, Error> {
    let (hw, hs, hc) = host_cols(s.host);
    db.prepare_cached(
        "INSERT OR REPLACE INTO comp(id,level,ctype,x,y,z,rotation,mirror,host_wall,host_slab,host_ceiling,vals,tilt) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
    )?
    .execute(params![
        id,
        level,
        s.ctype,
        s.at.0,
        s.at.1,
        s.at.2,
        s.rotation,
        s.mirror as i64,
        hw,
        hs,
        hc,
        vals,
        tilt_text(s.tilt)
    ])?;
    Ok(db.last_insert_rowid())
}

impl Model {
    /// Loads a family from definition text with all its types; the name must be new.
    pub fn load_family(&mut self, def: &str) -> Result<i64, Error> {
        let f = Family::parse(def).map_err(fam_err)?;
        if self.family_id(&f.name)?.is_some() {
            return invalid(format!("component family {} already exists", f.name));
        }
        for t in &f.types {
            let r = f.resolve(&t.values, &[]).map_err(fam_err)?;
            f.build(&r, Lod::Medium).map_err(fam_err)?;
        }
        self.op("comp_family_add", |db| {
            let id = family_into(db, def)?;
            let inv =
                Inv::default()
                    .delete("comp_type", "family", id)
                    .delete("comp_family", "id", id);
            Ok((id, id, inv))
        })
    }

    /// Built-in library family by name (if already loaded, its id).
    pub fn load_library_family(&mut self, name: &str) -> Result<i64, Error> {
        if let Some(id) = self.family_id(name)? {
            return Ok(id);
        }
        let src = library_sources()
            .into_iter()
            .find(|s| Family::parse(s).is_ok_and(|f| f.name == name))
            .ok_or_else(|| Error::Invalid(format!("no library family {name}")))?;
        self.load_family(src)
    }

    /// Replaces a family definition (same name); types from the new text are added,
    /// existing ones are kept; all instances must still build.
    pub fn reload_family(&mut self, id: i64, def: &str) -> Result<(), Error> {
        let f = Family::parse(def).map_err(fam_err)?;
        let old: String = self
            .db
            .query_row("SELECT name FROM comp_family WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no component family {id}")))?;
        if old != f.name {
            return invalid("reload_family: the family name must stay the same");
        }
        self.op("comp_family_edit", |db| {
            let (inv, _) = reload_into(db, id, &f, def, false)?;
            Ok(((), id, inv))
        })
    }

    pub fn family_id(&self, name: &str) -> Result<Option<i64>, Error> {
        Ok(self
            .db
            .query_row("SELECT id FROM comp_family WHERE name=?1", [name], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn family(&self, id: i64) -> Result<Family, Error> {
        Ok((*Cache::default().family(&self.db, id)?).clone())
    }

    /// Model families: (id, name).
    pub fn families(&self) -> Result<Vec<(i64, String)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id, name FROM comp_family ORDER BY name")?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// Family types: (id, name).
    pub fn comp_types(&self, family: i64) -> Result<Vec<(i64, String)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id, name FROM comp_type WHERE family=?1 ORDER BY id")?;
        let r = st.query_map([family], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    pub fn comp_type_id(&self, family: i64, name: &str) -> Result<Option<i64>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT id FROM comp_type WHERE family=?1 AND name=?2",
                params![family, name],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Type values.
    pub fn comp_type_values(&self, ctype: i64) -> Result<Vec<(String, Value)>, Error> {
        Ok((*Cache::default().ctype(&self.db, ctype)?.1).clone())
    }

    /// New family type (duplicate with changed values).
    pub fn add_comp_type(
        &mut self,
        family: i64,
        name: &str,
        values: &[(String, Value)],
    ) -> Result<i64, Error> {
        let f = self.family(family)?;
        if self.comp_type_id(family, name)?.is_some() || name.trim().is_empty() {
            return invalid(format!("component type '{name}' exists or is empty"));
        }
        let r = f.resolve(values, &[]).map_err(fam_err)?;
        f.build(&r, Lod::Medium).map_err(fam_err)?;
        let vals = f.assignments_text(values);
        self.op("comp_type_add", |db| {
            db.execute(
                "INSERT INTO comp_type(family, name, vals) VALUES (?1,?2,?3)",
                params![family, name, vals],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("comp_type", "id", id)))
        })
    }

    /// Changes type values; all its instances must still build.
    pub fn set_comp_type_values(
        &mut self,
        ctype: i64,
        values: &[(String, Value)],
    ) -> Result<(), Error> {
        let fid: i64 = self
            .db
            .query_row("SELECT family FROM comp_type WHERE id=?1", [ctype], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no component type {ctype}")))?;
        let f = self.family(fid)?;
        let vals = f.assignments_text(values);
        self.op("comp_type_edit", |db| {
            let inv = Inv::default().restore(db, "comp_type", "id", ctype)?;
            db.execute(
                "UPDATE comp_type SET vals=?2 WHERE id=?1",
                params![ctype, vals],
            )?;
            let r = f.resolve(values, &[]).map_err(fam_err)?;
            f.build(&r, Lod::Medium).map_err(fam_err)?;
            check_instances(db, &format!("ctype={ctype}"))?;
            Ok(((), ctype, inv))
        })
    }

    /// Family type values in one operation: existing ones are changed by name,
    /// new ones are added; every type and all instances must build.
    pub fn set_comp_types(
        &mut self,
        family: i64,
        types: &[(String, Vec<(String, Value)>)],
    ) -> Result<(), Error> {
        let f = self.family(family)?;
        let mut rows = vec![];
        for (name, values) in types {
            if name.trim().is_empty() {
                return invalid("empty component type name");
            }
            let r = f.resolve(values, &[]).map_err(fam_err)?;
            f.build(&r, Lod::Medium).map_err(fam_err)?;
            rows.push((name.trim().to_string(), f.assignments_text(values)));
        }
        self.op("comp_type_edit", |db| {
            let max: i64 = db.query_row("SELECT coalesce(max(id),0) FROM comp_type", [], |r| {
                r.get(0)
            })?;
            let inv = Inv::default()
                .sql(
                    "DELETE FROM comp_type WHERE family=?1 AND id>?2",
                    vec![family.into(), max.into()],
                )
                .restore(db, "comp_type", "family", family)?;
            for (name, vals) in &rows {
                db.execute(
                    "INSERT INTO comp_type(family, name, vals) VALUES (?1,?2,?3) \
                     ON CONFLICT(family, name) DO UPDATE SET vals=excluded.vals",
                    params![family, name, vals],
                )?;
            }
            check_instances(
                db,
                &format!("ctype IN (SELECT id FROM comp_type WHERE family={family})"),
            )?;
            Ok(((), family, inv))
        })
    }

    pub fn delete_comp_type(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("comp_type", "comp_type_delete", id, &[])
    }

    /// Deletes a family with its types; while instances exist, `Referenced`.
    pub fn delete_family(&mut self, id: i64) -> Result<(), Error> {
        let used: Vec<i64> = {
            let mut st = self.db.prepare(
                "SELECT c.id FROM comp c JOIN comp_type t ON t.id=c.ctype WHERE t.family=?1 ORDER BY c.id",
            )?;
            let v = st
                .query_map([id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        if !used.is_empty() {
            return Err(Error::Referenced {
                kind: "comp_family",
                id,
                by: used.into_iter().map(|c| ("comp", c)).collect(),
            });
        }
        self.op("comp_family_delete", |db| {
            if !exists(db, "comp_family", id)? {
                return invalid(format!("no component family {id}"));
            }
            let inv = Inv::default()
                .restore(db, "comp_family", "id", id)?
                .restore(db, "comp_type", "family", id)?;
            db.execute("DELETE FROM comp_type WHERE family=?1", [id])?;
            db.execute("DELETE FROM comp_family WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    pub fn add_component(&mut self, s: &CompSpec) -> Result<i64, Error> {
        Ok(self.add_components(std::slice::from_ref(s))?[0])
    }

    /// Several instances in one operation (one undo).
    pub fn add_components(&mut self, specs: &[CompSpec]) -> Result<Vec<i64>, Error> {
        if specs.is_empty() {
            return invalid("no components");
        }
        let mut cache = Cache::default();
        let mut checked: HashMap<(i64, String), ()> = HashMap::new();
        let mut rows = Vec::with_capacity(specs.len());
        for s in specs {
            let vals = cache
                .ctype(&self.db, s.ctype)?
                .0
                .assignments_text(&s.values);
            // identical type and values are checked once; host and placement always
            let key = (s.ctype, vals);
            if checked.contains_key(&key) && s.host == CompHost::None {
                if !exists(&self.db, "level", s.level)? {
                    return invalid(format!("component: no level {}", s.level));
                }
                if ![s.at.0, s.at.1, s.at.2, s.rotation]
                    .iter()
                    .all(|v| v.is_finite())
                {
                    return invalid("component: position must be finite");
                }
                rows.push((s.level, key.1));
                continue;
            }
            let lv = check(&self.db, &mut cache, s)?;
            checked.insert(key, ());
            rows.push(lv);
        }
        self.op("comp_add", |db| {
            let lo: i64 =
                db.query_row("SELECT COALESCE(MAX(rowid),0) FROM comp", [], |r| r.get(0))?;
            let mut ids = Vec::with_capacity(specs.len());
            for (s, (level, vals)) in specs.iter().zip(&rows) {
                ids.push(write(db, s, *level, vals, None)?);
            }
            let inv = Inv::default().sql(
                "DELETE FROM comp WHERE rowid>?1",
                vec![rusqlite::types::Value::Integer(lo)],
            );
            Ok((ids.clone(), ids[0], inv))
        })
    }

    pub fn update_component(&mut self, id: i64, s: &CompSpec) -> Result<(), Error> {
        let (level, vals) = check(&self.db, &mut Cache::default(), s)?;
        self.op("comp_edit", |db| {
            if !exists(db, "comp", id)? {
                return invalid(format!("no such component {id}"));
            }
            let inv = Inv::default().restore(db, "comp", "id", id)?;
            write(db, s, level, &vals, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_component(&mut self, id: i64) -> Result<(), Error> {
        self.op("comp_delete", |db| {
            let inv = Inv::default()
                .restore(db, "comp", "id", id)?
                .restore(db, "solid_comp", "comp", id)?
                .then(crate::joints::detach(db, id)?);
            if db.execute("DELETE FROM comp WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such component {id}"));
            }
            db.execute("DELETE FROM solid_comp WHERE comp=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    pub fn component(&self, id: i64) -> Result<CompSpec, Error> {
        let ((level, ctype, x, y, z, rot, mirror, hw, hs, hc, vals), tilt): (
            (i64, i64, f64, f64, f64, f64, i64, Option<i64>, Option<i64>, Option<i64>, String),
            Option<String>,
        ) = self
            .db
            .query_row(
                "SELECT level,ctype,x,y,z,rotation,mirror,host_wall,host_slab,host_ceiling,vals,tilt FROM comp WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        (
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                            r.get(9)?,
                            r.get(10)?,
                        ),
                        r.get(11)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such component {id}")))?;
        let host = match (hw, hs, hc) {
            (Some(w), _, _) => CompHost::Wall(w),
            (_, Some(s), _) => CompHost::Floor(s),
            (_, _, Some(c)) => CompHost::Ceiling(c),
            _ => CompHost::None,
        };
        let (f, _) = Cache::default().ctype(&self.db, ctype)?;
        Ok(CompSpec {
            level,
            ctype,
            at: (x, y, z),
            rotation: rot,
            mirror: mirror != 0,
            tilt: tilt_parse(tilt),
            host,
            values: f.assignments(&vals).map_err(fam_err)?,
        })
    }

    /// Instances (all levels when None), by id.
    pub fn components(&self, level: Option<i64>) -> Result<Vec<i64>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id FROM comp WHERE ?1 IS NULL OR level=?1 ORDER BY id")?;
        let r = st.query_map([level], |r| r.get(0))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    pub fn component_quantities(&self, id: i64) -> Result<CompQty, Error> {
        self.db
            .query_row(
                "SELECT q.wx,q.wy,q.wz,q.wrot,q.wmirror,q.volume,q.surface,q.x0,q.y0,q.z0,q.x1,q.y1,q.z1, \
                 c.tilt FROM comp_qty q JOIN comp c ON c.id=q.comp WHERE q.comp=?1",
                [id],
                |r| {
                    Ok(CompQty {
                        placement: Placement {
                            origin: (r.get(0)?, r.get(1)?, r.get(2)?),
                            rotation: r.get(3)?,
                            mirror: r.get::<_, i64>(4)? != 0,
                            tilt: tilt_parse(r.get(13)?),
                        },
                        volume: r.get(5)?,
                        surface: r.get(6)?,
                        bbox: [(r.get(7)?, r.get(8)?, r.get(9)?), (r.get(10)?, r.get(11)?, r.get(12)?)],
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("component {id} has no quantities")))
    }

    /// Schedule: rows by (family, type, id) with all parameter values.
    pub fn component_schedule(&self) -> Result<Vec<ScheduleRow>, Error> {
        let mut cache = Cache::default();
        let raw: Vec<(i64, i64, i64, String, String, String, f64, f64)> = {
            let mut st = self.db.prepare(
                "SELECT c.id, c.ctype, c.level, c.vals, f.name, t.name, q.volume, q.surface \
                 FROM comp c JOIN comp_type t ON t.id=c.ctype JOIN comp_family f ON f.id=t.family \
                 JOIN comp_qty q ON q.comp=c.id ORDER BY f.name, t.name, c.id",
            )?;
            let v = st
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        for (id, ctype, level, vals, family, type_name, volume, surface) in raw {
            let (f, _) = cache.ctype(&self.db, ctype)?;
            let Some(r) = cache.resolve(&self.db, ctype, &vals)? else {
                continue;
            };
            let params = f
                .params
                .iter()
                .map(|p| {
                    let v = if p.kind.is_text() {
                        Value::Text(r.texts.get(&p.name).cloned().unwrap_or_default())
                    } else {
                        Value::Num(r.num(&p.name).unwrap_or(0.0))
                    };
                    (p.name.clone(), p.kind, v)
                })
                .collect();
            out.push(ScheduleRow {
                id,
                family,
                type_name,
                category: f.category.name(),
                level,
                host: self.component(id)?.host,
                volume,
                surface,
                params,
            });
        }
        Ok(out)
    }

    /// Count and volume by (family, type).
    pub fn component_counts(&self) -> Result<Vec<(String, String, i64, f64)>, Error> {
        let mut st = self.db.prepare(
            "SELECT f.name, t.name, count(*), sum(q.volume) FROM comp c \
             JOIN comp_type t ON t.id=c.ctype JOIN comp_family f ON f.id=t.family \
             JOIN comp_qty q ON q.comp=c.id GROUP BY f.name, t.name ORDER BY f.name, t.name",
        )?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// Scene: shared meshes (one per family and value set) and instance placements.
    pub fn component_scene(&self, lod: Lod, level: Option<i64>) -> Result<CompScene, Error> {
        self.component_scene_plants(lod, lod, level)
    }

    /// Scene with plants asked at `plant_lod` (still coarsened in a crowd) and the rest at `lod`.
    /// A plant's `Scale` goes into its placement, so plants of one type share one mesh.
    pub fn component_scene_plants(
        &self,
        lod: Lod,
        plant_lod: Lod,
        level: Option<i64>,
    ) -> Result<CompScene, Error> {
        let mut cache = Cache::default();
        let mut by_vals: HashMap<(i64, String), Option<(usize, f64)>> = HashMap::new();
        let mut by_key: HashMap<(i64, String), usize> = HashMap::new();
        let mut scene = CompScene::default();
        let rows: Vec<(i64, i64, String, i64, Placement)> = {
            let mut st = self.db.prepare_cached(
                "SELECT c.id, c.ctype, c.vals, t.family, q.wx, q.wy, q.wz, q.wrot, q.wmirror, c.tilt FROM comp c \
                 JOIN comp_qty q ON q.comp=c.id JOIN comp_type t ON t.id=c.ctype \
                 WHERE ?1 IS NULL OR c.level=?1 ORDER BY c.id",
            )?;
            let v = st
                .query_map([level], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        Placement {
                            origin: (r.get(4)?, r.get(5)?, r.get(6)?),
                            rotation: r.get(7)?,
                            mirror: r.get::<_, i64>(8)? != 0,
                            tilt: tilt_parse(r.get(9)?),
                        },
                    ))
                })?
                .collect::<Result<_, _>>()?;
            v
        };
        let plants = plant_count(&mut cache, &self.db, rows.iter().map(|r| r.1));
        let (crowd, rank) = plant_level(plant_lod, plants, self.plant_rank[0].load(Ordering::Relaxed), Crowd::Scene);
        self.plant_rank[0].store(rank, Ordering::Relaxed);
        for (id, ctype, vals, family, placement) in rows {
            let vk = (ctype, vals);
            let mesh = match by_vals.get(&vk) {
                Some(m) => *m,
                None => {
                    let (f, _) = cache.ctype(&self.db, ctype)?;
                    let (unit, scale) = match f.assignments(&vk.1) {
                        Ok(iv) => {
                            let (u, s) = plant_unit_values(&f, &iv);
                            (f.assignments_text(&u), s)
                        }
                        Err(_) => (vk.1.clone(), 1.0),
                    };
                    let m = match cache.resolve(&self.db, ctype, &unit)? {
                        Some(r) => {
                            let key = (family, ddc_geom::component::cache_key(&r));
                            match by_key.get(&key) {
                                Some(i) => Some(*i),
                                None => {
                                    let (f, _) = cache.ctype(&self.db, ctype)?;
                                    let lod = if is_plant(&f) { crowd } else { lod };
                                    match f.build(&r, lod) {
                                        Ok(b) => {
                                            scene.materials.push(b.parts.iter().map(|p| p.material.clone()).collect());
                                            scene.lods.push(coarser(&f, &r, lod, &b));
                                            scene.meshes.push(Arc::new(b.mesh));
                                            by_key.insert(key, scene.meshes.len() - 1);
                                            Some(scene.meshes.len() - 1)
                                        }
                                        Err(_) => None,
                                    }
                                }
                            }
                        }
                        None => None,
                    };
                    let m = m.map(|m| (m, scale));
                    by_vals.insert(vk, m);
                    m
                }
            };
            if let Some((mesh, scale)) = mesh {
                scene.instances.push(CompInstance {
                    id,
                    family,
                    ctype,
                    mesh,
                    placement,
                    scale,
                });
            }
        }
        Ok(scene)
    }

    /// Plan symbols of a level's components (except ceiling ones, which belong to the ceiling plan).
    pub fn components_plan(&self, level: i64, lod: Lod) -> Result<Vec<ViewPrim>, Error> {
        let mut cache = Cache::default();
        let mut lines: HashMap<(i64, String), Option<Arc<Vec<(ddc_geom::V2, ddc_geom::V2)>>>> =
            HashMap::new();
        let rows: Vec<(i64, i64, String, Placement)> = {
            let mut st = self.db.prepare_cached(
                "SELECT c.id, c.ctype, c.vals, q.wx, q.wy, q.wz, q.wrot, q.wmirror, c.tilt FROM comp c \
                 JOIN comp_qty q ON q.comp=c.id WHERE c.level=?1 AND c.host_ceiling IS NULL ORDER BY c.id",
            )?;
            let v = st
                .query_map([level], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        Placement {
                            origin: (r.get(3)?, r.get(4)?, r.get(5)?),
                            rotation: r.get(6)?,
                            mirror: r.get::<_, i64>(7)? != 0,
                            tilt: tilt_parse(r.get(8)?),
                        },
                    ))
                })?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        let plants = plant_count(&mut cache, &self.db, rows.iter().map(|r| r.1));
        let (crowd, rank) = plant_level(lod, plants, self.plant_rank[1].load(Ordering::Relaxed), Crowd::Plan);
        self.plant_rank[1].store(rank, Ordering::Relaxed);
        for (id, ctype, vals, p) in rows {
            let k = (ctype, vals);
            let ls = match lines.get(&k) {
                Some(l) => l.clone(),
                None => {
                    let l = match cache.resolve(&self.db, ctype, &k.1)? {
                        Some(r) => {
                            let f = cache.ctype(&self.db, ctype)?.0;
                            let lod = if is_plant(&f) { crowd } else { lod };
                            f.build(&r, lod)
                            .ok()
                            .map(|b| Arc::new(b.plan))
                        }
                        None => None,
                    };
                    lines.insert(k, l.clone());
                    l
                }
            };
            for (a, b) in ls.iter().flat_map(|l| l.iter()) {
                let (pa, pb) = (p.apply((a.x, a.y, 0.0)), p.apply((b.x, b.y, 0.0)));
                out.push(ViewPrim::Edge {
                    table: "comp",
                    element: id,
                    class: LineClass::Projection,
                    a: (pa.0, pa.1),
                    b: (pb.0, pb.1),
                });
            }
        }
        Ok(out)
    }

    /// Plants placed in the model as world-space bodies for sections and elevations: the
    /// crown and trunk of every tree, shrub and hedge, cut or seen like any other body. Coarse
    /// when there are many of them; other components stay out of the views.
    pub fn plant_bodies(&self) -> Result<Vec<(&'static str, i64, String, ddc_geom::mesh::Mesh)>, Error> {
        let mut cache = Cache::default();
        let n = plant_count(&mut cache, &self.db, self.comp_ctypes()?.into_iter());
        let lod = if n > 150 { Lod::Coarse } else { Lod::Medium };
        let cs = self.component_scene(lod, None)?;
        let (mut out, mut tris) = (vec![], 0usize);
        for i in &cs.instances {
            if !cache.ctype(&self.db, i.ctype).is_ok_and(|(f, _)| is_plant(&f)) {
                continue;
            }
            let m = &cs.meshes[i.mesh];
            tris += m.triangles.len();
            if tris > 3_000_000 {
                break;
            }
            let positions = m
                .positions
                .iter()
                .map(|p| {
                    let q = i.apply((p.x, p.y, p.z));
                    ddc_geom::v3(q.0, q.1, q.2)
                })
                .collect();
            out.push(("comp", i.id, String::new(), ddc_geom::mesh::Mesh { positions, triangles: m.triangles.clone(), face_of: m.face_of.clone() }));
        }
        Ok(out)
    }

    fn comp_ctypes(&self) -> Result<Vec<i64>, Error> {
        let mut st = self.db.prepare_cached("SELECT ctype FROM comp")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Assembled instance body (for export and properties).
    pub fn component_built(&self, id: i64, lod: Lod) -> Result<(Family, Resolved, Built), Error> {
        let (ctype, vals): (i64, String) = self
            .db
            .query_row("SELECT ctype, vals FROM comp WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such component {id}")))?;
        let mut cache = Cache::default();
        let (f, tv) = cache.ctype(&self.db, ctype)?;
        let iv = f.assignments(&vals).map_err(fam_err)?;
        let r = f.resolve(&tv, &iv).map_err(fam_err)?;
        let b = f.build(&r, lod).map_err(fam_err)?;
        Ok(((*f).clone(), r, b))
    }
}

/// All instances selected by `filter` (on the comp table) must build.
/// All family types build (including those with no instances in the project).
pub(crate) fn check_types(db: &Connection, family: i64) -> Result<(), Error> {
    let mut st = db.prepare("SELECT id FROM comp_type WHERE family=?1")?;
    let ids: Vec<i64> = st
        .query_map([family], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut cache = Cache::default();
    for id in ids {
        let (f, tv) = cache.ctype(db, id)?;
        let r = f.resolve(&tv, &[]).map_err(fam_err)?;
        f.build(&r, Lod::Medium).map_err(fam_err)?;
    }
    Ok(())
}

/// Replaces the definition of family `id` within an operation (shared by reload and load from the
/// editor): types from the text are added (`overwrite` also overwrites them), all types and
/// instances must build. Project families that embed this family via a
/// `begin … end` block get the new block text and are checked the same way (down the nesting chain).
/// Returns the inverse operation and the number of updated embedding families.
pub(crate) fn reload_into(
    db: &Connection,
    id: i64,
    f: &Family,
    def: &str,
    overwrite: bool,
) -> Result<(Inv, usize), Error> {
    let mut inv = Inv::default()
        .restore(db, "comp_family", "id", id)?
        .delete("comp_type", "family", id)
        .restore(db, "comp_type", "family", id)?;
    db.execute(
        "UPDATE comp_family SET category=?2, host=?3, def=?4 WHERE id=?1",
        params![id, f.category.name(), f.hosting.name(), def],
    )?;
    let sql = if overwrite {
        "INSERT INTO comp_type(family, name, vals) VALUES (?1,?2,?3)          ON CONFLICT(family, name) DO UPDATE SET vals=excluded.vals"
    } else {
        "INSERT OR IGNORE INTO comp_type(family, name, vals) VALUES (?1,?2,?3)"
    };
    for t in &f.types {
        db.execute(sql, params![id, t.name, f.assignments_text(&t.values)])?;
    }
    let check = |fid: i64| -> Result<(), Error> {
        check_types(db, fid)?;
        check_instances(
            db,
            &format!("ctype IN (SELECT id FROM comp_type WHERE family={fid})"),
        )
    };
    check(id)?;
    // nesting: the embedded copy is refreshed from the project family
    let mut work = vec![(f.name.clone(), def.to_string())];
    let mut seen = vec![id];
    let mut nested = 0;
    while let Some((name, text)) = work.pop() {
        let mut st = db.prepare("SELECT id, def FROM comp_family")?;
        let all: Vec<(i64, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        for (pid, pdef) in all {
            if seen.contains(&pid) {
                continue;
            }
            let mut doc = ddc_geom::component::doc::FamDoc::new(pdef);
            if !doc
                .stmts()
                .iter()
                .any(|s| s.head == "begin" && s.name == name)
            {
                continue;
            }
            doc.add_nested(&text).map_err(fam_err)?;
            let ptext = doc.text().to_string();
            inv = inv.restore(db, "comp_family", "id", pid)?;
            db.execute(
                "UPDATE comp_family SET def=?2 WHERE id=?1",
                params![pid, ptext],
            )?;
            check(pid)?;
            seen.push(pid);
            nested += 1;
            let pname: String =
                db.query_row("SELECT name FROM comp_family WHERE id=?1", [pid], |r| {
                    r.get(0)
                })?;
            work.push((pname, ptext));
        }
    }
    Ok((inv, nested))
}

pub(crate) fn check_instances(db: &Connection, filter: &str) -> Result<(), Error> {
    let mut st = db.prepare(&format!(
        "SELECT DISTINCT ctype, vals FROM comp WHERE {filter}"
    ))?;
    let combos: Vec<(i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut cache = Cache::default();
    for (ctype, vals) in combos {
        let (f, tv) = cache.ctype(db, ctype)?;
        let iv = f.assignments(&vals).map_err(fam_err)?;
        let r = f.resolve(&tv, &iv).map_err(fam_err)?;
        f.build(&r, Lod::Medium).map_err(fam_err)?;
    }
    Ok(())
}
