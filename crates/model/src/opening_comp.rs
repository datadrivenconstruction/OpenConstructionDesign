//! Doors and windows as components (schema 50, additive). The wall cut stays an `opening` row,
//! so every reader of openings (cuts, plans, schedules, IFC) keeps working; the fill is a
//! wall-hosted component of category door/window linked in `opening_comp`. Its type's Width,
//! Height and Sill drive the opening. Projects without links open and behave as before;
//! [`Model::migrate_openings_to_components`] converts their doors and windows when asked.

use ddc_geom::component::gen::{door, window, DoorKind, WindowKind};
use ddc_geom::component::{Category, Hosting};
use rusqlite::{params, Connection, OptionalExtension};

use crate::components::{checked, family_into, insert, type_into, CompHost, CompSpec};
use crate::ops::Inv;
use crate::walls::{OpeningSpec, Swing};
use ddc_geom::v2;
use crate::{Error, Model};

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS opening_comp(opening INTEGER PRIMARY KEY, comp INTEGER NOT NULL UNIQUE);",
    )?;
    Ok(())
}

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

/// The opening lies on the wall axis and below the wall top.
fn fits(db: &Connection, wall: i64, o: &OpeningSpec) -> Result<i64, Error> {
    let (level, a, b, bulge, h): (i64, (f64, f64), (f64, f64), f64, f64) = db
        .query_row(
            "SELECT level, ax, ay, bx, by, bulge, height FROM wall WHERE id=?1",
            [wall],
            |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?), (r.get(3)?, r.get(4)?), r.get(5)?, r.get(6)?)),
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no wall {wall}")))?;
    let len = ddc_geom::arc::axis_length(v2(a.0, a.1), v2(b.0, b.1), bulge);
    if o.along < -1e-9 || o.along + o.width > len + 1e-9 {
        return invalid(format!("{} {:.3} m wide does not fit wall {wall} (length {len:.3} m)", o.kind, o.width));
    }
    if o.sill + o.height > h + 1e-9 {
        return invalid(format!("{} is taller than wall {wall}", o.kind));
    }
    Ok(level)
}

