//! Roofs and slabs on freeform surfaces (geometry plan W9). Table `shell_elem` keeps the
//! reference surface (a copy of the host patch, z above the level), the host trim, a layer
//! stack offset along the surface normal, the side the stack grows to and plan openings.
//! A hosted element follows its host: every host edit copies the new patch in the same undo
//! step; when the host is deleted or the new patch cannot carry the stack (an offset folds),
//! the element keeps its last good surface and is flagged orphan (the mass-host rule).
//! Openings are plan polygons projected vertically onto the surface and cut as trim holes.
//! Layer volumes are exact on an untrimmed surface (Steiner integral); with trims or openings
//! they come from the closed layer meshes, and the quantities say so.

use ddc_geom::mesh::Mesh;
use ddc_geom::nurbs::{self, Surface, Trim};
use ddc_geom::v3;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::freeform::{net_json, trim_json, trim_of, ShellSide, MAX_NET, TESS};
use crate::ops::Inv;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS shell_elem (
    id INTEGER PRIMARY KEY,
    level INTEGER NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    host INTEGER,
    orphan INTEGER NOT NULL DEFAULT 0,
    surface TEXT NOT NULL,
    trim TEXT NOT NULL DEFAULT '{}',
    side TEXT NOT NULL DEFAULT 'normal',
    layers TEXT NOT NULL,
    openings TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS shell_elem_level ON shell_elem(level);
CREATE INDEX IF NOT EXISTS shell_elem_host ON shell_elem(host);
";

pub const MAX_LAYERS: usize = 8;
pub const MAX_LAYER: f64 = 2.0;
pub const MAX_OPENINGS: usize = 64;
/// Plan distance within which an opening corner must find the surface, m.
pub const PLAN_TOL: f64 = 1e-6;
/// Gauss sub-spans of the exact layer volume.
const VOL_SUB: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Roof,
    Slab,
}

