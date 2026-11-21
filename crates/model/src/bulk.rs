// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A batch of edits across all categories (XLSX import) as one journal operation: one undo.

use std::collections::HashSet;

use rusqlite::{params, Connection};

use crate::ops::Inv;
use crate::structure::{check_beam, check_column, insert_beam, insert_column};
use crate::{
    check_bulge, check_curtain, BeamSpec, ColumnSpec, CurtainSpec, Error, LayerSpec, Model,
    OpeningSpec, RoomSpec, SlabSpec, Swing, WallSpec,
};

#[derive(Clone, Debug)]
pub enum Edit {
    Level {
        id: i64,
        name: String,
        elevation: f64,
    },
    NewLevel {
        name: String,
        elevation: f64,
    },
    WallType {
        id: i64,
        name: String,
    },
    Layer {
        wtype: i64,
        pos: i64,
        layer: LayerSpec,
    },
    /// Floor, roof and ceiling type: name and the whole layer list, as `Model::set_slab_type`.
    SlabType {
        id: i64,
        name: String,
        layers: Vec<LayerSpec>,
    },
    /// Properties of the material `name` (a new `spec.name` renames every reference), as
    /// `Model::set_material`.
    Material {
        name: String,
        spec: crate::MaterialSpec,
    },
    /// `bulge` is tan(angle/4) of the axis (0 is straight); validated by `check_bulge`.
    Wall {
        id: i64,
        spec: WallSpec,
        bulge: f64,
    },
    NewWall {
        spec: WallSpec,
        bulge: f64,
    },
    /// Lean (radians toward the left normal of a→b) and top/base thickness ratio of a wall with
    /// a flat top, straight or arc (`walls::check_lean`).
    WallShape {
        id: i64,
        lean: f64,
        top_scale: f64,
    },
    /// Generic delete of one row by table (`wall`, `opening`, `slab`); other tables are refused.
    Delete {
        table: &'static str,
        id: i64,
    },
    Opening {
        id: i64,
        wall: i64,
        spec: OpeningSpec,
    },
    NewOpening {
        wall: i64,
        spec: OpeningSpec,
        swing: Swing,
    },
    OpeningSwing {
        id: i64,
        swing: Swing,
    },
    Room {
        id: i64,
        spec: RoomSpec,
    },
    NewRoom(RoomSpec),
    DeleteRoom(i64),
    Slab {
        id: i64,
        level: i64,
        slab_type: i64,
        offset: f64,
    },
    Column {
        id: i64,
        spec: ColumnSpec,
    },
    NewColumn(ColumnSpec),
    DeleteColumn(i64),
    Beam {
        id: i64,
        spec: BeamSpec,
    },
    NewBeam(BeamSpec),
    DeleteBeam(i64),
    /// Roof elevation/overhang/fascia board; attached walls are recomputed.
    Roof {
        id: i64,
        offset: f64,
        overhang: f64,
        fascia: f64,
    },
    /// New outer outline; holes are passed through unchanged.
    SlabBoundary {
        id: i64,
        outer: Vec<(f64, f64)>,
        holes: Vec<Vec<(f64, f64)>>,
    },
    NewSlab(SlabSpec),
    /// Properties without geometry: the grid and panels are untouched.
    CurtainProps {
        id: i64,
        level: i64,
        name: String,
        base_offset: f64,
    },
    NewCurtain(CurtainSpec),
    DeleteCurtain(i64),
    /// A new project parameter (a new workbook column confirmed in the preview).
    NewParam(crate::ParamSpec),
    /// Value of a project parameter by name (may be created by the same batch); `kind` is
    /// the row's table (for a type parameter, the type table); None clears it.
    ParamValue {
        param: String,
        kind: String,
        id: i64,
        value: Option<crate::PVal>,
    },
    /// Names the element created by the nearest earlier `New*` edit of `kind` in this batch
    /// after its workbook row: the id it returns is what `ParamValueNew` finds by `row`.
    Mark { kind: String, row: u32 },
    /// Parameter value of the element made for workbook row `row` of `kind` (see `Mark`): a
    /// new row has no id yet.
    ParamValueNew {
        param: String,
        kind: String,
        row: u32,
        value: Option<crate::PVal>,
    },
    /// Element phases: ids of the creation and demolition phases.
    Phase {
        kind: String,
        id: i64,
        created: i64,
        demolished: Option<i64>,
    },
}

