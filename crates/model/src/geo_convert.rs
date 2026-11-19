//! One conversion between geometry kinds (mesh, parametric solid, native element) with a loss
//! report: every receipt lists exactly what is dropped or approximated (`loss: [{what,
//! measure}]`). A dry run executes the same code in a transaction that is rolled back, so its
//! report equals the applied one; the apply is one journal operation (one undo).

use ddc_geom::arc::BulgeRing;
use ddc_geom::gsolid::{Form, Section};
use ddc_geom::mesh_fit::{self, Candidate, Kind, Options};
use ddc_geom::mesh_ops;
use rusqlite::{params, types::Value, Connection};

use crate::mesh_edit::{
    convert_one, has_table, info, keep_props, repoint, unmark, watermarks, CONVERT_TABLES,
};
use crate::ops::Inv;
use crate::{Error, Model};

/// Weld distance for the closed/manifold check, m.
const WELD: f64 = 1e-6;
/// Fit tolerance when the request gives none: deviation thickness over the mesh area, m.
pub const DEFAULT_TOL: f64 = 1e-3;
/// Category of a mesh made from a solid.
pub const SOLID_MESH_CATEGORY: &str = "Generic";
/// Property that keeps the material of a solid turned into a mesh.
pub const SOLID_MATERIAL: &str = "ddc.material";

/// Target kind of a conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConvertTo {
    Solid,
    Mesh,
    Parametric,
    Copies,
    Path,
}

impl ConvertTo {
    pub const ALL: [ConvertTo; 5] = [
        ConvertTo::Solid,
        ConvertTo::Mesh,
        ConvertTo::Parametric,
        ConvertTo::Copies,
        ConvertTo::Path,
    ];

    pub fn key(self) -> &'static str {
        match self {
            ConvertTo::Solid => "solid",
            ConvertTo::Mesh => "mesh",
            ConvertTo::Parametric => "parametric",
            ConvertTo::Copies => "copies",
            ConvertTo::Path => "path",
        }
    }

    pub fn parse(s: &str) -> Option<ConvertTo> {
        ConvertTo::ALL.into_iter().find(|k| k.key() == s)
    }
}

/// Typed convert request. `table` is the source kind (`mesh` for imported or edited meshes,
/// `solid` for parametric solids).
#[derive(Clone, Debug, PartialEq)]
pub struct ConvertRequest {
    pub table: String,
    pub id: i64,
    pub to: ConvertTo,
    /// Allowed deviation thickness, m (volume deviation over the mesh area); <= 0 -> default.
    pub tol: f64,
    /// Preferred native kind for `Parametric`; None -> best candidate.
    pub hint: Option<Kind>,
    /// Keep the source element next to the result.
    pub keep_source: bool,
}

impl ConvertRequest {
    pub fn new(table: &str, id: i64, to: ConvertTo) -> Self {
        Self {
            table: table.to_string(),
            id,
            to,
            tol: DEFAULT_TOL,
            hint: None,
            keep_source: false,
        }
    }

    fn tol(&self) -> f64 {
        if self.tol.is_finite() && self.tol > 0.0 {
            self.tol
        } else {
            DEFAULT_TOL
        }
    }
}

/// One dropped or approximated item: what and how much.
#[derive(Clone, Debug, PartialEq)]
pub struct Loss {
    pub what: String,
    pub measure: String,
}

fn loss(what: impl Into<String>, measure: impl Into<String>) -> Loss {
    Loss {
        what: what.into(),
        measure: measure.into(),
    }
}

/// Receipt of a conversion; the same for a dry run and the apply.
#[derive(Clone, Debug, PartialEq)]
pub struct LossReport {
    pub from_table: String,
    pub from_id: i64,
    pub to: ConvertTo,
    /// Table of the new element (`mesh`, `solid`, `wall`, `slab`, `roof`, `col`, `beam`).
    pub result_table: String,
    /// Empty: nothing lost.
    pub loss: Vec<Loss>,
}

impl LossReport {
    pub fn lossless(&self) -> bool {
        self.loss.is_empty()
    }
}

fn refuse<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

/// Why a pair is not converted (checked before anything is read or written).
fn unsupported(table: &str, to: ConvertTo) -> Option<String> {
    let why = match (table, to) {
        ("mesh", ConvertTo::Solid | ConvertTo::Parametric) | ("solid", ConvertTo::Mesh) => {
            return None
        }
        ("mesh", ConvertTo::Mesh) | ("solid", ConvertTo::Solid) => {
            format!("{table} is already a {}", to.key())
        }
        ("solid", ConvertTo::Parametric) => {
            "solid to parametric: no native fit for solids yet; convert to mesh first".into()
        }
        (_, ConvertTo::Copies) => {
            format!("{table} to copies: only patterns explode into copies; {table} is not a pattern")
        }
        (_, ConvertTo::Path) => {
            format!("{table} to path: only 2D curves become paths; {table} is not a curve")
        }
        _ => format!("{table}: not a convertible kind (mesh, solid)"),
    };
    Some(why)
}

