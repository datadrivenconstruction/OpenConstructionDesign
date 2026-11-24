// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Generic solids (schema 23): extrusion, blend, revolve, sweep, sweep with blend; void variants
//! cut volume out of walls, slabs, roofs and other solids.
//! `solid.def` is the JSON of the solid kind (profiles are rings `[x, y, bulge]`), `solid.plane`
//! is the JSON of the work plane (the z origin is measured from the level elevation). `solid_host`
//! is an explicit list of void hosts (empty means all intersected). Derived: `solid_qty` (volume,
//! area, net, extent), `solid_cut`, `solid_base` (host quantities before void deduction, see
//! `solids_cut.rs`).

use ddc_geom::arc::BulgeRing;
use ddc_geom::gsolid::{self, Form, Frame, Path3, Section};
use ddc_geom::mesh::Mesh;
use ddc_geom::{v2, v3, V2, V3};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::ops::Inv;
use crate::views::{LineClass, ViewPrim};
use crate::{Error, Model};

/// Generic-solid tables; called from `schema::upgrade` (idempotent).
pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS solid (
    id INTEGER PRIMARY KEY,
    level INTEGER NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL,
    void INTEGER NOT NULL DEFAULT 0,
    material TEXT NOT NULL DEFAULT '',
    plane TEXT NOT NULL,
    def TEXT NOT NULL,
    ifc_class TEXT,
    ifc_type TEXT
);
CREATE INDEX IF NOT EXISTS solid_level ON solid(level);
CREATE TABLE IF NOT EXISTS solid_host (
    solid INTEGER NOT NULL,
    kind TEXT NOT NULL,
    ref INTEGER NOT NULL,
    PRIMARY KEY (solid, kind, ref)
);
CREATE TABLE IF NOT EXISTS solid_qty (
    solid INTEGER PRIMARY KEY,
    volume REAL NOT NULL,
    area REAL NOT NULL,
    net_volume REAL NOT NULL,
    x0 REAL NOT NULL, y0 REAL NOT NULL, z0 REAL NOT NULL,
    x1 REAL NOT NULL, y1 REAL NOT NULL, z1 REAL NOT NULL,
    error TEXT
);
CREATE TABLE IF NOT EXISTS solid_cut (
    solid INTEGER NOT NULL,
    kind TEXT NOT NULL,
    ref INTEGER NOT NULL,
    volume REAL NOT NULL,
    PRIMARY KEY (solid, kind, ref)
);
CREATE TABLE IF NOT EXISTS solid_base (
    kind TEXT NOT NULL,
    ref INTEGER NOT NULL,
    pos INTEGER NOT NULL,
    volume REAL NOT NULL,
    area REAL NOT NULL,
    net_volume REAL NOT NULL,
    net_area REAL NOT NULL,
    PRIMARY KEY (kind, ref, pos)
);
";

/// Columns added after the first release of the table (additive, NULL = generic proxy).
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    for c in ["ifc_class", "ifc_type"] {
        if !crate::schema::has_col(db, "solid", c)? {
            db.execute_batch(&format!("ALTER TABLE solid ADD COLUMN {c} TEXT"))?;
        }
    }
    Ok(())
}

/// Host kinds that a void cuts.
pub const HOST_KINDS: [&str; 4] = ["wall", "slab", "roof", "solid"];

