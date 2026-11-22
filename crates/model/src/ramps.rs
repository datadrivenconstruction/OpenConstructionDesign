// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Ramps (schema 32). A ramp is defined by a plan axis, a bottom level and a top (a level with an
//! offset, or a height above the bottom), slope, width and landing rules. Flights, landings,
//! solids and quantities are computed by `ddc_geom::ramp`; there are no derived tables.

use ddc_geom::mesh::Mesh;
use ddc_geom::ramp::{Ramp, RampDef, RampPlan, RampQty};
use ddc_geom::section::{Prism, Zfn};
use ddc_geom::v2;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

const DDL: &str = "\
CREATE TABLE IF NOT EXISTS ramp(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, top_level INTEGER,
    base_offset REAL NOT NULL, top_offset REAL NOT NULL, width REAL NOT NULL, slope REAL NOT NULL,
    thickness REAL NOT NULL, landing REAL NOT NULL, max_run REAL NOT NULL, path TEXT NOT NULL,
    material TEXT NOT NULL);";

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct RampSpec {
    pub level: i64,
    /// Top level; if absent, `top_offset` is treated as a height above the bottom.
    pub top_level: Option<i64>,
    pub base_offset: f64,
    pub top_offset: f64,
    pub width: f64,
    /// Rise per metre of plan length.
    pub slope: f64,
    pub thickness: f64,
    pub landing: f64,
    pub max_run: f64,
    /// Axis from bottom to top.
    pub path: Vec<(f64, f64)>,
    pub material: String,
}

impl RampSpec {
    /// A typical 1:12 ramp, 1.2 m wide, up to level `top_level`.
    pub fn new(level: i64, top_level: Option<i64>, path: Vec<(f64, f64)>) -> Self {
        RampSpec {
            level,
            top_level,
            base_offset: 0.0,
            top_offset: 0.0,
            width: 1.2,
            slope: 1.0 / 12.0,
            thickness: 0.2,
            landing: 1.5,
            max_run: 9.0,
            path,
            material: "Concrete".into(),
        }
    }
}

pub(crate) fn path_text(path: &[(f64, f64)]) -> String {
    let parts: Vec<String> = path.iter().map(|p| format!("{},{}", p.0, p.1)).collect();
    parts.join(";")
}

pub(crate) fn parse_path(s: &str) -> Vec<(f64, f64)> {
    s.split(';')
        .filter_map(|p| {
            let (x, y) = p.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        })
        .collect()
}

fn elevation(db: &Connection, level: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no such level {level}")))
}

fn def(db: &Connection, s: &RampSpec) -> Result<RampDef, Error> {
    let base = elevation(db, s.level)? + s.base_offset;
    let top = match s.top_level {
        Some(t) => elevation(db, t)? + s.top_offset,
        None => base + s.top_offset,
    };
    Ok(RampDef {
        path: s.path.iter().map(|p| v2(p.0, p.1)).collect(),
        base_z: base,
        rise: top - base,
        width: s.width,
        slope: s.slope,
        thickness: s.thickness,
        landing: s.landing,
        max_run: s.max_run,
    })
}

fn layout(db: &Connection, s: &RampSpec) -> Result<Ramp, Error> {
    def(db, s)?
        .layout()
        .map_err(|e| Error::Invalid(format!("ramp: {e}")))
}

fn axis(s: &RampSpec) -> Vec<ddc_geom::V2> {
    s.path.iter().map(|p| v2(p.0, p.1)).collect()
}

/// Ramp volume; a ramp that currently cannot be built has no volume.
pub(crate) fn volume(db: &Connection, id: i64) -> Result<f64, Error> {
    let s = load(db, id)?;
    Ok(layout(db, &s).map_or(0.0, |l| l.qty().volume))
}

