// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Walls: types (layers = material + thickness), instances, quantities via ddc-geom.
//! Quantities (`wall_qty`) are derived: recomputed in the same transaction as the edit.

use ddc_geom::arc::{
    arc_faces, arc_walls, wall_quantities, ArcAxis, ArcFace, CHORD_SAG, MIN_BULGE,
};
use ddc_geom::layered::{split_at, split_openings, JoinError, Layer, Opening, Strip};
pub use ddc_geom::wall_lean::Lean;
use ddc_geom::{v2, Tol};
use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

#[derive(Clone, Debug, PartialEq)]
pub struct LayerSpec {
    pub material: String,
    pub thickness: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpeningSpec {
    pub along: f64,
    pub width: f64,
    pub sill: f64,
    pub height: f64,
    /// "door", "window" or "void".
    pub kind: String,
}

/// Door swing side and hinges. `side`: false - the leaf opens to the left of the wall axis
/// a->b, true - to the right; `hinge`: false - hinges at the opening start (`along`), true - at the end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swing {
    pub side: bool,
    pub hinge: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WallSpec {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub level: i64,
    pub base_offset: f64,
    pub height: f64,
    pub wall_type: i64,
    /// Offset of the first layer from the axis to the left; None means the wall is centred on the axis.
    pub offset0: Option<f64>,
}

fn invalid<T>(msg: &str) -> Result<T, Error> {
    Err(Error::Invalid(msg.into()))
}

/// Deletes one wall row with its openings and everything it hosts or owns (roof edge hosts
/// are dropped by the caller for the whole set); a piece of a spline wall leaves its chain.
pub(crate) fn drop_wall_row(db: &Connection, inv: Inv, id: i64) -> Result<Inv, Error> {
    let mut inv = inv.restore(db, "wall", "id", id)?.restore(db, "opening", "wall", id)?;
    if db.execute("DELETE FROM wall WHERE id=?1", [id])? == 0 {
        return invalid(&format!("no such wall {id}"));
    }
    db.execute("DELETE FROM opening WHERE wall=?1", [id])?;
    if crate::mesh_edit::has_table(db, "wall_spline_piece")? {
        inv = inv.restore(db, "wall_spline_piece", "wall", id)?;
        db.execute("DELETE FROM wall_spline_piece WHERE wall=?1", [id])?;
    }
    inv = crate::components::drop_hosted(db, inv, "host_wall", id)?;
    inv = crate::constraints::drop_for_wall(db, inv, id)?;
    inv = crate::wall_edit::drop_wall(db, inv, id)?;
    crate::sketch::drop_owned(db, inv, "wall", id)
}

pub(crate) fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

impl Model {
    /// All levels (id, name, elevation) by ascending elevation.
    pub fn level_list(&self) -> Result<Vec<(i64, String, f64)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id, name, elevation FROM level ORDER BY elevation, id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The lowest level (by elevation, then id): where a script lands when none is named.
    pub fn lowest_level(&self) -> Result<Option<(i64, String, f64)>, Error> {
        Ok(self.level_list()?.into_iter().next())
    }

    pub fn add_level(&mut self, name: &str, elevation: f64) -> Result<i64, Error> {
        if !elevation.is_finite() {
            return invalid("level elevation must be finite");
        }
        self.op("level_add", |db| {
            db.execute(
                "INSERT INTO level(name, elevation) VALUES (?1,?2)",
                params![name, elevation],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("level", "id", id)))
        })
    }