/// IFC4 classes a solid may take instead of the generic proxy, with their predefined types
/// (NOTDEFINED is always allowed; USERDEFINED is not, a solid has no user type text).
pub const SOLID_IFC_CLASSES: &[(&str, &[&str])] = &[
    (
        "IfcMember",
        &[
            "BRACE", "CHORD", "COLLAR", "MEMBER", "MULLION", "PLATE", "POST", "PURLIN", "RAFTER",
            "STRINGER", "STRUT", "STUD",
        ],
    ),
    ("IfcPlate", &["CURTAIN_PANEL", "SHEET"]),
    ("IfcSlab", &["FLOOR", "ROOF", "LANDING", "BASESLAB"]),
    ("IfcColumn", &["COLUMN", "PILASTER"]),
    ("IfcBeam", &["BEAM", "JOIST", "HOLLOWCORE", "LINTEL", "SPANDREL", "T_BEAM"]),
    (
        "IfcCovering",
        &[
            "CEILING", "FLOORING", "CLADDING", "ROOFING", "MOLDING", "SKIRTINGBOARD",
            "INSULATION", "MEMBRANE", "SLEEVING", "WRAPPING",
        ],
    ),
    ("IfcRailing", &["HANDRAIL", "GUARDRAIL", "BALUSTRADE"]),
    (
        "IfcFooting",
        &["CAISSON_FOUNDATION", "FOOTING_BEAM", "PAD_FOOTING", "PILE_CAP", "STRIP_FOOTING"],
    ),
    (
        "IfcElementAssembly",
        &[
            "ACCESSORY_ASSEMBLY", "ARCH", "BEAM_GRID", "BRACED_FRAME", "GIRDER",
            "REINFORCEMENT_UNIT", "RIGID_FRAME", "SLAB_FIELD", "TRUSS",
        ],
    ),
    ("IfcShadingDevice", &["JALOUSIE", "SHUTTER", "AWNING"]),
    (
        "IfcMechanicalFastener",
        &[
            "ANCHORBOLT", "BOLT", "DOWEL", "NAIL", "NAILPLATE", "RIVET", "SCREW",
            "SHEARCONNECTOR", "STAPLE", "STUDSHEARCONNECTOR",
        ],
    ),
];

/// Semantic IFC class of a solid: canonical class name ("IfcMember") and predefined type
/// ("CHORD", or "NOTDEFINED").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolidIfc {
    pub class: String,
    pub predefined: String,
}

impl SolidIfc {
    /// Checks `class` (any case, with or without the `Ifc` prefix) and `predefined` against
    /// [`SOLID_IFC_CLASSES`]; the refusal lists what is allowed.
    pub fn new(class: &str, predefined: Option<&str>) -> Result<SolidIfc, Error> {
        let c = class.trim();
        let bare = c.get(..3).filter(|p| p.eq_ignore_ascii_case("ifc")).map_or(c, |_| &c[3..]);
        let Some((name, types)) = SOLID_IFC_CLASSES
            .iter()
            .find(|(n, _)| n[3..].eq_ignore_ascii_case(bare))
        else {
            let all: Vec<&str> = SOLID_IFC_CLASSES.iter().map(|x| x.0).collect();
            return invalid(format!(
                "solid: IFC class {class:?} is not allowed; use one of {}",
                all.join(", ")
            ));
        };
        let p = predefined.map(str::trim).filter(|p| !p.is_empty()).unwrap_or("NOTDEFINED");
        let p = p.to_ascii_uppercase();
        if p != "NOTDEFINED" && !types.contains(&p.as_str()) {
            return invalid(format!(
                "solid: {name} has no predefined type {p:?}; use NOTDEFINED or one of {}",
                types.join(", ")
            ));
        }
        Ok(SolidIfc {
            class: (*name).to_string(),
            predefined: p,
        })
    }

    /// Entity name in a STEP file ("IFCMEMBER").
    pub fn entity(&self) -> String {
        self.class.to_ascii_uppercase()
    }

    /// Index of PredefinedType among the IFC4 attributes of `class`.
    pub fn predefined_at(class: &str) -> usize {
        match class {
            "IfcElementAssembly" => 9,      // after AssemblyPlace
            "IfcMechanicalFastener" => 10,  // after NominalDiameter, NominalLength
            _ => 8,
        }
    }
}

