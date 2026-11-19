//! Freeform surfaces (geometry plan W3): a NURBS patch as a model element. Table `freeform`
//! stores degrees, clamped knots, the control net and weights (JSON, z above the level),
//! shell thickness and side, trim loops in normalized (u, v) and how the patch was made.
//! The scene mesh, quantities and the IFC fallback come from the own tessellation
//! (`nurbs::tessellate_trimmed`, `thicken_between`); the exact patch goes to IFC as is.
//! Nothing is derived into tables: the mesh is rebuilt from the row when asked.

use ddc_geom::mesh::Mesh;
use ddc_geom::nurbs::{self, Surface, TessTol, Trim};
use ddc_geom::{v3, V3};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::ops::Inv;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS freeform (
    id INTEGER PRIMARY KEY,
    level INTEGER NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    material TEXT NOT NULL DEFAULT '',
    deg_u INTEGER NOT NULL,
    deg_v INTEGER NOT NULL,
    nu INTEGER NOT NULL,
    nv INTEGER NOT NULL,
    knots_u TEXT NOT NULL,
    knots_v TEXT NOT NULL,
    net TEXT NOT NULL,
    weights TEXT NOT NULL,
    thickness REAL NOT NULL DEFAULT 0,
    side TEXT NOT NULL DEFAULT 'normal',
    trim TEXT NOT NULL DEFAULT '{}',
    origin TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS freeform_level ON freeform(level);
";

/// Largest control net accepted (rows x columns).
pub const MAX_NET: usize = 10_000;
/// Thickest shell, metres.
pub const MAX_THICKNESS: f64 = 5.0;
/// Chord and angle tolerance of the scene mesh and the tessellated fallbacks.
pub const TESS: TessTol = TessTol { chord: 2e-3, angle: 0.174_532_925_199_432_95, max_per_dir: 512 };

/// Where the shell lies relative to the surface (along its normal Su x Sv).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShellSide {
    #[default]
    Normal,
    Reverse,
    Center,
}