/// Level id of a level made by the same batch: `-(workbook row)`, resolved through `Mark`.
pub fn new_level_ref(row: u32) -> i64 {
    -i64::from(row)
}

fn level_of(l: i64, marks: &[(String, u32, i64)]) -> Result<i64, Error> {
    if l >= 0 {
        return Ok(l);
    }
    marks
        .iter()
        .find(|(t, r, _)| t == "level" && i64::from(*r) == -l)
        .map(|m| m.2)
        .ok_or_else(|| Error::Invalid(format!("no new level for row {}", -l)))
}

/// Copy of `e` with batch-local level references replaced by real ids; `None` if it has none.
fn resolve_levels(e: &Edit, marks: &[(String, u32, i64)]) -> Result<Option<Edit>, Error> {
    let cur = match e {
        Edit::Wall { spec, .. } | Edit::NewWall { spec, .. } => spec.level,
        Edit::Room { spec, .. } | Edit::NewRoom(spec) => spec.level,
        Edit::NewSlab(spec) => spec.level,
        Edit::NewCurtain(spec) => spec.level,
        Edit::Slab { level, .. } | Edit::CurtainProps { level, .. } => *level,
        _ => return Ok(None),
    };
    if cur >= 0 {
        return Ok(None);
    }
    let id = level_of(cur, marks)?;
    let mut o = e.clone();
    match &mut o {
        Edit::Wall { spec, .. } | Edit::NewWall { spec, .. } => spec.level = id,
        Edit::Room { spec, .. } | Edit::NewRoom(spec) => spec.level = id,
        Edit::NewSlab(spec) => spec.level = id,
        Edit::NewCurtain(spec) => spec.level = id,
        Edit::Slab { level, .. } | Edit::CurtainProps { level, .. } => *level = id,
        _ => {}
    }
    Ok(Some(o))
}

impl Edit {
    /// (table, id, deletion) of the row the edit changes; `None` for new rows.
    pub(crate) fn target(&self) -> Option<(&'static str, i64, bool)> {
        Some(match self {
            Edit::Wall { id, .. } | Edit::WallShape { id, .. } => ("wall", *id, false),
            Edit::Delete { table, id } => (*table, *id, true),
            Edit::Opening { id, .. } | Edit::OpeningSwing { id, .. } => ("opening", *id, false),
            Edit::Room { id, .. } => ("room", *id, false),
            Edit::DeleteRoom(id) => ("room", *id, true),
            Edit::Slab { id, .. } | Edit::SlabBoundary { id, .. } => ("slab", *id, false),
            Edit::Column { id, .. } => ("col", *id, false),
            Edit::DeleteColumn(id) => ("col", *id, true),
            Edit::Beam { id, .. } => ("beam", *id, false),
            Edit::DeleteBeam(id) => ("beam", *id, true),
            Edit::Roof { id, .. } => ("roof", *id, false),
            Edit::CurtainProps { id, .. } => ("curtain_wall", *id, false),
            Edit::DeleteCurtain(id) => ("curtain_wall", *id, true),
            _ => return None,
        })
    }

    /// (table, id) of the row this edit deletes; None for any other edit.
    pub fn deletes(&self) -> Option<(&'static str, i64)> {
        self.target().filter(|t| t.2).map(|t| (t.0, t.1))
    }
}

fn snap(
    db: &Connection,
    inv: Inv,
    seen: &mut HashSet<(&'static str, &'static str, i64)>,
    table: &'static str,
    col: &'static str,
    id: i64,
) -> Result<Inv, Error> {
    if seen.insert((table, col, id)) {
        inv.restore(db, table, col, id)
    } else {
        Ok(inv)
    }
}

fn changed(n: usize, what: &str, id: i64) -> Result<(), Error> {
    if n == 0 {
        return Err(Error::Invalid(format!("no such {what} {id}")));
    }
    Ok(())
}