fn kind_of(req: &ConvertRequest) -> &'static str {
    // the op kind selects the derived recompute: solids, or the native element tables
    if req.table == "solid" {
        "solid_convert"
    } else {
        "wall_from_mesh"
    }
}

/// Performs the conversion on `db`; the caller commits (apply) or rolls back (dry run).
fn run(db: &Connection, req: &ConvertRequest) -> Result<(LossReport, i64, Inv), Error> {
    if let Some(why) = unsupported(&req.table, req.to) {
        return refuse(why);
    }
    match req.to {
        ConvertTo::Mesh => solid_to_mesh(db, req),
        _ => mesh_to_native(db, req),
    }
}

fn ring_curved(r: &BulgeRing) -> bool {
    r.bulges.iter().any(|b| *b != 0.0)
}

fn section_curved(s: &Section) -> bool {
    ring_curved(&s.outer) || s.holes.iter().any(ring_curved)
}

fn form_curved(f: &Form) -> bool {
    match f {
        Form::Extrusion { profile, .. } => section_curved(profile),
        Form::Blend { bottom, top, .. } => ring_curved(bottom) || ring_curved(top),
        Form::Revolve { .. } => true,
        Form::Sweep { profile, path } => {
            section_curved(profile) || path.bulges.iter().any(|b| *b != 0.0)
        }
        Form::SweptBlend { bottom, top, path } => {
            ring_curved(bottom) || ring_curved(top) || path.bulges.iter().any(|b| *b != 0.0)
        }
    }
}

fn solid_to_mesh(db: &Connection, req: &ConvertRequest) -> Result<(LossReport, i64, Inv), Error> {
    let id = req.id;
    let s = crate::solids::read(db, id)?;
    if s.void {
        return refuse(format!(
            "solid {id} is a void: as a mesh it would add material instead of cutting"
        ));
    }
    let m = crate::solids::mesh_of(db, &s)?;
    if m.triangles.is_empty() {
        return refuse(format!("solid {id} has no body to tessellate"));
    }
    let mut lost = vec![loss(
        "parameters",
        format!(
            "{} definition and work plane are no longer editable",
            s.form.kind()
        ),
    )];
    if form_curved(&s.form) {
        lost.push(loss(
            "curved faces",
            format!("faceted into {} triangles", m.triangles.len()),
        ));
    }
    let (cuts, cut_vol): (i64, f64) = db.query_row(
        "SELECT count(*), COALESCE(SUM(volume),0) FROM solid_cut WHERE kind='solid' AND ref=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if cuts > 0 {
        lost.push(loss(
            "boolean tree",
            format!("{cuts} void cut(s) dropped; {cut_vol:.6} m3 no longer subtracted"),
        ));
    }
    let (p, i) = crate::mesh_blobs(&m);
    db.execute(
        "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,?2,?3,?4,?5)",
        params![s.level, SOLID_MESH_CATEGORY, s.name, p, i],
    )?;
    let new = db.last_insert_rowid();
    db.execute(
        "INSERT OR IGNORE INTO prop(kind,ref,key,value,unit) \
         SELECT 'mesh', ?1, key, value, unit FROM prop WHERE kind='solid' AND ref=?2",
        params![new, id],
    )?;
    if !s.material.is_empty() {
        db.execute(
            "INSERT OR IGNORE INTO prop(kind,ref,key,value,unit) VALUES ('mesh',?1,?2,?3,'')",
            params![new, SOLID_MATERIAL, s.material],
        )?;
    }
    let mut inv = Inv::default()
        .sql(
            "DELETE FROM prop WHERE kind='mesh' AND ref=?1",
            vec![Value::Integer(new)],
        )
        .delete("mesh", "id", new);
    if !req.keep_source {
        let back = keep_props(db, "solid", id, Inv::default())?
            .restore(db, "solid", "id", id)?
            .restore(db, "solid_host", "solid", id)?;
        let back = crate::recipe::unlink(db, "solid", id, back)?;
        let back = crate::recipe::drop_role_comp(db, id, back)?;
        let back = repoint(db, ("solid", id), ("mesh", new), back)?;
        db.execute("DELETE FROM prop WHERE kind='solid' AND ref=?1", [id])?;
        db.execute("DELETE FROM solid_host WHERE solid=?1", [id])?;
        db.execute("DELETE FROM solid WHERE id=?1", [id])?;
        inv = inv.then(back);
    }
    let report = LossReport {
        from_table: req.table.clone(),
        from_id: id,
        to: req.to,
        result_table: "mesh".into(),
        loss: lost,
    };
    Ok((report, new, inv))
}