impl ShellSide {
    pub fn key(self) -> &'static str {
        match self {
            ShellSide::Normal => "normal",
            ShellSide::Reverse => "reverse",
            ShellSide::Center => "center",
        }
    }
    pub fn parse(s: &str) -> Result<ShellSide, Error> {
        Ok(match s {
            "normal" => ShellSide::Normal,
            "reverse" => ShellSide::Reverse,
            "center" => ShellSide::Center,
            _ => return Err(Error::Invalid(format!("freeform: side {s:?}"))),
        })
    }
    /// Offsets (a, b) of the two shell faces along the normal for thickness `t`.
    pub fn offsets(self, t: f64) -> (f64, f64) {
        match self {
            ShellSide::Normal => (0.0, t),
            ShellSide::Reverse => (0.0, -t),
            ShellSide::Center => (-0.5 * t, 0.5 * t),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FreeformSpec {
    pub level: i64,
    pub name: String,
    pub material: String,
    /// Exact patch; z above the level elevation.
    pub surface: Surface,
    /// Shell thickness, m; 0 is a bare surface.
    pub thickness: f64,
    pub side: ShellSide,
    pub trim: Trim,
    /// How it was made: loft, ruled, revolve, points (kept through edits).
    pub origin: String,
}

/// Tessellated element in world coordinates and what the facets lost.
#[derive(Clone, Debug)]
pub struct FreeformMesh {
    pub mesh: Mesh,
    /// Closed shell (thickness > 0) or open surface.
    pub solid: bool,
    pub max_dev: f64,
    pub converged: bool,
    /// `what: measure` lines.
    pub loss: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreeformQty {
    /// Area of the kept surface (Gauss on the exact patch when untrimmed), m2.
    pub area: f64,
    /// Volume of the shell (divergence theorem on the closed mesh), m3; 0 for a surface.
    pub volume: f64,
    /// World box [x0, y0, z0, x1, y1, z1].
    pub bbox: [f64; 6],
}

fn bad<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn nerr(e: nurbs::NurbsError) -> Error {
    Error::Degenerate(format!("freeform: {e}"))
}

// ---------- JSON ----------

fn nums(v: &Value) -> Result<Vec<f64>, Error> {
    v.as_array()
        .ok_or_else(|| Error::Invalid("freeform: expected an array".into()))?
        .iter()
        .map(|x| x.as_f64().filter(|f| f.is_finite()).ok_or_else(|| Error::Invalid("freeform: bad number".into())))
        .collect()
}

pub fn net_json(s: &Surface) -> Value {
    Value::Array(s.ctrl().iter().map(|p| json!([p.x, p.y, p.z])).collect())
}

fn net_of(v: &Value) -> Result<Vec<V3>, Error> {
    v.as_array()
        .ok_or_else(|| Error::Invalid("freeform: net must be an array".into()))?
        .iter()
        .map(|p| {
            let c = nums(p)?;
            match c[..] {
                [x, y, z] => Ok(v3(x, y, z)),
                _ => bad("freeform: a control point has three coordinates"),
            }
        })
        .collect()
}

pub fn trim_json(t: &Trim) -> Value {
    let ring = |r: &Vec<[f64; 2]>| Value::Array(r.iter().map(|p| json!([p[0], p[1]])).collect());
    json!({
        "outer": t.outer.as_ref().map(ring),
        "holes": t.holes.iter().map(ring).collect::<Vec<_>>(),
    })
}

pub fn trim_of(v: &Value) -> Result<Trim, Error> {
    let ring = |r: &Value| -> Result<Vec<[f64; 2]>, Error> {
        r.as_array()
            .ok_or_else(|| Error::Invalid("freeform: trim loop must be an array".into()))?
            .iter()
            .map(|p| match nums(p)?[..] {
                [u, v] => Ok([u, v]),
                _ => bad("freeform: a trim point is [u, v]"),
            })
            .collect()
    };
    let outer = match v.get("outer") {
        None | Some(Value::Null) => None,
        Some(r) => Some(ring(r)?),
    };
    let holes = match v.get("holes") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().map(ring).collect::<Result<_, _>>()?,
        Some(_) => return bad("freeform: holes must be an array"),
    };
    Ok(Trim { outer, holes })
}

// ---------- storage ----------

fn elev(db: &Connection, level: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no level {level}")))
}

/// Refuses what cannot be built: unknown level, oversized or degenerate net, bad trims,
/// a shell thicker than the curvature allows.
pub(crate) fn check(db: &Connection, s: &FreeformSpec) -> Result<(), Error> {
    elev(db, s.level)?;
    if s.name.len() > 200 || s.material.len() > 200 || s.origin.len() > 40 {
        return bad("freeform: name, material or origin too long");
    }
    let sf = &s.surface;
    if sf.nu() * sf.nv() > MAX_NET {
        return bad(format!("freeform: control net {}x{} is over {MAX_NET} points", sf.nu(), sf.nv()));
    }
    if sf.ctrl().iter().any(|p| p.x.abs().max(p.y.abs()).max(p.z.abs()) > 1e5) {
        return bad("freeform: control point farther than 100 km");
    }
    let area = sf.area(2);
    if !(area > 1e-6) {
        return Err(Error::Degenerate(format!(
            "freeform: the control net spans no area ({area:.3e} m2); points are collinear or coincide"
        )));
    }
    if !(s.thickness.is_finite() && (0.0..=MAX_THICKNESS).contains(&s.thickness)) {
        return bad(format!("freeform: thickness {} m is outside 0..{MAX_THICKNESS} m", s.thickness));
    }
    s.trim.validate().map_err(|e| Error::Invalid(format!("freeform trim: {e}")))?;
    if s.thickness > 0.0 {
        let (a, b) = s.side.offsets(s.thickness);
        for d in [a, b] {
            if d != 0.0 {
                nurbs::check_offset(sf, d).map_err(|_| {
                    Error::Degenerate(format!(
                        "freeform: a {:.0} mm shell folds where the surface curves tighter than that",
                        s.thickness * 1000.0
                    ))
                })?;
            }
        }
    }
    Ok(())
}

pub(crate) fn write(db: &Connection, s: &FreeformSpec, id: Option<i64>) -> Result<i64, Error> {
    let sf = &s.surface;
    let args = params![
        id,
        s.level,
        s.name,
        s.material,
        sf.deg_u() as i64,
        sf.deg_v() as i64,
        sf.nu() as i64,
        sf.nv() as i64,
        json!(sf.knots_u()).to_string(),
        json!(sf.knots_v()).to_string(),
        net_json(sf).to_string(),
        json!(sf.weights()).to_string(),
        s.thickness,
        s.side.key(),
        trim_json(&s.trim).to_string(),
        s.origin,
    ];
    if id.is_some() {
        db.execute(
            "UPDATE freeform SET level=?2,name=?3,material=?4,deg_u=?5,deg_v=?6,nu=?7,nv=?8,\
             knots_u=?9,knots_v=?10,net=?11,weights=?12,thickness=?13,side=?14,trim=?15,origin=?16 \
             WHERE id=?1",
            args,
        )?;
        Ok(id.unwrap_or_default())
    } else {
        db.execute(
            "INSERT INTO freeform(id,level,name,material,deg_u,deg_v,nu,nv,knots_u,knots_v,net,\
             weights,thickness,side,trim,origin) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            args,
        )?;
        Ok(db.last_insert_rowid())
    }
}

pub(crate) fn read(db: &Connection, id: i64) -> Result<FreeformSpec, Error> {
    let row = db
        .query_row(
            "SELECT level,name,material,deg_u,deg_v,nu,nv,knots_u,knots_v,net,weights,thickness,\
             side,trim,origin FROM freeform WHERE id=?1",
            [id],
            |r| {
                Ok((
                    (r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                    (r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?),
                    (r.get::<_, String>(7)?, r.get::<_, String>(8)?, r.get::<_, String>(9)?, r.get::<_, String>(10)?),
                    (r.get::<_, f64>(11)?, r.get::<_, String>(12)?, r.get::<_, String>(13)?, r.get::<_, String>(14)?),
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no freeform {id}")))?;
    let ((level, name, material), (du, dv, nu, nv), (ku, kv, net, w), (thickness, side, trim, origin)) = row;
    let p = |t: &str| -> Result<Value, Error> { Ok(serde_json::from_str(t)?) };
    let u = |x: i64| usize::try_from(x).map_err(|_| Error::Invalid("freeform: negative size".into()));
    let surface = Surface::new(
        u(du)?,
        u(dv)?,
        nums(&p(&ku)?)?,
        nums(&p(&kv)?)?,
        u(nu)?,
        u(nv)?,
        net_of(&p(&net)?)?,
        nums(&p(&w)?)?,
    )
    .map_err(nerr)?;
    Ok(FreeformSpec {
        level,
        name,
        material,
        surface,
        thickness,
        side: ShellSide::parse(&side)?,
        trim: trim_of(&p(&trim)?)?,
        origin,
    })
}

fn ids(db: &Connection, level: Option<i64>) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare("SELECT id FROM freeform WHERE (?1 IS NULL OR level=?1) ORDER BY id")?;
    let r = st.query_map([level], |r| r.get(0))?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Scene mesh of a spec in world coordinates (`z0` is the level elevation).
pub fn mesh_of(s: &FreeformSpec, z0: f64) -> Result<FreeformMesh, Error> {
    let sm = nurbs::tessellate_trimmed(&s.surface, &s.trim, &TESS).map_err(nerr)?;
    let mut loss = vec![format!(
        "exact surface replaced by triangles: {:.4} m max deviation",
        sm.max_dev
    )];
    let (mut mesh, solid) = if s.thickness > 0.0 {
        let (a, b) = s.side.offsets(s.thickness);
        let th = nurbs::thicken_between(&sm, a, b).map_err(nerr)?;
        loss.push(format!(
            "shell offset sampled along vertex normals: {:.4} m thickness",
            s.thickness
        ));
        (th.mesh, true)
    } else {
        (sm.mesh, false)
    };
    for p in mesh.positions.iter_mut() {
        p.z += z0;
    }
    Ok(FreeformMesh { mesh, solid, max_dev: sm.max_dev, converged: sm.converged, loss })
}

/// Quantities of a spec (`z0` is the level elevation).
pub fn qty_of(s: &FreeformSpec, z0: f64) -> Result<FreeformQty, Error> {
    let fm = mesh_of(s, z0)?;
    let area = if s.trim.is_empty() {
        s.surface.area(4)
    } else {
        nurbs::tessellate_trimmed(&s.surface, &s.trim, &TESS).map_err(nerr)?.mesh.area()
    };
    let volume = if fm.solid { fm.mesh.volume().abs() } else { 0.0 };
    let mut bb = [f64::INFINITY, f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in &fm.mesh.positions {
        bb = [bb[0].min(p.x), bb[1].min(p.y), bb[2].min(p.z), bb[3].max(p.x), bb[4].max(p.y), bb[5].max(p.z)];
    }
    Ok(FreeformQty { area, volume, bbox: bb })
}

// ---------- API ----------

impl Model {
    pub fn add_freeform(&mut self, s: &FreeformSpec) -> Result<i64, Error> {
        check(&self.db, s)?;
        self.op("freeform_add", |db| {
            let id = write(db, s, None)?;
            Ok((id, id, Inv::default().delete("freeform", "id", id)))
        })
    }

    pub fn update_freeform(&mut self, id: i64, s: &FreeformSpec) -> Result<(), Error> {
        check(&self.db, s)?;
        self.op("freeform_edit", |db| {
            read(db, id)?;
            let inv = Inv::default().restore(db, "freeform", "id", id)?;
            write(db, s, Some(id))?;
            let inv = crate::shell_elem::sync_host(db, id, inv)?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_freeform(&mut self, id: i64) -> Result<(), Error> {
        self.op("freeform_delete", |db| {
            read(db, id)?;
            let inv = Inv::default().restore(db, "freeform", "id", id)?;
            db.execute("DELETE FROM freeform WHERE id=?1", [id])?;
            let inv = crate::panel_system::drop_hosted(db, id, inv)?;
            let inv = crate::shell_elem::sync_host(db, id, inv)?;
            let inv = crate::recipe::unlink(db, "freeform", id, inv)?;
            Ok(((), id, inv))
        })
    }

    pub fn freeform(&self, id: i64) -> Result<FreeformSpec, Error> {
        read(&self.db, id)
    }

    /// Patches of a level (or all); unreadable rows are skipped.
    pub fn freeforms(&self, level: Option<i64>) -> Result<Vec<(i64, FreeformSpec)>, Error> {
        Ok(ids(&self.db, level)?
            .into_iter()
            .filter_map(|id| read(&self.db, id).ok().map(|s| (id, s)))
            .collect())
    }

    /// Moves control point (i, j) (row i along u) to `p`, z above the level; one undo step.
    pub fn move_freeform_point(&mut self, id: i64, i: usize, j: usize, p: V3) -> Result<(), Error> {
        let mut s = self.freeform(id)?;
        s.surface = s.surface.with_ctrl(i, j, p).map_err(|_| {
            Error::Invalid(format!(
                "freeform {id}: no control point ({i}, {j}) in a {}x{} net",
                s.surface.nu(),
                s.surface.nv()
            ))
        })?;
        self.update_freeform(id, &s)
    }

    /// Sets the weight of control point (i, j); one undo step. Refused unless finite and > 0.
    pub fn set_freeform_weight(&mut self, id: i64, i: usize, j: usize, w: f64) -> Result<(), Error> {
        let mut s = self.freeform(id)?;
        s.surface = s.surface.with_weight(i, j, w).map_err(|_| {
            Error::Invalid(format!(
                "freeform {id}: weight {w} at ({i}, {j}) refused (needs a point of the {}x{} net and a weight above 0)",
                s.surface.nu(),
                s.surface.nv()
            ))
        })?;
        self.update_freeform(id, &s)
    }

    pub fn set_freeform_shell(&mut self, id: i64, thickness: f64, side: ShellSide) -> Result<(), Error> {
        let s = FreeformSpec { thickness, side, ..self.freeform(id)? };
        self.update_freeform(id, &s)
    }

    pub fn set_freeform_trim(&mut self, id: i64, trim: Trim) -> Result<(), Error> {
        let s = FreeformSpec { trim, ..self.freeform(id)? };
        self.update_freeform(id, &s)
    }

    /// Scene mesh in world coordinates.
    pub fn freeform_mesh(&self, id: i64) -> Result<FreeformMesh, Error> {
        let s = self.freeform(id)?;
        mesh_of(&s, elev(&self.db, s.level)?)
    }

    pub fn freeform_quantities(&self, id: i64) -> Result<FreeformQty, Error> {
        let s = self.freeform(id)?;
        qty_of(&s, elev(&self.db, s.level)?)
    }

    /// Replaces the patch by a plain mesh element (same level and name);
    /// returns the mesh id and the loss lines. One undo step brings the patch back.
    pub fn freeform_to_mesh(&mut self, id: i64) -> Result<(i64, Vec<String>), Error> {
        let s = self.freeform(id)?;
        let fm = mesh_of(&s, elev(&self.db, s.level)?)?;
        let mut loss = fm.loss.clone();
        loss.push(format!("control net {}x{} and trims dropped", s.surface.nu(), s.surface.nv()));
        let hosted = self.panel_systems_on(id)?.len();
        if hosted > 0 {
            loss.push(format!("panel systems: {hosted} on the surface deleted with it"));
        }
        let shells = self.shells_on(id)?.len();
        if shells > 0 {
            loss.push(format!("roofs and slabs: {shells} on the surface keep its last shape, flagged orphan"));
        }
        let (pos, idx) = crate::mesh_blobs(&fm.mesh);
        let new = self.op("freeform_to_mesh", |db| {
            let inv = Inv::default().restore(db, "freeform", "id", id)?;
            db.execute(
                "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,'Freeform',?2,?3,?4)",
                params![s.level, s.name, pos, idx],
            )?;
            let new = db.last_insert_rowid();
            db.execute("DELETE FROM freeform WHERE id=?1", [id])?;
            let inv = crate::panel_system::drop_hosted(db, id, inv)?;
            let inv = crate::shell_elem::sync_host(db, id, inv)?;
            let inv = crate::recipe::unlink(db, "freeform", id, inv.delete("mesh", "id", new))?;
            Ok((new, id, inv))
        })?;
        Ok((new, loss))
    }
}

#[cfg(test)]
#[path = "freeform_tests.rs"]
mod tests;