/// Renamed levels and wall types first get a temporary unique name (snapshot taken before),
/// so a swap A<->B or a rename cycle never meets a name the batch is about to free.
fn park_renamed(
    db: &Connection,
    mut inv: Inv,
    seen: &mut HashSet<(&'static str, &'static str, i64)>,
    edits: &[Edit],
) -> Result<Inv, Error> {
    for e in edits {
        let (table, id, name) = match e {
            Edit::Level { id, name, .. } => ("level", *id, name),
            Edit::WallType { id, name } => ("wall_type", *id, name),
            Edit::SlabType { id, name, .. } => ("slab_type", *id, name),
            _ => continue,
        };
        inv = snap(db, inv, seen, table, "id", id)?;
        // char(1) never appears in a typed name, so the parked name cannot collide
        let sql = match table {
            "level" => "UPDATE level SET name = char(1) || 'rename-' || id WHERE id=?1 AND name<>?2",
            "slab_type" => {
                "UPDATE slab_type SET name = char(1) || 'rename-' || id WHERE id=?1 AND name<>?2"
            }
            _ => "UPDATE wall_type SET name = char(1) || 'rename-' || id WHERE id=?1 AND name<>?2",
        };
        exec(db, sql, params![id, name])?;
    }
    Ok(inv)
}

/// A name clash reads as such, not as a raw constraint error.
fn taken(e: rusqlite::Error, what: &str, name: &str) -> Error {
    match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            Error::Invalid(format!("{what} name '{name}' is already used"))
        }
        _ => e.into(),
    }
}