    /// A level's name and elevation; one undo.
    pub fn update_level(&mut self, id: i64, name: &str, elevation: f64) -> Result<(), Error> {
        if name.trim().is_empty() || !elevation.is_finite() {
            return invalid("level needs a name and a finite elevation");
        }
        self.op("level_edit", |db| {
            if !exists(db, "level", id)? {
                return invalid("no such level");
            }
            // UPDATE, not REPLACE: walls reference the level row
            let (old_name, old_z): (String, f64) =
                db.query_row("SELECT name, elevation FROM level WHERE id=?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            // only a new name is checked: imported models may already hold equal names
            if old_name != name {
                let taken: i64 = db.query_row(
                    "SELECT count(*) FROM level WHERE name=?1 AND id<>?2",
                    params![name, id],
                    |r| r.get(0),
                )?;
                if taken > 0 {
                    return invalid("a level with this name already exists");
                }
            }
            let inv = Inv::default().sql(
                "UPDATE level SET name=?2, elevation=?3 WHERE id=?1",
                vec![
                    Value::Integer(id),
                    Value::Text(old_name),
                    Value::Real(old_z),
                ],
            );
            db.execute(
                "UPDATE level SET name=?2, elevation=?3 WHERE id=?1",
                params![id, name, elevation],
            )?;
            let inv = inv.then(crate::constraints::propagate_levels(db, id)?);
            Ok(((), id, inv))
        })
    }

    pub fn add_wall_type(&mut self, name: &str, layers: &[LayerSpec]) -> Result<i64, Error> {
        if layers.is_empty()
            || layers
                .iter()
                .any(|l| !(l.thickness.is_finite() && l.thickness > 0.0))
        {
            return invalid("wall type needs layers with positive thickness");
        }
        self.op("wall_type_add", |db| {
            db.execute("INSERT INTO wall_type(name) VALUES (?1)", [name])?;
            let id = db.last_insert_rowid();
            for (pos, l) in layers.iter().enumerate() {
                db.execute(
                    "INSERT INTO wall_type_layer VALUES (?1,?2,?3,?4)",
                    params![id, pos as i64, l.material, l.thickness],
                )?;
            }
            let inv = Inv::default()
                .delete("wall_type_layer", "wtype", id)
                .delete("wall_type", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn add_wall(&mut self, w: &WallSpec) -> Result<i64, Error> {
        self.add_arc_wall(w, 0.0)
    }

    /// A wall with an arc axis a->b: bulge = tan(angle/4), > 0 is counter-clockwise, 0 is straight.
    pub fn add_arc_wall(&mut self, w: &WallSpec, bulge: f64) -> Result<i64, Error> {
        let ok = w.height.is_finite()
            && w.height > 0.0
            && [w.a.0, w.a.1, w.b.0, w.b.1, w.base_offset]
                .iter()
                .all(|v| v.is_finite())
            && exists(&self.db, "level", w.level)?
            && exists(&self.db, "wall_type", w.wall_type)?;
        if !ok {
            return invalid("wall: bad height, level or type");
        }
        check_bulge(w.a, w.b, bulge)?;
        self.op("wall_add", |db| {
            db.execute(
                "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0,axis_kind,bulge)                  VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
                    (bulge != 0.0) as i64,
                    bulge
                ],
            )?;
            let id = db.last_insert_rowid();
            let inv = Inv::default().delete("wall", "id", id);
            let inv = crate::roof_auto::sync(db, inv, Some(&[id]))?;
            Ok((id, id, inv))
        })
    }

    /// Changes the axis curvature with the same ends; 0 is straight. Undoable.
    pub fn set_wall_bulge(&mut self, id: i64, bulge: f64) -> Result<(), Error> {
        crate::wall_spline::refuse_piece(&self.db, id)?;
        let (a, b) = wall_ends(&self.db, id)?;
        check_bulge(a, b, bulge)?;
        let same = self.wall_bulge(id)? == bulge;
        self.op("wall_bulge", |db| {
            let inv = Inv::default().restore(db, "wall", "id", id)?;
            // u runs along the axis length: another curvature means another length, so the profile is reset (undo
            // restores it); a rule-driven wall is free again and will re-bind
            db.execute(
                "UPDATE wall SET axis_kind=?2, bulge=?3, \
                 top_profile=CASE WHEN ?4 THEN top_profile END, \
                 top_core=CASE WHEN ?4 THEN top_core END, \
                 top_roof=CASE WHEN ?4 THEN top_roof END, \
                 top_auto=CASE WHEN ?4 THEN top_auto WHEN top_auto = 1 THEN 0 ELSE top_auto END WHERE id=?1",
                params![id, (bulge != 0.0) as i64, bulge, same],
            )?;
            let inv = crate::roof_auto::sync(db, inv, Some(&[id]))?;
            Ok(((), id, inv))
        })
    }

    /// Top polyline of a straight wall (u along the axis from `a`, h above the bottom; ascending u); `None`
    /// is a flat top at `height`. For an arc u is the length along the axis.
    pub fn set_wall_top_profile(
        &mut self,
        id: i64,
        top: Option<&[(f64, f64)]>,
    ) -> Result<(), Error> {
        crate::wall_spline::refuse_piece(&self.db, id)?;
        let text = match top {
            Some(t) => {
                if !top_profile_ok(t) {
                    return invalid(
                        "wall top profile: need >= 2 points, u ascending, h > 0",
                    );
                }
                Some(top_profile_text(t))
            }
            None => None,
        };
        self.op("wall_top", |db| {
            let inv = Inv::default().restore(db, "wall", "id", id)?;
            db.execute(
                "UPDATE wall SET top_profile=?2, top_core=NULL, top_auto=CASE WHEN ?2 IS NULL THEN 2 ELSE 0 END WHERE id=?1",
                params![id, text],
            )?;
            Ok(((), id, inv))
        })
    }

    /// Lean (radians toward the left normal of a→b) and top/base thickness ratio of a wall.
    pub fn wall_lean(&self, id: i64) -> Result<Lean, Error> {
        stored_lean(&self.db, id)
    }

    /// Sets the lean and taper of a straight wall with a flat top; one undo.
    pub fn set_wall_lean(&mut self, id: i64, lean: Lean) -> Result<(), Error> {
        self.apply_edits(&[crate::Edit::WallShape {
            id,
            lean: lean.angle,
            top_scale: lean.top_scale,
        }])
    }

    pub fn wall_top_profile(&self, id: i64) -> Result<Option<Vec<(f64, f64)>>, Error> {
        let t: Option<String> =
            self.db
                .query_row("SELECT top_profile FROM wall WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
        Ok(t.as_deref().and_then(parse_top_profile))
    }

    /// The wall axis bulge (0 is straight).
    pub fn wall_bulge(&self, id: i64) -> Result<f64, Error> {
        self.db
            .query_row("SELECT bulge FROM wall WHERE id=?1", [id], |r| r.get(0))
            .map_err(|_| Error::Invalid(format!("no such wall {id}")))
    }

    /// The highest opening top of a wall above its bottom (0 if no openings).
    pub fn openings_head(&self, wall: i64) -> Result<f64, Error> {
        Ok(self.db.query_row(
            "SELECT COALESCE(MAX(sill + height), 0) FROM opening WHERE wall=?1",
            [wall],
            |r| r.get(0),
        )?)
    }

    /// An opening fits its host wall: along the axis within its extent, the sill not below the bottom, the top not
    /// above the wall height (for a wall with a top profile a protrusion is clipped at assembly, not an error).
    pub fn opening_fits(&self, wall: i64, o: &OpeningSpec) -> Result<(), Error> {
        const EPS: f64 = 1e-6;
        let len = self.wall_length(wall)?;
        let (height, profile): (f64, Option<String>) = self.db.query_row(
            "SELECT height, top_profile FROM wall WHERE id=?1",
            [wall],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let along_ok = o.along >= -EPS && o.along + o.width <= len + EPS;
        let head_ok = profile.is_some() || o.sill + o.height <= height + EPS;
        if !along_ok || o.sill < -EPS || !head_ok {
            return Err(Error::Invalid(format!(
                "opening outside its wall {wall}: along {:.3}+{:.3} of {len:.3}, head {:.3} of {height:.3}",
                o.along,
                o.width,
                o.sill + o.height
            )));
        }
        Ok(())
    }

    /// The opening with an edge within the length tolerance of the wall start/end or bottom
    /// moved exactly onto it, so a flush design size is not lost to float rounding.
    pub fn snap_opening(&self, wall: i64, o: &OpeningSpec) -> Result<OpeningSpec, Error> {
        let eps = ddc_geom::math::Tol::default().length;
        let mut o = o.clone();
        let Ok(len) = self.wall_length(wall) else {
            return Ok(o);
        };
        if o.along < 0.0 && o.along >= -eps {
            o.width += o.along;
            o.along = 0.0;
        }
        let end = o.along + o.width;
        if end > len && end <= len + eps {
            o.width = len - o.along;
        }
        if o.sill < 0.0 && o.sill >= -eps {
            o.height += o.sill;
            o.sill = 0.0;
        }
        Ok(o)
    }

    /// Wall axis length: along the arc for an arc wall, exact.
    pub fn wall_length(&self, id: i64) -> Result<f64, Error> {
        let (a, b) = wall_ends(&self.db, id)?;
        let bulge = self.wall_bulge(id)?;
        Ok(ddc_geom::arc::axis_length(
            v2(a.0, a.1),
            v2(b.0, b.1),
            bulge,
        ))
    }

    pub fn set_wall_axis(&mut self, id: i64, a: (f64, f64), b: (f64, f64)) -> Result<(), Error> {
        crate::wall_spline::refuse_piece(&self.db, id)?;
        self.op("wall_axis", |db| {
            let inv = write_axis(db, Inv::default(), id, a, b)?;
            let inv = inv.then(crate::constraints::propagate(db, &[id])?);
            Ok(((), id, inv))
        })
    }

    /// Flipping a wall (a<->b): openings and instances on it stay in place in the world,
    /// the sign of distance locks from this wall changes along with its normal.
    pub fn flip_wall(&mut self, id: i64) -> Result<(), Error> {
        crate::wall_spline::refuse_piece(&self.db, id)?;
        let (a, b) = wall_ends(&self.db, id)?;
        let len = self.wall_length(id)?;
        self.op("wall_flip", |db| {
            let mut inv = Inv::default().restore(db, "wall", "id", id)?;
            inv = inv.restore(db, "opening", "wall", id)?;
            inv = inv.restore(db, "comp", "host_wall", id)?;
            db.execute(
                "UPDATE opening SET along=?2-along-width WHERE wall=?1",
                params![id, len],
            )?;
            db.execute(
                "UPDATE comp SET x=?2-x, y=-y, rotation=rotation+?3 WHERE host_wall=?1",
                params![id, len, std::f64::consts::PI],
            )?;
            for (rid, r) in crate::constraints::load(db)? {
                if matches!(
                    r.kind,
                    crate::ConstraintKind::LockDist | crate::ConstraintKind::LockDistGrid
                ) && r.walls[0] == id
                {
                    inv = inv.restore(db, "constraint_rule", "id", rid)?;
                    db.execute("UPDATE constraint_rule SET value=-value WHERE id=?1", [rid])?;
                }
            }
            inv = write_axis(db, inv, id, b, a)?;
            db.execute("UPDATE wall SET bulge=-bulge WHERE id=?1", [id])?;
            let inv = inv.then(crate::constraints::propagate(db, &[id])?);
            Ok(((), id, inv))
        })
    }

    pub fn set_wall_type(&mut self, id: i64, wall_type: i64) -> Result<(), Error> {
        crate::wall_spline::refuse_piece(&self.db, id)?;
        if !exists(&self.db, "wall_type", wall_type)? {
            return invalid("no such wall type");
        }
        self.op("wall_type_set", |db| {
            let inv = Inv::default().restore(db, "wall", "id", id)?;
            let n = db.execute(
                "UPDATE wall SET wtype=?2 WHERE id=?1",
                params![id, wall_type],
            )?;
            if n == 0 {
                return invalid("no such wall");
            }
            Ok(((), id, inv))
        })
    }

    pub fn set_layer_thickness(
        &mut self,
        wall_type: i64,
        pos: i64,
        thickness: f64,
    ) -> Result<(), Error> {
        if !(thickness.is_finite() && thickness > 0.0) {
            return invalid("thickness must be positive");
        }
        self.op("wall_layer_thickness", |db| {
            let old: f64 = db
                .query_row(
                    "SELECT thickness FROM wall_type_layer WHERE wtype=?1 AND pos=?2",
                    params![wall_type, pos],
                    |r| r.get(0),
                )
                .map_err(|_| Error::Invalid("no such layer".into()))?;
            db.execute(
                "UPDATE wall_type_layer SET thickness=?3 WHERE wtype=?1 AND pos=?2",
                params![wall_type, pos, thickness],
            )?;
            let inv = Inv::default().sql(
                "UPDATE wall_type_layer SET thickness=?3 WHERE wtype=?1 AND pos=?2",
                vec![
                    Value::Integer(wall_type),
                    Value::Integer(pos),
                    Value::Real(old),
                ],
            );
            Ok(((), wall_type, inv))
        })
    }

    pub fn delete_wall(&mut self, id: i64) -> Result<(), Error> {
        self.op("wall_delete", |db| {
            let inv = Inv::default()
                .restore(db, "wall", "id", id)?
                .restore(db, "opening", "wall", id)?;
            if db.execute("DELETE FROM wall WHERE id=?1", [id])? == 0 {
                return invalid("no such wall");
            }
            db.execute("DELETE FROM opening WHERE wall=?1", [id])?;
            let inv = crate::components::drop_hosted(db, inv, "host_wall", id)?;
            let inv = crate::constraints::drop_for_wall(db, inv, id)?;
            let inv = crate::wall_edit::drop_wall(db, inv, id)?;
            let inv = crate::sketch::drop_owned(db, inv, "wall", id)?;
            let inv = crate::roof_edge::drop_hosts(db, inv, "wall", &[id])?;
            Ok(((), id, inv))
        })
    }

    /// Several walls in one operation (one undo): the Wall tool gesture.
    pub fn add_walls(&mut self, ws: &[WallSpec]) -> Result<Vec<i64>, Error> {
        self.add_arc_walls(ws, &[])
    }

    /// Like [`Model::add_walls`], wall `i` on an arc of `bulges[i]` (0 straight); an empty
    /// `bulges` keeps every wall straight. One operation, one undo.
    pub fn add_arc_walls(&mut self, ws: &[WallSpec], bulges: &[f64]) -> Result<Vec<i64>, Error> {
        if !bulges.is_empty() && bulges.len() != ws.len() {
            return invalid("walls: one bulge per wall");
        }
        let bulge = |i: usize| bulges.get(i).copied().unwrap_or(0.0);
        for (i, w) in ws.iter().enumerate() {
            check_bulge(w.a, w.b, bulge(i))?;
        }
        for w in ws {
            let ok = w.height.is_finite()
                && w.height > 0.0
                && [w.a.0, w.a.1, w.b.0, w.b.1, w.base_offset]
                    .iter()
                    .all(|v| v.is_finite())
                && exists(&self.db, "level", w.level)?
                && exists(&self.db, "wall_type", w.wall_type)?;
            if !ok {
                return invalid("wall: bad height, level or type");
            }
        }
        if ws.is_empty() {
            return invalid("no walls");
        }
        self.op("wall_add_many", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for (i, w) in ws.iter().enumerate() {
                let k = bulge(i);
                db.execute(
                    "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0,axis_kind,bulge) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
                        (k != 0.0) as i64,
                        k
                    ],
                )?;
                let id = db.last_insert_rowid();
                inv = inv.delete("wall", "id", id);
                ids.push(id);
            }
            let inv = crate::roof_auto::sync(db, inv, Some(&ids))?;
            Ok((ids.clone(), ids[0], inv))
        })
    }

    /// Several walls (with their openings) in one operation.
    pub fn delete_walls(&mut self, ids: &[i64]) -> Result<(), Error> {
        if ids.is_empty() {
            return invalid("no walls");
        }
        self.op("wall_delete_many", |db| {
            let mut inv = Inv::default();
            for &id in ids {
                inv = drop_wall_row(db, inv, id)?;
            }
            inv = crate::roof_edge::drop_hosts(db, inv, "wall", ids)?;
            Ok(((), ids[0], inv))
        })
    }

    pub fn add_opening(&mut self, wall: i64, o: &OpeningSpec) -> Result<i64, Error> {
        self.add_opening_with(wall, o, Swing::default())
    }

    /// An opening with swing side and hinges: one operation.
    pub fn add_opening_with(
        &mut self,
        wall: i64,
        o: &OpeningSpec,
        swing: Swing,
    ) -> Result<i64, Error> {
        check_kind(o)?;
        let o = &self.snap_opening(wall, o)?;
        self.op("wall_opening_add", |db| {
            if !exists(db, "wall", wall)? {
                return invalid(&format!("opening: no wall {wall}"));
            }
            db.execute(
                "INSERT INTO opening(wall,along,width,sill,height,kind,swing_side,swing_hinge) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
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
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("opening", "id", id)))
        })
    }

    /// Openings (host wall, size, swing) as one operation: one journal record, one undo, the
    /// same rows as [`Self::add_opening_with`] one by one. Empty input: no-op.
    pub fn add_openings_with(
        &mut self,
        os: &[(i64, OpeningSpec, Swing)],
    ) -> Result<Vec<i64>, Error> {
        if os.is_empty() {
            return Ok(vec![]);
        }
        for (_, o, _) in os {
            check_kind(o)?;
        }
        let snapped = os
            .iter()
            .map(|(w, o, s)| Ok((*w, self.snap_opening(*w, o)?, *s)))
            .collect::<Result<Vec<_>, Error>>()?;
        let os = &snapped[..];
        self.op("wall_opening_add", |db| {
            let mut has = db.prepare_cached("SELECT count(*) FROM wall WHERE id=?1")?;
            let mut put = db.prepare_cached(
                "INSERT INTO opening(wall,along,width,sill,height,kind,swing_side,swing_hinge) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;
            let mut ids = Vec::with_capacity(os.len());
            let mut inv = Inv::default();
            for (wall, o, swing) in os {
                if has.query_row([wall], |r| r.get::<_, i64>(0))? == 0 {
                    return invalid(&format!("opening: no wall {wall}"));
                }
                put.execute(params![
                    wall,
                    o.along,
                    o.width,
                    o.sill,
                    o.height,
                    o.kind,
                    swing.side,
                    swing.hinge
                ])?;
                let id = db.last_insert_rowid();
                inv = inv.delete("opening", "id", id);
                ids.push(id);
            }
            let first = ids[0];
            Ok((ids, first, inv))
        })
    }

    /// Opening dimensions; the side and hinges are kept.
    pub fn update_opening(&mut self, id: i64, o: &OpeningSpec) -> Result<(), Error> {
        self.update_opening_in(id, o, None)
    }

    /// Dimensions, side and hinges in one operation.
    pub fn update_opening_with(
        &mut self,
        id: i64,
        o: &OpeningSpec,
        swing: Swing,
    ) -> Result<(), Error> {
        self.update_opening_in(id, o, Some(swing))
    }

    fn update_opening_in(
        &mut self,
        id: i64,
        o: &OpeningSpec,
        swing: Option<Swing>,
    ) -> Result<(), Error> {
        check_kind(o)?;
        let host: Option<i64> = self
            .db
            .query_row("SELECT wall FROM opening WHERE id=?1", [id], |r| r.get(0))
            .ok();
        let snapped;
        let o = match host {
            Some(w) => {
                snapped = self.snap_opening(w, o)?;
                &snapped
            }
            None => o,
        };
        if let Some(w) = host {
            self.opening_fits(w, o)
                .map_err(|e| Error::Invalid(format!("wall {w} opening {id}: {e}")))?;
        }
        self.op("wall_opening_update", |db| {
            let inv = Inv::default().restore(db, "opening", "id", id)?;
            let n = db.execute(
                "UPDATE opening SET along=?2,width=?3,sill=?4,height=?5,kind=?6 WHERE id=?1",
                params![id, o.along, o.width, o.sill, o.height, o.kind],
            )?;
            if n == 0 {
                return invalid(&format!("no such opening {id}"));
            }
            if let Some(s) = swing {
                db.execute(
                    "UPDATE opening SET swing_side=?2,swing_hinge=?3 WHERE id=?1",
                    params![id, s.side, s.hinge],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    pub fn set_opening_swing(&mut self, id: i64, swing: Swing) -> Result<(), Error> {
        self.op("wall_opening_swing", |db| {
            let inv = Inv::default().restore(db, "opening", "id", id)?;
            let n = db.execute(
                "UPDATE opening SET swing_side=?2,swing_hinge=?3 WHERE id=?1",
                params![id, swing.side, swing.hinge],
            )?;
            if n == 0 {
                return invalid(&format!("no such opening {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn opening_swing(&self, id: i64) -> Result<Swing, Error> {
        self.db
            .query_row(
                "SELECT swing_side, swing_hinge FROM opening WHERE id=?1",
                [id],
                |r| {
                    Ok(Swing {
                        side: r.get(0)?,
                        hinge: r.get(1)?,
                    })
                },
            )
            .map_err(|_| Error::Invalid(format!("no such opening {id}")))
    }

    pub fn delete_opening(&mut self, id: i64) -> Result<(), Error> {
        self.op("wall_opening_delete", |db| {
            let inv = Inv::default().restore(db, "opening", "id", id)?;
            if db.execute("DELETE FROM opening WHERE id=?1", [id])? == 0 {
                return invalid(&format!("no such opening {id}"));
            }
            let inv = crate::sketch::drop_owned(db, inv, "opening", id)?;
            Ok(((), id, inv))
        })
    }

    /// Splits a wall at axis point `t` from `a`; openings move to their half. Returns the id of the new
    /// (second) wall. An opening on the split is an error carrying the opening id.
    pub fn split_wall(&mut self, id: i64, t: f64) -> Result<i64, Error> {
        self.op("wall_split", |db| {
            let (new, inv) = split_in(db, id, t)?;
            Ok((new, new, inv))
        })
    }
}

/// Splitting wall `id` at distance `t` along the axis: the new right part and inverse commands.
pub(crate) fn split_in(db: &Connection, id: i64, t: f64) -> Result<(i64, Inv), Error> {
    crate::wall_spline::refuse_piece(db, id)?;
    {
        {
            let (ax, ay, bx, by, bulge): (f64, f64, f64, f64, f64) = db
                .query_row(
                    "SELECT ax,ay,bx,by,bulge FROM wall WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .map_err(|_| Error::Invalid(format!("no such wall {id}")))?;
            let split_geom = |e: String| Error::Geom(format!("wall {id}: {e}"));
            // an arc is split by axis length: a point on the arc, part angles proportional to lengths
            let (split_pt, bulges) = if bulge != 0.0 {
                let arc = ArcAxis::new(v2(ax, ay), v2(bx, by), bulge, Tol::default())
                    .map_err(|e| split_geom(format!("{e:?}")))?;
                let eps = Tol::default().length;
                if !(t > eps && t < arc.length() - eps) {
                    return Err(split_geom(format!("{:?}", JoinError::SplitOutside)));
                }
                let f = t / arc.length();
                // A part of a short arc with |bulge| < MIN_BULGE is stored straight: ArcAxis would not accept it.
                let part = |k: f64| {
                    let b = (arc.sweep * k / 4.0).tan();
                    if b.abs() < MIN_BULGE {
                        0.0
                    } else {
                        b
                    }
                };
                let (bl, br) = (part(f), part(1.0 - f));
                (arc.point_at(t), (bl, br))
            } else {
                let strip = Strip {
                    a: v2(ax, ay),
                    b: v2(bx, by),
                    layers: vec![],
                    offset0: 0.0,
                    base: 0.0,
                    height: 1.0,
                };
                let (_, right) = split_at(&strip, t, Tol::default())
                    .map_err(|e| split_geom(format!("{e:?}")))?;
                (right.a, (0.0, 0.0))
            };
            let rows = wall_openings(db, id)?;
            let ops: Vec<Opening> = rows.iter().map(|r| r.1).collect();
            let (_, moved) = split_openings(&ops, t).map_err(|e| match e {
                JoinError::OpeningStraddlesSplit { opening } => Error::Geom(format!(
                    "wall {id} opening {}: straddles split at {t}",
                    rows[opening].0
                )),
                e => Error::Geom(format!("wall {id}: {e:?}")),
            })?;
            let inv = Inv::default()
                .restore(db, "wall", "id", id)?
                .restore(db, "opening", "wall", id)?
                .restore(db, "comp", "host_wall", id)?;
            // the right part's profile is measured from the split point
            let top: Option<String> =
                db.query_row("SELECT top_profile FROM wall WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            let right_top = top
                .as_deref()
                .and_then(parse_top_profile)
                .map(|p| top_profile_text(&p.iter().map(|q| (q.0 - t, q.1)).collect::<Vec<_>>()));
            db.execute(
                "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0,axis_kind,bulge,top_profile) \
                 SELECT level,wtype,?2,?3,bx,by,base_offset,height,offset0,?4<>0,?4,?5 \
                 FROM wall WHERE id=?1",
                params![id, split_pt.x, split_pt.y, bulges.1, right_top],
            )?;
            let new = db.last_insert_rowid();
            db.execute(
                "UPDATE wall SET bx=?2, by=?3, bulge=?4, axis_kind=(?4<>0) WHERE id=?1",
                params![id, split_pt.x, split_pt.y, bulges.0],
            )?;
            // `moved` holds the right openings in id order; the left ones are determined by the same condition.
            let eps = Tol::default().length;
            let mut it = moved.iter();
            for (oid, o) in &rows {
                if o.along + o.width > t + eps {
                    let m = it
                        .next()
                        .ok_or_else(|| Error::Invalid("split mismatch".into()))?;
                    db.execute(
                        "UPDATE opening SET wall=?2, along=?3 WHERE id=?1",
                        params![oid, new, m.along],
                    )?;
                }
            }
            // components on the right part: a new host, x from the split point
            db.execute(
                "UPDATE comp SET host_wall=?2, x=x-?3 WHERE host_wall=?1 AND x>=?3",
                params![id, new, t],
            )?;
            Ok((new, inv.delete("wall", "id", new)))
        }
    }
}

impl Model {
    /// Volume by material over all walls, by material name.
    pub fn material_volumes(&self) -> Result<Vec<(String, f64)>, Error> {
        let mut st = self.db.prepare(
            "SELECT material, SUM(volume) FROM wall_qty GROUP BY material ORDER BY material",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

type Ends = ((f64, f64), (f64, f64));

fn wall_ends(db: &Connection, id: i64) -> Result<Ends, Error> {
    db.query_row("SELECT ax,ay,bx,by FROM wall WHERE id=?1", [id], |r| {
        Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))
    })
    .map_err(|_| Error::Invalid(format!("no such wall {id}")))
}

/// Arc axis check: 0 is straight, otherwise bulge must give a valid arc on chord a->b.
pub fn check_bulge(a: (f64, f64), b: (f64, f64), bulge: f64) -> Result<(), Error> {
    if bulge == 0.0 {
        return Ok(());
    }
    ArcAxis::new(v2(a.0, a.1), v2(b.0, b.1), bulge, Tol::default())
        .map(|_| ())
        .map_err(|e| Error::Invalid(format!("arc wall: {e:?}")))
}

fn check_kind(o: &OpeningSpec) -> Result<(), Error> {
    if ![o.along, o.width, o.sill, o.height]
        .iter()
        .all(|v| v.is_finite())
    {
        return invalid("opening: non-finite dimension");
    }
    if !["door", "window", "void"].contains(&o.kind.as_str()) {
        return invalid(&format!("opening kind '{}' (door/window/void)", o.kind));
    }
    Ok(())
}

/// A wall's openings in id order: (id, geometry).
fn wall_openings(db: &Connection, wall: i64) -> Result<Vec<(i64, Opening)>, Error> {
    let mut st = db.prepare_cached(
        "SELECT id,along,width,sill,height FROM opening WHERE wall=?1 ORDER BY id",
    )?;
    let rows = st.query_map([wall], |r| {
        Ok((
            r.get(0)?,
            Opening {
                along: r.get(1)?,
                width: r.get(2)?,
                sill: r.get(3)?,
                height: r.get(4)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

type WallRow = (i64, i64, [f64; 6], Option<f64>, f64, Option<String>);

/// The top profile after editing axis a->b (and, if given, bulge/heights), before writing to the database.
/// An arc or a new height resets the profile (flat top at `height`); a straight axis keeps
/// heights at the same plan points: a flip mirrors, a shift/extension projects, beyond the edges
/// the top continues horizontally; a rotation keeps the distance from the fixed end.
/// Minimum length of a linear element (m): shorter is degenerate.
pub const MIN_LEN: f64 = 1e-3;

pub(crate) fn check_len(what: &str, a: (f64, f64), b: (f64, f64)) -> Result<(), Error> {
    if (b.0 - a.0).hypot(b.1 - a.1) < MIN_LEN {
        return Err(Error::Degenerate(format!("{what} length would be zero")));
    }
    Ok(())
}

/// The single path for writing a wall axis: length check, top profile recompute, journal.
pub(crate) fn write_axis(
    db: &Connection,
    inv: Inv,
    id: i64,
    a: (f64, f64),
    b: (f64, f64),
) -> Result<Inv, Error> {
    crate::wall_spline::refuse_piece(db, id)?;
    check_len(&format!("wall #{id}"), a, b)?;
    let inv = inv.restore(db, "wall", "id", id)?;
    let top = remapped_top(db, id, a, b, None, None)?;
    let n = db.execute(
        "UPDATE wall SET ax=?2, ay=?3, bx=?4, by=?5, top_profile=?6 WHERE id=?1",
        params![id, a.0, a.1, b.0, b.1, top],
    )?;
    if n == 0 {
        return invalid("no such wall");
    }
    let inv = crate::roof_auto::sync(db, inv, Some(&[id]))?;
    Ok(inv)
}

pub(crate) fn remapped_top(
    db: &Connection,
    id: i64,
    a: (f64, f64),
    b: (f64, f64),
    bulge: Option<f64>,
    height: Option<f64>,
) -> Result<Option<String>, Error> {
    type Row = ((f64, f64), (f64, f64), f64, f64, Option<String>);
    let (oa, ob, obulge, oh, text): Row = db
        .prepare_cached("SELECT ax,ay,bx,by,bulge,height,top_profile FROM wall WHERE id=?1")?
        .query_row([id], |r| {
            Ok((
                (r.get(0)?, r.get(1)?),
                (r.get(2)?, r.get(3)?),
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .map_err(|_| Error::Invalid(format!("no such wall {id}")))?;
    let Some(top) = text.as_deref().and_then(parse_top_profile) else {
        return Ok(None);
    };
    let arc = obulge != 0.0 || bulge.is_some_and(|b| b != 0.0);
    if arc || height.is_some_and(|h| (h - oh).abs() > 1e-9) {
        return Ok(None);
    }
    Ok(Some(top_profile_text(&remap_profile(&top, oa, ob, a, b))))
}

fn remap_profile(
    top: &[(f64, f64)],
    oa: (f64, f64),
    ob: (f64, f64),
    na: (f64, f64),
    nb: (f64, f64),
) -> Vec<(f64, f64)> {
    let (oa, ob, na, nb) = (
        v2(oa.0, oa.1),
        v2(ob.0, ob.1),
        v2(na.0, na.1),
        v2(nb.0, nb.1),
    );
    let (ol, nl) = ((ob - oa).len(), (nb - na).len());
    if ol <= 0.0 || nl <= 0.0 {
        return top.to_vec();
    }
    let (od, nd) = ((ob - oa) * (1.0 / ol), (nb - na) * (1.0 / nl));
    if od.cross(nd).abs() <= 1e-9 {
        let mut out: Vec<(f64, f64)> = top
            .iter()
            .map(|&(u, h)| ((oa + od * u - na).dot(nd), h))
            .collect();
        if od.dot(nd) < 0.0 {
            out.reverse();
        }
        out
    } else if (oa - na).len() <= 1e-9 || (ob - nb).len() > 1e-9 {
        top.to_vec()
    } else {
        top.iter().map(|&(u, h)| (u + nl - ol, h)).collect()
    }
}

/// Top profile text in the file: `u h;u h;...` (metres, 9 digits; re-import compares strings).
pub fn top_profile_text(t: &[(f64, f64)]) -> String {
    let r = |v: f64| {
        let x = (v * 1e9).round() / 1e9;
        if x == 0.0 {
            0.0
        } else {
            x
        }
    };
    t.iter()
        .map(|p| format!("{} {}", r(p.0), r(p.1)))
        .collect::<Vec<_>>()
        .join(";")
}

pub fn parse_top_profile(s: &str) -> Option<Vec<(f64, f64)>> {
    let t: Vec<(f64, f64)> = s
        .split(';')
        .map(|p| {
            let mut it = p.split(' ');
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect::<Option<_>>()?;
    top_profile_ok(&t).then_some(t)
}

fn top_profile_ok(t: &[(f64, f64)]) -> bool {
    t.len() >= 2
        && t.iter()
            .all(|p| p.0.is_finite() && p.1.is_finite() && p.1 > 0.0)
        && t.windows(2).all(|w| w[1].0 >= w[0].0)
}

/// Stored lean of a wall (`wall.lean`, `wall.top_scale`, schema 52).
pub(crate) fn stored_lean(db: &Connection, id: i64) -> Result<Lean, Error> {
    db.prepare_cached("SELECT lean, top_scale FROM wall WHERE id=?1")?
        .query_row([id], |r| Ok(Lean::new(r.get(0)?, r.get(1)?)))
        .map_err(|_| Error::Invalid(format!("no such wall {id}")))
}

/// The lean the geometry uses: a wall with a flat top leans, straight or on an arc (the radial
/// map); a top profile keeps the wall vertical, `lean_ignored` warns.
pub(crate) fn effective_lean(db: &Connection, id: i64, top: bool) -> Result<Lean, Error> {
    let l = stored_lean(db, id)?;
    Ok(if top || !l.valid() { Lean::PLAIN } else { l })
}

/// A lean on this wall is refused: bad numbers or a top profile.
pub(crate) fn check_lean(db: &Connection, id: i64, l: &Lean) -> Result<(), Error> {
    if !l.valid() {
        return invalid("wall lean: angle within ±60°, top thickness ratio 0.1..10");
    }
    if l.is_plain() {
        return Ok(());
    }
    let top: Option<String> = db
        .prepare_cached("SELECT top_profile FROM wall WHERE id=?1")?
        .query_row([id], |r| r.get(0))
        .map_err(|_| Error::Invalid(format!("no such wall {id}")))?;
    if top.is_some() {
        return invalid("wall lean: only a wall with a flat top can lean or taper");
    }
    Ok(())
}

/// Walls whose stored lean the geometry ignores (top profile).
pub(crate) fn lean_ignored(db: &Connection) -> Result<Vec<ElementWarning>, Error> {
    let mut st = db.prepare(
        "SELECT id FROM wall WHERE (lean<>0 OR top_scale<>1) \
         AND top_profile IS NOT NULL ORDER BY id",
    )?;
    let ids = st.query_map([], |r| r.get::<_, i64>(0))?;
    let mut out = vec![];
    for id in ids {
        out.push(ElementWarning {
            table: "wall",
            id: id?,
            message: "lean and taper are ignored: the wall has a top profile".into(),
        });
    }
    Ok(out)
}

/// Openings protruding above their wall's top profile: (opening id, description).
pub(crate) fn openings_above_top(db: &Connection) -> Result<Vec<(i64, String)>, Error> {
    let mut st = db.prepare(
        "SELECT o.id, o.along, o.width, o.sill, o.height, w.top_profile FROM opening o \
         JOIN wall w ON w.id = o.wall \
         WHERE w.top_profile IS NOT NULL AND w.bulge = 0 ORDER BY o.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            [r.get::<_, f64>(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut out = vec![];
    for row in rows {
        let (id, [along, width, sill, height], text) = row?;
        if let Some(t) = parse_top_profile(&text) {
            let h = ddc_geom::top::min_height(&t, along, along + width);
            if sill + height > h + 1e-9 {
                let head = sill + height;
                out.push((
                    id,
                    format!("opening above wall top ({head:.3} > {h:.3}): only the part under the top is cut"),
                ));
            }
        }
    }
    Ok(out)
}

/// Level wall quantities and their footprint outlines (layer rings and joint fillers).
pub(crate) struct LevelWalls {
    pub rows: Vec<(i64, i64, String, f64, f64)>,
    pub polys: Vec<Vec<ddc_geom::V2>>,
    /// Face circles of arc walls: the correction of room areas for chords.
    pub faces: Vec<ArcFace>,
    /// Index pairs of walls whose corner joint was not built (`Overlapping`; `(i, i)` is a degenerate
    /// joint of wall `i`, with straight ends).
    pub overlaps: Vec<(usize, usize)>,
    /// Imported walls that are degenerate even alone: no footprint and no quantities.
    pub undrawn: Vec<usize>,
    /// Wall ids by index.
    pub ids: Vec<i64>,
    /// Layer rings: (wall, layer, plan rings, bottom, top), for column deduction.
    pub layers: Vec<(i64, i64, Vec<Vec<ddc_geom::V2>>, f64, f64)>,
    /// The same per wall (a joint filler belongs to its owner), for partial recompute.
    pub pieces: Pieces,
}

/// One wall's contribution to its level: everything that recomputing neighbours leaves valid.
#[derive(Clone, Default)]
pub(crate) struct Piece {
    /// The wall frame together with its axis: walls with non-intersecting frames do not affect each other's joints.
    pub bb: [f64; 4],
    /// `wall_qty` rows before column deduction.
    pub rows: Vec<(i64, i64, String, f64, f64)>,
    pub layers: Vec<WallLayer>,
    pub fillers: Vec<Vec<ddc_geom::V2>>,
    pub faces: Vec<ArcFace>,
}

/// Level wall contributions by id (the traversal order of a full recompute).
pub(crate) type Pieces = std::collections::BTreeMap<i64, Piece>;

/// Level footprints from contributions: the same ring order as `level_walls`.
pub(crate) fn pieces_polys(level: i64, elevation: f64, pieces: &Pieces) -> LevelPolys {
    let rings = pieces
        .values()
        .flat_map(|p| p.layers.iter().flat_map(|l| l.2.iter().cloned()));
    let fillers = pieces.values().flat_map(|p| p.fillers.iter().cloned());
    (
        level,
        elevation,
        rings.chain(fillers).collect(),
        pieces
            .values()
            .flat_map(|p| p.faces.iter().copied())
            .collect(),
    )
}

/// Box of the wall band with its axis; an arc axis bulges past the chord, so its band is the
/// arc's box grown by the farthest face offset.
fn wall_box(s: &Strip, bulge: f64, tol: Tol) -> [f64; 4] {
    let bounds = s.bounds();
    let (lo, hi) = (bounds[0], bounds[bounds.len() - 1]);
    let n = s.normal();
    let pad = 10.0 * tol.length;
    if bulge != 0.0 {
        if let Ok(ax) = ArcAxis::new(s.a, s.b, bulge, tol) {
            return ax.bbox(lo.abs().max(hi.abs()) + pad);
        }
    }
    [
        s.a,
        s.b,
        s.a + n * lo,
        s.a + n * hi,
        s.b + n * lo,
        s.b + n * hi,
    ]
    .iter()
    .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| {
        [
            b[0].min(p.x - pad),
            b[1].min(p.y - pad),
            b[2].max(p.x + pad),
            b[3].max(p.y + pad),
        ]
    })
}

fn boxes_touch(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

const WALL_SEL: &str =
    "SELECT id,wtype,ax,ay,bx,by,base_offset,height,offset0,bulge,top_profile FROM wall";

fn wall_row(r: &rusqlite::Row) -> rusqlite::Result<WallRow> {
    let mut f = [0.0; 6];
    for (k, slot) in f.iter_mut().enumerate() {
        *slot = r.get(k + 2)?;
    }
    Ok((r.get(0)?, r.get(1)?, f, r.get(8)?, r.get(9)?, r.get(10)?))
}

/// A wall strip and its layer materials.
fn wall_strip(db: &Connection, w: &WallRow, elevation: f64) -> Result<(Strip, Vec<String>), Error> {
    let (_, wtype, [ax, ay, bx, by, base_offset, height], offset0, _, _) = w;
    let layers: Vec<(String, f64)> = db
        .prepare_cached(
            "SELECT material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos",
        )?
        .query_map([wtype], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let total: f64 = layers.iter().map(|l| l.1).sum();
    let meta = crate::wall_edit::layer_meta(db, *wtype)?;
    Ok((
        Strip {
            a: v2(*ax, *ay),
            b: v2(*bx, *by),
            layers: layers
                .iter()
                .enumerate()
                .map(|(k, l)| {
                    Layer::with_priority(l.1, meta.get(k).map_or(Layer::DEFAULT_PRIORITY, |m| m.2))
                })
                .collect(),
            offset0: offset0.unwrap_or(-total / 2.0),
            base: elevation + base_offset,
            height: *height,
        },
        layers.into_iter().map(|l| l.0).collect(),
    ))
}

/// An element warning: the model is computed, but something is wrong with this element.
#[derive(Debug, Clone, PartialEq)]
pub struct ElementWarning {
    pub table: &'static str,
    pub id: i64,
    pub message: String,
}

/// Index of the single wall a join failure blames. Invalid walls themselves (zero length, no
/// layers, non-finite) are still rejected: edits must not create them.
/// Tolerance is for imported data only: an edit must not make a user's wall vanish, so a wall
/// without an import link that would lose its footprint fails the edit.
fn reject_undrawn(db: &Connection, ids: &[i64]) -> Result<(), Error> {
    for &id in ids {
        let imported: bool = db
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM ext_ref WHERE kind='wall' AND ref=?1)")?
            .query_row([id], |r| r.get(0))?;
        if !imported {
            return Err(Error::Degenerate(format!(
                "wall {id}: footprint would be degenerate"
            )));
        }
    }
    Ok(())
}

fn bad_strip(e: &JoinError) -> Option<usize> {
    match e {
        JoinError::Degenerate { strip, .. } | JoinError::CutOverlap { strip, .. } => Some(*strip),
        _ => None,
    }
}

/// Level wall joints. `pairs` are joints not built as a corner (`(i, i)` is a degenerate joint
/// of wall `i`, with straight ends); `undrawn` are walls degenerate even alone: no footprint.
pub(crate) struct Joined {
    pub walls: ddc_geom::arc::Walls,
    pub pairs: Vec<(usize, usize)>,
    pub undrawn: Vec<usize>,
}

/// `arc_walls`, but a wall that does not join at a corner (`Overlapping`, a nearly parallel or
/// degenerate joint) is computed separately with straight ends; the pairs are returned for
/// warnings (`(i, i)` is a degenerate joint of wall `i`). A wall with no footprint gets empty rings.
pub fn join_tolerant(
    strips: &[Strip],
    bulges: &[f64],
    prefs: &[ddc_geom::layered::JoinPrefs],
    tol: Tol,
) -> Result<(ddc_geom::arc::Walls, Vec<(usize, usize)>), JoinError> {
    join_walls(strips, bulges, prefs, tol).map(|j| (j.walls, j.pairs))
}

pub(crate) fn join_walls(
    strips: &[Strip],
    bulges: &[f64],
    prefs: &[ddc_geom::layered::JoinPrefs],
    tol: Tol,
) -> Result<Joined, JoinError> {
    let mut kept: Vec<usize> = (0..strips.len()).collect();
    let mut pairs = vec![];
    let mut alone: Vec<usize> = vec![];
    let mut undrawn: Vec<usize> = vec![];
    let joined = loop {
        let ss: Vec<Strip> = kept.iter().map(|&i| strips[i].clone()).collect();
        let bs: Vec<f64> = kept.iter().map(|&i| bulges[i]).collect();
        let ps: Vec<ddc_geom::layered::JoinPrefs> = kept
            .iter()
            .map(|&i| prefs.get(i).copied().unwrap_or([None, None]))
            .collect();
        match ddc_geom::arc::arc_walls_with(&ss, &bs, &ps, tol) {
            Ok(j) => break j,
            Err(JoinError::Overlapping { a, b } | JoinError::NearlyParallel { a, b })
                if a < kept.len() && b < kept.len() =>
            {
                let (a, b) = (kept[a], kept[b]);
                pairs.push((a, b));
                alone.push(b);
                kept.retain(|&i| i != b);
            }
            // one bad wall (imported data) must not hide the level: it is solved alone below
            Err(e) => match bad_strip(&e) {
                Some(k) if k < kept.len() => {
                    let i = kept[k];
                    pairs.push((i, i));
                    alone.push(i);
                    kept.retain(|&k| k != i);
                }
                _ => return Err(e),
            },
        }
    };
    if alone.is_empty() {
        return Ok(Joined {
            walls: joined,
            pairs,
            undrawn,
        });
    }
    let mut rings: Vec<Vec<Vec<ddc_geom::arc::BulgeRing>>> =
        (0..strips.len()).map(|_| vec![]).collect();
    for (k, r) in joined.rings.into_iter().enumerate() {
        rings[kept[k]] = r;
    }
    for &i in &alone {
        match arc_walls(&strips[i..=i], &bulges[i..=i], tol) {
            Ok(mut j) => rings[i] = j.rings.remove(0),
            // still degenerate alone: no footprint (not «butt ends used»)
            Err(e) if bad_strip(&e).is_some() => {
                pairs.retain(|&p| p != (i, i));
                undrawn.push(i);
            }
            Err(e) => {
                return Err(match e {
                    JoinError::Arc { cause, .. } => JoinError::Arc { strip: i, cause },
                    e => e,
                })
            }
        }
    }
    let owners = joined
        .filler_owners
        .iter()
        .map(|&(w, l)| (kept[w], l))
        .collect();
    Ok(Joined {
        walls: ddc_geom::arc::Walls {
            rings,
            fillers: joined.fillers,
            filler_owners: owners,
        },
        pairs,
        undrawn,
    })
}

/// Footprints and quantities of one level's walls; a geometry error names the wall/opening.
pub(crate) fn level_walls(
    db: &Connection,
    level: i64,
    elevation: f64,
) -> Result<LevelWalls, Error> {
    let walls: Vec<WallRow> = {
        let mut st = db.prepare_cached(&format!("{WALL_SEL} WHERE level=?1 ORDER BY id"))?;
        let rows = st.query_map([level], wall_row)?;
        rows.collect::<Result<_, _>>()?
    };
    build_walls(db, elevation, &walls, None)
}

/// Height above the level at which rooms see the walls (area computation height).
pub const ROOM_CUT: f64 = 1.2;

/// Wall footprints the rooms of a level are bounded by: the plan join at the base, or, when
/// a wall leans or tapers, the joined section at `ROOM_CUT` above the level.
pub(crate) fn room_wall_polys(db: &Connection, level: i64, elevation: f64) -> Result<Vec<Vec<ddc_geom::V2>>, Error> {
    match leaning_section(db, level, elevation)? {
        Some(p) => Ok(p),
        None => Ok(level_walls(db, level, elevation)?.polys),
    }
}

/// The joined wall section at `ROOM_CUT` when some wall of the level leans or tapers; None
/// when every wall is vertical (the base footprints already are the section).
pub(crate) fn leaning_section(
    db: &Connection,
    level: i64,
    elevation: f64,
) -> Result<Option<Vec<Vec<ddc_geom::V2>>>, Error> {
    let walls: Vec<WallRow> = {
        let mut st = db.prepare_cached(&format!("{WALL_SEL} WHERE level=?1 ORDER BY id"))?;
        let rows = st.query_map([level], wall_row)?;
        rows.collect::<Result<_, _>>()?
    };
    let leans: Vec<Lean> = walls
        .iter()
        .map(|w| effective_lean(db, w.0, w.5.is_some()))
        .collect::<Result<_, _>>()?;
    if leans.iter().all(Lean::is_plain) {
        return Ok(None);
    }
    let mut strips = vec![];
    for w in &walls {
        strips.push(wall_strip(db, w, elevation)?.0);
    }
    let bulges: Vec<f64> = walls.iter().map(|w| w.4).collect();
    let ids: Vec<i64> = walls.iter().map(|w| w.0).collect();
    let prefs = crate::wall_edit::join_prefs(db, &ids)?;
    let (cut, cut_b) =
        ddc_geom::wall_lean::section_strips(&strips, &bulges, &leans, elevation + ROOM_CUT);
    let j = join_walls(&cut, &cut_b, &prefs, Tol::default())
        .map_err(|e| Error::Geom(format!("walls at the room cut height: {e:?}")))?;
    let mut polys: Vec<Vec<ddc_geom::V2>> =
        j.walls.rings.iter().flatten().flatten().map(|r| r.tessellate(CHORD_SAG)).collect();
    polys.extend(j.walls.fillers.iter().cloned());
    Ok(Some(polys))
}

/// The joint of these walls and the quantities of those in `keep` (None means all).
fn build_walls(
    db: &Connection,
    elevation: f64,
    walls: &[WallRow],
    keep: Option<&std::collections::BTreeSet<i64>>,
) -> Result<LevelWalls, Error> {
    let _tables = crate::wall_edit::TableMemo::start();
    let tol = Tol::default();
    let geom = |e: JoinError| match e {
        JoinError::Arc { strip, cause } => {
            Error::Geom(format!("wall {}: arc {cause:?}", walls[strip].0))
        }
        e => Error::Geom(format!("{e:?}")),
    };
    let wanted = |id: i64| keep.is_none_or(|k| k.contains(&id));
    let mut mats: Vec<Vec<String>> = vec![];
    let mut strips = vec![];
    let mut bulges = vec![];
    for w in walls {
        let (strip, m) = wall_strip(db, w, elevation)?;
        strips.push(strip);
        mats.push(m);
        bulges.push(w.4);
    }
    let ids: Vec<i64> = walls.iter().map(|w| w.0).collect();
    let prefs = crate::wall_edit::join_prefs(db, &ids)?;
    let Joined {
        walls: joined,
        pairs: overlaps,
        undrawn,
    } = join_walls(&strips, &bulges, &prefs, tol).map_err(geom)?;
    let lost: Vec<i64> = undrawn.iter().map(|&i| walls[i].0).collect();
    reject_undrawn(db, &lost)?;
    let leans: Vec<Lean> = walls
        .iter()
        .map(|w| effective_lean(db, w.0, w.5.is_some()))
        .collect::<Result<_, _>>()?;
    // leaning walls: the plan join solved again at the top; each piece is lofted base→top
    let top_join = if leans.iter().any(|l| !l.is_plain()) {
        let (tops, top_b) = ddc_geom::wall_lean::top_strips(&strips, &bulges, &leans);
        join_walls(&tops, &top_b, &prefs, tol).ok()
    } else {
        None
    };
    let mut holes_all: Vec<Vec<ddc_geom::wall_edit::FacadeHole>> = vec![];
    let mut qtys = vec![];
    let mut shapes = crate::cuts::Shapes::default();
    let mut tops = vec![];
    let mut polys = vec![];
    let mut faces = vec![];
    let mut layers = vec![];
    let mut pieces = Pieces::new();
    for (i, s) in strips.iter().enumerate() {
        if !wanted(walls[i].0) {
            holes_all.push(vec![]);
            qtys.push(vec![]);
            tops.push(None);
            continue;
        }
        let piece = pieces.entry(walls[i].0).or_default();
        piece.bb = wall_box(s, bulges[i], tol);
        let fp = &joined.rings[i];
        polys.extend(fp.iter().flatten().map(|r| r.tessellate(CHORD_SAG)));
        for (k, rings) in fp.iter().enumerate() {
            layers.push((
                walls[i].0,
                k as i64,
                rings.iter().map(|r| r.tessellate(CHORD_SAG)).collect(),
                s.base,
                s.base + s.height,
            ));
        }
        if bulges[i] != 0.0 {
            let f = arc_faces(s, bulges[i]);
            piece.faces.extend(f.iter().copied());
            faces.extend(f);
        }
        let os = wall_openings(db, walls[i].0)?;
        let ops: Vec<Opening> = os.iter().map(|o| o.1).collect();
        holes_all.push(
            ops.iter()
                .map(|o| (o.along, o.along + o.width, o.sill, o.sill + o.height))
                .collect(),
        );
        let top = walls[i]
            .5
            .as_deref()
            .and_then(parse_top_profile);
        let alen = if bulges[i] != 0.0 { ddc_geom::arc::axis_length(s.a, s.b, bulges[i]) } else { s.length() };
        let arc_ax = (bulges[i] != 0.0)
            .then(|| ddc_geom::arc::ArcAxis::new(s.a, s.b, bulges[i], tol).ok())
            .flatten();
        // with a profile, openings are checked and subtracted below, by the top height over the opening
        let flat_ops: &[Opening] = if top.is_some() { &[] } else { &ops };
        if top.is_some() {
            if let Some(k) = ops
                .iter()
                .position(|o| !ddc_geom::layered::opening_within(o, alen, f64::INFINITY))
            {
                return Err(Error::Geom(format!(
                    "wall {} opening {}: outside wall bounds or non-positive size",
                    walls[i].0, os[k].0
                )));
            }
        }
        if fp.is_empty() {
            qtys.push(
                s.layers
                    .iter()
                    .map(|_| ddc_geom::layered::LayerQty {
                        plan_area: 0.0,
                        volume: 0.0,
                    })
                    .collect(),
            );
            tops.push(None);
            continue;
        }
        let q = match wall_quantities(s, bulges[i], fp, flat_ops, i) {
            // footprint drawn, quantities not measurable: zero rather than a failed level
            Err(JoinError::Degenerate { .. } | JoinError::CutOverlap { .. }) => {
                Ok(zero_qty(s.layers.len()))
            }
            // undo may return to a state a repair had fixed: the file stays openable and editable
            Err(JoinError::OpeningOutside { .. }) if LENIENT.with(std::cell::Cell::get) => {
                Ok(zero_qty(s.layers.len()))
            }
            r => r,
        };
        let mut q = q.map_err(|e| match e {
            JoinError::OpeningOutside { opening, .. } => Error::Geom(format!(
                "wall {} opening {}: outside wall bounds or non-positive size",
                walls[i].0, os[opening].0
            )),
            e => geom(e),
        })?;
        // sloped/stepped top: the area*height prism is replaced by the integral of height over the plan
        if let Some(top) = &top {
            // an opening above the top is subtracted only for the part under the polyline (the protrusion is in check_integrity)
            // the wall core runs to the bottom of the roof's load-bearing layer, finishes to the bottom of the whole roof
            let core_top: Option<Vec<(f64, f64)>> = db
                .prepare_cached("SELECT top_core FROM wall WHERE id=?1")?
                .query_row([walls[i].0], |r| r.get::<_, Option<String>>(0))?
                .as_deref()
                .and_then(parse_top_profile);
            let core = match core_top {
                Some(_) => crate::layer_core::wall_core(db, walls[i].1)?,
                None => vec![],
            };
            for (k, ((lq, rings), l)) in q.iter_mut().zip(fp).zip(&s.layers).enumerate() {
                let top = match (&core_top, core.get(k)) {
                    (Some(c), Some(true)) => c,
                    _ => top,
                };
                // on an arc an opening is a sector: the layer thickness along its mean circle
                let ring_k = arc_ax.as_ref().map_or(1.0, |ax| {
                    let bo = s.bounds();
                    (ax.radius - ax.sigma() * (bo[k] + bo[k + 1]) / 2.0) / ax.radius
                });
                let cut: f64 = ops
                    .iter()
                    .map(|o| {
                        ddc_geom::top::area_under(top, o.along, o.along + o.width, o.sill, o.sill + o.height)
                    })
                    .sum::<f64>()
                    * ring_k;
                let flat: f64 = rings.iter().map(|r| r.area().abs()).sum::<f64>() * s.height;
                let real: f64 = rings
                    .iter()
                    .map(|r| match &arc_ax {
                        Some(ax) => ddc_geom::top::integral_arc(&r.tessellate(CHORD_SAG), r.area().abs(), ax, top),
                        None => ddc_geom::top::integral(&r.tessellate(CHORD_SAG), s.a, s.dir(), top).abs(),
                    })
                    .sum();
                lq.volume += real - flat - cut * l.thickness;
            }
        }
        if !leans[i].is_plain() {
            let top = top_join.as_ref().map(|t| &t.walls.rings[i]);
            lean_volumes(s, bulges[i], &leans[i], fp, top, &ops, &mut q);
        }
        // bottom attached to a slab/profile: minus the integral of the bottom rise over the layer plan
        if bulges[i] == 0.0 {
            if let Some(bp) = crate::cuts::base_profile(db, &mut shapes, walls[i].0)? {
                for (lq, rings) in q.iter_mut().zip(fp) {
                    let under: f64 = rings
                        .iter()
                        .map(|r| {
                            ddc_geom::top::integral(&r.tessellate(CHORD_SAG), s.a, s.dir(), &bp)
                        })
                        .sum();
                    lq.volume -= under;
                }
            }
        }
        qtys.push(q);
        tops.push(top);
    }
    // A joint filler of 3+ walls goes wholly to the owner, otherwise the sum of walls is less than the union.
    // the order does not depend on the wall set: a partial recompute assembles the same footprints
    let mut fillers: Vec<(Vec<ddc_geom::V2>, (usize, usize))> = joined
        .fillers
        .into_iter()
        .zip(joined.filler_owners)
        .filter(|(_, o)| wanted(walls[o.0].0))
        .collect();
    fillers.sort_by(|a, b| {
        a.1.cmp(&b.1).then_with(|| {
            let key = |f: &[ddc_geom::V2]| f.iter().flat_map(|p| [p.x, p.y]).collect::<Vec<f64>>();
            let (x, y) = (key(&a.0), key(&b.0));
            x.iter()
                .zip(&y)
                .map(|(u, v)| u.total_cmp(v))
                .find(|o| o.is_ne())
                .unwrap_or(x.len().cmp(&y.len()))
        })
    });
    for (poly, &(w, l)) in fillers.iter().map(|(p, o)| (p, o)) {
        let area = ddc_geom::profile::signed_area(poly).abs();
        qtys[w][l].plan_area += area;
        qtys[w][l].volume += match &tops[w] {
            Some(t) if bulges[w] != 0.0 => {
                match ddc_geom::arc::ArcAxis::new(strips[w].a, strips[w].b, bulges[w], tol) {
                    Ok(ax) => ddc_geom::top::integral_arc(poly, area, &ax, t),
                    Err(_) => area * strips[w].height,
                }
            }
            Some(t) => ddc_geom::top::integral(poly, strips[w].a, strips[w].dir(), t).abs(),
            None => area * strips[w].height,
        };
    }
    for (poly, (w, _)) in fillers {
        if let Some(p) = pieces.get_mut(&walls[w].0) {
            p.fillers.push(poly.clone());
        }
        polys.push(poly);
    }
    let axes = AxisIndex::new(&strips);
    let mut rows = vec![];
    // wall edits: profiles, structure regions, return, stacked type
    for (i, s) in strips.iter().enumerate() {
        if !wanted(walls[i].0) {
            continue;
        }
        let free_ends = [(s.a, 0usize), (s.b, 1)]
            .iter()
            .filter(|&&(p, e)| !axes.near(i, p) || prefs[i][e].is_some_and(|x| !x.allowed))
            .count();
        let arc = (bulges[i] != 0.0)
            .then(|| ddc_geom::arc::ArcAxis::new(s.a, s.b, bulges[i], tol).ok())
            .flatten();
        let len = if bulges[i] != 0.0 {
            ddc_geom::arc::axis_length(s.a, s.b, bulges[i])
        } else {
            s.length()
        };
        let thick: Vec<f64> = s.layers.iter().map(|l| l.thickness).collect();
        let q = crate::wall_edit::WallQ {
            id: walls[i].0,
            wtype: walls[i].1,
            len,
            height: s.height,
            mats: &mats[i],
            thick: &thick,
            holes: holes_all[i].clone(),
            radius: arc.map(|a| Box::new(move |o: f64| a.radius_at(o)) as Box<dyn Fn(f64) -> f64>),
            offset0: s.offset0,
            free_ends,
        };
        let mut own: Vec<crate::wall_edit::QtyRow> = qtys[i]
            .iter()
            .enumerate()
            .map(|(pos, lq)| {
                (
                    walls[i].0,
                    pos as i64,
                    mats[i][pos].clone(),
                    lq.plan_area,
                    lq.volume,
                )
            })
            .collect();
        crate::wall_edit::adjust_rows(db, &q, &mut own)?;
        rows.append(&mut own);
    }
    for r in &rows {
        if let Some(p) = pieces.get_mut(&r.0) {
            p.rows.push(r.clone());
        }
    }
    for l in &layers {
        if let Some(p) = pieces.get_mut(&l.0) {
            p.layers.push(l.clone());
        }
    }
    Ok(LevelWalls {
        rows,
        polys,
        faces,
        overlaps,
        undrawn,
        ids: walls.iter().map(|w| w.0).collect(),
        layers,
        pieces,
    })
}

/// Layer volumes of a leaning wall: each piece from its base to its top footprint (the joined
/// top when it pairs, else the base mapped by the lean; radially on an arc wall), exact on the
/// arc edges, minus the openings cut normal to the wall. Plan areas stay the base footprint.
fn lean_volumes(
    s: &Strip,
    bulge: f64,
    l: &Lean,
    base: &[Vec<ddc_geom::arc::BulgeRing>],
    top: Option<&Vec<Vec<ddc_geom::arc::BulgeRing>>>,
    ops: &[Opening],
    q: &mut [ddc_geom::layered::LayerQty],
) {
    use ddc_geom::wall_lean::{arc_of, openings_volume, ring_volume, top_bulge_ring};
    let bo = s.bounds();
    let arc = arc_of(s, bulge);
    for (li, (lq, rings)) in q.iter_mut().zip(base).enumerate() {
        let tl = top.and_then(|t| t.get(li)).filter(|t| t.len() == rings.len());
        let mut v = 0.0;
        for (k, r) in rings.iter().enumerate() {
            let t = top_bulge_ring(s, bulge, l, r, tl.map(|t| &t[k]));
            v += ring_volume(r, &t, s.height);
        }
        lq.volume = v - openings_volume(l, s.height, bo[li], bo[li + 1], ops, arc);
    }
}

/// Level wall axes in a cell grid: thousands of walls per level, pairwise search is quadratic per edit.
struct AxisIndex<'a> {
    strips: &'a [Strip],
    cell: f64,
    cells: std::collections::HashMap<(i64, i64), Vec<usize>>,
    /// Occupy too many cells (or have non-finite coordinates): always checked.
    long: Vec<usize>,
}

impl<'a> AxisIndex<'a> {
    const TOL: f64 = 1e-6;
    const MAX_CELLS: f64 = 64.0;

    fn new(strips: &'a [Strip]) -> Self {
        let total: f64 = strips.iter().map(|s| (s.b - s.a).len()).sum();
        let cell = (total / strips.len().max(1) as f64).max(0.01);
        let cell = if cell.is_finite() { cell } else { 1.0 };
        let mut idx = AxisIndex {
            strips,
            cell,
            cells: Default::default(),
            long: vec![],
        };
        for (i, s) in strips.iter().enumerate() {
            let (x0, x1) = (s.a.x.min(s.b.x), s.a.x.max(s.b.x));
            let (y0, y1) = (s.a.y.min(s.b.y), s.a.y.max(s.b.y));
            let count = ((x1 / cell).floor() - (x0 / cell).floor() + 1.0)
                * ((y1 / cell).floor() - (y0 / cell).floor() + 1.0);
            if count.is_nan() || count > Self::MAX_CELLS {
                idx.long.push(i);
                continue;
            }
            for key in idx.keys(x0, y0, x1, y1) {
                idx.cells.entry(key).or_default().push(i);
            }
        }
        idx
    }

    fn keys(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(i64, i64)> {
        let at = |v: f64| (v / self.cell).floor() as i64;
        let mut out = vec![];
        for x in at(x0 - Self::TOL)..=at(x1 + Self::TOL) {
            for y in at(y0 - Self::TOL)..=at(y1 + Self::TOL) {
                out.push((x, y));
            }
        }
        out
    }

    /// The point lies on another wall's axis (not `me`).
    fn near(&self, me: usize, p: ddc_geom::V2) -> bool {
        let on = |j: usize| {
            let o = &self.strips[j];
            let d = o.b - o.a;
            let t = ((p - o.a).dot(d) / d.dot(d).max(1e-18)).clamp(0.0, 1.0);
            j != me && (o.a + d * t - p).len() <= Self::TOL
        };
        self.long.iter().any(|&j| on(j))
            || self
                .keys(p.x, p.y, p.x, p.y)
                .iter()
                .filter_map(|k| self.cells.get(k))
                .any(|v| v.iter().any(|&j| on(j)))
    }
}

/// A level: id, elevation, wall footprint rings.
pub(crate) type LevelPolys = (i64, f64, Vec<Vec<ddc_geom::V2>>, Vec<ArcFace>);

/// Levels (id, elevation) by ascending id.
pub(crate) fn levels(db: &Connection) -> Result<Vec<(i64, f64)>, Error> {
    let mut st = db.prepare("SELECT id, elevation FROM level ORDER BY id")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Wall layer rings: (wall, layer, plan rings, bottom, top), for column deduction.
pub(crate) type WallLayer = (i64, i64, Vec<Vec<ddc_geom::V2>>, f64, f64);

/// A recomputed wall level.
pub(crate) struct RegenLevel {
    pub polys: LevelPolys,
    pub layers: Vec<WallLayer>,
    pub pieces: Pieces,
    /// There are walls left unjoined because of overlap: their set depends on the whole level.
    pub overlaps: bool,
}

thread_local! {
    /// Set by undo: an opening outside its wall zeroes that wall's quantities instead of failing.
    pub(crate) static LENIENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn zero_qty(layers: usize) -> Vec<ddc_geom::layered::LayerQty> {
    (0..layers)
        .map(|_| ddc_geom::layered::LayerQty {
            plan_area: 0.0,
            volume: 0.0,
        })
        .collect()
}

/// Recomputes `wall_qty` for all levels (`only` limits to these: their wall rows are replaced);
/// returns footprints and wall layers per level. A geometry error rolls back the whole operation.
pub(crate) fn regenerate(
    db: &Connection,
    only: Option<&std::collections::BTreeSet<i64>>,
) -> Result<Vec<RegenLevel>, Error> {
    // only rows that differ are written: an unchanged quantity is not an edit of its wall
    // (a full regeneration would otherwise mark every wall of the model as changed)
    type Row = (String, u64, u64);
    let mut old: std::collections::HashMap<(i64, i64), Row> = Default::default();
    {
        let mut st = db.prepare_cached(
            "SELECT q.wall, q.pos, q.material, q.plan_area, q.volume, w.level FROM wall_qty q \
             LEFT JOIN wall w ON w.id = q.wall",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?),
                (r.get::<_, String>(2)?, r.get::<_, f64>(3)?.to_bits(), r.get::<_, f64>(4)?.to_bits()),
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;
        for row in rows {
            let (k, v, level) = row?;
            // with `only`, rows of other levels stay as they are
            if only.is_none_or(|s| level.is_some_and(|l| s.contains(&l))) {
                old.insert(k, v);
            }
        }
    }
    let mut ins = db.prepare_cached("INSERT INTO wall_qty VALUES (?1,?2,?3,?4,?5)")?;
    let mut del = db.prepare_cached("DELETE FROM wall_qty WHERE wall=?1 AND pos=?2")?;
    let mut out = vec![];
    for (level, elevation) in levels(db)? {
        if only.is_some_and(|s| !s.contains(&level)) {
            continue;
        }
        let lw = level_walls(db, level, elevation)?;
        for (wall, k, mat, area, vol) in &lw.rows {
            let new = (mat.clone(), area.to_bits(), vol.to_bits());
            match old.remove(&(*wall, *k)) {
                Some(o) if o == new => continue,
                Some(_) => {
                    del.execute(params![wall, k])?;
                }
                None => {}
            }
            ins.execute(params![wall, k, mat, area, vol])?;
        }
        out.push(RegenLevel {
            polys: (level, elevation, lw.polys, lw.faces),
            layers: lw.layers,
            pieces: lw.pieces,
            overlaps: !lw.overlaps.is_empty(),
        });
    }
    // rows no wall produces any more
    for (wall, k) in old.into_keys() {
        del.execute(params![wall, k])?;
    }
    Ok(out)
}

/// A neighbourhood larger than this many walls and larger than half the level means recomputing the whole level.
const NEAR_LIMIT: usize = 256;

/// Recomputes only the neighbourhood of changed walls `dirty` (their rows and `pieces` are replaced).
/// A joint reads only walls whose frame intersects the wall's frame: a node lies within the frames of all its
/// participants. So only contributions of walls touching the old and new `dirty` frames change,
/// and their neighbours suffice to compute them. Returns the layers of replaced walls (without column deduction);
/// None means compute the whole level (arcs, overlaps, a joint error, a large neighbourhood).
pub(crate) fn regenerate_near(
    db: &Connection,
    level: i64,
    elevation: f64,
    dirty: &std::collections::BTreeSet<i64>,
    pieces: &mut Pieces,
) -> Result<Option<Vec<WallLayer>>, Error> {
    use std::collections::{BTreeMap, BTreeSet};
    let tol = Tol::default();
    let mut rows: BTreeMap<i64, WallRow> = BTreeMap::new();
    let mut boxes: BTreeMap<i64, [f64; 4]> = BTreeMap::new();
    let mut probes: Vec<[f64; 4]> = vec![];
    for &id in dirty {
        probes.extend(pieces.get(&id).map(|p| p.bb));
        let row = db
            .prepare_cached(&format!("{WALL_SEL} WHERE id=?1 AND level=?2"))?
            .query_row([id, level], wall_row)
            .optional()?;
        let Some(row) = row else { continue };
        let bb = wall_box(&wall_strip(db, &row, elevation)?.0, row.4, tol);
        probes.push(bb);
        boxes.insert(id, bb);
        rows.insert(id, row);
    }
    let mut keep: BTreeSet<i64> = rows.keys().copied().collect();
    for (id, p) in pieces.iter().filter(|(id, _)| !dirty.contains(id)) {
        if probes.iter().any(|b| boxes_touch(b, &p.bb)) {
            keep.insert(*id);
            boxes.insert(*id, p.bb);
        }
    }
    let reach: Vec<[f64; 4]> = keep.iter().map(|id| boxes[id]).collect();
    let mut all: BTreeSet<i64> = keep.clone();
    for (id, p) in pieces.iter().filter(|(id, _)| !dirty.contains(id)) {
        if reach.iter().any(|b| boxes_touch(b, &p.bb)) {
            all.insert(*id);
        }
    }
    if all.len() > NEAR_LIMIT && all.len() * 2 > pieces.len() {
        return Ok(None);
    }
    let mut walls: Vec<WallRow> = Vec::with_capacity(all.len());
    for id in &all {
        walls.push(match rows.remove(id) {
            Some(r) => r,
            None => db
                .prepare_cached(&format!("{WALL_SEL} WHERE id=?1"))?
                .query_row([id], wall_row)?,
        });
    }
    let Ok(lw) = build_walls(db, elevation, &walls, Some(&keep)) else {
        return Ok(None);
    };
    if !lw.overlaps.is_empty() {
        return Ok(None);
    }
    for id in dirty.iter().chain(&keep) {
        db.prepare_cached("DELETE FROM wall_qty WHERE wall=?1")?
            .execute([id])?;
        pieces.remove(id);
    }
    let mut ins = db.prepare_cached("INSERT INTO wall_qty VALUES (?1,?2,?3,?4,?5)")?;
    for (wall, k, mat, area, vol) in &lw.rows {
        ins.execute(params![wall, k, mat, area, vol])?;
    }
    pieces.extend(lw.pieces);
    Ok(Some(lw.layers))
}

/// Restore the wall rows to their values before column deduction.
pub(crate) fn restore_gross(db: &Connection, id: i64, p: &Piece) -> Result<(), Error> {
    db.prepare_cached("DELETE FROM wall_qty WHERE wall=?1")?
        .execute([id])?;
    let mut ins = db.prepare_cached("INSERT INTO wall_qty VALUES (?1,?2,?3,?4,?5)")?;
    for (wall, k, mat, area, vol) in &p.rows {
        ins.execute(params![wall, k, mat, area, vol])?;
    }
    Ok(())
}

impl Model {
    /// Wall warnings: a corner joint was not built (walls overlap), the wall
    /// is shown with straight ends.
    pub fn wall_warnings(&self) -> Result<Vec<ElementWarning>, Error> {
        let mut out = vec![];
        for (level, elevation) in levels(&self.db)? {
            let lw = match level_walls(&self.db, level, elevation) {
                Ok(lw) => lw,
                // a wall written past the editor (zero length, degenerate footprint) is what
                // this report is for: name the walls that fail alone, do not fail the check
                Err(Error::Geom(_) | Error::Degenerate(_)) => {
                    let walls: Vec<WallRow> = {
                        let mut st = self
                            .db
                            .prepare_cached(&format!("{WALL_SEL} WHERE level=?1 ORDER BY id"))?;
                        let rows = st.query_map([level], wall_row)?;
                        rows.collect::<Result<_, _>>()?
                    };
                    for w in &walls {
                        if build_walls(&self.db, elevation, std::slice::from_ref(w), None).is_err() {
                            out.push(ElementWarning {
                                table: "wall",
                                id: w.0,
                                message: "degenerate footprint: wall not drawn, no quantities".into(),
                            });
                        }
                    }
                    continue;
                }
                Err(e) => return Err(e),
            };
            for &i in &lw.undrawn {
                out.push(ElementWarning {
                    table: "wall",
                    id: lw.ids[i],
                    message: "degenerate footprint: wall not drawn, no quantities".into(),
                });
            }
            for &(a, b) in &lw.overlaps {
                if a == b {
                    out.push(ElementWarning {
                        table: "wall",
                        id: lw.ids[a],
                        message: "corner join degenerate: butt ends used".into(),
                    });
                    continue;
                }
                for (x, y) in [(a, b), (b, a)] {
                    out.push(ElementWarning {
                        table: "wall",
                        id: lw.ids[x],
                        message: format!(
                            "overlaps wall {}: corner join skipped, butt ends used",
                            lw.ids[y]
                        ),
                    });
                }
            }
        }
        out.extend(lean_ignored(&self.db)?);
        out.sort_by_key(|w| w.id);
        out.extend(crate::mass::warnings(&self.db)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::layered::{join, layer_footprints, layer_quantities};

    #[test]
    fn level_edit_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("Floor 1", 0.0).unwrap();
        let b = m.add_level("Floor 2", 3.0).unwrap();
        m.update_level(b, "Roof", -1.0).unwrap();
        let l = m.level_list().unwrap();
        assert_eq!(l[0], (b, "Roof".into(), -1.0));
        assert!(m.update_level(a, " ", 0.0).is_err());
        m.undo().unwrap();
        assert_eq!(m.level_list().unwrap()[1], (b, "Floor 2".into(), 3.0));
        m.delete_level(b).unwrap();
        assert_eq!(m.level_list().unwrap().len(), 1);
        m.undo().unwrap();
        assert_eq!(m.level_list().unwrap().len(), 2);
    }

    /// An opening whose right edge is flush with the wall end by design (5.397 + 0.5614 against
    /// an axis length of 5.9583999999999975 after subtraction) fits and is snapped to the end.
    #[test]
    fn opening_flush_with_wall_end_survives_float_rounding() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("ext", &layers()).unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (41.603, -3.77),
                b: (47.5614, -3.77),
                level: lv,
                base_offset: 0.0,
                height: 1.28,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let len = m.wall_length(w).unwrap();
        assert!(5.397 + 0.5614 > len, "repro needs the rounding overshoot");
        let o = m
            .add_opening(w, &op_spec("void", 5.397, 0.5614, 0.0, 0.48))
            .unwrap();
        let (along, width): (f64, f64) = m
            .sql()
            .query_row("SELECT along,width FROM opening WHERE id=?1", [o], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(along + width <= len, "{along}+{width} > {len}");
        assert!((width - 0.5614).abs() < 1e-9);
        // beyond the tolerance it is still refused
        assert!(m
            .add_opening(w, &op_spec("void", 5.397, 0.5615, 0.0, 0.48))
            .is_err());
    }

    fn layers() -> Vec<LayerSpec> {
        vec![
            LayerSpec {
                material: "brick".into(),
                thickness: 0.2,
            },
            LayerSpec {
                material: "insulation".into(),
                thickness: 0.1,
            },
        ]
    }

    fn op_spec(kind: &str, along: f64, width: f64, sill: f64, height: f64) -> OpeningSpec {
        OpeningSpec {
            along,
            width,
            sill,
            height,
            kind: kind.into(),
        }
    }

    /// A 6x3 wall, layers 0.2 (brick) + 0.1 (insulation), a 0.9x2.1 door at 1.0, a 1.2x1.5 window at 4.0.
    fn single_wall(m: &mut Model) -> (i64, i64, i64) {
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("ext", &layers()).unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0., 0.),
                b: (6., 0.),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let d = m
            .add_opening(w, &op_spec("door", 1.0, 0.9, 0.0, 2.1))
            .unwrap();
        let win = m
            .add_opening(w, &op_spec("window", 4.0, 1.2, 0.9, 1.5))
            .unwrap();
        m.assert_integrity();
        (w, d, win)
    }

    fn near(v: &[(String, f64)], brick: f64, ins: f64) {
        assert!((v[0].1 - brick).abs() < 1e-9, "{v:?}");
        assert!((v[1].1 - ins).abs() < 1e-9, "{v:?}");
    }

    fn wall_volume(m: &Model, w: i64) -> f64 {
        m.sql()
            .query_row("SELECT SUM(volume) FROM wall_qty WHERE wall=?1", [w], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn top_profile_sets_volume_splits_and_undoes() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, _, _) = single_wall(&mut m);
        let flat = wall_volume(&m, w);
        // gable 3 → 4 → 3 m over the 6 m wall: mean height 3.5, openings unchanged
        let gable = [(0.0, 3.0), (3.0, 4.0), (6.0, 3.0)];
        m.set_wall_top_profile(w, Some(&gable)).unwrap();
        let thick: f64 = layers().iter().map(|l| l.thickness).sum();
        let v = wall_volume(&m, w);
        assert!((v - flat - 6.0 * thick * 0.5).abs() < 1e-9, "{v} {flat}");
        assert_eq!(m.wall_top_profile(w).unwrap().unwrap(), gable.to_vec());
        // split at 3: right half keeps its part of the profile, total unchanged
        let r = m.split_wall(w, 3.0).unwrap();
        assert_eq!(m.wall_top_profile(r).unwrap().unwrap()[0], (-3.0, 3.0));
        assert!((wall_volume(&m, w) + wall_volume(&m, r) - v).abs() < 1e-9);
        m.undo().unwrap();
        m.undo().unwrap();
        assert!((wall_volume(&m, w) - flat).abs() < 1e-12);
        assert_eq!(m.wall_top_profile(w).unwrap(), None);
        // bad profiles are refused
        assert!(m
            .set_wall_top_profile(w, Some(&[(1.0, 2.0), (0.0, 2.0)]))
            .is_err());
        assert!(m
            .set_wall_top_profile(w, Some(&[(0.0, 0.0), (1.0, 2.0)]))
            .is_err());
        assert_eq!(
            parse_top_profile(&top_profile_text(&gable)).unwrap(),
            gable.to_vec()
        );
        m.assert_integrity();
    }

    #[test]
    fn top_profile_clips_openings() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, d, win) = single_wall(&mut m);
        let thick: f64 = layers().iter().map(|l| l.thickness).sum();
        let gable = [(0.0, 3.0), (3.0, 4.0), (6.0, 3.0)];
        m.set_wall_top_profile(w, Some(&gable)).unwrap();
        let v0 = wall_volume(&m, w);
        // the window is raised above the slope: only the part under the top is subtracted, the level does not fail
        m.update_opening(win, &op_spec("window", 4.0, 1.2, 2.4, 1.5))
            .unwrap();
        let v1 = wall_volume(&m, w);
        let under = ddc_geom::top::area_under(&gable, 4.0, 5.2, 2.4, 3.9);
        assert!(under < 1.2 * 1.5 - 1e-6);
        assert!(
            (v1 - (v0 + (1.2 * 1.5 - under) * thick)).abs() < 1e-9,
            "{v1} {v0}"
        );
        let issues = m.check_integrity().unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!((issues[0].table.as_str(), issues[0].id), ("opening", win));
        assert!(issues.iter().all(|i| i.id != d));
    }

    #[test]
    fn top_profile_follows_axis_and_resets_on_height_or_arc() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, _, _) = single_wall(&mut m);
        let g = [(0.0, 3.0), (2.0, 4.0), (6.0, 3.5)];
        m.set_wall_top_profile(w, Some(&g)).unwrap();
        // flip: mirrored
        m.set_wall_axis(w, (6., 0.), (0., 0.)).unwrap();
        assert_eq!(
            m.wall_top_profile(w).unwrap().unwrap(),
            vec![(0.0, 3.5), (4.0, 4.0), (6.0, 3.0)]
        );
        m.undo().unwrap();
        // shifting the start 1 m back (extension): heights at the same plan points
        m.set_wall_axis(w, (-1., 0.), (6., 0.)).unwrap();
        assert_eq!(
            m.wall_top_profile(w).unwrap().unwrap(),
            vec![(1.0, 3.0), (3.0, 4.0), (7.0, 3.5)]
        );
        m.undo().unwrap();
        assert_eq!(m.wall_top_profile(w).unwrap().unwrap(), g.to_vec());
        // an arc resets the profile, undo restores it
        m.set_wall_bulge(w, 0.2).unwrap();
        assert_eq!(m.wall_top_profile(w).unwrap(), None);
        m.undo().unwrap();
        assert_eq!(m.wall_top_profile(w).unwrap().unwrap(), g.to_vec());
        // batch edit: the same height is a move, a new height is a reset
        let spec = |h: f64, a: (f64, f64)| WallSpec {
            a,
            b: (6., 0.),
            level: 1,
            base_offset: 0.0,
            height: h,
            wall_type: 1,
            offset0: None,
        };
        m.apply_edits(&[crate::Edit::Wall {
            id: w,
            spec: spec(3.0, (-0.5, 0.)),
            bulge: 0.0,
        }])
        .unwrap();
        assert_eq!(m.wall_top_profile(w).unwrap().unwrap()[0], (0.5, 3.0));
        m.apply_edits(&[crate::Edit::Wall {
            id: w,
            spec: spec(3.2, (-0.5, 0.)),
            bulge: 0.0,
        }])
        .unwrap();
        assert_eq!(m.wall_top_profile(w).unwrap(), None);
        m.undo().unwrap();
        assert_eq!(m.wall_top_profile(w).unwrap().unwrap()[0], (0.5, 3.0));
        m.assert_integrity();
    }

    #[test]
    fn openings_deduct_per_layer_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, d, win) = single_wall(&mut m);
        // Deduction 0.9*2.1 + 1.2*1.5 = 3.69 m2: masonry 3.6 - 0.738, insulation 1.8 - 0.369.
        near(&vol(&m), 2.862, 1.431);
        m.update_opening(d, &op_spec("door", 1.0, 1.0, 0.0, 2.1))
            .unwrap();
        near(&vol(&m), 3.6 - 3.9 * 0.2, 1.8 - 3.9 * 0.1);
        m.undo().unwrap();
        near(&vol(&m), 2.862, 1.431);
        m.delete_opening(win).unwrap();
        near(&vol(&m), 3.6 - 1.89 * 0.2, 1.8 - 1.89 * 0.1);
        m.undo().unwrap();
        near(&vol(&m), 2.862, 1.431);
        m.undo().unwrap();
        m.undo().unwrap();
        near(&vol(&m), 3.6, 1.8);
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM opening WHERE wall=?1", [w], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn opening_must_fit_its_wall() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, d, _) = single_wall(&mut m);
        let ok = op_spec("door", 1.0, 1.0, 0.0, 2.1);
        m.opening_fits(w, &ok).unwrap();
        // past the wall end (6 m), above the top (3 m), below the bottom
        for bad in [
            op_spec("door", 5.5, 1.0, 0.0, 2.1),
            op_spec("door", -0.1, 1.0, 0.0, 2.1),
            op_spec("window", 1.0, 1.0, 2.0, 1.5),
            op_spec("window", 1.0, 1.0, -0.2, 1.0),
        ] {
            assert!(m.opening_fits(w, &bad).is_err(), "{bad:?}");
            assert!(m.update_opening(d, &bad).is_err(), "{bad:?}");
        }
        m.update_opening(d, &ok).unwrap();
        let width: f64 =
            m.db.query_row("SELECT width FROM opening WHERE id=?1", [d], |r| r.get(0))
                .unwrap();
        assert!((width - 1.0).abs() < 1e-9);
        // the wall's opening top is by the window 0.9 + 1.5, the 2.1 door is lower
        assert!((m.openings_head(w).unwrap() - 2.4).abs() < 1e-9);
    }

    #[test]
    fn bad_opening_names_wall_and_opening() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, d, _) = single_wall(&mut m);
        let e = m
            .update_opening(d, &op_spec("door", 5.5, 0.9, 0.0, 2.1))
            .unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains(&format!("wall {w} opening {d}")), "{msg}");
        near(&vol(&m), 2.862, 1.431);
        assert!(m
            .add_opening(w, &op_spec("gate", 1.0, 1.0, 0.0, 1.0))
            .is_err());
    }

    #[test]
    fn split_moves_openings_keeps_volumes_and_undoes() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, d, win) = single_wall(&mut m);
        let before = vol(&m);
        let new = m.split_wall(w, 3.0).unwrap();
        let wall_of = |m: &Model, id: i64| -> (i64, f64) {
            m.sql()
                .query_row("SELECT wall, along FROM opening WHERE id=?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap()
        };
        assert_eq!(wall_of(&m, d), (w, 1.0));
        let (ww, along) = wall_of(&m, win);
        assert_eq!(ww, new);
        assert!((along - 1.0).abs() < 1e-12);
        near(&vol(&m), 2.862, 1.431);
        m.undo().unwrap();
        assert_eq!(vol(&m), before);
        assert_eq!(wall_of(&m, win), (w, 4.0));
        let walls: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap();
        assert_eq!(walls, 1);
    }

    #[test]
    fn split_through_opening_errors_with_ids() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, _, win) = single_wall(&mut m);
        let e = m.split_wall(w, 4.5).unwrap_err().to_string();
        assert!(e.contains(&format!("wall {w} opening {win}")), "{e}");
        near(&vol(&m), 2.862, 1.431);
    }

    const P: [(f64, f64); 4] = [(0., 0.), (5., 0.), (5., 4.), (0., 4.)];

    fn rectangle(m: &mut Model) -> Vec<i64> {
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("ext", &layers()).unwrap();
        let ids = (0..4)
            .map(|i| {
                let w = WallSpec {
                    a: P[i],
                    b: P[(i + 1) % 4],
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                };
                m.add_wall(&w).unwrap()
            })
            .collect();
        m.assert_integrity();
        ids
    }

    fn vol(m: &Model) -> Vec<(String, f64)> {
        m.assert_integrity();
        m.material_volumes().unwrap()
    }

    #[test]
    fn four_walls_volumes_match_geom() {
        let mut m = Model::open(":memory:").unwrap();
        rectangle(&mut m);
        // Masonry: 5.3x4.3 - 4.9x3.9 = 3.68 m2; insulation: 4.9x3.9 - 4.7x3.7 = 1.72 m2; height 3.
        let v = vol(&m);
        assert_eq!(v[0].0, "brick");
        assert!((v[0].1 - 3.68 * 3.0).abs() < 1e-9);
        assert_eq!(v[1].0, "insulation");
        assert!((v[1].1 - 1.72 * 3.0).abs() < 1e-9);

        // The same walls directly via ddc-geom.
        let tol = Tol::default();
        let strips: Vec<Strip> = (0..4)
            .map(|i| Strip {
                a: v2(P[i].0, P[i].1),
                b: v2(P[(i + 1) % 4].0, P[(i + 1) % 4].1),
                layers: vec![Layer::new(0.2), Layer::new(0.1)],
                offset0: -0.15,
                base: 0.0,
                height: 3.0,
            })
            .collect();
        let j = join(&strips, tol).unwrap();
        let mut sum = [0.0; 2];
        for (i, s) in strips.iter().enumerate() {
            let fp = layer_footprints(s, &j.pieces[i], i, tol).unwrap();
            for (k, q) in layer_quantities(s, &fp, &[], i).unwrap().iter().enumerate() {
                sum[k] += q.volume;
            }
        }
        assert!((v[0].1 - sum[0]).abs() < 1e-12 && (v[1].1 - sum[1]).abs() < 1e-12);
    }

    #[test]
    fn undo_reverts_every_wall_change() {
        let mut m = Model::open(":memory:").unwrap();
        let ids = rectangle(&mut m);
        let before = vol(&m);
        let t: i64 = m
            .sql()
            .query_row("SELECT id FROM wall_type", [], |r| r.get(0))
            .unwrap();
        m.set_layer_thickness(t, 0, 0.3).unwrap();
        assert!((vol(&m)[0].1 - before[0].1).abs() > 1e-6);
        m.undo().unwrap();
        assert_eq!(vol(&m), before);
        m.delete_wall(ids[3]).unwrap();
        assert_ne!(vol(&m), before);
        m.undo().unwrap();
        assert_eq!(vol(&m), before);
        m.set_wall_axis(ids[0], (0., 0.), (6., 0.)).unwrap();
        m.undo().unwrap();
        assert_eq!(vol(&m), before);
        // A degenerate edit is rejected entirely (transaction rollback).
        let bad = m.set_wall_axis(ids[0], (1., 1.), (1., 1.));
        assert!(matches!(bad, Err(Error::Degenerate(_))));
        assert_eq!(vol(&m), before);
        for _ in 0..6 {
            m.undo().unwrap();
        }
        assert!(vol(&m).is_empty());
        let walls: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap();
        assert_eq!(walls, 0);
        assert!(matches!(m.undo(), Err(Error::NothingToUndo)));
    }

    fn star(m: &mut Model, ends: &[(f64, f64)]) -> f64 {
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "one",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.3,
                }],
            )
            .unwrap();
        for &b in ends {
            m.add_wall(&WallSpec {
                a: (0., 0.),
                b,
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        }
        m.assert_integrity();
        m.material_volumes().unwrap()[0].1
    }

    #[test]
    fn three_way_node_sum_equals_union() {
        let mut m = Model::open(":memory:").unwrap();
        // Strips 10x0.3 and 4.85x0.3 (T): union 3 + 1.455 = 4.455 m2.
        let v = star(&mut m, &[(5., 0.), (-5., 0.), (0., 5.)]);
        assert!((v - 4.455 * 3.0).abs() < 1e-9, "{v}");
    }

    #[test]
    fn four_way_node_sum_equals_union() {
        let mut m = Model::open(":memory:").unwrap();
        // Two 10x0.3 strips intersect in a 0.09 square: 6 - 0.09 = 5.91 m2.
        let v = star(&mut m, &[(5., 0.), (-5., 0.), (0., 5.), (0., -5.)]);
        assert!((v - 5.91 * 3.0).abs() < 1e-9, "{v}");
    }

    /// A semicircle R=3 (3,0)->(-3,0), one 0.2 layer centred, h=3.
    fn semicircle(m: &mut Model) -> (i64, i64) {
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "c",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let spec = WallSpec {
            a: (3., 0.),
            b: (-3., 0.),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        (m.add_arc_wall(&spec, 1.0).unwrap(), lv)
    }

    #[test]
    fn split_short_arc_stores_straight_parts() {
        let mut m = Model::open(":memory:").unwrap();
        let (w, _) = semicircle(&mut m);
        // A nearly straight arc: the halves get |bulge| < MIN_BULGE.
        m.set_wall_bulge(w, 1.5e-9).unwrap();
        let w2 = m.split_wall(w, 3.0).unwrap();
        assert_eq!(
            (m.wall_bulge(w).unwrap(), m.wall_bulge(w2).unwrap()),
            (0.0, 0.0)
        );
        let kinds: Vec<i64> = m
            .sql()
            .prepare("SELECT axis_kind FROM wall ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(kinds, [0, 0]);
        let total = m.wall_length(w).unwrap() + m.wall_length(w2).unwrap();
        assert!((total - 6.0).abs() < 1e-9, "{total}");
        assert!((vol(&m).iter().map(|v| v.1).sum::<f64>() - 6.0 * 0.2 * 3.0).abs() < 1e-9);
        // An ordinary arc splits into two arcs.
        m.undo().unwrap();
        m.set_wall_bulge(w, 1.0).unwrap();
        let w3 = m.split_wall(w, 1.0).unwrap();
        assert!(m.wall_bulge(w).unwrap() > 0.0 && m.wall_bulge(w3).unwrap() > 0.0);
    }

    #[test]
    fn arc_wall_quantities_exact_and_undoable() {
        use std::f64::consts::PI;
        let mut m = Model::open(":memory:").unwrap();
        let (w, _) = semicircle(&mut m);
        assert!((vol(&m)[0].1 - 1.8 * PI).abs() < 1e-9);
        assert!((m.wall_length(w).unwrap() - 3.0 * PI).abs() < 1e-12);
        m.add_opening(w, &op_spec("door", 4.0, 1.0, 0.0, 2.0))
            .unwrap();
        assert!((vol(&m)[0].1 - (1.8 * PI - 0.4)).abs() < 1e-9);
        assert!(m
            .add_opening(w, &op_spec("door", 9.0, 1.0, 0.0, 2.0))
            .is_err());
        m.set_wall_bulge(w, 0.0).unwrap();
        assert!((vol(&m)[0].1 - (6.0 * 0.2 * 3.0 - 0.4)).abs() < 1e-9);
        m.undo().unwrap();
        assert!((vol(&m)[0].1 - (1.8 * PI - 0.4)).abs() < 1e-9);
        assert!(m.set_wall_bulge(w, f64::NAN).is_err());
        assert!(m.set_wall_bulge(w, 1e-12).is_err());
        // splitting by axis length preserves volume
        let new = m.split_wall(w, 3.0 * PI / 2.0 + 1.0).unwrap();
        assert!((vol(&m)[0].1 - (1.8 * PI - 0.4)).abs() < 1e-9);
        let l = m.wall_length(w).unwrap() + m.wall_length(new).unwrap();
        assert!((l - 3.0 * PI).abs() < 1e-9);
        assert!(m.split_wall(w, 100.0).is_err());
        m.undo().unwrap();
        assert!((m.wall_bulge(w).unwrap() - 1.0).abs() < 1e-15);
    }

    #[test]
    fn room_bounded_by_arc_and_chord_is_exact() {
        let mut m = Model::open(":memory:").unwrap();
        let (_, lv) = semicircle(&mut m);
        let t: i64 = m
            .sql()
            .query_row("SELECT wtype FROM wall", [], |r| r.get(0))
            .unwrap();
        m.add_wall(&WallSpec {
            a: (-3., 0.),
            b: (3., 0.),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
        let r = m
            .add_room(&crate::RoomSpec {
                level: lv,
                name: "R".into(),
                number: "1".into(),
                point: (0.0, 1.0),
                height: 2.5,
            })
            .unwrap();
        let q = m.room_quantities(r).unwrap().unwrap();
        let rr: f64 = 2.9;
        let exact = rr * rr * (0.1 / rr).acos() - 0.1 * (rr * rr - 0.01).sqrt();
        assert!((q.area - exact).abs() < 1e-9, "{} vs {exact}", q.area);
        assert!((q.area - 12.630512071383572).abs() < 1e-9);
    }

    #[test]
    fn schema_4_file_opens_as_straight_walls() {
        let p = std::env::temp_dir().join(format!("ddc_arc_v4_{}.ddc", std::process::id()));
        let p = p.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&p);
        {
            let mut m = Model::open(&p).unwrap();
            single_wall(&mut m);
        }
        let raw = Connection::open(&p).unwrap();
        raw.execute_batch(
            "ALTER TABLE wall DROP COLUMN axis_kind; ALTER TABLE wall DROP COLUMN bulge;
             DROP TABLE ext_ref; DROP TABLE mesh; DROP TABLE prop;
             UPDATE meta SET value=4 WHERE key='schema_version';",
        )
        .unwrap();
        drop(raw);
        let m = Model::open(&p).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        near(&vol(&m), 2.862, 1.431);
        assert_eq!(m.wall_bulge(1).unwrap(), 0.0);
        drop(m);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn schema_5_file_keeps_arcs_and_gains_bridge_tables() {
        let p = std::env::temp_dir().join(format!("ddc_arc_v5_{}.ddc", std::process::id()));
        let p = p.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&p);
        {
            let mut m = Model::open(&p).unwrap();
            single_wall(&mut m);
            m.set_wall_bulge(1, 0.3).unwrap();
        }
        let raw = Connection::open(&p).unwrap();
        raw.execute_batch(
            "DROP TABLE ext_ref; DROP TABLE mesh; DROP TABLE prop;
             UPDATE meta SET value=5 WHERE key='schema_version';",
        )
        .unwrap();
        drop(raw);
        for _ in 0..2 {
            let m = Model::open(&p).unwrap();
            assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
            assert_eq!(m.wall_bulge(1).unwrap(), 0.3);
            let n: i64 =
                m.db.query_row("SELECT count(*) FROM ext_ref", [], |r| r.get(0))
                    .unwrap();
            assert_eq!(n, 0);
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn schema_6_ext_ref_gains_source_and_keeps_rowids() {
        let p = std::env::temp_dir().join(format!("ddc_ext_v6_{}.ddc", std::process::id()));
        let p = p.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&p);
        drop(Model::open(&p).unwrap());
        let raw = Connection::open(&p).unwrap();
        raw.execute_batch(
            "DROP TABLE ext_ref;
             CREATE TABLE ext_ref(guid TEXT PRIMARY KEY, kind TEXT NOT NULL, ref INTEGER NOT NULL);
             INSERT INTO ext_ref(rowid, guid, kind, ref) VALUES (5, 'g1', 'wall', 1), (9, 'g2', 'level', 1);
             UPDATE meta SET value=6 WHERE key='schema_version';",
        )
        .unwrap();
        drop(raw);
        for _ in 0..2 {
            let m = Model::open(&p).unwrap();
            assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
            let r: (i64, String) =
                m.db.query_row(
                    "SELECT rowid, source FROM ext_ref WHERE guid='g2'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(r, (9, String::new()));
            // the same guid from another source is a separate row
            m.db.execute(
                "INSERT INTO ext_ref(source, guid, kind, ref) VALUES ('b|f2', 'g1', 'wall', 2)",
                [],
            )
            .unwrap();
            m.db.execute("DELETE FROM ext_ref WHERE source='b|f2'", [])
                .unwrap();
        }
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod overlap_tests {
    use super::*;

    /// An imported wall may lose its footprint (warning); a drawn one fails the edit.
    #[test]
    fn undrawn_wall_is_rejected_unless_imported() {
        let m = Model::open(":memory:").unwrap();
        let db = m.sql();
        db.execute(
            "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('a.rvt','g1','wall',7)",
            [],
        )
        .unwrap();
        reject_undrawn(db, &[7]).unwrap();
        reject_undrawn(db, &[]).unwrap();
        assert!(matches!(
            reject_undrawn(db, &[7, 8]),
            Err(Error::Degenerate(s)) if s.contains("wall 8")
        ));
        // a link of another kind does not count
        db.execute(
            "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('a.rvt','g2','mesh',9)",
            [],
        )
        .unwrap();
        assert!(reject_undrawn(db, &[9]).is_err());
    }

    #[test]
    fn overlapping_corner_is_warning_not_error() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        assert!(m.wall_warnings().unwrap().is_empty());
        let w = |a, b| WallSpec {
            a,
            b,
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: Some(0.0),
        };
        let ids = m
            .add_walls(&[w((0.0, 0.0), (1.0, 0.0)), w((1.0, 0.0), (0.0, 0.1))])
            .unwrap();
        let ws = m.wall_warnings().unwrap();
        assert_eq!(ws.len(), 2);
        assert_eq!((ws[0].table, ws[0].id), ("wall", ids[0]));
        assert_eq!(ws[1].id, ids[1]);
        assert!(ws[0].message.contains(&ids[1].to_string()));
        // quantities are computed, with no spike: each wall's volume is no more than its bounding prism
        let q: f64 = m
            .sql()
            .query_row("SELECT sum(volume) FROM wall_qty", [], |r| r.get(0))
            .unwrap();
        assert!(q > 0.0 && q < 1.2 * 0.5 * 3.0, "{q}");
    }

    #[test]
    fn arc_and_straight_walls_go_in_one_step() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type("W", &[LayerSpec { material: "Brick".into(), thickness: 0.3 }])
            .unwrap();
        let w = |a, b| WallSpec { a, b, level: l, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
        let (arc, line) = (w((0.0, 0.0), (20.0, 0.0)), w((20.0, 0.0), (20.0, 8.0)));
        assert!(m.add_arc_walls(&[arc.clone(), line.clone()], &[0.15]).is_err(), "one bulge per wall");
        // a bulge of 1e3 or more is no arc
        assert!(m.add_arc_walls(&[arc.clone()], &[2e3]).is_err());
        let ids = m.add_arc_walls(&[arc, line], &[0.15, 0.0]).unwrap();
        assert_eq!(m.wall_bulge(ids[0]).unwrap(), 0.15);
        assert_eq!(m.wall_bulge(ids[1]).unwrap(), 0.0);
        let len = ddc_geom::arc::axis_length(v2(0.0, 0.0), v2(20.0, 0.0), 0.15);
        assert!(len > 20.29 && len < 20.31, "{len}");
        m.undo().unwrap();
        let n: i64 = m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