fn load(db: &Connection, id: i64) -> Result<RampSpec, Error> {
    db.query_row(
        "SELECT level,top_level,base_offset,top_offset,width,slope,thickness,landing,max_run,path,\
         material FROM ramp WHERE id=?1",
        [id],
        |r| {
            let path: String = r.get(9)?;
            Ok(RampSpec {
                level: r.get(0)?,
                top_level: r.get(1)?,
                base_offset: r.get(2)?,
                top_offset: r.get(3)?,
                width: r.get(4)?,
                slope: r.get(5)?,
                thickness: r.get(6)?,
                landing: r.get(7)?,
                max_run: r.get(8)?,
                path: parse_path(&path),
                material: r.get(10)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no such ramp {id}")))
}

/// Checked ramp insert for importers (`id` = Some: replace that row in place).
pub fn put_ramp(db: &Connection, s: &RampSpec, id: Option<i64>) -> Result<i64, Error> {
    layout(db, s)?;
    write(db, s, id)
}

fn write(db: &Connection, s: &RampSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO ramp(id,level,top_level,base_offset,top_offset,width,slope,\
         thickness,landing,max_run,path,material) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            id,
            s.level,
            s.top_level,
            s.base_offset,
            s.top_offset,
            s.width,
            s.slope,
            s.thickness,
            s.landing,
            s.max_run,
            path_text(&s.path),
            crate::material_look::assign("ramp", &s.material, "")
        ],
    )?;
    Ok(db.last_insert_rowid())
}

/// Section prisms: sloped flights and landings.
pub(crate) fn prisms(db: &Connection) -> Result<Vec<(i64, String, Prism)>, Error> {
    let ids: Vec<i64> = {
        let mut st = db.prepare("SELECT id FROM ramp ORDER BY id")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    for id in ids {
        let spec = load(db, id)?;
        let t = spec.thickness;
        // cannot be built (top below bottom, etc.): a flat placeholder along the axis, see `ramp_warnings`
        let Ok(l) = layout(db, &spec) else {
            let top = def(db, &spec)?.base_z;
            for ring in ddc_geom::ramp::axis_rings(&axis(&spec), spec.width) {
                out.push((
                    id,
                    spec.material.clone(),
                    Prism {
                        rings: vec![ring],
                        bottom: Zfn::flat(top - t),
                        top: Zfn::flat(top),
                        voids: vec![],
                    },
                ));
            }
            continue;
        };
        for f in &l.flights {
            let z = |z0: f64| Zfn {
                o: f.start,
                z: z0,
                grad: f.dir * spec.slope,
                ..Default::default()
            };
            out.push((
                id,
                spec.material.clone(),
                Prism {
                    rings: vec![f.ring(spec.width)],
                    bottom: z(f.z0 - t),
                    top: z(f.z0),
                    voids: vec![],
                },
            ));
        }
        for g in &l.landings {
            out.push((
                id,
                spec.material.clone(),
                Prism {
                    rings: vec![g.ring.clone()],
                    bottom: Zfn::flat(g.top - t),
                    top: Zfn::flat(g.top),
                    voids: vec![],
                },
            ));
        }
    }
    Ok(out)
}

impl Model {
    pub fn add_ramp(&mut self, s: &RampSpec) -> Result<i64, Error> {
        layout(&self.db, s)?;
        self.op("ramp_add", |db| {
            let id = write(db, s, None)?;
            Ok((id, id, Inv::default().delete("ramp", "id", id)))
        })
    }

    pub fn update_ramp(&mut self, id: i64, s: &RampSpec) -> Result<(), Error> {
        layout(&self.db, s)?;
        self.op("ramp_edit", |db| {
            load(db, id)?;
            let inv = Inv::default().restore(db, "ramp", "id", id)?;
            write(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_ramp(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("ramp", "ramp_delete", id, &[])
    }

    pub fn ramp(&self, id: i64) -> Result<RampSpec, Error> {
        load(&self.db, id)
    }

    /// All ramps of the project: (id, bottom level).
    pub fn all_ramps(&self) -> Result<Vec<(i64, i64)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id, level FROM ramp ORDER BY level, id")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Why a ramp cannot currently be built; `None` means it can.
    pub fn ramp_problem(&self, id: i64) -> Result<Option<String>, Error> {
        let s = load(&self.db, id)?;
        Ok(def(&self.db, &s)?.layout().err())
    }

    /// Selection outlines of a ramp that cannot be built: strips along the axis.
    pub fn ramp_axis_rings(&self, id: i64) -> Result<Vec<Vec<ddc_geom::V2>>, Error> {
        let s = load(&self.db, id)?;
        Ok(ddc_geom::ramp::axis_rings(&axis(&s), s.width))
    }

    /// 3D solids for the same case: flat slabs along the axis at the bottom elevation.
    pub fn ramp_stub_meshes(&self, id: i64) -> Result<Vec<Mesh>, Error> {
        let s = load(&self.db, id)?;
        let z = def(&self.db, &s)?.base_z;
        Ok(ddc_geom::ramp::axis_meshes(
            &axis(&s),
            s.width,
            z,
            s.thickness,
        ))
    }

    /// Ramps that cannot be built: shown as a placeholder, not exported to IFC.
    pub fn ramp_warnings(&self) -> Result<Vec<crate::ElementWarning>, Error> {
        let mut out = vec![];
        for (id, _) in self.all_ramps()? {
            if let Some(why) = self.ramp_problem(id)? {
                out.push(crate::ElementWarning {
                    table: "ramp",
                    id,
                    message: format!("ramp cannot be built: {why}"),
                });
            }
        }
        Ok(out)
    }

    /// Ramps whose bottom is at level `level`.
    pub fn ramps(&self, level: i64) -> Result<Vec<i64>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id FROM ramp WHERE level=?1 ORDER BY id")?;
        let v = st
            .query_map([level], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    pub fn ramp_layout(&self, id: i64) -> Result<Ramp, Error> {
        layout(&self.db, &load(&self.db, id)?)
    }

    /// Layout of an unsaved ramp (preview, validation).
    pub fn ramp_preview(&self, s: &RampSpec) -> Result<Ramp, Error> {
        layout(&self.db, s)
    }

    pub fn ramp_meshes(&self, id: i64) -> Result<Vec<Mesh>, Error> {
        Ok(self.ramp_layout(id)?.meshes())
    }

    pub fn ramp_qty(&self, id: i64) -> Result<RampQty, Error> {
        Ok(self.ramp_layout(id)?.qty())
    }

    /// Plan symbol at the bottom level: `cut_height` is measured from that level's elevation.
    pub fn ramp_plan(&self, id: i64, cut_height: f64) -> Result<RampPlan, Error> {
        let spec = load(&self.db, id)?;
        let cut = elevation(&self.db, spec.level)? + cut_height;
        Ok(layout(&self.db, &spec)?.plan(cut))
    }

    /// Ramps of a level in the drawing: outlines, break line, ascent arrow. The part above the
    /// cut plane is not separated: the whole outline is a projection.
    pub fn ramps_plan(&self, level: i64, cut_height: f64) -> Result<Vec<crate::ViewPrim>, Error> {
        use crate::{LineClass, ViewPrim};
        let mut out = vec![];
        for id in self.ramps(level)? {
            let mut e = |a: ddc_geom::V2, b: ddc_geom::V2| {
                out.push(ViewPrim::Edge {
                    table: "ramp",
                    element: id,
                    class: LineClass::Projection,
                    a: (a.x, a.y),
                    b: (b.x, b.y),
                })
            };
            let Ok(p) = self.ramp_plan(id, cut_height) else {
                for r in self.ramp_axis_rings(id)? {
                    for k in 0..r.len() {
                        e(r[k], r[(k + 1) % r.len()]);
                    }
                }
                continue;
            };
            for r in &p.outlines {
                for k in 0..r.len() {
                    e(r[k], r[(k + 1) % r.len()]);
                }
            }
            for w in p.run_line.windows(2) {
                e(w[0], w[1]);
            }
            for (a, b) in p.head {
                e(a, b);
            }
            if let Some((a, b)) = p.cut {
                e(a, b);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("L1", 0.0).unwrap();
        let b = m.add_level("L2", 0.5).unwrap();
        (m, a, b)
    }

    fn spec(a: i64, b: i64) -> RampSpec {
        RampSpec::new(a, Some(b), vec![(0.0, 0.0), (6.0, 0.0)])
    }

    #[test]
    fn add_reads_back_and_reaches_top_level() {
        let (mut m, a, b) = model();
        let s = spec(a, b);
        let id = m.add_ramp(&s).unwrap();
        assert_eq!(m.ramp(id).unwrap(), s);
        let q = m.ramp_qty(id).unwrap();
        assert!((q.rise - 0.5).abs() < 1e-9);
        assert!((q.run - 6.0).abs() < 1e-9);
        assert!((q.volume - 6.0 * 1.2 * 0.2).abs() < 1e-9);
        assert_eq!(m.ramps(a).unwrap(), vec![id]);
    }

    #[test]
    fn free_height_uses_top_offset() {
        let (mut m, a, _) = model();
        let mut s = RampSpec::new(a, None, vec![(0.0, 0.0), (1.0, 0.0)]);
        s.top_offset = 0.25;
        s.base_offset = 0.1;
        let id = m.add_ramp(&s).unwrap();
        let l = m.ramp_layout(id).unwrap();
        assert!((l.def.base_z - 0.1).abs() < 1e-9);
        assert!((l.top_z() - 0.35).abs() < 1e-9);
    }

    #[test]
    fn invalid_ramp_writes_nothing() {
        let (mut m, a, b) = model();
        let mut s = spec(a, b);
        s.slope = 0.0;
        assert!(m.add_ramp(&s).is_err());
        // top below bottom: the rise is not positive
        assert!(m
            .add_ramp(&RampSpec::new(b, Some(a), vec![(0.0, 0.0), (6.0, 0.0)]))
            .is_err());
        assert!(m.ramps(a).unwrap().is_empty());
    }

    #[test]
    fn edit_delete_and_undo_are_single_steps() {
        let (mut m, a, b) = model();
        let s = spec(a, b);
        let id = m.add_ramp(&s).unwrap();
        let mut w = s.clone();
        w.width = 2.0;
        m.update_ramp(id, &w).unwrap();
        assert_eq!(m.ramp(id).unwrap().width, 2.0);
        m.undo().unwrap();
        assert_eq!(m.ramp(id).unwrap().width, 1.2);
        m.delete_ramp(id).unwrap();
        assert!(m.ramp(id).is_err());
        m.undo().unwrap();
        assert_eq!(m.ramp(id).unwrap(), s);
        m.undo().unwrap();
        assert!(m.ramp(id).is_err());
    }

    #[test]
    fn level_with_ramp_cannot_be_deleted() {
        let (mut m, a, b) = model();
        m.add_ramp(&spec(a, b)).unwrap();
        assert!(matches!(m.delete_level(b), Err(Error::Referenced { .. })));
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn plan_prims_cover_outline_and_arrow() {
        let (mut m, a, b) = model();
        let id = m.add_ramp(&spec(a, b)).unwrap();
        let p = m.ramps_plan(a, 0.25).unwrap();
        // outline 4 + ascent line 1 + arrow 2 + break 1
        assert_eq!(p.len(), 8);
        assert!(p.iter().all(|x| matches!(
            x,
            crate::ViewPrim::Edge { table: "ramp", element, .. } if *element == id
        )));
        assert!(m.ramps_plan(b, 1.2).unwrap().is_empty());
    }

    #[test]
    fn section_prisms_follow_slope() {
        let (mut m, a, b) = model();
        m.add_ramp(&spec(a, b)).unwrap();
        let ps = prisms(&m.db).unwrap();
        assert_eq!(ps.len(), 1);
        assert!((ps[0].2.top.grad.x - 1.0 / 12.0).abs() < 1e-12);
    }

    #[test]
    fn move_copy_rotate_mirror_and_undo() {
        let (mut m, a, b) = model();
        let s = spec(a, b);
        let id = m.add_ramp(&s).unwrap();
        let t = [crate::Target::Ramp(id)];
        m.move_elements(&t, (1.0, 2.0)).unwrap();
        assert_eq!(m.ramp(id).unwrap().path, vec![(1.0, 2.0), (7.0, 2.0)]);
        m.undo().unwrap();
        assert_eq!(m.ramp(id).unwrap(), s);
        let c = m.copy_elements(&t, &[(0.0, 5.0)]).unwrap();
        assert_eq!(m.all_ramps().unwrap().len(), 2);
        let crate::Target::Ramp(n) = c[0] else {
            panic!("{c:?}")
        };
        assert_eq!(m.ramp(n).unwrap().path, vec![(0.0, 5.0), (6.0, 5.0)]);
        assert_eq!(m.ramp(n).unwrap().top_level, Some(b));
        m.undo().unwrap();
        assert_eq!(m.all_ramps().unwrap().len(), 1);
        m.rotate_elements(&t, (0.0, 0.0), std::f64::consts::FRAC_PI_2, false)
            .unwrap();
        let p = m.ramp(id).unwrap().path;
        assert!(p[1].0.abs() < 1e-9 && (p[1].1 - 6.0).abs() < 1e-9, "{p:?}");
        m.undo().unwrap();
        // mirror: the axis still runs from bottom to top
        m.mirror_elements(&t, (3.0, -1.0), (3.0, 1.0), false)
            .unwrap();
        let p = m.ramp(id).unwrap().path;
        assert!((p[0].0 - 6.0).abs() < 1e-9 && p[1].0.abs() < 1e-9, "{p:?}");
        assert!((m.ramp_qty(id).unwrap().rise - 0.5).abs() < 1e-9);
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn broken_ramp_stays_visible_and_is_reported() {
        let (mut m, a, b) = model();
        let id = m.add_ramp(&spec(a, b)).unwrap();
        assert!(m.ramp_warnings().unwrap().is_empty());
        m.sql()
            .execute("UPDATE level SET elevation=-1 WHERE id=?1", [b])
            .unwrap();
        assert!(m.ramp_layout(id).is_err());
        assert!(m.ramp_problem(id).unwrap().is_some());
        let w = m.ramp_warnings().unwrap();
        assert_eq!((w.len(), w[0].table, w[0].id), (1, "ramp", id));
        assert!(w[0].message.starts_with("ramp cannot be built"));
        assert_eq!(m.ramp_axis_rings(id).unwrap().len(), 1);
        assert_eq!(m.ramp_stub_meshes(id).unwrap().len(), 1);
        assert_eq!(m.ramps_plan(a, 1.2).unwrap().len(), 4, "stub outline");
        assert_eq!(prisms(&m.db).unwrap().len(), 1);
        assert_eq!(volume(&m.db, id).unwrap(), 0.0);
        // it can be moved and deleted; afterwards the level is free
        m.move_elements(&[crate::Target::Ramp(id)], (1.0, 0.0))
            .unwrap();
        assert!(matches!(
            m.delete_level(b),
            Err(Error::Referenced { by, .. }) if by == vec![("ramp", id)]
        ));
        m.delete_ramp(id).unwrap();
        m.delete_level(b).unwrap();
    }

    #[test]
    fn phase_tracks_new_ramp() {
        let (mut m, a, b) = model();
        let id = m.add_ramp(&spec(a, b)).unwrap();
        assert!(crate::PHASED.contains(&"ramp"));
        m.delete_ramp(id).unwrap();
        m.undo().unwrap();
        assert!(m.ramp(id).is_ok());
    }
}