impl ShellKind {
    pub fn key(self) -> &'static str {
        match self {
            ShellKind::Roof => "roof",
            ShellKind::Slab => "slab",
        }
    }
    pub fn parse(s: &str) -> Result<ShellKind, Error> {
        match s {
            "roof" => Ok(ShellKind::Roof),
            "slab" => Ok(ShellKind::Slab),
            _ => Err(Error::Invalid(format!("shell: kind {s:?}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShellLayer {
    pub name: String,
    pub material: String,
    pub thickness: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShellElemSpec {
    pub level: i64,
    pub kind: ShellKind,
    pub name: String,
    /// Freeform host it follows; none for a free element.
    pub host: Option<i64>,
    /// The host is gone or can no longer carry the stack: the surface is the last good one.
    pub orphan: bool,
    /// Reference surface, z above the level.
    pub surface: Surface,
    /// Trim of the reference surface (the host trim), normalized uv.
    pub trim: Trim,
    /// Where the stack grows from the surface; layers are listed along the normal.
    pub side: ShellSide,
    pub layers: Vec<ShellLayer>,
    /// Plan polygons (absolute x, y) cut through the stack.
    pub openings: Vec<Vec<[f64; 2]>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerQty {
    pub name: String,
    pub material: String,
    pub thickness: f64,
    pub volume: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShellQty {
    /// Kept area of the reference surface, m2.
    pub area: f64,
    pub thickness: f64,
    pub volume: f64,
    pub layers: Vec<LayerQty>,
    /// Volumes by the exact integral (no trims, no openings) or from the layer meshes.
    pub exact: bool,
    /// World box [x0, y0, z0, x1, y1, z1].
    pub bbox: [f64; 6],
}

/// Closed layer meshes in world coordinates and what the facets lost.
#[derive(Clone, Debug)]
pub struct ShellMesh {
    pub layers: Vec<Mesh>,
    pub max_dev: f64,
    pub loss: Vec<String>,
}

fn bad<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn nerr(e: nurbs::NurbsError) -> Error {
    Error::Degenerate(format!("shell: {e}"))
}

impl ShellElemSpec {
    pub fn thickness(&self) -> f64 {
        self.layers.iter().map(|l| l.thickness).sum()
    }

    /// Offsets (a, b) of every layer along the normal.
    pub fn offsets(&self) -> Vec<(f64, f64)> {
        let t = self.thickness();
        let start = match self.side {
            ShellSide::Normal => 0.0,
            ShellSide::Reverse => -t,
            ShellSide::Center => -0.5 * t,
        };
        nurbs::layer_offsets(&self.layers.iter().map(|l| l.thickness).collect::<Vec<_>>(), start)
    }

    /// Host trim plus the openings projected vertically onto the surface (uv straight
    /// between projected corners).
    pub fn full_trim(&self) -> Result<Trim, Error> {
        let sf = &self.surface;
        let (u0, u1) = sf.domain_u();
        let (v0, v1) = sf.domain_v();
        let mut t = self.trim.clone();
        for (k, o) in self.openings.iter().enumerate() {
            let ring = o
                .iter()
                .map(|p| {
                    let uv = sf.at_plan(p[0], p[1], PLAN_TOL).ok_or_else(|| {
                        Error::Invalid(format!(
                            "shell: opening {} corner ({:.3}, {:.3}) is not over the surface",
                            k + 1,
                            p[0],
                            p[1]
                        ))
                    })?;
                    Ok([(uv[0] - u0) / (u1 - u0), (uv[1] - v0) / (v1 - v0)])
                })
                .collect::<Result<Vec<_>, Error>>()?;
            let n = ring.len();
            let jump = (0..n).any(|i| {
                let (p, q) = (ring[i], ring[(i + 1) % n]);
                (p[0] - q[0]).abs() > 0.5 || (p[1] - q[1]).abs() > 0.5
            });
            if jump {
                return bad(format!("shell: opening {} crosses the seam of the surface; split it in two", k + 1));
            }
            t.holes.push(ring);
        }
        t.validate().map_err(|e| Error::Invalid(format!("shell trim: {e}")))?;
        Ok(t)
    }
}

// ---------- JSON ----------

fn surface_json(s: &Surface) -> Value {
    json!({
        "du": s.deg_u(), "dv": s.deg_v(), "nu": s.nu(), "nv": s.nv(),
        "ku": s.knots_u(), "kv": s.knots_v(), "net": net_json(s), "w": s.weights(),
    })
}

fn nums(v: &Value) -> Result<Vec<f64>, Error> {
    v.as_array()
        .ok_or_else(|| Error::Invalid("shell: expected an array".into()))?
        .iter()
        .map(|x| x.as_f64().filter(|f| f.is_finite()).ok_or_else(|| Error::Invalid("shell: bad number".into())))
        .collect()
}

fn surface_of(v: &Value) -> Result<Surface, Error> {
    let n = |k: &str| v[k].as_u64().map(|x| x as usize).ok_or_else(|| Error::Invalid(format!("shell: {k}")));
    let net = v["net"]
        .as_array()
        .ok_or_else(|| Error::Invalid("shell: net".into()))?
        .iter()
        .map(|p| match nums(p)?[..] {
            [x, y, z] => Ok(v3(x, y, z)),
            _ => bad("shell: a control point has three coordinates"),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Surface::new(n("du")?, n("dv")?, nums(&v["ku"])?, nums(&v["kv"])?, n("nu")?, n("nv")?, net, nums(&v["w"])?)
        .map_err(nerr)
}

fn layers_json(ls: &[ShellLayer]) -> Value {
    Value::Array(
        ls.iter()
            .map(|l| json!({"name": l.name, "material": l.material, "t": l.thickness}))
            .collect(),
    )
}

fn layers_of(v: &Value) -> Result<Vec<ShellLayer>, Error> {
    v.as_array()
        .ok_or_else(|| Error::Invalid("shell: layers".into()))?
        .iter()
        .map(|l| {
            Ok(ShellLayer {
                name: l["name"].as_str().unwrap_or_default().to_owned(),
                material: l["material"].as_str().unwrap_or_default().to_owned(),
                thickness: l["t"].as_f64().ok_or_else(|| Error::Invalid("shell: layer thickness".into()))?,
            })
        })
        .collect()
}

fn openings_of(v: &Value) -> Result<Vec<Vec<[f64; 2]>>, Error> {
    v.as_array()
        .ok_or_else(|| Error::Invalid("shell: openings".into()))?
        .iter()
        .map(|o| {
            o.as_array()
                .ok_or_else(|| Error::Invalid("shell: opening".into()))?
                .iter()
                .map(|p| match nums(p)?[..] {
                    [x, y] => Ok([x, y]),
                    _ => bad("shell: an opening corner is [x, y]"),
                })
                .collect()
        })
        .collect()
}

// ---------- storage ----------

/// Refuses what cannot be built: unknown level, bad stack, a stack thicker than the
/// curvature allows, openings off the surface.
pub(crate) fn check(db: &Connection, s: &ShellElemSpec) -> Result<(), Error> {
    db.query_row("SELECT 1 FROM level WHERE id=?1", [s.level], |_| Ok(()))
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no level {}", s.level)))?;
    if let (Some(h), false) = (s.host, s.orphan) {
        crate::freeform::read(db, h).map_err(|_| Error::Invalid(format!("shell: no freeform host {h}")))?;
    }
    if s.name.len() > 200 || s.layers.iter().any(|l| l.name.len() > 200 || l.material.len() > 200) {
        return bad("shell: name or material too long");
    }
    if s.layers.is_empty() || s.layers.len() > MAX_LAYERS {
        return bad(format!("shell: 1 to {MAX_LAYERS} layers"));
    }
    if let Some(l) = s.layers.iter().find(|l| !(l.thickness.is_finite() && l.thickness > 0.0 && l.thickness <= MAX_LAYER)) {
        return bad(format!("shell: layer {:?} thickness {} m is outside (0, {MAX_LAYER}]", l.name, l.thickness));
    }
    let sf = &s.surface;
    if sf.nu() * sf.nv() > MAX_NET {
        return bad(format!("shell: control net {}x{} is over {MAX_NET} points", sf.nu(), sf.nv()));
    }
    if s.openings.len() > MAX_OPENINGS || s.openings.iter().any(|o| o.len() < 3 || o.len() > 256) {
        return bad(format!("shell: up to {MAX_OPENINGS} openings of 3 to 256 corners"));
    }
    let offs = s.offsets();
    let (a, b) = (offs[0].0, offs[offs.len() - 1].1);
    for d in [a, b] {
        if d != 0.0 {
            nurbs::check_offset(sf, d).map_err(|_| {
                Error::Degenerate(format!(
                    "shell: a {:.0} mm stack folds where the surface curves tighter than that",
                    s.thickness() * 1000.0
                ))
            })?;
        }
    }
    s.full_trim()?;
    Ok(())
}

fn write(db: &Connection, s: &ShellElemSpec, id: Option<i64>) -> Result<i64, Error> {
    let args = params![
        id,
        s.level,
        s.kind.key(),
        s.name,
        s.host,
        s.orphan as i64,
        surface_json(&s.surface).to_string(),
        trim_json(&s.trim).to_string(),
        s.side.key(),
        layers_json(&s.layers).to_string(),
        json!(s.openings).to_string(),
    ];
    if id.is_some() {
        db.execute(
            "UPDATE shell_elem SET level=?2,kind=?3,name=?4,host=?5,orphan=?6,surface=?7,trim=?8,side=?9,\
             layers=?10,openings=?11 WHERE id=?1",
            args,
        )?;
        Ok(id.unwrap_or_default())
    } else {
        db.execute(
            "INSERT INTO shell_elem(id,level,kind,name,host,orphan,surface,trim,side,layers,openings) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            args,
        )?;
        Ok(db.last_insert_rowid())
    }
}

pub(crate) fn read(db: &Connection, id: i64) -> Result<ShellElemSpec, Error> {
    let row = db
        .query_row(
            "SELECT level,kind,name,host,orphan,surface,trim,side,layers,openings FROM shell_elem WHERE id=?1",
            [id],
            |r| {
                Ok((
                    (r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                    (r.get::<_, Option<i64>>(3)?, r.get::<_, i64>(4)?),
                    (r.get::<_, String>(5)?, r.get::<_, String>(6)?, r.get::<_, String>(7)?),
                    (r.get::<_, String>(8)?, r.get::<_, String>(9)?),
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no shell element {id}")))?;
    let ((level, kind, name), (host, orphan), (surface, trim, side), (layers, openings)) = row;
    let p = |t: &str| -> Result<Value, Error> { Ok(serde_json::from_str(t)?) };
    Ok(ShellElemSpec {
        level,
        kind: ShellKind::parse(&kind)?,
        name,
        host,
        orphan: orphan != 0,
        surface: surface_of(&p(&surface)?)?,
        trim: trim_of(&p(&trim)?)?,
        side: ShellSide::parse(&side)?,
        layers: layers_of(&p(&layers)?)?,
        openings: openings_of(&p(&openings)?)?,
    })
}

fn hosted(db: &Connection, host: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare_cached("SELECT id FROM shell_elem WHERE host=?1 ORDER BY id")?;
    let r = st.query_map([host], |r| r.get(0))?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// After an edit or deletion of freeform `host`: hosted elements take the new patch, or keep
/// their last surface flagged orphan. Part of the host's undo step.
pub(crate) fn sync_host(db: &Connection, host: i64, inv: Inv) -> Result<Inv, Error> {
    let ids = hosted(db, host)?;
    if ids.is_empty() {
        return Ok(inv);
    }
    let inv = inv.restore(db, "shell_elem", "host", host)?;
    let h = crate::freeform::read(db, host).ok();
    for id in ids {
        let old = read(db, id)?;
        let next = h.as_ref().map(|f| ShellElemSpec {
            level: f.level,
            surface: f.surface.clone(),
            trim: f.trim.clone(),
            orphan: false,
            ..old.clone()
        });
        match next {
            Some(n) if check(db, &n).is_ok() => write(db, &n, Some(id))?,
            _ => write(db, &ShellElemSpec { orphan: true, ..old }, Some(id))?,
        };
    }
    Ok(inv)
}

fn elev(db: &Connection, level: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no level {level}")))
}

/// Closed layer meshes in world coordinates (`z0`: level elevation).
pub fn mesh_of(s: &ShellElemSpec, z0: f64) -> Result<ShellMesh, Error> {
    let trim = s.full_trim()?;
    let sm = nurbs::tessellate_trimmed(&s.surface, &trim, &TESS).map_err(nerr)?;
    let mut loss = vec![format!("exact surface replaced by triangles: {:.4} m max deviation", sm.max_dev)];
    if !s.openings.is_empty() {
        loss.push(format!("openings: {} cut as plan projections on the surface", s.openings.len()));
    }
    let mut layers = vec![];
    for (a, b) in s.offsets() {
        let mut m = nurbs::thicken_between(&sm, a, b).map_err(nerr)?.mesh;
        for p in m.positions.iter_mut() {
            p.z += z0;
        }
        layers.push(m);
    }
    Ok(ShellMesh { layers, max_dev: sm.max_dev, loss })
}

pub fn qty_of(s: &ShellElemSpec, z0: f64) -> Result<ShellQty, Error> {
    let trim = s.full_trim()?;
    let exact = trim.is_empty();
    let sm = mesh_of(s, z0)?;
    let area = if exact {
        s.surface.area(VOL_SUB)
    } else {
        nurbs::tessellate_trimmed(&s.surface, &trim, &TESS).map_err(nerr)?.mesh.area()
    };
    let layers: Vec<LayerQty> = s
        .layers
        .iter()
        .zip(s.offsets())
        .zip(&sm.layers)
        .map(|((l, (a, b)), m)| LayerQty {
            name: l.name.clone(),
            material: l.material.clone(),
            thickness: l.thickness,
            volume: if exact { s.surface.offset_volume(a, b, VOL_SUB).abs() } else { m.volume().abs() },
        })
        .collect();
    let mut bb = [f64::INFINITY, f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in sm.layers.iter().flat_map(|m| &m.positions) {
        bb = [bb[0].min(p.x), bb[1].min(p.y), bb[2].min(p.z), bb[3].max(p.x), bb[4].max(p.y), bb[5].max(p.z)];
    }
    Ok(ShellQty {
        area,
        thickness: s.thickness(),
        volume: layers.iter().map(|l| l.volume).sum(),
        layers,
        exact,
        bbox: bb,
    })
}

// ---------- API ----------

impl Model {
    /// A roof or slab following freeform `host` (its patch, trim and level).
    pub fn shell_on_host(
        &self,
        host: i64,
        kind: ShellKind,
        layers: Vec<ShellLayer>,
        side: ShellSide,
        openings: Vec<Vec<[f64; 2]>>,
        name: &str,
    ) -> Result<ShellElemSpec, Error> {
        let f = self.freeform(host)?;
        Ok(ShellElemSpec {
            level: f.level,
            kind,
            name: name.to_owned(),
            host: Some(host),
            orphan: false,
            surface: f.surface,
            trim: f.trim,
            side,
            layers,
            openings,
        })
    }

    pub fn add_shell_elem(&mut self, s: &ShellElemSpec) -> Result<i64, Error> {
        check(&self.db, s)?;
        self.op("shell_add", |db| {
            let id = write(db, s, None)?;
            Ok((id, id, Inv::default().delete("shell_elem", "id", id)))
        })
    }

    pub fn update_shell_elem(&mut self, id: i64, s: &ShellElemSpec) -> Result<(), Error> {
        check(&self.db, s)?;
        self.op("shell_edit", |db| {
            read(db, id)?;
            let inv = Inv::default().restore(db, "shell_elem", "id", id)?;
            write(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_shell_elem(&mut self, id: i64) -> Result<(), Error> {
        self.op("shell_delete", |db| {
            read(db, id)?;
            let inv = Inv::default().restore(db, "shell_elem", "id", id)?;
            db.execute("DELETE FROM shell_elem WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    pub fn shell_elem(&self, id: i64) -> Result<ShellElemSpec, Error> {
        read(&self.db, id)
    }

    /// Elements of a level (or all); unreadable rows are skipped.
    pub fn shell_elems(&self, level: Option<i64>) -> Result<Vec<(i64, ShellElemSpec)>, Error> {
        let mut st = self.db.prepare("SELECT id FROM shell_elem WHERE (?1 IS NULL OR level=?1) ORDER BY id")?;
        let ids: Vec<i64> = st.query_map([level], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(ids.into_iter().filter_map(|id| read(&self.db, id).ok().map(|s| (id, s))).collect())
    }

    /// Elements that follow freeform `host`.
    pub fn shells_on(&self, host: i64) -> Result<Vec<i64>, Error> {
        hosted(&self.db, host)
    }

    pub fn shell_elem_mesh(&self, id: i64) -> Result<ShellMesh, Error> {
        let s = self.shell_elem(id)?;
        mesh_of(&s, elev(&self.db, s.level)?)
    }

    pub fn shell_elem_quantities(&self, id: i64) -> Result<ShellQty, Error> {
        let s = self.shell_elem(id)?;
        qty_of(&s, elev(&self.db, s.level)?)
    }
}

#[cfg(test)]
#[path = "shell_elem_tests.rs"]
mod tests;