/// Size of a door/window type: (kind, width, height, sill).
fn sizes(m: &Model, ctype: i64) -> Result<(&'static str, f64, f64, f64), Error> {
    let fid: i64 = m
        .db
        .query_row("SELECT family FROM comp_type WHERE id=?1", [ctype], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no component type {ctype}")))?;
    let f = m.family(fid)?;
    let kind = match f.category {
        Category::Door => "door",
        Category::Window => "window",
        _ => return invalid(format!("{} is not a door or window family", f.name)),
    };
    if f.hosting != Hosting::Wall {
        return invalid(format!("{} is not wall-hosted", f.name));
    }
    let r = f
        .resolve(&m.comp_type_values(ctype)?, &[])
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let w = r.num("Width").ok_or_else(|| Error::Invalid("type has no Width".into()))?;
    let h = r.num("Height").ok_or_else(|| Error::Invalid("type has no Height".into()))?;
    Ok((kind, w, h, r.num("Sill").unwrap_or(0.0)))
}

impl Model {
    /// Door or window of type `ctype` centred at `centre` along the wall: the opening and the
    /// hosted component in one operation (one undo). Returns (opening, component).
    pub fn add_opening_component(
        &mut self,
        wall: i64,
        ctype: i64,
        centre: f64,
        swing: Swing,
    ) -> Result<(i64, i64), Error> {
        let (kind, w, h, sill) = sizes(self, ctype)?;
        let o = OpeningSpec {
            along: centre - w / 2.0,
            width: w,
            sill,
            height: h,
            kind: kind.into(),
        };
        let level = fits(&self.db, wall, &o)?;
        let mut spec = CompSpec::at(level, ctype, centre, 0.0);
        spec.at.2 = sill;
        spec.host = CompHost::Wall(wall);
        let (level, vals) = checked(&self.db, &spec)?;
        self.op("wall_opening_comp_add", |db| {
            db.execute(
                "INSERT INTO opening(wall,along,width,sill,height,kind,swing_side,swing_hinge) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![wall, o.along, o.width, o.sill, o.height, o.kind, swing.side, swing.hinge],
            )?;
            let oid = db.last_insert_rowid();
            let cid = insert(db, &spec, level, &vals)?;
            db.execute(
                "INSERT INTO opening_comp(opening, comp) VALUES (?1,?2)",
                params![oid, cid],
            )?;
            let inv = Inv::default()
                .delete("opening_comp", "opening", oid)
                .delete("comp", "id", cid)
                .delete("opening", "id", oid);
            Ok(((oid, cid), oid, inv))
        })
    }

    /// Component that fills the opening, if it is a component-backed door or window.
    pub fn opening_component(&self, opening: i64) -> Result<Option<i64>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT l.comp FROM opening_comp l JOIN comp c ON c.id=l.comp WHERE l.opening=?1",
                [opening],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Changes the type of a component-backed door/window; the opening follows its sizes,
    /// keeping its centre.
    pub fn set_opening_component_type(&mut self, opening: i64, ctype: i64) -> Result<(), Error> {
        let comp = self
            .opening_component(opening)?
            .ok_or_else(|| Error::Invalid(format!("opening {opening} has no component")))?;
        let (kind, w, h, sill) = sizes(self, ctype)?;
        let (wall, along, width): (i64, f64, f64) = self.db.query_row(
            "SELECT wall, along, width FROM opening WHERE id=?1",
            [opening],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let centre = along + width / 2.0;
        let o = OpeningSpec {
            along: centre - w / 2.0,
            width: w,
            sill,
            height: h,
            kind: kind.into(),
        };
        fits(&self.db, wall, &o)?;
        let mut spec = self.component(comp)?;
        spec.ctype = ctype;
        spec.at = (centre, spec.at.1, sill);
        let (_, vals) = checked(&self.db, &spec)?;
        self.op("wall_opening_comp_type", |db| {
            let inv = Inv::default()
                .restore(db, "opening", "id", opening)?
                .then(Inv::default().restore(db, "comp", "id", comp)?);
            db.execute(
                "UPDATE opening SET along=?2, width=?3, sill=?4, height=?5, kind=?6 WHERE id=?1",
                params![opening, o.along, o.width, o.sill, o.height, o.kind],
            )?;
            db.execute(
                "UPDATE comp SET ctype=?2, x=?3, z=?4, vals=?5 WHERE id=?1",
                params![comp, ctype, centre, sill, vals],
            )?;
            Ok(((), opening, inv))
        })
    }

    /// Gives every door and window opening without a component one: a type per (kind, width,
    /// height, sill) of the generated single door / fixed window. One operation; returns how
    /// many openings were converted. Void openings stay plain cuts.
    pub fn migrate_openings_to_components(&mut self) -> Result<usize, Error> {
        let rows: Vec<(i64, i64, i64, f64, f64, f64, f64, String)> = {
            let mut st = self.db.prepare(
                "SELECT o.id, o.wall, w.level, o.along, o.width, o.sill, o.height, o.kind \
                 FROM opening o JOIN wall w ON w.id=o.wall \
                 WHERE o.kind IN ('door','window') \
                 AND o.id NOT IN (SELECT opening FROM opening_comp) ORDER BY o.id",
            )?;
            let it = st.query_map([], |r| {
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
            })?;
            it.collect::<Result<_, _>>()?
        };
        if rows.is_empty() {
            return Ok(0);
        }
        let n = rows.len();
        self.op("wall_opening_comp_migrate", |db| {
            let max = |t: &str| -> Result<i64, Error> {
                Ok(db.query_row(&format!("SELECT COALESCE(MAX(rowid),0) FROM {t}"), [], |r| {
                    r.get(0)
                })?)
            };
            let (f0, t0, c0) = (max("comp_family")?, max("comp_type")?, max("comp")?);
            let doors = family_into(db, &door(DoorKind::Single))?;
            let windows = family_into(db, &window(WindowKind::Fixed))?;
            let mm = |v: f64| ddc_geom::component::doc::mm(v);
            let mut ids = vec![];
            for (oid, wall, level, along, w, sill, h, kind) in &rows {
                let fam = if kind == "door" { doors } else { windows };
                let name = format!("{:.0} x {:.0}", w * 1000.0, h * 1000.0);
                let name = if *sill > 0.0 && kind == "window" {
                    format!("{name} / {:.0}", sill * 1000.0)
                } else {
                    name
                };
                let vals = format!("Width={}, Height={}, Sill={}", mm(*w), mm(*h), mm(*sill));
                let ct = type_into(db, fam, &name, &vals)?;
                let mut spec = CompSpec::at(*level, ct, along + w / 2.0, 0.0);
                spec.at.2 = *sill;
                spec.host = CompHost::Wall(*wall);
                let cid = insert(db, &spec, *level, "")?;
                db.execute(
                    "INSERT INTO opening_comp(opening, comp) VALUES (?1,?2)",
                    params![oid, cid],
                )?;
                ids.push(*oid);
            }
            let first = ids[0];
            let iv = |v: i64| vec![rusqlite::types::Value::Integer(v)];
            let inv = Inv::default()
                .sql(
                    "DELETE FROM opening_comp WHERE comp IN (SELECT id FROM comp WHERE rowid>?1)",
                    iv(c0),
                )
                .sql("DELETE FROM comp WHERE rowid>?1", iv(c0))
                .sql("DELETE FROM comp_type WHERE rowid>?1", iv(t0))
                .sql("DELETE FROM comp_family WHERE rowid>?1", iv(f0));
            Ok((n, first, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walls::{LayerSpec, WallSpec};

    fn model() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "W200",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (8.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        (m, w)
    }

    fn opening(m: &Model, id: i64) -> (f64, f64, f64, f64, String) {
        m.sql()
            .query_row(
                "SELECT along, width, sill, height, kind FROM opening WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap()
    }

    #[test]
    fn placed_window_cuts_the_wall_and_follows_its_type() {
        let (mut m, w) = model();
        let f = m.load_family(&window(WindowKind::Casement)).unwrap();
        let t = m.comp_type_id(f, "1200 x 1400").unwrap().unwrap();
        let (o, c) = m.add_opening_component(w, t, 3.0, Swing::default()).unwrap();
        let (along, width, sill, height, kind) = opening(&m, o);
        assert_eq!(kind, "window");
        assert!((along - 2.4).abs() < 1e-12 && (width - 1.2).abs() < 1e-12);
        assert!((sill - 0.9).abs() < 1e-12 && (height - 1.4).abs() < 1e-12);
        assert_eq!(m.opening_component(o).unwrap(), Some(c));
        let s = m.component(c).unwrap();
        assert_eq!(s.host, CompHost::Wall(w));
        assert!((s.at.0 - 3.0).abs() < 1e-12 && (s.at.2 - 0.9).abs() < 1e-12);
        // bigger type: the opening grows about its centre
        let t2 = m.comp_type_id(f, "1800 x 1500").unwrap().unwrap();
        m.set_opening_component_type(o, t2).unwrap();
        let (along, width, ..) = opening(&m, o);
        assert!((along - 2.1).abs() < 1e-12 && (width - 1.8).abs() < 1e-12);
        assert_eq!(m.component(c).unwrap().ctype, t2);
        m.undo().unwrap();
        assert!((opening(&m, o).1 - 1.2).abs() < 1e-12);
        m.undo().unwrap();
        assert_eq!(m.opening_component(o).unwrap(), None);
        assert_eq!(m.sql().query_row("SELECT count(*) FROM opening", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        // not a door/window family
        let lib = m.load_library_family("Table").unwrap();
        let tt = m.comp_types(lib).unwrap()[0].0;
        assert!(m.add_opening_component(w, tt, 3.0, Swing::default()).is_err());
        // outside the wall: nothing written
        assert!(m.add_opening_component(w, t, 7.9, Swing::default()).is_err());
        assert_eq!(m.components(None).unwrap().len(), 0);
    }

    #[test]
    fn old_openings_migrate_to_components_in_one_undo() {
        let (mut m, w) = model();
        let spec = |k: &str, along: f64, width: f64, sill: f64, h: f64| OpeningSpec {
            along,
            width,
            sill,
            height: h,
            kind: k.into(),
        };
        let d = m.add_opening(w, &spec("door", 0.5, 0.9, 0.0, 2.1)).unwrap();
        let w1 = m.add_opening(w, &spec("window", 2.0, 1.2, 0.9, 1.4)).unwrap();
        let w2 = m.add_opening(w, &spec("window", 4.0, 1.2, 0.9, 1.4)).unwrap();
        let v = m.add_opening(w, &spec("void", 6.0, 1.0, 0.0, 2.0)).unwrap();
        assert_eq!(m.migrate_openings_to_components().unwrap(), 3);
        assert_eq!(m.migrate_openings_to_components().unwrap(), 0, "idempotent");
        assert_eq!(m.opening_component(v).unwrap(), None);
        let (c1, c2) = (
            m.opening_component(w1).unwrap().unwrap(),
            m.opening_component(w2).unwrap().unwrap(),
        );
        assert_eq!(m.component(c1).unwrap().ctype, m.component(c2).unwrap().ctype, "one type");
        let cd = m.opening_component(d).unwrap().unwrap();
        let q = m.component_quantities(cd).unwrap();
        assert!((q.bbox[1].0 - q.bbox[0].0 - 0.9).abs() < 1e-9, "{:?}", q.bbox);
        assert!((q.bbox[1].2 - 2.1).abs() < 1e-9);
        m.undo().unwrap();
        assert_eq!(m.components(None).unwrap().len(), 0);
        assert_eq!(m.families().unwrap().len(), 0);
        assert_eq!(m.opening_component(d).unwrap(), None);
    }
}