/// Solids sharing one form ([`Model::solid_groups`]): its key and mesh in its own frame, and
/// (id, frame with the level elevation, void) of each solid.
#[derive(Clone, Debug)]
pub struct SolidGroup {
    pub key: u64,
    pub mesh: Result<std::sync::Arc<Mesh>, String>,
    pub items: Vec<(i64, Frame, bool)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SolidSpec {
    pub level: i64,
    pub name: String,
    /// Void: has no volume of its own in the estimate, cuts its hosts.
    pub void: bool,
    pub material: String,
    /// Work plane; `o.z` is measured from the level elevation.
    pub frame: Frame,
    pub form: Form,
    /// Semantic IFC class; None exports a generic proxy. A void has none.
    pub ifc: Option<SolidIfc>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolidQty {
    pub volume: f64,
    pub area: f64,
    /// Volume net of voids (0 for a void).
    pub net_volume: f64,
    /// World extent: [x0, y0, z0, x1, y1, z1].
    pub bbox: [f64; 6],
}

/// A void cutout in a host.
#[derive(Clone, Debug, PartialEq)]
pub struct SolidCut {
    pub kind: String,
    pub id: i64,
    pub volume: f64,
}

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

// ---------- JSON ----------

fn num(v: &Value) -> Result<f64, Error> {
    v.as_f64()
        .filter(|x| x.is_finite())
        .ok_or_else(|| Error::Invalid(format!("solid: expected a number, got {v}")))
}

fn arr(v: &Value) -> Result<&Vec<Value>, Error> {
    v.as_array()
        .ok_or_else(|| Error::Invalid(format!("solid: expected an array, got {v}")))
}

fn ring_json(r: &BulgeRing) -> Value {
    Value::Array(
        r.pts
            .iter()
            .zip(&r.bulges)
            .map(|(p, b)| json!([p.x, p.y, b]))
            .collect(),
    )
}

fn ring_of(v: &Value) -> Result<BulgeRing, Error> {
    let mut r = BulgeRing {
        pts: vec![],
        bulges: vec![],
    };
    for p in arr(v)? {
        let p = arr(p)?;
        if p.len() < 2 {
            return invalid("solid: ring point needs [x, y, bulge]");
        }
        r.pts.push(v2(num(&p[0])?, num(&p[1])?));
        r.bulges.push(p.get(2).map(num).transpose()?.unwrap_or(0.0));
    }
    Ok(r)
}

fn section_json(s: &Section) -> Value {
    json!({
        "outer": ring_json(&s.outer),
        "holes": Value::Array(s.holes.iter().map(ring_json).collect()),
    })
}

fn section_of(v: &Value) -> Result<Section, Error> {
    let holes = match v.get("holes") {
        Some(h) => arr(h)?.iter().map(ring_of).collect::<Result<_, _>>()?,
        None => vec![],
    };
    Ok(Section {
        outer: ring_of(v.get("outer").unwrap_or(&Value::Null))?,
        holes,
    })
}

fn path_json(p: &Path3) -> Value {
    Value::Array(
        p.pts
            .iter()
            .enumerate()
            .map(|(i, q)| json!([q.x, q.y, q.z, p.bulges.get(i).copied().unwrap_or(0.0)]))
            .collect(),
    )
}

fn path_of(v: &Value) -> Result<Path3, Error> {
    let mut p = Path3 {
        pts: vec![],
        bulges: vec![],
        smooth: None,
    };
    for q in arr(v)? {
        let q = arr(q)?;
        if q.len() < 3 {
            return invalid("solid: path point needs [x, y, z, bulge]");
        }
        p.pts.push(v3(num(&q[0])?, num(&q[1])?, num(&q[2])?));
        p.bulges.push(q.get(3).map(num).transpose()?.unwrap_or(0.0));
    }
    p.bulges.pop();
    Ok(p)
}

fn field(v: &Value, k: &str) -> Result<f64, Error> {
    num(v.get(k).unwrap_or(&Value::Null))
}

pub fn form_json(f: &Form) -> Value {
    match f {
        Form::Extrusion {
            profile,
            start,
            end,
        } => {
            json!({"kind": "extrusion", "profile": section_json(profile), "start": start, "end": end})
        }
        Form::Blend {
            bottom,
            top,
            height,
        } => {
            json!({"kind": "blend", "bottom": ring_json(bottom), "top": ring_json(top), "height": height})
        }
        Form::Revolve {
            profile,
            axis_o,
            axis_d,
            start,
            end,
        } => json!({
            "kind": "revolve",
            "profile": section_json(profile),
            "axis": [axis_o.x, axis_o.y, axis_d.x, axis_d.y],
            "start": start,
            "end": end,
        }),
        Form::Sweep { profile, path } => {
            let mut v = json!({"kind": "sweep", "profile": section_json(profile), "path": path_json(path)});
            if let Some(sm) = &path.smooth {
                v["smooth"] = smooth::smooth_json(sm);
            }
            v
        }
        Form::SweptBlend { bottom, top, path } => json!({
            "kind": "swept_blend",
            "bottom": ring_json(bottom),
            "top": ring_json(top),
            "path": path_json(path),
        }),
    }
}

pub fn form_of(v: &Value) -> Result<Form, Error> {
    let g = |k: &str| v.get(k).unwrap_or(&Value::Null);
    Ok(match v.get("kind").and_then(Value::as_str).unwrap_or("") {
        "extrusion" => Form::Extrusion {
            profile: section_of(g("profile"))?,
            start: field(v, "start")?,
            end: field(v, "end")?,
        },
        "blend" => Form::Blend {
            bottom: ring_of(g("bottom"))?,
            top: ring_of(g("top"))?,
            height: field(v, "height")?,
        },
        "revolve" => {
            let a = arr(g("axis"))?;
            if a.len() != 4 {
                return invalid("solid: axis needs [ox, oy, dx, dy]");
            }
            Form::Revolve {
                profile: section_of(g("profile"))?,
                axis_o: v2(num(&a[0])?, num(&a[1])?),
                axis_d: v2(num(&a[2])?, num(&a[3])?),
                start: field(v, "start")?,
                end: field(v, "end")?,
            }
        }
        "sweep" => Form::Sweep {
            profile: section_of(g("profile"))?,
            path: match v.get("smooth").filter(|s| !s.is_null()) {
                Some(s) => Path3::smooth(smooth::smooth_of(s)?),
                None => path_of(g("path"))?,
            },
        },
        "swept_blend" => Form::SweptBlend {
            bottom: ring_of(g("bottom"))?,
            top: ring_of(g("top"))?,
            path: path_of(g("path"))?,
        },
        k => return invalid(format!("solid: unknown kind {k:?}")),
    })
}

fn v3_json(p: V3) -> Value {
    json!([p.x, p.y, p.z])
}

fn v3_of(v: &Value) -> Result<V3, Error> {
    let a = arr(v)?;
    if a.len() != 3 {
        return invalid("solid: vector needs 3 numbers");
    }
    Ok(v3(num(&a[0])?, num(&a[1])?, num(&a[2])?))
}

pub fn frame_json(f: &Frame) -> Value {
    json!({"o": v3_json(f.o), "x": v3_json(f.x), "y": v3_json(f.y), "z": v3_json(f.z)})
}

pub fn frame_of(v: &Value) -> Result<Frame, Error> {
    let g = |k: &str| v.get(k).unwrap_or(&Value::Null);
    Ok(Frame {
        o: v3_of(g("o"))?,
        x: v3_of(g("x"))?,
        y: v3_of(g("y"))?,
        z: v3_of(g("z"))?,
    })
}

// ---------- storage ----------

fn elev(db: &Connection, level: i64) -> Result<f64, Error> {
    db.prepare_cached("SELECT elevation FROM level WHERE id=?1")?
        .query_row([level], |r| r.get(0))
        .optional()?
    .ok_or_else(|| Error::Invalid(format!("no level {level}")))
}

fn geom_err(e: gsolid::FormError) -> Error {
    Error::Degenerate(format!("solid: {e}"))
}

pub(crate) fn check(db: &Connection, s: &SolidSpec) -> Result<(), Error> {
    elev(db, s.level)?;
    if !s.frame.is_valid() {
        return invalid("solid: work plane axes must be orthonormal");
    }
    if s.name.len() > 200 || s.material.len() > 200 {
        return invalid("solid: name or material too long");
    }
    if let Some(c) = &s.ifc {
        if s.void {
            return invalid("solid: a void has no IFC class");
        }
        // stored values go through the same allow-list as requests
        if SolidIfc::new(&c.class, Some(&c.predefined))? != *c {
            return invalid(format!("solid: IFC class {:?} is not canonical", c.class));
        }
    }
    gsolid::validate(&s.form).map_err(geom_err)
}

/// Checked row write for importers that work on a bare connection (inside their transaction).
pub fn put_solid(db: &Connection, s: &SolidSpec, id: Option<i64>) -> Result<i64, Error> {
    check(db, s)?;
    write(db, s, id)
}

pub(crate) fn write(db: &Connection, s: &SolidSpec, id: Option<i64>) -> Result<i64, Error> {
    let (plane, def) = (
        frame_json(&s.frame).to_string(),
        form_json(&s.form).to_string(),
    );
    let (kind, void) = (s.form.kind(), i64::from(s.void));
    let (cls, pre) = match &s.ifc {
        Some(c) => (Some(c.class.as_str()), Some(c.predefined.as_str())),
        None => (None, None),
    };
    let args = params![id, s.level, s.name, kind, void, s.material, plane, def, cls, pre];
    if let Some(id) = id {
        db.prepare_cached(
            "UPDATE solid SET level=?2,name=?3,kind=?4,void=?5,material=?6,plane=?7,def=?8,\
             ifc_class=?9,ifc_type=?10 WHERE id=?1",
        )?
        .execute(args)?;
        return Ok(id);
    }
    db.prepare_cached(
        "INSERT INTO solid(id,level,name,kind,void,material,plane,def,ifc_class,ifc_type) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?
    .execute(args)?;
    Ok(db.last_insert_rowid())
}

pub(crate) fn read(db: &Connection, id: i64) -> Result<SolidSpec, Error> {
    type Row = (i64, String, i64, String, String, String, Option<String>, Option<String>);
    let row: Option<Row> = db
        .prepare_cached(
            "SELECT level,name,void,material,plane,def,ifc_class,ifc_type FROM solid WHERE id=?1",
        )?
        .query_row(
            [id],
            |r| {
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
            },
        )
        .optional()?;
    let Some((level, name, void, material, plane, def, cls, pre)) = row else {
        return invalid(format!("no such solid {id}"));
    };
    let ifc = match cls {
        Some(c) => Some(SolidIfc::new(&c, pre.as_deref())?),
        None => None,
    };
    let def: Value = serde_json::from_str(&def)?;
    let mut form = form_of(&def)?;
    // a path taken from a wall or a slab follows that element
    crate::geo_req::follow(db, &def, level, &mut form);
    Ok(SolidSpec {
        level,
        name,
        void: void != 0,
        material,
        frame: frame_of(&serde_json::from_str(&plane)?)?,
        form,
        ifc,
    })
}

fn ids(db: &Connection, level: Option<i64>) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare("SELECT id FROM solid WHERE ?1 IS NULL OR level=?1 ORDER BY id")?;
    let r = st.query_map([level], |r| r.get(0))?;
    Ok(r.collect::<Result<_, _>>()?)
}

fn hosts_of(db: &Connection, id: i64) -> Result<Vec<(String, i64)>, Error> {
    let mut st =
        db.prepare("SELECT kind, ref FROM solid_host WHERE solid=?1 ORDER BY kind, ref")?;
    let r = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Move/rotate/mirror in plan (`m` is the linear part, `t` the translation): only the work
/// plane changes. Called from `modify`.
pub(crate) fn transform(
    db: &Connection,
    id: i64,
    m: [[f64; 2]; 2],
    t: (f64, f64),
) -> Result<(), Error> {
    let mut s = read(db, id)?;
    let lin = |v: V3| {
        v3(
            m[0][0] * v.x + m[0][1] * v.y,
            m[1][0] * v.x + m[1][1] * v.y,
            v.z,
        )
    };
    let f = &mut s.frame;
    let o = lin(f.o);
    f.o = v3(o.x + t.0, o.y + t.1, f.o.z);
    f.x = lin(f.x);
    f.y = lin(f.y);
    f.z = lin(f.z);
    db.execute(
        "UPDATE solid SET plane=?2 WHERE id=?1",
        params![id, frame_json(&s.frame).to_string()],
    )?;
    Ok(())
}

/// Vertical shift: only the work plane origin moves. Called from `modify`.
pub(crate) fn lift(db: &Connection, id: i64, dz: f64) -> Result<(), Error> {
    let mut s = read(db, id)?;
    s.frame.o = v3(s.frame.o.x, s.frame.o.y, s.frame.o.z + dz);
    db.execute(
        "UPDATE solid SET plane=?2 WHERE id=?1",
        params![id, frame_json(&s.frame).to_string()],
    )?;
    Ok(())
}

// ---------- derived ----------

#[path = "solids_cut.rs"]
mod cut;
#[path = "solids_smooth.rs"]
mod smooth;

pub(crate) use cut::mesh_of;
pub(crate) use cut::{prune_hosts, refresh, regenerate};

// ---------- views ----------

/// Cuts and projection lines of the solids of level `level` in plan at height `cut` above the level.
pub(crate) fn plan_prims(db: &Connection, level: i64, cut: f64) -> Result<Vec<ViewPrim>, Error> {
    let mut out = vec![];
    let z = elev(db, level)? + cut;
    for id in ids(db, Some(level))? {
        // a corrupt definition does not break the view: the error goes to solid_qty.error
        let Ok(s) = read(db, id) else { continue };
        if s.void {
            continue;
        }
        let Ok(m) = mesh_of(db, &s) else { continue };
        for (outer, holes) in gsolid::plan_cut(&m, z) {
            out.push(ViewPrim::SectionCut {
                table: "solid",
                element: id,
                material: s.material.clone(),
                rings: outer.iter().map(|p| (p.x, p.y)).collect(),
                holes: holes
                    .iter()
                    .map(|h| h.iter().map(|p| (p.x, p.y)).collect())
                    .collect(),
            });
        }
        for (a, b) in gsolid::plan_lines(&m, z) {
            out.push(ViewPrim::Edge {
                table: "solid",
                element: id,
                class: LineClass::Projection,
                a: (a.x, a.y),
                b: (b.x, b.y),
            });
        }
    }
    Ok(out)
}

/// Section/elevation: (primitives, points for the extent). `o`, `u` is the view line, `far` is the depth.
pub(crate) fn section_prims(
    db: &Connection,
    o: V2,
    u: V2,
    far: Option<f64>,
    is_cut: bool,
) -> Result<(Vec<ViewPrim>, Vec<V2>), Error> {
    let (mut out, mut pts) = (vec![], vec![]);
    for id in ids(db, None)? {
        let Ok(s) = read(db, id) else { continue };
        if s.void {
            continue;
        }
        let Ok(m) = mesh_of(db, &s) else { continue };
        let n = v2(-u.y, u.x);
        let (lo, hi) = m.positions.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), p| {
            let d = (v2(p.x, p.y) - o).dot(n);
            (l.min(d), h.max(d))
        });
        let cuts = match crate::sections::cut_depth(lo, hi).filter(|_| is_cut) {
            Some(d) => gsolid::section_cut(&m, o + n * d, u),
            None => vec![],
        };
        let class = if !is_cut || !cuts.is_empty() {
            LineClass::Projection
        } else {
            LineClass::Beyond
        };
        for (outer, holes) in cuts {
            pts.extend(outer.iter().copied());
            out.push(ViewPrim::SectionCut {
                table: "solid",
                element: id,
                material: s.material.clone(),
                rings: outer.iter().map(|p| (p.x, p.y)).collect(),
                holes: holes
                    .iter()
                    .map(|h| h.iter().map(|p| (p.x, p.y)).collect())
                    .collect(),
            });
        }
        for (a, b) in gsolid::section_lines(&m, o, u, far) {
            pts.push(a);
            pts.push(b);
            out.push(ViewPrim::Edge {
                table: "solid",
                element: id,
                class,
                a: (a.x, a.y),
                b: (b.x, b.y),
            });
        }
    }
    Ok((out, pts))
}

// ---------- API ----------

impl Model {
    pub fn add_solid(&mut self, s: &SolidSpec) -> Result<i64, Error> {
        check(&self.db, s)?;
        self.op("solid_add", |db| {
            let id = write(db, s, None)?;
            Ok((id, id, Inv::default().delete("solid", "id", id)))
        })
    }