impl Model {
    /// Applies all edits in one transaction; any error (including geometry) rolls everything back.
    pub fn apply_edits(&mut self, edits: &[Edit]) -> Result<(), Error> {
        self.op("wall_bulk_edit", |db| {
            let mut inv = Inv::default();
            let mut seen = HashSet::new();
            let mut news: Vec<(&str, &str, i64)> = vec![];
            let mut marks: Vec<(String, u32, i64)> = vec![];
            let mut defs: Option<Vec<crate::ParamDef>> = None;
            let (mut gone_walls, mut gone_slabs) = (vec![], vec![]);
            inv = park_renamed(db, inv, &mut seen, edits)?;
            for e0 in edits {
                let resolved = resolve_levels(e0, &marks)?;
                let e = resolved.as_ref().unwrap_or(e0);
                match e {
                    Edit::NewParam(sp) => {
                        let (_, i) = crate::params::insert_def(db, sp, Inv::default())?;
                        inv = i.then(inv);
                        defs = None;
                    }
                    Edit::ParamValue {
                        param,
                        kind,
                        id,
                        value,
                    } => {
                        if defs.is_none() {
                            defs = Some(crate::params::load_defs(db)?);
                        }
                        let ds = defs.as_deref().unwrap_or_default();
                        let d = ds
                            .iter()
                            .find(|d| d.spec.name == param.trim())
                            .ok_or_else(|| Error::Invalid(format!("no such parameter '{param}'")))?;
                        let set = crate::ParamSet {
                            param: d.id,
                            kind: kind.clone(),
                            id: *id,
                            value: value.clone(),
                        };
                        inv = crate::params::write_value(db, ds, &set, Inv::default())?.then(inv);
                    }
                    Edit::Mark { kind, row } => {
                        let id = news
                            .iter()
                            .rev()
                            .find(|(t, c, _)| *t == kind.as_str() && *c == "id")
                            .map(|x| x.2)
                            .ok_or_else(|| Error::Invalid(format!("no new {kind} to mark row {row}")))?;
                        marks.push((kind.clone(), *row, id));
                    }
                    Edit::ParamValueNew {
                        param,
                        kind,
                        row,
                        value,
                    } => {
                        let id = marks
                            .iter()
                            .find(|(t, r, _)| t == kind && r == row)
                            .map(|x| x.2)
                            .ok_or_else(|| {
                                Error::Invalid(format!("no new {kind} for row {row} of '{param}'"))
                            })?;
                        if defs.is_none() {
                            defs = Some(crate::params::load_defs(db)?);
                        }
                        let ds = defs.as_deref().unwrap_or_default();
                        let d = ds
                            .iter()
                            .find(|d| d.spec.name == param.trim())
                            .ok_or_else(|| Error::Invalid(format!("no such parameter '{param}'")))?;
                        let set = crate::ParamSet {
                            param: d.id,
                            kind: kind.clone(),
                            id,
                            value: value.clone(),
                        };
                        inv = crate::params::write_value(db, ds, &set, Inv::default())?.then(inv);
                    }
                    Edit::Phase {
                        kind,
                        id,
                        created,
                        demolished,
                    } => {
                        let e = crate::ElemPhase {
                            created: *created,
                            demolished: *demolished,
                        };
                        inv = crate::phases::put(db, kind, *id, e, Inv::default())?.then(inv);
                    }
                    Edit::Level { id, name, elevation } => {
                        inv = snap(db, inv, &mut seen, "level", "id", *id)?;
                        let n = exec(
                            db,
                            "UPDATE level SET name=?2, elevation=?3 WHERE id=?1",
                            params![id, name, elevation],
                        )
                        .map_err(|e| taken(e, "level", name))?;
                        changed(n, "level", *id)?;
                    }
                    Edit::NewLevel { name, elevation } => {
                        exec(
                            db,
                            "INSERT INTO level(name, elevation) VALUES (?1,?2)",
                            params![name, elevation],
                        )
                        .map_err(|e| taken(e, "level", name))?;
                        news.push(("level", "id", db.last_insert_rowid()));
                    }
                    Edit::WallType { id, name } => {
                        inv = snap(db, inv, &mut seen, "wall_type", "id", *id)?;
                        let n = exec(
                            db,
                            "UPDATE wall_type SET name=?2 WHERE id=?1",
                            params![id, name],
                        )
                        .map_err(|e| taken(e, "wall type", name))?;
                        changed(n, "wall type", *id)?;
                    }
                    Edit::Layer { wtype, pos, layer } => {
                        inv = snap(db, inv, &mut seen, "wall_type_layer", "wtype", *wtype)?;
                        let n = exec(
                            db,
                            "UPDATE wall_type_layer SET material=?3, thickness=?4 \
                             WHERE wtype=?1 AND pos=?2",
                            params![wtype, pos, layer.material, layer.thickness],
                        )?;
                        changed(n, "layer of type", *wtype)?;
                    }
                    Edit::SlabType { id, name, layers } => {
                        crate::types::check_slab_type(name, layers)?;
                        inv = snap(db, inv, &mut seen, "slab_type", "id", *id)?;
                        // the old layers go before the snapshot comes back: the count may differ
                        if seen.insert(("slab_type_layer", "stype", *id)) {
                            inv = inv
                                .delete("slab_type_layer", "stype", *id)
                                .restore(db, "slab_type_layer", "stype", *id)?;
                        }
                        crate::types::write_slab_type(db, *id, name, layers)?;
                    }
                    Edit::Material { name, spec } => {
                        let (_, i) = crate::materials::write_material(db, name, spec)?;
                        inv = i.then(inv);
                    }
                    Edit::Wall {
                        id,
                        spec: w,
                        bulge,
                    } => {
                        check_bulge(w.a, w.b, *bulge)?;
                        crate::wall_spline::refuse_piece_change(db, *id, w, *bulge)?;
                        inv = snap(db, inv, &mut seen, "wall", "id", *id)?;
                        // an explicit height or an arc resets the top profile; an axis edit is a translation
                        let top = crate::walls::remapped_top(
                            db,
                            *id,
                            w.a,
                            w.b,
                            Some(*bulge),
                            Some(w.height),
                        )?;
                        let n = exec(
                            db,
                            "UPDATE wall SET level=?2,wtype=?3,ax=?4,ay=?5,bx=?6,by=?7,\
                             base_offset=?8,height=?9,offset0=?10,axis_kind=?11,bulge=?12,\
                             top_profile=?13 WHERE id=?1",
                            params![
                                id,
                                w.level,
                                w.wall_type,
                                w.a.0,
                                w.a.1,
                                w.b.0,
                                w.b.1,
                                w.base_offset,
                                w.height,
                                w.offset0,
                                (*bulge != 0.0) as i64,
                                bulge,
                                top
                            ],
                        )?;
                        changed(n, "wall", *id)?;
                    }
                    Edit::NewWall { spec: w, bulge } => {
                        check_bulge(w.a, w.b, *bulge)?;
                        exec(
                            db,
                            "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0,\
                             axis_kind,bulge) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                            params![
                                w.level,
                                w.wall_type,
                                w.a.0,
                                w.a.1,
                                w.b.0,
                                w.b.1,
                                w.base_offset,
                                w.height,
                                w.offset0,
                                (*bulge != 0.0) as i64,
                                bulge
                            ],
                        )?;
                        news.push(("wall", "id", db.last_insert_rowid()));
                    }
                    Edit::WallShape { id, lean, top_scale } => {
                        let l = crate::walls::Lean::new(*lean, *top_scale);
                        crate::wall_spline::refuse_piece_shape(db, *id, *lean, *top_scale)?;
                        crate::walls::check_lean(db, *id, &l)?;
                        inv = snap(db, inv, &mut seen, "wall", "id", *id)?;
                        let n = exec(
                            db,
                            "UPDATE wall SET lean=?2, top_scale=?3 WHERE id=?1",
                            params![id, lean, top_scale],
                        )?;
                        changed(n, "wall", *id)?;
                    }
                    Edit::Delete { table, id } => match *table {
                    "wall" => {
                        inv = snap(db, inv, &mut seen, "wall", "id", *id)?;
                        inv = snap(db, inv, &mut seen, "opening", "wall", *id)?;
                        exec(db, "DELETE FROM opening WHERE wall=?1", [id])?;
                        inv = snap(db, inv, &mut seen, "comp", "host_wall", *id)?;
                        exec(db, "DELETE FROM comp WHERE host_wall=?1", [id])?;
                        changed(exec(db, "DELETE FROM wall WHERE id=?1", [id])?, "wall", *id)?;
                        gone_walls.push(*id);
                    }
                    "slab" => {
                        inv = snap(db, inv, &mut seen, "slab", "id", *id)?;
                        inv = snap(db, inv, &mut seen, "slab_vertex", "slab", *id)?;
                        exec(db, "DELETE FROM slab_vertex WHERE slab=?1", [id])?;
                        inv = snap(db, inv, &mut seen, "comp", "host_slab", *id)?;
                        exec(db, "DELETE FROM comp WHERE host_slab=?1", [id])?;
                        changed(exec(db, "DELETE FROM slab WHERE id=?1", [id])?, "slab", *id)?;
                        gone_slabs.push(*id);
                    }
                    "opening" => {
                        inv = snap(db, inv, &mut seen, "opening", "id", *id)?;
                        let n = exec(db, "DELETE FROM opening WHERE id=?1", [id])?;
                        changed(n, "opening", *id)?;
                    }
                    other => return Err(Error::Invalid(format!("no generic delete for {other}"))),
                    },
                    Edit::Opening { id, wall, spec: o } => {
                        inv = snap(db, inv, &mut seen, "opening", "id", *id)?;
                        let n = exec(
                            db,
                            "UPDATE opening SET along=?2,width=?3,sill=?4,height=?5,kind=?6,wall=?7 \
                             WHERE id=?1",
                            params![id, o.along, o.width, o.sill, o.height, o.kind, wall],
                        )?;
                        changed(n, "opening", *id)?;
                    }
                    Edit::NewOpening { wall, spec: o, swing } => {
                        let host: i64 = db.query_row(
                            "SELECT count(*) FROM wall WHERE id=?1",
                            [wall],
                            |r| r.get(0),
                        )?;
                        changed(host as usize, "wall", *wall)?;
                        exec(
                            db,
                            "INSERT INTO opening(wall,along,width,sill,height,kind,\
                             swing_side,swing_hinge) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                            params![
                                wall,
                                o.along,
                                o.width,
                                o.sill,
                                o.height,
                                o.kind,
                                swing.side,
                                swing.hinge
                            ],
                        )?;
                        news.push(("opening", "id", db.last_insert_rowid()));
                    }
                    Edit::OpeningSwing { id, swing } => {
                        inv = snap(db, inv, &mut seen, "opening", "id", *id)?;
                        let n = exec(
                            db,
                            "UPDATE opening SET swing_side=?2,swing_hinge=?3 WHERE id=?1",
                            params![id, swing.side, swing.hinge],
                        )?;
                        changed(n, "opening", *id)?;
                    }
                    Edit::Room { id, spec: r } => {
                        crate::rooms::check(db, r)?;
                        inv = snap(db, inv, &mut seen, "room", "id", *id)?;
                        let n = exec(
                            db,
                            "UPDATE room SET level=?2,name=?3,number=?4,px=?5,py=?6,height=?7                              WHERE id=?1",
                            params![id, r.level, r.name, r.number, r.point.0, r.point.1, r.height],
                        )?;
                        changed(n, "room", *id)?;
                    }
                    Edit::NewRoom(r) => {
                        crate::rooms::check(db, r)?;
                        exec(
                            db,
                            "INSERT INTO room(level,name,number,px,py,height) VALUES (?1,?2,?3,?4,?5,?6)",
                            params![r.level, r.name, r.number, r.point.0, r.point.1, r.height],
                        )?;
                        news.push(("room", "id", db.last_insert_rowid()));
                    }
                    Edit::DeleteRoom(id) => {
                        inv = snap(db, inv, &mut seen, "room", "id", *id)?;
                        inv = snap(db, inv, &mut seen, "zone_room", "room", *id)?;
                        let zone = crate::zones::zone_of(db, *id)?;
                        exec(db, "DELETE FROM zone_room WHERE room=?1", [id])?;
                        if let Some(z) = zone {
                            if crate::zones::is_empty(db, z)? {
                                inv = snap(db, inv, &mut seen, "zone", "id", z)?;
                                exec(db, "DELETE FROM zone WHERE id=?1", [z])?;
                            }
                        }
                        changed(exec(db, "DELETE FROM room WHERE id=?1", [id])?, "room", *id)?;
                    }
                    Edit::SlabBoundary { id, outer, holes } => {
                        let old = crate::slabs::rings(db, *id)?;
                        let p = crate::slabs::profile_keeping(outer, holes, &old)?;
                        // an unchanged outline does not touch vertices: pinning protects them
                        let new: Vec<Vec<(f64, f64)>> =
                            p.rings().map(|r| r.iter().map(|q| (q.x, q.y)).collect()).collect();
                        if old != new {
                            inv = snap(db, inv, &mut seen, "slab_vertex", "slab", *id)?;
                            exec(db, "DELETE FROM slab_vertex WHERE slab=?1", [id])?;
                            crate::slabs::write_rings(db, *id, &p)?;
                            news.push(("slab_vertex", "slab", *id));
                        }
                    }
                    Edit::NewSlab(sp) => {
                        let p = crate::slabs::check_slab_in(db, sp)?;
                        exec(
                            db,
                            "INSERT INTO slab(level, stype, offset) VALUES (?1,?2,?3)",
                            params![sp.level, sp.slab_type, sp.offset],
                        )?;
                        let id = db.last_insert_rowid();
                        crate::slabs::write_rings(db, id, &p)?;
                        news.push(("slab_vertex", "slab", id));
                        news.push(("slab", "id", id));
                    }
                    Edit::CurtainProps { id, level, name, base_offset } => {
                        inv = snap(db, inv, &mut seen, "curtain_wall", "id", *id)?;
                        let n = exec(
                            db,
                            "UPDATE curtain_wall SET level=?2,name=?3,base_offset=?4 WHERE id=?1",
                            params![id, level, name, base_offset],
                        )?;
                        changed(n, "curtain wall", *id)?;
                    }
                    Edit::NewCurtain(c) => {
                        check_curtain(db, c)?;
                        let id = crate::curtain::insert(db, c, None)?;
                        news.push(("curtain_panel", "cw", id));
                        news.push(("curtain_wall", "id", id));
                    }
                    Edit::DeleteCurtain(id) => {
                        inv = snap(db, inv, &mut seen, "curtain_wall", "id", *id)?;
                        inv = snap(db, inv, &mut seen, "curtain_panel", "cw", *id)?;
                        exec(db, "DELETE FROM curtain_panel WHERE cw=?1", [id])?;
                        let n = exec(db, "DELETE FROM curtain_wall WHERE id=?1", [id])?;
                        changed(n, "curtain wall", *id)?;
                    }
                    Edit::Slab { id, level, slab_type, offset } => {
                        inv = snap(db, inv, &mut seen, "slab", "id", *id)?;
                        let n = exec(
                            db,
                            "UPDATE slab SET level=?2,stype=?3,offset=?4 WHERE id=?1",
                            params![id, level, slab_type, offset],
                        )?;
                        changed(n, "slab", *id)?;
                    }
                    Edit::Column { id, spec } => {
                        check_column(db, spec)?;
                        inv = snap(db, inv, &mut seen, "col", "id", *id)?;
                        let n: i64 = db.query_row("SELECT count(*) FROM col WHERE id=?1", [id], |r| r.get(0))?;
                        changed(n as usize, "column", *id)?;
                        insert_column(db, spec, Some(*id))?;
                    }
                    Edit::NewColumn(spec) => {
                        check_column(db, spec)?;
                        news.push(("col", "id", insert_column(db, spec, None)?));
                    }
                    Edit::DeleteColumn(id) => {
                        inv = snap(db, inv, &mut seen, "col", "id", *id)?;
                        changed(db.execute("DELETE FROM col WHERE id=?1", [id])?, "column", *id)?;
                    }
                    Edit::Beam { id, spec } => {
                        check_beam(db, spec)?;
                        inv = snap(db, inv, &mut seen, "beam", "id", *id)?;
                        let n: i64 = db.query_row("SELECT count(*) FROM beam WHERE id=?1", [id], |r| r.get(0))?;
                        changed(n as usize, "beam", *id)?;
                        insert_beam(db, spec, Some(*id))?;
                    }
                    Edit::NewBeam(spec) => {
                        check_beam(db, spec)?;
                        news.push(("beam", "id", insert_beam(db, spec, None)?));
                    }
                    Edit::DeleteBeam(id) => {
                        inv = snap(db, inv, &mut seen, "beam", "id", *id)?;
                        changed(db.execute("DELETE FROM beam WHERE id=?1", [id])?, "beam", *id)?;
                    }
                    Edit::Roof { id, offset, overhang, fascia } => {
                        if ![*offset, *overhang, *fascia].iter().all(|v| v.is_finite())
                            || *overhang < 0.0
                            || *fascia < 0.0
                        {
                            return Err(Error::Invalid(format!("roof {id}: offset/overhang/fascia")));
                        }
                        inv = snap(db, inv, &mut seen, "roof", "id", *id)?;
                        let n = db.execute(
                            "UPDATE roof SET offset=?2, overhang=?3, fascia=?4 WHERE id=?1",
                            params![id, offset, overhang, fascia],
                        )?;
                        changed(n, "roof", *id)?;
                        crate::roofs::geometry(db, &crate::roofs::load(db, *id)?)?;
                        let walls: Vec<i64> = {
                            let mut st = db.prepare("SELECT id FROM wall WHERE top_roof=?1")?;
                            let r = st.query_map([id], |r| r.get(0))?;
                            r.collect::<Result<_, _>>()?
                        };
                        for w in walls {
                            inv = snap(db, inv, &mut seen, "wall", "id", w)?;
                        }
                        // snapshots are already in inv: reattach writes its own, so duplicates are harmless (INSERT OR REPLACE)
                        inv = crate::roofs::reattach(db, *id, inv)?;
                    }
                }
            }
            inv = crate::roof_edge::drop_hosts(db, inv, "wall", &gone_walls)?;
            inv = crate::roof_edge::drop_hosts(db, inv, "slab", &gone_slabs)?;
            // wall tops under a roof are recomputed once, and only if the edit affects them
            let trims = edits.iter().any(|e| {
                matches!(
                    e,
                    Edit::Level { .. }
                        | Edit::NewLevel { .. }
                        | Edit::SlabType { .. }
                        | Edit::Material { .. }
                        | Edit::Wall { .. }
                        | Edit::NewWall { .. }
                        | Edit::Roof { .. }
                        | Edit::Slab { .. }
                        | Edit::SlabBoundary { .. }
                        | Edit::NewSlab(_)
                        | Edit::WallShape { .. }
                ) || matches!(e, Edit::Delete { table: "wall" | "roof" | "slab", .. })
            });
            // recompute snapshots go before deleting new rows: otherwise an undo would resurrect the new wall
            let trimmed = if trims { crate::roof_auto::sync(db, Inv::default(), None)? } else { Inv::default() };
            // Deletes of new rows go first: a new row may reuse an id freed by a deletion.
            let mut undo = Inv::default();
            for (t, c, id) in news {
                undo = undo.delete(t, c, id);
            }
            inv = trimmed.then(undo).then(inv);
            Ok(((), 0, inv))
        })
    }
}