fn pick(cands: Vec<Candidate>, solid: bool, hint: Option<Kind>) -> Option<Candidate> {
    cands.into_iter().find(|c| match (solid, hint) {
        (true, _) => c.kind() == Kind::Extrusion,
        (false, Some(h)) => c.kind() == h,
        (false, None) => c.kind() != Kind::Extrusion,
    })
}

fn mesh_to_native(db: &Connection, req: &ConvertRequest) -> Result<(LossReport, i64, Inv), Error> {
    let id = req.id;
    let solid = req.to == ConvertTo::Solid;
    let src = info(db, id)?;
    let st = mesh_ops::stats(&src.mesh, WELD);
    if solid && (!st.closed || st.nonmanifold_edges > 0) {
        return refuse(format!(
            "mesh {id} is not a closed manifold: {} open edge(s), {} non-manifold edge(s)",
            st.boundary_edges, st.nonmanifold_edges
        ));
    }
    let solids = has_table(db, "solid")?;
    if solid && !solids {
        return refuse("parametric solids are not available in this model");
    }
    let hint = if solid { Some(Kind::Extrusion) } else { req.hint };
    let cands = mesh_fit::recognize_category(&src.mesh, &src.category, hint, &Options::default());
    let Some(c) = pick(cands, solid, hint) else {
        let what = match hint {
            Some(k) => k.key(),
            None => "native element",
        };
        return refuse(format!("mesh {id}: no {what} fits this mesh"));
    };
    let tol = req.tol();
    let mesh_vol = if st.closed {
        st.volume.abs()
    } else {
        c.mesh_volume
    };
    let dev = (c.fit_volume - mesh_vol).abs();
    let allowed = tol * st.area;
    let mut lost = vec![];
    if dev > allowed {
        let measure = format!(
            "volume {mesh_vol:.6} m3 -> {:.6} m3: deviation {dev:.6} m3 over {allowed:.6} m3 \
             allowed (tolerance {tol} m)",
            c.fit_volume
        );
        if solid {
            return refuse(format!(
                "mesh {id} is not a prism within tolerance ({measure}); use parametric for a \
                 reviewed fit"
            ));
        }
        lost.push(loss("geometry beyond fit tolerance", measure));
    }
    for n in &c.notes {
        lost.push(loss(format!("fit: {n}"), "approximated by the native element"));
    }
    let mut tables: Vec<&str> = CONVERT_TABLES.to_vec();
    if solids {
        tables.push("solid");
    }
    let marks = watermarks(db, &tables)?;
    let (done, back, warn) = convert_one(db, id, &c.fit, req.keep_source)?;
    lost.extend(warn.into_iter().map(|w| loss("detail", w)));
    let report = LossReport {
        from_table: req.table.clone(),
        from_id: id,
        to: req.to,
        result_table: done.table.to_string(),
        loss: lost,
    };
    Ok((report, done.id, unmark(&marks).then(back)))
}

impl Model {
    /// Dry run: the full conversion inside a transaction that is rolled back. The model is
    /// unchanged; the report equals what [`Self::convert_kind`] returns on the same state.
    pub fn convert_kind_dry(&self, req: &ConvertRequest) -> Result<LossReport, Error> {
        if let Some(why) = unsupported(&req.table, req.to) {
            return refuse(why);
        }
        if !self.db.is_autocommit() {
            return refuse("convert dry run: a transaction is already open");
        }
        self.db.execute_batch("BEGIN")?;
        let out = run(&self.db, req);
        self.db.execute_batch("ROLLBACK")?;
        out.map(|(r, _, _)| r)
    }

    /// Converts one element as one operation (one undo); returns the report and the new id.
    /// A refusal writes nothing.
    pub fn convert_kind(&mut self, req: &ConvertRequest) -> Result<(LossReport, i64), Error> {
        if let Some(why) = unsupported(&req.table, req.to) {
            return refuse(why);
        }
        self.op(kind_of(req), |db| {
            let (r, id, inv) = run(db, req)?;
            Ok(((r, id), id, inv))
        })
    }
}

#[cfg(test)]
#[path = "geo_convert_tests.rs"]
mod tests;