    pub fn update_solid(&mut self, id: i64, s: &SolidSpec) -> Result<(), Error> {
        check(&self.db, s)?;
        self.op("solid_edit", |db| {
            read(db, id)?;
            let inv = Inv::default().restore(db, "solid", "id", id)?;
            write(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_solid(&mut self, id: i64) -> Result<(), Error> {
        // references from other voids to this solid are removed by `prune_hosts` (undo restores them)
        self.op("solid_delete", |db| {
            read(db, id)?;
            let inv = Inv::default().restore(db, "solid", "id", id)?.restore(
                db,
                "solid_host",
                "solid",
                id,
            )?;
            db.execute("DELETE FROM solid_host WHERE solid=?1", [id])?;
            db.execute("DELETE FROM solid WHERE id=?1", [id])?;
            let inv = crate::recipe::unlink(db, "solid", id, inv)?;
            let inv = crate::recipe::drop_role_comp(db, id, inv)?;
            Ok(((), id, inv))
        })
    }

    /// Explicit hosts of a void (empty means it cuts everything it intersects).
    pub fn set_solid_hosts(&mut self, id: i64, hosts: &[(String, i64)]) -> Result<(), Error> {
        for (k, _) in hosts {
            if !HOST_KINDS.contains(&k.as_str()) {
                return invalid(format!("solid: host kind {k:?}"));
            }
        }
        self.op("solid_hosts", |db| {
            read(db, id)?;
            let inv = Inv::default().delete("solid_host", "solid", id).restore(
                db,
                "solid_host",
                "solid",
                id,
            )?;
            db.execute("DELETE FROM solid_host WHERE solid=?1", [id])?;
            for (k, r) in hosts {
                if k == "solid" && *r == id {
                    continue;
                }
                db.execute(
                    "INSERT OR IGNORE INTO solid_host VALUES (?1,?2,?3)",
                    params![id, k, r],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    pub fn solid(&self, id: i64) -> Result<SolidSpec, Error> {
        read(&self.db, id)
    }

    /// Solids of a level (or all): (id, specification); unreadable rows are skipped (see
    /// [`Self::solid_error`]).
    pub fn solids(&self, level: Option<i64>) -> Result<Vec<(i64, SolidSpec)>, Error> {
        Ok(ids(&self.db, level)?
            .into_iter()
            .filter_map(|id| read(&self.db, id).ok().map(|s| (id, s)))
            .collect())
    }

    /// Void solids of all levels, as [`Self::solids`] reads them (cutters need no other rows).
    pub fn void_solids(&self) -> Result<Vec<(i64, SolidSpec)>, Error> {
        let mut st = self.db.prepare("SELECT id FROM solid WHERE void<>0 ORDER BY id")?;
        let ids: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(ids
            .into_iter()
            .filter_map(|id| read(&self.db, id).ok().map(|s| (id, s)))
            .collect())
    }

    /// Solids of a level (or all) grouped by definition: one form mesh in its own frame and
    /// the solids placed with it, for drawing repeated parts (bolts, plates) as instances.
    /// A form that follows another element (a path taken from a wall) is a group of its own;
    /// unreadable rows are left out as in [`Self::solids`].
    pub fn solid_groups(&self, level: Option<i64>) -> Result<Vec<SolidGroup>, Error> {
        type Row = (i64, i64, i64, String, String);
        let rows: Vec<Row> = {
            let mut st = self.db.prepare(
                "SELECT id, level, void, plane, def FROM solid WHERE ?1 IS NULL OR level=?1 ORDER BY id",
            )?;
            let v = st
                .query_map([level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut out: Vec<SolidGroup> = vec![];
        let mut by_def: std::collections::HashMap<String, Option<usize>> = Default::default();
        for (id, lv, void, plane, def) in rows {
            let Ok(mut fr) = serde_json::from_str::<Value>(&plane).map_err(Error::from).and_then(|v| frame_of(&v)) else {
                continue;
            };
            let Ok(z) = elev(&self.db, lv) else { continue };
            fr.o.z += z;
            let one = |form: &Form| {
                let (key, b) = cut::proto_keyed(form);
                SolidGroup { key, mesh: b.mesh.clone(), items: vec![] }
            };
            let at = if def.contains("path_ref") {
                let Ok(s) = read(&self.db, id) else { continue };
                out.push(one(&s.form));
                out.len() - 1
            } else {
                let slot = match by_def.get(&def) {
                    Some(s) => *s,
                    None => {
                        let form = serde_json::from_str::<Value>(&def).map_err(Error::from).and_then(|v| form_of(&v));
                        let s = form.ok().map(|f| {
                            out.push(one(&f));
                            out.len() - 1
                        });
                        by_def.insert(def, s);
                        s
                    }
                };
                let Some(at) = slot else { continue };
                at
            };
            out[at].items.push((id, fr, void != 0));
        }
        Ok(out)
    }

    /// Why a solid was not built (corrupt definition, invalid geometry).
    pub fn solid_error(&self, id: i64) -> Result<Option<String>, Error> {
        let e: Option<Option<String>> = self
            .db
            .query_row("SELECT error FROM solid_qty WHERE solid=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(e.flatten())
    }

    /// Solids with an error: (id, reason), for warnings.
    pub fn solid_errors(&self) -> Result<Vec<(i64, String)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT solid, error FROM solid_qty WHERE error IS NOT NULL ORDER BY solid")?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    pub fn solid_hosts(&self, id: i64) -> Result<Vec<(String, i64)>, Error> {
        hosts_of(&self.db, id)
    }

    pub fn solid_quantities(&self, id: i64) -> Result<Option<SolidQty>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT volume,area,net_volume,x0,y0,z0,x1,y1,z1 FROM solid_qty \
                 WHERE solid=?1 AND error IS NULL",
                [id],
                |r| {
                    Ok(SolidQty {
                        volume: r.get(0)?,
                        area: r.get(1)?,
                        net_volume: r.get(2)?,
                        bbox: [
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                        ],
                    })
                },
            )
            .optional()?)
    }

    pub fn solid_cuts(&self, id: i64) -> Result<Vec<SolidCut>, Error> {
        let mut st = self
            .db
            .prepare("SELECT kind, ref, volume FROM solid_cut WHERE solid=?1 ORDER BY kind, ref")?;
        let r = st.query_map([id], |r| {
            Ok(SolidCut {
                kind: r.get(0)?,
                id: r.get(1)?,
                volume: r.get(2)?,
            })
        })?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// Solid mesh in world coordinates.
    pub fn solid_mesh(&self, id: i64) -> Result<Mesh, Error> {
        Ok((*mesh_of(&self.db, &read(&self.db, id)?)?).clone())
    }

    /// Cuts and lines of a level's solids in plan (`cut` is the cut-plane height above the level).
    pub fn solids_plan(&self, level: i64, cut: f64) -> Result<Vec<ViewPrim>, Error> {
        plan_prims(&self.db, level, cut)
    }
}

#[cfg(test)]
#[path = "solids_tests.rs"]
mod tests;