/// Prepared-statement cache: a batch of 10^4 rows does not re-parse SQL.
fn exec<P: rusqlite::Params>(db: &Connection, sql: &str, p: P) -> rusqlite::Result<usize> {
    db.prepare_cached(sql)?.execute(p)
}

#[cfg(test)]
mod rename_tests {
    use super::*;

    fn names(m: &Model, ids: &[i64]) -> Vec<String> {
        ids.iter()
            .map(|i| {
                m.sql()
                    .query_row("SELECT name FROM level WHERE id=?1", [i], |r| r.get(0))
                    .unwrap()
            })
            .collect()
    }

    fn lv(id: i64, name: &str) -> Edit {
        Edit::Level { id, name: name.into(), elevation: 0.0 }
    }

    #[test]
    fn level_swap_and_cycle_apply_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let ids: Vec<i64> = ["A", "B", "C"].iter().map(|n| m.add_level(n, 0.0).unwrap()).collect();
        m.apply_edits(&[lv(ids[0], "B"), lv(ids[1], "A")]).unwrap();
        assert_eq!(names(&m, &ids), ["B", "A", "C"]);
        m.apply_edits(&[lv(ids[0], "C"), lv(ids[1], "B"), lv(ids[2], "A")]).unwrap();
        assert_eq!(names(&m, &ids), ["C", "B", "A"]);
        m.undo().unwrap();
        assert_eq!(names(&m, &ids), ["B", "A", "C"]);
        m.undo().unwrap();
        assert_eq!(names(&m, &ids), ["A", "B", "C"]);
    }

    #[test]
    fn freed_name_can_be_taken_by_a_new_level() {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("A", 0.0).unwrap();
        let e = [Edit::NewLevel { name: "A".into(), elevation: 3.0 }, lv(a, "Z")];
        m.apply_edits(&e).unwrap();
        assert_eq!(names(&m, &[a]), ["Z"]);
    }

    #[test]
    fn real_clash_is_readable_and_rolls_back() {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("A", 0.0).unwrap();
        let b = m.add_level("B", 0.0).unwrap();
        let err = m.apply_edits(&[lv(a, "B")]).unwrap_err().to_string();
        assert_eq!(err, "invalid: level name 'B' is already used");
        assert_eq!(names(&m, &[a, b]), ["A", "B"]);
        let t = m
            .add_wall_type("T1", &[LayerSpec { material: "m".into(), thickness: 0.2 }])
            .unwrap();
        m.add_wall_type("T2", &[LayerSpec { material: "m".into(), thickness: 0.2 }]).unwrap();
        let err = m
            .apply_edits(&[Edit::WallType { id: t, name: "T2".into() }])
            .unwrap_err()
            .to_string();
        assert_eq!(err, "invalid: wall type name 'T2' is already used");
    }
}
